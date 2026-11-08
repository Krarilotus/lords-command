//! D28 r2 (review r1, R1-01 and R1-02; the master thread, 8 Nov 16:09: "stop baking the mode at install time. The
//! privacy mode belongs to the checkout that runs the hook, so the hook should decide it at execution"): a privacy hook
//! checks in the mode of the checkout it runs in, whichever checkout installed it. Every copy of that checkout's
//! manifests (`stack.toml`, `stack/stack.toml`; in HEAD, staged, on disk, tracked or not) saying `private` is secrets
//! only; all saying `public` is the whole policy; anything else is the whole policy with a line saying why. Design note
//! 2026-11-08-lords-d28-r2, attack list 1-9. Fix check r3 (R2-01, filesystem aliases): design note
//! 2026-11-08-lords-d28-alias, attack list 1-12.

// Tests are outside the filesystem boundary (fsguard.rs): their fixtures write their own temp dirs.
#![allow(clippy::disallowed_methods, clippy::disallowed_types)]

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
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

    /// `lords` with these variables set besides the fixture's own.
    fn lords_with(&self, cwd: &Path, args: &[&str], vars: &[(&str, &Path)]) -> Output {
        let mut c = self.cmd(env!("CARGO_BIN_EXE_lords"), cwd);
        for (k, v) in vars {
            c.env(k, v);
        }
        c.args(args).output().unwrap()
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

// Spec (fix check r2, D28-R2-01: "discovery must include every index/HEAD spelling which can designate a recognized
// manifest on the running filesystem, or fail closed on such aliases"): a public manifest committed or staged under
// another spelling (`STACK.toml`, `STACK/stack.toml`), whose disk copy says private, leaves a commit message holding a
// user path refused, with the old `--secrets-only` text and with `--by-stack`: in HEAD and the index, the index alone
// (no commit yet), and HEAD alone (no longer staged).
#[test]
fn another_spelling_of_a_manifest_is_never_left_out() {
    for spelling in ["STACK.toml", "STACK/stack.toml"] {
        for copies in ["head and index", "index", "head"] {
            let env = Env::new();
            let repo = env.path("repo");
            fs::create_dir_all(&repo).unwrap();
            env.ok_git(&repo, &["init", "-q"]);
            write(&repo.join(spelling), PUBLIC);
            env.ok_git(&repo, &["add", "-A"]);
            if copies != "index" {
                env.ok_git(&repo, &["commit", "-q", "-m", "start"]);
            }
            if copies == "head" {
                env.ok_git(&repo, &["rm", "-q", "--cached", spelling]);
            }
            write(&repo.join(spelling), "name = \"pub\"\nvisibility = \"private\"\n");
            let msg = env.path("msg");
            write(&msg, &detail());
            for flag in ["--secrets-only", "--by-stack"] {
                let args = ["privacy", "--message", &msg.to_string_lossy(), flag];
                let o = env.lords(&repo, &args);
                let case = format!("{spelling}, {copies}, {flag}: {}", text(&o));
                assert!(!o.status.success(), "{case}");
                assert!(text(&o).contains("path"), "{case}");
            }
        }
    }
}

// Spec (fix check r2, D28-R2-02: "preserve the checkout-discovery error as an unknown/error with its real cause"): where
// git can't tell the checkout (a global config it can't read), `privacy --message --secrets-only` in a public stack
// checkout checks the whole policy and names the cause; outside any repo, the flag holds (the control).
#[test]
fn a_failed_checkout_lookup_checks_the_whole_policy_and_says_why() {
    let env = Env::new();
    let repo = env.repo("repo", &[("stack/stack.toml", PUBLIC)]);
    let broken = env.path("broken-gitconfig");
    write(&broken, "[user\n\tname = Test\n");
    let msg = env.path("msg");
    write(&msg, &detail());
    let args = ["privacy", "--message", &msg.to_string_lossy(), "--secrets-only"];
    let o = env.lords_with(&repo, &args, &[("GIT_CONFIG_GLOBAL", &broken)]);
    assert!(!o.status.success(), "{}", text(&o));
    assert!(text(&o).contains("path") && text(&o).contains("config"), "{}", text(&o));

    let plain = env.path("plain");
    fs::create_dir_all(&plain).unwrap();
    let ceiling = [("GIT_CEILING_DIRECTORIES", env.dir.path())];
    let o = env.lords_with(&plain, &args, &ceiling);
    assert!(o.status.success(), "no repo, the flag holds: {}", text(&o));
    let o = env.lords_with(&plain, &args[..3], &ceiling);
    assert!(!o.status.success(), "no flag, the whole policy: {}", text(&o));
}

// Spec (fix check r2, D28-R2-F01: "install the runtime stack pre-push whenever any worktree that shares the hooks has a
// stack"): hooks installed from a manifest-free sibling still give the public checkout its pre-push, which refuses a
// history holding a user path (committed before any hook); the manifest-free sibling commits as before.
#[test]
fn a_manifest_free_installer_still_gives_the_public_sibling_its_push_check() {
    let env = Env::new();
    let main = env.repo("pub", &[("stack/stack.toml", PUBLIC)]);
    write(&main.join("notes.md"), &format!("{}\n", detail()));
    env.ok_git(&main, &["add", "notes.md"]);
    env.ok_git(&main, &["commit", "-q", "-m", "notes"]);
    let plain = env.path("plain");
    env.ok_git(&main, &["worktree", "add", "-q", "-b", "plain", &plain.to_string_lossy()]);
    env.ok_git(&plain, &["rm", "-q", "-r", "stack", "notes.md"]);
    env.ok_git(&plain, &["commit", "-q", "-m", "no stack"]);
    let o = env.lords(&plain, &["hooks", "install"]);
    assert!(o.status.success(), "{}", text(&o));
    assert!(main.join(".git/hooks/pre-push").is_file(), "the stack's pre-push");
    let o = env.hook(&main, "pre-push", &["origin", "unused"]);
    assert!(!o.status.success(), "{}", text(&o));
    assert!(text(&o).contains("notes.md"), "{}", text(&o));
    write(&plain.join("readme.md"), "a clean line\n");
    env.ok_git(&plain, &["add", "readme.md"]);
    let o = env.git(&plain, &["commit", "-q", "-m", "clean"]);
    assert!(o.status.success(), "{}", text(&o));
}

// Spec (fix check r2, D28-R2-F01: "Say in the note what happens when a stack worktree is added later. One option: the
// commit hooks warn"): a repo whose hooks were installed without a stack, where a checkout has one later, commits with a
// line saying its pushes go unchecked until `lords hooks install` runs again.
#[test]
fn a_stack_that_came_later_is_named_at_the_next_commit() {
    let env = Env::new();
    let repo = env.repo("repo", &[("README.md", "a repo\n")]);
    assert!(env.lords(&repo, &["hooks", "install"]).status.success());
    write(&repo.join("stack/stack.toml"), PUBLIC);
    write(&repo.join("notes.md"), "a clean line\n");
    env.ok_git(&repo, &["add", "notes.md"]);
    let o = env.git(&repo, &["commit", "-q", "-m", "clean"]);
    assert!(o.status.success(), "{}", text(&o));
    assert!(text(&o).contains("lords hooks install"), "{}", text(&o));
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

// ---------------------------------------------------------------- fix check r3: aliases (design note d28-alias)

/// A fresh repo at `repo`, with nothing committed.
fn bare_repo(env: &Env) -> PathBuf {
    let repo = env.path("repo");
    fs::create_dir_all(&repo).unwrap();
    env.ok_git(&repo, &["init", "-q"]);
    repo
}

/// Stages `body` at the git path `at` with `mode`, as git takes it, without a file on disk and without Windows' path
/// protection (`core.protectNTFS`), which refuses names such as `stack.toml.` at `git add`. A submodule's commit is a
/// made-up id. Checks the index then holds `at` byte for byte, so no row silently tests another name.
fn stage_raw(env: &Env, repo: &Path, mode: &str, at: &str, body: &str) {
    let id = if mode == "160000" {
        "1".repeat(40)
    } else {
        let mut c = env.cmd("git", repo);
        c.args(["hash-object", "-w", "--stdin"]).stdin(Stdio::piped()).stdout(Stdio::piped());
        let mut child = c.spawn().unwrap();
        child.stdin.take().unwrap().write_all(body.as_bytes()).unwrap();
        let o = child.wait_with_output().unwrap();
        assert!(o.status.success(), "setup: hash-object: {}", text(&o));
        String::from_utf8_lossy(&o.stdout).trim().to_string()
    };
    let mut c = env.cmd("git", repo);
    c.args(["-c", "core.protectNTFS=false", "update-index", "--add", "--index-info"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = c.spawn().unwrap();
    child.stdin.take().unwrap().write_all(format!("{mode} {id}\t{at}\n").as_bytes()).unwrap();
    let o = child.wait_with_output().unwrap();
    assert!(o.status.success(), "setup: stage {at:?}: {}", text(&o));
    let listed = env.git(repo, &["ls-files", "-s", "-z"]);
    let want = format!("{mode} {id} 0\t{at}");
    assert!(
        String::from_utf8_lossy(&listed.stdout).split('\0').any(|r| r == want),
        "setup: the index holds {at:?} as written: {}",
        text(&listed)
    );
}

/// Commits the index as HEAD without a hook and without path protection, then empties the index, so HEAD alone
/// holds what was staged.
fn commit_raw_then_unstage(env: &Env, repo: &Path) {
    let raw = |args: &[&str]| {
        let o = env.git(repo, &[&["-c", "core.protectNTFS=false"][..], args].concat());
        assert!(o.status.success(), "setup: git {args:?}: {}", text(&o));
        String::from_utf8_lossy(&o.stdout).trim().to_string()
    };
    let tree = raw(&["write-tree"]);
    let commit = raw(&["commit-tree", &tree, "-m", "start"]);
    raw(&["update-ref", "HEAD", &commit]);
    raw(&["read-tree", "--empty"]);
}

/// A commit message holding a user path is refused in `repo` under both `--by-stack` and `--secrets-only`, and the
/// output names `names` (why the whole policy runs).
fn refused_by_both(env: &Env, repo: &Path, case: &str, names: &str) {
    let msg = env.path("msg");
    write(&msg, &detail());
    for flag in ["--by-stack", "--secrets-only"] {
        let o = env.lords(repo, &["privacy", "--message", &msg.to_string_lossy(), flag]);
        let out = text(&o);
        assert!(!o.status.success() && out.contains("path"), "{case}, {flag}: refused: {out}");
        assert!(out.contains(names), "{case}, {flag}: names {names:?}: {out}");
    }
}

/// The same message passes in `repo` under both flags: secrets-only mode.
fn accepted_by_both(env: &Env, repo: &Path, case: &str) {
    let msg = env.path("msg");
    write(&msg, &detail());
    for flag in ["--by-stack", "--secrets-only"] {
        let o = env.lords(repo, &["privacy", "--message", &msg.to_string_lossy(), flag]);
        assert!(o.status.success(), "{case}, {flag}: secrets only: {}", text(&o));
    }
}

/// A check this host can't make, said on the real stderr (the test harness captures only `eprintln!`), so the job's
/// log shows it.
fn not_run(why: &str) {
    let _ = writeln!(std::io::stderr().lock(), "hook_mode: NOT RUN: {why}");
}

// Spec (fix check r3, R2-01: "filesystem aliases still hide public manifest copies"; the master: "If any entry anywhere
// in the tree has a basename that, after case folding and trailing-dot/space stripping, equals `stack.toml`, and it
// isn't one of the two canonical paths, the result is Unknown"): a public manifest staged or committed under a name a
// filesystem may take for a manifest, with a private one at the canonical path on disk, leaves a commit message holding
// a user path refused under both flags, naming that name. The rows: trailing dot and space on the dir and the file (r3's
// second witness), an NTFS stream, `\` as a separator, an HFS+-ignorable code point, and r3's short-name witness's git
// half (a long dir holding `stack.toml`), on either OS.
#[test]
fn a_git_name_a_filesystem_may_take_for_a_manifest_is_never_left_out() {
    let rows = [
        ("stack./stack.toml", "stack/stack.toml"),
        ("stack /stack.toml", "stack/stack.toml"),
        ("stack.toml.", "stack.toml"),
        ("stack.toml ", "stack.toml"),
        ("stack.toml::$DATA", "stack.toml"),
        ("stack\\stack.toml", "stack/stack.toml"),
        ("stack\u{200d}.toml", "stack.toml"),
        ("long-stack-directory/stack.toml", "stack/stack.toml"),
    ];
    for (alias, disk) in rows {
        for copies in ["index", "head"] {
            let env = Env::new();
            let repo = bare_repo(&env);
            stage_raw(&env, &repo, "100644", alias, PUBLIC);
            if copies == "head" {
                commit_raw_then_unstage(&env, &repo);
            }
            write(&repo.join(disk), PRIVATE);
            refused_by_both(&env, &repo, &format!("{alias:?} in the {copies}"), alias);
        }
    }
}

// Spec (design note d28-alias, attack 8: "a symlink or a submodule whose path folds to `stack` or `stack.toml` is
// Unknown"): a submodule or a symlink staged at `stack`, or a symlink at `stack.toml` whose target text reads as a
// private manifest, with a private manifest on disk, checks the whole policy under both flags.
#[test]
fn a_symlink_or_submodule_where_a_manifest_goes_checks_the_whole_policy() {
    let rows = [
        ("160000", "stack", "elsewhere", "stack/stack.toml", "submodule"),
        ("120000", "stack", "elsewhere", "stack/stack.toml", "symlink"),
        ("120000", "stack.toml", PRIVATE, "stack.toml", "symlink"),
    ];
    for (mode, at, body, disk, names) in rows {
        let env = Env::new();
        let repo = bare_repo(&env);
        stage_raw(&env, &repo, mode, at, body);
        write(&repo.join(disk), PRIVATE);
        refused_by_both(&env, &repo, &format!("{mode} at {at}"), names);
    }
}

// Spec (the master: "a repo with a fixture `stack.toml` deeper in the tree that gets the full policy", deliberately
// over-restrictive and fixed by renaming): a private store that also commits `tests/fixtures/stack.toml` checks the
// whole policy under both flags, naming the fixture; so does a repo without a stack that holds only the fixture, under
// `--secrets-only`.
#[test]
fn a_stack_toml_deeper_in_the_tree_checks_the_whole_policy() {
    let fixture = "tests/fixtures/stack.toml";
    let env = Env::new();
    let repo = env.repo("repo", &[("stack.toml", PRIVATE), (fixture, PRIVATE)]);
    refused_by_both(&env, &repo, "a private store with a fixture", fixture);
    let plain = env.repo("plain", &[("README.md", "a repo\n"), (fixture, PRIVATE)]);
    let msg = env.path("msg");
    write(&msg, &detail());
    let o = env.lords(&plain, &["privacy", "--message", &msg.to_string_lossy(), "--secrets-only"]);
    let out = text(&o);
    assert!(!o.status.success() && out.contains("path"), "no stack, a fixture: refused: {out}");
    assert!(out.contains(fixture), "no stack, a fixture: names it: {out}");
}

// Spec (design note d28-alias, disk: "If a canonical path opens but its exact entry isn't listed, the answer is
// Unknown"): an untracked private manifest under another case (`STACK.toml`, `STACK/stack.toml`) is, where the
// filesystem opens the canonical path to it (Windows, macOS), a copy reached by another name: both flags refuse the
// message, naming the exact-name rule. Where it doesn't (Linux), it is no manifest, and `--secrets-only` holds.
#[test]
fn a_manifest_on_disk_counts_only_under_its_exact_name() {
    for (spelling, canonical) in
        [("STACK.toml", "stack.toml"), ("STACK/stack.toml", "stack/stack.toml")]
    {
        let env = Env::new();
        let repo = bare_repo(&env);
        write(&repo.join(spelling), PRIVATE);
        if fs::symlink_metadata(repo.join(canonical)).is_ok() {
            refused_by_both(&env, &repo, &format!("untracked {spelling}"), "exactly");
        } else {
            let msg = env.path("msg");
            write(&msg, &detail());
            let args = ["privacy", "--message", &msg.to_string_lossy(), "--secrets-only"];
            let o = env.lords(&repo, &args);
            assert!(o.status.success(), "case-sensitive, {spelling} is no manifest: {}", text(&o));
        }
    }
}

/// Gives `dir` the NTFS short name `stack`, or says why this host can't (not NTFS, short names off on the volume, or
/// no right to set them).
fn short_name_stack(dir: &Path) -> bool {
    #[cfg(windows)]
    {
        match Command::new("fsutil").args(["file", "setshortname"]).arg(dir).arg("stack").output() {
            Ok(o) if o.status.success() => return true,
            Ok(o) => not_run(&format!("fsutil can't set a short name here: {}", text(&o).trim())),
            Err(e) => not_run(&format!("fsutil didn't start: {e}")),
        }
    }
    #[cfg(not(windows))]
    {
        let _ = dir;
        not_run("short names are NTFS's");
    }
    false
}

// Spec (fix check r3, R2-01's primary witness: "the indexed `long-stack-directory/stack.toml` is public, while the disk
// path `stack/stack.toml` reaches the same file through its short name, which is private"; the master: "Add a
// behavioural fails-without message check for the short-name witness, with both flags"): with the dir given the short
// name `stack`, both flags refuse the message, staged or untracked. Both name the disk rule: discovery names the first
// copy it can't count, and the disk copies come first. That the git rule names the staged entry too is the git-name
// test's `long-stack-directory/stack.toml` row.
#[test]
fn a_short_name_never_reaches_a_manifest() {
    for staged in [true, false] {
        let env = Env::new();
        let repo = bare_repo(&env);
        let long = repo.join("long-stack-directory");
        write(&long.join("stack.toml"), PUBLIC);
        if staged {
            env.ok_git(&repo, &["add", "-A"]);
        }
        write(&long.join("stack.toml"), PRIVATE);
        if !short_name_stack(&long) {
            return;
        }
        let through = fs::read_to_string(repo.join("stack/stack.toml")).unwrap();
        assert_eq!(through, PRIVATE, "setup: the short name reaches the private copy");
        refused_by_both(&env, &repo, &format!("short name, staged {staged}"), "exactly");
    }
}

// Spec (the master: "Add canonical-private controls"; design note C1: every copy `private` is secrets only): a
// manifest committed at either canonical path, saying private, still checks secrets only under both flags; the exact
// listing rule leaves the real store shapes alone.
#[test]
fn a_canonical_private_manifest_still_checks_secrets_only() {
    for at in ["stack.toml", "stack/stack.toml"] {
        let env = Env::new();
        let repo = env.repo("repo", &[(at, PRIVATE), ("README.md", "a repo\n")]);
        accepted_by_both(&env, &repo, at);
    }
}

// ---------------------------------------------------------------- fix check r4: `check --by-stack` (D28-R4-01)

/// config.txt naming a clean public stack in a repo of its own, `elsewhere`: what a `check --by-stack` must never
/// check in place of the checkout it runs in. Checked clean first, so a pass elsewhere can't come from it failing.
fn config_elsewhere(env: &Env) {
    let other = env.repo("elsewhere", &[("stack/stack.toml", PUBLIC), ("README.md", "clean\n")]);
    let cfg = env.path("home/.lords-command");
    write(&cfg.join("config.txt"), &format!("public = {}\n", other.join("stack").display()));
    for variant in ["--stack", "--public"] {
        let o = env.lords(&other, &["check", variant, "--by-stack"]);
        assert!(
            o.status.success(),
            "setup: the configured stack is clean ({variant}): {}",
            text(&o)
        );
    }
}

// Spec (fix check r4, D28-R4-01: "bind automatic `--by-stack` checks to this checkout. When its stack cannot be safely
// loaded, return its cause instead of falling back to configured stacks"; witness: a public HEAD, a private index and
// neither manifest on disk): with a clean, unrelated stack configured, `check --stack --by-stack` and `check --public
// --by-stack` refuse in a checkout whose copies disagree, naming the disagreement, and in one whose public stack isn't
// on disk, saying so.
#[test]
fn check_by_stack_never_lets_a_configured_stack_stand_in() {
    for (row, cause) in
        [("public HEAD, private index", "disagree"), ("public, not on disk", "on disk")]
    {
        for variant in ["--stack", "--public"] {
            let env = Env::new();
            config_elsewhere(&env);
            let repo = env.repo("repo", &[("stack/stack.toml", PUBLIC), ("README.md", "a repo\n")]);
            let manifest = repo.join("stack/stack.toml");
            if row == "public HEAD, private index" {
                write(&manifest, PRIVATE);
                env.ok_git(&repo, &["add", "stack/stack.toml"]);
            }
            fs::remove_file(&manifest).unwrap();
            let o = env.lords(&repo, &["check", variant, "--by-stack"]);
            let out = text(&o);
            assert!(!o.status.success(), "{row}, {variant}: refused: {out}");
            assert!(out.contains(cause), "{row}, {variant}: names {cause:?}: {out}");
        }
    }
}

// Spec (D28-R4-01: "A known current stack may still be checked conservatively ... Keep proven no-stack/private
// controls"; design note d28-alias, r4 table): in a checkout proven to have no stack, `check --public --by-stack`
// scans its own tree, so a user path in its own file is refused while the configured stack is clean, and its clean twin
// passes; `check --stack --by-stack` skips, as before.
#[test]
fn check_by_stack_without_a_stack_checks_this_checkouts_own_tree() {
    let env = Env::new();
    config_elsewhere(&env);
    let dirty =
        env.repo("dirty", &[("README.md", "a repo\n"), ("notes.md", &format!("{}\n", detail()))]);
    let o = env.lords(&dirty, &["check", "--public", "--by-stack"]);
    assert!(!o.status.success() && text(&o).contains("notes.md"), "its own file: {}", text(&o));
    let clean = env.repo("clean", &[("README.md", "a repo\n")]);
    let o = env.lords(&clean, &["check", "--public", "--by-stack"]);
    assert!(o.status.success(), "a clean tree passes: {}", text(&o));
    let o = env.lords(&dirty, &["check", "--stack", "--by-stack"]);
    assert!(
        o.status.success() && text(&o).contains("no entries"),
        "no stack, no entries: {}",
        text(&o)
    );
}

// Spec (D28-R4-01: "Keep proven ... private controls"; design note C5): in a checkout proven private, with a clean
// unrelated stack configured, `check --public --by-stack` skips, saying its stack is private, and `check --stack
// --by-stack` checks its own manifest and passes.
#[test]
fn check_by_stack_in_a_private_checkout_checks_its_own_stack() {
    let env = Env::new();
    config_elsewhere(&env);
    let repo = env.repo("priv", &[("stack.toml", PRIVATE), ("README.md", "a repo\n")]);
    let o = env.lords(&repo, &["check", "--public", "--by-stack"]);
    assert!(o.status.success() && text(&o).contains("private"), "{}", text(&o));
    let o = env.lords(&repo, &["check", "--stack", "--by-stack"]);
    assert!(o.status.success(), "{}", text(&o));
}

// ---------------------------------------------------------------- fix check r5: allowances (D28-R5-01)

/// Adds to config.txt, after `config_elsewhere`, a private stack outside any repo with these lists, each left out where
/// empty. Its manifest names a fleet config, which holds `fleet`, or is not there for `None`.
fn config_private(env: &Env, allow: &str, deny: &str, fleet: Option<&str>) {
    let private = env.path("privstack");
    let fleet_path = env.path("fleet.toml");
    write(
        &private.join("stack.toml"),
        &format!("{PRIVATE}[privacy]\nfleet_config = '{}'\n", fleet_path.display()),
    );
    for (file, body) in [("allow.txt", allow), ("deny.txt", deny)] {
        if !body.is_empty() {
            write(&private.join("privacy").join(file), body);
        }
    }
    if let Some(body) = fleet {
        write(&fleet_path, body);
    }
    let cfg = env.path("home/.lords-command/config.txt");
    let before = fs::read_to_string(&cfg).unwrap();
    write(&cfg, &format!("{before}private = {}\n", private.display()));
}

/// The three `check --by-stack` branches that load a configured private stack and check this checkout's content: no
/// stack (`--public`), and a public stack (`--public`, `--stack`).
const LOADS_A_PRIVATE_STACK: [(&str, bool, &str); 3] = [
    ("no stack", false, "--public"),
    ("public stack", true, "--public"),
    ("public stack", true, "--stack"),
];

/// A committed checkout at `rel` whose own content says `body`: with `public`, a public stack whose one entry says it
/// (so `--stack` and `--public` both meet it) and, unless `own_allow` is empty, its own allow-list; else no stack, and
/// `notes.md` says it.
fn checkout_saying(env: &Env, rel: &str, public: bool, body: &str, own_allow: &str) -> PathBuf {
    let rule = format!(
        "---\nid: rule.a\nkind: rule\ntitle: A\naudience: [all]\nscope: global\nstatus: active\nvisibility: public\n\
         origin:\n  type: own\n---\nRule A: {body}\n"
    );
    let mut files = vec![("README.md", "a repo\n".to_string())];
    if public {
        files.push(("stack/stack.toml", PUBLIC.to_string()));
        files.push(("stack/global/rules/a.md", rule));
        if !own_allow.is_empty() {
            files.push(("stack/privacy/allow.txt", own_allow.to_string()));
        }
    } else {
        files.push(("notes.md", format!("{body}\n")));
    }
    let files: Vec<(&str, &str)> = files.iter().map(|(f, b)| (*f, b.as_str())).collect();
    env.repo(rel, &files)
}

// Spec (fix check r5, D28-R5-01: "use a manifest-free checkout with `notes.md` containing [a user path, `detail()`];
// configure the existing clean unrelated public stack and a valid private stack with `privacy/allow.txt` containing
// `alice` ... refusal must be asserted before diagnostics"; design note d28-alias, r5 invariant, attack items 2, 7 and
// 9): a configured private stack's allow-list never lets a hit in this checkout's own content through. With no stack and
// with a public one, `--public` refuses the user path in its own file, and with a public one `--stack` refuses it in its
// own entry. Every row runs before the refusals are asserted, so each row's outcome is shown red on its own.
#[test]
fn a_configured_private_allow_list_never_lets_this_checkout_through() {
    let outputs: Vec<_> = LOADS_A_PRIVATE_STACK
        .iter()
        .map(|&(row, public, variant)| {
            let env = Env::new();
            config_elsewhere(&env);
            config_private(&env, "alice\n", "", Some("[host.alpha]\n"));
            let repo = checkout_saying(&env, "repo", public, &detail(), "");
            (format!("{row}, {variant}"), env.lords(&repo, &["check", variant, "--by-stack"]))
        })
        .collect();
    let let_through: Vec<_> = outputs
        .iter()
        .filter(|(_, o)| o.status.success())
        .map(|(row, o)| format!("{row}: {}", text(o)))
        .collect();
    assert!(let_through.is_empty(), "refused in every row, but let through: {let_through:#?}");
    for (row, o) in &outputs {
        assert!(text(o).contains("path: `alice`"), "{row}: names the hit: {}", text(o));
    }
}

// Spec (D28-R5-01: "compose the no-stack public rules with configured private deny/fleet additions only, preserving
// strict loading/read failures"; design note d28-alias, r5 attack items 4 and 5): in each branch that loads a configured
// private stack, its deny term and fleet host in this checkout's content are refused, naming both; and a fleet config
// its manifest names but that isn't there fails the check, naming it.
#[test]
fn check_by_stack_keeps_the_configured_private_deny_and_fleet_terms() {
    for (row, public, variant) in LOADS_A_PRIVATE_STACK {
        let env = Env::new();
        config_elsewhere(&env);
        config_private(&env, "", "zephyr plan\n", Some("[host.alpha]\n"));
        let repo = checkout_saying(&env, "repo", public, "the zephyr plan runs on alpha", "");
        let o = env.lords(&repo, &["check", variant, "--by-stack"]);
        let out = text(&o);
        assert!(!o.status.success(), "{row}, {variant}: refused: {out}");
        assert!(out.contains("deny: `") && out.contains("fleet: `"), "{row}, {variant}: {out}");
    }
    let env = Env::new();
    config_elsewhere(&env);
    config_private(&env, "", "", None);
    let repo = checkout_saying(&env, "repo", false, "clean", "");
    let o = env.lords(&repo, &["check", "--public", "--by-stack"]);
    assert!(!o.status.success(), "a missing fleet config: refused: {}", text(&o));
    assert!(text(&o).contains("fleet.toml"), "names it: {}", text(&o));
}

// Spec (D28-R5-01: "include configured-public-allow isolation"; design note d28-alias, r5 attack items 1 and 3):
// allowances are this checkout's own. With no stack, the configured public stack's allow-list doesn't accept the user
// path in its file; with a public stack, its own `privacy/allow.txt` accepts it under both variants.
#[test]
fn check_by_stack_allowances_are_this_checkouts_own() {
    let env = Env::new();
    config_elsewhere(&env);
    write(&env.path("elsewhere/stack/privacy/allow.txt"), "alice\n");
    config_private(&env, "", "", Some("[host.alpha]\n"));
    let repo = checkout_saying(&env, "nostack", false, &detail(), "");
    let o = env.lords(&repo, &["check", "--public", "--by-stack"]);
    assert!(!o.status.success(), "the configured public allow-list: refused: {}", text(&o));
    assert!(text(&o).contains("path: `alice`"), "names the hit: {}", text(&o));
    let repo = checkout_saying(&env, "own", true, &detail(), "alice\n");
    for variant in ["--public", "--stack"] {
        let o = env.lords(&repo, &["check", variant, "--by-stack"]);
        assert!(o.status.success(), "its own allow-list accepts it ({variant}): {}", text(&o));
    }
}
