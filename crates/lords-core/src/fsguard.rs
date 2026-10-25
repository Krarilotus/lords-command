//! The one door to the filesystem for everything that changes it (design: Ownership boundary). Every call first
//! refuses a reparse point (a junction or a symlink) anywhere from the drive root down to the path, so no read, write,
//! move or delete here ever lands somewhere other than the path it names. Nothing here overwrites: a new file or dir
//! appears only where nothing was, and replacing means moving the old one aside first. Deletes follow nothing.
//!
//! A test (tests/fsguard.rs) fails if any other source file of the crate writes, renames, copies or removes.

use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::fs;
use std::io::Write;
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

/// A dir's contents, as sync builds them: each file's path inside (with `/`) and its bytes.
pub type Tree = BTreeMap<String, Vec<u8>>;

pub fn sha(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

/// A tree's hash: its files' paths and hashes, in order. A dir on disk and a tree in memory hash alike.
pub fn tree_sha(tree: &Tree) -> String {
    let lines: String = tree.iter().map(|(r, b)| format!("{}  {r}\n", sha(b))).collect();
    sha(lines.as_bytes())
}

fn is_link(m: &fs::Metadata) -> bool {
    m.file_type().is_symlink()
}

/// Refuses `path` when it, or any dir above it up to the drive root, is a reparse point.
pub fn check(path: &Path) -> Result<(), String> {
    for p in path.ancestors() {
        if p.as_os_str().is_empty() {
            continue;
        }
        if fs::symlink_metadata(p).is_ok_and(|m| is_link(&m)) {
            return Err(format!(
                "{} is a link (junction or symlink); sync goes nowhere through one",
                p.display()
            ));
        }
    }
    Ok(())
}

/// What is at a path (checked first).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Missing,
    File,
    Dir,
}

pub fn kind(path: &Path) -> Result<Kind, String> {
    check(path)?;
    match fs::symlink_metadata(path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Kind::Missing),
        Err(e) => Err(format!("{}: {e}", path.display())),
        Ok(m) if m.is_dir() => Ok(Kind::Dir),
        Ok(m) if m.is_file() => Ok(Kind::File),
        Ok(_) => Err(format!("{} is neither a file nor a dir", path.display())),
    }
}

/// A file's bytes, `None` when nothing is there; a dir is an error.
pub fn read(path: &Path) -> Result<Option<Vec<u8>>, String> {
    match kind(path)? {
        Kind::Missing => Ok(None),
        Kind::Dir => Err(format!("{} is a dir where a file goes", path.display())),
        Kind::File => fs::read(path).map(Some).map_err(|e| format!("{}: {e}", path.display())),
    }
}

/// A file's text; anything that isn't UTF-8 is an error, never taken for a missing file.
pub fn read_text(path: &Path) -> Result<Option<String>, String> {
    match read(path)? {
        None => Ok(None),
        Some(b) => String::from_utf8(b).map(Some).map_err(|_| {
            format!("{}: not UTF-8 text (UTF-16 or binary?); sync doesn't edit it", path.display())
        }),
    }
}

/// A dir's contents, read without following anything: an error when a link is anywhere inside.
pub fn read_tree(dir: &Path) -> Result<Tree, String> {
    check(dir)?;
    let mut tree = Tree::new();
    fn walk(root: &Path, dir: &Path, tree: &mut Tree) -> Result<(), String> {
        let mut children: Vec<PathBuf> = fs::read_dir(dir)
            .map_err(|e| format!("{}: {e}", dir.display()))?
            .map(|e| e.map(|e| e.path()).map_err(|e| e.to_string()))
            .collect::<Result<_, _>>()?;
        children.sort();
        for c in children {
            let m = fs::symlink_metadata(&c).map_err(|e| format!("{}: {e}", c.display()))?;
            if is_link(&m) {
                return Err(format!(
                    "{} is a link inside {}; sync follows none",
                    c.display(),
                    root.display()
                ));
            } else if m.is_dir() {
                walk(root, &c, tree)?;
            } else if m.is_file() {
                let rel = c.strip_prefix(root).map_err(|e| e.to_string())?;
                let bytes = fs::read(&c).map_err(|e| format!("{}: {e}", c.display()))?;
                tree.insert(rel.to_string_lossy().replace('\\', "/"), bytes);
            }
        }
        Ok(())
    }
    walk(dir, dir, &mut tree)?;
    Ok(tree)
}

