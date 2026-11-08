//! The one door to child processes (Sol review r4, R4-06), as `fsguard.rs` is to the filesystem: no other runtime
//! source reaches `std::process`, by any spelling (tests/fsguard_lint.rs). Four programs start here: `git` (git.rs
//! calls it; its writing commands change only the stack repo an operation names), the file manager, which the
//! GUI asks to show a path or open a link, and which writes nothing itself, a staged copy of the guard, run once
//! for its self-test before sync installs it, and the T3 CLI, which the queue guard asks about its thread (it
//! reads; its process tree ends with the guard's). The panel's push is `git` too, bounded by a deadline that ends its
//! whole tree (`bounded`), and the window on another PC runs `ssh -N` for its forward while it is open (`Tunnel`).
//! `lords paths` asks Codex's app-server which hooks it trusts (`codex_ask`; it reads), bounded the same way; sync asks it
//! the same, and to trust Lords' own hooks after it wrote Codex's hooks file, which Codex's own writer does
//! (codex_trust.rs). The helper guard reads the arguments its session's process was started with (`command_line`).

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

/// `git_inherited`, with `stdin` if any, waiting at most `deadline`: the stores' commit check reads the index and
/// objects of the commit its hook checks (gap 11, §0d.3), within `[delivery] store_git_secs`. See `bounded`.
pub(crate) fn git_inherited_bounded(
    dir: &Path,
    args: &[&str],
    stdin: Option<Vec<u8>>,
    deadline: Duration,
) -> Result<Output, String> {
    let mut c = Command::new("git");
    c.arg("-C").arg(dir).args(args);
    for v in PATHSPEC_VARS {
        c.env_remove(v);
    }
    bounded(c, stdin, deadline).map_err(|e| format!("git {}: {e}", args.join(" ")))
}

/// Runs `git -C <dir> <args>` with these environment variables set, and waits for it (the hook probe).
pub(crate) fn git_env(dir: &Path, args: &[&str], env: &[(&str, &str)]) -> Result<Output, String> {
    let mut c = command(dir, args);
    for (k, v) in env {
        c.env(k, v);
    }
    c.output().map_err(|e| format!("git {}: {e}", args.join(" ")))
}

/// How long one run of the guard copy may take, its start included.
const GUARD_TIME: Duration = Duration::from_secs(20);

/// Runs `<exe> guard --self-test` and requires it to print `ok` and exit 0 within 20 s: the copy a harness hook will
/// run answers before sync names it (orchestrator, 30 Oct). A child past the deadline is killed. Public for tests only
/// (tests/guard_exec.rs runs it at the moment a copy is held open, which `install_guard`'s reads before it can't fix).
#[doc(hidden)]
pub fn guard_self_test(guard_copy: &Path) -> Result<(), String> {
    guard_self_test_within(guard_copy, GUARD_TIME)
}

/// `guard_self_test` within `budget` instead of 20 s: public for tests only, which can't wait 20 s for a hung copy.
#[doc(hidden)]
pub fn guard_self_test_within(guard_copy: &Path, budget: Duration) -> Result<(), String> {
    let args = ["guard", "--self-test"];
    let (status, out) = run_guard_within(guard_copy, &args, b"", "guard self-test", budget)?;
    if status.success() && out.trim() == "ok" {
        Ok(())
    } else {
        Err(format!("{}: guard self-test failed ({status}): {}", guard_copy.display(), out.trim()))
    }
}

/// The protocol of guard `which` (`child`, `role`) in the guard copy (`guard::CHILD_PROTOCOL`, `guard::ROLE_PROTOCOL`),
/// asked with exactly the arguments sync writes into its hook (`guard <which> --home <home>`, Claude Code's and Codex's
/// alike) and the probe event on stdin (fix check r2, FC4): `None` when it answers anything else, as a copy from before
/// that protocol does (no such guard, no `--home`, or no answer to the probe). `extra`: the hook's further arguments
/// (the role hook's `--adapter ID`). No `home`: a hook without `--home` (the rm hook; review r1, D16-R1-01).
pub(crate) fn guard_protocol(
    guard_copy: &Path,
    which: &str,
    home: Option<&Path>,
    extra: &[&str],
) -> Result<Option<u32>, String> {
    let home = home.map(|h| h.to_string_lossy());
    let mut args = vec!["guard", which];
    if let Some(h) = &home {
        args.extend(["--home", h.as_ref()]);
    }
    args.extend(extra);
    let probe = format!("{{\"hook_event_name\":\"{}\"}}", crate::guard::CHILD_PROBE_EVENT);
    let what = format!("guard {which} probe");
    let (status, out) = run_guard(guard_copy, &args, probe.as_bytes(), &what)?;
    let prefix = format!("lords guard {which} ");
    let answer = out.trim().strip_prefix(&prefix).and_then(|n| n.parse().ok());
    Ok(answer.filter(|_| status.success()))
}

