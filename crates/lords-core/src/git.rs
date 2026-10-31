//! git through its CLI, so the user's config and hooks apply; the process starts in process.rs.

use std::fmt::Display;
use std::path::{Path, PathBuf};

/// A git call that didn't succeed, with how it ended (its exit status or signal, as the OS reports it) and what it
/// said: an empty message can't hide whether git ran at all (orchestrator, 26 Oct).
fn failed(args: &[&str], status: impl Display, stderr: &[u8], stdout: &[u8]) -> String {
    format!(
        "git {} failed ({status}): {}{}",
        args.join(" "),
        String::from_utf8_lossy(stderr).trim(),
        String::from_utf8_lossy(stdout).trim()
    )
}

/// Windows' STATUS_DLL_INIT_FAILED as an exit code: the new process couldn't initialise, so git never ran (it
/// happens on this machine under a peak of processes; orchestrator, 26 Oct).
const NOT_STARTED: i32 = 0xC000_0142_u32 as i32;

/// The pauses before each retry of a git that never started.
const RETRY_PAUSES: [std::time::Duration; 3] = [
    std::time::Duration::from_millis(500),
    std::time::Duration::from_secs(1),
    std::time::Duration::from_secs(2),
];

/// One git call, through `attempt`, retried only while it ends with `NOT_STARTED`: the process never ran, so a retry
/// has no side effect. At most `RETRY_PAUSES.len()` retries, each said on stderr; the last result is returned as it
/// is, so a git that never started still fails closed in its caller, and so does every other failure, at once.
fn started<T>(
    args: &[&str],
    mut attempt: impl FnMut() -> Result<T, String>,
    code: impl Fn(&T) -> Option<i32>,
    mut pause: impl FnMut(std::time::Duration),
) -> Result<T, String> {
    let mut result = attempt();
    for (n, wait) in RETRY_PAUSES.iter().enumerate() {
        match &result {
            Ok(out) if code(out) == Some(NOT_STARTED) => {}
            _ => break,
        }
        eprintln!(
            "note: git {} didn't start (0xC0000142, the process couldn't initialise); retry {} of {} in {:?}",
            args.join(" "),
            n + 1,
            RETRY_PAUSES.len(),
            wait
        );
        pause(*wait);
        result = attempt();
    }
    result
}

/// `git -C dir args`, run through `started`.
fn git(dir: &Path, args: &[&str]) -> Result<crate::process::Ran, String> {
    started(args, || crate::process::git(dir, args), |out| out.status.code(), std::thread::sleep)
}

pub fn run(dir: &Path, args: &[&str]) -> Result<String, String> {
    text(args, git(dir, args)?)
}

/// `run` on the index of the commit a hook checks: the git variables this process got reach it, so a hook of
/// `git commit <paths>` reads the commit's own temporary index. Every other call is for its repo alone (`run`).
pub fn run_index(dir: &Path, args: &[&str]) -> Result<String, String> {
    let out = started(
        args,
        || crate::process::git_inherited(dir, args),
        |out| out.status.code(),
        std::thread::sleep,
    )?;
    text(args, out)
}

fn text(args: &[&str], out: crate::process::Ran) -> Result<String, String> {
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    } else {
        Err(failed(args, out.status, &out.stderr, &out.stdout))
    }
}

pub fn bytes(dir: &Path, args: &[&str]) -> Result<Vec<u8>, String> {
    let out = git(dir, args)?;
    if out.status.success() {
        Ok(out.stdout)
    } else {
        Err(failed(args, out.status, &out.stderr, &[]))
    }
}

/// A git command that answers yes or no by its exit status (`ls-files --error-unmatch`, `check-ignore -q`,
/// `diff --quiet`, `config --get`): `true` for 0, `false` for 1, and an error for anything else, so a git that
/// failed never reads as "no" (Sol review r5, decision 1: "a failed git call never means untracked").
pub fn status(dir: &Path, args: &[&str]) -> Result<bool, String> {
    let out = git(dir, args)?;
    match out.status.code() {
        Some(0) => Ok(true),
        Some(1) => Ok(false),
        _ => Err(failed(args, out.status, &out.stderr, &out.stdout)),
    }
}

pub fn toplevel(dir: &Path) -> Option<PathBuf> {
    run(dir, &["rev-parse", "--show-toplevel"]).ok().map(|s| PathBuf::from(s.trim()))
}

