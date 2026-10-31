//! The one door to child processes (Sol review r4, R4-06), as `fsguard.rs` is to the filesystem: no other runtime
//! source reaches `std::process`, by any spelling (tests/fsguard_lint.rs). Four programs start here: `git` (git.rs
//! calls it; its writing commands change only the stack repo an operation names), the file manager, which the
//! GUI asks to show a path or open a link, and which writes nothing itself, a staged copy of the guard, run once
//! for its self-test before sync installs it, and the T3 CLI, which the queue guard asks about its thread (it
//! reads; its process tree ends with the guard's). The panel's push is `git` too, bounded by a deadline that ends its
//! whole tree (`bounded`), and the window on another PC runs `ssh -N` for its forward while it is open (`Tunnel`).

// The door: the one place the resolved boundary lints allow (clippy.toml).
#![allow(clippy::disallowed_methods, clippy::disallowed_types)]

use std::path::Path;
use std::process::{Command, Output};
use std::time::{Duration, Instant};

/// The exit status of the CLI.
pub use std::process::ExitCode;

/// This process's id (staging names, backup dirs, the lock file).
pub fn id() -> u32 {
    std::process::id()
}

/// What a finished child process left: its status, stdout and stderr (git.rs names it without reaching
/// `std::process`).
pub(crate) type Ran = Output;

/// The variables that tie a git process to one repo, as `git rev-parse --local-env-vars` lists them (git 2.56):
/// a hook gets some of them from the git that runs it. Git clears them itself when it enters a submodule.
const LOCAL_VARS: [&str; 15] = [
    "GIT_ALTERNATE_OBJECT_DIRECTORIES",
    "GIT_CONFIG",
    "GIT_CONFIG_PARAMETERS",
    "GIT_CONFIG_COUNT",
    "GIT_OBJECT_DIRECTORY",
    "GIT_DIR",
    "GIT_WORK_TREE",
    "GIT_IMPLICIT_WORK_TREE",
    "GIT_GRAFT_FILE",
    "GIT_INDEX_FILE",
    "GIT_NO_REPLACE_OBJECTS",
    "GIT_REPLACE_REF_BASE",
    "GIT_PREFIX",
    "GIT_SHALLOW_FILE",
    "GIT_COMMON_DIR",
];

/// The variables that set how git reads every pathspec. Lords writes its own pathspecs (`:(icase)…` among them), so
/// they mean what Lords wrote only in git's default mode (sync perf review r1, R1: with `GIT_LITERAL_PATHSPECS=1`
/// inherited, `:(icase)CLAUDE.local.md` was a literal name, matched nothing, and a tracked file looked untracked).
const PATHSPEC_VARS: [&str; 4] =
    ["GIT_LITERAL_PATHSPECS", "GIT_GLOB_PATHSPECS", "GIT_NOGLOB_PATHSPECS", "GIT_ICASE_PATHSPECS"];

/// `git -C <dir> <args>` for the repo at `dir` alone: none of the variables that tie git to the repo this process
/// was started from reach it (finding F1, 27 Oct: a hook's `GIT_INDEX_FILE` made another repo's `ls-files` read
/// the commit's index), and no inherited pathspec mode (`PATHSPEC_VARS`).
fn command(dir: &Path, args: &[&str]) -> Command {
    let mut c = Command::new("git");
    c.arg("-C").arg(dir).args(args);
    for v in LOCAL_VARS.iter().chain(&PATHSPEC_VARS) {
        c.env_remove(v);
    }
    c
}

/// Runs `git -C <dir> <args>` and waits for it.
pub(crate) fn git(dir: &Path, args: &[&str]) -> Result<Output, String> {
    command(dir, args).output().map_err(|e| format!("git {}: {e}", args.join(" ")))
}

/// Runs `git -C <dir> <args>` with the git variables this process got, and waits for it: only for reading the
/// index of the commit a hook checks, which `git commit <paths>` names in `GIT_INDEX_FILE`.
pub(crate) fn git_inherited(dir: &Path, args: &[&str]) -> Result<Output, String> {
    let mut c = Command::new("git");
    c.arg("-C").arg(dir).args(args);
    for v in PATHSPEC_VARS {
        c.env_remove(v);
    }
    c.output().map_err(|e| format!("git {}: {e}", args.join(" ")))
}

