//! The filesystem guard is the only door (orchestrator, 25 Oct: "The choke point is the only filesystem API in the
//! apply path. Add a test or lint that fails if std::fs remove, rename or write calls appear outside it"). This
//! reads every Rust source of the CLI, the GUI and the library and fails on a call that changes the filesystem
//! anywhere but `fsguard.rs`. The boundary it enforces is stated in fsguard.rs's header (Sol review r3, R3-08; r4,
//! R4-06): outside the two doors, no runtime source reaches `std::fs`, `std::os` or `std::process` by any spelling
//! (a path through them, a grouped or renamed `use`, a renamed `std`); and, so a name that reaches them some other
//! way is still caught, no call that changes a file and no child process. fsguard.rs may reach `std::fs` and
//! `std::os`, process.rs `std::process` and its two platform process extensions (`extensions_allowed`), each nothing
//! else of the three.
//!
//! This text layer is the second of two (Sol review r5, R5-03; decision 3). The first is clippy's resolved
//! `disallowed-types`/`disallowed-methods` (clippy.toml), which tools/commit-check.sh runs before every commit: the
//! compiler resolves aliases, grouped `use` trees and raw identifiers, which no text match can (R5-03's three
//! spellings got past this layer alone). This layer keeps the module ban and refuses whatever would switch the
//! first one off in an ordinary source: an `allow`/`expect` of `clippy::disallowed_*`, `clippy::style`,
//! `clippy::all` or `warnings`, and code clippy never sees (`cfg(clippy)`); and it checks that clippy.toml names
//! every door and no Cargo.toml loosens it.

// Tests are outside the filesystem boundary (fsguard.rs): their fixtures write their own temp dirs.
#![allow(clippy::disallowed_methods, clippy::disallowed_types)]

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

/// The platform extensions a door may reach beyond its modules, each one module of `std::os`: the process door's child
/// handles and creation flags (a child's tree in a job of its own, no console window of its own; the Lords panel,
/// design note 2026-10-31-lords-panel-remote). Imported one item per `use`.
fn extensions_allowed(d: Door) -> &'static [&'static str] {
    match d {
        Door::Process => &["std::os::windows::process::", "std::os::windows::io::"],
        _ => &[],
    }
}

