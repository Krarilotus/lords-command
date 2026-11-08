//! The one door to the filesystem for everything that changes it (design: Ownership boundary). Every call first
//! refuses a reparse point (a junction or a symlink) anywhere from the drive root down to the path, so no read, write,
//! move or delete here ever lands somewhere other than the path it names. Nothing here overwrites: a new file or dir
//! appears only where nothing was, and replacing means moving the old one aside first. Deletes follow nothing.
//!
//! The boundary, exactly (Sol review r3, R3-08; r4, R4-06), which tests/fsguard_lint.rs enforces: in the run-time
//! sources of the library, the CLI and the GUI (`src/` of each crate), only this file reaches `std::fs` (and the
//! platform `std::os` extensions), and only process.rs reaches `std::process` (and its two platform process
//! extensions, a child's handle and its creation flags), by any spelling: no path through
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

/// A file's lines in turn, each without its line end, read a piece at a time: a session's transcript, which the queue
/// guard scans, can be hundreds of MB. Any error comes back.
pub fn read_lines(path: impl AsRef<Path>, mut each: impl FnMut(&[u8])) -> std::io::Result<()> {
    use std::io::BufRead;
    let mut r = std::io::BufReader::with_capacity(1 << 16, fs::File::open(path)?);
    let mut line = Vec::new();
    while r.read_until(b'\n', &mut line)? > 0 {
        let l = line.strip_suffix(b"\n").unwrap_or(&line);
        each(l.strip_suffix(b"\r").unwrap_or(l));
        line.clear();
    }
    Ok(())
}

/// When a file was last written (std's `metadata().modified()`): the session guard's age of a record.
pub fn modified(path: impl AsRef<Path>) -> std::io::Result<std::time::SystemTime> {
    fs::metadata(path)?.modified()
}

