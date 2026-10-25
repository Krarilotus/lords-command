//! The one door to the filesystem for everything that changes it (design: Ownership boundary). Every call first
//! refuses a reparse point (a junction or a symlink) anywhere from the drive root down to the path, so no read, write,
//! move or delete here ever lands somewhere other than the path it names. Nothing here overwrites: a new file or dir
//! appears only where nothing was, and replacing means moving the old one aside first. Deletes follow nothing.
//!
//! The boundary, exactly (Sol review r3, R3-08; r4, R4-06), which tests/fsguard_lint.rs enforces: in the run-time
//! sources of the library, the CLI and the GUI (`src/` of each crate), only this file reaches `std::fs` (and the
//! platform `std::os` extensions), and only process.rs reaches `std::process`, by any spelling: no path through
//! them, no grouped or renamed `use` of them, no renamed `std`. Every other source reads and writes through here:
//! the checked calls for anything sync decides on, and the plain reads below (which follow links, as std does) for
//! the stacks, configs and files only shown or scanned. Build scripts (Cargo's `OUT_DIR`) and tests (their own temp
//! dirs) are outside it.
//!
//! Each check here and the action after it are separate steps by path, not one step on an open handle: a process
//! that swaps a checked dir for a junction in between could redirect the action. The design (Ownership boundary,
//! "Operating assumption") says why that is accepted for now and what apply checks after each action.

use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::fs;
use std::io::Write;
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

/// A dir's contents, as sync builds them: each file's path inside (with `/`) and its bytes, and each dir inside,
/// empty or not, as its path with a final `/` and no bytes, so a dir added to a copy changes its hash (Sol review
/// r3, R3-01).
pub type Tree = BTreeMap<String, Vec<u8>>;

/// Whether a tree's entry is a dir (its path ends in `/`).
pub fn is_dir_entry(rel: &str) -> bool {
    rel.ends_with('/')
}

pub fn sha(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

/// A tree's hash: its entries' paths and hashes, in order (a dir's is that of no bytes; its final `/` sets it
/// apart). A dir on disk and a tree in memory hash alike.
pub fn tree_sha(tree: &Tree) -> String {
    let lines: String = tree.iter().map(|(r, b)| format!("{}  {r}\n", sha(b))).collect();
    sha(lines.as_bytes())
}

fn is_link(m: &fs::Metadata) -> bool {
    m.file_type().is_symlink()
}

/// Refuses `path` when it, or any dir above it up to the drive root, is a reparse point, or when what it is can't be
/// told: only "not found" means a part isn't there yet; any other error refuses (orchestrator, 25 Oct: the check
/// failed open on a metadata error).
pub fn check(path: &Path) -> Result<(), String> {
    for p in path.ancestors() {
        if p.as_os_str().is_empty() {
            continue;
        }
        match fs::symlink_metadata(p) {
            Ok(m) if is_link(&m) => {
                return Err(format!(
                    "{} is a link (junction or symlink); sync goes nowhere through one",
                    p.display()
                ))
            }
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => {
                return Err(format!(
                    "{}: {e}; sync can't tell whether it is a link, so it goes nowhere through it",
                    p.display()
                ))
            }
        }
    }
    Ok(())
}

// ---------------------------------------------------------------- plain reads
//
// For the stacks, configs and files sync only shows or scans: like std's, they follow links. Nothing sync decides
// to write, move or delete is read through these.

/// A file's bytes (std's `fs::read`).
pub fn read_bytes(path: impl AsRef<Path>) -> std::io::Result<Vec<u8>> {
    fs::read(path)
}

/// A file's text (std's `fs::read_to_string`).
pub fn read_string(path: impl AsRef<Path>) -> std::io::Result<String> {
    fs::read_to_string(path)
}

/// The paths in a dir, unsorted; none when it can't be read.
pub fn list_dir(dir: &Path) -> Vec<PathBuf> {
    fs::read_dir(dir).into_iter().flatten().flatten().map(|e| e.path()).collect()
}

/// What is at a path, not following a link there: `Some(true)` a link, `Some(false)` anything else, `None` when
/// it can't be read.
pub fn is_link_at(path: &Path) -> Option<bool> {
    fs::symlink_metadata(path).ok().map(|m| is_link(&m))
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
            }
            let rel = c.strip_prefix(root).map_err(|e| e.to_string())?;
            let rel = rel.to_string_lossy().replace('\\', "/");
            if m.is_dir() {
                tree.insert(format!("{rel}/"), Vec::new());
                walk(root, &c, tree)?;
            } else if m.is_file() {
                let bytes = fs::read(&c).map_err(|e| format!("{}: {e}", c.display()))?;
                tree.insert(rel, bytes);
            } else {
                return Err(format!("{} is neither a file nor a dir", c.display()));
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
    Ok(dir.join(format!(".{name}.lords-{what}-{}-{nanos}-{n}", crate::process::id())))
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
    put_file_as(path, bytes, None)
}

/// `put_file`, then the file attributes `like` (an owner file's, taken before it moved aside: Sol review r3, R3-06).
/// If they can't be set, the new file goes again, so `path` is absent or whole with them.
pub fn put_file_as(path: &Path, bytes: &[u8], like: Option<&Attrs>) -> Result<(), String> {
    check(path)?;
    if let Some(dir) = path.parent() {
        create_dir_all(dir)?;
    }
    let staged = beside(path, "new")?;
    create_file(&staged, bytes)?;
    // Linking in is the only way a file appears at a sync path: on a volume without hard links sync refuses, never
    // writes in place (Sol review r4, R4-04: that left a partial file where the old one had to go back).
    let linked = fs::hard_link(&staged, path);
    let _ = fs::remove_file(&staged);
    linked.map_err(|e| match e.kind() {
        std::io::ErrorKind::AlreadyExists => format!("{}: {e}", path.display()),
        _ => format!(
            "{}: {e}; sync puts a file in place only as a hard link, so it writes only on volumes that have them",
            path.display()
        ),
    })?;
    if let Some(a) = like {
        if let Err(e) = set_attrs(path, a) {
            fs::remove_file(path).map_err(|d| format!("{e}; and {}: {d}", path.display()))?;
            return Err(e);
        }
    }
    Ok(())
}

/// A file's attributes that an owner may have set: on Windows read-only, hidden, system, archive and
/// not-indexed; elsewhere its mode.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Attrs(u32);

impl Attrs {
    /// As a backup manifest stores them (Sol review r4, R4-03): `attrs:<hex>`.
    pub fn text(&self) -> String {
        format!("attrs:{:x}", self.0)
    }

    /// From the manifest's form; `None` for anything else.
    pub fn parse(text: &str) -> Option<Attrs> {
        u32::from_str_radix(text.strip_prefix("attrs:")?, 16).ok().map(Attrs)
    }
}

/// The attributes of the file at `path`.
pub fn attrs(path: &Path) -> Result<Attrs, String> {
    check(path)?;
    let m = fs::symlink_metadata(path).map_err(|e| format!("{}: {e}", path.display()))?;
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        // READONLY, HIDDEN, SYSTEM, ARCHIVE, NOT_CONTENT_INDEXED: the ones SetFileAttributes takes.
        Ok(Attrs(m.file_attributes() & (0x1 | 0x2 | 0x4 | 0x20 | 0x2000)))
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        Ok(Attrs(m.permissions().mode() & 0o7777))
    }
}

