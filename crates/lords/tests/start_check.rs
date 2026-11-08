//! The stores' commit check of the starts one hook gives, through the hooks `lords hooks install` writes and real
//! `git commit`s (gap 11, design note `2026-11-08-lords-gap11-headroom.md` §0b.3–§0e). The check's own cases are
//! `lords-core`'s tests/admission.rs; these are its wiring: every way a commit reaches the hook, its refusal and
//! message, and git's environment as a hook gets it.

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
    /// Two stores with hooks installed and config.txt naming them: a public repo whose stack is `stack/`, and a
    /// private repo that is its stack, declaring repo `demo` (no checkout: a start needs none) and roles coder and
    /// reviewer.
    fn new() -> Env {
        let env = Env { dir: TempDir::new().unwrap() };
        let email = ["test", "example.invalid"].join("@");
        let gitconfig =
            format!("[user]\n\tname = Test\n\temail = {email}\n[init]\n\tdefaultBranch = main\n");
        env.write("gitconfig", &gitconfig);
        let roles = "roles = [\"coder\", \"reviewer\"]\n";
        env.write(
            "pub/stack/stack.toml",
            &format!("name = \"pub\"\nvisibility = \"public\"\n{roles}"),
        );
        env.write(
            "pub/stack/global/rules/all.md",
            &rule("rule.all", "public", "", "For everyone.\n"),
        );
        env.write(
            "priv/stack.toml",
            &format!("name = \"priv\"\nvisibility = \"private\"\n{roles}[repos.demo]\n"),
        );
        env.write(
            "home/.lords-command/config.txt",
            &format!(
                "public = {}\nprivate = {}\n",
                env.path("pub/stack").display(),
                env.path("priv").display()
            ),
        );
        for store in ["pub", "priv"] {
            let dir = env.path(store);
            assert!(env.git(&dir, &["init", "-q"]).status.success());
            env.git(&dir, &["add", "-A"]);
            assert!(env.git(&dir, &["commit", "-q", "-m", "store"]).status.success());
            let o = env.lords(&dir, &["hooks", "install"]);
            assert!(o.status.success(), "{}", text(&o));
            assert!(fs::read_to_string(dir.join(".git/hooks/pre-commit"))
                .unwrap()
                .contains("check --stack"));
        }
        env
    }

    fn path(&self, rel: &str) -> PathBuf {
        self.dir.path().join(rel)
    }

    fn write(&self, rel: &str, text: &str) {
        let p = self.path(rel);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, text).unwrap();
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

    fn lords(&self, cwd: &Path, args: &[&str]) -> Output {
        self.cmd(env!("CARGO_BIN_EXE_lords"), cwd).args(args).output().unwrap()
    }

    fn git(&self, cwd: &Path, args: &[&str]) -> Output {
        self.cmd("git", cwd).args(args).output().unwrap()
    }

    fn head(&self, store: &str) -> String {
        out(&self.git(&self.path(store), &["rev-parse", "HEAD"]))
    }

    /// The private rule `rule.big`, for coder only, whose body is `n` characters: past the limit for n ≥ 9,500.
    fn big(&self, n: usize) {
        self.write(
            "priv/global/rules/big.md",
            &rule("rule.big", "private", "roles: [coder]\n", &"b".repeat(n)),
        );
    }
}

fn rule(id: &str, vis: &str, extra: &str, body: &str) -> String {
    format!(
        "---\nid: {id}\nkind: rule\ntitle: {id}\naudience: [all]\nscope: global\nstatus: active\nvisibility: {vis}\norigin:\n  type: own\n{extra}---\n{body}\n"
    )
}

fn text(o: &Output) -> String {
    format!("{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr))
}

fn out(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).trim().to_string()
}

/// A refusal of the commit `o` by the start check: it names the place, the start, and the limit.
fn refused_by_the_check(o: &Output) -> bool {
    let t = text(o);
    !o.status.success()
        && t.contains("repo demo: start-coder.md would be")
        && t.contains("past the 9500")
}

