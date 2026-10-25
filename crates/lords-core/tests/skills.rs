//! Where skills install (orchestrator decision, 25 Oct: "Development-role skills don't install at user level. They
//! go into each checkout or workspace, as junctions under .claude/skills/ (Claude) and in the repo-file index
//! (Codex) … Add each junction's exact path to .git/info/exclude, never the directory … check flags a name clash
//! between a stack skill and a tracked one … User-level skill dirs keep only skills without roles"). Since the
//! same day's incident, copies instead of junctions (orchestrator: "managed copies for Claude (deletable with the
//! worktree without harming the source) plus the index for Codex"; tests/copies.rs).

mod common;
use common::*;
use lords_core::sync::{apply, plan, Action, Options};
use std::fs;

fn skill(f: &Fixture, name: &str, roles: &str) {
    f.write(
        &format!("pub/global/skills/{name}/SKILL.md"),
        &format!("---\nname: {name}\ndescription: the {name} skill\n---\nSteps.\n"),
    );
    f.write(
        &format!("pub/global/skills/{name}/lords.yaml"),
        &format!("id: skill.{name}\nkind: skill\ntitle: {name}\naudience: [all]\n{roles}scope: global\nstatus: active\nvisibility: public\norigin:\n  type: own\n"),
    );
}

// Spec (above): a development skill skips the user-level skills dir and is linked into each checkout's
// .claude/skills/<name> instead, excluded by its exact path; a skill without roles stays user-level; every
// adapter's place file lists the place's skills.
#[test]
fn development_skills_install_per_checkout() {
    let f = Fixture::new();
    skill(&f, "spec-tests", "roles: [orchestrator, coder, helper, reviewer]\n");
    skill(&f, "any-run", "");
    let checkout = f.path("checkout");
    f.write("checkout/README.md", "a repo\n");
    f.git_init("checkout");
    let user_dir = f.path("home/skills");
    f.write(
        "priv/stack.toml",
        &format!(
            "name = \"priv\"\nvisibility = \"private\"\nroles = [\"orchestrator\", \"coder\", \"helper\", \"reviewer\", \"product\"]\n[repos.beta-core]\ncheckouts = ['{}']\nworktrees = false\n[[targets]]\nname = \"skills\"\nadapter = \"claude\"\nkind = \"skills\"\npath = '{}'\n",
            checkout.display(),
            user_dir.display()
        ),
    );
    let p = plan(&f.stacks(), &f.opts()).unwrap();
    let at = |path: std::path::PathBuf| {
        p.items.iter().find(|i| i.path == path).map(|i| i.action.clone())
    };
    assert_eq!(at(user_dir.join("any-run")), Some(Action::Install));
    assert_eq!(at(user_dir.join("spec-tests")), None, "a development skill isn't user-level");
    assert_eq!(at(checkout.join(".claude/skills/spec-tests")), Some(Action::Install));
    assert_eq!(at(checkout.join(".claude/skills/any-run")), None);
    let codex = p
        .items
        .iter()
        .find(|i| i.path == checkout.join("AGENTS.override.md"))
        .unwrap()
        .proposed
        .clone()
        .unwrap();
    assert!(
        codex.contains("# Skills here\n\nIn `")
            && codex.contains(":\n- spec-tests: the spec-tests skill\n"),
        "{codex}"
    );

    apply(&p).unwrap();
    // A copy, not a link (orchestrator, 25 Oct: "managed copies for Claude (deletable with the worktree without
    // harming the source)"), recorded in the ledger; it ignores itself, so the exclude file needs no line for it.
    let installed = checkout.join(".claude/skills/spec-tests");
    assert!(fs::read_link(&installed).is_err() && installed.join("SKILL.md").is_file());
    assert!(lords_core::ledger::Ledger::load(&f.home()).unwrap().get(&installed).is_some());
    let exclude = fs::read_to_string(checkout.join(".git/info/exclude")).unwrap();
    assert!(!exclude.contains(".claude/skills"), "{exclude}");
    assert_eq!(git(&checkout, &["status", "--porcelain"]), "");
}

// Spec (above): where the repo tracks a skill of the same name, linking is a conflict (nothing is applied) and
// `check` reports the clash.
#[test]
fn a_tracked_skill_of_the_same_name_is_a_clash() {
    let f = Fixture::new();
    skill(&f, "verify", "roles: [coder]\n");
    let checkout = f.path("checkout");
    f.write(
        "checkout/.claude/skills/verify/SKILL.md",
        "---\nname: verify\ndescription: the repo's own\n---\nX\n",
    );
    f.git_init("checkout");
    f.write(
        "priv/stack.toml",
        &format!(
            "name = \"priv\"\nvisibility = \"private\"\nroles = [\"coder\"]\n[repos.beta-core]\ncheckouts = ['{}']\nworktrees = false\nskills = ['.claude/skills']\n",
            checkout.display()
        ),
    );
    let p = plan(&f.stacks(), &f.opts()).unwrap();
    assert!(p
        .items
        .iter()
        .any(|i| i.path == checkout.join(".claude/skills/verify") && i.action == Action::Conflict));
    assert!(apply(&p).is_err());
    assert!(has(&f.check(), "skill-clash", "skill.verify"));
}

// Spec (above: "the four adapted skills are development skills, so their user-level junctions are dropped. The
// non-OWL skills … aren't ours to manage; leave them untouched"; design: Ownership boundary, the changeover: "an
// old copy is moved to the backup only on the owner's confirmation"): sync leaves every dir in a user-level skills
// dir that the ledger doesn't hold, an older copy of a stack skill included; `set_aside`, the human's step, moves
// one into a new backup dir, and `restore` moves it back.
#[test]
fn an_old_user_level_copy_goes_only_by_a_humans_step() {
    let f = Fixture::new();
    skill(
        &f,
        "spec-tests",
        "roles: [coder]
",
    );
    let dir = f.path("home/skills");
    f.write(
        "home/skills/spec-tests/SKILL.md",
        "---
name: spec-tests
---
An old copy.
",
    );
    f.write(
        "home/skills/frontend-design/SKILL.md",
        "not ours
",
    );
    f.write(
        "priv/stack.toml",
        &format!(
            "name = \"priv\"
visibility = \"private\"
roles = [\"coder\"]
[[targets]]
name = \"skills\"
adapter = \"codex\"
kind = \"skills\"
path = '{}'
",
            dir.display()
        ),
    );
    let p = plan(&f.stacks(), &Options { user_only: true, ..f.opts() }).unwrap();
    assert!(p.items.iter().all(|i| !i.path.starts_with(&dir)), "{:#?}", p.items);
    let set = lords_core::sync::set_aside(&f.home(), &[dir.join("spec-tests")]).unwrap();
    assert!(!dir.join("spec-tests").exists() && dir.join("frontend-design/SKILL.md").exists());
    let backups = set.backups.unwrap();
    assert!(
        texts_under(&backups).iter().any(|t| t.contains("An old copy.")),
        "the copy is in the backup"
    );
    lords_core::sync::restore(&f.home(), &backups).unwrap();
    assert_eq!(
        f.read("home/skills/spec-tests/SKILL.md"),
        "---
name: spec-tests
---
An old copy.
"
    );
}

/// Every file's text under `dir`, recursively.
fn texts_under(dir: &std::path::Path) -> Vec<String> {
    let mut out = Vec::new();
    for e in fs::read_dir(dir).into_iter().flatten().flatten() {
        if e.path().is_dir() {
            out.extend(texts_under(&e.path()));
        } else {
            out.push(fs::read_to_string(e.path()).unwrap_or_default());
        }
    }
    out
}