/// The protocol of `lords deliver` in the guard copy (`deliver::PROTOCOL`), as `deliver --protocol` answers it (F8,
/// D-3): `None` when it answers anything else, as a copy from before F8 does (no such command).
pub(crate) fn deliver_protocol(guard_copy: &Path) -> Result<Option<u32>, String> {
    let (status, out) = run_guard(guard_copy, &["deliver", "--protocol"], b"", "deliver protocol")?;
    let answer = out.trim().strip_prefix("lords deliver ").and_then(|n| n.parse().ok());
    Ok(answer.filter(|_| status.success()))
}

/// The guards the guard copy runs, as `guard --guards` lists them (`guard::GUARDS`; D20): `None` when it lists none, as
/// a copy from before that list answers (the rm usage note, nothing on stdout), or answers anything else.
pub(crate) fn guard_list(guard_copy: &Path) -> Result<Option<Vec<String>>, String> {
    let (status, out) = run_guard(guard_copy, &["guard", "--guards"], b"", "guard list")?;
    let list = out
        .trim()
        .strip_prefix("lords guards ")
        .map(|l| l.split_whitespace().map(str::to_string).collect::<Vec<_>>());
    Ok(list.filter(|_| status.success()))
}

/// Asks `codex app-server` (Codex's own JSON-RPC over stdio) one question, with `codex_home` as its `CODEX_HOME`: it
/// writes `requests`, one per line, keeps stdin open (the server ends at the end of its input without answering; probe
/// H5, design note 2026-11-02-lords-role-at-launch), and gives the first line whose `id` is `id`, or why not: it can't
/// start, it ends first, or `deadline` passes (rule.remote-calls-have-deadlines). The server is in a job of its own
/// that kills every process in it when this returns, however it returns (`tree::Tree`).
pub fn codex_ask(
    codex: &Path,
    codex_home: &Path,
    requests: &[String],
    id: u64,
    deadline: Duration,
) -> Result<serde_json::Value, String> {
    use std::io::{BufRead, Write};
    use std::process::Stdio;
    let started = Instant::now();
    let mut c = Command::new(codex);
    c.arg("app-server")
        .env("CODEX_HOME", codex_home)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        c.creation_flags(CREATE_NO_WINDOW);
    }
    let mut child = c.spawn().map_err(|e| format!("{}: {e}", codex.display()))?;
    let tree = match tree::Tree::holding(&child) {
        Ok(t) => t,
        Err(e) => {
            let _ = child.kill();
            let _ = child.wait();
            return Err(e);
        }
    };
    let mut stdin = child.stdin.take();
    if let Some(pipe) = stdin.as_mut() {
        for r in requests {
            let _ = writeln!(pipe, "{r}");
        }
        let _ = pipe.flush();
    }
    let (tx, rx) = std::sync::mpsc::channel();
    let stdout = child.stdout.take();
    std::thread::spawn(move || {
        for line in stdout.into_iter().flat_map(|o| std::io::BufReader::new(o).lines()) {
            let Ok(line) = line else { break };
            if tx.send(line).is_err() {
                break;
            }
        }
    });
    let answer = loop {
        let left = deadline.saturating_sub(started.elapsed());
        match rx.recv_timeout(left) {
            Ok(line) => {
                let Ok(v) = serde_json::from_str::<serde_json::Value>(&line) else { continue };
                if v["id"].as_u64() == Some(id) {
                    break Ok(v);
                }
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                break Err(format!("no answer within its deadline ({} ms)", deadline.as_millis()))
            }
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                break Err("it ended without answering".to_string())
            }
        }
    };
    drop(stdin);
    tree.end();
    let _ = child.kill();
    let _ = child.wait();
    answer
}

/// How long a guard copy that the OS won't run yet, because something holds it open for writing, is tried again.
const BUSY_FOR: Duration = Duration::from_secs(1);

/// The pause between two starts of a copy the OS refused as busy.
const BUSY_PAUSE: Duration = Duration::from_millis(20);

