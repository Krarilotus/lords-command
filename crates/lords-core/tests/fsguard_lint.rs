//! The filesystem guard is the only door (orchestrator, 25 Oct: "The choke point is the only filesystem API in the
//! apply path. Add a test or lint that fails if std::fs remove, rename or write calls appear outside it"). This
//! reads every Rust source of the CLI, the GUI and the library and fails on a call that changes the filesystem
//! anywhere but `fsguard.rs`. The boundary it enforces is stated in fsguard.rs's header (Sol review r3, R3-08; r4,
//! R4-06): outside the two doors, no runtime source reaches `std::fs`, `std::os` or `std::process` by any spelling
//! (a path through them, a grouped or renamed `use`, a renamed `std`); and, so a name that reaches them some other
//! way is still caught, no call that changes a file and no child process. fsguard.rs may reach `std::fs` and
//! `std::os`, process.rs `std::process`, each nothing else of the three.
//!
//! A module ban here rather than clippy's `disallowed-types`/`disallowed-methods`: this repo's hooks don't run
//! clippy, `cargo test` does run this; and a ban of the whole module needs no list of its functions, which is what
//! let `DirBuilder` through.

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

/// Which door a source is, by its path.
#[derive(Clone, Copy, PartialEq)]
enum Door {
    Fs,
    Process,
    None,
}

fn door(file: &str) -> Door {
    let file = file.replace('\\', "/");
    if file.ends_with("lords-core/src/fsguard.rs") {
        Door::Fs
    } else if file.ends_with("lords-core/src/process.rs") {
        Door::Process
    } else {
        Door::None
    }
}

/// The std modules a door may reach.
fn modules_allowed(d: Door) -> &'static [&'static str] {
    match d {
        Door::Fs => &["fs", "os"],
        Door::Process => &["process"],
        Door::None => &[],
    }
}

/// The child processes a source may start: `git` and the file manager, both in process.rs.
fn programs_allowed(d: Door) -> &'static [&'static str] {
    match d {
        Door::Process => &["\"git\"", "\"explorer\"", "\"xdg-open\""],
        _ => &[],
    }
}

