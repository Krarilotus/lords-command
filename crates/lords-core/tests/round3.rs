//! One test per finding of the third Sol review (25 Oct, Harness/notes/2026-10-25-sol-review-lords-command-r3.md),
//! each stating its property. Committed before the fixes and run red on their parent, 3546e9e. The one finding whose
//! test needs a new call (R3-05, the check after each action) is in round3_api.rs.

// Tests are outside the filesystem boundary (fsguard.rs): their fixtures write their own temp dirs.
#![allow(clippy::disallowed_methods, clippy::disallowed_types)]

mod common;
use common::*;
use lords_core::sync::{self, Action};
use std::fs;
use std::path::Path;

fn rule(f: &Fixture) {
    f.write("pub/global/rules/r.md", &entry("rule.r", "global", "public", "", "Rule text.\n"));
}

fn skill(f: &Fixture, name: &str) {
    f.write(
        &format!("pub/global/skills/{name}/SKILL.md"),
        &format!("---\nname: {name}\ndescription: the {name} skill\n---\nSteps.\n"),
    );
    f.write(
        &format!("pub/global/skills/{name}/lords.yaml"),
        &format!("id: skill.{name}\nkind: skill\ntitle: {name}\naudience: [all]\nscope: global\nstatus: active\nvisibility: public\norigin:\n  type: own\n"),
    );
}

fn targets(f: &Fixture, toml: &str) {
    f.write(
        "priv/stack.toml",
        &format!("name = \"priv\"\nvisibility = \"private\"\nroles = [\"coder\"]\n{toml}"),
    );
}

fn skills_target(dir: &Path) -> String {
    format!(
        "[[targets]]\nname = \"skills\"\nadapter = \"claude\"\nkind = \"skills\"\npath = '{}'\n",
        dir.display()
    )
}

fn instructions(name: &str, path: &Path) -> String {
    format!("[[targets]]\nname = \"{name}\"\nadapter = \"codex\"\nkind = \"instructions\"\npath = '{}'\n", path.display())
}

fn plan(f: &Fixture) -> Result<sync::Plan, String> {
    sync::plan(&f.stacks(), &f.opts())
}

/// Every name in `dir`.
fn names(dir: &Path) -> Vec<String> {
    let mut v: Vec<String> = fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    v.sort();
    v
}

// Spec (R3-01: "An added empty directory is treated as part of an unchanged skill and deleted"; property: a copy's
// hash covers its dirs, empty ones too): a dir the owner adds inside a copy, even an empty one, makes the copy no
// longer sync's, and no uninstall deletes it.
#[test]
fn r3_01_an_empty_dir_added_to_a_copy_is_never_deleted() {
    let f = Fixture::new();
    skill(&f, "demo");
    targets(&f, &skills_target(&f.path("skills")));
    sync::apply(&plan(&f).unwrap()).unwrap();
    fs::create_dir(f.path("skills/demo/OWNER-EMPTY")).unwrap();
    let opts = sync::Options { uninstall: true, ..f.opts() };
    let p = sync::plan(&f.stacks(), &opts).unwrap();
    let demo = p.items.iter().find(|i| i.path == f.path("skills/demo")).map(|i| i.action.clone());
    let result = sync::apply(&p);
    assert!(f.path("skills/demo/OWNER-EMPTY").is_dir(), "{demo:?} {result:?}");
}

// Spec (R3-02: "A backup failure strands an owner file at an unrecorded aside name"; property: an owner file is
// backed up before it moves, and any failure after the move puts it back): with the backups dir blocked by a plain
// file, the apply fails and the owner file is where it was, as it was, with nothing left beside it.
#[test]
fn r3_02_a_failed_backup_leaves_the_owner_file_in_place() {
    let f = Fixture::new();
    rule(&f);
    f.write("out/owner-file.md", "OLD OWNER TEXT\n");
    targets(&f, &instructions("t", &f.path("out/owner-file.md")));
    f.write("lords-home/backups", "BLOCKED");
    let p = plan(&f).unwrap();
    let result = sync::apply(&p);
    assert!(result.is_err(), "{result:?}");
    let now = fs::read_to_string(f.path("out/owner-file.md")).ok();
    assert_eq!(now.as_deref(), Some("OLD OWNER TEXT\n"), "{:?}", names(&f.path("out")));
    assert_eq!(names(&f.path("out")), vec!["owner-file.md".to_string()]);
}

