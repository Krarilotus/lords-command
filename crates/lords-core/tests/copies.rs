//! Skills installed as tool-owned copies (orchestrator, 25 Oct, after `git worktree remove --force` followed a skill
//! junction and emptied the stack's skill dirs: "managed copies for Claude (deletable with the worktree without
//! harming the source) plus the index for Codex"; design: Ownership boundary: the ledger, not the copy, says whose).

mod common;
use common::*;
use lords_core::sync::{apply, plan, Action, Plan};
use std::fs;
use std::path::Path;

fn skill(f: &Fixture, name: &str, text: &str) {
    f.write(&format!("pub/global/skills/{name}/SKILL.md"), text);
    f.write(
        &format!("pub/global/skills/{name}/lords.yaml"),
        &format!("id: skill.{name}\nkind: skill\ntitle: {name}\naudience: [all]\nroles: [coder]\nscope: global\nstatus: active\nvisibility: public\norigin:\n  type: own\n"),
    );
}

/// A repo with its main checkout and worktrees under `wts`, Claude's files only.
fn repo(f: &Fixture) -> std::path::PathBuf {
    let checkout = f.path("checkout");
    f.write("checkout/README.md", "a repo\n");
    f.git_init("checkout");
    f.write(
        "priv/stack.toml",
        &format!(
            "name = \"priv\"\nvisibility = \"private\"\nroles = [\"coder\"]\n[repos.beta-core]\ncheckouts = ['{}']\nworktree_roots = ['{}']\nadapters = [\"claude\"]\n",
            checkout.display(),
            f.path("wts").display()
        ),
    );
    checkout
}

fn plan_all(f: &Fixture) -> Plan {
    plan(&f.stacks(), &f.opts()).unwrap()
}

fn action(p: &Plan, path: &Path) -> Option<Action> {
    p.items.iter().find(|i| i.path == path).map(|i| i.action.clone())
}

// Spec (above): a worktree with installed skills can be removed with `git worktree remove --force`, and the
// stack's skill files are all still there.
#[test]
fn removing_a_worktree_never_reaches_the_stack() {
    let f = Fixture::new();
    skill(&f, "spec-tests", "---\nname: spec-tests\n---\nSteps.\n");
    let checkout = repo(&f);
    let wt = f.path("wts/one");
    git(&checkout, &["worktree", "add", "-q", "--detach", wt.to_str().unwrap()]);
    apply(&plan_all(&f)).unwrap();
    assert!(wt.join(".claude/skills/spec-tests/SKILL.md").is_file(), "installed in the worktree");
    git(&checkout, &["worktree", "remove", "--force", wt.to_str().unwrap()]);
    assert!(!wt.exists());
    assert_eq!(
        f.read("pub/global/skills/spec-tests/SKILL.md"),
        "---\nname: spec-tests\n---\nSteps.\n"
    );
    // The next plan drops the gone worktree's ledger records.
    let p = plan_all(&f);
    assert!(
        p.items.iter().any(|i| i.action == Action::Forget && i.path.starts_with(&wt)),
        "{:#?}",
        p.items
    );
}

// Spec (above: sync refreshes a copy only while it matches its ledger record): a copy of an older version of the
// skill is replaced whole by the new one; a copy changed by hand is a conflict, and apply leaves it.
#[test]
fn an_unchanged_copy_refreshes_and_a_changed_one_is_a_conflict() {
    let f = Fixture::new();
    skill(&f, "spec-tests", "---\nname: spec-tests\n---\nOld steps.\n");
    let checkout = repo(&f);
    let installed = checkout.join(".claude/skills/spec-tests");
    apply(&plan_all(&f)).unwrap();
    skill(&f, "spec-tests", "---\nname: spec-tests\n---\nNew steps.\n");
    let p = plan_all(&f);
    assert_eq!(action(&p, &installed), Some(Action::Refresh));
    apply(&p).unwrap();
    assert_eq!(
        fs::read_to_string(installed.join("SKILL.md")).unwrap(),
        "---\nname: spec-tests\n---\nNew steps.\n"
    );

    fs::write(installed.join("SKILL.md"), "my own edit\n").unwrap();
    let p = plan_all(&f);
    assert_eq!(action(&p, &installed), Some(Action::Conflict));
    assert!(apply(&p).is_err());
    assert_eq!(fs::read_to_string(installed.join("SKILL.md")).unwrap(), "my own edit\n");
}

