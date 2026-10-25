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
/// without the verbatim prefix and in lower case (its paths ignore case).
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
    let s = s.trim_start_matches("//?/").trim_end_matches('/').to_string();
    if cfg!(windows) {
        s.to_lowercase()
    } else {
        s
    }
}

pub fn file(home: &Path) -> PathBuf {
    home.join("state").join("ledger.toml")
}

impl Ledger {
    /// The ledger in `home`; none yet is an empty one. A ledger that can't be read is an error: sync then owns
    /// nothing it can prove, and stops.
    pub fn load(home: &Path) -> Result<Ledger, String> {
        let path = file(home);
        match fsguard::read_text(&path)? {
            None => Ok(Ledger::default()),
            Some(text) => toml::from_str(&text).map_err(|e| format!("{}: {e}", path.display())),
        }
    }

    pub fn save(&self, home: &Path) -> Result<(), String> {
        let path = file(home);
        fsguard::create_dir_all(path.parent().expect("the ledger has a dir"))?;
        let body = toml::to_string(self).map_err(|e| e.to_string())?;
        fsguard::replace_own_file(&path, format!("{HEADER}\n{body}").as_bytes())
    }

    pub fn get(&self, path: &Path) -> Option<&Owned> {
        self.owned.get(&key(path))
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
