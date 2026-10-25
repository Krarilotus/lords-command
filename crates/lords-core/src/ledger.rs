//! The ledger (design: Ownership boundary): every path sync created and still owns, with the hash of what it put
//! there. It is the only proof that a path is the tool's. A path that isn't in it is not, whatever its name or
//! contents. It lives in the tool's own dir (`<home>/state/ledger.toml`) and changes only under the apply lock.

use crate::fsguard;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Component, Path, PathBuf};

const HEADER: &str =
    "# Lords Command: every path sync created and still owns. Sync rewrites this file; don't edit it by hand.";

/// One tool-owned path.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Owned {
    /// The path as sync wrote it.
    pub path: String,
    /// "file" or "dir".
    pub kind: String,
    /// What sync put there: a file's SHA-256, or a dir's tree hash (`fsguard::tree_sha`).
    pub sha: String,
    /// What it belongs to: a target (`target:<name>`) or a place (`repo:<name>`, `workspace:<name>`).
    pub scope: String,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Ledger {
    /// By `key` of the path.
    #[serde(default)]
    pub owned: BTreeMap<String, Owned>,
}

/// The one form two spellings of a path share: absolute, `.` and `..` folded, `/` between parts, and on Windows
/// without the verbatim prefix (`\\?\UNC\host\share` is `//host/share`) and with each char upper-cased on its own,
/// as NTFS compares names: by a table that maps one UTF-16 unit to one, never by Unicode's full case mapping, which
/// would merge names NTFS keeps apart (Sol review r3, R3-04: `İ.txt` and `i̇.txt`). Where a mapping takes more
/// than one char the char stays. A key that still matches two distinct files is caught by `Ledger::get`.
pub fn key(path: &Path) -> String {
    let abs = std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf());
    let mut clean = PathBuf::new();
    for c in abs.components() {
        match c {
            Component::CurDir => {}
            Component::ParentDir => {
                clean.pop();
            }
            c => clean.push(c.as_os_str()),
        }
    }
    let s = clean.to_string_lossy().replace('\\', "/");
    let s = match s.strip_prefix("//?/UNC/") {
        Some(unc) => format!("//{unc}"),
        None => s.trim_start_matches("//?/").to_string(),
    };
    let s = s.trim_end_matches('/');
    if cfg!(windows) {
        s.chars().map(upcase).collect()
    } else {
        s.to_string()
    }
}

/// One char upper-cased as NTFS's table does it: a char of the Basic Multilingual Plane whose upper case is one
/// char; any other stays.
fn upcase(c: char) -> char {
    let mut up = c.to_uppercase();
    match (u32::from(c) <= 0xFFFF, up.next(), up.next()) {
        (true, Some(u), None) if u32::from(u) <= 0xFFFF => u,
        _ => c,
    }
}

/// What a record must be, whatever its scope (Sol review r3, R3-03): a file or a dir, a SHA-256, a scope of a known
/// kind, a hook in the hooks dir its scope names. Where it must lie is sync's to check (`sync::plan`), which knows
/// the targets.
fn well_formed(o: &Owned) -> Result<(), String> {
    if o.kind != "file" && o.kind != "dir" {
        return Err(format!("kind `{}` (a file or a dir)", o.kind));
    }
    if o.sha.len() != 64 || !o.sha.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return Err(format!("sha `{}` (a SHA-256)", o.sha));
    }
    let path = Path::new(&o.path);
    if !path.is_absolute() {
        return Err("a relative path".into());
    }
    match o.scope.split_once(':') {
        Some(("hooks", dir)) => {
            let name =
                path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            let parent = path.parent().map(key).unwrap_or_default();
            if parent != dir || !crate::hooks::NAMES.contains(&name.as_str()) || o.kind != "file" {
                return Err(format!("not a hook in {dir}"));
            }
            Ok(())
        }
        Some(("target" | "repo" | "workspace", name)) if !name.is_empty() => Ok(()),
        _ => Err(format!("scope `{}` (target:, repo:, workspace: or hooks:)", o.scope)),
    }
}

pub fn file(home: &Path) -> PathBuf {
    home.join("state").join("ledger.toml")
}

impl Ledger {
    /// The ledger in `home`; none yet is an empty one. A ledger that can't be read, or holds a record that isn't
    /// well formed, is an error: sync then owns nothing it can prove, and stops. Each record is filed under the key
    /// of its path as this version computes it, so a key an earlier version wrote never decides anything; two
    /// records of one key are an error.
    pub fn load(home: &Path) -> Result<Ledger, String> {
        let path = file(home);
        let Some(text) = fsguard::read_text(&path)? else { return Ok(Ledger::default()) };
        let read: Ledger = toml::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))?;
        let mut ledger = Ledger::default();
        for o in read.owned.into_values() {
            well_formed(&o).map_err(|e| {
                format!(
                    "{}: the record of {} is wrong: {e}; sync does nothing until it is put right",
                    path.display(),
                    o.path
                )
            })?;
            let k = key(Path::new(&o.path));
            if let Some(other) = ledger.owned.insert(k, o.clone()) {
                return Err(format!(
                    "{}: {} and {} are one path; sync does nothing until the ledger is put right",
                    path.display(),
                    other.path,
                    o.path
                ));
            }
        }
        Ok(ledger)
    }

    pub fn save(&self, home: &Path) -> Result<(), String> {
        let path = file(home);
        fsguard::create_dir_all(path.parent().expect("the ledger has a dir"))?;
        let body = toml::to_string(self).map_err(|e| e.to_string())?;
        fsguard::replace_own_file(&path, format!("{HEADER}\n{body}").as_bytes())
    }

    /// The record of `path`. A record spelt otherwise (a case-only rename, say) counts only while both spellings
    /// exist and name one file by its identity (Sol review r3, R3-04): never two files whose keys merely agree.
    pub fn get(&self, path: &Path) -> Option<&Owned> {
        let o = self.owned.get(&key(path))?;
        let recorded = Path::new(&o.path);
        if recorded == path || fsguard::same_file(recorded, path).unwrap_or(false) {
            Some(o)
        } else {
            None
        }
    }

    pub fn record(&mut self, path: &Path, kind: &str, sha: &str, scope: &str) {
        self.owned.insert(
            key(path),
            Owned {
                path: path.to_string_lossy().into_owned(),
                kind: kind.into(),
                sha: sha.into(),
                scope: scope.into(),
            },
        );
    }

    pub fn forget(&mut self, path: &Path) {
        self.owned.remove(&key(path));
    }

    /// A hash of the whole ledger: apply refuses a plan made against another one.
    pub fn revision(&self) -> String {
        fsguard::sha(toml::to_string(self).unwrap_or_default().as_bytes())
    }
}
