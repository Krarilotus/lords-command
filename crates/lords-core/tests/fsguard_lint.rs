//! The filesystem guard is the only door (orchestrator, 25 Oct: "The choke point is the only filesystem API in the
//! apply path. Add a test or lint that fails if std::fs remove, rename or write calls appear outside it"). This
//! reads every Rust source of the CLI, the GUI and the library and fails on a call that changes the filesystem
//! anywhere but `fsguard.rs`. The boundary it enforces is stated in fsguard.rs's header (Sol review r3, R3-08):
//! std's functions that change the filesystem, in any spelling (qualified, imported, renamed, spaced or split over
//! lines), file handles opened for writing or changed, and child processes outside the two places allowed them.

use std::fs;
use std::path::{Path, PathBuf};

/// std's functions (`std::fs` and the platform `fs` extensions) that create, write, rename, copy, link or remove.
const FS_CHANGES: [&str; 14] = [
    "write",
    "rename",
    "copy",
    "remove_file",
    "remove_dir",
    "remove_dir_all",
    "create_dir",
    "create_dir_all",
    "hard_link",
    "soft_link",
    "set_permissions",
    "symlink",
    "symlink_file",
    "symlink_dir",
];

/// Anything else that opens a file for writing or changes one, whatever it is called on.
const CHANGES: [&str; 11] = [
    "File::create",
    "File::create_new",
    "File::options",
    "OpenOptions",
    ".set_len(",
    ".set_permissions(",
    ".set_readonly(",
    ".set_modified(",
    ".set_times(",
    "FileTimes",
    "mklink",
];

/// The child processes a file may start (fsguard.rs's header): `git` in git.rs, the file manager in the GUI.
fn programs_allowed(file: &str) -> &'static [&'static str] {
    let file = file.replace('\\', "/");
    if file.ends_with("lords-core/src/git.rs") || file == "git.rs" {
        &["\"git\""]
    } else if file.ends_with("lords-command/src/main.rs") {
        &["\"explorer\"", "\"xdg-open\""]
    } else {
        &[]
    }
}

/// The code of a Rust source with its comments removed (string and char literals kept whole), so neither a
/// comment hides code nor a `//` inside a string ends the line early.
fn code_of(text: &str) -> String {
    let c: Vec<char> = text.chars().collect();
    let mut out = String::new();
    let mut i = 0;
    while i < c.len() {
        match (c[i], c.get(i + 1).copied()) {
            ('/', Some('/')) => {
                while i < c.len() && c[i] != '\n' {
                    i += 1;
                }
            }
            ('/', Some('*')) => {
                let mut depth = 0;
                while i < c.len() {
                    if c[i] == '/' && c.get(i + 1) == Some(&'*') {
                        depth += 1;
                        i += 2;
                    } else if c[i] == '*' && c.get(i + 1) == Some(&'/') {
                        depth -= 1;
                        i += 2;
                        if depth == 0 {
                            break;
                        }
                    } else {
                        i += 1;
                    }
                }
                out.push(' ');
            }
            ('r', Some('"' | '#'))
                if !c[..i].last().is_some_and(|p| p.is_alphanumeric() || *p == '_') =>
            {
                // A raw string: r"…" or r#"…"#, to its closing quote and as many #.
                let start = i;
                i += 1;
                let mut hashes = 0;
                while c.get(i) == Some(&'#') {
                    hashes += 1;
                    i += 1;
                }
                if c.get(i) != Some(&'"') {
                    out.extend(&c[start..i]);
                    continue;
                }
                i += 1;
                while i < c.len()
                    && !(c[i] == '"' && (0..hashes).all(|h| c.get(i + 1 + h) == Some(&'#')))
                {
                    i += 1;
                }
                i = (i + 1 + hashes).min(c.len());
                out.extend(&c[start..i]);
            }
            ('"', _) => {
                let start = i;
                i += 1;
                while i < c.len() && c[i] != '"' {
                    i += if c[i] == '\\' { 2 } else { 1 };
                }
                i = (i + 1).min(c.len());
                out.extend(&c[start..i]);
            }
            ('\'', _) => {
                // A char literal ('x', '\n', '\u{..}'), not a lifetime.
                let end = if c.get(i + 1) == Some(&'\\') {
                    (i + 2..c.len()).find(|&j| c[j] == '\'')
                } else {
                    Some(i + 2).filter(|&j| c.get(j) == Some(&'\''))
                };
                let end = end.map_or(i + 1, |e| e + 1);
                out.extend(&c[i..end]);
                i = end;
            }
            (ch, _) => {
                out.push(ch);
                i += 1;
            }
        }
    }
    out
}