/// Runs `git -C <dir> <args>` with these environment variables set, and waits for it (the hook probe).
pub(crate) fn git_env(dir: &Path, args: &[&str], env: &[(&str, &str)]) -> Result<Output, String> {
    let mut c = command(dir, args);
    for (k, v) in env {
        c.env(k, v);
    }
    c.output().map_err(|e| format!("git {}: {e}", args.join(" ")))
}

/// Runs `<exe> guard --self-test` and requires it to print `ok` and exit 0 within 20 s: the copy a harness hook will
/// run answers before sync names it (orchestrator, 30 Oct). A child past the deadline is killed.
pub(crate) fn guard_self_test(guard_copy: &Path) -> Result<(), String> {
    use std::io::Read;
    let mut child = Command::new(guard_copy)
        .args(["guard", "--self-test"])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| format!("{}: {e}", guard_copy.display()))?;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    // Its output is read beside it, so a child that keeps the pipe open can't hold the sync past the deadline
    // (sol-loop r2, S14).
    let (tx, rx) = std::sync::mpsc::channel();
    let mut stdout = child.stdout.take();
    std::thread::spawn(move || {
        let mut out = String::new();
        if let Some(o) = stdout.as_mut() {
            let _ = o.read_to_string(&mut out);
        }
        let _ = tx.send(out);
    });
    let status = loop {
        match child.try_wait().map_err(|e| e.to_string())? {
            Some(s) => break s,
            None if std::time::Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!("{}: guard self-test took over 20 s", guard_copy.display()));
            }
            None => std::thread::sleep(std::time::Duration::from_millis(20)),
        }
    };
    let left = deadline.saturating_duration_since(std::time::Instant::now());
    let out = rx.recv_timeout(left).map_err(|_| {
        format!("{}: guard self-test's output not closed within 20 s", guard_copy.display())
    })?;
    if status.success() && out.trim() == "ok" {
        Ok(())
    } else {
        Err(format!("{}: guard self-test failed ({status}): {}", guard_copy.display(), out.trim()))
    }
}

/// Runs the T3 CLI (`t3_cli`, as `T3CODE_CLI_PATH` names it) with `args` and gives its stdout, or why not: it can't
/// start, it fails, or it isn't done within `deadline` (the queue guard, guard.rs). Its whole process tree ends with
/// this process (rule.stop-processes-by-tree): before it starts, this process joins a job that kills every process in
/// it when the job's last handle closes, which happens when this process exits, however it exits, a harness's kill
/// included. At the deadline the CLI itself is killed at once; what it started ends a moment later, with this
/// process. Without such a job (it can't be made, or this isn't Windows) the CLI doesn't start: nothing here could
/// end what it starts.
pub fn t3_cli(t3_cli: &Path, args: &[&str], deadline: Duration) -> Result<String, String> {
    use std::io::Read;
    use std::process::Stdio;
    tree_ends_with_this_process()?;
    let started = Instant::now();
    let mut child = Command::new(t3_cli)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("{}: {e}", t3_cli.display()))?;
    // Both pipes are read beside it, so a process that keeps one open can't hold the guard past the deadline.
    let read = |pipe: Option<Box<dyn Read + Send>>| {
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let mut text = String::new();
            if let Some(mut p) = pipe {
                let _ = p.read_to_string(&mut text);
            }
            let _ = tx.send(text);
        });
        rx
    };
    let out = read(child.stdout.take().map(|p| Box::new(p) as Box<dyn Read + Send>));
    let err = read(child.stderr.take().map(|p| Box::new(p) as Box<dyn Read + Send>));
    let status = loop {
        match child.try_wait().map_err(|e| e.to_string())? {
            Some(s) => break s,
            None if started.elapsed() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!("no answer within its deadline ({} ms)", deadline.as_millis()));
            }
            None => std::thread::sleep(Duration::from_millis(20)),
        }
    };
    // The pipes close as the CLI ends; a short grace, not a second deadline.
    let grace = deadline.saturating_sub(started.elapsed()).max(Duration::from_millis(200));
    let text = out
        .recv_timeout(grace)
        .map_err(|_| "its output wasn't closed within its deadline".to_string())?;
    if !status.success() {
        let why = err.recv_timeout(Duration::from_millis(200)).unwrap_or_default();
        let why = why.lines().find(|l| !l.trim().is_empty()).unwrap_or("").trim().to_string();
        return Err(format!("it failed ({status}): {why}"));
    }
    Ok(text)
}

