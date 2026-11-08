//! `--repo` selectors and scopes end to end (gap 4b, design note Harness/notes/2026-11-08-lords-gap4b-selectors.md):
//! a place is named by its key in any case, a path in it or its dir name, never by a guess between two; uninstall
//! takes a key only; a scoped plan names the declared places it leaves out. Child processes get an empty home and
//! global git config, so no real home dir, user hook or setting is read or touched. Every run here is a dry run.

// Tests are outside the filesystem boundary (fsguard.rs): their fixtures write their own temp dirs.
#![allow(clippy::disallowed_methods, clippy::disallowed_types)]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use tempfile::TempDir;

struct Env {
    dir: TempDir,
}

impl Env {
    /// A public stack, and a private one declaring `workspaces` (key, dir relative to the env), each dir made.
    fn new(workspaces: &[(&str, &str)]) -> Env {
        let dir = TempDir::new().unwrap();
        let env = Env { dir };
        let email = ["test", "example.invalid"].join("@");
        env.write(
            "gitconfig",
            &format!("[user]\n\tname = Test\n\temail = {email}\n[init]\n\tdefaultBranch = main\n"),
        );
        fs::create_dir_all(env.path("home")).unwrap();
        env.write("pub/stack.toml", "name = \"pub\"\nvisibility = \"public\"\n");
        let mut manifest = String::from("name = \"priv\"\nvisibility = \"private\"\n");
        for (key, rel) in workspaces {
            fs::create_dir_all(env.path(rel)).unwrap();
            manifest.push_str(&format!(
                "[workspaces.{key}]\npath = '{}'\nadapters = [\"codex\"]\n",
                env.path(rel).display()
            ));
        }
        env.write("priv/stack.toml", &manifest);
        for args in [&["init", "-q"][..], &["add", "-A"], &["commit", "-q", "-m", "start"]] {
            let o = env.cmd("git", &env.path("priv")).args(args).output().unwrap();
            assert!(o.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&o.stderr));
        }
        env
    }

    fn path(&self, rel: &str) -> PathBuf {
        self.dir.path().join(rel)
    }

    fn write(&self, rel: &str, text: &str) {
        let p = self.path(rel);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(&p, text).unwrap();
    }

    fn cmd(&self, program: &str, cwd: &Path) -> Command {
        let mut c = Command::new(program);
        c.current_dir(cwd)
            .env("GIT_CONFIG_GLOBAL", self.path("gitconfig"))
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("HOME", self.path("home"))
            .env("USERPROFILE", self.path("home"))
            .env("LORDS_CODEX", env!("CARGO_BIN_EXE_lords-codex-stub"));
        c
    }

    /// `lords COMMAND` on this env's stacks with `args`, run in `cwd`.
    fn lords_in(&self, cwd: &Path, command: &[&str], args: &[&str]) -> Output {
        let (public, private) = (self.path("pub"), self.path("priv"));
        let stacks = [
            "--public-stack",
            public.to_str().unwrap(),
            "--private-stack",
            private.to_str().unwrap(),
        ];
        let all = [command, &stacks[..], args].concat();
        self.cmd(env!("CARGO_BIN_EXE_lords"), cwd).args(all).output().unwrap()
    }

    fn sync(&self, args: &[&str]) -> Output {
        self.lords_in(self.dir.path(), &["sync"], args)
    }
}

