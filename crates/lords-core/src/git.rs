//! git through its CLI, so the user's config and hooks apply; the process starts in process.rs.

use std::path::{Path, PathBuf};

pub fn run(dir: &Path, args: &[&str]) -> Result<String, String> {
    let out = crate::process::git(dir, args)?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    } else {
        Err(format!(
            "git {} failed: {}{}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim(),
            String::from_utf8_lossy(&out.stdout).trim()
        ))
    }
}

pub fn bytes(dir: &Path, args: &[&str]) -> Result<Vec<u8>, String> {
    let out = crate::process::git(dir, args)?;
    if out.status.success() {
        Ok(out.stdout)
    } else {
        Err(format!(
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        ))
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

/// One commit in a file's history.
#[derive(Clone, Debug, PartialEq, Eq)]
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
    Ok(out
        .lines()
        .filter_map(|l| {
            let mut parts = l.split('\u{1f}');
            Some(Commit {
                hash: parts.next()?.to_string(),
                date: parts.next()?.to_string(),
                subject: parts.next().unwrap_or("").to_string(),
            })
        })
        .collect())
}

/// What one commit changed in a file, as a patch.
pub fn show(dir: &Path, hash: &str, file: &Path) -> Result<String, String> {
    let file = file.to_string_lossy();
    run(dir, &["show", "--format=", "--follow", hash, "--", &file])
        .or_else(|_| run(dir, &["show", "--format=", hash, "--", &file]))
}
