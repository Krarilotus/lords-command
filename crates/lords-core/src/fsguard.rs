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

// The door: the one place the resolved boundary lints allow (clippy.toml).
#![allow(clippy::disallowed_methods, clippy::disallowed_types)]

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
// to write, move or delete is read through these. They are strict: every error comes back (Sol review r5, R5-04;
// decision 4). The `_lossy` ones turn an error into "nothing" and are for display only: no safety decision calls
// one (the design's swallowed-errors inventory names each caller).

/// A file's bytes (std's `fs::read`).
pub fn read_bytes(path: impl AsRef<Path>) -> std::io::Result<Vec<u8>> {
    fs::read(path)
}

/// A file's text (std's `fs::read_to_string`).
pub fn read_string(path: impl AsRef<Path>) -> std::io::Result<String> {
    fs::read_to_string(path)
}

/// A file's bytes for display, `None` on any error (fonts in the GUI).
pub fn read_bytes_lossy(path: impl AsRef<Path>) -> Option<Vec<u8>> {
    fs::read(path).ok()
}

/// A file's text for display, `None` on any error.
pub fn read_string_lossy(path: impl AsRef<Path>) -> Option<String> {
    fs::read_to_string(path).ok()
}

/// The paths in a dir, unsorted. A dir that isn't there has none; any other error (the dir can't be opened, or an
/// entry can't be read while listing it) comes back.
pub fn list_dir(dir: &Path) -> Result<Vec<PathBuf>, String> {
    let entries = match fs::read_dir(dir) {
        Ok(e) => e,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(format!("{}: {e}", dir.display())),
    };
    entries.map(|e| e.map(|e| e.path()).map_err(|e| format!("{}: {e}", dir.display()))).collect()
}

/// What is at a path itself, not following a link there and not checking the dirs above (a stack or a source dir,
/// read as std reads it): `Missing`, `File` or `Dir`; a link is an error, as is anything that can't be told.
pub fn kind_here(path: &Path) -> Result<Kind, String> {
    match fs::symlink_metadata(path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Kind::Missing),
        Err(e) => Err(format!("{}: {e}", path.display())),
        Ok(m) if is_link(&m) => Err(format!("{} is a link", path.display())),
        Ok(m) if m.is_dir() => Ok(Kind::Dir),
        Ok(m) if m.is_file() => Ok(Kind::File),
        Ok(_) => Err(format!("{} is neither a file nor a dir", path.display())),
    }
}

/// Whether what is at a path is a link, not following it; an error when that can't be told.
pub fn link_at(path: &Path) -> Result<bool, String> {
    fs::symlink_metadata(path).map(|m| is_link(&m)).map_err(|e| format!("{}: {e}", path.display()))
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

fn create_dir_all_at(dir: &Path) -> Result<(), String> {
    check(dir)?;
    fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    check(dir)
}

/// Writes a new file where nothing is; refused when anything is there. Its bytes are on disk when this returns.
fn create_file_at(path: &Path, bytes: &[u8]) -> Result<(), String> {
    check(path)?;
    let mut f = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|e| format!("{}: {e}", path.display()))?;
    f.write_all(bytes).and_then(|()| f.sync_all()).map_err(|e| format!("{}: {e}", path.display()))
}

