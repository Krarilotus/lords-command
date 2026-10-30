//! Where skills install (orchestrator decision, 25 Oct: "Development-role skills don't install at user level. They
//! go into each checkout or workspace, as junctions under .claude/skills/ (Claude) and in the repo-file index
//! (Codex) … Add each junction's exact path to .git/info/exclude, never the directory … check flags a name clash
//! between a stack skill and a tracked one … User-level skill dirs keep only skills without roles"). Since the
//! same day's incident, copies instead of junctions (orchestrator: "managed copies for Claude (deletable with the
//! worktree without harming the source) plus the index for Codex"; tests/copies.rs).

// Tests are outside the filesystem boundary (fsguard.rs): their fixtures write their own temp dirs.
#![allow(clippy::disallowed_methods, clippy::disallowed_types)]

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

// Spec (orchestrator, 30 Oct, item 2: "Bring `~/.codex/skills` under Lords: doubt-review, scope-discipline,
// stop-the-line, spec-tests and frontend-design are hand copies"; an opt-in per target, the 25 Oct default kept): a
// user-level skills target that names roles installs the skills for those roles; one that names none keeps only
// skills without roles.
#[test]
fn a_skills_target_with_roles_installs_their_skills() {
    let installs = |roles: &str| {
        let f = Fixture::new();
        skill(&f, "spec-tests", "roles: [orchestrator, coder, helper, reviewer]\n");
        skill(&f, "any-run", "");
        let dir = f.path("home/codex-skills");
        f.write(
            "priv/stack.toml",
            &format!(
                "name = \"priv\"\nvisibility = \"private\"\nroles = [\"orchestrator\", \"coder\", \"helper\", \"reviewer\"]\n[[targets]]\nname = \"codex skills\"\nadapter = \"codex\"\nkind = \"skills\"\npath = '{}'\n{roles}",
                dir.display()
            ),
        );
        let p = plan(&f.stacks(), &Options { user_only: true, ..f.opts() }).unwrap();
        let at = |name: &str| {
            p.items.iter().find(|i| i.path == dir.join(name)).map(|i| i.action.clone())
        };
        (at("spec-tests"), at("any-run"))
    };
    assert_eq!(
        installs("roles = [\"coder\", \"reviewer\"]\n"),
        (Some(Action::Install), Some(Action::Install))
    );
    assert_eq!(installs(""), (None, Some(Action::Install)));
}

// Spec (Codex switch-on review r1, R3, witness: the target's roles changed from `coder` to the typo `codeer` selected no
// role skill, and the full plan removed the installed `spec-tests`): a target role that no stack declares is a check
// error and a conflict for that target; nothing it installed is removed.
#[test]
fn an_undeclared_target_role_is_a_conflict_and_removes_nothing() {
    let f = Fixture::new();
    skill(&f, "spec-tests", "roles: [coder]\n");
    let dir = f.path("home/codex-skills");
    let manifest = |roles: &str| {
        format!(
            "name = \"priv\"\nvisibility = \"private\"\nroles = [\"coder\"]\n[[targets]]\nname = \"codex skills\"\nadapter = \"codex\"\nkind = \"skills\"\npath = '{}'\nroles = [\"{roles}\"]\n",
            dir.display()
        )
    };
    f.write("priv/stack.toml", &manifest("coder"));
    apply(&plan(&f.stacks(), &f.opts()).unwrap()).unwrap();
    assert!(dir.join("spec-tests/SKILL.md").is_file());
    f.write("priv/stack.toml", &manifest("codeer"));
    let p = plan(&f.stacks(), &f.opts()).unwrap();
    assert!(
        p.items.iter().any(|i| i.action == Action::Conflict && i.target == "codex skills"),
        "{:?}",
        p.items
    );
    assert!(
        !p.items.iter().any(|i| matches!(i.action, Action::Remove | Action::Forget)),
        "{:?}",
        p.items
    );
    assert!(apply(&p).is_err());
    assert!(dir.join("spec-tests/SKILL.md").is_file(), "still there");
    assert!(f.check().iter().any(|x| x.code == "role-undeclared" && x.message.contains("codeer")));
}

