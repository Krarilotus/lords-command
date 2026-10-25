//! Installed skills are copies, never links (orchestrator, 25 Oct, after a `git worktree remove --force` followed a
//! skill junction and emptied the stack's skill dirs; symlinks need admin rights or developer mode on Windows). A
//! copy carries a manifest, `.lords-copy`, with one hash per file: it says the copy is sync's, and whether anyone
//! has changed it since. Deleting a copy (with its worktree, say) can never reach the stack.

use crate::stack;
use sha2::{Digest, Sha256};
use std::fs;
use std::path::{Path, PathBuf};

/// The manifest file in every installed copy.
pub const MANIFEST: &str = ".lords-copy";

const HEADER: &str =
    "# Lords Command: a copy of a stack skill, kept by sync (it refreshes and removes it). Edit the stack's copy.";

fn sha(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn rel_name(rel: &Path) -> String {
    rel.to_string_lossy().replace('\\', "/")
}

/// The files a copy of `src` holds (its plain files, the sidecar left out; links never followed), sorted.
fn files_of(src: &Path) -> Vec<PathBuf> {
    let mut files: Vec<PathBuf> = stack::all_files(src)
        .into_iter()
        .filter_map(|f| f.strip_prefix(src).ok().map(Path::to_path_buf))
        .filter(|r| r != Path::new(stack::SIDECAR) && r != Path::new(MANIFEST))
        .collect();
    files.sort();
    files
}

/// The manifest a fresh copy of `src` gets, and its hash (which identifies that copy's contents).
pub fn manifest_of(src: &Path) -> Result<(String, String), String> {
    let mut text = format!("{HEADER}\n");
    for rel in files_of(src) {
        let bytes =
            fs::read(src.join(&rel)).map_err(|e| format!("{}: {e}", src.join(&rel).display()))?;
        text.push_str(&format!("{}  {}\n", sha(&bytes), rel_name(&rel)));
    }
    let hash = sha(text.as_bytes());
    Ok((text, hash))
}

/// What is at a skill's install path, as far as sync's ownership goes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum State {
    /// A copy sync made, unchanged since: the hash of its manifest.
    Clean(String),
    /// A copy sync made that someone changed since: what differs.
    Modified(Vec<String>),
    /// A real dir without sync's manifest.
    NotOurs,
}

/// The state of the real dir `dir` (call it only for a dir that is no link).
pub fn state(dir: &Path) -> State {
    let Ok(text) = fs::read_to_string(dir.join(MANIFEST)) else { return State::NotOurs };
    if !text.starts_with(HEADER) {
        return State::NotOurs;
    }
    let mut changed = Vec::new();
    if stack::has_links(dir) {
        changed.push("a link inside".to_string());
    }
    let listed: Vec<(String, String)> = text
        .lines()
        .skip(1)
        .filter_map(|l| l.split_once("  ").map(|(h, r)| (h.to_string(), r.to_string())))
        .collect();
    let have: Vec<String> = files_of(dir).iter().map(|r| rel_name(r)).collect();
    for (hash, rel) in &listed {
        match fs::read(dir.join(rel)) {
            Ok(b) if sha(&b) == *hash => {}
            Ok(_) => changed.push(format!("{rel} edited")),
            Err(_) => changed.push(format!("{rel} missing")),
        }
    }
    for rel in have.iter().filter(|r| !listed.iter().any(|(_, l)| l == *r)) {
        changed.push(format!("{rel} added"));
    }
    if changed.is_empty() {
        State::Clean(sha(text.as_bytes()))
    } else {
        State::Modified(changed)
    }
}

/// Installs a copy of `src` at `dest`, which must not exist: built in a new dir beside it, then renamed into place,
/// so `dest` is either absent or a whole copy. Returns the manifest's hash.
pub fn install(src: &Path, dest: &Path) -> Result<String, String> {
    let parent = dest.parent().ok_or_else(|| format!("{}: no parent dir", dest.display()))?;
    fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
    let name = dest.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let tmp = parent.join(format!(".{name}.lords-{}.tmp", std::process::id()));
    if fs::symlink_metadata(&tmp).is_ok() {
        return Err(format!(
            "{} is in the way (an interrupted install?); remove it by hand",
            tmp.display()
        ));
    }
    let built = (|| -> Result<String, String> {
        fs::create_dir(&tmp).map_err(|e| format!("{}: {e}", tmp.display()))?;
        for rel in files_of(src) {
            let to = tmp.join(&rel);
            if let Some(d) = to.parent() {
                fs::create_dir_all(d).map_err(|e| format!("{}: {e}", d.display()))?;
            }
            fs::copy(src.join(&rel), &to).map_err(|e| format!("{}: {e}", to.display()))?;
        }
        let (text, hash) = manifest_of(src)?;
        fs::write(tmp.join(MANIFEST), text).map_err(|e| format!("{}: {e}", tmp.display()))?;
        Ok(hash)
    })();
    let hash = match built {
        Ok(h) => h,
        Err(e) => {
            let _ = remove_tree(&tmp);
            return Err(e);
        }
    };
    if let Err(e) = fs::rename(&tmp, dest) {
        let _ = remove_tree(&tmp);
        return Err(format!("{}: {e}", dest.display()));
    }
    Ok(hash)
}

/// Removes a dir tree that holds no links (refused otherwise): only ever a copy sync made itself.
fn remove_tree(dir: &Path) -> Result<(), String> {
    if fs::symlink_metadata(dir).is_ok_and(|m| m.file_type().is_symlink()) || stack::has_links(dir)
    {
        return Err(format!("{} is or holds a link; sync won't remove it", dir.display()));
    }
    fs::remove_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))
}

/// Removes a copy sync made, when it is still exactly the copy with manifest hash `hash`.
pub fn remove_clean(dir: &Path, hash: &str) -> Result<bool, String> {
    if state(dir) != State::Clean(hash.to_string()) {
        return Ok(false);
    }
    remove_tree(dir)?;
    Ok(true)
}
