//! Sol review r7 (Harness/notes/2026-10-25-sol-review-lords-command-r7.md): the reviewer's reproductions, as spec
//! tests. Each spec line quotes the rule it holds.

// Tests are outside the filesystem boundary (fsguard.rs): their fixtures write their own temp dirs.
#![allow(clippy::disallowed_methods, clippy::disallowed_types)]

mod common;
use common::*;
use lords_core::fsguard;
use lords_core::privacy;
use lords_core::sync::{self, Stage};
use std::fs;
use std::io::Write;

fn declare(f: &Fixture, body: &str) {
    f.write("priv/stack.toml", &format!("name = 'priv'\nvisibility = 'private'\n{body}"));
}

fn rule(f: &Fixture) {
    f.write("pub/global/rules/r.md", &entry("rule.r", "global", "public", "", "RULE\n"));
}

/// A Codex instructions target named `name`, whose file is `out/owner.md`.
fn instructions(f: &Fixture, name: &str) {
    declare(
        f,
        &format!(
            "[[targets]]\nname = '{name}'\nadapter = 'codex'\nkind = 'instructions'\npath = '{}'\n",
            f.path("out/owner.md").display()
        ),
    );
}

fn skill(f: &Fixture) {
    f.write("pub/global/skills/demo/SKILL.md", "---\nname: demo\ndescription: demo\n---\nSteps.\n");
    f.write(
        "pub/global/skills/demo/lords.yaml",
        "id: skill.demo\nkind: skill\ntitle: demo\naudience: [all]\nscope: global\nstatus: active\nvisibility: public\norigin:\n  type: own\n",
    );
}

fn skills_target(f: &Fixture, more: &str) {
    declare(
        f,
        &format!(
            "[[targets]]\nname = 'skills'\nadapter = 'claude'\nkind = 'skills'\npath = '{}'\n{more}",
            f.path("out").display()
        ),
    );
}

// Spec (R7-01, guarantee 7: commits through the effective privacy hooks can't disclose a personal detail): the
// staged scan reads every staged entry that isn't a deletion, a type change included.
#[test]
fn the_staged_scan_reads_a_type_change() {
    let f = Fixture::new();
    f.write("repo/file.md", "ordinary clean content\n");
    f.git_init("repo");
    f.write("repo/file.md", &email("someone"));
    let hash = git(&f.path("repo"), &["hash-object", "-w", "file.md"]);
    // Index only: a link's mode, with no link made on disk.
    git(
        &f.path("repo"),
        &["update-index", "--cacheinfo", &format!("120000,{},file.md", hash.trim())],
    );
    let status = git(&f.path("repo"), &["diff", "--cached", "--name-status"]);
    assert!(status.starts_with("T\t"), "a type change: {status:?}");
    let scan = privacy::scan_staged(&f.path("repo"), &privacy::Rules::public()).unwrap();
    assert!(!scan.hits.is_empty(), "the new blob is scanned");
}

// Spec (R7-01): a mode change alone brings no new bytes, but its blob is read like any other staged entry.
#[test]
fn the_staged_scan_reads_a_mode_change() {
    let f = Fixture::new();
    f.write("repo/run.sh", &email("someone"));
    f.git_init("repo");
    git(&f.path("repo"), &["update-index", "--chmod=+x", "run.sh"]);
    let status = git(&f.path("repo"), &["diff", "--cached", "--name-status"]);
    assert!(status.starts_with("M\t"), "a mode change: {status:?}");
    let scan = privacy::scan_staged(&f.path("repo"), &privacy::Rules::public()).unwrap();
    assert!(!scan.hits.is_empty(), "its blob is scanned");
}

// Spec (R7-02, staged first use: "no other owner file is edited"): what the install stage may do is decided by
// the item's type, not its label; a user target named like an exclude file is still an owner file it leaves alone.
#[test]
fn the_install_stage_leaves_an_owner_target_whatever_its_name() {
    let f = Fixture::new();
    rule(&f);
    instructions(&f, "user (git exclude)");
    f.write("out/owner.md", "OWNER\n");
    let p = sync::plan(&f.stacks(), &sync::Options { stage: Stage::Install, ..f.opts() }).unwrap();
    sync::apply(&p).unwrap();
    assert_eq!(f.read("out/owner.md"), "OWNER\n");
}

// Spec (R7-03, staged first use: "a place with a conflict is skipped whole"): a target's shared `via` file is part
// of that target's place, so a conflict in the target's own file skips the shared file too.
#[test]
fn the_install_stage_skips_a_via_target_whole() {
    let f = Fixture::new();
    rule(&f);
    f.write("out/owner.md", "<!-- lords-command:begin -->\nAN UNCLOSED BLOCK\n");
    declare(
        &f,
        &format!(
            "[[targets]]\nname = 'user'\nadapter = 'claude'\nkind = 'instructions'\npath = '{}'\nvia = '{}'\n",
            f.path("out/owner.md").display(),
            f.path("out/shared.md").display()
        ),
    );
    let p = sync::plan(&f.stacks(), &sync::Options { stage: Stage::Install, ..f.opts() }).unwrap();
    assert!(p.notes.iter().any(|n| n.contains("skipped whole")), "{:?}", p.notes);
    sync::apply(&p).unwrap();
    assert!(!f.path("out/shared.md").exists(), "nothing of the target is written");
}

