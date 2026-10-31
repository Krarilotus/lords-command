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

/// Why `commit_entry` gives no commit of its own.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Uncommitted {
    /// Nothing was published: HEAD and the index are as they were (the hooks refused, git failed). The caller puts its
    /// own writes back.
    Refused(String),
    /// The entry changed under the change: it is committed over a newer version of a file its version covers (the
    /// message names both commits; nothing is reverted, history keeps both, the owner decides), or someone else
    /// committed the file with other bytes. Nothing of the caller's is left to put back; it reloads.
    Conflict(String),
    /// The commit is in HEAD and isn't this change as made (it carries another writer's bytes, or git couldn't be
    /// asked about it): what it says is for its owner to settle by hand.
    Published { commit: String, problem: String },
}

impl std::fmt::Display for Uncommitted {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Uncommitted::Refused(m) | Uncommitted::Conflict(m) => f.write_str(m),
            Uncommitted::Published { commit, problem } => {
                write!(f, "committed as {commit}, but {problem}")
            }
        }
    }
}

/// One file an entry's version covers: its path relative to the stack's dir, the bytes the version was read from, and
/// the bytes this change wrote to it, if it wrote it.
#[derive(Clone, Copy, Debug)]
pub struct Covered<'a> {
    pub path: &'a Path,
    pub was: &'a [u8],
    pub wrote: Option<&'a [u8]>,
}

