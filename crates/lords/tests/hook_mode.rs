//! D28 r2 (review r1, R1-01 and R1-02; the master thread, 8 Nov 16:09: "stop baking the mode at install time. The
//! privacy mode belongs to the checkout that runs the hook, so the hook should decide it at execution"): a privacy hook
//! checks in the mode of the checkout it runs in, whichever checkout installed it. Every copy of that checkout's
//! manifests (`stack.toml`, `stack/stack.toml`; in HEAD, staged, on disk, tracked or not) saying `private` is secrets
//! only; all saying `public` is the whole policy; anything else is the whole policy with a line saying why. Design note
//! 2026-11-08-lords-d28-r2, attack list 1-9.

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
    /// An empty home and global git config, so no real home dir, hook or setting is read or touched.
    fn new() -> Env {
        let dir = TempDir::new().unwrap();
        let email = ["test", "example.invalid"].join("@");
        fs::write(
            dir.path().join("gitconfig"),
            format!("[user]\n\tname = Test\n\temail = {email}\n"),
        )
        .unwrap();
        fs::create_dir_all(dir.path().join("home")).unwrap();
        Env { dir }
    }

    fn cmd(&self, program: &str, cwd: &Path) -> Command {
        let mut c = Command::new(program);
        c.current_dir(cwd)
            .env("GIT_CONFIG_GLOBAL", self.dir.path().join("gitconfig"))
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("HOME", self.dir.path().join("home"))
            .env("USERPROFILE", self.dir.path().join("home"))
            .env("LORDS_CODEX", env!("CARGO_BIN_EXE_lords-codex-stub"));
        c
    }

    fn lords(&self, cwd: &Path, args: &[&str]) -> Output {
        self.cmd(env!("CARGO_BIN_EXE_lords"), cwd).args(args).output().unwrap()
    }

    fn git(&self, cwd: &Path, args: &[&str]) -> Output {
        self.cmd("git", cwd).args(args).output().unwrap()
    }

    fn ok_git(&self, cwd: &Path, args: &[&str]) {
        let o = self.git(cwd, args);
        assert!(o.status.success(), "git {args:?}: {}", text(&o));
    }

    fn path(&self, rel: &str) -> PathBuf {
        self.dir.path().join(rel)
    }

    /// A repo at `rel` with these files, committed.
    fn repo(&self, rel: &str, files: &[(&str, &str)]) -> PathBuf {
        let r = self.path(rel);
        fs::create_dir_all(&r).unwrap();
        self.ok_git(&r, &["init", "-q"]);
        for (f, body) in files {
            write(&r.join(f), body);
        }
        self.ok_git(&r, &["add", "-A"]);
        self.ok_git(&r, &["commit", "-q", "-m", "start"]);
        r
    }

    /// config.txt naming a public stack outside any repo, so a private stack repo's `check --stack` loads.
    fn config(&self) {
        let p = self.path("pubstack");
        write(&p.join("stack.toml"), PUBLIC);
        let cfg = self.path("home/.lords-command");
        write(&cfg.join("config.txt"), &format!("public = {}\n", p.display()));
    }

    fn head(&self, repo: &Path) -> String {
        String::from_utf8_lossy(&self.git(repo, &["rev-parse", "HEAD"]).stdout).trim().to_string()
    }

    /// The installed hook `hook` run in `cwd` as git runs it, with `args`.
    fn hook(&self, cwd: &Path, hook: &str, args: &[&str]) -> Output {
        self.git(cwd, &[&["hook", "run", hook, "--"][..], args].concat())
    }
}

const PUBLIC: &str = "name = \"pub\"\nvisibility = \"public\"\n";
const PRIVATE: &str = "name = \"priv\"\nvisibility = \"private\"\n";

fn write(path: &Path, body: &str) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, body).unwrap();
}