/// The code of a Rust source with its comments removed, so neither a comment hides code nor a `//` inside a string
/// ends the line early; string literals kept whole, or (`keep_strings` false) emptied, so a string that names a
/// module is not taken for a path to it.
fn code_of(text: &str, keep_strings: bool) -> String {
    let literal = |out: &mut String, s: &[char]| {
        if keep_strings {
            out.extend(s);
        } else {
            out.push_str("\"\"");
        }
    };
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
                literal(&mut out, &c[start..i]);
            }
            ('"', _) => {
                let start = i;
                i += 1;
                while i < c.len() && c[i] != '"' {
                    i += if c[i] == '\\' { 2 } else { 1 };
                }
                i = (i + 1).min(c.len());
                literal(&mut out, &c[start..i]);
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

/// What in one source file (named `file`) reaches a module, changes a file or starts a process it may not: one line
/// each.
fn forbidden(file: &str, text: &str) -> Vec<String> {
    let d = door(file);
    let allowed = modules_allowed(d);
    let squeeze = |s: String| -> String { s.chars().filter(|c| !c.is_whitespace()).collect() };
    // Strings emptied for the module ban (a string naming a module reaches nothing), kept for the program names.
    let code = squeeze(code_of(text, false));
    let with_strings = squeeze(code_of(text, true));
    let mut found = Vec::new();
    let ident = |c: char| c.is_alphanumeric() || c == '_';
    // A path through a module it may not reach, however spaced or split (`std :: fs`, `::std::fs`).
    for m in ["fs", "os", "process"].into_iter().filter(|m| !allowed.contains(m)) {
        let path = format!("std::{m}");
        if code.match_indices(&path).any(|(at, _)| {
            !code[at + path.len()..].starts_with(ident) && !code[..at].ends_with(ident)
        }) {
            found.push(format!("{file}: {path}"));
        }
    }
    // `use` of std: no renamed std, no group or renamed import that reaches one of the three.
    for chunk in code_of(text, false).split(';') {
        let words = idents(chunk);
        if words.windows(3).any(|w| w == ["extern", "crate", "std"]) {
            found.push(format!("{file}: std named as an extern crate ({})", words.join(" ")));
        }
        let Some(at) = words.iter().position(|w| *w == "use") else { continue };
        let words = &words[at..];
        if words.get(1) != Some(&"std") {
            continue;
        }
        let stmt = words.join(" ");
        if words.get(2) == Some(&"as") || words.windows(2).any(|w| w == ["self", "as"]) {
            found.push(format!("{file}: std or a std module imported under another name ({stmt})"));
        }
        for m in
            ["fs", "os", "process"].iter().filter(|m| !allowed.contains(m) && words.contains(m))
        {
            found.push(format!("{file}: std::{m} imported ({stmt})"));
        }
    }
    if d != Door::Fs {
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
    }
    let programs = programs_allowed(d);
    for (at, _) in with_strings.match_indices("Command::new(") {
        let arg = &with_strings[at + "Command::new(".len()..];
        if !programs.iter().any(|p| arg.starts_with(&format!("{p})"))) {
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

// Spec (above): no source of the crates' `src` dirs reaches std's filesystem or processes but through its door.
#[test]
fn only_fsguard_changes_the_filesystem() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
    let mut files = Vec::new();
    for krate in ["lords-core", "lords", "lords-command"] {
        sources(&root.join(krate).join("src"), &mut files);
    }
    assert!(files.len() > 10, "the sources were found: {files:?}");
    let doors: Vec<Door> = files.iter().map(|f| door(&f.display().to_string())).collect();
    assert!(doors.contains(&Door::Fs) && doors.contains(&Door::Process), "both doors were found");
    let mut found = Vec::new();
    for f in &files {
        found.extend(forbidden(&f.display().to_string(), &fs::read_to_string(f).unwrap()));
    }
    assert!(found.is_empty(), "past the doors:\n{}", found.join("\n"));
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

// Spec (Sol review r5, R5-03; decision 3: the resolved compiler check closes aliases and raw identifiers, and this
// text layer "additionally refuses any `allow`/`expect` of `clippy::disallowed_*`, `clippy::style` or
// `clippy::all` outside `fsguard.rs`/`process.rs`, whitespace normalised"): each way to switch the resolved check
// off in an ordinary source is found; in a door it is allowed.
#[test]
fn r5_03_nothing_switches_the_resolved_check_off() {
    let cases = [
        ("allow, disallowed_methods", "#[allow(clippy::disallowed_methods)]\nfn f() {}"),
        ("inner allow, disallowed_types", "#![allow(clippy::disallowed_types)]"),
        ("expect", "#[expect(clippy::disallowed_methods)]\nfn f() {}"),
        ("spaced", "# [ allow ( clippy :: disallowed_methods ) ]\nfn f() {}"),
        ("the style group", "#![allow(clippy::style)]"),
        ("all of clippy", "#![allow(clippy::all)]"),
        ("in a list", "#[allow(unused, clippy::disallowed_methods)]\nfn f() {}"),
        ("through cfg_attr", "#[cfg_attr(test, allow(clippy::disallowed_types))]\nfn f() {}"),
        ("every warning", "#![allow(warnings)]"),
        ("code clippy never sees", "#[cfg(not(clippy))]\nfn f() {}"),
    ];
    let missed: Vec<&str> = cases
        .iter()
        .filter(|(_, code)| forbidden("crates/lords-core/src/sync.rs", code).is_empty())
        .map(|(w, _)| *w)
        .collect();
    assert!(missed.is_empty(), "not caught: {missed:?}");
    let door = "#![allow(clippy::disallowed_methods, clippy::disallowed_types)]";
    assert_eq!(forbidden("crates/lords-core/src/fsguard.rs", door), Vec::<String>::new());
    assert_eq!(forbidden("crates/lords-core/src/process.rs", door), Vec::<String>::new());
}

/// The workspace root.
fn workspace() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..")
}

// Spec (decision 3: "clippy `disallowed-types` for `std::fs::{File, OpenOptions, DirBuilder}` and
// `std::process::Command`, plus `disallowed-methods` for every free function in `std::fs` and the
// `std::os::{windows,unix}::fs` link functions"): clippy.toml names each, and no Cargo.toml loosens them.
#[test]
fn r5_03_the_resolved_check_names_every_door() {
    let text = fs::read_to_string(workspace().join("clippy.toml")).unwrap_or_default();
    let config: toml::Table = toml::from_str(&text).unwrap_or_default();
    let paths = |key: &str| -> Vec<String> {
        config
            .get(key)
            .and_then(|v| v.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|e| e.get("path").and_then(|p| p.as_str()).map(str::to_string))
                    .collect()
            })
            .unwrap_or_default()
    };
    let (types, methods) = (paths("disallowed-types"), paths("disallowed-methods"));
    for t in
        ["std::fs::File", "std::fs::OpenOptions", "std::fs::DirBuilder", "std::process::Command"]
    {
        assert!(types.iter().any(|x| x == t), "disallowed-types lacks {t}: {types:?}");
    }
    let fs_free = [
        "canonicalize",
        "copy",
        "create_dir",
        "create_dir_all",
        "exists",
        "hard_link",
        "metadata",
        "read",
        "read_dir",
        "read_link",
        "read_to_string",
        "remove_dir",
        "remove_dir_all",
        "remove_file",
        "rename",
        "set_permissions",
        "soft_link",
        "symlink_metadata",
        "write",
    ];
    for m in fs_free.iter().map(|f| format!("std::fs::{f}")).chain(
        [
            "std::os::windows::fs::symlink_file",
            "std::os::windows::fs::symlink_dir",
            "std::os::unix::fs::symlink",
        ]
        .map(String::from),
    ) {
        assert!(methods.contains(&m), "disallowed-methods lacks {m}");
    }
    for krate in ["", "crates/lords-core/", "crates/lords/", "crates/lords-command/"] {
        let manifest = fs::read_to_string(workspace().join(format!("{krate}Cargo.toml"))).unwrap();
        let squeezed: String = manifest.chars().filter(|c| !c.is_whitespace()).collect();
        for loosened in ["disallowed_methods=", "disallowed_types=", "style=", "all="] {
            assert!(!squeezed.contains(loosened), "{krate}Cargo.toml sets {loosened}");
        }
    }
}

// Spec (decision 3: "make clippy `-D warnings` a required part of the repo's commit check"): the repo's commit
// check runs clippy with `-D warnings`, and the privacy pre-commit hook runs the commit check first.
#[test]
fn r5_03_the_commit_check_runs_clippy() {
    let check =
        fs::read_to_string(workspace().join("tools").join("commit-check.sh")).unwrap_or_default();
    assert!(check.contains("cargo clippy") && check.contains("-D warnings"), "{check}");
    let hook = lords_core::hooks::script(Path::new("C:/lords/lords.exe"), false);
    assert!(hook.contains("tools/commit-check.sh"), "{hook}");
}
