//! Installed skills are copies, never links (orchestrator, 25 Oct, after a `git worktree remove --force` followed a
//! skill junction and emptied the stack's skill dirs; symlinks would make the tool depend on developer mode or admin
//! rights on each machine). A copy carries an ownership record, `.lords/owned.toml`: the stack entry's id, the source
//! dir and a SHA-256 per file. It says the copy is sync's, and whether anyone has changed it since. Deleting a copy
//! (with its worktree, say) can never reach the stack.

use crate::stack;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

/// The ownership record in every installed copy, relative to the copy's dir.
pub const RECORD: &str = ".lords/owned.toml";

const HEADER: &str =
    "# Lords Command: a copy of a stack skill, kept by sync (it refreshes and removes it). Edit the stack's copy.";

#[derive(Debug, Serialize, Deserialize)]
struct Owned {
    /// The stack entry the copy is of.
    entry: String,
    /// The stack dir it was copied from.
    source: String,
    /// Each file's path in the copy and its SHA-256.
    files: BTreeMap<String, String>,
}

fn sha(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn rel_name(rel: &Path) -> String {
    rel.to_string_lossy().replace('\\', "/")
}

/// The files a copy of `src` holds: its plain files, without the sidecar and the record dir (links never
/// followed), sorted.
fn files_of(src: &Path) -> Vec<PathBuf> {
    let mut files: Vec<PathBuf> = stack::all_files(src)
        .into_iter()
        .filter_map(|f| f.strip_prefix(src).ok().map(Path::to_path_buf))
        .filter(|r| r != Path::new(stack::SIDECAR) && !r.starts_with(".lords"))
        .collect();
    files.sort();
    files
}

/// A copy's identity: its files and their hashes (not where it came from).
fn identity(files: &BTreeMap<String, String>) -> String {
    let lines: String = files.iter().map(|(r, h)| format!("{h}  {r}\n")).collect();
    sha(lines.as_bytes())
}

/// The id in a skill dir's sidecar, else the dir's name.
fn entry_id(src: &Path) -> String {
    fs::read_to_string(src.join(stack::SIDECAR))
        .ok()
        .and_then(|t| {
            t.lines()
                .find_map(|l| l.strip_prefix("id:").map(|v| v.trim().trim_matches('"').to_string()))
        })
        .unwrap_or_else(|| format!("skill.{}", stack::file_name(src)))
}

/// The record a fresh copy of `src` gets, and the copy's identity hash.
pub fn record_of(src: &Path) -> Result<(String, String), String> {
    let mut files = BTreeMap::new();
    for rel in files_of(src) {
        let bytes =
            fs::read(src.join(&rel)).map_err(|e| format!("{}: {e}", src.join(&rel).display()))?;
        files.insert(rel_name(&rel), sha(&bytes));
    }
    let hash = identity(&files);
    let owned =
        Owned { entry: entry_id(src), source: src.to_string_lossy().replace('\\', "/"), files };
    let body = toml::to_string(&owned).map_err(|e| e.to_string())?;
    Ok((format!("{HEADER}\n{body}"), hash))
}

/// The bytes one copy of `src` takes: its files and its record.
pub fn copy_bytes(src: &Path) -> Result<u64, String> {
    let files: u64 =
        files_of(src).iter().map(|r| fs::metadata(src.join(r)).map(|m| m.len()).unwrap_or(0)).sum();
    Ok(files + record_of(src)?.0.len() as u64)
}

/// What is at a skill's install path, as far as sync's ownership goes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum State {
    /// A copy sync made, unchanged since: its identity hash.
    Clean(String),
    /// A copy sync made that someone changed since: what differs.
    Modified(Vec<String>),
    /// A real dir without sync's record.
    NotOurs,
}

/// The state of the real dir `dir` (call it only for a dir that is no link).
pub fn state(dir: &Path) -> State {
    let Ok(text) = fs::read_to_string(dir.join(RECORD)) else { return State::NotOurs };
    if !text.starts_with(HEADER) {
        return State::NotOurs;
    }
    let owned: Owned = match toml::from_str(&text) {
        Ok(o) => o,
        Err(_) => return State::Modified(vec![format!("{RECORD} unreadable")]),
    };
    let mut changed = Vec::new();
    if stack::has_links(dir) {
        changed.push("a link inside".to_string());
    }
    let have: Vec<String> = files_of(dir).iter().map(|r| rel_name(r)).collect();
    for (rel, hash) in &owned.files {
        match fs::read(dir.join(rel)) {
            Ok(b) if sha(&b) == *hash => {}
            Ok(_) => changed.push(format!("{rel} edited")),
            Err(_) => changed.push(format!("{rel} missing")),
        }
    }
    for rel in have.iter().filter(|r| !owned.files.contains_key(*r)) {
        changed.push(format!("{rel} added"));
    }
    if changed.is_empty() {
        State::Clean(identity(&owned.files))
    } else {
        State::Modified(changed)
    }
}

/// Installs a copy of `src` at `dest`, which must not exist: built in a new dir beside it, then renamed into place,
/// so `dest` is either absent or a whole copy. Returns its identity hash.
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
        let (text, hash) = record_of(src)?;
        let record = tmp.join(RECORD);
        fs::create_dir_all(record.parent().expect("the record has a dir"))
            .map_err(|e| format!("{}: {e}", tmp.display()))?;
        fs::write(&record, text).map_err(|e| format!("{}: {e}", record.display()))?;
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

/// Removes a copy sync made, when it is still exactly the copy with identity hash `hash`.
pub fn remove_clean(dir: &Path, hash: &str) -> Result<bool, String> {
    if state(dir) != State::Clean(hash.to_string()) {
        return Ok(false);
    }
    remove_tree(dir)?;
    Ok(true)
}