/// Puts this process in a job that kills every process in it once the job's last handle closes; the handle stays
/// open until this process exits. Once per process: children started after it are in the job from their start, so
/// none can leave it before it closes.
#[cfg(windows)]
fn tree_ends_with_this_process() -> Result<(), String> {
    static JOINED: std::sync::OnceLock<Result<(), String>> = std::sync::OnceLock::new();
    JOINED.get_or_init(join_kill_on_close_job).clone()
}

#[cfg(windows)]
fn join_kill_on_close_job() -> Result<(), String> {
    use windows_sys::Win32::Foundation::CloseHandle;
    use windows_sys::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, JobObjectExtendedLimitInformation,
        SetInformationJobObject, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
        JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    };
    use windows_sys::Win32::System::Threading::GetCurrentProcess;
    let failed = |call: &str| {
        let e = std::io::Error::last_os_error();
        format!("no job to end the T3 CLI's process tree ({call}: {e})")
    };
    // SAFETY: Win32 calls with null (default) attributes and name, a limit struct that lives across the call, the
    // job handle made here, and the pseudo-handle of this process.
    unsafe {
        let job = CreateJobObjectW(std::ptr::null(), std::ptr::null());
        if job.is_null() {
            return Err(failed("CreateJobObjectW"));
        }
        let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
        limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        let size = std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32;
        let set = SetInformationJobObject(
            job,
            JobObjectExtendedLimitInformation,
            (&raw const limits).cast(),
            size,
        );
        if set == 0 {
            let e = failed("SetInformationJobObject");
            CloseHandle(job);
            return Err(e);
        }
        if AssignProcessToJobObject(job, GetCurrentProcess()) == 0 {
            let e = failed("AssignProcessToJobObject");
            CloseHandle(job);
            return Err(e);
        }
    }
    Ok(())
}

#[cfg(not(windows))]
fn tree_ends_with_this_process() -> Result<(), String> {
    Err("nothing here can end the T3 CLI's process tree on this system".into())
}

/// Runs `git -C <dir> <args>` with `stdin` as its input (`hash-object --stdin`), within a minute. See `bounded`.
pub(crate) fn git_input(dir: &Path, args: &[&str], stdin: Vec<u8>) -> Result<Output, String> {
    bounded(command(dir, args), Some(stdin), Duration::from_secs(60))
        .map_err(|e| format!("git {}: {e}", args.join(" ")))
}

/// Runs `git -C <dir> <args>` and waits at most `deadline` (a remote call: the panel's push). See `bounded`.
pub(crate) fn git_bounded(dir: &Path, args: &[&str], deadline: Duration) -> Result<Output, String> {
    bounded(command(dir, args), None, deadline).map_err(|e| format!("git {}: {e}", args.join(" ")))
}

