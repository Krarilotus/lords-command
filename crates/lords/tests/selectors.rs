//! `--repo` selectors and scopes end to end (gap 4b, design note Harness/notes/2026-11-08-lords-gap4b-selectors.md):
//! a place is named by its key in any case or a path in it, never by a guess between two nor by a dir name; uninstall
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

    fn git(&self, cwd: &Path, args: &[&str]) {
        let o = self.cmd("git", cwd).args(args).output().unwrap();
        assert!(o.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&o.stderr));
    }

    /// Adds `text` to the private stack's `rel` and commits it.
    fn commit_private(&self, rel: &str, text: &str) {
        let p = self.path("priv").join(rel);
        let before = fs::read_to_string(&p).unwrap_or_default();
        self.write(&format!("priv/{rel}"), &format!("{before}{text}"));
        self.git(&self.path("priv"), &["add", "-A"]);
        self.git(&self.path("priv"), &["commit", "-q", "-m", "more"]);
    }

    /// A repo `key` at `rel` (one commit) with the worktrees `wts/<name>`, declared in the private stack with the
    /// Codex adapter; its main checkout.
    fn repo(&self, key: &str, rel: &str, worktrees: &[&str]) -> PathBuf {
        let main = self.path(rel);
        fs::create_dir_all(&main).unwrap();
        self.git(&main, &["init", "-q"]);
        self.write(&format!("{rel}/README"), "a repo\n");
        self.git(&main, &["add", "-A"]);
        self.git(&main, &["commit", "-q", "-m", "start"]);
        for w in worktrees {
            let at = self.path(&format!("wts/{w}"));
            self.git(&main, &["worktree", "add", "-q", "-b", w, at.to_str().unwrap()]);
        }
        let decl =
            format!("[repos.{key}]\ncheckouts = ['{}']\nadapters = [\"codex\"]\n", main.display());
        self.commit_private("stack.toml", &decl);
        main
    }
}