fn idents(s: &str) -> Vec<&str> {
    s.split(|c: char| !(c.is_alphanumeric() || c == '_')).filter(|t| !t.is_empty()).collect()
}

/// What in one source file (named `file`) changes the filesystem or starts a process it may not: one line each.
fn forbidden(file: &str, text: &str) -> Vec<String> {
    let code: String = code_of(text).chars().filter(|c| !c.is_whitespace()).collect();
    let mut found = Vec::new();
    let ident = |c: char| c.is_alphanumeric() || c == '_';
    for name in FS_CHANGES {
        let path = format!("fs::{name}");
        // Called or passed as a value: anything but a longer name (`fs::write_all`) after it.
        if code.match_indices(&path).any(|(at, _)| {
            !code[at + path.len()..].starts_with(ident) && !code[..at].ends_with(ident)
        }) {
            found.push(format!("{file}: {path}"));
        }
    }
    for what in CHANGES {
        if code.contains(what) {
            found.push(format!("{file}: {what}"));
        }
    }
    // An import of the module under another name, or of a changing function (then callable unqualified).
    for chunk in code_of(text).split(';') {
        let words = idents(chunk);
        let Some(at) = words.iter().position(|w| *w == "use") else { continue };
        let words = &words[at..];
        if !words.contains(&"fs") {
            continue;
        }
        let stmt = words.join(" ");
        if words.windows(2).any(|w| (w[0] == "fs" || w[0] == "self") && w[1] == "as") {
            found.push(format!("{file}: the fs module imported under another name ({stmt})"));
        }
        for name in FS_CHANGES.iter().filter(|n| words.contains(n)) {
            found.push(format!("{file}: fs::{name} imported ({stmt})"));
        }
    }
    let allowed = programs_allowed(file);
    for (at, _) in code.match_indices("Command::new(") {
        let arg = &code[at + "Command::new(".len()..];
        if !allowed.iter().any(|p| arg.starts_with(&format!("{p})"))) {
            found.push(format!("{file}: a child process ({})", &arg[..arg.len().min(30)]));
        }
    }
    found
}

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

// Spec (Sol review r4, R4-06, orchestrator 25 Oct: "Outside `fsguard.rs` and the one process door, no runtime
// source may reach `std::fs` or `std::process` by any spelling (aliases, grouped `use`, `use std as`)"): each of
// these, in an ordinary source, is found, the two counterexamples of the review first.
#[test]
fn r4_06_no_spelling_reaches_std_fs_or_process() {
    let cases = [
        ("a builder the old list lacked", "std::fs::DirBuilder::new().create(p)?;"),
        ("a renamed Command", "use std::process::Command as C;\nC::new(\"powershell\").status()?;"),
        ("std itself renamed", "use std as s;\ns::fs::write(p, b)?;"),
        ("std renamed with a leading ::", "use ::std as s;"),
        ("std renamed as a crate", "extern crate std as s;"),
        ("fs in a grouped use", "use std::{fs, io};\nfs::metadata(p)?;"),
        ("process in a grouped use", "use std::{collections::BTreeMap, process};"),
        ("the module renamed in a group", "use std::{fs as f};"),
        ("a read", "let t = std::fs::read_to_string(p)?;"),
        ("a platform fs extension", "std::os::windows::fs::symlink_dir(a, b)?;"),
        ("the process id, through std", "let pid = std::process::id();"),
    ];
    let missed: Vec<&str> = cases
        .iter()
        .filter(|(_, code)| forbidden("crates/lords-core/src/sync.rs", code).is_empty())
        .map(|(w, _)| *w)
        .collect();
    assert!(missed.is_empty(), "not caught: {missed:?}");
    // A comment or a string that names them is not code that reaches them; other std modules are fine.
    let fine = "// std::fs::write is fsguard's\nlet s = \"std::process\";\nuse std::io::Write;\nuse std::path::Path;";
    assert_eq!(forbidden("crates/lords-core/src/sync.rs", fine), Vec::<String>::new());
}
