//! One test per finding of the fourth Sol review (25 Oct, Harness/notes/2026-10-25-sol-review-lords-command-r4.md)
//! and per side item the orchestrator raised to a fix, each stating its property. Committed before the fixes and
//! run red on their parent, 67d07b1. R4-04 (the no-hard-link fallback, removed) has no test: it needs a volume
//! without hard links. R4-06 is the lint's own table, in fsguard_lint.rs.

// Tests are outside the filesystem boundary (fsguard.rs): their fixtures write their own temp dirs.
#![allow(clippy::disallowed_methods, clippy::disallowed_types)]

mod common;
use common::*;
use lords_core::ledger::Ledger;
use lords_core::sync;
use std::fs;
use std::path::Path;

fn rule(f: &Fixture) {
    f.write("pub/global/rules/r.md", &entry("rule.r", "global", "public", "", "Rule text.\n"));
}

fn skill(f: &Fixture, stack: &str, name: &str) {
    f.write(
        &format!("{stack}/global/skills/{name}/SKILL.md"),
        &format!("---\nname: {name}\ndescription: the {name} skill\n---\nSteps.\n"),
    );
    let visibility = if stack == "pub" { "public" } else { "private" };
    f.write(
        &format!("{stack}/global/skills/{name}/lords.yaml"),
        &format!("id: skill.{name}\nkind: skill\ntitle: {name}\naudience: [all]\nscope: global\nstatus: active\nvisibility: {visibility}\norigin:\n  type: own\n"),
    );
}

fn private_stack(f: &Fixture, toml: &str) {
    f.write(
        "priv/stack.toml",
        &format!("name = \"priv\"\nvisibility = \"private\"\nroles = [\"coder\"]\n{toml}"),
    );
}

fn plan(f: &Fixture) -> Result<sync::Plan, String> {
    sync::plan(&f.stacks(), &f.opts())
}

/// A record of `path` as the tool's, holding what the file there holds now.
fn forge(f: &Fixture, path: &Path, kind: &str, scope: &str) {
    let name = path.file_name().unwrap().to_string_lossy().into_owned();
    common::forge(&f.home(), path.parent().unwrap(), &[&name], kind, scope);
}

/// Plans and applies; whatever happens, the owner file at `rel` must still hold `OWNER`.
fn owner_file_survives(f: &Fixture, rel: &str) {
    let result = plan(f).and_then(|p| {
        let a: Vec<_> = p.items.iter().map(|i| (i.path.clone(), i.action.clone())).collect();
        sync::apply(&p).map(|d| (a, d.done))
    });
    let now = fs::read_to_string(f.path(rel)).ok();
    assert_eq!(now.as_deref(), Some("OWNER"), "{result:?}");
    assert!(result.is_err(), "a misplaced record refuses the plan: {result:?}");
}

// Spec (R4-01: "A false file record inside a skills target still deletes an owner file"; property: a record is
// accepted only for a path, kind and scope sync itself writes): a file record for a file in a skills target, where
// sync writes only one dir per skill, refuses the plan, and the file stays.
#[test]
fn r4_01_a_file_record_where_sync_writes_skill_dirs_is_refused() {
    let f = Fixture::new();
    skill(&f, "pub", "demo");
    private_stack(
        &f,
        &format!("[[targets]]\nname = \"skills\"\nadapter = \"claude\"\nkind = \"skills\"\npath = '{}'\n", f.path("skills").display()),
    );
    let owner = f.write("skills/owner.txt", "OWNER");
    forge(&f, &owner, "file", "target:skills");
    owner_file_survives(&f, "skills/owner.txt");
}

fn repo(f: &Fixture, extra: &str) {
    f.write("checkout/README.md", "a repo\n");
    f.git_init("checkout");
    private_stack(
        f,
        &format!(
            "[repos.beta-core]\ncheckouts = ['{}']\nadapters = [\"claude\"]\n{extra}",
            f.path("checkout").display()
        ),
    );
}

// Spec (R4-01, the adapter variant: "accepts an adapter's file under the wrong `repo:<name>/<adapter>` scope"):
// Claude's file recorded under the Codex scope is no record of it: the plan is refused and the owner's file stays.
#[test]
fn r4_01_a_record_under_another_adapters_scope_is_refused() {
    let f = Fixture::new();
    rule(&f);
    repo(&f, "worktrees = false\n");
    let owner = f.write("checkout/CLAUDE.local.md", "OWNER");
    forge(&f, &owner, "file", "repo:beta-core/codex");
    owner_file_survives(&f, "checkout/CLAUDE.local.md");
}

// Spec (R4-01, the worktree-root variant: "adds a repo's worktree container roots as though they were checkout
// roots"): only a real checkout or worktree is a place; a record in a worktree root that is neither is refused.
#[test]
fn r4_01_a_record_in_a_worktree_root_that_is_no_checkout_is_refused() {
    let f = Fixture::new();
    rule(&f);
    repo(&f, &format!("worktrees = true\nworktree_roots = ['{}']\n", f.path("wts").display()));
    let owner = f.write("wts/CLAUDE.local.md", "OWNER");
    forge(&f, &owner, "file", "repo:beta-core/claude");
    owner_file_survives(&f, "wts/CLAUDE.local.md");
}

