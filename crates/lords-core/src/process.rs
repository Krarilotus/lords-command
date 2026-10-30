//! The one door to child processes (Sol review r4, R4-06), as `fsguard.rs` is to the filesystem: no other runtime
//! source reaches `std::process`, by any spelling (tests/fsguard_lint.rs). Three programs start here: `git` (git.rs
//! calls it; its writing commands change only the stack repo an operation names), the file manager, which the
//! GUI asks to show a path or open a link, and which writes nothing itself, and a staged copy of the `rm` guard,
//! run once for its self-test before sync installs it.

// The door: the one place the resolved boundary lints allow (clippy.toml).
#![allow(clippy::disallowed_methods, clippy::disallowed_types)]

use std::path::Path;
use std::process::{Command, Output};

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

/// `git -C <dir> <args>` for the repo at `dir` alone: none of the variables that tie git to the repo this process
/// was started from reach it (finding F1, 27 Oct: a hook's `GIT_INDEX_FILE` made another repo's `ls-files` read
/// the commit's index).
fn command(dir: &Path, args: &[&str]) -> Command {
    let mut c = Command::new("git");
    c.arg("-C").arg(dir).args(args);
    for v in LOCAL_VARS {
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
    Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .map_err(|e| format!("git {}: {e}", args.join(" ")))
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
    let mut out = String::new();
    if let Some(mut o) = child.stdout.take() {
        let _ = o.read_to_string(&mut out);
    }
    if status.success() && out.trim() == "ok" {
        Ok(())
    } else {
        Err(format!("{}: guard self-test failed ({status}): {}", guard_copy.display(), out.trim()))
    }
}

/// Shows a path in the file manager, or opens a link in the browser; doesn't wait.
pub fn open_in_file_manager(target: &str) {
    #[cfg(windows)]
    let _ = Command::new("explorer").arg(target).spawn();
    #[cfg(not(windows))]
    let _ = Command::new("xdg-open").arg(target).spawn();
}