/// Stages `paths` (relative to `dir`, or `.`) and commits if anything changed. Returns the new commit, if any.
pub fn commit(dir: &Path, paths: &[&Path], message: &str) -> Result<Option<String>, String> {
    let mut add = vec!["add", "-A", "--"];
    let owned: Vec<String> = paths.iter().map(|p| p.to_string_lossy().into_owned()).collect();
    add.extend(owned.iter().map(String::as_str));
    run(dir, &add)?;
    let mut diff = vec!["diff", "--cached", "--quiet", "--"];
    diff.extend(owned.iter().map(String::as_str));
    if run(dir, &diff).is_ok() {
        return Ok(None);
    }
    let mut c = vec!["commit", "-q", "-m", message, "--"];
    c.extend(owned.iter().map(String::as_str));
    run(dir, &c)?;
    Ok(Some(run(dir, &["rev-parse", "--short", "HEAD"])?.trim().to_string()))
}

/// `git -C dir args` with these variables set, run through `started`.
fn run_env(dir: &Path, args: &[&str], env: &[(&str, &str)]) -> Result<String, String> {
    let out = started(
        args,
        || crate::process::git_env(dir, args, env),
        |out| out.status.code(),
        std::thread::sleep,
    )?;
    text(args, out)
}

/// Commits exactly `files` (each a path relative to `dir`, with the bytes the commit gives it), whatever the working
/// tree holds for them by then: an agent's write that lands after an edit's own is never committed as that edit
/// (sol-loop r2, S2). The commit is made from an index of its own, HEAD's with these blobs, so the repo's hooks see
/// exactly what is committed; then the repo's index takes the commit's entries for these paths, and the working tree
/// stays as it is. A commit the hooks refuse leaves HEAD, the index and the working tree as they were. `None`: nothing
/// would change.
pub fn commit_exact(
    dir: &Path,
    files: &[(&Path, &[u8])],
    message: &str,
) -> Result<Option<String>, String> {
    let git_dir = PathBuf::from(run(dir, &["rev-parse", "--absolute-git-dir"])?.trim());
    let name = format!("lords-commit-index-{}", crate::process::id());
    let index = crate::fsguard::Root::git(&git_dir)?.join(&[crate::fsguard::Part::new(&name)?]);
    let at = index.path().to_string_lossy().into_owned();
    let env = [("GIT_INDEX_FILE", at.as_str())];
    let paths: Vec<String> =
        files.iter().map(|(p, _)| p.to_string_lossy().replace('\\', "/")).collect();
    let made = (|| -> Result<Option<String>, String> {
        run_env(dir, &["read-tree", "HEAD"], &env)?;
        for ((_, bytes), path) in files.iter().zip(&paths) {
            let args = ["hash-object", "-w", "--stdin"];
            let blob = text(&args, crate::process::git_input(dir, &args, bytes.to_vec())?)?;
            let staged = run(dir, &["ls-files", "-s", "--", path])?;
            let mode = staged.split_whitespace().next().unwrap_or("100644");
            let entry = format!("{mode},{},{path}", blob.trim());
            run_env(dir, &["update-index", "--add", "--cacheinfo", &entry], &env)?;
        }
        let unchanged =
            crate::process::git_env(dir, &["diff", "--cached", "--quiet", "HEAD"], &env)?;
        if unchanged.status.success() {
            return Ok(None);
        }
        run_env(dir, &["commit", "-q", "-m", message], &env)?;
        let mut reset = vec!["reset", "-q", "--"];
        reset.extend(paths.iter().map(String::as_str));
        run(dir, &reset)?;
        Ok(Some(run(dir, &["rev-parse", "--short", "HEAD"])?.trim().to_string()))
    })();
    let removed = crate::fsguard::delete_file(&index);
    let made = made?;
    removed?;
    Ok(made)
}

/// One commit in a file's history.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Commit {
    pub hash: String,
    pub date: String,
    pub subject: String,
}

/// A file's history, newest first, following renames.
pub fn log(dir: &Path, file: &Path) -> Result<Vec<Commit>, String> {
    let file = file.to_string_lossy();
    let out =
        run(dir, &["log", "--follow", "--date=short", "--format=%h%x1f%ad%x1f%s", "--", &file])?;
    Ok(commits(&out))
}