// Spec (R4-02: "Valid UTF-8 bytes can hide a BOM-less UTF-16 deny term"; property: every file is scanned as UTF-8
// and in all four UTF-16 readings): a deny term in UTF-16LE whose bytes are valid UTF-8 is found.
#[test]
fn r4_02_utf16_that_is_also_valid_utf8_is_scanned() {
    let mut rules = lords_core::privacy::Rules::public();
    rules.deny = vec![lords_core::privacy::fold_term("\u{4e2d}\u{4e2d}\u{4e2d}\u{4e2d}")];
    let bytes: Vec<u8> =
        "\u{4e2d}\u{4e2d}\u{4e2d}\u{4e2d}".encode_utf16().flat_map(u16::to_le_bytes).collect();
    assert_eq!(std::str::from_utf8(&bytes), Ok("-N-N-N-N"));
    let mut scan = lords_core::privacy::Scan::default();
    scan.add("note.txt".into(), &bytes, &rules);
    assert!(!scan.hits.is_empty(), "the deny term was missed");
}

// Spec (side item, orchestrator 25 Oct: "`check()` fails open on metadata errors: only NotFound means absent"):
// a path whose metadata can't be read for any other reason (here a name Windows rejects) is refused.
#[cfg(windows)]
#[test]
fn check_refuses_a_path_it_cannot_read_metadata_for() {
    let f = Fixture::new();
    let bad = f.path("a<b").join("file");
    assert!(lords_core::fsguard::check(&bad).is_err());
    assert!(lords_core::fsguard::check(&f.path("not-there-yet/file")).is_ok());
}

// Spec (R4-03: "Restore loses the owner's file attributes"; property: restore gives an owner file back everything
// apply changed): a read-only (and on Windows hidden) owner file is read-only and hidden again after restore.
#[test]
fn r4_03_restore_gives_back_the_attributes() {
    let f = Fixture::new();
    rule(&f);
    let file = f.write("out/owner.md", "OLD\n");
    #[cfg(windows)]
    {
        let out = std::process::Command::new("attrib").arg("+h").arg(&file).output().unwrap();
        assert!(out.status.success());
    }
    let mut perms = fs::metadata(&file).unwrap().permissions();
    perms.set_readonly(true);
    fs::set_permissions(&file, perms).unwrap();
    private_stack(
        &f,
        &format!("[[targets]]\nname = \"t\"\nadapter = \"codex\"\nkind = \"instructions\"\npath = '{}'\n", file.display()),
    );
    let backups = sync::apply(&plan(&f).unwrap()).unwrap().backups.expect("a backup");
    let steps = sync::restore(&plan(&f).unwrap(), &backups).unwrap();
    assert_eq!(f.read("out/owner.md"), "OLD\n", "{steps:?}");
    let m = fs::metadata(&file).unwrap();
    assert!(m.permissions().readonly(), "read-only again");
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        assert_ne!(m.file_attributes() & 0x2, 0, "hidden again");
    }
    let mut perms = m.permissions();
    #[allow(clippy::permissions_set_readonly_false)]
    perms.set_readonly(false);
    fs::set_permissions(&file, perms).unwrap();
}

// Spec (R4-05, orchestrator 25 Oct: "a ledger format version: a different or missing version refuses with 'ledger
// format N, this build reads M'"): a saved ledger states its format, and a ledger without one is refused.
#[test]
fn r4_05_a_ledger_states_its_format_and_another_is_refused() {
    let f = Fixture::new();
    Ledger::default().save(&f.home()).unwrap();
    let text = f.read("lords-home/state/ledger.toml");
    assert!(text.contains("format = "), "{text}");
    f.write("lords-home/state/ledger.toml", &text.replace("format = ", "old_format = "));
    let err = Ledger::load(&f.home()).unwrap_err();
    assert!(err.contains("ledger format") && err.contains("this build reads"), "{err}");
}

// Spec (side item, orchestrator 25 Oct: "the interrupted `Ledger::save`: `load` refuses while a
// `.ledger.toml.lords-old-*` sibling exists, and its message names the recovery"): a save cut short after moving
// the old ledger aside is never read as an empty ledger.
#[test]
fn an_interrupted_ledger_save_is_never_read_as_empty() {
    let f = Fixture::new();
    Ledger::default().save(&f.home()).unwrap();
    let state = f.path("lords-home/state");
    fs::rename(state.join("ledger.toml"), state.join(".ledger.toml.lords-old-1-2-3")).unwrap();
    let err = Ledger::load(&f.home()).map(|l| l.owned.len()).unwrap_err();
    assert!(err.contains(".ledger.toml.lords-old-1-2-3"), "{err}");
}

// Spec (R4-07: "Promoting a skill can delete an empty directory that Git cannot restore"; property: a promotion
// deletes nothing git can't give back): a private skill holding an empty dir is not promoted, and the dir stays.
#[test]
fn r4_07_a_skill_with_an_empty_dir_is_not_promoted() {
    let f = Fixture::new();
    skill(&f, "priv", "demo");
    f.git_init("priv");
    f.git_init("pub");
    fs::create_dir(f.path("priv/global/skills/demo/OWNER-EMPTY")).unwrap();
    let result = lords_core::ops::promote(
        &f.stacks(),
        "skill.demo",
        lords_core::ops::Promote::Clean,
        &lords_core::privacy::Rules::public(),
        true,
    )
    .map(|p| p.to);
    assert!(result.is_err(), "{result:?}");
    assert!(f.path("priv/global/skills/demo/OWNER-EMPTY").is_dir());
}
