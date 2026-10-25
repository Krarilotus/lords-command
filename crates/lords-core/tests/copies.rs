//! Skills installed as managed copies (orchestrator, 25 Oct, after `git worktree remove --force` followed a skill
//! junction and emptied the stack's skill dirs: "managed copies for Claude (deletable with the worktree without
//! harming the source) plus the index for Codex"; symlinks need admin rights or developer mode here).

mod common;
use common::*;
use lords_core::sync::{apply, plan, Action, Options, Plan};
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
    plan(&f.stacks(), &Options::default()).unwrap()
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
    apply(&plan_all(&f), &f.path("backups/one")).unwrap();
    assert!(wt.join(".claude/skills/spec-tests/SKILL.md").is_file(), "installed in the worktree");
    git(&checkout, &["worktree", "remove", "--force", wt.to_str().unwrap()]);
    assert!(!wt.exists());
    assert_eq!(
        f.read("pub/global/skills/spec-tests/SKILL.md"),
        "---\nname: spec-tests\n---\nSteps.\n"
    );
}

// Spec (above: sync refreshes a copy only while nobody changed it): a copy of an older version of the skill is
// refreshed (the old copy kept in the backup); a copy changed by hand is a conflict, and apply leaves it.
#[test]
fn an_unchanged_copy_refreshes_and_a_changed_one_is_a_conflict() {
    let f = Fixture::new();
    skill(&f, "spec-tests", "---\nname: spec-tests\n---\nOld steps.\n");
    let checkout = repo(&f);
    let installed = checkout.join(".claude/skills/spec-tests");
    apply(&plan_all(&f), &f.path("backups/one")).unwrap();
    skill(&f, "spec-tests", "---\nname: spec-tests\n---\nNew steps.\n");
    let p = plan_all(&f);
    assert_eq!(action(&p, &installed), Some(Action::Refresh));
    apply(&p, &f.path("backups/two")).unwrap();
    assert_eq!(
        fs::read_to_string(installed.join("SKILL.md")).unwrap(),
        "---\nname: spec-tests\n---\nNew steps.\n"
    );
    assert!(
        texts(&f.path("backups/two/dirs")).iter().any(|t| t.contains("Old steps.")),
        "the old copy is kept"
    );

    fs::write(installed.join("SKILL.md"), "my own edit\n").unwrap();
    let p = plan_all(&f);
    assert_eq!(action(&p, &installed), Some(Action::Conflict));
    assert!(apply(&p, &f.path("backups/three")).is_err());
    assert_eq!(fs::read_to_string(installed.join("SKILL.md")).unwrap(), "my own edit\n");
}

// Spec (above: a link from an earlier version gives way to a copy): sync removes the link itself and installs a
// copy in its place; the stack's skill is untouched.
#[test]
fn an_old_link_of_ours_becomes_a_copy() {
    let f = Fixture::new();
    skill(&f, "spec-tests", "---\nname: spec-tests\n---\nSteps.\n");
    let checkout = repo(&f);
    let installed = checkout.join(".claude/skills/spec-tests");
    dir_link(&installed, &f.path("pub/global/skills/spec-tests"));
    let p = plan_all(&f);
    assert_eq!(action(&p, &installed), Some(Action::ReplaceLink));
    apply(&p, &f.path("backups/one")).unwrap();
    assert!(fs::read_link(&installed).is_err() && installed.join("SKILL.md").is_file());
    assert!(f.path("pub/global/skills/spec-tests/SKILL.md").is_file());
    assert!(f.path("pub/global/skills/spec-tests/lords.yaml").is_file());
}

// Spec (above, and finding 8's rollback): `restore` takes back an install, removing the copy while it is still
// exactly the one apply made.
#[test]
fn restore_removes_an_installed_copy() {
    let f = Fixture::new();
    skill(&f, "spec-tests", "---\nname: spec-tests\n---\nSteps.\n");
    let checkout = repo(&f);
    let installed = checkout.join(".claude/skills/spec-tests");
    apply(&plan_all(&f), &f.path("backups/one")).unwrap();
    assert!(installed.is_dir());
    lords_core::sync::restore(&f.path("backups/one")).unwrap();
    assert!(!installed.exists());
    assert!(f.path("pub/global/skills/spec-tests/SKILL.md").is_file());
}

/// Every file's text under `dir`, recursively.
fn texts(dir: &Path) -> Vec<String> {
    let mut out = Vec::new();
    for e in fs::read_dir(dir).into_iter().flatten().flatten() {
        if e.path().is_dir() {
            out.extend(texts(&e.path()));
        } else {
            out.push(fs::read_to_string(e.path()).unwrap_or_default());
        }
    }
    out
}

/// A dir link: a junction on Windows, a symlink elsewhere.
fn dir_link(link: &Path, to: &Path) {
    fs::create_dir_all(link.parent().unwrap()).unwrap();
    #[cfg(windows)]
    {
        let win = |p: &Path| p.to_string_lossy().replace('/', "\\");
        let out = std::process::Command::new("cmd")
            .args(["/C", "mklink", "/J"])
            .arg(win(link))
            .arg(win(to))
            .output()
            .unwrap();
        assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    }
    #[cfg(not(windows))]
    std::os::unix::fs::symlink(to, link).unwrap();
}
