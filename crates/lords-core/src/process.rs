//! The one door to child processes (Sol review r4, R4-06), as `fsguard.rs` is to the filesystem: no other runtime
//! source reaches `std::process`, by any spelling (tests/fsguard_lint.rs). Four programs start here: `git` (git.rs
//! calls it; its writing commands change only the stack repo an operation names), the file manager, which the
//! GUI asks to show a path or open a link, and which writes nothing itself, a staged copy of the guard, run once
//! for its self-test before sync installs it, and the T3 CLI, which the queue guard asks about its thread (it
//! reads; its process tree ends with the guard's).

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

/// Shows a path in the file manager, or opens a link in the browser; doesn't wait.
pub fn open_in_file_manager(target: &str) {
    #[cfg(windows)]
    let _ = Command::new("explorer").arg(target).spawn();
    #[cfg(not(windows))]
    let _ = Command::new("xdg-open").arg(target).spawn();
}
