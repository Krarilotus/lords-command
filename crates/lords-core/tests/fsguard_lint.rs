//! The filesystem guard is the only door (orchestrator, 25 Oct: "The choke point is the only filesystem API in the
//! apply path. Add a test or lint that fails if std::fs remove, rename or write calls appear outside it"). This
//! reads every Rust source of the CLI, the GUI and the library and fails on a call that changes the filesystem
//! anywhere but `fsguard.rs`.

use std::fs;
use std::path::{Path, PathBuf};

/// Calls that write, rename, copy, link or remove (std and the platform extensions).
const FORBIDDEN: [&str; 16] = [
    "fs::write",
    "fs::rename",
    "fs::copy",
    "fs::remove_file",
    "fs::remove_dir",
    "fs::create_dir",
    "fs::hard_link",
    "fs::set_permissions",
    "File::create",
    "OpenOptions",
    "remove_dir_all",
    "symlink_dir",
    "symlink_file",
    "fs::symlink(",
    "mklink",
    "set_readonly",
];

fn sources(dir: &Path, out: &mut Vec<PathBuf>) {
    for e in fs::read_dir(dir).unwrap().flatten() {
        let p = e.path();
        if p.is_dir() {
            sources(&p, out);
        } else if p.extension().is_some_and(|x| x == "rs") {
            out.push(p);
        }
    }
}

// Spec (above): outside fsguard.rs, no source of the crates' `src` dirs calls anything that changes the filesystem.
#[test]
fn only_fsguard_changes_the_filesystem() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
    let mut files = Vec::new();
    for krate in ["lords-core", "lords", "lords-command"] {
        sources(&root.join(krate).join("src"), &mut files);
    }
    assert!(files.len() > 10, "the sources were found: {files:?}");
    let mut found = Vec::new();
    for f in files.iter().filter(|f| f.file_name().is_some_and(|n| n != "fsguard.rs")) {
        let text = fs::read_to_string(f).unwrap();
        for (n, line) in text.lines().enumerate() {
            let code = line.split("//").next().unwrap_or("");
            for call in FORBIDDEN.iter().filter(|c| code.contains(*c)) {
                found.push(format!("{}:{}: {call}", f.display(), n + 1));
            }
        }
    }
    assert!(found.is_empty(), "filesystem changes outside fsguard.rs:\n{}", found.join("\n"));
}