// Spec (orchestrator, 25 Oct: "A copy carries an ownership record … stack entry id, source path, sha256 per file";
// "Keep the copies out of git … and size them into the check"; design: the record is the ledger's, never the
// copy's): the ledger holds the copy with its hash and scope; the copy ignores itself in git; `check` sizes it.
#[test]
fn the_ledger_records_each_copy_and_git_never_sees_it() {
    let f = Fixture::new();
    skill(&f, "spec-tests", "---\nname: spec-tests\n---\nSteps.\n");
    let checkout = repo(&f);
    apply(&plan_all(&f)).unwrap();
    let installed = checkout.join(".claude/skills/spec-tests");
    let ledger = lords_core::ledger::Ledger::load(&f.home()).unwrap();
    let owned = ledger.get(&installed).expect("the copy is in the ledger");
    let tree = lords_core::fsguard::read_tree(&installed).unwrap();
    assert_eq!(owned.sha, lords_core::fsguard::tree_sha(&tree));
    assert_eq!((owned.kind.as_str(), owned.scope.as_str()), ("dir", "repo:beta-core/claude"));
    assert!(!tree.contains_key("lords.yaml"), "the sidecar stays in the stack");
    assert_eq!(tree.get(".gitignore").map(Vec::as_slice), Some(b"*\n".as_slice()));
    assert_eq!(git(&checkout, &["status", "--porcelain"]), "");
    let found = lords_core::sync::drift_findings(&f.stacks(), &f.home());
    assert!(
        found.iter().any(|x| x.code == "skill-copies" && x.message.contains("repo beta-core")),
        "{found:#?}"
    );
}

// Spec (design: Ownership boundary, "`lords uninstall`, which removes every ledger path that still matches"): an
// uninstall plan removes the tool's copies and leaves a changed one, and the stack's skill.
#[test]
fn uninstall_removes_only_what_still_matches() {
    let f = Fixture::new();
    skill(&f, "spec-tests", "---\nname: spec-tests\n---\nSteps.\n");
    skill(&f, "other", "---\nname: other\n---\nSteps.\n");
    let checkout = repo(&f);
    apply(&plan_all(&f)).unwrap();
    fs::write(checkout.join(".claude/skills/other/SKILL.md"), "mine now\n").unwrap();
    let opts = lords_core::sync::Options { uninstall: true, ..f.opts() };
    apply(&plan(&f.stacks(), &opts).unwrap()).unwrap();
    assert!(!checkout.join(".claude/skills/spec-tests").exists());
    assert_eq!(
        fs::read_to_string(checkout.join(".claude/skills/other/SKILL.md")).unwrap(),
        "mine now\n"
    );
    assert!(f.path("pub/global/skills/spec-tests/SKILL.md").is_file());
}

// Spec (design: Ownership boundary, the changeover: "Adopting a file records it only if it is byte for byte what
// sync would write now"; orchestrator: adopt stays a human-confirmed step): a place file written before the ledger
// is a conflict sync won't touch; `adopt` records it when it holds exactly what sync writes, and refuses one that
// differs; once adopted, the plan is in sync.
#[test]
fn adopt_records_only_exactly_what_sync_writes() {
    let f = Fixture::new();
    skill(&f, "spec-tests", "---\nname: spec-tests\n---\nSteps.\n");
    f.write(
        "pub/global/rules/dev.md",
        &entry("rule.dev", "global", "public", "roles: [coder]\n", "Dev text.\n"),
    );
    let checkout = repo(&f);
    let local = checkout.join("CLAUDE.local.md");
    let wanted =
        plan_all(&f).items.iter().find(|i| i.path == local).unwrap().proposed.clone().unwrap();
    fs::write(&local, &wanted).unwrap();
    let p = plan_all(&f);
    let item = p.items.iter().find(|i| i.path == local).unwrap();
    assert!(item.action == Action::Conflict && item.adoptable, "{:?}", item.notes);
    assert!(
        lords_core::sync::adopt(&p, &[checkout.join("README.md")]).is_err(),
        "a path the plan doesn't name"
    );
    lords_core::sync::adopt(&p, std::slice::from_ref(&local)).unwrap();
    assert_eq!(action(&plan_all(&f), &local), Some(Action::InSync));

    let roles = checkout.join(".lords");
    fs::create_dir_all(&roles).unwrap();
    fs::write(roles.join("role-coder.md"), "older text\n").unwrap();
    let p = plan_all(&f);
    let item = p.items.iter().find(|i| i.path == roles).unwrap();
    assert!(item.action == Action::Conflict && !item.adoptable);
    assert!(lords_core::sync::adopt(&p, std::slice::from_ref(&roles)).is_err());
    assert_eq!(fs::read_to_string(roles.join("role-coder.md")).unwrap(), "older text\n");
}