fn set_attrs(path: &Path, a: &Attrs) -> Result<(), String> {
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        let wide: Vec<u16> = path.as_os_str().encode_wide().chain([0]).collect();
        // A file with no other attribute set is NORMAL (0x80); SetFileAttributes takes no 0.
        let value = if a.0 == 0 { 0x80 } else { a.0 };
        // SAFETY: `wide` is a NUL-terminated UTF-16 path that lives across the call; the call only reads it.
        let ok = unsafe {
            windows_sys::Win32::Storage::FileSystem::SetFileAttributesW(wide.as_ptr(), value)
        };
        if ok == 0 {
            return Err(format!("{}: {}", path.display(), std::io::Error::last_os_error()));
        }
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(a.0))
            .map_err(|e| format!("{}: {e}", path.display()))?;
    }
    Ok(())
}

/// Whether two paths name one file or dir, by the filesystem's identity of it (volume and file id), not by
/// spelling (Sol review r3, R3-04). Both must exist.
pub fn same_file(a: &Path, b: &Path) -> Result<bool, String> {
    Ok(identity(a)? == identity(b)?)
}

#[cfg(windows)]
fn identity(path: &Path) -> Result<(u32, u64), String> {
    use std::os::windows::fs::OpenOptionsExt;
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Storage::FileSystem::{
        GetFileInformationByHandle, BY_HANDLE_FILE_INFORMATION, FILE_FLAG_BACKUP_SEMANTICS,
        FILE_FLAG_OPEN_REPARSE_POINT,
    };
    check(path)?;
    // Opened for no access at all (only its information is read), dirs too, and never through a link.
    let f = fs::OpenOptions::new()
        .access_mode(0)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path)
        .map_err(|e| format!("{}: {e}", path.display()))?;
    // SAFETY: an all-zero BY_HANDLE_FILE_INFORMATION is a valid value of that plain C struct.
    let mut info: BY_HANDLE_FILE_INFORMATION = unsafe { std::mem::zeroed() };
    // SAFETY: the handle is open for the call (`f` lives across it) and `info` is a valid place to write to.
    let ok = unsafe { GetFileInformationByHandle(f.as_raw_handle(), &mut info) };
    if ok == 0 {
        return Err(format!("{}: {}", path.display(), std::io::Error::last_os_error()));
    }
    let index = (u64::from(info.nFileIndexHigh) << 32) | u64::from(info.nFileIndexLow);
    Ok((info.dwVolumeSerialNumber, index))
}

#[cfg(unix)]
fn identity(path: &Path) -> Result<(u64, u64), String> {
    use std::os::unix::fs::MetadataExt;
    check(path)?;
    let m = fs::symlink_metadata(path).map_err(|e| format!("{}: {e}", path.display()))?;
    Ok((m.dev(), m.ino()))
}

/// The check after an action (Sol review r3, R3-05): the path it acted on still lies inside `root` (its target, as
/// the plan placed it) with no link on the way to it. An error means the filesystem changed under the action: apply
/// stops and says where.
pub fn verify_inside(path: &Path, root: &Path) -> Result<(), String> {
    check(path).map_err(|e| format!("after acting on {}: {e}", path.display()))?;
    let (p, r) = (crate::ledger::key(path), crate::ledger::key(root));
    if p != r && !p.starts_with(&format!("{}/", r.trim_end_matches('/'))) {
        return Err(format!(
            "after acting on {}: it lies outside {}, the target the plan placed it in",
            path.display(),
            root.display()
        ));
    }
    Ok(())
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
            let to = inside(&staged, rel.trim_end_matches('/'))?;
            let dir = if is_dir_entry(rel) { Some(to.as_path()) } else { to.parent() };
            if let Some(d) = dir {
                fs::create_dir_all(d).map_err(|e| format!("{}: {e}", d.display()))?;
            }
            if !is_dir_entry(rel) {
                create_file(&to, bytes)?;
            }
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