/// A private rule for the place `scope` (`workspace:NAME` or `repo:NAME`) whose text is `body`.
fn rule(id: &str, scope: &str, body: &str) -> String {
    format!("---\nid: rule.{id}\nkind: rule\ntitle: {id}\naudience: [all]\nscope: {scope}\nstatus: active\nvisibility: private\norigin:\n  type: own\n---\n{body}\n")
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

// Spec (tool contract, L-select: "name a place by any form it would guess (case-insensitive key, path …)", the dir
// name taken out by the master thread after fix check r1 (8 Nov 10:49); witness W-F8: `--repo Harness` for the
// workspace `harness`): the key in another case, the place's dir, a dir inside it and `.` from inside it each select
// the place, for sync and sync --safe-only; the key it resolved to is said when it isn't what was typed.
#[test]
fn a_place_is_selected_by_its_key_in_any_case_or_a_path() {
    let env = Env::new(&[("work", "Harness")]);
    let dir = env.path("Harness");
    fs::create_dir_all(dir.join("sub")).unwrap();
    let typed =
        ["WORK".to_string(), dir.display().to_string(), dir.join("sub").display().to_string()];
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
}

// Spec (review 4b r1, F2): compose by a selector composes the selected place, not another, and an undeclared name is
// refused naming the declared keys.
#[test]
fn compose_by_a_selector_composes_the_selected_place() {
    let env = Env::new(&[("work", "Harness"), ("other", "Other")]);
    env.commit_private(
        "global/rules/w.md",
        &rule("w", "workspace:work", "Only the work place reads this."),
    );
    env.commit_private(
        "global/rules/o.md",
        &rule("o", "workspace:other", "Only the other place reads this."),
    );
    for typed in ["HARNESS", env.path("Harness").to_str().unwrap()] {
        let o = env.lords_in(env.dir.path(), &["compose"], &["--repo", typed]);
        let out = String::from_utf8_lossy(&o.stdout).into_owned();
        assert!(o.status.success(), "compose --repo {typed}: {}", text(&o));
        assert!(out.contains("Only the work place reads this."), "{out}");
        assert!(!out.contains("Only the other place reads this."), "{out}");
    }
    let o = env.lords_in(env.dir.path(), &["compose"], &["--repo", "elsewhere"]);
    assert!(!o.status.success() && text(&o).contains("names: other, work"), "{}", text(&o));
}

// Spec (design note A1: the exact key first): an exact key that is also a dir below the cwd, inside another place,
// selects the key's place.
#[test]
fn an_exact_key_wins_over_a_dir_of_that_name_below_the_cwd() {
    let env = Env::new(&[("work", "Harness"), ("other", "Other")]);
    let inside = env.path("Harness");
    fs::create_dir_all(inside.join("other")).unwrap();
    let o = env.lords_in(&inside, &["sync"], &["--repo", "other"]);
    assert!(plans_place(&o, &env.path("Other")), "{}", text(&o));
    assert!(!names_file(&o, &inside), "{}", text(&o));
}

// Spec (review 4b r1, F1: "a path selector keeps the matched place as the scope, the same as --place DIR"): a path in
// one worktree scopes a writing sync, normal or safe-only, to that worktree; the repo's main checkout and its other
// worktree stay untouched. A --place beside a path selector is refused, and nothing is written.
#[test]
fn a_path_in_one_worktree_scopes_a_writing_sync_to_that_worktree() {
    let env = Env::new(&[]);
    // A place with no records yet is safe to create in, so the safe-only apply writes too (D13's setting).
    env.commit_private("stack.toml", "[safe_only]\napplies = [\"new-place\"]\n");
    let main = env.repo("r", "r", &["a", "b"]);
    env.commit_private("global/rules/r.md", &rule("r", "repo:r", "The repo's rule."));
    let (a, b) = (env.path("wts/a"), env.path("wts/b"));
    let file = |dir: &Path| dir.join("AGENTS.override.md");
    fs::create_dir_all(a.join("sub")).unwrap();
    fs::create_dir_all(b.join("sub")).unwrap();

    let o = env.sync(&["--repo", a.join("sub").to_str().unwrap(), "--apply"]);
    assert!(file(&a).exists(), "the worktree named is written: {}", text(&o));
    assert!(!file(&b).exists() && !file(&main).exists(), "only it: {}", text(&o));

    let o = env.sync(&["--safe-only", "--repo", b.to_str().unwrap(), "--apply"]);
    assert!(file(&b).exists(), "safe-only, the worktree named is written: {}", text(&o));
    assert!(!file(&main).exists(), "safe-only, only it: {}", text(&o));

    let place = ["--place", main.to_str().unwrap()];
    let o = env.sync(&[&["--repo", a.to_str().unwrap(), "--apply"][..], &place].concat());
    assert!(!o.status.success() && text(&o).contains("--place"), "{}", text(&o));
    assert!(!file(&main).exists(), "a refused run writes nothing: {}", text(&o));
}

// Spec (design note A8): a repo whose worktrees can't be listed (its checkout isn't a git repo) is still selected by
// a path in its declared checkout.
#[test]
fn a_repo_whose_worktrees_cant_be_listed_is_selected_by_its_checkout() {
    let env = Env::new(&[]);
    let plain = env.path("Plain");
    fs::create_dir_all(plain.join("sub")).unwrap();
    let decl =
        format!("[repos.plain]\ncheckouts = ['{}']\nadapters = [\"codex\"]\n", plain.display());
    env.commit_private("stack.toml", &decl);
    env.commit_private("global/rules/p.md", &rule("p", "repo:plain", "The plain repo's rule."));
    let o = env.lords_in(
        env.dir.path(),
        &["compose"],
        &["--repo", plain.join("sub").to_str().unwrap()],
    );
    assert!(o.status.success(), "{}", text(&o));
    assert!(String::from_utf8_lossy(&o.stdout).contains("The plain repo's rule."), "{}", text(&o));
}

// Spec (fix check r1 of gap 4b: a checkout and a worktree with one dir name selected the checkout; the master thread,
// 8 Nov 10:49: "Drop rule 4, the folder-name match, entirely"): a dir name that is no key selects nothing, even where
// it names a checkout or a worktree; a writing sync by it is refused and writes nothing anywhere.
#[test]
fn a_dir_name_that_is_no_key_selects_nothing() {
    let env = Env::new(&[("a", "x/same"), ("b", "y/same")]);
    let o = env.sync(&["--repo", "same"]);
    let out = text(&o);
    assert!(!o.status.success() && out.contains("no repo or workspace `same`"), "{out}");
    assert!(!names_file(&o, &env.path("x/same")) && !names_file(&o, &env.path("y/same")), "{out}");

    let env = Env::new(&[]);
    let main = env.repo("r", "same", &["same"]);
    env.commit_private("global/rules/r.md", &rule("r", "repo:r", "The repo's rule."));
    let worktree = env.path("wts/same");
    // From a dir where `same` is no path (a path selects, A8 and F1).
    let o = env.lords_in(&env.path("home"), &["sync"], &["--repo", "same", "--apply"]);
    let out = text(&o);
    assert!(!o.status.success() && out.contains("no repo or workspace `same`"), "{out}");
    for d in [&main, &worktree] {
        assert!(!d.join("AGENTS.override.md").exists(), "nothing written: {out}");
    }
}

// Spec (design note S2, attack A3): of two nested places, a path resolves to the deepest that holds it, and each key
// still resolves in any case.
#[test]
fn a_path_resolves_to_the_deepest_place_that_holds_it() {
    let env = Env::new(&[("a", "x/same"), ("outer", "n"), ("inner", "n/in")]);
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
