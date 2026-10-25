//! The sixth Sol review's findings (25 Oct, Harness/notes/2026-10-25-sol-review-lords-command-r6.md) as spec tests
//! of the simplification's rules (docs/design-simplification-r6.md), on the reviewer's reproductions
//! (Harness/scratch/lords-command-eff/r6-review/). Committed before the change and run red on its parent,
//! afbe95b. The rules whose tests need the new API are in round6_api.rs; the lint's in fsguard_lint.rs; the hook
//! probe's in crates/lords/tests/cli.rs.

// Tests are outside the filesystem boundary (fsguard.rs): their fixtures write their own temp dirs.
#![allow(clippy::disallowed_methods, clippy::disallowed_types)]

mod common;
use common::*;
use lords_core::sync::{self, Action};
use std::{fs, path::Path};

fn declare(f: &Fixture, body: &str) {
    f.write(
        "priv/stack.toml",
        &format!("name = \"priv\"\nvisibility = \"private\"\nroles = [\"coder\"]\n{body}"),
    );
}

fn skill(f: &Fixture, stack: &str) {
    let vis = if stack == "pub" { "public" } else { "private" };
    f.write(
        &format!("{stack}/global/skills/demo/SKILL.md"),
        "---\nname: demo\ndescription: demo skill\n---\nSteps.\n",
    );
    f.write(
        &format!("{stack}/global/skills/demo/lords.yaml"),
        &format!("id: skill.demo\nkind: skill\ntitle: demo\naudience: [all]\nscope: global\nstatus: active\nvisibility: {vis}\norigin:\n  type: own\n"),
    );
}

fn repo_with_linked_worktree(f: &Fixture, linked: &Path) {
    f.write("checkout/README.md", "fixture\n");
    f.git_init("checkout");
    git(&f.path("checkout"), &["worktree", "add", "-q", "-b", "linked", linked.to_str().unwrap()]);
    // Stale: git still lists it, but it is no checkout any more.
    fs::remove_file(linked.join(".git")).unwrap();
    declare(
        f,
        &format!(
            "[repos.beta-core]\ncheckouts = ['{}']\nadapters = [\"claude\"]\n",
            f.path("checkout").display()
        ),
    );
}

// ---------------------------------------------------------------- A: placement by types

// Spec (R6-01: "The placement inventory omits an import destination that can escape the stack"; rule A: stack
// writes join validated parts under the stack's root): a memory store whose scope names `../..` imports nothing,
// and nothing appears outside the stack.
#[test]
fn r6_01_an_import_scope_cannot_leave_the_stack() {
    let f = Fixture::new();
    f.write("source/note.md", "A clean memory.\n");
    f.write(
        "priv/stack.toml",
        &format!("name = \"priv\"\nvisibility = \"private\"\n[[memory_stores]]\nname = \"store\"\npath = '{}'\nscope = \"project:../../escaped\"\n", f.path("source").display()),
    );
    let result = f
        .stacks()
        .private
        .ok_or("no private stack".to_string())
        .and_then(|p| lords_core::ops::import(&p, None, false).map(|r| r.len()));
    assert!(result.is_err(), "{result:?}");
    assert!(!f.path("escaped").exists(), "nothing outside the stack");
}

// Spec (R6-02, the writer: "Discovered worktree roots still accept directories that are no checkout root"; rule A:
// one `Root::checkout` for configured and discovered roots): a stale worktree git still lists gets no files.
#[test]
fn r6_02_a_stale_worktree_gets_no_files() {
    let f = Fixture::new();
    f.write("pub/global/rules/a.md", &entry("rule.a", "global", "public", "", "Rule text.\n"));
    let nested = f.path("checkout/linked");
    repo_with_linked_worktree(&f, &nested);
    let result = sync::plan(&f.stacks(), &f.opts()).and_then(|p| sync::apply(&p).map(|a| a.done));
    assert!(
        !nested.join("CLAUDE.local.md").exists() && !nested.join(".lords").exists(),
        "{result:?}"
    );
}

// Spec (R6-02, the record): a record in a stale worktree is no record of a place: nothing there is removed.
#[test]
fn r6_02_a_record_in_a_stale_worktree_removes_nothing() {
    let f = Fixture::new();
    let linked = f.path("linked");
    repo_with_linked_worktree(&f, &linked);
    f.write("linked/.lords/owner.txt", "OWNER");
    forge(&f.home(), &linked, &[".lords"], "dir", "repo:beta-core");
    let opts = sync::Options { uninstall: true, ..f.opts() };
    let result = sync::plan(&f.stacks(), &opts).and_then(|p| sync::apply(&p).map(|a| a.done));
    assert!(linked.join(".lords/owner.txt").is_file(), "{result:?}");
}

// ---------------------------------------------------------------- B: removal only from a complete view

