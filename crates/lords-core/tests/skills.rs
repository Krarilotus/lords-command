//! Where skills install (orchestrator decision, 25 Oct: "Development-role skills don't install at user level. They
//! go into each checkout or workspace, as junctions under .claude/skills/ (Claude) and in the repo-file index
//! (Codex) … Add each junction's exact path to .git/info/exclude, never the directory … check flags a name clash
//! between a stack skill and a tracked one … User-level skill dirs keep only skills without roles").

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
    let p = plan(&f.stacks(), &Options::default()).unwrap();
    let at = |path: std::path::PathBuf| {
        p.items.iter().find(|i| i.path == path).map(|i| i.action.clone())
    };
    assert_eq!(at(user_dir.join("any-run")), Some(Action::Link));
    assert_eq!(at(user_dir.join("spec-tests")), None, "a development skill isn't user-level");
    assert_eq!(at(checkout.join(".claude/skills/spec-tests")), Some(Action::Link));
    assert_eq!(at(checkout.join(".claude/skills/any-run")), None);
    let codex = p
        .items
        .iter()
        .find(|i| i.path == checkout.join("AGENTS.override.md"))
        .unwrap()
        .proposed
        .clone()
        .unwrap();
    assert!(codex.contains("# Skills here\n\n- spec-tests: the spec-tests skill (`"), "{codex}");

    apply(&p, &f.path("backups")).unwrap();
    assert!(
        fs::read_link(checkout.join(".claude/skills/spec-tests")).is_ok(),
        "a link, not a copy"
    );
    let exclude = fs::read_to_string(checkout.join(".git/info/exclude")).unwrap();
    assert!(
        exclude.lines().any(|l| l == ".claude/skills/spec-tests")
            && !exclude.lines().any(|l| l == ".claude/skills" || l == ".claude/skills/"),
        "{exclude}"
    );
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
    let p = plan(&f.stacks(), &Options::default()).unwrap();
    assert!(p
        .items
        .iter()
        .any(|i| i.path == checkout.join(".claude/skills/verify") && i.action == Action::Conflict));
    assert!(apply(&p, &f.path("backups")).is_err());
    assert!(has(&f.check(), "skill-clash", "skill.verify"));
}

// Spec (above: "the four adapted skills are development skills, so their user-level junctions are dropped. The
// non-OWL skills … aren't ours to manage; leave them untouched"): in a user-level skills dir, a plain copy of a
// stack skill that no longer installs there is backed up and removed; a dir that is no stack skill stays.
#[test]
fn an_old_user_level_copy_of_a_development_skill_is_retired() {
    let f = Fixture::new();
    skill(&f, "spec-tests", "roles: [coder]\n");
    let dir = f.path("home/skills");
    f.write("home/skills/spec-tests/SKILL.md", "an old copy\n");
    f.write("home/skills/frontend-design/SKILL.md", "not ours\n");
    f.write(
        "priv/stack.toml",
        &format!("name = \"priv\"\nvisibility = \"private\"\nroles = [\"coder\"]\n[[targets]]\nname = \"skills\"\nadapter = \"codex\"\nkind = \"skills\"\npath = '{}'\n", dir.display()),
    );
    let p = plan(&f.stacks(), &Options { user_only: true, ..Options::default() }).unwrap();
    let action =
        |name: &str| p.items.iter().find(|i| i.path == dir.join(name)).map(|i| i.action.clone());
    assert_eq!(action("spec-tests"), Some(Action::RemoveDir));
    assert_eq!(action("frontend-design"), None);
    apply(&p, &f.path("backups")).unwrap();
    assert!(!dir.join("spec-tests").exists() && dir.join("frontend-design/SKILL.md").exists());
    let backed: Vec<_> = fs::read_dir(f.path("backups")).unwrap().flatten().collect();
    assert_eq!(backed.len(), 1, "the copy is in the backup");
}