/// The child processes a source may start: `git`, the file manager, the staged guard copy's self-test
/// (`guard_copy`), the queue guard's T3 CLI (`t3_cli`), the panel's tunnel and `lords panel add`'s remote steps
/// (`ssh`, `scp`), its task for `lords panel serve` (`schtasks`) and its test of the bridge's sshd config
/// (`sshd_exe`), all in process.rs.
fn programs_allowed(d: Door) -> &'static [&'static str] {
    match d {
        Door::Process => &[
            "\"git\"",
            "\"explorer\"",
            "\"xdg-open\"",
            "guard_copy",
            "t3_cli",
            "\"ssh\"",
            "\"scp\"",
            "\"schtasks\"",
            "sshd_exe",
        ],
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

/// Code with each raw identifier's `r#` dropped (`r#fs` is `fs`): what the matches below see (Sol review r6,
/// R6-04). A raw string (`r#"…"#`) is no identifier and keeps its `r#`.
fn unraw(code: &str) -> String {
    let c: Vec<char> = code.chars().collect();
    let ident = |ch: char| ch.is_alphanumeric() || ch == '_';
    let mut out = String::new();
    let mut i = 0;
    while i < c.len() {
        let raw = c[i] == 'r'
            && c.get(i + 1) == Some(&'#')
            && c.get(i + 2).is_some_and(|n| n.is_alphabetic() || *n == '_')
            && !c[..i].last().is_some_and(|p| ident(*p));
        if raw {
            i += 2;
            continue;
        }
        out.push(c[i]);
        i += 1;
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
    // Strings emptied for the module ban (a string naming a module reaches nothing), kept for the program names. A
    // door's own platform extensions are taken out first, and only they.
    let code = extensions_allowed(d)
        .iter()
        .fold(squeeze(unraw(&code_of(text, false))), |c, e| c.replace(e, ""));
    let with_strings = squeeze(unraw(&code_of(text, true)));
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
    for chunk in unraw(&code_of(text, false)).split(';') {
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
        let extension = |e: &&str| {
            let parts: Vec<&str> = e.trim_end_matches(':').split("::").collect();
            words.len() == parts.len() + 2 && words[1..=parts.len()] == parts[..]
        };
        if extensions_allowed(d).iter().any(extension) {
            continue;
        }
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
    // Nothing in an ordinary source switches the resolved check off (decision 3): no `allow`/`expect` of the
    // boundary lints, their group, all of clippy or every warning, and no code clippy doesn't compile.
    if d == Door::None {
        for attr in ["allow(", "expect("] {
            for (at, _) in code.match_indices(attr) {
                let args = code[at + attr.len()..].split(')').next().unwrap_or("");
                let off = args.split(',').find(|a| {
                    a.starts_with("clippy::disallowed_")
                        || ["clippy::style", "clippy::all", "warnings"].contains(a)
                });
                if let Some(a) = off {
                    found.push(format!(
                        "{file}: {attr}{a}) switches the resolved boundary check off"
                    ));
                }
            }
        }
        // The OS's own calls, which no clippy path list covers: only the doors may use an FFI crate or declare
        // foreign functions (fsguard.rs uses `windows_sys` for attributes and file identity).
        for ffi in ["windows_sys", "winapi", "libc", "nix"] {
            if idents(&unraw(&code_of(text, false))).contains(&ffi) {
                found.push(format!("{file}: {ffi} (the OS's own calls, only in a door)"));
            }
        }
        // Every `extern` but `extern crate` declares or defines a foreign function, whatever its ABI string (Sol
        // review r7, R7-08): the word itself is matched, in code with comments and strings out.
        let plain = unraw(&code_of(text, false));
        for (at, _) in plain.match_indices("extern") {
            let after = &plain[at + "extern".len()..];
            if plain[..at].ends_with(ident) || after.starts_with(ident) {
                continue;
            }
            let next = after.trim_start();
            let krate = next.strip_prefix("crate").is_some_and(|r| !r.starts_with(ident));
            if !krate {
                let shown: String = next.chars().take(20).collect();
                found.push(format!("{file}: extern {shown} (foreign functions, only in a door)"));
            }
        }
        if with_strings.contains("#[link(") {
            found.push(format!("{file}: #[link( (foreign functions, only in a door)"));
        }
        for attr in ["cfg(", "cfg_attr("] {
            for (at, _) in code.match_indices(attr) {
                let inside = code[at..].split(']').next().unwrap_or("");
                if inside
                    .match_indices("clippy")
                    .any(|(i, _)| !inside[i + "clippy".len()..].starts_with("::"))
                {
                    found.push(format!(
                        "{file}: {inside} hides code from the resolved boundary check"
                    ));
                }
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
    for f in files.iter().filter(|f| !test_only(f)) {
        found.extend(forbidden(&f.display().to_string(), &fs::read_to_string(f).unwrap()));
    }
    assert!(found.is_empty(), "past the doors:\n{}", found.join("\n"));
}

/// Whether a source is a module its crate root compiles only for its own tests: the root (`main.rs` or `lib.rs`
/// beside it) declares it exactly as `#[cfg(test)] mod <name>;`, so it is never in a run-time build (the window's
/// headless tests, which write their own temp dirs like any test). Any other declaration keeps it in the scan.
fn test_only(file: &Path) -> bool {
    let Some(name) = file.file_stem().and_then(|s| s.to_str()) else { return false };
    let declared = format!("#[cfg(test)]mod{name};");
    ["main.rs", "lib.rs"].iter().any(|root| {
        let root = file.with_file_name(root);
        root != file
            && fs::read_to_string(&root).is_ok_and(|text| {
                let code: String =
                    code_of(&text, false).chars().filter(|c| !c.is_whitespace()).collect();
                code.contains(&declared) && code.matches(&format!("mod{name}")).count() == 1
            })
    })
}

// Spec (the Lords panel, design note 2026-10-31-lords-panel-remote: the window's headless tests live beside it):
// only a module its crate root compiles for tests alone is outside the scan; declared any other way, it is scanned.
#[test]
fn only_a_test_only_module_is_outside_the_scan() {
    let dir = tempfile::TempDir::new().unwrap();
    let root = dir.path().join("main.rs");
    let module = dir.path().join("checks.rs");
    fs::write(&module, "use std::fs;").unwrap();
    for (declared, outside) in [
        ("#[cfg(test)]\nmod checks;", true),
        ("#[cfg(test)] // its tests\nmod checks;", true),
        ("mod checks;", false),
        ("#[cfg(not(test))]\nmod checks;", false),
        (
            "#[cfg(test)]\nmod checks;\n#[cfg(not(test))]\n#[path = \"checks.rs\"]\nmod checks;",
            false,
        ),
    ] {
        fs::write(&root, declared).unwrap();
        assert_eq!(test_only(&module), outside, "{declared:?}");
    }
    assert!(!test_only(&root), "a crate root is never its own module");
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

// Spec (the Lords panel, design note 2026-10-31-lords-panel-remote: the process door bounds a child's whole tree in a
// job and starts it without a console window): process.rs may import exactly its two platform process extensions, one
// item each; anything else of `std::os` stays out of it, and no other source reaches those two.
#[test]
fn the_process_door_reaches_only_its_platform_process_extensions() {
    let process = "crates/lords-core/src/process.rs";
    let fine = "use std::os::windows::process::CommandExt;\nuse std::os::windows::io::AsRawHandle;\nc.creation_flags(f);";
    assert_eq!(forbidden(process, fine), Vec::<String>::new());
    let cases = [
        ("a platform fs extension", "use std::os::windows::fs::MetadataExt;"),
        ("a link made by path", "std::os::windows::fs::symlink_dir(a, b)?;"),
        ("an extension renamed", "use std::os::windows::process::CommandExt as C;"),
        ("an extension in a group with fs", "use std::os::windows::{process::CommandExt, fs};"),
        ("all of os", "use std::os;"),
    ];
    let missed: Vec<&str> = cases
        .iter()
        .filter(|(_, code)| forbidden(process, code).is_empty())
        .map(|(w, _)| *w)
        .collect();
    assert!(missed.is_empty(), "not caught in process.rs: {missed:?}");
    assert!(!forbidden("crates/lords-core/src/panel.rs", fine).is_empty(), "outside the door");
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
    let hook =
        lords_core::hooks::script(Path::new("C:/lords/lords.exe"), false, Path::new("C:/home"));
    assert!(hook.contains("tools/commit-check.sh"), "{hook}");
}

// Spec (the lint class's inventory, orchestrator 25 Oct: "list every place in the code where the class can occur"):
// the OS's own calls reach the filesystem past clippy's path lists, so only a door may use an FFI crate or declare
// foreign functions. Written with its fix (the matcher is in this file).
#[test]
fn r5_03_no_ffi_outside_the_doors() {
    let cases = [
        ("windows-sys", "use windows_sys::Win32::Storage::FileSystem::DeleteFileW;"),
        ("libc", "unsafe { libc::unlink(p) };"),
        ("a foreign block", "extern \"system\" { fn DeleteFileW(p: *const u16) -> i32; }"),
    ];
    let missed: Vec<&str> = cases
        .iter()
        .filter(|(_, code)| forbidden("crates/lords-core/src/sync.rs", code).is_empty())
        .map(|(w, _)| *w)
        .collect();
    assert!(missed.is_empty(), "not caught: {missed:?}");
    assert_eq!(forbidden("crates/lords-core/src/fsguard.rs", cases[0].1), Vec::<String>::new());
}

// Spec (Sol review r6, R6-03: "The required commit check never executes the text lint"; rule E: the runner runs
// both layers): the commit check runs this lint test, not only clippy.
#[test]
fn r6_03_the_commit_check_runs_the_text_lint() {
    let check =
        fs::read_to_string(workspace().join("tools").join("commit-check.sh")).unwrap_or_default();
    assert!(check.contains("--test fsguard_lint"), "{check}");
}

// Spec (R6-04: "Raw identifiers let a Clippy escape pass even when both lint layers run"; rule E: `r#` is
// normalised before matching): the reviewer's raw spellings are each caught in an ordinary source.
#[test]
fn r6_04_raw_identifiers_are_normalised() {
    let cases = [
        ("a raw allow", "#[allow(clippy::r#disallowed_methods)]\nfn f() {}"),
        ("a raw std, renamed", "use r#std as s;"),
        ("a raw path", "let _ = std::r#fs::r#remove_file(p);"),
        ("a raw module ban", "r#std::r#process::r#Command::new(\"powershell\");"),
    ];
    let missed: Vec<&str> = cases
        .iter()
        .filter(|(_, code)| forbidden("crates/lords-core/src/sync.rs", code).is_empty())
        .map(|(w, _)| *w)
        .collect();
    assert!(missed.is_empty(), "not caught: {missed:?}");
}

// Spec (Sol review r7, R7-08: "the text layer refuses every `extern \"<anything>\"` block and `extern` fn outside
// the doors, whatever the ABI string"): every foreign block or function is caught in an ordinary source, whatever
// its ABI; `extern crate` and the word in a name, a string or a comment are not foreign functions.
#[test]
fn r7_08_no_foreign_function_of_any_abi_outside_the_doors() {
    let cases = [
        ("C-unwind", "unsafe extern \"C-unwind\" { fn remove(p: *const u8) -> i32; }"),
        ("system-unwind", "extern \"system-unwind\" { fn DeleteFileW(p: *const u16) -> i32; }"),
        ("a raw ABI string", "extern r#\"C\"# { fn unlink(p: *const u8) -> i32; }"),
        ("no ABI", "extern { fn unlink(p: *const u8) -> i32; }"),
        ("a foreign fn", "pub extern \"efiapi\" fn f() {}"),
        ("a foreign fn, no ABI", "extern fn f() {}"),
        ("a link attribute", "#[link(name = \"c\")]\nunsafe extern \"C\" {}"),
    ];
    let missed: Vec<&str> = cases
        .iter()
        .filter(|(_, code)| forbidden("crates/lords-core/src/sync.rs", code).is_empty())
        .map(|(w, _)| *w)
        .collect();
    assert!(missed.is_empty(), "not caught: {missed:?}");
    for ok in [
        "extern crate alloc;",
        "let external = 1;",
        "let s = \"extern \\\"C\\\" {\";",
        "// extern \"C\" { }\nfn f() {}",
    ] {
        assert_eq!(forbidden("crates/lords-core/src/sync.rs", ok), Vec::<String>::new(), "{ok}");
    }
}