fn text(o: &Output) -> String {
    format!("{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr))
}

/// A private detail that is no secret: a user path, which the whole policy refuses and secrets-only mode lets through
/// (tests/privacy.rs). Built from parts, so this file holds none.
fn detail() -> String {
    format!("logs in C:\\{}\\alice\\logs", "Users")
}

/// R1-01's witness: the public store's shape, `pub` tracking `stack/stack.toml` (public), and its linked worktree `wt`
/// on branch `priv`, tracking a root `stack.toml` (private) instead. Both run the hooks of one git common dir.
fn shared_hooks(env: &Env) -> (PathBuf, PathBuf) {
    env.config();
    let main = env.repo("pub", &[("stack/stack.toml", PUBLIC)]);
    let wt = env.path("wt");
    env.ok_git(&main, &["worktree", "add", "-q", "-b", "priv", &wt.to_string_lossy()]);
    env.ok_git(&wt, &["rm", "-q", "-r", "stack"]);
    write(&wt.join("stack.toml"), PRIVATE);
    env.ok_git(&wt, &["add", "-A"]);
    env.ok_git(&wt, &["commit", "-q", "-m", "private"]);
    (main, wt)
}

// Spec (attack 1; R1-01: "install from a private worktree; the public checkout must refuse a non-secret private
// commit message"): hooks installed from the private worktree, with the flag or without, leave the public checkout
// its whole check: a commit whose message holds a user path is refused, and nothing is committed.
#[test]
fn a_private_worktrees_install_leaves_the_public_checkout_its_whole_check() {
    for flag in [&[][..], &["--secrets-only"][..]] {
        let env = Env::new();
        let (main, wt) = shared_hooks(&env);
        let o = env.lords(&wt, &[&["hooks", "install"][..], flag].concat());
        assert!(o.status.success(), "{flag:?}: {}", text(&o));
        write(&main.join("notes.md"), "clean notes\n");
        env.ok_git(&main, &["add", "notes.md"]);
        let before = env.head(&main);
        let o = env.git(&main, &["commit", "-q", "-m", &detail()]);
        assert!(!o.status.success() && text(&o).contains("path"), "{flag:?}: {}", text(&o));
        assert_eq!(env.head(&main), before, "{flag:?}: nothing committed");
    }
}

// Spec (attack 2; R1-01: "the reverse: a private checkout is not given public restrictions"): hooks installed from
// the public checkout let the private worktree commit a user path, in a file and in the message.
#[test]
fn the_public_checkouts_install_leaves_the_private_worktree_secrets_only() {
    let env = Env::new();
    let (main, wt) = shared_hooks(&env);
    let o = env.lords(&main, &["hooks", "install"]);
    assert!(o.status.success(), "{}", text(&o));
    write(&wt.join("notes.md"), &format!("{}\n", detail()));
    env.ok_git(&wt, &["add", "notes.md"]);
    let o = env.git(&wt, &["commit", "-q", "-m", &detail()]);
    assert!(o.status.success(), "{}", text(&o));
}

// Spec (design note C5; the orchestrator, 27 Oct: a public stack's push "checks the whole tree too, as `lords check
// --public` checks it", a private stack's its history's secrets): from shared hooks, each checkout pushes by its own
// stack: the private worktree's push passes without the public tree check, the public checkout's runs it.
#[test]
fn each_checkout_pushes_by_its_own_stack() {
    let env = Env::new();
    let (main, wt) = shared_hooks(&env);
    assert!(env.lords(&main, &["hooks", "install"]).status.success());
    let o = env.hook(&wt, "pre-push", &["origin", "unused"]);
    assert!(o.status.success() && !text(&o).contains("public: "), "{}", text(&o));
    let o = env.hook(&main, "pre-push", &["origin", "unused"]);
    assert!(o.status.success() && text(&o).contains("public: "), "{}", text(&o));
}

// Spec (attack 3; R1-02: "an untracked public manifest plus the flag"): a present but untracked public manifest
// makes a stack repo: `--secrets-only` doesn't give it secrets-only hooks, a commit of a user path is refused, and the
// stack's pre-push is installed.
#[test]
fn an_untracked_public_manifest_gets_the_stack_hooks_and_the_whole_check() {
    let env = Env::new();
    let repo = env.repo("repo", &[("README.md", "a repo\n")]);
    write(&repo.join("stack/stack.toml"), PUBLIC);
    let o = env.lords(&repo, &["hooks", "install", "--secrets-only"]);
    assert!(o.status.success(), "{}", text(&o));
    assert!(repo.join(".git/hooks/pre-push").is_file(), "the stack's pre-push");
    write(&repo.join("notes.md"), &format!("{}\n", detail()));
    env.ok_git(&repo, &["add", "notes.md"]);
    let o = env.git(&repo, &["commit", "-q", "-m", "notes"]);
    assert!(!o.status.success() && text(&o).contains("path"), "{}", text(&o));
}

// Spec (attack 4; R1-02: "an untracked manifest with no `visibility`"; the master: missing or unreadable "fails
// closed: full public mode, with a message naming the cause"): install installs, and the hook checks the whole policy,
// naming the missing visibility.
#[test]
fn an_untracked_manifest_without_visibility_checks_the_whole_policy_and_says_why() {
    let env = Env::new();
    let repo = env.repo("repo", &[("README.md", "a repo\n")]);
    write(&repo.join("stack/stack.toml"), "name = \"x\"\n");
    let o = env.lords(&repo, &["hooks", "install", "--secrets-only"]);
    assert!(o.status.success(), "{}", text(&o));
    let msg = env.path("msg");
    write(&msg, &detail());
    let o = env.hook(&repo, "commit-msg", &[&msg.to_string_lossy()]);
    assert!(!o.status.success() && text(&o).contains("visibility"), "{}", text(&o));
}

// Spec (attack 5; R1-02: "An untracked private manifest with no flag instead receives full mode"): a present but
// untracked private manifest makes a private stack repo, whose hooks check secrets only without the flag.
#[test]
fn an_untracked_private_manifest_checks_secrets_only_without_the_flag() {
    let env = Env::new();
    env.config();
    let repo = env.repo("repo", &[("README.md", "a repo\n")]);
    write(&repo.join("stack.toml"), PRIVATE);
    let o = env.lords(&repo, &["hooks", "install"]);
    assert!(o.status.success(), "{}", text(&o));
    write(&repo.join("notes.md"), &format!("{}\n", detail()));
    env.ok_git(&repo, &["add", "notes.md"]);
    let o = env.git(&repo, &["commit", "-q", "-m", &detail()]);
    assert!(o.status.success(), "{}", text(&o));
}

// Spec (attack 6; R1-01: two manifests, "Resolve or refuse disagreement rather than silently choosing the private
// manifest"; the master: "ambiguous (two manifests that disagree)" fails closed): a root `stack.toml` saying private
// and a `stack/stack.toml` saying public check the whole policy, naming both.
#[test]
fn two_manifests_that_disagree_check_the_whole_policy_naming_both() {
    let env = Env::new();
    let repo = env.repo("repo", &[("stack.toml", PRIVATE), ("stack/stack.toml", PUBLIC)]);
    let o = env.lords(&repo, &["hooks", "install"]);
    assert!(o.status.success(), "{}", text(&o));
    let msg = env.path("msg");
    write(&msg, &detail());
    let o = env.hook(&repo, "commit-msg", &[&msg.to_string_lossy()]);
    let out = text(&o);
    assert!(!o.status.success() && out.contains("path"), "{out}");
    assert!(out.contains("stack/stack.toml") && out.contains("private"), "{out}");
}

// Spec (attack 7; design note C1: every copy of the manifest, so the commit's own content decides with the disk): a
// public checkout whose manifest on disk is edited to private but not staged checks the whole policy, naming the
// disagreement. (Not a fails-without test against r1, which read no mode at run time.)
#[test]
fn an_unstaged_private_edit_of_a_public_manifest_checks_the_whole_policy() {
    let env = Env::new();
    let repo = env.repo("repo", &[("stack/stack.toml", PUBLIC)]);
    write(&repo.join("stack/stack.toml"), "name = \"pub\"\nvisibility = \"private\"\n");
    write(&repo.join("notes.md"), &format!("{}\n", detail()));
    env.ok_git(&repo, &["add", "notes.md"]);
    let o = env.lords(&repo, &["privacy", "--staged", "--by-stack"]);
    let out = text(&o);
    assert!(!o.status.success() && out.contains("path"), "{out}");
    assert!(out.contains("public") && out.contains("private"), "{out}");
}

// Spec (attack 8; design note C2: no caller gives a public checkout secrets-only mode): `privacy --staged
// --secrets-only` in a public stack checkout checks the whole policy and says so.
#[test]
fn secrets_only_in_a_public_checkout_checks_the_whole_policy_and_says_so() {
    let env = Env::new();
    let repo = env.repo("repo", &[("stack/stack.toml", PUBLIC)]);
    write(&repo.join("notes.md"), &format!("{}\n", detail()));
    env.ok_git(&repo, &["add", "notes.md"]);
    let o = env.lords(&repo, &["privacy", "--staged", "--secrets-only"]);
    let out = text(&o);
    assert!(!o.status.success() && out.contains("path") && out.contains("public"), "{out}");
}

// Spec (attack 9; D28: the mode comes from a stack where there is one): a repo with no stack keeps what the flag says:
// with `--secrets-only` a user path commits, without it the commit is refused.
#[test]
fn a_repo_without_a_stack_keeps_its_flag() {
    let env = Env::new();
    for (rel, flag, commits) in [("a", &["--secrets-only"][..], true), ("b", &[][..], false)] {
        let repo = env.repo(rel, &[("README.md", "a repo\n")]);
        let o = env.lords(&repo, &[&["hooks", "install"][..], flag].concat());
        assert!(o.status.success(), "{}", text(&o));
        write(&repo.join("notes.md"), &format!("{}\n", detail()));
        env.ok_git(&repo, &["add", "notes.md"]);
        let o = env.git(&repo, &["commit", "-q", "-m", "notes"]);
        assert_eq!(o.status.success(), commits, "{flag:?}: {}", text(&o));
    }
}