/// A name for a file or dir beside `path` that this process makes for one step: never reused.
fn beside(path: &Path, what: &str) -> Result<PathBuf, String> {
    static N: AtomicUsize = AtomicUsize::new(0);
    let dir = path.parent().ok_or_else(|| format!("{}: no parent dir", path.display()))?;
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let n = N.fetch_add(1, Ordering::SeqCst);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    Ok(dir.join(format!(".{name}.lords-{what}-{}-{nanos}-{n}", std::process::id())))
}

pub fn create_dir_all(dir: &Path) -> Result<(), String> {
    check(dir)?;
    fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    check(dir)
}

/// Writes a new file where nothing is; refused when anything is there. Its bytes are on disk when this returns.
pub fn create_file(path: &Path, bytes: &[u8]) -> Result<(), String> {
    check(path)?;
    let mut f = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|e| format!("{}: {e}", path.display()))?;
    f.write_all(bytes).and_then(|()| f.sync_all()).map_err(|e| format!("{}: {e}", path.display()))
}

/// Appends one line to a file that is the tool's alone (a backup manifest); its bytes are on disk when this returns.
pub fn append_line(path: &Path, line: &str) -> Result<(), String> {
    check(path)?;
    let mut f = fs::OpenOptions::new()
        .append(true)
        .open(path)
        .map_err(|e| format!("{}: {e}", path.display()))?;
    writeln!(f, "{line}")
        .and_then(|()| f.sync_all())
        .map_err(|e| format!("{}: {e}", path.display()))
}

/// Puts a file at `path`, where nothing may be: written beside it, then linked in (which fails if something
/// appeared meanwhile), so `path` is absent or whole.
pub fn put_file(path: &Path, bytes: &[u8]) -> Result<(), String> {
    check(path)?;
    if let Some(dir) = path.parent() {
        create_dir_all(dir)?;
    }
    let staged = beside(path, "new")?;
    create_file(&staged, bytes)?;
    let linked = fs::hard_link(&staged, path).or_else(|e| match e.kind() {
        // A volume without hardlinks: rename, after making sure nothing is there.
        std::io::ErrorKind::Unsupported | std::io::ErrorKind::Other if !path.exists() => {
            fs::rename(&staged, path)
        }
        _ => Err(e),
    });
    let _ = fs::remove_file(&staged);
    linked.map_err(|e| format!("{}: {e}", path.display()))
}

/// Builds `tree` as a new dir beside `path` and moves it into place, where nothing may be. Returns the hash of
/// what was built, read back from disk.
pub fn put_tree(path: &Path, tree: &Tree) -> Result<String, String> {
    check(path)?;
    if let Some(dir) = path.parent() {
        create_dir_all(dir)?;
    }
    let staged = beside(path, "new")?;
    fs::create_dir(&staged).map_err(|e| format!("{}: {e}", staged.display()))?;
    let built = (|| -> Result<String, String> {
        for (rel, bytes) in tree {
            let to = inside(&staged, rel)?;
            if let Some(d) = to.parent() {
                fs::create_dir_all(d).map_err(|e| format!("{}: {e}", d.display()))?;
            }
            create_file(&to, bytes)?;
        }
        let hash = tree_sha(&read_tree(&staged)?);
        if hash != tree_sha(tree) {
            return Err(format!(
                "{}: what was built differs from what was planned",
                path.display()
            ));
        }
        Ok(hash)
    })();
    let done = built.and_then(|hash| {
        if fs::symlink_metadata(path).is_ok() {
            return Err(format!("{}: something appeared there meanwhile", path.display()));
        }
        fs::rename(&staged, path).map_err(|e| format!("{}: {e}", path.display())).map(|()| hash)
    });
    if done.is_err() {
        // Only the dir this call made, and nothing in it is a link: it was built here, file by file.
        let _ = delete_tree(&staged);
    }
    done
}