// Spec (R3-03: "A semantically wrong ledger can remove a file outside every configured target"; property: every
// ledger entry lies under the root its scope names, or nothing is done): a well-formed record of a file outside the
// skills target, scoped to it, refuses the plan, and the file stays.
#[test]
fn r3_03_a_ledger_entry_outside_its_target_refuses_everything() {
    let f = Fixture::new();
    skill(&f, "demo");
    targets(&f, &skills_target(&f.path("skills")));
    let outside = f.write("outside/owner.txt", "OWNER");
    forge(&f.home(), outside.parent().unwrap(), &["owner.txt"], "file", "target:skills");
    let result = plan(&f).and_then(|p| sync::apply(&p).map(|a| a.done));
    assert!(result.is_err(), "{result:?}");
    assert_eq!(f.read("outside/owner.txt"), "OWNER");
}

// Spec (R3-04: "Unicode lowercasing merges two distinct Windows files into one ledger identity"; property: two
// names the filesystem keeps apart are two ledger identities): `İ.txt` and `i̇.txt` are distinct files on Windows,
// so a record of one never makes the other the tool's.
#[cfg(windows)]
#[test]
fn r3_04_two_distinct_files_are_two_identities() {
    let f = Fixture::new();
    let dotted = f.write("out/\u{130}.txt", "A");
    let combining = f.write("out/i\u{307}.txt", "B");
    assert_eq!((f.read("out/\u{130}.txt"), f.read("out/i\u{307}.txt")), ("A".into(), "B".into()));
    // A record of the one, and an update planned at the other (which holds what the record says).
    rule(&f);
    f.write("out/\u{130}.txt", "SAME");
    f.write("out/i\u{307}.txt", "SAME");
    f.write("doc/CLAUDE.md", "OWNER\n");
    targets(
        &f,
        &format!(
            "[[targets]]\nname = \"t\"\nadapter = \"claude\"\nkind = \"instructions\"\npath = '{}'\nvia = '{}'\n",
            f.path("doc/CLAUDE.md").display(),
            combining.display()
        ),
    );
    forge(&f.home(), dotted.parent().unwrap(), &["\u{130}.txt"], "file", "target:t");
    let result = plan(&f).and_then(|p| {
        let a = p.items.iter().find(|i| i.path == combining).map(|i| i.action.clone());
        assert_ne!(a, Some(Action::Update), "the unrecorded file is not the tool's");
        sync::apply(&p).map(|a| a.done)
    });
    assert_eq!(f.read("out/i\u{307}.txt"), "SAME", "{result:?}");
    assert_eq!(f.read("out/\u{130}.txt"), "SAME");
    assert_ne!(lords_core::ledger::key(&dotted), lords_core::ledger::key(&combining));
}

// Spec (R3-06: "Replacing an owner file drops its read-only attribute"; property: an owner file's attributes stay
// as the owner set them): an owner file marked read-only (and on Windows hidden) is still so after its update.
#[test]
fn r3_06_an_owner_file_keeps_its_attributes() {
    let f = Fixture::new();
    rule(&f);
    let file = f.write("out/owner-file.md", "OLD\n");
    #[cfg(windows)]
    {
        let out = std::process::Command::new("attrib").arg("+h").arg(&file).output().unwrap();
        assert!(out.status.success());
    }
    let mut perms = fs::metadata(&file).unwrap().permissions();
    perms.set_readonly(true);
    fs::set_permissions(&file, perms).unwrap();
    targets(&f, &instructions("t", &file));
    let applied = sync::apply(&plan(&f).unwrap()).unwrap();
    assert!(f.read("out/owner-file.md").contains("Rule text."), "{:?}", applied.done);
    let m = fs::metadata(&file).unwrap();
    assert!(m.permissions().readonly(), "still read-only");
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        assert_ne!(m.file_attributes() & 0x2, 0, "still hidden");
    }
    let mut perms = m.permissions();
    #[allow(clippy::permissions_set_readonly_false)]
    perms.set_readonly(false);
    fs::set_permissions(&file, perms).unwrap();
}