/// Runs `c`, gives it `stdin`, and waits at most `deadline` for it to end: its status and output, or why not (rule
/// .remote-calls-have-deadlines). The child is put in a job of its own, which kills every process in it once its
/// handle closes: at the deadline, and however this returns. A child it can't put in such a job is killed and refused.
/// What the child starts before it joins the job (its first moments) is outside that job; on Windows a process that
/// joined `tree_ends_with_this_process` still ends it with this one.
fn bounded(mut c: Command, stdin: Option<Vec<u8>>, deadline: Duration) -> Result<Output, String> {
    use std::io::{Read, Write};
    use std::process::Stdio;
    let started = Instant::now();
    c.stdin(if stdin.is_some() { Stdio::piped() } else { Stdio::null() })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        // No console window of its own when the window or a hidden task starts it.
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        c.creation_flags(CREATE_NO_WINDOW);
    }
    let mut child = c.spawn().map_err(|e| e.to_string())?;
    let tree = match tree::Tree::holding(&child) {
        Ok(t) => t,
        Err(e) => {
            let _ = child.kill();
            let _ = child.wait();
            return Err(e);
        }
    };
    if let (Some(bytes), Some(mut pipe)) = (stdin, child.stdin.take()) {
        std::thread::spawn(move || {
            let _ = pipe.write_all(&bytes);
        });
    }
    let read = |pipe: Option<Box<dyn Read + Send>>| {
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let mut bytes = Vec::new();
            if let Some(mut p) = pipe {
                let _ = p.read_to_end(&mut bytes);
            }
            let _ = tx.send(bytes);
        });
        rx
    };
    let out = read(child.stdout.take().map(|p| Box::new(p) as Box<dyn Read + Send>));
    let err = read(child.stderr.take().map(|p| Box::new(p) as Box<dyn Read + Send>));
    let status = loop {
        match child.try_wait().map_err(|e| e.to_string())? {
            Some(s) => break s,
            None if started.elapsed() >= deadline => {
                drop(tree);
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!("no answer within its deadline ({} s)", deadline.as_secs()));
            }
            None => std::thread::sleep(Duration::from_millis(20)),
        }
    };
    // Whatever the child started ends now, so the pipes close.
    drop(tree);
    let grace = Duration::from_millis(500);
    let stdout = out.recv_timeout(grace).map_err(|_| "its output wasn't closed".to_string())?;
    let stderr = err.recv_timeout(grace).unwrap_or_default();
    Ok(Output { status, stdout, stderr })
}

/// `ssh <alias> <command…>`, bounded (`lords panel add`'s remote steps; each script run there carries its own
/// deadline too). `alias` is checked to be an alias, never an option.
pub fn ssh(
    alias: &str,
    command: &[&str],
    stdin: Option<Vec<u8>>,
    deadline: Duration,
) -> Result<Output, String> {
    crate::config::check_alias(alias)?;
    let mut c = Command::new("ssh");
    c.args(["-o", "BatchMode=yes", "-o", "ConnectTimeout=10", "--", alias]).args(command);
    bounded(c, stdin, deadline).map_err(|e| format!("ssh {alias}: {e}"))
}

/// `schtasks <args>`, bounded (`lords panel add`'s task for `lords panel serve`).
pub fn schtasks(args: &[&str], deadline: Duration) -> Result<Output, String> {
    let mut c = Command::new("schtasks");
    c.args(args);
    bounded(c, None, deadline).map_err(|e| format!("schtasks {}: {e}", args.join(" ")))
}

/// `<sshd_exe> -t -f <config>`: whether an sshd would start with this config, bounded.
pub fn sshd_check(sshd_exe: &Path, config: &Path, deadline: Duration) -> Result<Output, String> {
    let mut c = Command::new(sshd_exe);
    c.arg("-t").arg("-f").arg(config);
    bounded(c, None, deadline).map_err(|e| format!("{} -t: {e}", sshd_exe.display()))
}

/// One running process: its id, its parent's, and its image's path (empty where it can't be read).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Running {
    pub pid: u32,
    pub parent: u32,
    pub image: std::path::PathBuf,
}

/// Every running process, as one snapshot.
#[cfg(windows)]
pub fn running() -> Result<Vec<Running>, String> {
    use windows_sys::Win32::Foundation::{CloseHandle, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W,
        TH32CS_SNAPPROCESS,
    };
    // SAFETY: a snapshot handle made here and closed here; the entry struct lives across each call, its size set as
    // the API asks.
    unsafe {
        let snap = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);
        if snap == INVALID_HANDLE_VALUE {
            return Err(format!("no process snapshot: {}", std::io::Error::last_os_error()));
        }
        let mut e: PROCESSENTRY32W = std::mem::zeroed();
        e.dwSize = std::mem::size_of::<PROCESSENTRY32W>() as u32;
        let mut all = Vec::new();
        let mut more = Process32FirstW(snap, &mut e) != 0;
        while more {
            let (pid, parent) = (e.th32ProcessID, e.th32ParentProcessID);
            all.push(Running { pid, parent, image: image_of(pid).unwrap_or_default() });
            more = Process32NextW(snap, &mut e) != 0;
        }
        CloseHandle(snap);
        Ok(all)
    }
}

