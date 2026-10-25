//! The `lords` binary end to end. Child processes get an empty home and global git config, so no real home
//! dir, user hook or setting is read or touched.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use tempfile::TempDir;

struct Env {
    dir: TempDir,
}

impl Env {
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
            .env("USERPROFILE", self.dir.path().join("home"));
        c
    }

    fn lords(&self, cwd: &Path, args: &[&str]) -> Output {
        self.cmd(env!("CARGO_BIN_EXE_lords"), cwd).args(args).output().unwrap()
    }

    fn git(&self, cwd: &Path, args: &[&str]) -> Output {
        self.cmd("git", cwd).args(args).output().unwrap()
    }

    fn repo(&self) -> PathBuf {
        let r = self.dir.path().join("repo");
        fs::create_dir_all(&r).unwrap();
        assert!(self.git(&r, &["init", "-q"]).status.success());
        r
    }
}

fn text(o: &Output) -> String {
    format!("{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr))
}

// Spec (Privacy: "refuses commits containing private patterns"): with the hook installed, a commit whose staged
// file holds a user path is refused and nothing is committed; a clean commit goes through.
#[test]
fn the_hook_refuses_a_private_commit_and_passes_a_clean_one() {
    let env = Env::new();
    let repo = env.repo();
    let o = env.lords(&repo, &["hooks", "install"]);
    assert!(o.status.success(), "{}", text(&o));
    assert!(repo.join(".git/hooks/pre-commit").is_file());

    fs::write(repo.join("notes.md"), format!("logs in C:\\{}\\alice\\logs\n", "Users")).unwrap();
    env.git(&repo, &["add", "notes.md"]);
    let o = env.git(&repo, &["commit", "-q", "-m", "leak"]);
    assert!(!o.status.success(), "the commit must be refused");
    assert!(text(&o).contains("path"), "{}", text(&o));
    assert!(
        !env.git(&repo, &["rev-parse", "--verify", "-q", "HEAD"]).status.success(),
        "nothing committed"
    );

    fs::write(repo.join("notes.md"), "logs in ~/logs\n").unwrap();
    env.git(&repo, &["add", "notes.md"]);
    let o = env.git(&repo, &["commit", "-q", "-m", "clean"]);
    assert!(o.status.success(), "{}", text(&o));
}

// Spec (hooks install): it never replaces a hook it didn't write unless asked, and then keeps a backup; it
// refuses a repo whose local core.hooksPath would bypass .git/hooks.
#[test]
fn install_respects_existing_hooks() {
    let env = Env::new();
    let repo = env.repo();
    fs::write(repo.join(".git/hooks/pre-commit"), "#!/bin/sh\necho mine\n").unwrap();
    assert!(!env.lords(&repo, &["hooks", "install"]).status.success());
    assert_eq!(
        fs::read_to_string(repo.join(".git/hooks/pre-commit")).unwrap(),
        "#!/bin/sh\necho mine\n"
    );
    let o = env.lords(&repo, &["hooks", "install", "--force"]);
    assert!(o.status.success(), "{}", text(&o));
    let backups: Vec<_> = fs::read_dir(repo.join(".git/hooks"))
        .unwrap()
        .flatten()
        .filter(|e| e.file_name().to_string_lossy().starts_with("pre-commit.bak-"))
        .collect();
    assert_eq!(backups.len(), 1);
    env.git(&repo, &["config", "core.hooksPath", ".githooks"]);
    assert!(!env.lords(&repo, &["hooks", "install"]).status.success());
}

// Spec (orchestrator, 25 Oct: "the privacy check must pass on the whole tree … so M6 is mechanical"): this repo,
// every file git would commit, passes `lords check --public`, and the scan covers exactly those files.
#[test]
fn this_repo_passes_check_public_and_every_file_is_scanned() {
    let env = Env::new();
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..").canonicalize().unwrap();
    let o = env.lords(
        &root,
        &["check", "--public", "--public-stack", root.join("stack").to_str().unwrap()],
    );
    let out = text(&o);
    assert!(o.status.success(), "{out}");
    let listed = env.git(&root, &["ls-files", "--cached", "--others", "--exclude-standard"]);
    let files =
        String::from_utf8_lossy(&listed.stdout).lines().filter(|l| root.join(l).is_file()).count();
    let line = out.lines().find(|l| l.starts_with("public: ")).expect("a summary line");
    let num = |after: &str| -> usize {
        let rest = &line[line.find(after).unwrap() + after.len()..];
        rest.split_whitespace().next().unwrap().trim_end_matches(',').parse().unwrap()
    };
    let (scanned, binary) = (num("entries; "), num("scanned, "));
    assert_eq!(scanned + binary, files, "{line}");
}
