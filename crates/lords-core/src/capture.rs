//! A store's committed stack held in memory (gap 11, design note `2026-11-08-lords-gap11-headroom.md` §0d–§0e): what
//! whole delivery and the stores' commit check load instead of a store's checkout, so both read the same immutable
//! objects and no state of a checkout (its line endings, filters, sparse paths, unstaged edits) reaches what they
//! compose.

use crate::fsguard;
use crate::stack::Source;
use std::collections::{BTreeMap, BTreeSet};
use std::io::{Error, ErrorKind};
use std::path::{Path, PathBuf};

/// One stack's files at their logical paths, `<root>/<path in the tree>`, where `root` is the stack dir as its store's
/// checkout has it: so what is composed from it names the paths a checkout would. Plain files hold their bytes; links
/// are known as links, and the loader refuses one where it touches it, as it reads no link's target. A path that
/// isn't here is absent: nothing is ever read from disk. Names match exactly, as git's do, whatever the filesystem; a
/// dir is here only as the dir of something in it, as git keeps no empty dir.
#[derive(Clone, Debug)]
pub struct Captured {
    root: PathBuf,
    files: BTreeMap<PathBuf, Vec<u8>>,
    links: BTreeSet<PathBuf>,
    dirs: BTreeSet<PathBuf>,
}

impl Captured {
    /// Nothing yet, under `root`.
    pub fn new(root: &Path) -> Captured {
        Captured {
            root: root.to_path_buf(),
            files: BTreeMap::new(),
            links: BTreeSet::new(),
            dirs: BTreeSet::from([root.to_path_buf()]),
        }
    }

    /// The stack dir its paths lie under.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// A plain file at `rel`, a path in the tree with `/` between its parts.
    pub fn add_file(&mut self, rel: &str, bytes: Vec<u8>) -> Result<(), String> {
        let path = self.place(rel)?;
        self.files.insert(path, bytes);
        Ok(())
    }

    /// A link at `rel` (a symlink or a submodule in the tree).
    pub fn add_link(&mut self, rel: &str) -> Result<(), String> {
        let path = self.place(rel)?;
        self.links.insert(path);
        Ok(())
    }

    /// The logical path of `rel`, its dirs recorded; refused for a part a tree can't hold here, for a path already
    /// there, and below a file or link.
    fn place(&mut self, rel: &str) -> Result<PathBuf, String> {
        let mut path = self.root.clone();
        let parts: Vec<&str> = rel.split('/').collect();
        for (i, part) in parts.iter().enumerate() {
            if part.is_empty() || *part == "." || *part == ".." || part.contains(['\\', ':']) {
                return Err(format!("`{rel}` isn't a plain path in a stack's tree"));
            }
            path.push(part);
            let taken = self.files.contains_key(&path) || self.links.contains(&path);
            let last = i + 1 == parts.len();
            if taken || (last && self.dirs.contains(&path)) {
                return Err(format!("`{rel}` meets another path of the tree"));
            }
            if !last {
                self.dirs.insert(path.clone());
            }
        }
        Ok(path)
    }
}

impl Source for Captured {
    fn read(&self, path: &Path) -> std::io::Result<String> {
        if let Some(bytes) = self.files.get(path) {
            // As `std::fs::read_to_string` refuses it.
            return String::from_utf8(bytes.clone()).map_err(|_| {
                Error::new(ErrorKind::InvalidData, "stream did not contain valid UTF-8")
            });
        }
        if self.links.contains(path) {
            return Err(Error::other(format!(
                "{} is a link in the captured stack",
                path.display()
            )));
        }
        if self.dirs.contains(path) {
            return Err(Error::other(format!("{} is a dir", path.display())));
        }
        Err(Error::new(
            ErrorKind::NotFound,
            format!("{} isn't in the captured stack", path.display()),
        ))
    }

    fn list(&self, dir: &Path) -> Result<Vec<PathBuf>, String> {
        if self.links.contains(dir) {
            return Err(format!("{}: a link in the captured stack, never followed", dir.display()));
        }
        if self.files.contains_key(dir) {
            return Err(format!("{}: not a dir", dir.display()));
        }
        let here = |p: &&PathBuf| p.parent() == Some(dir);
        Ok(self
            .dirs
            .iter()
            .filter(here)
            .chain(self.files.keys().filter(here))
            .chain(self.links.iter().filter(here))
            .cloned()
            .collect())
    }

    fn is_dir(&self, path: &Path) -> bool {
        self.dirs.contains(path)
    }

    fn is_file(&self, path: &Path) -> bool {
        self.files.contains_key(path)
    }

    fn link_at(&self, path: &Path) -> Result<bool, String> {
        Ok(self.links.contains(path))
    }

    fn kind_here(&self, path: &Path) -> Result<fsguard::Kind, String> {
        if self.links.contains(path) {
            Err(format!("{} is a link", path.display()))
        } else if self.dirs.contains(path) {
            Ok(fsguard::Kind::Dir)
        } else if self.files.contains_key(path) {
            Ok(fsguard::Kind::File)
        } else {
            Ok(fsguard::Kind::Missing)
        }
    }
}