#[cfg(not(windows))]
pub fn running() -> Result<Vec<Running>, String> {
    Err("processes are listed on Windows only".into())
}

/// The image path of the process that `handle` (opened with query rights) is.
#[cfg(windows)]
unsafe fn image_by_handle(
    handle: windows_sys::Win32::Foundation::HANDLE,
) -> Option<std::path::PathBuf> {
    use windows_sys::Win32::System::Threading::{QueryFullProcessImageNameW, PROCESS_NAME_WIN32};
    let mut buf = [0u16; 1024];
    let mut len = buf.len() as u32;
    // SAFETY: the caller's open handle; a buffer and its length that live across the call.
    let ok = unsafe {
        QueryFullProcessImageNameW(handle, PROCESS_NAME_WIN32, buf.as_mut_ptr(), &mut len)
    };
    (ok != 0).then(|| std::path::PathBuf::from(String::from_utf16_lossy(&buf[..len as usize])))
}

#[cfg(windows)]
fn image_of(pid: u32) -> Option<std::path::PathBuf> {
    use windows_sys::Win32::Foundation::CloseHandle;
    use windows_sys::Win32::System::Threading::{OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION};
    // SAFETY: a handle opened here, read through `image_by_handle` and closed here.
    unsafe {
        let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if h.is_null() {
            return None;
        }
        let image = image_by_handle(h);
        CloseHandle(h);
        image
    }
}

/// Stops process `pid` alone, not its children (a bridge listener's live sessions are its children, and stay), and
/// only while it is `image`: the check and the stop go through one open handle, so a pid reused by another program
/// is never stopped.
#[cfg(windows)]
pub fn stop_one(pid: u32, image: &Path) -> Result<(), String> {
    use windows_sys::Win32::Foundation::CloseHandle;
    use windows_sys::Win32::System::Threading::{
        OpenProcess, TerminateProcess, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_TERMINATE,
    };
    let key = |p: &Path| p.to_string_lossy().replace('/', "\\").to_lowercase();
    // SAFETY: a handle opened here, used for the image check and the stop, and closed here.
    unsafe {
        let h = OpenProcess(PROCESS_TERMINATE | PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if h.is_null() {
            return Err(format!("process {pid}: {}", std::io::Error::last_os_error()));
        }
        let is = image_by_handle(h);
        let result = match &is {
            Some(found) if key(found) == key(image) => {
                if TerminateProcess(h, 1) != 0 {
                    Ok(())
                } else {
                    Err(format!("process {pid}: {}", std::io::Error::last_os_error()))
                }
            }
            other => Err(format!(
                "process {pid} is {}, not {}: left alone",
                other.as_ref().map_or("unknown".into(), |p| p.display().to_string()),
                image.display()
            )),
        };
        CloseHandle(h);
        result
    }
}

#[cfg(not(windows))]
pub fn stop_one(pid: u32, _image: &Path) -> Result<(), String> {
    Err(format!("process {pid}: stopped on Windows only"))
}

/// `ssh -N <alias>` while the window is open: the alias's `LocalForward` to the panel's server on the PC that holds the
/// stacks (overseer's runner). Restarted when it ends, at most once in 15 s; its whole tree ends with this value, or
/// with the window however it ends.
pub struct Tunnel {
    alias: String,
    child: Option<(std::process::Child, tree::Tree)>,
    started: Option<Instant>,
    /// ssh's last line on stderr: why it ended, when it did.
    said: std::sync::Arc<std::sync::Mutex<String>>,
}

impl Tunnel {
    /// `alias` as config.txt has it, checked there to be an alias and no option.
    pub fn new(alias: &str) -> Tunnel {
        Tunnel { alias: alias.to_string(), child: None, started: None, said: Default::default() }
    }

    pub fn alias(&self) -> &str {
        &self.alias
    }

    /// Keeps it running: `None` while it runs, else why not.
    pub fn ensure(&mut self) -> Option<String> {
        use std::io::BufRead;
        use std::process::Stdio;
        if let Some((child, _)) = &mut self.child {
            match child.try_wait() {
                Ok(None) => return None,
                Ok(Some(_)) | Err(_) => self.child = None,
            }
        }
        let said = self.said.lock().map(|s| s.clone()).unwrap_or_default();
        let ended = if said.is_empty() {
            format!("ssh {} ended", self.alias)
        } else {
            format!("ssh {} ended: {said}", self.alias)
        };
        if self.started.is_some_and(|t| t.elapsed() < Duration::from_secs(15)) {
            return Some(ended);
        }
        self.started = Some(Instant::now());
        let mut c = Command::new("ssh");
        c.args(["-N", "-o", "BatchMode=yes", "-o", "ExitOnForwardFailure=yes"])
            .args(["-o", "ServerAliveInterval=15", "-o", "ConnectTimeout=10"])
            .arg(&self.alias)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped());
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            const CREATE_NO_WINDOW: u32 = 0x0800_0000;
            c.creation_flags(CREATE_NO_WINDOW);
        }
        let mut child = match c.spawn() {
            Ok(child) => child,
            Err(e) => return Some(format!("ssh can't start: {e}")),
        };
        let tree = match tree::Tree::holding(&child) {
            Ok(t) => t,
            Err(e) => {
                let _ = child.kill();
                let _ = child.wait();
                return Some(e);
            }
        };
        if let Some(err) = child.stderr.take() {
            let said = self.said.clone();
            std::thread::spawn(move || {
                for line in std::io::BufReader::new(err).lines().map_while(Result::ok) {
                    if !line.trim().is_empty() {
                        if let Ok(mut s) = said.lock() {
                            *s = line.trim().to_string();
                        }
                    }
                }
            });
        }
        self.child = Some((child, tree));
        None
    }
}

