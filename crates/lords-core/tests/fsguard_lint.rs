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

/// What in one source file (named `file`) changes the filesystem: one line each.
fn forbidden(file: &str, text: &str) -> Vec<String> {
    let mut found = Vec::new();
    for (n, line) in text.lines().enumerate() {
        let code = line.split("//").next().unwrap_or("");
        for call in FORBIDDEN.iter().filter(|c| code.contains(*c)) {
            found.push(format!("{file}:{}: {call}", n + 1));
        }
    }
    found
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
        found.extend(forbidden(&f.display().to_string(), &fs::read_to_string(f).unwrap()));
    }
    assert!(found.is_empty(), "filesystem changes outside fsguard.rs:\n{}", found.join("\n"));
}

// Spec (Sol review r3, R3-08: "The filesystem lint does not enforce its stated boundary"; property: the lint catches
// every spelling of what the boundary forbids): each of these, in a source that isn't fsguard.rs, is found.
#[test]
fn r3_08_the_lint_catches_every_spelling() {
    let cases = [
        ("an aliased module", "use std::fs as f;\nf::write(p, b)?;"),
        ("an imported function", "use std::fs::{read, write};\nwrite(p, b)?;"),
        ("an imported and renamed function", "use std::fs::rename as mv;\nmv(a, b)?;"),
        ("spaces around the path", "std::fs :: remove_file(p)?;"),
        ("a call split over lines", "std::fs::\n    remove_file(p)?;"),
        ("truncating an open file", "file.set_len(0)?;"),
        ("a new child process outside git.rs", "Command::new(\"robocopy\").arg(a).status()?;"),
        ("a file created through File", "File::create_new(p)?;"),
        ("times changed", "file.set_modified(t)?;"),
    ];
    let missed: Vec<&str> = cases
        .iter()
        .filter(|(_, code)| forbidden("sync.rs", code).is_empty())
        .map(|(w, _)| *w)
        .collect();
    assert!(missed.is_empty(), "not caught: {missed:?}");
    // Reading stays allowed, and a comment is not code.
    assert_eq!(
        forbidden("sync.rs", "let t = fs::read_to_string(p)?; // fs::write(p) is fsguard's"),
        Vec::<String>::new()
    );
}