/// A path with every link, junction and short name resolved (std's `fs::canonicalize`): `lords paths` matches a
/// session's cwd to a place by it.
pub fn canonical(path: impl AsRef<Path>) -> std::io::Result<PathBuf> {
    fs::canonicalize(path)
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

/// What reading a file in a dir found, decided by the one open (`read_text_in`).
#[derive(Debug, PartialEq, Eq)]
pub enum InDir {
    Text(String),
    /// The dir was there and held no such file.
    NoFile,
    /// No dir was there (or no dir at that path).
    NoDir,
}

/// The text of `dir`'s file `name`, read with one open, whose own answer tells a missing file from a missing dir: on
/// Windows, "file not found" means the dir was there at that instant, "path not found" that it wasn't (fix check r1,
/// FC1: a reader may never take a dir moved aside for one without the file). Elsewhere a missing dir is told by a look
/// after the open. Anything that isn't UTF-8 is an error.
pub fn read_text_in(dir: &Path, name: &str) -> Result<InDir, String> {
    let path = dir.join(name);
    check(&path)?;
    match fs::read(&path) {
        Ok(b) => String::from_utf8(b)
            .map(InDir::Text)
            .map_err(|_| format!("{}: not UTF-8 text (UTF-16 or binary?)", path.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            #[cfg(windows)]
            {
                // ERROR_PATH_NOT_FOUND: a dir on the way wasn't there.
                Ok(if e.raw_os_error() == Some(3) { InDir::NoDir } else { InDir::NoFile })
            }
            #[cfg(not(windows))]
            {
                Ok(if kind(dir)? == Kind::Dir { InDir::NoFile } else { InDir::NoDir })
            }
        }
        Err(e) => Err(format!("{}: {e}", path.display())),
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

/// Whether a replacement of `path` is under way: its old version is moved aside (`beside(path, "old")`, as
/// `move_aside` names it) while the new one isn't in yet. A reader that finds `path` missing then knows it is between
/// two versions, not gone (fix check r1, FC1).
pub fn being_replaced(path: &Path) -> Result<bool, String> {
    check(path)?;
    let (Some(dir), Some(name)) = (path.parent(), path.file_name()) else { return Ok(false) };
    let prefix = format!(".{}.lords-old-", name.to_string_lossy());
    Ok(list_dir(dir)?
        .iter()
        .any(|p| p.file_name().is_some_and(|n| n.to_string_lossy().starts_with(&prefix))))
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

/// `n` random bytes from the OS.
#[cfg(windows)]
fn random(n: usize) -> Result<Vec<u8>, String> {
    use windows_sys::Win32::Security::Cryptography::{
        BCryptGenRandom, BCRYPT_USE_SYSTEM_PREFERRED_RNG,
    };
    let mut buf = vec![0u8; n];
    // SAFETY: a buffer of `n` bytes that lives across the call; no algorithm handle, as the system-preferred flag asks.
    let status = unsafe {
        BCryptGenRandom(
            std::ptr::null_mut(),
            buf.as_mut_ptr(),
            n as u32,
            BCRYPT_USE_SYSTEM_PREFERRED_RNG,
        )
    };
    if status == 0 {
        Ok(buf)
    } else {
        Err(format!("no random bytes from the OS (status {status:#x})"))
    }
}

#[cfg(not(windows))]
fn random(n: usize) -> Result<Vec<u8>, String> {
    let mut buf = vec![0u8; n];
    std::io::Read::read_exact(
        &mut fs::File::open("/dev/urandom").map_err(|e| e.to_string())?,
        &mut buf,
    )
    .map_err(|e| e.to_string())?;
    Ok(buf)
}

/// Creates a new file only its owner can open, from its first moment: its access list is set as it is made (a
/// protected list with one entry, the file's owner), never loosened after. Refused when anything is there.
#[cfg(windows)]
fn create_owner_only_at(path: &Path, bytes: &[u8]) -> Result<(), String> {
    use std::os::windows::ffi::OsStrExt;
    use std::os::windows::io::FromRawHandle;
    use windows_sys::Win32::Foundation::{LocalFree, GENERIC_WRITE, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::Security::Authorization::{
        ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
    };
    use windows_sys::Win32::Security::SECURITY_ATTRIBUTES;
    use windows_sys::Win32::Storage::FileSystem::{CreateFileW, CREATE_NEW, FILE_ATTRIBUTE_NORMAL};
    check(path)?;
    let failed =
        |what: &str| format!("{}: {what}: {}", path.display(), std::io::Error::last_os_error());
    // A protected DACL ("P", no inherited entries) with one entry: full access for the owner ("OW").
    let sddl: Vec<u16> = "D:P(A;;FA;;;OW)".encode_utf16().chain([0]).collect();
    let wide: Vec<u16> = path.as_os_str().encode_wide().chain([0]).collect();
    // SAFETY: NUL-terminated wide strings that live across the calls; the descriptor the first call allocates is
    // freed once, after CreateFileW has copied it; the handle CreateFileW returns is owned by the `File` made from it.
    unsafe {
        let mut sd = std::ptr::null_mut();
        if ConvertStringSecurityDescriptorToSecurityDescriptorW(
            sddl.as_ptr(),
            SDDL_REVISION_1,
            &mut sd,
            std::ptr::null_mut(),
        ) == 0
        {
            return Err(failed("no owner-only security descriptor"));
        }
        let sa = SECURITY_ATTRIBUTES {
            nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: sd,
            bInheritHandle: 0,
        };
        let h = CreateFileW(
            wide.as_ptr(),
            GENERIC_WRITE,
            0,
            &sa,
            CREATE_NEW,
            FILE_ATTRIBUTE_NORMAL,
            std::ptr::null_mut(),
        );
        let made = if h == INVALID_HANDLE_VALUE { Err(failed("can't be made")) } else { Ok(h) };
        LocalFree(sd);
        let mut f = fs::File::from_raw_handle(made? as _);
        f.write_all(bytes)
            .and_then(|()| f.sync_all())
            .map_err(|e| format!("{}: {e}", path.display()))
    }
}

#[cfg(not(windows))]
fn create_owner_only_at(path: &Path, bytes: &[u8]) -> Result<(), String> {
    use std::os::unix::fs::OpenOptionsExt;
    check(path)?;
    let mut f = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
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

/// The record a `Lock` writes in its file, first: a file holding it whose OS lock is free has no holder left.
pub const LOCK_RECORD: &str = "lords-lock 2";

/// A lock held by an OS lock on its file's open handle (std's `File::lock`), so it ends when its holder does, however
/// that ends: a process killed while it holds one leaves a file no one holds (fix check r2, FC5). The file appears at
/// its path only whole and already locked (made beside it, then linked in, which fails if a file is there), so the
/// path never names a file without its record or before its maker holds it; builds before this one hold the lock by
/// the file's existence alone and wait while it is there, so old and new binaries still exclude each other. Dropping
/// it deletes the file, then lets the OS lock go.
pub struct Lock {
    file: fs::File,
    path: PathBuf,
}

/// One attempt at a `Lock` (`try_lock`).
pub enum TryLock {
    Held(Lock),
    /// Someone holds it: what its file says of its holder, where that can be read.
    Busy(String),
}

/// One attempt to take the lock at `p`, recording `holder` in it when it makes the file. When the file is there, it is
/// held if its OS lock is taken; if its OS lock is free, it holds this lock's record (`LOCK_RECORD`) and the path still
/// names it, its holder died and the lock is taken over as it is (never rewritten, so no instant leaves it without its
/// record); a file in an older build's form (no OS lock) is that build's holder, waited for as before.
pub fn try_lock(p: &Contained, holder: &str) -> Result<TryLock, String> {
    use std::io::{Read, Write};
    let path = &p.path;
    check(path)?;
    let err = |e: std::io::Error| format!("{}: {e}", path.display());
    let mut f = match fs::OpenOptions::new().read(true).write(true).open(path) {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            let staged = beside(path, "new")?;
            let mut f = fs::OpenOptions::new()
                .read(true)
                .write(true)
                .create_new(true)
                .open(&staged)
                .map_err(|e| format!("{}: {e}", staged.display()))?;
            let made = f
                .lock()
                .and_then(|()| f.write_all(format!("{LOCK_RECORD} {holder}\n").as_bytes()))
                .and_then(|()| f.sync_all())
                .and_then(|()| link_in(&staged, path));
            // The staging name is this attempt's own and never read: a leftover is harmless, as `put_file`'s.
            let _ = fs::remove_file(&staged);
            return match made {
                Ok(()) => Ok(TryLock::Held(Lock { file: f, path: path.clone() })),
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                    Ok(TryLock::Busy(String::new()))
                }
                Err(e) => Err(err(e)),
            };
        }
        // Being deleted by its holder's release (Windows: delete pending).
        Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => {
            return Ok(TryLock::Busy(String::new()))
        }
        Err(e) => return Err(err(e)),
    };
    let mut text = String::new();
    match f.read_to_string(&mut text) {
        Ok(_) => {}
        Err(e) if locked_out(&e) => return Ok(TryLock::Busy("its holder is running".into())),
        Err(e) => return Err(err(e)),
    }
    // Not this build's record: an earlier build's lock, held by the file's existence and perhaps still being written.
    // Decided by its content alone, never by taking its OS lock, which on Windows would fail that build's write of its
    // record and leave an empty file no one removes (repair 3 sol-loop r1, Y3). This build's files never change once
    // linked in, so what was read is what is there.
    if !text.starts_with(LOCK_RECORD) {
        return Ok(TryLock::Busy(text.trim().to_string()));
    }
    match f.try_lock() {
        Ok(()) => {}
        Err(fs::TryLockError::WouldBlock) => {
            return Ok(TryLock::Busy("its holder is running".into()))
        }
        Err(fs::TryLockError::Error(e)) => return Err(err(e)),
    }
    // This lock's record and no holder: its holder died. Taken over only while the path still names this file (its
    // holder's release may have deleted it, and another made the next one).
    let same = handle_identity(&f).ok().zip(identity(path).ok()).is_some_and(|(a, b)| a == b);
    if same {
        return Ok(TryLock::Held(Lock { file: f, path: path.clone() }));
    }
    let _ = f.unlock();
    Ok(TryLock::Busy(text.trim().to_string()))
}

/// Whether a read failed on another handle's OS lock over the bytes (Windows; elsewhere OS locks don't stop reads).
fn locked_out(e: &std::io::Error) -> bool {
    #[cfg(windows)]
    {
        e.raw_os_error() == Some(windows_sys::Win32::Foundation::ERROR_LOCK_VIOLATION as i32)
    }
    #[cfg(not(windows))]
    {
        let _ = e;
        false
    }
}

impl Drop for Lock {
    fn drop(&mut self) {
        // Deleted while held, so no one takes it between: on Windows it goes when this handle closes, right after.
        let _ = fs::remove_file(&self.path);
        let _ = self.file.unlock();
    }
}

#[cfg(windows)]
fn handle_identity(f: &fs::File) -> Result<(u32, u64), String> {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Storage::FileSystem::{
        GetFileInformationByHandle, BY_HANDLE_FILE_INFORMATION,
    };
    // SAFETY: an all-zero BY_HANDLE_FILE_INFORMATION is a valid value of that plain C struct.
    let mut info: BY_HANDLE_FILE_INFORMATION = unsafe { std::mem::zeroed() };
    // SAFETY: the handle is open for the call (`f` lives across it) and `info` is a valid place to write to.
    let ok = unsafe { GetFileInformationByHandle(f.as_raw_handle(), &mut info) };
    if ok == 0 {
        return Err(std::io::Error::last_os_error().to_string());
    }
    Ok((
        info.dwVolumeSerialNumber,
        (u64::from(info.nFileIndexHigh) << 32) | u64::from(info.nFileIndexLow),
    ))
}

#[cfg(unix)]
fn handle_identity(f: &fs::File) -> Result<(u64, u64), String> {
    use std::os::unix::fs::MetadataExt;
    let m = f.metadata().map_err(|e| e.to_string())?;
    Ok((m.dev(), m.ino()))
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
fn replace_own_file_at(path: &Path, was: Option<&[u8]>, bytes: &[u8]) -> Result<(), String> {
    let changed =
        || format!("{} changed while it was being replaced; nothing changed", path.display());
    // The new file keeps the old one's attributes (Sol review r5, R5-05: a stack edit kept them nowhere).
    let (old, like) = match kind(path)? {
        Kind::Missing if was.is_some() => return Err(changed()),
        Kind::Missing => (None, None),
        Kind::File => {
            // Kept exactly, or refused before anything moves (rule C).
            let like = attrs(path)?.keep().map_err(|e| format!("{}: {e}", path.display()))?;
            let aside = move_aside_at(path)?;
            // What was moved aside is what the file held at the move, which no later write can change: unless it is
            // what the caller read, it goes back and nothing changes.
            if let Some(was) = was {
                let held = fs::read(&aside).map_err(|e| format!("{}: {e}", aside.display()));
                if held.as_deref().ok() != Some(was) {
                    return Err(match move_back_at(&aside, path) {
                        Ok(()) => held.err().unwrap_or_else(changed),
                        Err(back) => format!("{}; and it couldn't go back ({back})", changed()),
                    });
                }
            }
            (Some(aside), Some(like))
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

    /// An SSH bridge's dir, which `lords panel add` changes: it holds the bridge's `sshd_config`.
    pub fn bridge(dir: &Path) -> Result<Root, String> {
        let root = Root::at(dir.to_path_buf())?;
        if kind(&root.path.join("sshd_config"))? != Kind::File {
            return Err(format!("{}: no sshd_config, so no SSH bridge", dir.display()));
        }
        Ok(root)
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

    /// A dropped target's dir, as the ledger records it (`lords uninstall --target`; D21): the stack names it no longer,
    /// so it is the ledger's word, checked as every root is (absolute, no link on the way). Only a copy below it that
    /// holds what its record says is ever removed.
    pub fn recorded(dir: &Path) -> Result<Root, String> {
        Root::at(dir.to_path_buf())
    }

    /// A workspace's dir.
    pub fn workspace(w: &crate::stack::Workspace) -> Result<Root, String> {
        Root::at(crate::config::expand_home(&w.path))
    }

    /// A real checkout or worktree, configured or discovered alike (Sol review r5, R5-01; r6, R6-02): git's own top
    /// level there is exactly this path, and its git dir is there. A dir that is none, a dir inside one, a stale
    /// worktree git still lists, and a git that fails are all errors.
    pub fn checkout(path: &Path) -> Result<Root, String> {
        // One git process for both answers, a line each: sync verifies every worktree of a repo this way.
        let out = crate::git::run(path, &["rev-parse", "--show-toplevel", "--absolute-git-dir"])
            .map_err(|e| format!("{} is no git checkout ({e})", path.display()))?;
        let mut lines = out.lines().map(str::trim);
        let (top, git_dir) = match (lines.next(), lines.next()) {
            (Some(t), Some(g)) => (t, g),
            _ => return Err(format!("{} is no git checkout (git said {out:?})", path.display())),
        };
        if crate::ledger::key(Path::new(top)) != crate::ledger::key(path) {
            return Err(format!(
                "{} is no checkout's root: git's top level there is {top}",
                path.display()
            ));
        }
        if kind(Path::new(git_dir))? != Kind::Dir {
            return Err(format!("{}: its git dir {git_dir} is gone", path.display()));
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

    /// Back where it was after `failed`, the error that stopped a change: that error; or, if it can't go back, both
    /// errors and where the moved copy is now (uninstall-place review r1, UP2-R1-01).
    pub fn back_after(&self, failed: String) -> String {
        match self.back() {
            Ok(()) => failed,
            Err(back) => format!(
                "{failed}; and it couldn't go back ({back}): it is at {}",
                self.at.display()
            ),
        }
    }

    /// Gone; or, if that stops part way, what is left goes back where it was (`back_after`): a removal that can't
    /// finish leaves its path holding what remains, never the copy beside it.
    pub fn delete_or_back(self) -> Result<(), String> {
        let gone = kind(&self.at).and_then(|k| match k {
            Kind::Dir => delete_tree_at(&self.at),
            _ => delete_file_at(&self.at),
        });
        gone.map_err(|e| self.back_after(e))
    }
}

pub fn create_dir_all(p: &Contained) -> Result<(), String> {
    create_dir_all_at(&p.path)
}

/// Writes a new file where nothing is; refused when anything is there.
pub fn create_file(p: &Contained, bytes: &[u8]) -> Result<(), String> {
    create_file_at(&p.path, bytes)
}

/// Makes a new secret where nothing is: `len` random bytes from the OS, as hex, in a file only its owner can open
/// from its first moment (the panel's token, review r1 R2). Gives the hex; refused when anything is there.
pub fn create_secret(p: &Contained, len: usize) -> Result<String, String> {
    let hex: String = random(len)?.iter().map(|b| format!("{b:02x}")).collect();
    create_owner_only_at(&p.path, hex.as_bytes())?;
    Ok(hex)
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

// F8's place transaction (design note Harness/notes/2026-11-07-lords-f8-delivery.md §4 D3, D4; F8 §15 M1): a file's next
// version is staged beside it (`sibling` "new"), its current one linked beside it as a backup (`link_as` to `sibling`
// "old", the same file object, so an edit made in place shows in it), and the staged one renamed over it
// (`rename_over`), so a reader finds the old file or the new one, never none. The checks and the undo are the caller's
// (`sync::places`).

/// A fresh name beside `p` in its dir, `.<name>.lords-<what>-…`, below the same root.
pub fn sibling(p: &Contained, what: &str) -> Result<Contained, String> {
    let name = beside(&p.path, what)?;
    let name = name.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let mut parts = p.parts.clone();
    parts.pop();
    parts.push(Part::new(&name)?);
    Ok(p.root.join(&parts))
}

/// Every `.<name>.lords-<what>-…` beside `p` (what `sibling` names), below the same root.
pub fn siblings(p: &Contained, what: &str) -> Result<Vec<Contained>, String> {
    let (Some(dir), Some(name)) = (p.path.parent(), p.path.file_name()) else {
        return Ok(Vec::new());
    };
    let prefix = format!(".{}.lords-{what}-", name.to_string_lossy());
    let mut found = Vec::new();
    for e in list_dir(dir)? {
        let Some(n) = e.file_name().map(|n| n.to_string_lossy().into_owned()) else { continue };
        if n.starts_with(&prefix) {
            let mut parts = p.parts.clone();
            parts.pop();
            parts.push(Part::new(&n)?);
            found.push(p.root.join(&parts));
        }
    }
    Ok(found)
}

/// The sibling of `p` called `name`, below the same root: one a place transaction recorded making (F8 review r1,
/// R1-01). Refused unless `name` is one `sibling` gives `p` (`.<name>.lords-<what>-…`), so a record can name nothing
/// else.
pub fn sibling_named(p: &Contained, name: &str) -> Result<Contained, String> {
    let file = p.path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let prefix = format!(".{file}.lords-");
    let known = ["new-", "old-", "done-"].iter().any(|w| name.starts_with(&format!("{prefix}{w}")));
    if !known || name.contains(['/', '\\']) {
        return Err(format!("{name}: not a name a transaction gives beside {}", p.path.display()));
    }
    let mut parts = p.parts.clone();
    parts.pop();
    parts.push(Part::new(name)?);
    Ok(p.root.join(&parts))
}

/// A file held open for a place transaction (F8 review r1, R1-02): while it is, Windows refuses any open that doesn't
/// share delete, so no reader of that kind can come between the transaction's swaps or its undo. Off Windows it holds
/// nothing: no reader blocks a rename there.
pub struct Holding {
    _file: Option<fs::File>,
}

/// Holds the file at `p` open with delete access, sharing read, write and delete (`Holding`). While a reader without
/// delete sharing holds it, Windows refuses: tried again every 50 ms until `deadline`, then refused with that error, so
/// such a reader is found before the transaction writes.
pub fn hold_open(p: &Contained, deadline: std::time::Instant) -> Result<Holding, String> {
    check(&p.path)?;
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        // DELETE | FILE_READ_ATTRIBUTES; FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE.
        let (access, share) = (0x0001_0000 | 0x0080, 0x7);
        loop {
            match fs::OpenOptions::new().access_mode(access).share_mode(share).open(&p.path) {
                Ok(file) => return Ok(Holding { _file: Some(file) }),
                Err(e)
                    if (e.kind() == std::io::ErrorKind::PermissionDenied
                        || e.raw_os_error() == Some(32))
                        && std::time::Instant::now() < deadline =>
                {
                    std::thread::sleep(std::time::Duration::from_millis(50))
                }
                Err(e) => {
                    return Err(format!(
                        "{}: {e} (a reader holds it open without delete sharing)",
                        p.path.display()
                    ))
                }
            }
        }
    }
    #[cfg(not(windows))]
    {
        let _ = deadline;
        Ok(Holding { _file: None })
    }
}

/// The file at `from` gets the second name `to`, where nothing may be.
pub fn link_as(from: &Contained, to: &Contained) -> Result<(), String> {
    check(&from.path)?;
    check(&to.path)?;
    link_in(&from.path, &to.path).map_err(|e| format!("{}: {e}", to.path.display()))
}

/// Renames the file `from` over `to` in one step that replaces what is at `to`. While a reader holds `to` open without
/// delete sharing, Windows refuses it: it is tried again every 50 ms until `deadline`, then refused with that error.
pub fn rename_over(
    from: &Contained,
    to: &Contained,
    deadline: std::time::Instant,
) -> Result<(), String> {
    check(&from.path)?;
    check(&to.path)?;
    loop {
        match fs::rename(&from.path, &to.path) {
            Ok(()) => return check(&to.path),
            Err(e)
                if (e.kind() == std::io::ErrorKind::PermissionDenied
                    || e.raw_os_error() == Some(32))
                    && std::time::Instant::now() < deadline =>
            {
                std::thread::sleep(std::time::Duration::from_millis(50))
            }
            Err(e) => {
                return Err(format!("{}: {e} (a reader may hold it open)", to.path.display()))
            }
        }
    }
}

/// Puts an executable at `p` that is the tool's alone: written and synced as a staged `.exe` beside it, run there by
/// `verify`, then renamed over `p`. A copy that fails `verify` is removed and `p` keeps what it had. One a running
/// process holds, which Windows won't replace (os error 5) but lets be renamed, goes aside first, its name recording the
/// hash of the bytes it holds (`.<name>.lords-busy-<sha256>-…`), and is deleted once free and only while it still holds
/// them, here or by `sweep_busy` (design note 2026-11-07-lords-delivery-integrity §4, I4b): `p` never lacks a copy but
/// between those two renames, or when the second fails and the old copy can't go back, which the error then names.
pub fn install_exe(
    p: &impl Place,
    bytes: &[u8],
    verify: impl FnOnce(&Path) -> Result<(), String>,
) -> Result<(), String> {
    let path = p.place();
    check(path)?;
    // `.exe` appended, keeping the staged name unique (sol-loop r2, S11).
    let staged = PathBuf::from(format!("{}.exe", beside(path, "new")?.display()));
    if let Err(e) = create_file_at(&staged, bytes) {
        let _ = delete_file_at(&staged);
        return Err(e);
    }
    if let Err(e) = verify(&staged) {
        let _ = delete_file_at(&staged);
        return Err(e);
    }
    let Err(busy) = fs::rename(&staged, path) else { return Ok(()) };
    let refused = |e: String| {
        let _ = delete_file_at(&staged);
        format!("{}: {e}", path.display())
    };
    // Nothing there (or it can't be told): the rename failed for another reason, and nothing is moved.
    if nothing_at(path).unwrap_or(true) {
        return Err(refused(busy.to_string()));
    }
    // Lords' record of the copy it sets aside: the hash of its bytes, in its name (review r1, R1-01).
    let held = fs::read(path).map(|b| sha(&b)).map_err(|e| refused(e.to_string()))?;
    let aside = beside(path, &format!("busy-{held}")).map_err(refused)?;
    if let Err(e) = fs::rename(path, &aside) {
        return Err(refused(format!("{busy}; and it can't be set aside ({e})")));
    }
    if let Err(e) = fs::rename(&staged, path) {
        let back = move_back_at(&aside, path);
        let _ = delete_file_at(&staged);
        return Err(match back {
            Ok(()) => format!("{}: {e}", path.display()),
            Err(b) => format!(
                "{}: {e}; and the old copy couldn't go back ({b}): it is at {}",
                path.display(),
                aside.display()
            ),
        });
    }
    // Still running, it stays for the next `sweep_busy`.
    let _ = delete_if_holds(&aside, &held);
    Ok(())
}

/// Deletes the file at `path` only while it holds bytes whose hash is `want`: it is moved to a fresh name first and
/// hashed there, so nothing written to `path` after the check is deleted (as sync's removals do); otherwise it goes back,
/// and why it stays comes back as the error.
fn delete_if_holds(path: &Path, want: &str) -> Result<(), String> {
    let at = move_aside_at(path)?;
    let why = match fs::read(&at) {
        Ok(b) if sha(&b) == want => match fs::remove_file(&at) {
            Ok(()) => return Ok(()),
            Err(e) => format!("can't be deleted yet ({e}): a process may still run it"),
        },
        Ok(_) => "holds other bytes than Lords set aside".to_string(),
        Err(e) => format!("can't be read ({e})"),
    };
    Err(match move_back_at(&at, path) {
        Ok(()) => why,
        Err(back) => format!("{why}; and it couldn't go back ({back}): it is at {}", at.display()),
    })
}

/// Deletes each copy `install_exe` set aside beside `p` that still holds the bytes whose hash its name records
/// (`delete_if_holds`; review r1, R1-01). Every other file of that name's prefix stays, and comes back with why: a name
/// with no hash (Lords never set it aside), other bytes, a copy a process still runs, anything but a plain file.
pub fn sweep_busy(p: &impl Place) -> Result<Vec<String>, String> {
    let path = p.place();
    check(path)?;
    let (Some(dir), Some(name)) = (path.parent(), path.file_name()) else { return Ok(Vec::new()) };
    let prefix = format!(".{}.lords-busy-", name.to_string_lossy());
    let mut kept = Vec::new();
    for f in list_dir(dir)? {
        let Some(name) = f.file_name().map(|n| n.to_string_lossy().into_owned()) else { continue };
        let Some(rest) = name.strip_prefix(&prefix) else { continue };
        let hash = rest.get(..64).filter(|h| {
            rest.as_bytes().get(64) == Some(&b'-')
                && h.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
        });
        let gone = match (hash, kind_here(&f)) {
            (None, _) => Err("its name records no hash, so Lords never set it aside".to_string()),
            (Some(h), Ok(Kind::File)) => delete_if_holds(&f, h),
            (Some(_), _) => Err("not a plain file".to_string()),
        };
        if let Err(why) = gone {
            kept.push(format!("{}: {why}; kept", f.display()));
        }
    }
    Ok(kept)
}

/// Replaces a file that is the tool's alone, keeping its attributes or refusing.
pub fn replace_own_file(p: &impl Place, bytes: &[u8]) -> Result<(), String> {
    replace_own_file_at(p.place(), None, bytes)
}

/// Replaces a file only if it holds exactly `was` at the moment it is moved aside (a compare-and-swap): a write that
/// lands after the caller read `was` is put back untouched, and this one refused. A stack edit made from what was read
/// never replaces a newer file.
pub fn replace_file_if(p: &impl Place, was: &[u8], bytes: &[u8]) -> Result<(), String> {
    replace_own_file_at(p.place(), Some(was), bytes)
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