impl Drop for Tunnel {
    fn drop(&mut self) {
        if let Some((mut child, tree)) = self.child.take() {
            drop(tree);
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

/// A job holding one child and what it starts, killed when the job's handle closes.
#[cfg(windows)]
mod tree {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};
    use windows_sys::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, JobObjectExtendedLimitInformation,
        SetInformationJobObject, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
        JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    };

    pub struct Tree(HANDLE);

    // The handle is only closed, from whichever thread drops it.
    unsafe impl Send for Tree {}

    impl Tree {
        pub fn holding(child: &std::process::Child) -> Result<Tree, String> {
            let failed = |call: &str| {
                format!("no job to bound the child ({call}: {})", std::io::Error::last_os_error())
            };
            // SAFETY: Win32 calls with null (default) attributes and name, a limit struct that lives across the call,
            // the job handle made here (closed by `Drop`, or here on failure), and the child's own process handle,
            // valid while `child` lives.
            unsafe {
                let job = CreateJobObjectW(std::ptr::null(), std::ptr::null());
                if job.is_null() {
                    return Err(failed("CreateJobObjectW"));
                }
                let tree = Tree(job);
                let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
                limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
                let size = std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32;
                if SetInformationJobObject(
                    job,
                    JobObjectExtendedLimitInformation,
                    (&raw const limits).cast(),
                    size,
                ) == 0
                {
                    return Err(failed("SetInformationJobObject"));
                }
                if AssignProcessToJobObject(job, child.as_raw_handle() as HANDLE) == 0 {
                    return Err(failed("AssignProcessToJobObject"));
                }
                Ok(tree)
            }
        }
    }

    impl Drop for Tree {
        fn drop(&mut self) {
            // SAFETY: the job handle this value owns, closed once.
            unsafe {
                CloseHandle(self.0);
            }
        }
    }
}

/// Elsewhere only the child itself is killed at the deadline.
#[cfg(not(windows))]
mod tree {
    pub struct Tree;

    impl Tree {
        pub fn holding(_: &std::process::Child) -> Result<Tree, String> {
            Ok(Tree)
        }
    }
}

/// Shows a path in the file manager, or opens a link in the browser; doesn't wait.
pub fn open_in_file_manager(target: &str) {
    #[cfg(windows)]
    let _ = Command::new("explorer").arg(target).spawn();
    #[cfg(not(windows))]
    let _ = Command::new("xdg-open").arg(target).spawn();
}