// Spec (§0b.3 step 6, I1; the hook is the stores' pre-commit, `check --stack`): a private commit that takes coder's
// start past the limit is refused, naming the place and the start, and leaves HEAD as it was; a fitting one is
// admitted and says how long each start is at most.
#[test]
fn a_commit_that_takes_a_start_past_the_limit_is_refused() {
    let env = Env::new();
    let private = env.path("priv");
    let before = env.head("priv");
    env.big(9_600);
    env.git(&private, &["add", "-A"]);
    let o = env.git(&private, &["commit", "-q", "-m", "too long"]);
    assert!(refused_by_the_check(&o), "{}", text(&o));
    assert_eq!(env.head("priv"), before);
    env.big(100);
    env.git(&private, &["add", "-A"]);
    let o = env.git(&private, &["commit", "-m", "fits"]);
    assert!(o.status.success(), "{}", text(&o));
    assert!(
        text(&o).contains("starts, the longest of each: ") && text(&o).contains("start-coder.md "),
        "{}",
        text(&o)
    );
}

// Spec (r1 F3, r2: "Cover … normal commit, `commit -a`, and `commit <paths>`"; git hands the hook the index the
// commit will hold in GIT_INDEX_FILE): `commit -a` and `commit <paths>` of a too-long rule, staged nowhere else, are
// refused alike.
#[test]
fn commit_a_and_commit_paths_are_checked_as_they_commit() {
    let env = Env::new();
    let private = env.path("priv");
    env.big(100);
    env.git(&private, &["add", "-A"]);
    assert!(env.git(&private, &["commit", "-q", "-m", "fits"]).status.success());
    let before = env.head("priv");
    env.big(9_600);
    let o = env.git(&private, &["commit", "-q", "-a", "-m", "too long, by -a"]);
    assert!(refused_by_the_check(&o), "{}", text(&o));
    let o = env.git(
        &private,
        &["commit", "-q", "-m", "too long, by its path", "--", "global/rules/big.md"],
    );
    assert!(refused_by_the_check(&o), "{}", text(&o));
    assert_eq!(env.head("priv"), before);
}

// Spec (r2: "add amend, automatic merge, resolved merge … cases"; the stack repos' pre-merge-commit runs the same
// check, hooks.rs): an amend that makes the start too long is refused, as is an automatic merge of a branch that does.
#[test]
fn an_amend_and_a_merge_are_checked() {
    let env = Env::new();
    let private = env.path("priv");
    env.big(100);
    env.git(&private, &["add", "-A"]);
    assert!(env.git(&private, &["commit", "-q", "-m", "fits"]).status.success());
    let before = env.head("priv");
    env.big(9_600);
    env.git(&private, &["add", "-A"]);
    let o = env.git(&private, &["commit", "-q", "--amend", "-m", "amended too long"]);
    assert!(refused_by_the_check(&o), "{}", text(&o));
    assert_eq!(env.head("priv"), before);

    env.git(&private, &["checkout", "-q", "-b", "long"]);
    let o =
        env.git(&private, &["commit", "-q", "--no-verify", "-m", "a branch made without hooks"]);
    assert!(o.status.success(), "{}", text(&o));
    env.git(&private, &["checkout", "-q", "main"]);
    env.write("priv/global/rules/other.md", &rule("rule.other", "private", "", "On main.\n"));
    env.git(&private, &["add", "-A"]);
    assert!(env.git(&private, &["commit", "-q", "-m", "main moves on"]).status.success());
    let main = env.head("priv");
    let o = env.git(&private, &["merge", "--no-ff", "--no-edit", "long"]);
    assert!(refused_by_the_check(&o), "{}", text(&o));
    assert_eq!(env.head("priv"), main);
}