/// A path inside `root` for `rel`, which must stay inside (no `..`, no root or drive).
fn inside(root: &Path, rel: &str) -> Result<PathBuf, String> {
    let r = Path::new(rel);
    if rel.is_empty() || r.components().any(|c| !matches!(c, Component::Normal(_))) {
        return Err(format!("{rel:?}: a path that leaves its dir"));
    }
    Ok(root.join(r))
}

/// Moves what is at `path` to a fresh name beside it and returns that name. Nothing is ever overwritten.
pub fn move_aside(path: &Path) -> Result<PathBuf, String> {
    check(path)?;
    let aside = beside(path, "old")?;
    fs::rename(path, &aside).map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(aside)
}

/// Moves `from` back to `to`, where nothing may be (undoing a `move_aside`).
pub fn move_back(from: &Path, to: &Path) -> Result<(), String> {
    check(from)?;
    check(to)?;
    if fs::symlink_metadata(to).is_ok() {
        return Err(format!(
            "{}: something is there now; {} keeps the old one",
            to.display(),
            from.display()
        ));
    }
    fs::rename(from, to).map_err(|e| format!("{}: {e}", to.display()))
}

/// Moves a file or dir to `to`, a new path (into a backup, say); nothing is overwritten.
pub fn move_to(from: &Path, to: &Path) -> Result<(), String> {
    check(from)?;
    check(to)?;
    if let Some(dir) = to.parent() {
        create_dir_all(dir)?;
    }
    if fs::symlink_metadata(to).is_ok() {
        return Err(format!("{} exists; nothing is overwritten", to.display()));
    }
    fs::rename(from, to).map_err(|e| format!("{}: {e}", from.display()))
}

/// Replaces the file at `path` (or puts it where nothing is): the old one moves aside, the new one goes in, and the
/// old one is deleted. For files that are the tool's alone: its ledger, a backup manifest.
pub fn replace_own_file(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let old = match kind(path)? {
        Kind::Missing => None,
        Kind::File => Some(move_aside(path)?),
        Kind::Dir => return Err(format!("{} is a dir", path.display())),
    };
    if let Err(e) = put_file(path, bytes) {
        if let Some(old) = &old {
            let _ = move_back(old, path);
        }
        return Err(e);
    }
    if let Some(old) = old {
        delete_file(&old)?;
    }
    Ok(())
}

pub fn delete_file(path: &Path) -> Result<(), String> {
    match kind(path)? {
        Kind::File => fs::remove_file(path).map_err(|e| format!("{}: {e}", path.display())),
        Kind::Missing => Ok(()),
        Kind::Dir => Err(format!("{} is a dir", path.display())),
    }
}

/// Deletes a dir and everything in it, following nothing: refused when it, a dir above it, or anything inside is a
/// link; then file by file, bottom up, each entry checked again just before it goes.
pub fn delete_tree(dir: &Path) -> Result<(), String> {
    read_tree(dir)?;
    fn walk(dir: &Path) -> Result<(), String> {
        for e in fs::read_dir(dir).map_err(|e| format!("{}: {e}", dir.display()))? {
            let p = e.map_err(|e| e.to_string())?.path();
            let m = fs::symlink_metadata(&p).map_err(|e| format!("{}: {e}", p.display()))?;
            if is_link(&m) {
                return Err(format!("{} became a link; stopped", p.display()));
            } else if m.is_dir() {
                walk(&p)?;
                fs::remove_dir(&p).map_err(|e| format!("{}: {e}", p.display()))?;
            } else {
                fs::remove_file(&p).map_err(|e| format!("{}: {e}", p.display()))?;
            }
        }
        Ok(())
    }
    walk(dir)?;
    check(dir)?;
    fs::remove_dir(dir).map_err(|e| format!("{}: {e}", dir.display()))
}

/// Makes a file executable (Unix); a no-op elsewhere.
pub fn make_executable(path: &Path) -> Result<(), String> {
    check(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o755))
            .map_err(|e| format!("{}: {e}", path.display()))?;
    }
    Ok(())
}