/// `spawn`, tried again only while it fails because the file it runs is held open for writing (ETXTBSY, off Windows).
/// A copy written and closed a moment ago can still be held: a child another thread of this process started meanwhile
/// holds every file open then until its own exec (job lords-test-linux-84c9449e-1794157526-435833; golang/go#22315).
/// Every pause ends by `until`, and no start is tried at or past it, so the last busy refusal is the answer then (review
/// r1, R1-01); any other result is the answer at once. `now` and `sleep` are the clock (a test's own, or
/// `Instant::now` and `std::thread::sleep`). Public for tests only (tests/delivery.rs starts its copies through it).
#[doc(hidden)]
pub fn start_while_busy<T>(
    mut spawn: impl FnMut() -> std::io::Result<T>,
    until: Instant,
    now: impl Fn() -> Instant,
    sleep: impl Fn(Duration),
) -> std::io::Result<T> {
    loop {
        match spawn() {
            Err(e) if cfg!(unix) && e.kind() == std::io::ErrorKind::ExecutableFileBusy => {
                let left = until.saturating_duration_since(now());
                if left.is_zero() {
                    return Err(e);
                }
                sleep(left.min(BUSY_PAUSE));
                if now() >= until {
                    return Err(e);
                }
            }
            r => return r,
        }
    }
}

/// Runs the guard copy with `args` and `stdin`, within 20 s of the call, its start included: its exit status and
/// stdout. A child past the deadline is killed.
fn run_guard(
    guard_copy: &Path,
    args: &[&str],
    stdin: &[u8],
    what: &str,
) -> Result<(std::process::ExitStatus, String), String> {
    run_guard_within(guard_copy, args, stdin, what, GUARD_TIME)
}

/// `run_guard` within `budget`.
fn run_guard_within(
    guard_copy: &Path,
    args: &[&str],
    stdin: &[u8],
    what: &str,
    budget: Duration,
) -> Result<(std::process::ExitStatus, String), String> {
    use std::io::{Read, Write};
    // One budget from here, the start included (review r1, R1-01): a copy that starts late gets only what is left.
    let called = Instant::now();
    let deadline = called + budget;
    let secs = budget.as_secs_f32();
    let spawn = || {
        Command::new(guard_copy)
            .args(args)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            // Never read, so never a full pipe that holds the child (an older copy's usage note goes there).
            .stderr(std::process::Stdio::null())
            .spawn()
    };
    let busy_until = (called + BUSY_FOR).min(deadline);
    let mut child = start_while_busy(spawn, busy_until, Instant::now, std::thread::sleep)
        .map_err(|e| format!("{}: {e}", guard_copy.display()))?;
    // Written whole and closed before the wait: the guard reads its event to the end first.
    if let Some(mut i) = child.stdin.take() {
        let _ = i.write_all(stdin);
    }
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
                return Err(format!("{}: {what} took over {secs} s", guard_copy.display()));
            }
            None => std::thread::sleep(std::time::Duration::from_millis(20)),
        }
    };
    let left = deadline.saturating_duration_since(std::time::Instant::now());
    let out = rx.recv_timeout(left).map_err(|_| {
        format!("{}: {what}'s output not closed within {secs} s", guard_copy.display())
    })?;
    Ok((status, out))
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
    git_input_bounded(dir, args, stdin, Duration::from_secs(60))
}

/// Runs `git -C <dir> <args>` with `stdin` as its input, and waits at most `deadline` (a store's capture: its blobs by
/// `cat-file --batch`, within `[delivery] store_git_secs`). See `bounded`.
pub(crate) fn git_input_bounded(
    dir: &Path,
    args: &[&str],
    stdin: Vec<u8>,
    deadline: Duration,
) -> Result<Output, String> {
    bounded(command(dir, args), Some(stdin), deadline)
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
                tree.end();
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!("no answer within its deadline ({} s)", deadline.as_secs()));
            }
            None => std::thread::sleep(Duration::from_millis(20)),
        }
    };
    // Whatever the child started ends now, so the pipes close.
    tree.end();
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