// Spec (R7-04, rule B: removal only from a complete view): a declared place that isn't there (a misconfigured
// workspace) means the plan isn't complete, so uninstall refuses and removes nothing.
#[test]
fn uninstall_refuses_with_a_missing_workspace() {
    let f = Fixture::new();
    rule(&f);
    skill(&f);
    skills_target(
        &f,
        &format!(
            "[workspaces.bad]\npath = '{}'\nadapters = ['claude']\n",
            f.path("workspace").display()
        ),
    );
    let user = sync::Options { user_only: true, ..f.opts() };
    sync::apply(&sync::plan(&f.stacks(), &user).unwrap()).unwrap();
    assert!(f.path("out/demo/SKILL.md").is_file());
    let r = sync::plan(&f.stacks(), &sync::Options { uninstall: true, ..f.opts() })
        .and_then(|p| sync::apply(&p));
    assert!(r.is_err(), "refused: {r:?}");
    assert!(f.path("out/demo/SKILL.md").is_file(), "nothing removed");
}

// Spec (R7-04): the plan's proof of a complete view needs a plan with no conflict at all.
#[test]
fn a_plan_with_a_conflict_has_no_complete_view() {
    let f = Fixture::new();
    rule(&f);
    skills_target(
        &f,
        &format!(
            "[workspaces.bad]\npath = '{}'\nadapters = ['claude']\n",
            f.path("workspace").display()
        ),
    );
    let p = sync::plan(&f.stacks(), &f.opts()).unwrap();
    assert!(p.items.iter().any(|i| i.action == sync::Action::Conflict));
    assert!(p.complete.is_none());
}

// Spec (R7-05, rule B): a problem of any level in the stacks (here a skill that is a file, a warning) holds
// removal: an installed skill whose source didn't load isn't retired.
#[test]
fn a_malformed_skill_source_holds_its_removal() {
    let f = Fixture::new();
    skill(&f);
    skills_target(&f, "");
    sync::apply(&sync::plan(&f.stacks(), &f.opts()).unwrap()).unwrap();
    fs::remove_dir_all(f.path("pub/global/skills/demo")).unwrap();
    f.write("pub/global/skills/demo", "not a skill dir\n");
    let stacks = f.stacks();
    assert!(!stacks.public.problems.is_empty(), "the load says so");
    let p = sync::plan(&stacks, &f.opts()).unwrap();
    assert!(p.complete.is_none());
    sync::apply(&p).unwrap();
    assert!(f.path("out/demo/SKILL.md").is_file(), "the copy stays");
}

// Spec (R7-06, rule C: an owner file with a bit sync can't set back is never replaced): restore checks the file's
// attributes as they are now, like apply, and refuses before changing anything.
#[cfg(windows)]
#[test]
fn restore_refuses_a_file_made_sparse_since() {
    let f = Fixture::new();
    rule(&f);
    instructions(&f, "user");
    let file = f.write("out/owner.md", "OWNER\n");
    let b = sync::apply(&sync::plan(&f.stacks(), &f.opts()).unwrap()).unwrap().backups.unwrap();
    let applied = f.read("out/owner.md");
    let out = std::process::Command::new("fsutil")
        .args(["sparse", "setflag"])
        .arg(&file)
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let before = fsguard::attrs(&file).unwrap();
    assert_ne!(before.bits() & 0x200, 0, "sparse now");
    let r = sync::restore(&sync::plan(&f.stacks(), &f.opts()).unwrap(), &b);
    assert!(r.is_err(), "refused: {r:?}");
    assert_eq!(f.read("out/owner.md"), applied, "unchanged");
    assert_eq!(fsguard::attrs(&file).unwrap(), before, "its attributes too");
}

// Spec (R7-07, rule A: every manifest line re-derives or restore refuses before any change): a line that doesn't
// parse refuses the whole restore; none is skipped.
#[test]
fn a_manifest_line_that_does_not_parse_refuses_the_restore() {
    for bad in ["aside\tOUTSIDE", "edit\tOWNER\t0001", "something else", ""] {
        let f = Fixture::new();
        rule(&f);
        instructions(&f, "user");
        f.write("out/owner.md", "OWNER\n");
        let b = sync::apply(&sync::plan(&f.stacks(), &f.opts()).unwrap()).unwrap().backups.unwrap();
        let applied = f.read("out/owner.md");
        let line = bad
            .replace("OUTSIDE", &f.path("outside/owner").display().to_string())
            .replace("OWNER", &f.path("out/owner.md").display().to_string());
        let mut manifest =
            fs::OpenOptions::new().append(true).open(b.join("manifest.tsv")).unwrap();
        writeln!(manifest, "{line}").unwrap();
        drop(manifest);
        let r = sync::restore(&sync::plan(&f.stacks(), &f.opts()).unwrap(), &b);
        assert!(r.is_err(), "{bad:?} refused: {r:?}");
        assert_eq!(f.read("out/owner.md"), applied, "{bad:?}: unchanged");
    }
}

// Spec (R7-09, rule A: a `Part` is one name every supported filesystem holds as an ordinary file): Windows device
// names, with or without an extension and in any case, and the characters Windows refuses are refused.
#[test]
fn a_part_refuses_device_names_and_windows_reserved_characters() {
    let names = [
        "NUL", "nul", "con.txt", "aux", "PRN", "COM1", "com0", "COM9.md", "lpt9.log", "LPT0",
        "COM¹", "a<b", "a>b", "a|b", "a?b", "a*b", "a\"b",
    ];
    let accepted: Vec<_> = names.iter().filter(|n| fsguard::Part::new(n).is_ok()).collect();
    assert!(accepted.is_empty(), "accepted: {accepted:?}");
    assert!(fsguard::Part::skill("nul").is_err());
    for ok in ["console", "com10", "lpt", "nul-check", "auxiliary.md", "CLAUDE.md"] {
        assert!(fsguard::Part::new(ok).is_ok(), "{ok} is an ordinary name");
    }
}
