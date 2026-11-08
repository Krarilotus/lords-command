//! A store's committed stack held in memory (gap 11, design note `2026-11-08-lords-gap11-headroom.md` §0d–§0e): what
//! whole delivery and the stores' commit check load instead of a store's checkout, so both read the same immutable
//! objects and no state of a checkout (its line endings, filters, sparse paths, unstaged edits) reaches what they
//! compose.

use crate::deliver::StoreTree;
use crate::fsguard::{self, Tree};
use crate::ledger;
use crate::stack::Source;
use std::collections::{BTreeMap, BTreeSet};
use std::io::{Error, ErrorKind};
use std::path::{Path, PathBuf};
use std::time::Duration;

/// One git read of a store, stopped past `limit` (`[delivery] store_git_secs`).
pub(crate) fn git(dir: &Path, args: &[&str], limit: Duration) -> Result<String, String> {
    let out = crate::process::git_bounded(dir, args, limit)?;
    succeeded(dir, args, &out).map(|b| String::from_utf8_lossy(&b).trim().to_string())
}

/// What a git read of a store printed, or why it failed.
fn succeeded(dir: &Path, args: &[&str], out: &crate::process::Ran) -> Result<Vec<u8>, String> {
    if !out.status.success() {
        return Err(format!(
            "{}: git {} failed: {}",
            dir.display(),
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Ok(out.stdout.clone())
}

/// The git tree of the stack committed in `dir` (a store's stack dir, its repo's root or below it), keyed by the dir.
pub(crate) fn tree(dir: &Path, limit: Duration) -> Result<StoreTree, String> {
    Ok(StoreTree { stack: ledger::key(dir), tree: git(dir, &["rev-parse", "HEAD:./"], limit)? })
}

/// Each store's committed stack, refusing a store whose stack dir holds anything not committed: edited, untracked, or
/// ignored (F8 K4; sol-loop r1 S1). Only committed entries, which passed the stores' commit hooks, reach agents: since
/// gap 11 by construction too, as a delivery loads what `capture` reads of these trees, never the checkout.
pub(crate) fn committed(dirs: &[&Path], limit: Duration) -> Result<Vec<StoreTree>, String> {
    let mut trees = Vec::new();
    for d in dirs {
        let status = "--no-optional-locks status --porcelain --untracked-files=all --ignored -- .";
        if !git(d, &status.split(' ').collect::<Vec<_>>(), limit)?.is_empty() {
            return Err(format!(
                "{} has uncommitted (edited, untracked or ignored) files in its stack, and a delivery takes committed entries only; nothing was written: the next commit delivers",
                d.display()
            ));
        }
        trees.push(tree(d, limit)?);
    }
    Ok(trees)
}

/// The stack the store at `dir` committed as `tree` (a stack dir's tree, as `tree` resolves it), read from git's
/// objects alone, each git read within `limit`: every blob's bytes as committed, with no checkout filter, and each
/// symlink or submodule as a link, at its logical path under `dir`.
pub(crate) fn capture(dir: &Path, tree: &str, limit: Duration) -> Result<Captured, String> {
    let ls = ["ls-tree", "-r", "-z", "--full-tree", tree];
    let listed = succeeded(dir, &ls, &crate::process::git_bounded(dir, &ls, limit)?)?;
    let mut kept = Captured::new(dir);
    let mut blobs: Vec<(String, String)> = Vec::new();
    for item in listed.split(|b| *b == 0).filter(|i| !i.is_empty()) {
        let item =
            std::str::from_utf8(item).map_err(|_| malformed(dir, "a path that isn't UTF-8"))?;
        let (head, path) = item.split_once('\t').ok_or_else(|| malformed(dir, item))?;
        match head.split(' ').collect::<Vec<_>>()[..] {
            ["120000", "blob", _] | ["160000", "commit", _] => kept.add_link(path)?,
            [_, "blob", oid] => blobs.push((oid.to_string(), path.to_string())),
            _ => return Err(malformed(dir, item)),
        }
    }
    if blobs.is_empty() {
        return Ok(kept);
    }
    let input: String = blobs.iter().map(|(oid, _)| format!("{oid}\n")).collect();
    let cat = ["cat-file", "--batch"];
    let out = crate::process::git_input_bounded(dir, &cat, input.into_bytes(), limit)?;
    let mut rest = &succeeded(dir, &cat, &out)?[..];
    for (oid, path) in blobs {
        let end = rest.iter().position(|b| *b == b'\n').ok_or_else(|| malformed(dir, &oid))?;
        let header = std::str::from_utf8(&rest[..end]).map_err(|_| malformed(dir, &oid))?;
        let size: usize = match header.split(' ').collect::<Vec<_>>()[..] {
            [o, "blob", n] if o == oid => n.parse().map_err(|_| malformed(dir, header))?,
            _ => return Err(malformed(dir, header)),
        };
        let body = rest.get(end + 1..end + 1 + size).ok_or_else(|| malformed(dir, header))?;
        kept.add_file(&path, body.to_vec())?;
        rest = rest.get(end + 2 + size..).ok_or_else(|| malformed(dir, header))?;
    }
    Ok(kept)
}

fn malformed(dir: &Path, what: &str) -> String {
    format!("{}: git's listing of the committed stack can't be read ({what})", dir.display())
}

/// One stack's files at their logical paths, `<root>/<path in the tree>`, where `root` is the stack dir as its store's
/// checkout has it: so what is composed from it names the paths a checkout would. Plain files hold their bytes; links
/// are known as links, and the loader refuses one where it touches it, as it reads no link's target. A path that
/// isn't here is absent: nothing is ever read from disk. Names match exactly, as git's do, whatever the filesystem; a
/// dir is here only as the dir of something in it, as git keeps no empty dir.
#[derive(Clone)]
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

    /// Everything under `dir` as `fsguard::read_tree` reads a dir on disk: each dir below it as `<rel>/`, each file
    /// with its bytes, `/` between parts; a link anywhere in it is an error, as sync follows none.
    pub(crate) fn tree(&self, dir: &Path) -> Result<Tree, String> {
        let rel = |p: &Path| {
            let parts: Vec<String> = p
                .strip_prefix(dir)
                .unwrap_or(p)
                .components()
                .map(|c| c.as_os_str().to_string_lossy().into_owned())
                .collect();
            parts.join("/")
        };
        let below = |p: &&PathBuf| p.starts_with(dir) && p.as_path() != dir;
        if let Some(l) = self.links.iter().find(below) {
            return Err(format!(
                "{} is a link inside {}; sync follows none",
                l.display(),
                dir.display()
            ));
        }
        let mut tree = Tree::new();
        for d in self.dirs.iter().filter(below) {
            tree.insert(format!("{}/", rel(d)), Vec::new());
        }
        for (f, bytes) in self.files.iter().filter(|(f, _)| below(f)) {
            tree.insert(rel(f), bytes.clone());
        }
        Ok(tree)
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

/// Its root and how much it holds, never the bytes (a `Stack` keeps its capture, and is printed in diagnostics).
impl std::fmt::Debug for Captured {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Captured")
            .field("root", &self.root)
            .field("files", &self.files.len())
            .field("links", &self.links.len())
            .finish()
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