/// Commits the files a change wrote, as one of an entry's versions (review r1 R1 and R3, the Lords owner's direction):
/// - with git's own `git commit --only -- <paths>` in the repo's own index. Git holds the index lock for the whole
///   commit, hooks included, so another committer meanwhile can't race it: git refuses that commit on the lock, and
///   it can be made again after; it moves HEAD only from the parent it read, and updates the index for these paths
///   before it lets go. The repo's normal hooks run unchanged.
/// - The commit holds the version as read: its parent holds every file the version covers, written or not, as read,
///   and it changes none the change didn't write. One that differs means the entry changed under the change: the
///   outcome is a conflict naming the change's commit and the newer one, and nothing further changes (no revert: the
///   Lords owner, fix check r1 N1, N2).
/// - The commit must hold the bytes the change wrote; a write by someone else between the change's own and git's
///   staging is committed under the change's message and said, never reported as its success.
/// - When nothing is left to commit because someone's commit (an agent's `git commit -a`) already holds exactly these
///   bytes, the last commit to touch them is the change's, if it holds the version as above (fix check r1 R3);
///   otherwise the same conflict, and that commit stays.
///
/// `None`: the change wrote nothing.
pub fn commit_entry(
    dir: &Path,
    files: &[Covered],
    message: &str,
) -> Result<Option<String>, Uncommitted> {
    use Uncommitted::{Conflict, Published, Refused};
    let top =
        toplevel(dir).ok_or_else(|| Refused(format!("{}: not in a git repo", dir.display())))?;
    let prefix = run(dir, &["rev-parse", "--show-prefix"]).map_err(Refused)?.trim().to_string();
    // The repo's paths, wherever in it the stack is (sol-loop r3, S4).
    let paths: Vec<String> = files
        .iter()
        .map(|c| format!("{prefix}{}", c.path.to_string_lossy().replace('\\', "/")))
        .collect();
    // Each blob as `git add` stores the file at its path: its filters and line ends applied (core.autocrlf, D7.6).
    let blob = |bytes: &[u8], path: &str| -> Result<String, String> {
        let args = ["hash-object", "--stdin", "--path", path];
        Ok(text(&args, crate::process::git_input(&top, &args, bytes.to_vec())?)?.trim().to_string())
    };
    // A blob in a commit, or none where the commit has no such path.
    let at = |commit: &str, path: &str| -> Option<String> {
        run(&top, &["rev-parse", "--verify", "--quiet", &format!("{commit}:{path}")])
            .ok()
            .map(|b| b.trim().to_string())
    };
    let mut bases = Vec::new();
    let mut wrote: Vec<(&str, String)> = Vec::new();
    for (c, path) in files.iter().zip(&paths) {
        bases.push(blob(c.was, path).map_err(Refused)?);
        if let Some(bytes) = c.wrote {
            wrote.push((path.as_str(), blob(bytes, path).map_err(Refused)?));
        }
    }
    if wrote.is_empty() {
        return Ok(None);
    }
    let written: Vec<&str> = wrote.iter().map(|(p, _)| *p).collect();
    // The first covered file `commit` (a full id) doesn't hold as the version read it (its parent must hold each as
    // read, and it must change none the change didn't write), with the commit that changed it there: full id, short.
    let newer = |commit: &str| -> Result<Option<(String, String, String)>, String> {
        let parent = format!("{commit}^");
        for ((c, path), base) in files.iter().zip(&paths).zip(&bases) {
            let since = if at(&parent, path).as_deref() != Some(base.as_str()) {
                parent.as_str()
            } else if c.wrote.is_none() && at(commit, path).as_deref() != Some(base.as_str()) {
                commit
            } else {
                continue;
            };
            let by = run(&top, &["log", "-1", "--format=%H %h", since, "--", path])?;
            let (full, short) = by.trim().split_once(' ').unwrap_or((by.trim(), by.trim()));
            return Ok(Some((path.clone(), full.to_string(), short.to_string())));
        }
        Ok(None)
    };
    // The conflict names the commit that changed the file and the one carrying the change: one commit, said as one.
    let conflict = |(path, by, by_short): (String, String, String), commit: &str, short: &str| {
        Conflict(if by == commit {
            format!(
                "{path} isn't as it was read: {short}, the commit that carries this change, changed it too; nothing \
                 is reverted"
            )
        } else {
            format!(
                "{path} changed in {by_short} after it was read; this change is committed as {short}, over it: both \
                 stay in history, nothing is reverted"
            )
        })
    };
    // Nothing left to commit: the files hold what HEAD holds.
    let mut same = vec!["diff", "--quiet", "HEAD", "--"];
    same.extend(&written);
    if status(&top, &same).map_err(Refused)? {
        // Someone's commit (an agent's `git commit -a`) holds them: one candidate, the last commit to touch them, named
        // once by its id; every check is on that id, and success names only it (fix check r2 N3).
        let mut last = vec!["log", "-1", "--format=%H %h", "--"];
        last.extend(&written);
        let found = run(&top, &last).map_err(Refused)?;
        let Some((id, theirs)) = found.trim().split_once(' ') else {
            return Err(Published {
                commit: "HEAD".into(),
                problem: format!("git named no commit that holds {}", written.join(", ")),
            });
        };
        if let Some((path, _)) = wrote.iter().find(|(p, b)| at(id, p).as_deref() != Some(b)) {
            return Err(Conflict(format!(
                "{path} was committed meanwhile by someone else, with other bytes"
            )));
        }
        return match newer(id) {
            Ok(None) => Ok(Some(theirs.to_string())),
            Ok(Some(changed)) => Err(conflict(changed, id, theirs)),
            Err(problem) => Err(Published { commit: theirs.to_string(), problem }),
        };
    }
    // Not quiet: git's summary line names the commit it made (sol-loop r4, S5).
    let mut commit = vec!["commit", "--only", "-m", message, "--"];
    commit.extend(&written);
    let said = run(&top, &commit).map_err(Refused)?;
    let short = said
        .lines()
        .find_map(|l| l.strip_prefix('[')?.split(']').next()?.split_whitespace().last())
        .map(str::to_string)
        .ok_or_else(|| Published {
            commit: "HEAD".into(),
            problem: format!("git named no commit: {}", said.trim()),
        })?;
    let published = |problem: String| Published { commit: short.clone(), problem };
    let made = run(&top, &["rev-parse", "--verify", &format!("{short}^{{commit}}")])
        .map_err(published)?
        .trim()
        .to_string();
    // The version over every covered file, around the commit git made on the parent it read.
    if let Some(changed) = newer(&made).map_err(published)? {
        return Err(conflict(changed, &made, &short));
    }
    if let Some((path, _)) = wrote.iter().find(|(p, b)| at(&made, p).as_deref() != Some(b.as_str()))
    {
        return Err(published(format!(
            "another writer's bytes for {path} were committed under its message; the edit's own aren't in it"
        )));
    }
    Ok(Some(short))
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