// Spec (§0b.1, the no-op skip): a public commit outside `stack/` goes through while the private store has work not
// committed; a public stack commit then doesn't, naming the private store's file (§0b.3 step 3).
#[test]
fn only_a_stack_commit_waits_for_the_other_store() {
    let env = Env::new();
    let public = env.path("pub");
    env.write("priv/global/rules/draft.md", &rule("rule.draft", "private", "", "Not committed.\n"));
    env.write("pub/README.md", "code, not stack\n");
    env.git(&public, &["add", "-A"]);
    let o = env.git(&public, &["commit", "-q", "-m", "code"]);
    assert!(o.status.success(), "{}", text(&o));
    env.write("pub/stack/global/rules/q.md", &rule("rule.q", "public", "", "A public rule.\n"));
    env.git(&public, &["add", "-A"]);
    let o = env.git(&public, &["commit", "-q", "-m", "a public rule"]);
    assert!(!o.status.success() && text(&o).contains("draft.md"), "{}", text(&o));
}

// Spec (r3, F3 source-pair boundaries, "Environment isolation": the other store's reads clear git's local variables,
// `process.rs`): a public stack commit made with GIT_DIR and GIT_INDEX_FILE exported for the public repo still reads
// the private store as itself: its untracked file is seen and named.
#[test]
fn the_other_store_is_read_as_itself_whatever_git_exports() {
    let env = Env::new();
    let public = env.path("pub");
    env.write("priv/global/rules/draft.md", &rule("rule.draft", "private", "", "Not committed.\n"));
    env.write("pub/stack/global/rules/q.md", &rule("rule.q", "public", "", "A public rule.\n"));
    env.git(&public, &["add", "-A"]);
    let o = env
        .cmd("git", &public)
        .env("GIT_DIR", public.join(".git"))
        .env("GIT_INDEX_FILE", public.join(".git/index"))
        .env("GIT_WORK_TREE", &public)
        .args(["commit", "-q", "-m", "a public rule"])
        .output()
        .unwrap();
    assert!(!o.status.success() && text(&o).contains("draft.md"), "{}", text(&o));
}

// Spec (the master, 20:48: "pass D28's held `here` identity and checkout to admission; `Unknown` refuses and `None`
// skips; never call `checkout_stack` twice"; review gap11a r1, "Merge and interface risks"; design note d28-alias,
// attack item 11: "H3 refuses on `Unknown`"): the start check knows a store's checkout by D28's discovery, not by which
// manifest is on disk. A `check --stack` without `--by-stack` in either store, with a stray `docs/stack.toml` staged
// (a copy discovery can't count), is refused, naming it. Every row runs before the refusals are asserted.
#[test]
fn the_start_check_refuses_a_store_whose_stack_cant_be_told() {
    let env = Env::new();
    let outputs: Vec<_> = ["priv", "pub"]
        .iter()
        .map(|store| {
            let dir = env.path(store);
            env.write(
                &format!("{store}/docs/stack.toml"),
                "name = \"x\"\nvisibility = \"private\"\n",
            );
            env.git(&dir, &["add", "-A"]);
            (store, env.lords(&dir, &["check", "--stack"]))
        })
        .collect();
    let let_through: Vec<_> = outputs
        .iter()
        .filter(|(_, o)| o.status.success())
        .map(|(store, o)| format!("{store}: {}", text(o)))
        .collect();
    assert!(let_through.is_empty(), "refused in both stores, but let through: {let_through:#?}");
    for (store, o) in &outputs {
        assert!(text(o).contains("docs/stack.toml"), "{store}: names it: {}", text(o));
    }
}

// Spec (the same: "`None` skips"): outside any store, a `check --stack` with both stores configured checks no starts,
// and passes.
#[test]
fn the_start_check_skips_a_checkout_without_a_stack() {
    let env = Env::new();
    let other = env.path("other");
    env.write("other/README.md", "a repo\n");
    assert!(env.git(&other, &["init", "-q"]).status.success());
    env.git(&other, &["add", "-A"]);
    assert!(env.git(&other, &["commit", "-q", "-m", "start"]).status.success());
    let o = env.lords(&other, &["check", "--stack"]);
    assert!(o.status.success(), "{}", text(&o));
    assert!(!text(&o).contains("starts, the longest"), "no starts checked: {}", text(&o));
}