fn commits(out: &str) -> Vec<Commit> {
    out.lines()
        .filter_map(|l| {
            let mut parts = l.split('\u{1f}');
            Some(Commit {
                hash: parts.next()?.to_string(),
                date: parts.next()?.to_string(),
                subject: parts.next().unwrap_or("").to_string(),
            })
        })
        .collect()
}

/// The branch checked out in `dir`'s repo, its upstream (`None` when it has none, so nothing can be pushed), and the
/// commits the upstream doesn't have, newest first. A git that fails is an error, never "nothing to push".
pub fn ahead(dir: &Path) -> Result<(String, Option<String>, Vec<Commit>), String> {
    let branch = run(dir, &["rev-parse", "--abbrev-ref", "HEAD"])?.trim().to_string();
    let reference = format!("refs/heads/{branch}");
    let upstream = run(dir, &["for-each-ref", "--format=%(upstream:short)", &reference])?;
    let upstream = Some(upstream.trim().to_string()).filter(|u| !u.is_empty());
    let Some(up) = &upstream else { return Ok((branch, None, Vec::new())) };
    let range = format!("{up}..HEAD");
    let out = run(dir, &["log", "--date=short", "--format=%h%x1f%ad%x1f%s", &range])?;
    Ok((branch, upstream, commits(&out)))
}

/// `git push` of the branch checked out in `dir`'s repo to its upstream, through the repo's pre-push hook, within
/// `deadline`. What git said, or why it refused (the hook's words included).
pub fn push(dir: &Path, deadline: std::time::Duration) -> Result<String, String> {
    let args = ["push"];
    let out = crate::process::git_bounded(dir, &args, deadline)?;
    let said =
        format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
    if out.status.success() {
        Ok(said.trim().to_string())
    } else {
        Err(failed(&args, out.status, &out.stderr, &out.stdout))
    }
}

/// What one commit changed in a file, as a patch. `hash` must name a commit: `git show` of a blob or a tree prints
/// that object whatever the path (sol-loop r1 S1), so anything else is refused before it runs.
pub fn show(dir: &Path, hash: &str, file: &Path) -> Result<String, String> {
    let file = file.to_string_lossy();
    let named = format!("{hash}^{{commit}}");
    let commit = run(dir, &["rev-parse", "--verify", "--quiet", "--end-of-options", &named])
        .map_err(|_| format!("`{hash}` names no commit"))?;
    let commit = commit.trim();
    run(dir, &["show", "--format=", "--follow", commit, "--", &file])
        .or_else(|_| run(dir, &["show", "--format=", commit, "--", &file]))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    /// A fake git: each call gives the next exit code of `codes`; returns what `started` returned, how many calls
    /// it made and the pauses it took.
    fn fake(codes: &[i32]) -> (Result<Option<i32>, String>, usize, Vec<Duration>) {
        let mut calls = 0;
        let mut pauses = Vec::new();
        let result = started(
            &["rev-parse", "--show-toplevel"],
            || {
                calls += 1;
                Ok(codes.get(calls - 1).copied())
            },
            |code| *code,
            |d| pauses.push(d),
        );
        (result, calls, pauses)
    }

    // Spec (orchestrator, 26 Oct: "a bounded retry for exit code 0xC0000142 only: at most 3 attempts with a short
    // backoff (0.5, 1 and 2 s), each logged"): a git that didn't start twice and then ran is retried, after 0.5 s
    // and 1 s, and its result returned.
    #[test]
    fn a_git_that_did_not_start_is_retried() {
        let (result, calls, pauses) = fake(&[NOT_STARTED, NOT_STARTED, 0]);
        assert_eq!(result, Ok(Some(0)));
        assert_eq!(calls, 3);
        assert_eq!(pauses, vec![Duration::from_millis(500), Duration::from_secs(1)]);
    }

    // Spec (the same; "every other failure stays fail-closed as now"): a git that didn't start 4 times is given up
    // after 3 retries, and its not-started result comes back for the caller to refuse; any other failure isn't
    // retried at all.
    #[test]
    fn retries_are_bounded_and_only_for_a_git_that_did_not_start() {
        let (result, calls, pauses) = fake(&[NOT_STARTED; 5]);
        assert_eq!(result, Ok(Some(NOT_STARTED)));
        assert_eq!(calls, 4);
        assert_eq!(pauses, RETRY_PAUSES.to_vec());
        let (result, calls, pauses) = fake(&[128, 0]);
        assert_eq!(result, Ok(Some(128)));
        assert_eq!((calls, pauses.len()), (1, 0));
    }
}