// Spec (above; orchestrator, 27 Oct: "the repo's tracked copy wins in its indexed skill dir and leaves a note; content
// that differs from the stack's stays a finding"): where the repo tracks a skill of the same name in a dir it indexes,
// its copy wins, nothing is installed over it, and `check` reports the difference; where it tracks one in a dir it
// doesn't index, installing over it is still a conflict (nothing is applied).
#[test]
fn a_tracked_skill_of_the_same_name_wins_where_indexed_and_conflicts_elsewhere() {
    for indexed in [true, false] {
        let f = Fixture::new();
        skill(&f, "verify", "roles: [coder]\n");
        let checkout = f.path("checkout");
        f.write(
            "checkout/.claude/skills/verify/SKILL.md",
            "---\nname: verify\ndescription: the repo's own\n---\nX\n",
        );
        f.git_init("checkout");
        let skills = if indexed { "skills = ['.claude/skills']\n" } else { "" };
        f.write(
            "priv/stack.toml",
            &format!(
                "name = \"priv\"\nvisibility = \"private\"\nroles = [\"coder\"]\n[repos.beta-core]\ncheckouts = ['{}']\nworktrees = false\n{skills}",
                checkout.display()
            ),
        );
        let p = plan(&f.stacks(), &f.opts()).unwrap();
        let at = |a: Action| {
            p.items
                .iter()
                .any(|i| i.path == checkout.join(".claude/skills/verify") && i.action == a)
        };
        if indexed {
            assert!(!p.items.iter().any(|i| i.path == checkout.join(".claude/skills/verify")));
            apply(&p).unwrap();
            assert_eq!(
                f.read("checkout/.claude/skills/verify/SKILL.md"),
                "---\nname: verify\ndescription: the repo's own\n---\nX\n"
            );
            assert!(has(&f.check(), "skill-clash", "skill.verify"));
        } else {
            assert!(at(Action::Conflict));
            assert!(apply(&p).is_err());
        }
    }
}

// Spec (above: only a tracked skill "of the same name" wins or conflicts): a skill the repo tracks under another
// name that starts with the stack skill's (`verify-old` beside `verify`) is no clash, and the stack's copy installs.
#[test]
fn a_tracked_skill_whose_name_only_starts_alike_is_no_clash() {
    let f = Fixture::new();
    skill(&f, "verify", "roles: [coder]\n");
    let checkout = f.path("checkout");
    f.write(
        "checkout/.claude/skills/verify-old/SKILL.md",
        "---\nname: verify-old\ndescription: the repo's own\n---\nX\n",
    );
    f.git_init("checkout");
    f.write(
        "priv/stack.toml",
        &format!(
            "name = \"priv\"\nvisibility = \"private\"\nroles = [\"coder\"]\n[repos.beta-core]\ncheckouts = ['{}']\nworktrees = false\n",
            checkout.display()
        ),
    );
    let p = plan(&f.stacks(), &f.opts()).unwrap();
    let verify = p.items.iter().find(|i| i.path == checkout.join(".claude/skills/verify"));
    assert_eq!(verify.map(|i| i.action.clone()), Some(Action::Install), "{:#?}", p.items);
    apply(&p).unwrap();
    assert!(f.path("checkout/.claude/skills/verify/SKILL.md").is_file());
}

// Spec (sol-loop sync-perf r2, S3, witness: a tracked `straße` skill and a stack skill `strasse` were taken for one name,
// a false conflict): only names the filesystem and git's `:(icase)` hold the same are one; `straße` and `strasse`,
// `fıx` and `fix` are two, and the stack's copy installs.
#[cfg(windows)]
#[test]
fn names_alike_only_under_unicode_case_rules_are_no_clash() {
    for (tracked, ours) in [("straße", "strasse"), ("fıx", "fix")] {
        let f = Fixture::new();
        skill(&f, ours, "roles: [coder]\n");
        let checkout = f.path("checkout");
        f.write(
            &format!("checkout/.claude/skills/{tracked}/SKILL.md"),
            &format!("---\nname: {tracked}\ndescription: the repo's own\n---\nX\n"),
        );
        f.git_init("checkout");
        f.write(
            "priv/stack.toml",
            &format!(
                "name = \"priv\"\nvisibility = \"private\"\nroles = [\"coder\"]\n[repos.beta-core]\ncheckouts = ['{}']\nworktrees = false\n",
                checkout.display()
            ),
        );
        let p = plan(&f.stacks(), &f.opts()).unwrap();
        let it = p.items.iter().find(|i| i.path == checkout.join(format!(".claude/skills/{ours}")));
        assert_eq!(
            it.map(|i| i.action.clone()),
            Some(Action::Install),
            "{tracked}: {:#?}",
            p.items
        );
    }
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
    let set = lords_core::sync::set_aside(&p, &[dir.join("spec-tests")]).unwrap();
    assert!(!dir.join("spec-tests").exists() && dir.join("frontend-design/SKILL.md").exists());
    let backups = set.backups.unwrap();
    assert!(
        texts_under(&backups).iter().any(|t| t.contains("An old copy.")),
        "the copy is in the backup"
    );
    lords_core::sync::restore(&p, &backups).unwrap();
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