/// Appends one line to a file that is the tool's alone (a backup manifest); its bytes are on disk when this returns.
fn append_line_at(path: &Path, line: &str) -> Result<(), String> {
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
fn put_file_at(path: &Path, bytes: &[u8]) -> Result<(), String> {
    put_file_as_at(path, bytes, None)
}

#[cfg(test)]
thread_local! {
    static FAIL_LINKS: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Tests only: makes every following `hard_link` on this thread fail as on a volume without hard links (Sol review
/// r4, R4-04; r5, decision 7).
#[cfg(test)]
pub fn fail_links(on: bool) {
    FAIL_LINKS.with(|f| f.set(on));
}

fn link_in(staged: &Path, path: &Path) -> std::io::Result<()> {
    #[cfg(test)]
    if FAIL_LINKS.with(|f| f.get()) {
        return Err(std::io::Error::from(std::io::ErrorKind::Unsupported));
    }
    fs::hard_link(staged, path)
}

/// `put_file`, then the file attributes `like` (an owner file's, taken before it moved aside: Sol review r3, R3-06).
/// If they can't be set, the new file goes again, so `path` is absent or whole with them.
fn put_file_as_at(path: &Path, bytes: &[u8], like: Option<&Attrs>) -> Result<(), String> {
    check(path)?;
    if let Some(dir) = path.parent() {
        create_dir_all_at(dir)?;
    }
    let staged = beside(path, "new")?;
    create_file_at(&staged, bytes)?;
    // Linking in is the only way a file appears at a sync path: on a volume without hard links sync refuses, never
    // writes in place (Sol review r4, R4-04: that left a partial file where the old one had to go back).
    let linked = link_in(&staged, path);
    // The staging name is this run's own and never read again: a leftover is harmless (inventory: out of contract).
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

/// A file's or dir's attributes, all of them (design-simplification-r6, rule C): on Windows its attribute bits,
/// elsewhere its mode. Sync keeps exactly what it can set back and refuses the rest, by an allowlist, so a bit it
/// has never heard of refuses too.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Attrs(u32);

/// Windows bits: the ones SetFileAttributes sets back (read-only, hidden, system, archive, not-indexed), NORMAL
/// (no other bit), and DIRECTORY.
#[cfg(windows)]
const SETTABLE: u32 = 0x1 | 0x2 | 0x4 | 0x20 | 0x2000;
#[cfg(windows)]
const NORMAL: u32 = 0x80;
#[cfg(windows)]
const DIRECTORY: u32 = 0x10;

impl Attrs {
    /// As a backup manifest stores them (Sol review r4, R4-03): `attrs:<hex>`.
    pub fn text(&self) -> String {
        format!("attrs:{:x}", self.0)
    }

    /// From the manifest's form; `None` for anything else.
    pub fn parse(text: &str) -> Option<Attrs> {
        u32::from_str_radix(text.strip_prefix("attrs:")?, 16).ok().map(Attrs)
    }

    /// What a replace keeps (rule C): the attributes it can set back on the new file, or, when anything else is set
    /// (sparse, compressed, encrypted, offline, the cloud bits, a reparse point, integrity, a bit Windows adds
    /// later), an error naming it: the item refuses.
    pub fn keep(&self) -> Result<Attrs, String> {
        #[cfg(windows)]
        {
            let other = self.0 & !(SETTABLE | NORMAL | DIRECTORY);
            if other != 0 {
                return Err(format!(
                    "attributes 0x{other:x} it can't set back on a new file; sync leaves it as it is"
                ));
            }
            Ok(Attrs(self.0 & SETTABLE))
        }
        #[cfg(unix)]
        Ok(Attrs(self.0 & 0o7777))
    }

    /// Whether git alone gives this back (rule C, for promotion, whose only recovery is git): on Windows only ARCHIVE
    /// or NORMAL on a file and nothing but DIRECTORY on a dir; elsewhere a mode git restores (owner read and write,
    /// no set-id or sticky bit).
    pub fn git_restores(&self, dir: bool) -> bool {
        #[cfg(windows)]
        {
            let allowed = if dir { DIRECTORY } else { 0x20 | NORMAL };
            self.0 & !allowed == 0
        }
        #[cfg(unix)]
        {
            let _ = dir;
            self.0 & 0o7000 == 0 && self.0 & 0o600 == 0o600
        }
    }

    /// Whether only what sync itself leaves is set (ARCHIVE or NORMAL; a mode git restores): a file it created and
    /// nobody marked since.
    pub fn as_sync_leaves(&self) -> bool {
        self.git_restores(false)
    }

    /// The raw bits (Windows attributes; elsewhere the mode).
    pub fn bits(&self) -> u32 {
        self.0
    }

    /// The attributes, for a message.
    pub fn describe(&self) -> String {
        if cfg!(windows) {
            format!("attributes 0x{:x}", self.0)
        } else {
            format!("mode {:o}", self.0)
        }
    }
}

/// All the attributes of the file or dir at `path` (not following a link).
pub fn attrs(path: &Path) -> Result<Attrs, String> {
    check(path)?;
    let m = fs::symlink_metadata(path).map_err(|e| format!("{}: {e}", path.display()))?;
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        Ok(Attrs(m.file_attributes()))
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
        // Only what SetFileAttributes sets; a file with none of it is NORMAL, as SetFileAttributes takes no 0.
        let value = if a.0 & SETTABLE == 0 { NORMAL } else { a.0 & SETTABLE };
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
fn put_tree_at(path: &Path, tree: &Tree) -> Result<String, String> {
    check(path)?;
    if let Some(dir) = path.parent() {
        create_dir_all_at(dir)?;
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
                create_file_at(&to, bytes)?;
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
        if !nothing_at(path)? {
            return Err(format!("{}: something appeared there meanwhile", path.display()));
        }
        fs::rename(&staged, path).map_err(|e| format!("{}: {e}", path.display())).map(|()| hash)
    });
    if done.is_err() {
        // Only the dir this call made, and nothing in it is a link: it was built here, file by file.
        let _ = delete_tree_at(&staged);
    }
    done
}

/// Whether nothing is at `path` (not following a link there): only "not found" is nothing, and an error that
/// can't tell refuses, so a rename (which replaces on Windows) never lands on a file that is there (r5, R5-04's class).
fn nothing_at(path: &Path) -> Result<bool, String> {
    match fs::symlink_metadata(path) {
        Ok(_) => Ok(false),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(true),
        Err(e) => Err(format!("{}: {e}", path.display())),
    }
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
fn move_aside_at(path: &Path) -> Result<PathBuf, String> {
    check(path)?;
    let aside = beside(path, "old")?;
    fs::rename(path, &aside).map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(aside)
}

/// Moves `from` back to `to`, where nothing may be (undoing a `move_aside`).
fn move_back_at(from: &Path, to: &Path) -> Result<(), String> {
    check(from)?;
    check(to)?;
    if !nothing_at(to)? {
        return Err(format!(
            "{}: something is there now; {} keeps the old one",
            to.display(),
            from.display()
        ));
    }
    fs::rename(from, to).map_err(|e| format!("{}: {e}", to.display()))
}

/// Moves a file or dir to `to`, a new path (into a backup, say); nothing is overwritten.
fn move_to_at(from: &Path, to: &Path) -> Result<(), String> {
    check(from)?;
    check(to)?;
    if let Some(dir) = to.parent() {
        create_dir_all_at(dir)?;
    }
    if !nothing_at(to)? {
        return Err(format!("{} exists; nothing is overwritten", to.display()));
    }
    fs::rename(from, to).map_err(|e| format!("{}: {e}", from.display()))
}

/// Replaces the file at `path` (or puts it where nothing is): the old one moves aside, the new one goes in, and the
/// old one is deleted. For files that are the tool's alone: its ledger, a backup manifest.
fn replace_own_file_at(path: &Path, bytes: &[u8]) -> Result<(), String> {
    // The new file keeps the old one's attributes (Sol review r5, R5-05: a stack edit kept them nowhere).
    let (old, like) = match kind(path)? {
        Kind::Missing => (None, None),
        Kind::File => {
            // Kept exactly, or refused before anything moves (rule C).
            let like = attrs(path)?.keep().map_err(|e| format!("{}: {e}", path.display()))?;
            (Some(move_aside_at(path)?), Some(like))
        }
        Kind::Dir => return Err(format!("{} is a dir", path.display())),
    };
    if let Err(e) = put_file_as_at(path, bytes, like.as_ref()) {
        if let Some(old) = &old {
            return Err(match move_back_at(old, path) {
                Ok(()) => e,
                Err(back) => format!(
                    "{e}; and the old file couldn't go back ({back}): it is at {}",
                    old.display()
                ),
            });
        }
        return Err(e);
    }
    if let Some(old) = old {
        delete_file_at(&old)?;
    }
    Ok(())
}

fn delete_file_at(path: &Path) -> Result<(), String> {
    match kind(path)? {
        Kind::File => fs::remove_file(path).map_err(|e| format!("{}: {e}", path.display())),
        Kind::Missing => Ok(()),
        Kind::Dir => Err(format!("{} is a dir", path.display())),
    }
}

/// Deletes a dir and everything in it, following nothing: refused when it, a dir above it, or anything inside is a
/// link; then file by file, bottom up, each entry checked again just before it goes.
fn delete_tree_at(dir: &Path) -> Result<(), String> {
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
fn make_executable_at(path: &Path) -> Result<(), String> {
    check(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o755))
            .map_err(|e| format!("{}: {e}", path.display()))?;
    }
    Ok(())
}

// ---------------------------------------------------------------- places (design-simplification-r6, rule A)
//
// Nothing here changes a path given as a `&Path`. A change takes a `Contained`, which only `Root::join` makes from a
// root that was verified when it was made, or an `OwnerFile`, which only the config or a verified checkout names.
// So every writer names its root, and a path no root contains can't be written, by construction.

/// One name in a path below a root: not empty, no separator, not `.` or `..`, no drive or stream (`:`), no control
/// character or other character Windows refuses (`<>"|?*`), nothing NTFS would trim (a final space or dot), and no
/// Windows device name (`CON`, `PRN`, `AUX`, `NUL`, `COM0`-`COM9`, `LPT0`-`LPT9`, in any case and with any
/// extension; Sol review r7, R7-09).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Part(String);

/// A Windows device name: the part before the first dot, in any case (`COM` and `LPT` with a digit, `¹`, `²` or
/// `³`, which Windows reserves too).
fn device(name: &str) -> bool {
    let stem = name.split('.').next().unwrap_or("").to_ascii_uppercase();
    if ["CON", "PRN", "AUX", "NUL"].contains(&stem.as_str()) {
        return true;
    }
    let port = stem.strip_prefix("COM").or_else(|| stem.strip_prefix("LPT"));
    let mut rest = port.unwrap_or("").chars();
    match (rest.next(), rest.next()) {
        (Some(d), None) => d.is_ascii_digit() || matches!(d, '¹' | '²' | '³'),
        _ => false,
    }
}

impl Part {
    pub fn new(name: &str) -> Result<Part, String> {
        let bad = name.is_empty()
            || name == "."
            || name == ".."
            || name.ends_with([' ', '.'])
            || device(name)
            || name.chars().any(|c| {
                matches!(c, '/' | '\\' | ':' | '\0' | '<' | '>' | '"' | '|' | '?' | '*')
                    || c.is_control()
            });
        if bad {
            return Err(format!(
                "{name:?} is no name sync writes (a path part must be one plain name)"
            ));
        }
        Ok(Part(name.to_string()))
    }

    /// A skill's name: a slug, as the schema's ids have it (Sol review r5, R5-02).
    pub fn skill(name: &str) -> Result<Part, String> {
        if !crate::stack::is_slug(name) {
            return Err(format!(
                "{name:?} is no skill name (a slug: lowercase letters, digits, `-` and `.`)"
            ));
        }
        Part::new(name)
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// A dir sync may write below, verified when it was made: only these constructors make one.
#[derive(Clone, Debug)]
pub struct Root {
    path: PathBuf,
}

impl Root {
    fn at(path: PathBuf) -> Result<Root, String> {
        if !path.is_absolute() {
            return Err(format!("{}: not an absolute path", path.display()));
        }
        check(&path)?;
        Ok(Root { path })
    }

    /// A loaded stack's dir: it holds its `stack.toml`.
    pub fn stack(dir: &Path) -> Result<Root, String> {
        let root = Root::at(dir.to_path_buf())?;
        if kind(&root.path.join("stack.toml"))? != Kind::File {
            return Err(format!("{}: no stack.toml, so no stack", dir.display()));
        }
        Ok(root)
    }

    /// The tool's own dir (its ledger, backups and lock).
    pub fn home(dir: &Path) -> Result<Root, String> {
        Root::at(dir.to_path_buf())
    }

    /// A target's dir: a skills target's own, or the dir of an instructions target's shared (`via`) file.
    pub fn target(t: &crate::stack::Target) -> Result<Root, String> {
        let dir = match (t.kind.as_str(), &t.via) {
            ("skills", _) => crate::config::expand_home(&t.path),
            ("instructions", Some(via)) => {
                let file = crate::config::expand_home(via);
                file.parent()
                    .map(Path::to_path_buf)
                    .ok_or_else(|| format!("{}: no dir", file.display()))?
            }
            _ => return Err(format!("target {} has no dir sync writes in", t.name)),
        };
        Root::at(dir)
    }

    /// A workspace's dir.
    pub fn workspace(w: &crate::stack::Workspace) -> Result<Root, String> {
        Root::at(crate::config::expand_home(&w.path))
    }

    /// A real checkout or worktree, configured or discovered alike (Sol review r5, R5-01; r6, R6-02): git's own top
    /// level there is exactly this path, and its git dir is there. A dir that is none, a dir inside one, a stale
    /// worktree git still lists, and a git that fails are all errors.
    pub fn checkout(path: &Path) -> Result<Root, String> {
        let top = crate::git::run(path, &["rev-parse", "--show-toplevel"])
            .map_err(|e| format!("{} is no git checkout ({e})", path.display()))?;
        if crate::ledger::key(Path::new(top.trim())) != crate::ledger::key(path) {
            return Err(format!(
                "{} is no checkout's root: git's top level there is {}",
                path.display(),
                top.trim()
            ));
        }
        let git_dir = crate::git::run(path, &["rev-parse", "--absolute-git-dir"])?;
        if kind(Path::new(git_dir.trim()))? != Kind::Dir {
            return Err(format!("{}: its git dir {} is gone", path.display(), git_dir.trim()));
        }
        Root::at(path.to_path_buf())
    }

    /// A checkout's hooks dir, git's common one (every worktree runs it).
    pub fn hooks(checkout: &Root) -> Result<Root, String> {
        let common = crate::git::run(
            &checkout.path,
            &["rev-parse", "--path-format=absolute", "--git-common-dir"],
        )?;
        Root::at(PathBuf::from(common.trim()).join("hooks"))
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The place `parts` names below this root.
    pub fn join(&self, parts: &[Part]) -> Contained {
        let path = parts.iter().fold(self.path.clone(), |p, part| p.join(&part.0));
        Contained { root: self.clone(), parts: parts.to_vec(), path }
    }

    /// `path` as a place below this root, if it lies below it: its parts each a plain name (design review, point 2:
    /// only ever below a root that was verified, never a root made from the path).
    pub fn below(&self, path: &Path) -> Option<Contained> {
        let (p, r) = (crate::ledger::key(path), crate::ledger::key(&self.path));
        p.strip_prefix(&format!("{}/", r.trim_end_matches('/')))?;
        let rel = path.strip_prefix(&self.path).ok().map(Path::to_path_buf).or_else(|| {
            // The same path spelt otherwise (case, separators): its last components, as many as the key has.
            let n = p.len() - r.len();
            let tail = &p[p.len() - n + 1..];
            let count = tail.split('/').count();
            let comps: Vec<_> = path.components().collect();
            Some(comps[comps.len().checked_sub(count)?..].iter().collect())
        })?;
        let parts: Vec<Part> = rel
            .components()
            .map(|c| match c {
                Component::Normal(n) => Part::new(&n.to_string_lossy()).ok(),
                _ => None,
            })
            .collect::<Option<_>>()?;
        (!parts.is_empty()).then(|| self.join(&parts))
    }

    /// Makes the root's own dir where it isn't yet (a skills target, the tool's home, a new hooks dir).
    pub fn create(&self) -> Result<(), String> {
        create_dir_all_at(&self.path)
    }
}

/// A path below a verified root: what fsguard's changes take.
#[derive(Clone, Debug)]
pub struct Contained {
    root: Root,
    parts: Vec<Part>,
    path: PathBuf,
}

impl Contained {
    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn root(&self) -> &Root {
        &self.root
    }

    pub fn parts(&self) -> &[Part] {
        &self.parts
    }

    /// The place one more name below.
    pub fn child(&self, part: Part) -> Contained {
        let mut parts = self.parts.clone();
        parts.push(part);
        self.root.join(&parts)
    }
}

/// An owner's file sync edits in one managed part: only the config names one (an instructions or settings target's
/// path), or git does (a verified checkout's `info/exclude`).
#[derive(Clone, Debug)]
pub struct OwnerFile {
    path: PathBuf,
    kind: OwnerKind,
}

/// Which owner file an `OwnerFile` is, set only by its constructor (Sol review r7, R7-02): what an apply stage may
/// edit is decided by this, never by a label.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OwnerKind {
    /// An instructions or settings target's own file.
    Target,
    /// A checkout's git exclude file.
    Exclude,
}

impl OwnerFile {
    /// An instructions or settings target's own file.
    pub fn target(t: &crate::stack::Target) -> OwnerFile {
        OwnerFile { path: crate::config::expand_home(&t.path), kind: OwnerKind::Target }
    }

    /// Tests only: an owner file at `path`, for a unit test of an owner edit.
    #[cfg(test)]
    pub fn for_test(path: &Path) -> OwnerFile {
        OwnerFile { path: path.to_path_buf(), kind: OwnerKind::Target }
    }

    /// A checkout's git exclude file, in git's common dir.
    pub fn exclude(checkout: &Root) -> Result<OwnerFile, String> {
        let common = crate::git::run(
            &checkout.path,
            &["rev-parse", "--path-format=absolute", "--git-common-dir"],
        )?;
        Ok(OwnerFile {
            path: PathBuf::from(common.trim()).join("info").join("exclude"),
            kind: OwnerKind::Exclude,
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn kind(&self) -> OwnerKind {
        self.kind
    }
}

mod sealed {
    pub trait Sealed {}
    impl Sealed for super::Contained {}
    impl Sealed for super::OwnerFile {}
}

/// What fsguard's changes take: a `Contained` or an `OwnerFile`, and nothing else (sealed).
pub trait Place: sealed::Sealed {
    fn place(&self) -> &Path;
}

impl Place for Contained {
    fn place(&self) -> &Path {
        &self.path
    }
}

impl Place for OwnerFile {
    fn place(&self) -> &Path {
        &self.path
    }
}

/// What `move_aside` moved: it goes back where it was, or goes.
#[derive(Debug)]
pub struct Aside {
    at: PathBuf,
    from: PathBuf,
}

impl Aside {
    /// Where it is now, to read it.
    pub fn path(&self) -> &Path {
        &self.at
    }

    /// Back where it was, where nothing may be.
    pub fn back(&self) -> Result<(), String> {
        move_back_at(&self.at, &self.from)
    }

    /// Gone: a file, or a dir followed by nothing.
    pub fn delete(self) -> Result<(), String> {
        match kind(&self.at)? {
            Kind::Dir => delete_tree_at(&self.at),
            _ => delete_file_at(&self.at),
        }
    }
}

pub fn create_dir_all(p: &Contained) -> Result<(), String> {
    create_dir_all_at(&p.path)
}

/// Writes a new file where nothing is; refused when anything is there.
pub fn create_file(p: &Contained, bytes: &[u8]) -> Result<(), String> {
    create_file_at(&p.path, bytes)
}

/// Appends one line to a file that is the tool's alone (a backup manifest).
pub fn append_line(p: &Contained, line: &str) -> Result<(), String> {
    append_line_at(&p.path, line)
}

/// Puts a file where nothing may be, absent or whole.
///
/// Spec (design-simplification-r6, rule A: "fsguard's mutating calls take no `&Path`"): a plain path doesn't
/// compile, only a place built from a verified root.
///
/// ```compile_fail
/// lords_core::fsguard::put_file(std::path::Path::new("anywhere"), b"bytes").unwrap();
/// ```
pub fn put_file(p: &impl Place, bytes: &[u8]) -> Result<(), String> {
    put_file_at(p.place(), bytes)
}

/// `put_file`, then the attributes `like`.
pub fn put_file_as(p: &impl Place, bytes: &[u8], like: Option<&Attrs>) -> Result<(), String> {
    put_file_as_at(p.place(), bytes, like)
}

/// Builds `tree` beside the place and moves it in, where nothing may be; the hash of what was built.
pub fn put_tree(p: &Contained, tree: &Tree) -> Result<String, String> {
    put_tree_at(&p.path, tree)
}

/// Moves what is there to a fresh name beside it.
pub fn move_aside(p: &impl Place) -> Result<Aside, String> {
    let at = move_aside_at(p.place())?;
    Ok(Aside { at, from: p.place().to_path_buf() })
}

/// Moves a file or dir to a new place (into a backup); nothing is overwritten.
pub fn move_to(from: &impl Place, to: &Contained) -> Result<(), String> {
    move_to_at(from.place(), &to.path)
}

/// Replaces a file that is the tool's alone, keeping its attributes or refusing.
pub fn replace_own_file(p: &impl Place, bytes: &[u8]) -> Result<(), String> {
    replace_own_file_at(p.place(), bytes)
}

pub fn delete_file(p: &impl Place) -> Result<(), String> {
    delete_file_at(p.place())
}

/// Deletes a dir and everything in it, following nothing.
pub fn delete_tree(p: &Contained) -> Result<(), String> {
    delete_tree_at(&p.path)
}

pub fn make_executable(p: &Contained) -> Result<(), String> {
    make_executable_at(&p.path)
}