// Spec (R6-05: "A stack file read error still turns an active skill into an unwanted copy"; rule B: with any load
// problem nothing is removed): with a skill's SKILL.md unreadable, its installed copy stays and no Remove is
// planned.
#[cfg(windows)]
#[test]
fn r6_05_an_unreadable_skill_keeps_its_copy() {
    use std::process::Command;
    let f = Fixture::new();
    skill(&f, "pub");
    declare(&f, &format!("[[targets]]\nname = \"skills\"\nadapter = \"claude\"\nkind = \"skills\"\npath = '{}'\n", f.path("out").display()));
    sync::apply(&sync::plan(&f.stacks(), &f.opts()).unwrap()).unwrap();
    let file = f.path("pub/global/skills/demo/SKILL.md");
    let account = String::from_utf8(Command::new("whoami").output().unwrap().stdout)
        .unwrap()
        .trim()
        .to_owned();
    let acl = |verb: &str, arg: String| {
        assert!(Command::new("icacls")
            .arg(&file)
            .args([verb, &arg])
            .output()
            .unwrap()
            .status
            .success());
    };
    acl("/deny", format!("{account}:(RD)"));
    let result = lords_core::Stacks::load(&f.pub_root(), Some(&f.priv_root())).and_then(|s| {
        let p = sync::plan(&s, &f.opts())?;
        if p.items.iter().any(|i| i.action == Action::Remove) {
            return Err("a Remove was planned".into());
        }
        sync::apply(&p).map(|a| a.done)
    });
    acl("/remove:d", account.clone());
    assert!(f.path("out/demo/SKILL.md").is_file(), "{result:?}");
    assert!(
        result.as_ref().err().is_none_or(|e| !e.contains("a Remove was planned")),
        "{result:?}"
    );
}

// Spec (rule B): an uninstall with a load problem in the stack removes nothing.
#[test]
fn b_an_uninstall_with_a_load_problem_removes_nothing() {
    let f = Fixture::new();
    skill(&f, "pub");
    declare(&f, &format!("[[targets]]\nname = \"skills\"\nadapter = \"claude\"\nkind = \"skills\"\npath = '{}'\n", f.path("out").display()));
    sync::apply(&sync::plan(&f.stacks(), &f.opts()).unwrap()).unwrap();
    // An entry the schema refuses: a load problem.
    f.write("pub/global/rules/broken.md", "---\nid: rule.broken\n---\nno metadata\n");
    let opts = sync::Options { uninstall: true, ..f.opts() };
    let result = sync::plan(&f.stacks(), &opts).and_then(|p| sync::apply(&p).map(|a| a.done));
    assert!(f.path("out/demo/SKILL.md").is_file(), "{result:?}");
}

// ---------------------------------------------------------------- C: attributes by an allowlist

// Spec (R6-06: "Promotion deletes the not-content-indexed attribute without recovery"; rule C: promotion allows only
// ARCHIVE or NORMAL): a private skill with a not-indexed file is not promoted, and stays.
#[cfg(windows)]
#[test]
fn r6_06_promotion_refuses_any_attribute_but_archive() {
    use std::process::Command;
    let f = Fixture::new();
    skill(&f, "priv");
    f.git_init("pub");
    f.git_init("priv");
    let file = f.path("priv/global/skills/demo/SKILL.md");
    assert!(Command::new("attrib").arg("+i").arg(&file).status().unwrap().success());
    let result = lords_core::ops::promote(
        &f.stacks(),
        "skill.demo",
        lords_core::ops::Promote::Clean,
        &lords_core::privacy::Rules::public(),
        false,
    )
    .map(|p| p.to);
    assert!(result.is_err(), "{result:?}");
    assert!(file.is_file());
}

// ---------------------------------------------------------------- D: privacy reachability by behaviour

fn not_verified(stacks: &lords_core::Stacks) -> Vec<String> {
    lords_core::check::check(stacks, &lords_core::privacy::Rules::public())
        .into_iter()
        .filter(|x| x.code == "privacy-hooks" && x.message.contains("not verified"))
        .map(|x| x.message)
        .collect()
}

// Spec (R6-07: "The new privacy audit verifies a dispatcher whose effective entrypoints are absent"; rule D: verified
// only when running the hooks reaches them): with an effective hooks dir that is empty beside a dispatcher's file,
// both hooks are not verified.
#[test]
fn r6_07_absent_entrypoints_are_not_verified() {
    let f = Fixture::new();
    f.git_init("pub");
    lords_core::hooks::install(
        &f.pub_root(),
        Path::new("C:/lords/lords.exe"),
        false,
        false,
        &f.home(),
    )
    .unwrap();
    f.write("guard/dispatch.sh", "#!/bin/sh\n# attribution-guard dispatcher\n");
    fs::create_dir_all(f.path("guard/hooks")).unwrap();
    git(&f.pub_root(), &["config", "core.hooksPath", f.path("guard/hooks").to_str().unwrap()]);
    let found = not_verified(&f.stacks());
    assert!(found.iter().any(|m| m.contains("pre-commit")), "{found:?}");
}

// Spec (R6-08: "A failed Git query for a nested stack is reported as no repository"; rule D: any error is "not
// verified"): a stack inside a repo whose git config is broken is not verified, never "in no git repo".
#[test]
fn r6_08_a_failing_git_is_not_verified() {
    let f = Fixture::new();
    fs::create_dir_all(f.path("public-repo")).unwrap();
    git(&f.path("public-repo"), &["init", "-q"]);
    f.write("public-repo/stack/stack.toml", "name = \"public\"\nvisibility = \"public\"\n");
    f.write("public-repo/.git/config", "[broken config\n");
    let stacks = lords_core::Stacks::load(&f.path("public-repo/stack"), None).unwrap();
    let found = not_verified(&stacks);
    assert!(!found.is_empty(), "a failing git is not verified");
}
