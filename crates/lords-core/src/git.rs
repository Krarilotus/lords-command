//! git through its CLI, so the user's config and hooks apply.

use std::path::{Path, PathBuf};
use std::process::Command;

pub fn run(dir: &Path, args: &[&str]) -> Result<String, String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .map_err(|e| format!("git {}: {e}", args.join(" ")))?;
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
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .map_err(|e| format!("git {}: {e}", args.join(" ")))?;
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