/// The arguments process `pid` (one of this user's) was started with, read only: its command line
/// (`NtQueryInformationProcess`, `ProcessCommandLineInformation`) split as its program reads it (`split_command_line`).
/// Every failure is an error, and so is any other system than Windows, as for `running`. The helper guard's T3 test
/// reads its session's process with it (t3.rs).
#[cfg(windows)]
pub fn command_line(pid: u32) -> Result<Vec<String>, String> {
    use windows_sys::Wdk::System::Threading::{
        NtQueryInformationProcess, ProcessCommandLineInformation,
    };
    use windows_sys::Win32::Foundation::{CloseHandle, UNICODE_STRING};
    use windows_sys::Win32::System::Threading::{OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION};
    let header = std::mem::size_of::<UNICODE_STRING>();
    // The answer: its header, then the line, at most 32,767 UTF-16 units (CreateProcess's limit) and its end.
    let mut buf = vec![0u64; (header + 2 * 32_768).div_ceil(8)];
    let (start, end) = (buf.as_ptr() as usize, buf.as_ptr() as usize + buf.len() * 8);
    let why = |what: String| format!("process {pid}: {what}");
    // SAFETY: a handle opened here and closed right after the one query; the buffer is this function's, aligned for
    // the `UNICODE_STRING` the query writes at its start, and outlives every read of it; the string the header points
    // at is read only once it lies whole inside the buffer, after the header.
    unsafe {
        let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if h.is_null() {
            return Err(why(std::io::Error::last_os_error().to_string()));
        }
        let mut got = 0u32;
        let size = (buf.len() * 8) as u32;
        let status = NtQueryInformationProcess(
            h,
            ProcessCommandLineInformation,
            buf.as_mut_ptr().cast(),
            size,
            &mut got,
        );
        CloseHandle(h);
        if status != 0 {
            return Err(why(format!("its command line can't be read (NTSTATUS {status:#010x})")));
        }
        let line = &*buf.as_ptr().cast::<UNICODE_STRING>();
        let (at, len) = (line.Buffer as usize, usize::from(line.Length));
        if at < start + header || at % 2 != 0 || len % 2 != 0 || at + len > end {
            return Err(why("its command line came back malformed".into()));
        }
        let units = std::slice::from_raw_parts(line.Buffer, len / 2);
        Ok(split_command_line(&String::from_utf16_lossy(units)))
    }
}

#[cfg(not(windows))]
pub fn command_line(pid: u32) -> Result<Vec<String>, String> {
    Err(format!("process {pid}: command lines are read on Windows only"))
}

/// A Windows command line as the arguments its program reads, by the C runtime's rules (Microsoft Learn, "Parsing C++
/// command-line arguments"; `CommandLineToArgvW` alike after the program name). The program name runs to the first
/// space or tab outside quotes, and its quotes only open and close. After it, spaces and tabs outside quotes part
/// arguments; `2n` backslashes before a quote are `n` and the quote opens or closes, `2n+1` are `n` and a literal quote;
/// two quotes inside quotes are one; any other backslash is itself.
pub fn split_command_line(line: &str) -> Vec<String> {
    let mut chars = line.chars().peekable();
    let blank = |c: char| c == ' ' || c == '\t';
    if line.chars().all(blank) {
        return Vec::new();
    }
    let (mut name, mut quoted) = (String::new(), false);
    while let Some(&c) = chars.peek() {
        if blank(c) && !quoted {
            break;
        }
        chars.next();
        if c == '"' {
            quoted = !quoted;
        } else {
            name.push(c);
        }
    }
    let mut args = vec![name];
    loop {
        while chars.next_if(|c| blank(*c)).is_some() {}
        if chars.peek().is_none() {
            return args;
        }
        let (mut arg, mut quoted) = (String::new(), false);
        while let Some(&c) = chars.peek() {
            if blank(c) && !quoted {
                break;
            }
            chars.next();
            match c {
                '\\' => {
                    let mut n = 1;
                    while chars.next_if_eq(&'\\').is_some() {
                        n += 1;
                    }
                    let before_quote = chars.peek() == Some(&'"');
                    arg.extend(std::iter::repeat_n('\\', if before_quote { n / 2 } else { n }));
                    if before_quote && n % 2 == 1 {
                        chars.next();
                        arg.push('"');
                    }
                }
                '"' if quoted && chars.peek() == Some(&'"') => {
                    chars.next();
                    arg.push('"');
                }
                '"' => quoted = !quoted,
                c => arg.push(c),
            }
        }
        args.push(arg);
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
            tree.end();
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

    impl Tree {
        /// Ends every process in the job now (closing the job kills them), where the caller says so.
        pub fn end(self) {
            drop(self);
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

        /// Nothing to end here: no job holds the child's processes (D27).
        pub fn end(self) {}
    }
}

/// Shows a path in the file manager, or opens a link in the browser; doesn't wait.
pub fn open_in_file_manager(target: &str) {
    #[cfg(windows)]
    let _ = Command::new("explorer").arg(target).spawn();
    #[cfg(not(windows))]
    let _ = Command::new("xdg-open").arg(target).spawn();
}