fn text(o: &Output) -> String {
    format!("{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr))
}

/// The plan names the workspace's Codex file in `dir`: the selector reached that place.
fn plans_place(o: &Output, dir: &Path) -> bool {
    o.status.success() && names_file(o, dir)
}

/// A safe-only plan of a place with no records yet holds it (D13: exit 1), and still names its file.
fn safe_only_plans_place(o: &Output, dir: &Path) -> bool {
    o.status.code() == Some(1) && names_file(o, dir) || plans_place(o, dir)
}

fn names_file(o: &Output, dir: &Path) -> bool {
    let file = dir.join("AGENTS.override.md");
    let stdout = String::from_utf8_lossy(&o.stdout).replace('\\', "/");
    stdout.contains(&file.display().to_string().replace('\\', "/"))
}

// Spec (tool contract, L-select: "name a place by any form it would guess (case-insensitive key, path, directory
// name)"; witness W-F8: `--repo Harness` for the workspace `harness`): the key in another case, the place's dir, a
// dir inside it, `.` from inside it, and its dir name each select the place, for sync, sync --safe-only and compose;
// the key it resolved to is said when it isn't what was typed.
#[test]
fn a_place_is_selected_by_its_key_in_any_case_a_path_or_its_dir_name() {
    let env = Env::new(&[("work", "Harness")]);
    let dir = env.path("Harness");
    fs::create_dir_all(dir.join("sub")).unwrap();
    let typed = [
        "WORK".to_string(),
        dir.display().to_string(),
        dir.join("sub").display().to_string(),
        "harness".to_string(),
    ];
    for t in &typed {
        let o = env.sync(&["--repo", t]);
        assert!(plans_place(&o, &dir), "--repo {t}: {}", text(&o));
        assert!(String::from_utf8_lossy(&o.stderr).contains("`work`"), "--repo {t}: {}", text(&o));
        let o = env.sync(&["--safe-only", "--repo", t]);
        assert!(safe_only_plans_place(&o, &dir), "--safe-only --repo {t}: {}", text(&o));
    }
    let o = env.lords_in(&dir.join("sub"), &["sync"], &["--repo", "."]);
    assert!(plans_place(&o, &dir), "--repo . from inside: {}", text(&o));
    // The exact key resolves as before, and nothing is said about it.
    let o = env.sync(&["--repo", "work"]);
    assert!(plans_place(&o, &dir) && !text(&o).contains("--repo work:"), "{}", text(&o));
    let o = env.lords_in(env.dir.path(), &["compose"], &["--repo", "HARNESS", "--role", "coder"]);
    assert!(String::from_utf8_lossy(&o.stderr).contains("`work`"), "compose: {}", text(&o));
}

// Spec (design note S2, attacks A3, A5: never a guess between two): two places with the same dir name are refused
// naming both candidates, while each key still resolves; of two nested places, a path resolves to the deepest that
// holds it.
#[test]
fn a_selector_matching_two_places_is_refused_naming_each_and_the_deepest_path_wins() {
    let env = Env::new(&[("a", "x/same"), ("b", "y/same"), ("outer", "n"), ("inner", "n/in")]);
    let o = env.sync(&["--repo", "same"]);
    let out = text(&o);
    assert!(!o.status.success() && out.contains("`a`") && out.contains("`b`"), "{out}");
    assert!(plans_place(&env.sync(&["--repo", "A"]), &env.path("x/same")));
    fs::create_dir_all(env.path("n/in/deep")).unwrap();
    fs::create_dir_all(env.path("n/other")).unwrap();
    let o = env.sync(&["--repo", env.path("n/in/deep").to_str().unwrap()]);
    assert!(plans_place(&o, &env.path("n/in")), "{}", text(&o));
    assert!(String::from_utf8_lossy(&o.stderr).contains("`inner`"), "{}", text(&o));
    let o = env.sync(&["--repo", env.path("n/other").to_str().unwrap()]);
    assert!(String::from_utf8_lossy(&o.stderr).contains("`outer`"), "{}", text(&o));
}

// Spec (design note S5, attack A6): uninstall takes a key, in any case, but never a path, which could name one
// worktree yet remove the whole repo's copies; the refusal points to `--place DIR`.
#[test]
fn uninstall_takes_a_key_in_any_case_but_never_a_path() {
    let env = Env::new(&[("work", "Harness")]);
    let dir = env.path("Harness");
    let o = env.lords_in(env.dir.path(), &["uninstall"], &["--repo", dir.to_str().unwrap()]);
    let out = text(&o);
    assert!(!o.status.success() && out.contains("--place DIR"), "{out}");
    let o = env.lords_in(env.dir.path(), &["uninstall"], &["--repo", "harness"]);
    let out = text(&o);
    assert!(!o.status.success(), "a dir name isn't a key for uninstall: {out}");
    let o = env.lords_in(env.dir.path(), &["uninstall"], &["--repo", "WORK"]);
    let out = text(&o);
    assert!(o.status.success() && out.contains("`work`") && out.contains("dry run"), "{out}");
}

// Spec (tool contract, L-select: "be told what a scope leaves out"; witness W-F9: `--user` said in sync with
// Harness behind): --user names every declared place it leaves out, --repo every other one, in text and JSON; no
// scope leaves nothing out and says nothing.
#[test]
fn a_scoped_plan_names_the_places_it_leaves_out() {
    let env = Env::new(&[("work", "Harness"), ("notes", "Notes")]);
    let out = String::from_utf8_lossy(&env.sync(&["--user"]).stdout).into_owned();
    assert!(
        out.contains("not in this scope: workspace notes, workspace work")
            || out.contains("not in this scope: workspace work, workspace notes"),
        "{out}"
    );
    let out = String::from_utf8_lossy(&env.sync(&["--safe-only", "--user"]).stdout).into_owned();
    assert!(out.contains("not in this scope:") && out.contains("workspace work"), "{out}");
    let out = String::from_utf8_lossy(&env.sync(&["--repo", "work"]).stdout).into_owned();
    assert!(out.contains("not in this scope: workspace notes;"), "{out}");
    let json = String::from_utf8_lossy(&env.sync(&["--user", "--json"]).stdout).into_owned();
    let v: serde_json::Value = serde_json::from_str(&json).unwrap();
    let left: Vec<&str> =
        v["out_of_scope"].as_array().unwrap().iter().map(|s| s.as_str().unwrap()).collect();
    assert!(left.contains(&"workspace work") && left.contains(&"workspace notes"), "{json}");
    let out = String::from_utf8_lossy(&env.sync(&[]).stdout).into_owned();
    assert!(!out.contains("not in this scope"), "{out}");
}