// Spec (R3-07: "BOM-less UTF-16 with no zero bytes evades the privacy gate when marked binary"; property: content
// that reads as UTF-16 is scanned as such, binary or not): a deny term in UTF-16 without a byte order mark and
// without one NUL byte is found.
#[test]
fn r3_07_utf16_without_nul_bytes_is_scanned() {
    let mut rules = lords_core::privacy::Rules::public();
    rules.deny = vec![lords_core::privacy::fold_term("\u{79d8}\u{5bc6}\u{9879}\u{76ee}")];
    for be in [false, true] {
        let bytes: Vec<u8> = "\u{79d8}\u{5bc6}\u{9879}\u{76ee}"
            .encode_utf16()
            .flat_map(|u| if be { u.to_be_bytes() } else { u.to_le_bytes() })
            .collect();
        assert!(!bytes.contains(&0));
        let mut scan = lords_core::privacy::Scan::default();
        scan.add("blob.dat".into(), &bytes, &rules);
        assert!(
            !scan.hits.is_empty(),
            "big-endian {be}: {:?}",
            lords_core::privacy::decode(&bytes)
        );
    }
}

// Spec (R3-09: "Hook installation does not share the apply/ledger lock"; property: one writer of the ledger at a
// time): while an apply holds the lock, hook installation waits, then installs.
#[test]
fn r3_09_hook_installation_waits_for_the_apply_lock() {
    let f = Fixture::new();
    f.write("repo/README.md", "a repo\n");
    f.git_init("repo");
    let lock = f.write("lords-home/apply.lock", "pid 1 (a running apply)\n");
    let (repo, home) = (f.path("repo"), f.home());
    let install = std::thread::spawn(move || {
        let exe = std::path::PathBuf::from("C:/lords/lords.exe");
        lords_core::hooks::install(&repo, &exe, false, false, &home).map(|_| ())
    });
    std::thread::sleep(std::time::Duration::from_millis(1500));
    let early = f.path("repo/.git/hooks/pre-commit").exists();
    fs::remove_file(&lock).unwrap();
    let result = install.join().unwrap();
    assert!(!early, "installed while an apply held the lock");
    assert!(result.is_ok() && f.path("repo/.git/hooks/pre-commit").is_file(), "{result:?}");
}

// Spec (R3-10: "Stack edits without commits have no durable backup"; property: a stack edit only replaces bytes git
// holds, committing or not): `retire --no-commit` of an entry with uncommitted changes is refused and changes nothing.
#[test]
fn r3_10_a_stack_edit_without_a_commit_needs_git_to_hold_the_old_bytes() {
    let f = Fixture::new();
    let file = f.write("pub/global/rules/a.md", &entry("rule.a", "global", "public", "", "x\n"));
    f.git_init("pub");
    let edited = entry("rule.a", "global", "public", "", "OWNER EDIT, NOT COMMITTED\n");
    f.write("pub/global/rules/a.md", &edited);
    let r = lords_core::ops::Retire {
        id: "rule.a",
        reason: "r",
        superseded_by: None,
        date: "2026-10-25".into(),
        commit: false,
    };
    let result = lords_core::ops::retire(&f.stacks(), &r).map(|r| r.file);
    assert!(result.is_err(), "{result:?}");
    assert_eq!(fs::read_to_string(file).unwrap(), edited);
}
