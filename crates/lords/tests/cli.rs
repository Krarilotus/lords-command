//! The `lords` binary end to end. Child processes get an empty home and global git config, so no real home
//! dir, user hook or setting is read or touched.

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
            // The Codex sync asks after it writes Codex's hooks: the stub, never one on PATH (tests/codex_trust.rs).
            .env("LORDS_CODEX", env!("CARGO_BIN_EXE_lords-codex-stub"));
        c
    }

    fn lords(&self, cwd: &Path, args: &[&str]) -> Output {
        self.cmd(env!("CARGO_BIN_EXE_lords"), cwd).args(args).output().unwrap()
    }

    /// `lords` with these variables set, as a git hook sets them.
    fn lords_env(&self, cwd: &Path, args: &[&str], vars: &[(&str, &Path)]) -> Output {
        let mut c = self.cmd(env!("CARGO_BIN_EXE_lords"), cwd);
        for (k, v) in vars {
            c.env(k, v);
        }
        c.args(args).output().unwrap()
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

// Spec (Sol review 25 Oct, finding 12: "One NUL byte disables staged privacy scanning"): a staged file that isn't
// text is refused by name, even with nothing found in it, unless the repo marks it `lords-binary` in
// .gitattributes; one with a private detail after a NUL byte is refused as a finding.
#[test]
fn a_staged_binary_is_refused_unless_marked() {
    let env = Env::new();
    let repo = env.repo();
    fs::write(repo.join("blob.dat"), b"\x00\x01\x02\xff\xfe\x00").unwrap();
    env.git(&repo, &["add", "-A"]);
    let o = env.lords(&repo, &["privacy", "--staged"]);
    assert!(!o.status.success() && text(&o).contains("blob.dat"), "{}", text(&o));

    fs::write(repo.join(".gitattributes"), "*.dat binary lords-binary\n").unwrap();
    env.git(&repo, &["add", "-A"]);
    let o = env.lords(&repo, &["privacy", "--staged"]);
    assert!(o.status.success(), "{}", text(&o));

    let leak = [b"\x00".as_slice(), ["alice", "example.org"].join("@").as_bytes()].concat();
    fs::write(repo.join("blob.dat"), leak).unwrap();
    env.git(&repo, &["add", "-A"]);
    let o = env.lords(&repo, &["privacy", "--staged"]);
    assert!(!o.status.success() && text(&o).contains("contact"), "{}", text(&o));
}

// Spec (Sol review r7, R7-01; guarantee 7: commits through the effective privacy hooks can't disclose a personal
// detail): a type change that brings new bytes is refused by the installed hooks, and nothing is committed.
#[test]
fn the_hook_refuses_a_type_change_with_a_private_detail() {
    let env = Env::new();
    let repo = env.repo();
    let o = env.lords(&repo, &["hooks", "install"]);
    assert!(o.status.success(), "{}", text(&o));
    fs::write(repo.join("file.md"), "clean\n").unwrap();
    env.git(&repo, &["add", "file.md"]);
    assert!(env.git(&repo, &["commit", "-q", "-m", "clean"]).status.success());
    let head = text(&env.git(&repo, &["rev-parse", "HEAD"]));

    fs::write(repo.join("file.md"), format!("Ask {}\n", ["alice", "example.org"].join("@")))
        .unwrap();
    let hash = text(&env.git(&repo, &["hash-object", "-w", "file.md"]));
    // Index only: a link's mode, with no link made on disk.
    let entry = format!("120000,{},file.md", hash.trim());
    assert!(env.git(&repo, &["update-index", "--cacheinfo", &entry]).status.success());
    let staged = text(&env.git(&repo, &["diff", "--cached", "--name-status"]));
    assert!(staged.starts_with("T\t"), "a type change: {staged}");
    let o = env.git(&repo, &["commit", "-q", "-m", "a type change"]);
    assert!(!o.status.success() && text(&o).contains("contact"), "refused: {}", text(&o));
    assert_eq!(text(&env.git(&repo, &["rev-parse", "HEAD"])), head, "nothing committed");
}

// Spec (r7 fix check, FC-03: "refuse mode 160000 (gitlink) with a clear message"): the installed hooks refuse a
// staged gitlink whose commit is in the repo, and nothing is committed.
#[test]
fn the_hook_refuses_a_staged_gitlink() {
    let env = Env::new();
    let repo = env.repo();
    let o = env.lords(&repo, &["hooks", "install"]);
    assert!(o.status.success(), "{}", text(&o));
    fs::write(repo.join("README"), "clean\n").unwrap();
    env.git(&repo, &["add", "README"]);
    assert!(env.git(&repo, &["commit", "-q", "-m", "clean"]).status.success());
    let head = text(&env.git(&repo, &["rev-parse", "HEAD"]));
    let entry = format!("160000,{},sub", head.trim());
    assert!(env.git(&repo, &["update-index", "--add", "--cacheinfo", &entry]).status.success());
    let o = env.git(&repo, &["commit", "-q", "-m", "a gitlink"]);
    assert!(!o.status.success() && text(&o).contains("gitlink"), "refused: {}", text(&o));
    assert_eq!(text(&env.git(&repo, &["rev-parse", "HEAD"])), head, "nothing committed");
}

// Spec (orchestrator, 26 Oct: "`pre-merge-commit` with the same staged privacy scan; `hooks install` writes it
// too"): an automatic merge commit that would bring a private detail is refused by the installed hooks, and
// nothing is committed; a clean merge goes through.
#[test]
fn the_hook_refuses_a_private_merge_commit() {
    let env = Env::new();
    let repo = env.repo();
    fs::write(repo.join("README"), "clean\n").unwrap();
    env.git(&repo, &["add", "README"]);
    assert!(env.git(&repo, &["commit", "-q", "-m", "base"]).status.success());
    let main = text(&env.git(&repo, &["branch", "--show-current"]));
    // Both branches made before the hooks are in: one leaks, one is clean.
    env.git(&repo, &["switch", "-q", "-c", "leak"]);
    fs::write(repo.join("notes.md"), format!("Ask {}\n", ["alice", "example.org"].join("@")))
        .unwrap();
    env.git(&repo, &["add", "notes.md"]);
    assert!(env.git(&repo, &["commit", "-q", "-m", "leak"]).status.success());
    env.git(&repo, &["switch", "-q", main.trim()]);
    env.git(&repo, &["switch", "-q", "-c", "clean"]);
    fs::write(repo.join("other.md"), "clean\n").unwrap();
    env.git(&repo, &["add", "other.md"]);
    assert!(env.git(&repo, &["commit", "-q", "-m", "clean"]).status.success());
    env.git(&repo, &["switch", "-q", main.trim()]);
    fs::write(repo.join("third.md"), "clean\n").unwrap();
    env.git(&repo, &["add", "third.md"]);
    assert!(env
        .git(&repo, &["commit", "-q", "-m", "so a merge isn't a fast-forward"])
        .status
        .success());

    let o = env.lords(&repo, &["hooks", "install"]);
    assert!(o.status.success(), "{}", text(&o));
    assert!(repo.join(".git/hooks/pre-merge-commit").is_file(), "{}", text(&o));
    let head = text(&env.git(&repo, &["rev-parse", "HEAD"]));
    let o = env.git(&repo, &["merge", "--no-ff", "-m", "merge leak", "leak"]);
    assert!(!o.status.success() && text(&o).contains("contact"), "refused: {}", text(&o));
    env.git(&repo, &["merge", "--abort"]);
    assert_eq!(text(&env.git(&repo, &["rev-parse", "HEAD"])), head, "nothing committed");
    let o = env.git(&repo, &["merge", "--no-ff", "-m", "merge clean", "clean"]);
    assert!(o.status.success(), "{}", text(&o));
}

// Spec (Sol review 25 Oct, finding 14: "The installed hook cannot prevent private commit messages"): `hooks
// install` also writes a commit-msg hook, so a clean commit with a private message is refused; `privacy --message`
// checks a message file.
#[test]
fn the_hook_refuses_a_private_commit_message() {
    let env = Env::new();
    let repo = env.repo();
    let o = env.lords(&repo, &["hooks", "install"]);
    assert!(o.status.success(), "{}", text(&o));
    assert!(repo.join(".git/hooks/commit-msg").is_file(), "{}", text(&o));

    fs::write(repo.join("notes.md"), "clean\n").unwrap();
    env.git(&repo, &["add", "notes.md"]);
    let msg = format!("Ask {}", ["alice", "example.org"].join("@"));
    let o = env.git(&repo, &["commit", "-q", "-m", &msg]);
    assert!(!o.status.success() && text(&o).contains("contact"), "{}", text(&o));
    assert!(!env.git(&repo, &["rev-parse", "--verify", "-q", "HEAD"]).status.success());
    assert!(env.git(&repo, &["commit", "-q", "-m", "clean"]).status.success());

    let file = env.dir.path().join("MSG");
    fs::write(&file, &msg).unwrap();
    assert!(!env.lords(&repo, &["privacy", "--message", file.to_str().unwrap()]).status.success());
    fs::write(&file, "clean\n").unwrap();
    assert!(env.lords(&repo, &["privacy", "--message", file.to_str().unwrap()]).status.success());
}

// Spec (Sol review 25 Oct, finding 15: "A broken configured private stack silently weakens the hook's rules"): a
// configured private stack that doesn't load, or whose fleet config can't be read, fails the check; with no private
// stack configured the check runs and says what it skipped.
#[test]
fn a_broken_private_stack_fails_the_check() {
    let env = Env::new();
    let repo = env.repo();
    fs::write(repo.join("notes.md"), "clean\n").unwrap();
    env.git(&repo, &["add", "-A"]);
    let missing = env.dir.path().join("missing-private");
    let o =
        env.lords(&repo, &["privacy", "--staged", "--private-stack", missing.to_str().unwrap()]);
    assert!(!o.status.success(), "{}", text(&o));

    let private = env.dir.path().join("priv");
    fs::create_dir_all(&private).unwrap();
    let fleet = env.dir.path().join("no-fleet.toml");
    fs::write(
        private.join("stack.toml"),
        format!(
            "name = \"priv\"\nvisibility = \"private\"\n[privacy]\nfleet_config = '{}'\n",
            fleet.display()
        ),
    )
    .unwrap();
    let o =
        env.lords(&repo, &["privacy", "--staged", "--private-stack", private.to_str().unwrap()]);
    assert!(!o.status.success() && text(&o).contains("no-fleet.toml"), "{}", text(&o));
    fs::write(&fleet, "[host.alpha]\n").unwrap();
    let o =
        env.lords(&repo, &["privacy", "--staged", "--private-stack", private.to_str().unwrap()]);
    assert!(o.status.success(), "{}", text(&o));

    let o = env.lords(&repo, &["privacy", "--staged"]);
    assert!(o.status.success() && text(&o).contains("no private stack"), "{}", text(&o));
}

// Spec (Sol review r2, R2-12: "An unreadable config silently disables the private privacy rules"): a config.txt
// that is there but can't be read or decoded fails the privacy check and names the file; with none the check runs
// and says what it skipped.
#[test]
fn an_unreadable_config_fails_the_privacy_check() {
    let env = Env::new();
    let repo = env.repo();
    fs::write(repo.join("notes.md"), "clean\n").unwrap();
    env.git(&repo, &["add", "-A"]);
    let o = env.lords(&repo, &["privacy", "--staged"]);
    assert!(o.status.success() && text(&o).contains("no private stack"), "{}", text(&o));

    let cfg = env.dir.path().join("home").join(".lords-command");
    fs::create_dir_all(&cfg).unwrap();
    fs::write(cfg.join("config.txt"), b"private = \xff\xfe\n").unwrap();
    let o = env.lords(&repo, &["privacy", "--staged"]);
    assert!(!o.status.success() && text(&o).contains("config.txt"), "{}", text(&o));
}

// Spec (Sol review r2, R2-25: "Combining --message with --staged silently skips the message"): `privacy --staged
// --message FILE` scans the staged files and the message.
#[test]
fn staged_and_message_are_both_scanned() {
    let env = Env::new();
    let repo = env.repo();
    fs::write(repo.join("notes.md"), "clean\n").unwrap();
    env.git(&repo, &["add", "-A"]);
    let file = env.dir.path().join("MSG");
    fs::write(&file, format!("Ask {}", ["owner", "example.org"].join("@"))).unwrap();
    let o = env.lords(&repo, &["privacy", "--staged", "--message", file.to_str().unwrap()]);
    assert!(!o.status.success() && text(&o).contains("contact"), "{}", text(&o));
    fs::write(&file, "clean\n").unwrap();
    let o = env.lords(&repo, &["privacy", "--staged", "--message", file.to_str().unwrap()]);
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

// Spec (owner, 25 Oct: Claude's auto-memory "stays the write path, and `import` gives it history"; sync makes it
// readable for the other harnesses "without a manual promotion"; backbone requirement BB-5: the index goes to repo and workspace
// files): `sync --apply` first imports what the agent wrote since the last import, with a commit, then writes the
// workspace's files, whose index points at the imported note.
#[test]
fn sync_apply_imports_new_memory_before_writing_the_index() {
    let env = Env::new();
    let d = env.dir.path();
    let (public, private, mem, work) =
        (d.join("pub"), d.join("priv"), d.join("agent-memory"), d.join("work"));
    for p in [&public, &private, &mem, &work] {
        fs::create_dir_all(p).unwrap();
    }
    fs::write(public.join("stack.toml"), "name = \"pub\"\nvisibility = \"public\"\n").unwrap();
    fs::write(
        private.join("stack.toml"),
        format!(
            "name = \"priv\"\nvisibility = \"private\"\n[[memory_stores]]\nname = \"agent\"\npath = '{}'\nscope = \"global\"\n\
             [workspaces.work]\npath = '{}'\nadapters = [\"codex\"]\n",
            mem.display(),
            work.display()
        ),
    )
    .unwrap();
    assert!(env.git(&private, &["init", "-q"]).status.success());
    env.git(&private, &["add", "-A"]);
    env.git(&private, &["commit", "-q", "-m", "start"]);
    fs::write(mem.join("note.md"), "---\nname: note\ndescription: the hook line\n---\nThe note.\n")
        .unwrap();

    let args = [
        "sync",
        "--public-stack",
        public.to_str().unwrap(),
        "--private-stack",
        private.to_str().unwrap(),
    ];
    let dry = env.lords(d, &args);
    assert!(text(&dry).contains("memory store agent: 1 new"), "{}", text(&dry));
    assert!(!work.join("AGENTS.override.md").exists(), "a dry run writes nothing");

    let o = env.lords(d, &[&args[..], &["--apply"]].concat());
    assert!(o.status.success(), "{}", text(&o));
    let agents = fs::read_to_string(work.join("AGENTS.override.md")).unwrap();
    assert!(agents.contains("- note: the hook line\n"), "{agents}");
    assert!(
        agents.lines().any(|l| l.starts_with("In `")
            && l.contains("global")
            && l.ends_with("as `<name>.md`:")),
        "points at the imported copy: {agents}"
    );
    let log =
        String::from_utf8_lossy(&env.git(&private, &["log", "--format=%s"]).stdout).into_owned();
    assert!(log.starts_with("Import memory from agent: 1 new, 0 changed"), "{log}");
}

// Spec (load-order.md; orchestrator D1, 25 Oct: unmanaged repos are a known gap, "list it in check's info
// output"): with a second Claude profile, `check` says that `~/.claude/CLAUDE.md` hides repos' AGENTS.md, until a
// settings target in that profile excludes it.
#[test]
fn check_lists_the_hidden_agents_md_gap_until_excluded() {
    let env = Env::new();
    let d = env.dir.path();
    let home = d.join("home");
    fs::create_dir_all(home.join(".claude")).unwrap();
    fs::write(home.join(".claude/CLAUDE.md"), "old global text\n").unwrap();
    let (public, private) = (d.join("pub"), d.join("priv"));
    fs::create_dir_all(&public).unwrap();
    fs::create_dir_all(&private).unwrap();
    fs::write(public.join("stack.toml"), "name = \"pub\"\nvisibility = \"public\"\n").unwrap();
    let base = "name = \"priv\"\nvisibility = \"private\"\n[[targets]]\nname = \"balanced\"\nadapter = \"claude\"\nkind = \"instructions\"\npath = \"~/.claude-balanced/CLAUDE.md\"\n";
    fs::write(private.join("stack.toml"), base).unwrap();
    let args = [
        "check",
        "--public-stack",
        public.to_str().unwrap(),
        "--private-stack",
        private.to_str().unwrap(),
    ];
    let out = text(&env.lords(d, &args));
    assert!(out.contains("hides AGENTS.md in repos sync doesn't manage"), "{out}");
    let excluded = format!("{base}[[targets]]\nname = \"balanced settings\"\nadapter = \"claude\"\nkind = \"settings\"\npath = \"~/.claude-balanced/settings.json\"\nexclude = [\"~/.claude/CLAUDE.md\"]\n");
    fs::write(private.join("stack.toml"), excluded).unwrap();
    let out = text(&env.lords(d, &args));
    assert!(!out.contains("hides AGENTS.md"), "{out}");
}

// Spec (orchestrator, 25 Oct: `lords check --public` makes the pre-push check mechanical): run inside a repo with
// a stack/, it checks that repo, even when config.txt names another public stack.
#[test]
fn check_public_checks_the_repo_it_runs_in() {
    let env = Env::new();
    let d = env.dir.path();
    let elsewhere = d.join("elsewhere");
    fs::create_dir_all(&elsewhere).unwrap();
    fs::write(elsewhere.join("stack.toml"), "name = \"x\"\nvisibility = \"public\"\n").unwrap();
    let cfg = d.join("home/.lords-command");
    fs::create_dir_all(&cfg).unwrap();
    fs::write(cfg.join("config.txt"), format!("public = {}\n", elsewhere.display())).unwrap();
    let repo = env.repo();
    fs::create_dir_all(repo.join("stack")).unwrap();
    fs::write(repo.join("stack/stack.toml"), "name = \"here\"\nvisibility = \"public\"\n").unwrap();
    let sep = std::path::MAIN_SEPARATOR;
    fs::write(repo.join("notes.md"), format!("see C:{sep}{}{sep}alice{sep}x\n", "Users")).unwrap();
    let o = env.lords(&repo, &["check", "--public"]);
    let out = text(&o);
    assert!(!o.status.success() && out.contains("notes.md"), "{out}");
}

// Spec (README: "lords compose … what a harness reads there, with sources"): the user level prints the composed
// text, the JSON form maps each line to its entry, and a role file needs a place.
#[test]
fn compose_prints_what_a_harness_reads() {
    let env = Env::new();
    let d = env.dir.path();
    let (public, private) = (d.join("pub"), d.join("priv"));
    fs::create_dir_all(public.join("global/rules")).unwrap();
    fs::create_dir_all(&private).unwrap();
    fs::write(public.join("stack.toml"), "name = \"pub\"\nvisibility = \"public\"\n").unwrap();
    fs::write(private.join("stack.toml"), "name = \"priv\"\nvisibility = \"private\"\n").unwrap();
    fs::write(
        public.join("global/rules/a.md"),
        "---\nid: rule.a\nkind: rule\ntitle: A\naudience: [all]\nscope: global\nstatus: active\nvisibility: public\norigin:\n  type: own\n---\nRule A holds.\n",
    )
    .unwrap();
    let args = [
        "compose",
        "--public-stack",
        public.to_str().unwrap(),
        "--private-stack",
        private.to_str().unwrap(),
    ];
    let o = env.lords(d, &args);
    assert!(
        o.status.success() && String::from_utf8_lossy(&o.stdout).contains("Rule A holds."),
        "{}",
        text(&o)
    );
    let o = env.lords(d, &[&args[..], &["--json"]].concat());
    let out = String::from_utf8_lossy(&o.stdout).into_owned();
    assert!(
        out.contains("\"text\": \"Rule A holds.\"") && out.contains("\"id\": \"rule.a\""),
        "{out}"
    );
    assert!(
        !env.lords(d, &[&args[..], &["--role", "coder"]].concat()).status.success(),
        "a role file needs --repo"
    );
}

/// A repo `r` (one commit) declared in a private stack with one rule, worktrees under `wts`; the hook installed.
fn repo_with_post_checkout(env: &Env) -> (PathBuf, PathBuf, PathBuf) {
    let (repo, wts, stacks) = post_checkout_repo(env);
    let o = install_post_checkout(env, &repo, &stacks);
    assert!(o.status.success(), "{}", text(&o));
    let log = repo_log(&repo);
    (repo, wts, log)
}

/// `lords hooks install --post-checkout` in `repo`, with the stacks `post_checkout_repo` made.
fn install_post_checkout(env: &Env, repo: &Path, stacks: &[String; 4]) -> Output {
    let args: Vec<&str> = stacks.iter().map(String::as_str).collect();
    env.lords(repo, &[&["hooks", "install", "--post-checkout"][..], &args].concat())
}

/// The repo `repo_with_post_checkout` makes, without the hook: the repo, where its worktrees go, and the stack args.
fn post_checkout_repo(env: &Env) -> (PathBuf, PathBuf, [String; 4]) {
    let d = env.dir.path();
    let (public, private, repo) = (d.join("pub"), d.join("priv"), d.join("repo"));
    fs::create_dir_all(public.join("global/rules")).unwrap();
    fs::create_dir_all(private.join("global/rules")).unwrap();
    fs::write(
        public.join("stack.toml"),
        "name = \"pub\"\nvisibility = \"public\"\nroles = [\"coder\"]\n",
    )
    .unwrap();
    fs::write(
        private.join("stack.toml"),
        format!(
            "name = \"priv\"\nvisibility = \"private\"\nroles = [\"coder\"]\n[repos.r]\ncheckouts = ['{}']\nworktree_roots = ['{}']\nadapters = [\"claude\"]\n",
            repo.display(),
            d.join("wts").display()
        ),
    )
    .unwrap();
    fs::write(
        private.join("global/rules/r.md"),
        "---\nid: rule.r\nkind: rule\ntitle: r\naudience: [all]\nroles: [coder]\nscope: global\nstatus: active\nvisibility: private\norigin:\n  type: own\n---\nTest before merging.\n",
    )
    .unwrap();
    let repo = env.repo();
    fs::write(repo.join("README.md"), "a repo\n").unwrap();
    env.git(&repo, &["add", "-A"]);
    assert!(env.git(&repo, &["commit", "-q", "-m", "start"]).status.success());
    let stacks = [
        "--public-stack".to_string(),
        public.display().to_string(),
        "--private-stack".to_string(),
        private.display().to_string(),
    ];
    (repo, d.join("wts"), stacks)
}

fn repo_log(repo: &Path) -> PathBuf {
    repo.join(".git").join("lords-post-checkout.log")
}

/// Waits up to 60 s for `path` (the hook writes in the background).
fn appears(path: &Path) -> bool {
    let until = std::time::Instant::now() + std::time::Duration::from_secs(60);
    while std::time::Instant::now() < until {
        if path.is_file() {
            return true;
        }
        std::thread::sleep(std::time::Duration::from_millis(200));
    }
    false
}

// Spec (orchestrator, 25 Oct: "`lords hooks install` writes the managed repo's own .git/hooks/post-checkout … It
// acts only on a new worktree (old HEAD all zeros) and runs `lords sync --repo X --place <that worktree> --apply`
// for that one place. It must never fail or slow the checkout … If a repo already has a post-checkout hook, chain
// it, don't replace it"): a new worktree gets its place file; a branch checkout runs no sync and returns at once;
// a hook that was there before still runs, and its exit status is kept.
#[test]
fn a_new_worktree_gets_its_place_files_from_the_post_checkout_hook() {
    let env = Env::new();
    // A hook that was there first: it is chained, not replaced.
    let hooks = env.dir.path().join("repo/.git/hooks");
    fs::create_dir_all(&hooks).unwrap();
    fs::write(
        hooks.join("post-checkout"),
        "#!/bin/sh\necho ran >> \"$(git rev-parse --git-common-dir)/before.log\"\n",
    )
    .unwrap();
    let (repo, wts, log) = repo_with_post_checkout(&env);
    assert!(hooks.join("post-checkout.before-lords").is_file(), "the old hook is kept");

    let wt = wts.join("one");
    let o = env.git(&repo, &["worktree", "add", "-q", "-b", "one", wt.to_str().unwrap()]);
    assert!(o.status.success(), "{}", text(&o));
    assert!(
        appears(&wt.join("CLAUDE.local.md")),
        "log: {}",
        fs::read_to_string(&log).unwrap_or_default()
    );
    let place = fs::read_to_string(wt.join("CLAUDE.local.md")).unwrap();
    assert!(
        place.contains(
            "
Test before merging.
"
        ),
        "{place}"
    );
    assert!(
        !wt.join("AGENTS.override.md").exists(),
        "the stack gives this repo Claude's files only"
    );
    assert!(fs::read_to_string(repo.join(".git/before.log")).unwrap().contains("ran"));

    // The new worktree's sync runs in the background: wait until its log stops growing.
    let mut before = fs::read_to_string(&log).unwrap();
    loop {
        std::thread::sleep(std::time::Duration::from_millis(1500));
        let now = fs::read_to_string(&log).unwrap();
        if now == before {
            break;
        }
        before = now;
    }
    let start = std::time::Instant::now();
    let o = env.git(&wt, &["checkout", "-q", "-b", "two"]);
    let took = start.elapsed();
    assert!(o.status.success(), "{}", text(&o));
    std::thread::sleep(std::time::Duration::from_millis(500));
    assert_eq!(fs::read_to_string(&log).unwrap(), before, "a branch checkout runs no sync");
    eprintln!("branch checkout with the hook: {took:?}");
}

// Spec (above: "including the case where lords is missing"): with the lords binary gone, `git worktree add` still
// succeeds and the hook only logs that it wrote nothing.
#[test]
fn a_missing_lords_never_fails_the_checkout() {
    let env = Env::new();
    let (repo, wts, log) = repo_with_post_checkout(&env);
    let hook = repo.join(".git/hooks/post-checkout");
    let script = fs::read_to_string(&hook).unwrap();
    let exe = script.lines().find_map(|l| l.strip_prefix("exe=")).unwrap().to_string();
    fs::write(&hook, script.replace(&exe, "'/nowhere/lords.exe'")).unwrap();

    let wt = wts.join("one");
    let o = env.git(&repo, &["worktree", "add", "-q", "-b", "one", wt.to_str().unwrap()]);
    assert!(o.status.success(), "{}", text(&o));
    assert!(!wt.join("CLAUDE.local.md").exists());
    assert!(fs::read_to_string(&log).unwrap().contains("lords not found at /nowhere/lords.exe"));
}

// ---------------------------------------------------------------- post-checkout through a dispatcher (D14)

/// A verbatim copy of the owner's attribution guard dispatcher and its post-checkout wrapper
/// (`~/.config/git/attribution-guard`, 2 Nov: dispatch.sh sha256 61886b99…, hooks/post-checkout 00949024…), set up in
/// `dir` as the owner's is: `dir/hooks/post-checkout` hands over to `dir/dispatch.sh`. Returns `dir/hooks`.
fn real_dispatcher(dir: &Path) -> PathBuf {
    fs::create_dir_all(dir.join("hooks")).unwrap();
    for (name, text) in [
        ("dispatch.sh", include_str!("support/attribution-guard/dispatch.sh")),
        ("hooks/post-checkout", include_str!("support/attribution-guard/hooks/post-checkout")),
    ] {
        let p = dir.join(name);
        fs::write(&p, text).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&p, fs::Permissions::from_mode(0o755)).unwrap();
        }
    }
    dir.join("hooks")
}

/// An owner's post-checkout hook: it logs that it ran, in the git common dir's `before.log`.
const OWNERS_HOOK: &str =
    "#!/bin/sh\necho ran >> \"$(git rev-parse --git-common-dir)/before.log\"\n";

/// One config value of `repo` as git reads it (any scope), or `None`.
fn config(env: &Env, repo: &Path, key: &str) -> Option<String> {
    let o = env.git(repo, &["config", "--get", key]);
    o.status.success().then(|| String::from_utf8_lossy(&o.stdout).trim().to_string())
}

/// Waits until the post-checkout log stops growing: the hook's sync runs in the background.
fn settled(log: &Path) {
    let mut before = fs::read_to_string(log).unwrap_or_default();
    loop {
        std::thread::sleep(std::time::Duration::from_millis(1500));
        let now = fs::read_to_string(log).unwrap_or_default();
        if now == before {
            return;
        }
        before = now;
    }
}

/// `git worktree add` of a new branch `name` under `wts`, then whether its place file appears (the hook's sync).
fn new_worktree_gets_its_place_file(env: &Env, repo: &Path, wts: &Path, name: &str) -> bool {
    let wt = wts.join(name);
    let o = env.git(repo, &["worktree", "add", "-q", "-b", name, wt.to_str().unwrap()]);
    assert!(o.status.success(), "{}", text(&o));
    let got = appears(&wt.join("CLAUDE.local.md"));
    settled(&repo_log(repo));
    got
}

// Spec (D14, the Lords owner's contract, 2 Nov: "When the effective hooks path (local or global) is a dispatcher that
// provably forwards `post-checkout` to a directory, the install writes Lords' hook into that forwarded-to directory,
// chaining any hook already there, as today … The install never repoints `core.hooksPath` and never edits the
// dispatcher. Test it against a copy of the real dispatcher: `git worktree add` then gets place files."): with the
// repo's local hooks path the real dispatcher, the install succeeds and chains the owner's hook; a new worktree gets
// its place file and the owner's hook runs; the hooks path and the dispatcher are as they were. A conditional include
// that only a linked worktree reads, and that routes no hook (it names the user), doesn't stop it (review r1, R1-01).
#[test]
fn the_post_checkout_install_goes_through_a_local_dispatcher_that_forwards() {
    let env = Env::new();
    let (repo, wts, stacks) = post_checkout_repo(&env);
    let guard = real_dispatcher(&env.dir.path().join("guard"));
    let guard_path = guard.to_str().unwrap();
    env.git(&repo, &["config", "core.hooksPath", guard_path]);
    fs::write(repo.join(".git/user.inc"), "[user]\n\tname = Linked\n").unwrap();
    env.git(&repo, &["config", "includeIf.gitdir:**/worktrees/.path", "user.inc"]);
    fs::write(repo.join(".git/hooks/post-checkout"), OWNERS_HOOK).unwrap();
    let dispatcher = fs::read(env.dir.path().join("guard/dispatch.sh")).unwrap();

    let o = install_post_checkout(&env, &repo, &stacks);
    assert!(o.status.success(), "{}", text(&o));
    assert_eq!(
        fs::read_to_string(repo.join(".git/hooks/post-checkout.before-lords")).unwrap(),
        OWNERS_HOOK,
        "the owner's hook is chained"
    );
    assert!(
        new_worktree_gets_its_place_file(&env, &repo, &wts, "one"),
        "log: {}",
        fs::read_to_string(repo_log(&repo)).unwrap_or_default()
    );
    assert!(fs::read_to_string(repo.join(".git/before.log")).unwrap().contains("ran"));
    assert_eq!(config(&env, &repo, "core.hooksPath").as_deref(), Some(guard_path));
    assert_eq!(fs::read(env.dir.path().join("guard/dispatch.sh")).unwrap(), dispatcher);
}

// Spec (D14 contract, above: "local or global"; and "That dir is `attributionGuard.previousHooksPath` when it's set,
// else the common dir's `hooks/`"): with the global hooks path the real dispatcher, and again with the repo's
// `previousHooksPath` set to its own hooks dir (absolute), the install succeeds and a new worktree gets its place file.
#[test]
fn the_post_checkout_install_goes_through_a_global_dispatcher_and_a_previous_hooks_path() {
    let env = Env::new();
    let (repo, wts, stacks) = post_checkout_repo(&env);
    let guard = real_dispatcher(&env.dir.path().join("guard"));
    env.git(&repo, &["config", "--global", "core.hooksPath", guard.to_str().unwrap()]);
    let o = install_post_checkout(&env, &repo, &stacks);
    assert!(o.status.success(), "{}", text(&o));
    assert!(new_worktree_gets_its_place_file(&env, &repo, &wts, "one"));

    let own = repo.join(".git/hooks").canonicalize().unwrap();
    let own = own.to_str().unwrap().trim_start_matches(r"\\?\");
    env.git(&repo, &["config", "attributionGuard.previousHooksPath", own]);
    let o = install_post_checkout(&env, &repo, &stacks);
    assert!(o.status.success(), "{}", text(&o));
    assert!(new_worktree_gets_its_place_file(&env, &repo, &wts, "two"));
}

// Spec (D14 contract: "A hooks path Lords can't prove forwards stays a refusal, with a message that names the case";
// design note 2026-11-02-lords-hooks-dispatcher, H2, H4, H6): each case below is refused with its own reason, and
// leaves everything as it was: the owner's hook back at `post-checkout` with its bytes, no `.before-lords`, no ledger
// record of a hook, `core.hooksPath` unchanged.
// - The real dispatcher, `previousHooksPath = .githooks` (relative, tracked hooks in the tree): "relative".
// - The real dispatcher, `skipPreviousHook = post-checkout`: "skipPreviousHook".
// - The real dispatcher, `previousHooksPath` absolute elsewhere: "not to this repo's own hooks dir". Elsewhere is
//   another repo's hooks dir with Lords' post-checkout installed (same tool home), which answers probes too: only an
//   answer bound to this repo's hooks dir tells them apart (sol-loop r1, S2).
// - A local hooks path whose post-checkout runs nothing: "local".
// - A global hooks path without a post-checkout (today this one installed, with a note): "global".
// - Review r1, R1-01, its witness: no hooks path in the main checkout, but a conditional include that only a linked
//   worktree's git dir matches sets an empty one there; the stand-in can't see it, so the include is named.
// Since D14b these cases fall back to a config hook; an owner's global `hook.lords-post-checkout` blocks that here, and
// is named too (design note 2026-11-02-lords-hooks-config, C5), and the local config stays as it was.
#[test]
fn a_hooks_path_that_doesnt_reach_the_hook_is_refused_and_changes_nothing() {
    let env = Env::new();
    let (repo, _wts, stacks) = post_checkout_repo(&env);
    let d = env.dir.path();
    let guard = real_dispatcher(&d.join("guard"));
    let guard = guard.to_str().unwrap();
    let elsewhere = other_repo_with_lords_hook(&env);
    for (k, v) in [("event", "pre-commit"), ("command", "true")] {
        env.git(&repo, &["config", "--global", &format!("hook.lords-post-checkout.{k}"), v]);
    }
    let silent = d.join("silent");
    fs::create_dir_all(&silent).unwrap();
    fs::write(silent.join("post-checkout"), "#!/bin/sh\nexit 0\n").unwrap();
    let empty = d.join("empty");
    fs::create_dir_all(&empty).unwrap();
    let inc = format!("[core]\n\thooksPath = {}\n", empty.display()).replace('\\', "/");
    fs::write(repo.join(".git/linked-hooks.inc"), inc).unwrap();
    let hooks = repo.join(".git/hooks");
    fs::write(hooks.join("post-checkout"), OWNERS_HOOK).unwrap();
    let before = listing(&hooks);
    let ledger = d.join("home/.lords-command/state/ledger.toml");
    let recorded = fs::read(&ledger).unwrap();

    type Case<'a> = (&'a str, Vec<(&'a str, &'a str)>, &'a str);
    let cases: [Case; 6] = [
        (
            "relative",
            vec![("core.hooksPath", guard), ("attributionGuard.previousHooksPath", ".githooks")],
            "relative",
        ),
        (
            "skip",
            vec![("core.hooksPath", guard), ("attributionGuard.skipPreviousHook", "post-checkout")],
            "skipPreviousHook",
        ),
        (
            "elsewhere",
            vec![
                ("core.hooksPath", guard),
                ("attributionGuard.previousHooksPath", elsewhere.to_str().unwrap()),
            ],
            "not to this repo's own hooks dir",
        ),
        ("local", vec![("core.hooksPath", silent.to_str().unwrap())], "local"),
        ("global", vec![("--global core.hooksPath", empty.to_str().unwrap())], "global"),
        (
            "include",
            vec![("includeIf.gitdir:**/worktrees/.path", "linked-hooks.inc")],
            "includeif.gitdir:**/worktrees/.path",
        ),
    ];
    for (name, settings, says) in cases {
        for (k, v) in &settings {
            let mut args = vec!["config"];
            args.extend(k.split(' '));
            args.push(v);
            assert!(env.git(&repo, &args).status.success(), "{name}: git {args:?}");
        }
        let (hooks_path, local) = (config(&env, &repo, "core.hooksPath"), local_config(&repo));
        let o = install_post_checkout(&env, &repo, &stacks);
        let out = text(&o);
        eprintln!("{name}: {out}");
        assert!(!o.status.success(), "{name}: must be refused: {out}");
        assert!(out.contains(says) && out.contains("post-checkout"), "{name}: {out}");
        assert!(name == "include" || out.contains("set already (global"), "{name}: {out}");
        assert_eq!(local_config(&repo), local, "{name}: the local config is as it was");
        assert_eq!(listing(&hooks), before, "{name}: the hooks dir is as it was");
        assert_eq!(fs::read(&ledger).unwrap(), recorded, "{name}: the ledger is as it was");
        assert_eq!(config(&env, &repo, "core.hooksPath"), hooks_path, "{name}");
        for (k, _) in &settings {
            let mut args = vec!["config", "--unset-all"];
            args.extend(k.split(' '));
            assert!(env.git(&repo, &args).status.success(), "{name}: git {args:?}");
        }
    }
    // Sol-loop repair r1, R1-01: the include refuses before anything is written, so a missing hooks dir stays missing.
    fs::remove_dir_all(&hooks).unwrap();
    env.git(&repo, &["config", "includeIf.gitdir:**/worktrees/.path", "linked-hooks.inc"]);
    assert!(!install_post_checkout(&env, &repo, &stacks).status.success());
    assert!(!hooks.exists(), "no hooks dir made");
}

/// Every file in `dir`, by name, with its bytes, sorted.
fn listing(dir: &Path) -> Vec<(String, Vec<u8>)> {
    let mut v: Vec<_> = fs::read_dir(dir)
        .unwrap()
        .flatten()
        .map(|e| (e.file_name().to_string_lossy().into_owned(), fs::read(e.path()).unwrap()))
        .collect();
    v.sort();
    v
}

/// Another repo, `other` (one commit), declared in a stack of its own (`pub2`, `priv2`) and given Lords' post-checkout
/// in the same tool home; its hooks dir.
fn other_repo_with_lords_hook(env: &Env) -> PathBuf {
    let d = env.dir.path();
    let other = d.join("other");
    fs::create_dir_all(&other).unwrap();
    assert!(env.git(&other, &["init", "-q"]).status.success());
    fs::write(other.join("README.md"), "another repo\n").unwrap();
    env.git(&other, &["add", "-A"]);
    assert!(env.git(&other, &["commit", "-q", "-m", "start"]).status.success());
    let stacks = copied_stacks(env, "2");
    let manifest = format!(
        "name = \"priv\"\nvisibility = \"private\"\nroles = [\"coder\"]\n[repos.o]\ncheckouts = ['{}']\nadapters = [\"claude\"]\n",
        other.display()
    );
    fs::write(d.join("priv2/stack.toml"), manifest).unwrap();
    let o = install_post_checkout(env, &other, &stacks);
    assert!(o.status.success(), "{}", text(&o));
    other.join(".git/hooks")
}

/// The stacks `post_checkout_repo` made, copied to `pub<n>` and `priv<n>`; their args.
fn copied_stacks(env: &Env, n: &str) -> [String; 4] {
    let d = env.dir.path();
    for f in ["pub/stack.toml", "priv/stack.toml", "priv/global/rules/r.md", "pub/global/rules"] {
        let to = d.join(f.replacen("pub", &format!("pub{n}"), 1).replacen(
            "priv",
            &format!("priv{n}"),
            1,
        ));
        if d.join(f).is_dir() {
            fs::create_dir_all(&to).unwrap();
        } else {
            fs::create_dir_all(to.parent().unwrap()).unwrap();
            fs::copy(d.join(f), &to).unwrap();
        }
    }
    let arg = |s: &str| d.join(format!("{s}{n}")).display().to_string();
    ["--public-stack".into(), arg("pub"), "--private-stack".into(), arg("priv")]
}

// Spec (sol-loop r1, S1, and r2, S2; design note H1, H4: only the hook this install wrote may answer its probe, and a
// refused install leaves the hooks dir as it was): after a first install, the real dispatcher forwards to another dir
// holding a copy of that earlier hook; a re-install is refused, as the copy isn't the hook it writes, and keeps the
// tool's earlier hook, byte for byte, though the new one would differ (the stacks are named at other paths), and the
// ledger as it was. Nor is a config hook written beside that earlier hook: both would run (D14b, design note
// 2026-11-02-lords-hooks-config, C6), so the refusal names it and the local config is as it was.
#[test]
fn a_refused_reinstall_keeps_the_tools_earlier_hook() {
    let env = Env::new();
    let (repo, _wts, stacks) = post_checkout_repo(&env);
    let o = install_post_checkout(&env, &repo, &stacks);
    assert!(o.status.success(), "{}", text(&o));
    let d = env.dir.path();
    let hooks = repo.join(".git/hooks");
    let ledger = d.join("home/.lords-command/state/ledger.toml");
    let stale = d.join("stale");
    fs::create_dir_all(&stale).unwrap();
    fs::copy(hooks.join("post-checkout"), stale.join("post-checkout")).unwrap();
    let guard = real_dispatcher(&d.join("guard"));
    env.git(&repo, &["config", "core.hooksPath", guard.to_str().unwrap()]);
    env.git(&repo, &["config", "attributionGuard.previousHooksPath", stale.to_str().unwrap()]);
    let before = (listing(&hooks), fs::read(&ledger).unwrap(), local_config(&repo));

    let o = install_post_checkout(&env, &repo, &copied_stacks(&env, "2"));
    let out = text(&o);
    assert!(
        !o.status.success() && !out.contains("back failed") && out.contains("stands in"),
        "{out}"
    );
    assert_eq!((listing(&hooks), fs::read(&ledger).unwrap(), local_config(&repo)), before);
}

// Spec (review r1, D14D15-R1-02: "A refusal restores the hooks dir and ledger exactly, including a chained earlier hook
// and a Lords hook from before"; its witness): after an install over an owner's hook, a re-install whose probe passes
// but whose ledger save is refused (the ledger NTFS-compressed, an attribute fsguard won't carry over) leaves the hooks
// dir, every file in it byte for byte (no copy of the earlier hook left), and the ledger as they were. Windows only:
// elsewhere fsguard keeps every mode, and a read-only state dir would stop the probe first.
#[cfg(windows)]
#[test]
fn a_reinstall_whose_ledger_cant_be_saved_changes_nothing() {
    let env = Env::new();
    let (repo, _wts, stacks) = post_checkout_repo(&env);
    let hooks = repo.join(".git/hooks");
    fs::write(hooks.join("post-checkout"), OWNERS_HOOK).unwrap();
    let o = install_post_checkout(&env, &repo, &stacks);
    assert!(o.status.success(), "{}", text(&o));
    let ledger = env.dir.path().join("home/.lords-command/state/ledger.toml");
    let before = (listing(&hooks), fs::read(&ledger).unwrap());
    let o = Command::new("compact").args(["/c", "/q"]).arg(&ledger).output().unwrap();
    assert!(o.status.success(), "{}", text(&o));

    let o = install_post_checkout(&env, &repo, &stacks);
    assert!(!o.status.success(), "{}", text(&o));
    assert_eq!((listing(&hooks), fs::read(&ledger).unwrap()), before, "{}", text(&o));
}

// ---------------------------------------------------------------- post-checkout as a config hook (D14b)

/// The repo's local config file, its bytes.
fn local_config(repo: &Path) -> Vec<u8> {
    fs::read(repo.join(".git/config")).unwrap()
}

/// The relative case: the real dispatcher as the local hooks path, forwarding to `.githooks`, a path relative to each
/// worktree, so to its tracked files (design note 2026-11-02-lords-hooks-dispatcher, "Finding first").
fn forwards_to_tracked_hooks(env: &Env, repo: &Path) {
    let guard = real_dispatcher(&env.dir.path().join("guard"));
    env.git(repo, &["config", "core.hooksPath", guard.to_str().unwrap()]);
    env.git(repo, &["config", "attributionGuard.previousHooksPath", ".githooks"]);
}

/// An owner's note in an otherwise empty `hook.lords-post-checkout` section: no entry, so nobody's hook (sol-loop r1, S1).
fn owners_note(repo: &Path) {
    let mut text = local_config(repo);
    text.extend_from_slice(b"[hook \"lords-post-checkout\"]\n\t# the owner's note\n");
    fs::write(repo.join(".git/config"), text).unwrap();
}

// Spec (D14b, the Lords owner's brief, 2 Nov: "`lords hooks install --post-checkout` on a repo whose own hooks dir
// can't be proven to run installs a config-defined hook instead, in the repo's local config only. It never repoints
// `core.hooksPath`, never touches the dispatcher or its keys, and never edits a tracked file"; "`lords uninstall` for that
// repo removes exactly that entry, and only while it still holds what Lords wrote"; design note
// 2026-11-02-lords-hooks-config, C1-C3, C6, C8): in the relative case, with an owner's global config hook for
// post-checkout under another name, the install writes `hook.lords-post-checkout` and names why the hooks dir isn't
// reached; the hooks dir, the dispatcher's keys and the tracked files are as they were; `git worktree add` gets its place
// file and runs the owner's hook too. Once the hooks dir would be reached, a re-install stays a config hook. Uninstall
// leaves the local config byte for byte as before the install, an owner's note in the hook's section included (sol-loop
// r1, S1); after an owner's edit it leaves the entry as edited.
#[test]
fn a_hooks_dir_that_isnt_reached_gets_a_config_hook_instead() {
    let env = Env::new();
    let (repo, wts, stacks) = post_checkout_repo(&env);
    fs::create_dir_all(repo.join(".githooks")).unwrap();
    fs::write(repo.join(".githooks/pre-commit"), "#!/bin/sh\nexit 0\n").unwrap();
    env.git(&repo, &["add", "-A"]);
    assert!(env.git(&repo, &["commit", "-q", "-m", "tracked hooks"]).status.success());
    forwards_to_tracked_hooks(&env, &repo);
    let owners = "echo ran >> \"$(git rev-parse --git-common-dir)/owners.log\"";
    env.git(&repo, &["config", "--global", "hook.owners.command", owners]);
    env.git(&repo, &["config", "--global", "hook.owners.event", "post-checkout"]);
    owners_note(&repo);
    let hooks = repo.join(".git/hooks");
    let before = (listing(&hooks), local_config(&repo));

    let o = install_post_checkout(&env, &repo, &stacks);
    let out = text(&o);
    assert!(
        o.status.success() && out.contains("relative") && out.contains("lords-post-checkout"),
        "{out}"
    );
    assert_eq!(listing(&hooks), before.0, "the hooks dir is as it was");
    assert_eq!(
        config(&env, &repo, "attributionGuard.previousHooksPath").as_deref(),
        Some(".githooks")
    );
    let _ = fs::remove_file(repo.join(".git/owners.log"));
    assert!(
        new_worktree_gets_its_place_file(&env, &repo, &wts, "one"),
        "log: {}",
        fs::read_to_string(repo_log(&repo)).unwrap_or_default()
    );
    assert!(fs::read_to_string(repo.join(".git/owners.log")).unwrap().contains("ran"));
    assert!(text(&env.git(&repo, &["status", "--porcelain"])).is_empty(), "no tracked file edited");

    let own = hooks.canonicalize().unwrap();
    let own = own.to_str().unwrap().trim_start_matches(r"\\?\");
    env.git(&repo, &["config", "attributionGuard.previousHooksPath", own]);
    let o = install_post_checkout(&env, &repo, &stacks);
    assert!(o.status.success() && listing(&hooks) == before.0, "{}", text(&o));
    env.git(&repo, &["config", "attributionGuard.previousHooksPath", ".githooks"]);

    let mut uninstall = vec!["uninstall", "--repo", "r", "--apply"];
    uninstall.extend(stacks.iter().map(String::as_str));
    let o = env.lords(&repo, &uninstall);
    assert!(o.status.success(), "{}", text(&o));
    assert_eq!(local_config(&repo), before.1, "the local config is byte for byte as before");
    assert!(install_post_checkout(&env, &repo, &stacks).status.success());
    env.git(&repo, &["config", "hook.lords-post-checkout.enabled", "false"]);
    let edited = local_config(&repo);
    let o = env.lords(&repo, &uninstall);
    assert!(
        o.status.success() && text(&o).contains("changed since Lords wrote it"),
        "{}",
        text(&o)
    );
    assert_eq!(local_config(&repo), edited, "the owner's edit stays");
    let ledger = env.dir.path().join("home/.lords-command/state/ledger.toml");
    assert!(!fs::read_to_string(ledger).unwrap().contains("hookconfig:"), "its record goes");
}

// Spec (D14b brief: "where Git doesn't list config hooks …, and wherever the probe fails, the install refuses, names
// the reason, and leaves the config as it was"; "every failure path restores the config and the ledger exactly"; design
// note C3-C5, C7, H7): in the relative case each of these is refused, naming its reason, with the local config, the
// ledger and the hooks dir byte for byte as before:
// - `git hook list` fails: Git 2.43 has no `list` (exit 129); the witness here is a config Git 2.56 dies on (128);
// - the owner set `hook.lords-post-checkout` locally;
// - a conditional include that only a linked worktree reads sets `hook.*`;
// - the probe gets no answer after the write: `hook.post-checkout.enabled = false` switches the event off.
// Then, over Lords' config hook, a re-install whose probe fails and (Windows) one whose ledger save fails. An owner's
// note in the hook's otherwise empty section stays through each (sol-loop r1, S1).
#[test]
fn a_config_hook_that_cant_be_proven_is_refused_and_changes_nothing() {
    let env = Env::new();
    let (repo, _wts, stacks) = post_checkout_repo(&env);
    forwards_to_tracked_hooks(&env, &repo);
    owners_note(&repo);
    fs::write(repo.join(".git/hooks.inc"), "[hook \"lords-post-checkout\"]\n\tenabled = false\n")
        .unwrap();
    let hooks = repo.join(".git/hooks");
    let ledger = env.dir.path().join("home/.lords-command/state/ledger.toml");
    let state = || (listing(&hooks), local_config(&repo), fs::read(&ledger).ok());
    let cases = [
        ("hook.post-checkout.event", "post-checkout", "nor a config hook: `git hook list"),
        ("hook.lords-post-checkout.command", "echo mine", "set already (local"),
        ("includeIf.gitdir:**/worktrees/.path", "hooks.inc", "includeif.gitdir"),
        ("hook.post-checkout.enabled", "false", "nor a config hook: `git hook run"),
    ];
    for (key, value, says) in cases {
        env.git(&repo, &["config", key, value]);
        let before = state();
        let o = install_post_checkout(&env, &repo, &stacks);
        assert!(!o.status.success() && text(&o).contains(says), "{key}: {}", text(&o));
        assert_eq!(state(), before, "{key}: as it was");
        env.git(&repo, &["config", "--unset", key]);
    }

    assert!(install_post_checkout(&env, &repo, &stacks).status.success());
    env.git(&repo, &["config", "hook.post-checkout.enabled", "false"]);
    let before = state();
    let o = install_post_checkout(&env, &repo, &stacks);
    assert!(!o.status.success() && !text(&o).contains("differs"), "{}", text(&o));
    assert_eq!(state(), before, "{}", text(&o));
    env.git(&repo, &["config", "--unset", "hook.post-checkout.enabled"]);
    #[cfg(windows)]
    {
        let before = state();
        let o = Command::new("compact").args(["/c", "/q"]).arg(&ledger).output().unwrap();
        assert!(o.status.success(), "{}", text(&o));
        let o = install_post_checkout(&env, &repo, &stacks);
        assert!(!o.status.success(), "{}", text(&o));
        assert_eq!(state(), before, "{}", text(&o));
    }
}

// Spec (review r1, D14b-R1-01, accepted by the Lords owner, 2 Nov: "a config-hook uninstall is all-or-nothing, just like
// install. If the ledger save fails, compression refusal included, Lords' two keys go back exactly as they were (owner
// sections and comments included), and the ledger stays unchanged"; the review's witness): with only a config hook
// recorded and an owner's note in its section, an uninstall whose ledger save is refused (the ledger NTFS-compressed)
// fails, and the local config, the ledger and the hooks dir are byte for byte as before. A guard (it passes before the
// fix too): one whose key removal fails (git's own `config.lock` held) keeps the entry and its record. Windows only, as
// for install's save refusal.
#[cfg(windows)]
#[test]
fn a_config_hook_uninstall_whose_ledger_cant_be_saved_changes_nothing() {
    let env = Env::new();
    let (repo, _wts, stacks) = post_checkout_repo(&env);
    forwards_to_tracked_hooks(&env, &repo);
    owners_note(&repo);
    let o = install_post_checkout(&env, &repo, &stacks);
    assert!(o.status.success(), "{}", text(&o));
    let hooks = repo.join(".git/hooks");
    let ledger = env.dir.path().join("home/.lords-command/state/ledger.toml");
    let state = || (listing(&hooks), local_config(&repo), fs::read(&ledger).unwrap());
    let before = state();
    let mut uninstall = vec!["uninstall", "--repo", "r", "--apply"];
    uninstall.extend(stacks.iter().map(String::as_str));

    fs::write(repo.join(".git/config.lock"), "").unwrap();
    let o = env.lords(&repo, &uninstall);
    assert!(text(&o).contains("can't remove it"), "{}", text(&o));
    assert_eq!(state(), before, "{}", text(&o));
    fs::remove_file(repo.join(".git/config.lock")).unwrap();
    let o = Command::new("compact").args(["/c", "/q"]).arg(&ledger).output().unwrap();
    assert!(o.status.success(), "{}", text(&o));
    let o = env.lords(&repo, &uninstall);
    assert!(!o.status.success(), "{}", text(&o));
    assert_eq!(state(), before, "{}", text(&o));
}

/// A nonce as `lords check` makes them: 32 hex digits.
const NONCE: &str = "0123456789abcdef0123456789abcdef";

fn hook_run(env: &Env, repo: &Path, hook: &str, probe: &str, args: &[&str]) -> Output {
    let mut c = env.cmd("git", repo);
    c.args(["hook", "run", "--ignore-missing", hook]);
    if !args.is_empty() {
        c.arg("--").args(args);
    }
    c.env("LORDS_HOOK_PROBE", probe).output().unwrap()
}

// Spec (design review, point 1: "The hook prints the token and exits early only when `LORDS_HOOK_PROBE` equals a
// nonce whose file exists. Anything else → the normal check, unchanged. The test: a stale or invented value with no
// file doesn't switch the check off"): with an invented nonce and no file, the pre-commit hook still refuses a
// staged leak.
#[test]
fn an_invented_probe_value_doesnt_switch_the_check_off() {
    let env = Env::new();
    let repo = env.repo();
    assert!(env.lords(&repo, &["hooks", "install"]).status.success());
    fs::write(repo.join("notes.md"), format!("logs in C:\\{}\\alice\\logs\n", "Users")).unwrap();
    env.git(&repo, &["add", "notes.md"]);
    for probe in [NONCE, "", "../../etc", "yes"] {
        let o = hook_run(&env, &repo, "pre-commit", probe, &[]);
        assert!(!o.status.success() && text(&o).contains("path"), "{probe:?}: {}", text(&o));
        assert!(!text(&o).contains("lords-hook-probe"), "{probe:?}: {}", text(&o));
    }
}

// Spec (design, rule D: "A hook prints `lords-hook-probe:<nonce>:<hook>` and exits 0 only when `LORDS_HOOK_PROBE`
// is a nonce and `<home>/state/probe-<nonce>` exists"): with the nonce's file there, both hooks answer the probe
// and check nothing.
#[test]
fn a_probe_with_its_nonce_file_is_answered() {
    let env = Env::new();
    let repo = env.repo();
    assert!(env.lords(&repo, &["hooks", "install"]).status.success());
    let state = env.dir.path().join("home").join(".lords-command").join("state");
    fs::create_dir_all(&state).unwrap();
    fs::write(state.join(format!("probe-{NONCE}")), "").unwrap();
    let msg = env.dir.path().join("probe-message");
    fs::write(&msg, "a probe\n").unwrap();
    let pre = hook_run(&env, &repo, "pre-commit", NONCE, &[]);
    assert!(
        pre.status.success()
            && text(&pre).contains(&format!("lords-hook-probe:{NONCE}:pre-commit")),
        "{}",
        text(&pre)
    );
    let m = hook_run(&env, &repo, "commit-msg", NONCE, &[msg.to_str().unwrap()]);
    assert!(
        m.status.success() && text(&m).contains(&format!("lords-hook-probe:{NONCE}:commit-msg")),
        "{}",
        text(&m)
    );
}

/// A public stack repo (`pubrepo/stack`) and a private stack repo (`priv`), each with one commit; the private stack
/// declares a user-level file sync hasn't written yet, so the full check reports drift.
fn stack_repos(env: &Env) -> (PathBuf, PathBuf) {
    let d = env.dir.path();
    let (public, private) = (d.join("pubrepo"), d.join("priv"));
    for p in [&public.join("stack"), &private.join("global/rules")] {
        fs::create_dir_all(p).unwrap();
    }
    fs::write(public.join("stack/stack.toml"), "name = \"pub\"\nvisibility = \"public\"\n")
        .unwrap();
    fs::write(
        private.join("stack.toml"),
        "name = \"priv\"\nvisibility = \"private\"\n[[targets]]\nname = \"user\"\nadapter = \"claude\"\nkind = \"instructions\"\npath = \"~/.claude/CLAUDE.md\"\n",
    )
    .unwrap();
    for r in [&public, &private] {
        assert!(env.git(r, &["init", "-q"]).status.success());
        env.git(r, &["add", "-A"]);
        assert!(env.git(r, &["commit", "-q", "-m", "start"]).status.success());
    }
    (public, private)
}

/// A temporary rule, with or without the review date its schema requires.
fn temporary_rule(review_by: bool) -> String {
    format!(
        "---\nid: rule.t\nkind: rule\ntitle: T\naudience: [all]\nscope: global\nstatus: active\nvisibility: private\ntemporary:\n  until: 2099-01-01\n{}  reason: a test\norigin:\n  type: own\n---\nA rule for a while.\n",
        if review_by { "  review_by: 2099-01-01\n" } else { "" }
    )
}

fn commits(env: &Env, repo: &Path) -> usize {
    String::from_utf8_lossy(&env.git(repo, &["rev-list", "--count", "HEAD"]).stdout)
        .trim()
        .parse()
        .unwrap()
}

// Spec (orchestrator, 27 Oct: "The install refuses to write any place's file that would exceed its provider's hard
// limit (Codex 32,768 bytes). It names the place, the size and the largest sections, and leaves the old file in
// place"; past it, Codex cuts the file's end without a word): a workspace's Codex file under the limit is written;
// once a rule grows it past the limit, the install leaves the old file as it was and says why.
#[test]
fn the_install_never_writes_a_file_past_its_harness_limit() {
    let env = Env::new();
    let d = env.dir.path();
    let (public, private, work) = (d.join("pub"), d.join("priv"), d.join("work"));
    for p in [&public, &private.join("global/rules"), &work] {
        fs::create_dir_all(p).unwrap();
    }
    fs::write(public.join("stack.toml"), "name = \"pub\"\nvisibility = \"public\"\n").unwrap();
    fs::write(
        private.join("stack.toml"),
        format!(
            "name = \"priv\"\nvisibility = \"private\"\n[workspaces.work]\npath = '{}'\nadapters = [\"codex\"]\n",
            work.display()
        ),
    )
    .unwrap();
    let rule = |body: &str| {
        format!("---\nid: rule.big\nkind: rule\ntitle: Big\naudience: [all]\nscope: workspace:work\nstatus: active\nvisibility: private\norigin:\n  type: own\n---\n{body}\n")
    };
    fs::write(private.join("global/rules/big.md"), rule("A short rule.")).unwrap();
    assert!(env.git(&private, &["init", "-q"]).status.success());
    env.git(&private, &["add", "-A"]);
    env.git(&private, &["commit", "-q", "-m", "start"]);
    let plan = [
        "sync",
        "--stage",
        "install",
        "--public-stack",
        public.to_str().unwrap(),
        "--private-stack",
        private.to_str().unwrap(),
    ];
    let args = [&plan[..], &["--apply"]].concat();
    let o = env.lords(d, &args);
    assert!(o.status.success(), "{}", text(&o));
    let file = work.join("AGENTS.override.md");
    let before = fs::read_to_string(&file).unwrap();
    assert!(before.contains("A short rule."), "{before}\n{}", text(&o));

    let long = "Each line of this rule is long enough to count. ".repeat(700);
    fs::write(private.join("global/rules/big.md"), rule(&long)).unwrap();
    env.git(&private, &["commit", "-q", "-am", "a long rule"]);
    let o = env.lords(d, &args);
    let out = text(&o);
    assert_eq!(fs::read_to_string(&file).unwrap(), before, "{out}");
    assert!(out.contains("32768") && out.contains("Rules"), "{out}");
    // A skipped place never reads as success, neither the install nor the fresh plan that verifies it.
    assert!(!o.status.success(), "{out}");
    let fresh = env.lords(d, &plan);
    assert!(!fresh.status.success(), "{}", text(&fresh));
}

// Spec (Codex's source, openai/codex 0d68383: `project_doc_max_bytes` bounds only the project's AGENTS files,
// codex-rs/core/src/agents_md.rs:68-92 and 142-178, while `~/.codex/AGENTS.md` is read whole outside it,
// codex-rs/codex-home/src/instructions/mod.rs:41-78): a user-level Codex file past 32 KiB is no conflict; it is
// written, and the install succeeds.
#[test]
fn a_user_level_codex_file_has_no_project_limit() {
    let env = Env::new();
    let d = env.dir.path();
    let (public, private) = (d.join("pub"), d.join("priv"));
    fs::create_dir_all(&public).unwrap();
    fs::create_dir_all(private.join("global/rules")).unwrap();
    fs::write(public.join("stack.toml"), "name = \"pub\"\nvisibility = \"public\"\n").unwrap();
    fs::write(
        private.join("stack.toml"),
        "name = \"priv\"\nvisibility = \"private\"\n[[targets]]\nname = \"codex user\"\nadapter = \"codex\"\nkind = \"instructions\"\npath = \"~/.codex/AGENTS.md\"\n",
    )
    .unwrap();
    let long = "Each line of this rule is long enough to count. ".repeat(700);
    fs::write(
        private.join("global/rules/big.md"),
        format!("---\nid: rule.big\nkind: rule\ntitle: Big\naudience: [all]\nscope: global\nstatus: active\nvisibility: private\norigin:\n  type: own\n---\n{long}\n"),
    )
    .unwrap();
    assert!(env.git(&private, &["init", "-q"]).status.success());
    env.git(&private, &["add", "-A"]);
    env.git(&private, &["commit", "-q", "-m", "start"]);
    let o = env.lords(
        d,
        &[
            "sync",
            "--user",
            "--apply",
            "--public-stack",
            public.to_str().unwrap(),
            "--private-stack",
            private.to_str().unwrap(),
        ],
    );
    assert!(o.status.success(), "{}", text(&o));
    let written = fs::read_to_string(d.join("home/.codex/AGENTS.md")).unwrap();
    assert!(written.len() > 32 * 1024 && written.contains("long enough"), "{}", text(&o));
}

/// `lords guard` as Claude Code runs it: the event on stdin; its output and exit code.
fn guard(env: &Env, exe: &Path, args: &[&str], stdin: &str) -> Output {
    use std::io::Write;
    let mut c = env
        .cmd(exe.to_str().unwrap(), env.dir.path())
        .args(args)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    c.stdin.take().unwrap().write_all(stdin.as_bytes()).unwrap();
    c.wait_with_output().unwrap()
}

// Spec (orchestrator, 30 Oct): "The guard should exit 2 only on a deliberate refusal: any internal error or
// unreadable input fails open with exit 0 and a stderr note" (Claude Code takes exit 2 from a PreToolUse hook as a
// block of the call). Usage errors and unreadable input: exit 0, nothing on stdout; the refusal: exit 0 with the
// denial; the self-test: `ok`.
#[test]
fn the_guard_fails_open_on_anything_but_a_refusal() {
    let env = Env::new();
    let exe = Path::new(env!("CARGO_BIN_EXE_lords"));
    let event = r#"{"tool_name":"Bash","tool_input":{"command":"rm -f $OUT/x"}}"#;
    for args in [&["guard"][..], &["guard", "nope"], &["guard", "rm", "extra"], &["guard", "--x"]] {
        let o = guard(&env, exe, args, event);
        assert_eq!(o.status.code(), Some(0), "{args:?}: {}", text(&o));
        assert!(o.stdout.is_empty() && !o.stderr.is_empty(), "{args:?}: {}", text(&o));
    }
    for input in ["", "not json", "{\"tool_name\": 5}"] {
        let o = guard(&env, exe, &["guard", "rm"], input);
        assert_eq!(o.status.code(), Some(0), "{input}: {}", text(&o));
        assert!(o.stdout.is_empty(), "{input}: {}", text(&o));
    }
    let o = guard(&env, exe, &["guard", "rm"], event);
    assert_eq!(o.status.code(), Some(0));
    assert!(String::from_utf8_lossy(&o.stdout).contains("\"deny\""), "{}", text(&o));
    let o = guard(&env, exe, &["guard", "--self-test"], "");
    assert_eq!((o.status.code(), String::from_utf8_lossy(&o.stdout).trim()), (Some(0), "ok"));
}

// Spec: rm-guard review r1, R8 (witness: `lords guard nope` with its stderr reader closed, and `lords guard rm` with
// its stdout reader closed, exited 101 on a print panic): the guard fails open, exit 0, when it can't write.
#[test]
fn the_guard_fails_open_when_it_cant_write() {
    use std::io::Write;
    let env = Env::new();
    let exe = env!("CARGO_BIN_EXE_lords");
    let mut c = env.cmd(exe, env.dir.path());
    let mut child = c
        .args(["guard", "nope"])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    drop(child.stderr.take());
    drop(child.stdin.take());
    assert_eq!(child.wait().unwrap().code(), Some(0), "a closed diagnostic pipe");
    let mut c = env.cmd(exe, env.dir.path());
    let mut child = c
        .args(["guard", "rm"])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    drop(child.stdout.take());
    let event = r#"{"tool_name":"Bash","tool_input":{"command":"rm -f $OUT/x"}}"#;
    child.stdin.take().unwrap().write_all(event.as_bytes()).unwrap();
    assert_eq!(child.wait().unwrap().code(), Some(0), "a closed response pipe");
    // rm-guard fix check r2, R8: the guard's help, with its stdout closed.
    for args in [&["guard", "--help"][..], &["guard", "rm", "-h"]] {
        let mut c = env.cmd(exe, env.dir.path());
        let mut child = c
            .args(args)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        drop(child.stdout.take());
        assert_eq!(child.wait().unwrap().code(), Some(0), "{args:?} with a closed stdout");
    }
}

// Spec: rm-guard fix check r5, R10 (witness: `lords guard install --r5-invalid-option` with its stderr reader closed
// exited 101 on a print panic): `guard install` keeps its exit codes when it can't write, 2 for a refusal and 0 for an
// install, never a panic.
#[test]
fn guard_install_keeps_its_exit_code_when_it_cant_write() {
    let env = Env::new();
    let exe = env!("CARGO_BIN_EXE_lords");
    let home = env.dir.path().join("tool-home");
    let home = home.to_str().unwrap();
    for (args, closed_stdout, code) in [
        (&["guard", "install", "--r5-invalid-option"][..], false, 2),
        (&["guard", "install", "--home", home][..], true, 0),
    ] {
        let mut c = env.cmd(exe, env.dir.path());
        let mut child = c
            .args(args)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        if closed_stdout {
            drop(child.stdout.take());
        } else {
            drop(child.stderr.take());
        }
        assert_eq!(child.wait().unwrap().code(), Some(code), "{args:?}");
    }
}

/// A user-level fixture for the guard and stale-plan tests: a settings target with the rm guard, a skills target, and
/// three `lords` builds told apart by a trailing byte (A: none, B, C).
struct Stale {
    dir: TempDir,
}

impl Stale {
    fn new() -> Stale {
        let s = Stale { dir: TempDir::new().unwrap() };
        s.write("pub/stack.toml", b"name = \"pub\"\nvisibility = \"public\"\n");
        s.skill("a");
        s.write("home/settings.json", b"{}\n");
        let r = s.dir.path();
        let targets = format!(
            "name = \"priv\"\nvisibility = \"private\"\n[[targets]]\nname = \"settings\"\nadapter = \"claude\"\nkind = \"settings\"\npath = '{}'\nguards = [\"rm\"]\n[[targets]]\nname = \"skills\"\nadapter = \"claude\"\nkind = \"skills\"\npath = '{}'\n",
            r.join("home/settings.json").display(),
            r.join("home/skills").display()
        );
        s.write("priv/stack.toml", targets.as_bytes());
        let lords = fs::read(env!("CARGO_BIN_EXE_lords")).unwrap();
        s.write("a/lords.exe", &lords);
        s.write("b/lords.exe", &[&lords[..], b"B"].concat());
        s.write("c/lords.exe", &[&lords[..], b"C"].concat());
        s
    }

    fn write(&self, rel: &str, text: &[u8]) {
        let p = self.dir.path().join(rel);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, text).unwrap();
    }

    fn skill(&self, name: &str) {
        self.write(
            &format!("pub/global/skills/{name}/SKILL.md"),
            format!("---\nname: {name}\n---\nX\n").as_bytes(),
        );
        self.write(
            &format!("pub/global/skills/{name}/lords.yaml"),
            format!("id: skill.{name}\nkind: skill\ntitle: {name}\naudience: [all]\nscope: global\nstatus: active\nvisibility: public\norigin:\n  type: own\n").as_bytes(),
        );
    }

    fn home(&self) -> PathBuf {
        self.dir.path().join("tool")
    }

    fn install(&self, build: &str) -> Result<bool, String> {
        lords_core::sync::install_guard(
            &self.home(),
            &self.dir.path().join(build).join("lords.exe"),
        )
    }

    fn plan(&self) -> lords_core::sync::Plan {
        let r = self.dir.path();
        let stacks = lords_core::Stacks::load(&r.join("pub"), Some(&r.join("priv"))).unwrap();
        let opts = lords_core::sync::Options {
            user_only: true,
            home: self.home(),
            ..lords_core::sync::Options::default()
        };
        lords_core::sync::plan(&stacks, &opts).unwrap()
    }

    /// The guard copy's last byte, the settings' bytes and the ledger's bytes.
    fn state(&self) -> (Option<u8>, Vec<u8>, Option<Vec<u8>>) {
        let r = self.dir.path();
        let copy =
            fs::read(r.join("tool/bin/lords-guard.exe")).ok().and_then(|b| b.last().copied());
        let ledger = fs::read(r.join("tool/state/ledger.toml")).ok();
        (copy, fs::read(r.join("home/settings.json")).unwrap(), ledger)
    }
}

// Spec (orchestrator, 30 Oct, after the rm-guard fix check r4: "Take guard installation out of `sync apply`
// altogether … Sync never writes, replaces or deletes the guard copy"): no apply, of a fresh plan or a stale one,
// changes the guard copy's bytes; only `lords guard install` does.
#[test]
fn no_apply_changes_the_guard_copy() {
    let s = Stale::new();
    assert!(s.install("b").unwrap());
    let plan_with_b = s.plan();
    assert!(s.install("c").unwrap());
    lords_core::sync::apply(&plan_with_b).unwrap();
    assert_eq!(
        s.state().0,
        Some(b'C'),
        "a plan made with B installed applied with C installed: C stays"
    );
    lords_core::sync::apply(&s.plan()).unwrap();
    assert_eq!(s.state().0, Some(b'C'));
    assert!(!s.install("c").unwrap(), "already current");
}

// Spec (as above, the stale cases; rm-guard fix checks r2, r3, R7): a stale plan is refused before it changes anything:
// its ledger changed, a settings file changed, or a tool destination was occupied; the guard copy, the settings and
// the ledger stay as they were.
#[test]
fn a_stale_plan_changes_nothing() {
    // The ledger changed.
    let s = Stale::new();
    s.install("b").unwrap();
    let stale = s.plan();
    s.skill("b");
    lords_core::sync::apply(&s.plan()).unwrap();
    let before = s.state();
    assert!(lords_core::sync::apply(&stale).is_err(), "a stale ledger");
    assert_eq!(s.state(), before);
    // A settings file changed.
    let s = Stale::new();
    s.install("b").unwrap();
    let stale = s.plan();
    s.write("home/settings.json", b"{\"model\":\"x\"}\n");
    let before = s.state();
    assert!(lords_core::sync::apply(&stale).is_err(), "stale settings");
    assert_eq!(s.state(), before);
    // A tool destination was occupied (r3's witness).
    let s = Stale::new();
    s.install("b").unwrap();
    lords_core::sync::apply(&s.plan()).unwrap();
    s.skill("b");
    let stale = s.plan();
    s.write("home/skills/b", b"not the tool's\n");
    let before = s.state();
    assert!(lords_core::sync::apply(&stale).is_err(), "an occupied destination");
    assert_eq!(s.state(), before);
}

// Spec (orchestrator, 30 Oct: "`lords guard install` installs or updates the stable copy under its own lock. It
// copies to a staged path, self-tests it, and renames it into place atomically"; "sync with no copy installed writes no
// hook and says why"): before an install, sync leaves the hook out and names the fix; `lords guard install` puts the
// copy there, and sync then names it in the settings; a staged copy an interrupted install left doesn't stop the next.
#[test]
fn sync_names_the_guard_only_once_it_is_installed() {
    let env = Env::new();
    let d = env.dir.path();
    let (public, private) = (d.join("pub"), d.join("priv"));
    fs::create_dir_all(&public).unwrap();
    fs::create_dir_all(&private).unwrap();
    fs::create_dir_all(d.join("home/.claude-balanced")).unwrap();
    fs::write(d.join("home/.claude-balanced/settings.json"), "{\n  \"model\": \"x\"\n}\n").unwrap();
    fs::write(public.join("stack.toml"), "name = \"pub\"\nvisibility = \"public\"\n").unwrap();
    fs::write(
        private.join("stack.toml"),
        "name = \"priv\"\nvisibility = \"private\"\n[[targets]]\nname = \"balanced settings\"\nadapter = \"claude\"\nkind = \"settings\"\npath = \"~/.claude-balanced/settings.json\"\nguards = [\"rm\"]\n",
    )
    .unwrap();
    assert!(env.git(&private, &["init", "-q"]).status.success());
    env.git(&private, &["add", "-A"]);
    env.git(&private, &["commit", "-q", "-m", "start"]);
    let stacks =
        ["--public-stack", public.to_str().unwrap(), "--private-stack", private.to_str().unwrap()];
    let sync = |apply: bool| {
        let mut args = vec!["sync", "--user"];
        if apply {
            args.push("--apply");
        }
        env.lords(d, &[&args[..], &stacks].concat())
    };
    let settings = || fs::read_to_string(d.join("home/.claude-balanced/settings.json")).unwrap();
    // Not installed: no hook, and the plan says why.
    let o = sync(false);
    assert!(text(&o).contains("guard not installed: run `lords guard install`"), "{}", text(&o));
    assert!(sync(true).status.success());
    assert!(!settings().contains("lords-guard"), "{}", settings());
    // A staged copy an interrupted install left behind.
    let copy = d.join("home/.lords-command/bin/lords-guard.exe");
    fs::create_dir_all(copy.parent().unwrap()).unwrap();
    fs::write(copy.with_file_name(".lords-guard.exe.exe"), b"cut").unwrap();
    let o = env.lords(d, &["guard", "install"]);
    assert!(o.status.success() && copy.is_file(), "{}", text(&o));
    let o = sync(true);
    assert!(o.status.success(), "{}", text(&o));
    let v: serde_json::Value = serde_json::from_str(&settings()).unwrap();
    let h = &v["hooks"]["PreToolUse"][0]["hooks"][0];
    let named = h["command"].as_str().unwrap().replace('\\', "/").to_lowercase();
    assert_eq!(named, copy.display().to_string().replace('\\', "/").to_lowercase(), "{h}");
    let event = r#"{"tool_name":"Bash","tool_input":{"command":"rm -f $OUT/x"}}"#;
    let o = guard(&env, &copy, &["guard", "rm"], event);
    assert!(String::from_utf8_lossy(&o.stdout).contains("\"deny\""), "{}", text(&o));
    // A check of the synced settings reports no conflict for them.
    let out = text(&env.lords(d, &[&["check"][..], &stacks].concat()));
    assert!(!out.contains("settings.json"), "{out}");
    // The copy gone: sync takes the hook out and says why; it never puts a copy back.
    fs::remove_file(&copy).unwrap();
    let o = sync(true);
    assert!(o.status.success(), "{}", text(&o));
    assert!(!settings().contains("lords-guard") && !copy.exists(), "{}", settings());
}

// Spec (orchestrator, 30 Oct: "a one-line 'applies: yes/no (why)' in the `lords sync` dry-run output itself", with
// what it releases; fix check r2, R2-03: a forget drops a record, and an owner-edited copy it leaves is still there):
// the dry run says whether its plan applies, and why not, in one line; what it forgets is counted as records, never
// called gone.
#[test]
fn a_sync_dry_run_says_whether_it_applies() {
    let env = Env::new();
    let d = env.dir.path();
    let (public, private) = (d.join("pub"), d.join("priv"));
    fs::create_dir_all(&public).unwrap();
    fs::create_dir_all(&private).unwrap();
    fs::create_dir_all(d.join("home/out")).unwrap();
    fs::write(public.join("stack.toml"), "name = \"pub\"\nvisibility = \"public\"\n").unwrap();
    let skill = |name: &str| {
        fs::create_dir_all(public.join(format!("global/skills/{name}"))).unwrap();
        fs::write(
            public.join(format!("global/skills/{name}/SKILL.md")),
            format!("---\nname: {name}\n---\nX\n"),
        )
        .unwrap();
        fs::write(
            public.join(format!("global/skills/{name}/lords.yaml")),
            format!("id: skill.{name}\nkind: skill\ntitle: {name}\naudience: [all]\nscope: global\nstatus: active\nvisibility: public\norigin:\n  type: own\n"),
        )
        .unwrap();
    };
    skill("a");
    fs::write(
        private.join("stack.toml"),
        "name = \"priv\"\nvisibility = \"private\"\n[[targets]]\nname = \"skills\"\nadapter = \"claude\"\nkind = \"skills\"\npath = \"~/out/skills\"\n",
    )
    .unwrap();
    let stacks =
        ["--public-stack", public.to_str().unwrap(), "--private-stack", private.to_str().unwrap()];
    let sync =
        |more: &[&str]| text(&env.lords(d, &[&["sync", "--user"][..], more, &stacks].concat()));
    let line = |out: &str| {
        out.lines().find(|l| l.starts_with("applies: ")).unwrap_or_default().to_string()
    };
    let out = sync(&[]);
    assert!(line(&out).starts_with("applies: yes"), "{out}");
    // An owner-edited copy that is no longer wanted: its record is forgotten, the copy stays.
    sync(&["--apply"]);
    fs::write(d.join("home/out/skills/a/SKILL.md"), "OWNER EDIT\n").unwrap();
    fs::remove_dir_all(public.join("global/skills/a")).unwrap();
    let out = sync(&[]);
    let l = line(&out);
    assert!(l.contains("forgets 1 record(s)") && !l.contains("gone"), "{out}");
    // A skill copy that isn't the tool's (no ledger record) where one installs: a conflict there, and the plan doesn't
    // apply.
    skill("b");
    fs::create_dir_all(d.join("home/out/skills/b")).unwrap();
    fs::write(d.join("home/out/skills/b/SKILL.md"), "mine\n").unwrap();
    let out = sync(&[]);
    let l = line(&out);
    assert!(l.starts_with("applies: no") && l.contains("target skills"), "{out}");
}

// Spec (Sync perf review r1, R1, witness: `GIT_LITERAL_PATHSPECS=1` inherited made git read Lords' `:(icase)` pathspec
// as a literal name, so a tracked destination looked untracked): a destination git tracks is a conflict whatever
// pathspec mode Lords inherits. Both witnesses: a tracked file deleted from the working tree, and a ledger-owned
// generated file force-added to the index.
#[test]
fn an_inherited_pathspec_mode_never_hides_a_tracked_destination() {
    let env = Env::new();
    let d = env.dir.path();
    let (public, private, checkout) = (d.join("pub"), d.join("priv"), d.join("checkout"));
    for dir in [&public, &private.join("repo/beta-core/facts"), &checkout] {
        fs::create_dir_all(dir).unwrap();
    }
    fs::write(public.join("stack.toml"), "name = \"pub\"\nvisibility = \"public\"\n").unwrap();
    let fact = |body: &str| {
        format!("---\nid: fact.h\nkind: fact\ntitle: h\naudience: [all]\nscope: repo:beta-core\nstatus: active\nvisibility: private\norigin:\n  type: own\n---\n{body}\n")
    };
    fs::write(private.join("repo/beta-core/facts/h.md"), fact("Hosts.")).unwrap();
    fs::write(
        private.join("stack.toml"),
        format!(
            "name = \"priv\"\nvisibility = \"private\"\n[projects.beta]\n[repos.beta-core]\nproject = \"beta\"\ncheckouts = ['{}']\nworktrees = false\n",
            checkout.display()
        ),
    )
    .unwrap();
    for (dir, files) in [(&private, "-A"), (&checkout, "-A")] {
        if dir == &checkout {
            fs::write(checkout.join("README.md"), "x\n").unwrap();
        }
        assert!(env.git(dir, &["init", "-q"]).status.success());
        env.git(dir, &["add", files]);
        env.git(dir, &["commit", "-q", "-m", "start"]);
    }
    let stacks =
        ["--public-stack", public.to_str().unwrap(), "--private-stack", private.to_str().unwrap()];
    let literal = |args: &[&str]| {
        let mut c = env.cmd(env!("CARGO_BIN_EXE_lords"), d);
        c.env("GIT_LITERAL_PATHSPECS", "1").args(args).args(stacks).output().unwrap()
    };
    let action_of = |o: &Output, name: &str| -> String {
        let v: serde_json::Value =
            serde_json::from_slice(&o.stdout).unwrap_or_else(|_| panic!("{}", text(o)));
        v["items"]
            .as_array()
            .unwrap()
            .iter()
            .find(|i| i["path"].as_str().is_some_and(|p| p.ends_with(name)))
            .map(|i| i["action"].as_str().unwrap().to_string())
            .unwrap_or_default()
    };
    // A ledger-owned generated file, then force-added to the index, then its composition changed.
    let o = env.lords(d, &[&["sync", "--repo", "beta-core", "--apply"][..], &stacks].concat());
    assert!(o.status.success(), "{}", text(&o));
    env.git(&checkout, &["add", "-f", "CLAUDE.local.md"]);
    fs::write(private.join("repo/beta-core/facts/h.md"), fact("Hosts, changed.")).unwrap();
    env.git(&private, &["commit", "-qam", "changed"]);
    let o = literal(&["sync", "--repo", "beta-core", "--json"]);
    assert_eq!(action_of(&o, "CLAUDE.local.md"), "conflict", "{}", text(&o));
    // A tracked file deleted from the working tree.
    fs::remove_file(checkout.join("CLAUDE.local.md")).unwrap();
    let o = literal(&["sync", "--repo", "beta-core", "--json"]);
    assert_eq!(action_of(&o, "CLAUDE.local.md"), "conflict", "{}", text(&o));
}

// Spec (orchestrator, 27 Oct: `lords check --stack` is "one flag that skips the drift part", run by a stack repo's
// pre-commit, where the hook probe can't run): it fails on a broken entry and passes once that is fixed, and
// reports neither drift nor the hook probe, which the full check reports for the same stacks.
#[test]
fn check_stack_checks_the_entries_without_drift_or_the_hook_probe() {
    let env = Env::new();
    let (public, private) = stack_repos(&env);
    let (pubs, privs) = (public.join("stack"), private.clone());
    let stacks =
        ["--public-stack", pubs.to_str().unwrap(), "--private-stack", privs.to_str().unwrap()];
    let check = |extra: &[&str]| env.lords(&private, &[&["check"][..], extra, &stacks].concat());

    fs::write(private.join("global/rules/t.md"), temporary_rule(false)).unwrap();
    let o = check(&["--stack"]);
    assert!(!o.status.success() && text(&o).contains("t.md"), "{}", text(&o));

    fs::write(private.join("global/rules/t.md"), temporary_rule(true)).unwrap();
    let o = check(&["--stack"]);
    let out = text(&o);
    assert!(o.status.success(), "{out}");
    assert!(!out.contains("drift") && !out.contains("privacy-hooks"), "{out}");
    let full = text(&check(&[]));
    assert!(full.contains("drift") && full.contains("privacy-hooks"), "{full}");
}

// Spec (orchestrator, 27 Oct: "`lords check` as the stack repos' pre-commit hook … full-history secrets scan as
// the private stack's pre-push hook"): in a private stack repo, `hooks install --secrets-only` refuses a commit of a
// broken entry, even when config.txt names another private stack, and lets the fixed one through; its pre-push
// passes a clean history and refuses one holding a secret. A repo with no stack gets neither check.
#[test]
fn a_private_stack_repo_checks_its_commits_and_pushes_only_a_clean_history() {
    let env = Env::new();
    let (public, private) = stack_repos(&env);
    let other = env.dir.path().join("other-priv");
    fs::create_dir_all(&other).unwrap();
    fs::write(other.join("stack.toml"), "name = \"other\"\nvisibility = \"private\"\n").unwrap();
    let cfg = env.dir.path().join("home/.lords-command");
    fs::create_dir_all(&cfg).unwrap();
    fs::write(
        cfg.join("config.txt"),
        format!("public = {}\nprivate = {}\n", public.join("stack").display(), other.display()),
    )
    .unwrap();
    let o = env.lords(&private, &["hooks", "install", "--secrets-only"]);
    assert!(o.status.success(), "{}", text(&o));
    assert!(private.join(".git/hooks/pre-push").is_file());

    fs::write(private.join("global/rules/t.md"), temporary_rule(false)).unwrap();
    env.git(&private, &["add", "-A"]);
    let o = env.git(&private, &["commit", "-q", "-m", "a broken rule"]);
    assert!(!o.status.success() && text(&o).contains("t.md"), "{}", text(&o));
    assert_eq!(commits(&env, &private), 1);
    fs::write(private.join("global/rules/t.md"), temporary_rule(true)).unwrap();
    env.git(&private, &["add", "-A"]);
    let o = env.git(&private, &["commit", "-q", "-m", "the fixed rule"]);
    assert!(o.status.success(), "{}", text(&o));

    let push = || env.git(&private, &["hook", "run", "pre-push", "--", "origin", "unused"]);
    let o = push();
    assert!(o.status.success(), "{}", text(&o));
    // A commit no hook ran for (made elsewhere and fetched), holding a token built here from its parts.
    let token = ["ghp", "_", &"a1".repeat(18)].concat();
    fs::write(private.join("notes.md"), format!("the token {token}\n")).unwrap();
    env.git(&private, &["add", "notes.md"]);
    let tree =
        String::from_utf8_lossy(&env.git(&private, &["write-tree"]).stdout).trim().to_string();
    let planted = env.git(&private, &["commit-tree", &tree, "-p", "HEAD", "-m", "planted"]);
    let planted = String::from_utf8_lossy(&planted.stdout).trim().to_string();
    assert!(env.git(&private, &["update-ref", "HEAD", &planted]).status.success());
    let o = push();
    assert!(!o.status.success() && text(&o).contains("notes.md"), "{}", text(&o));

    let plain = env.repo();
    assert!(env.lords(&plain, &["hooks", "install"]).status.success());
    assert!(!plain.join(".git/hooks/pre-push").exists());
    let pre = fs::read_to_string(plain.join(".git/hooks/pre-commit")).unwrap();
    assert!(!pre.contains("check --stack"), "{pre}");
}

// Spec (orchestrator, 27 Oct: the public stack's pre-push runs `privacy --history` and `check --public`): in a
// public stack repo it passes a clean repo and refuses one whose tree holds a user path.
#[test]
fn a_public_stack_repo_pre_push_runs_the_public_check() {
    let env = Env::new();
    let (public, _) = stack_repos(&env);
    let o = env.lords(&public, &["hooks", "install"]);
    assert!(o.status.success(), "{}", text(&o));
    let push = || env.git(&public, &["hook", "run", "pre-push", "--", "origin", "unused"]);
    let o = push();
    assert!(o.status.success(), "{}", text(&o));
    let sep = std::path::MAIN_SEPARATOR;
    fs::write(public.join("notes.md"), format!("see C:{sep}{}{sep}alice{sep}x\n", "Users"))
        .unwrap();
    let o = push();
    assert!(!o.status.success() && text(&o).contains("notes.md"), "{}", text(&o));
}

// Spec (finding F1, 27 Oct: under a real `git commit` the hook's `check --stack` loaded 219 entries where `git hook
// run` loaded 225, because a hook's `GIT_INDEX_FILE` reached lords' git calls into other repos): what `lords`
// loads from a repo doesn't depend on the git variables of the repo it was started from. A repo's tracked skill is
// an entry with and without a `GIT_INDEX_FILE` naming another repo's index.
#[test]
fn a_hooks_git_variables_dont_change_what_other_repos_hold() {
    let env = Env::new();
    let d = env.dir.path();
    let (public, private, other) = (d.join("pub"), d.join("priv"), d.join("other"));
    let skills = d.join("skilled");
    for p in [&public, &private, &skills.join("skills/tidy"), &other] {
        fs::create_dir_all(p).unwrap();
    }
    fs::write(public.join("stack.toml"), "name = \"pub\"\nvisibility = \"public\"\n").unwrap();
    fs::write(
        private.join("stack.toml"),
        format!(
            "name = \"priv\"\nvisibility = \"private\"\n[repos.skilled]\ncheckouts = ['{}']\nskills = ['skills']\n",
            skills.display()
        ),
    )
    .unwrap();
    fs::write(
        skills.join("skills/tidy/SKILL.md"),
        "---\nname: tidy\ndescription: Tidy up.\n---\nTidy.\n",
    )
    .unwrap();
    for r in [&skills, &other] {
        assert!(env.git(r, &["init", "-q"]).status.success());
    }
    env.git(&skills, &["add", "-A"]);
    fs::write(other.join("x.md"), "x\n").unwrap();
    env.git(&other, &["add", "-A"]);
    let args = [
        "check",
        "--stack",
        "--public-stack",
        public.to_str().unwrap(),
        "--private-stack",
        private.to_str().unwrap(),
    ];
    let loaded = "0 public and 1 private entries";
    let plain = text(&env.lords(d, &args));
    assert!(plain.contains(loaded), "{plain}");
    let index = other.join(".git/index");
    let hooked = text(&env.lords_env(d, &args, &[("GIT_INDEX_FILE", &index)]));
    assert!(hooked.contains(loaded), "{hooked}");
}

// Spec (Privacy: the hook checks "what the commit would contain"): `git commit <path>` commits from a temporary
// index git names in the hook's `GIT_INDEX_FILE`, so the staged scan reads that index. A user path that only the
// commit's own index holds is refused.
#[test]
fn a_commit_of_named_paths_is_scanned_as_it_commits() {
    let env = Env::new();
    let repo = env.repo();
    fs::write(repo.join("notes.md"), "clean\n").unwrap();
    env.git(&repo, &["add", "notes.md"]);
    assert!(env.git(&repo, &["commit", "-q", "-m", "start"]).status.success());
    let o = env.lords(&repo, &["hooks", "install"]);
    assert!(o.status.success(), "{}", text(&o));
    // Changed in the working tree only: `.git/index` still holds the clean text.
    fs::write(repo.join("notes.md"), format!("logs in C:\\{}\\alice\\logs\n", "Users")).unwrap();
    let o = env.git(&repo, &["commit", "-q", "-m", "named", "--", "notes.md"]);
    assert!(!o.status.success() && text(&o).contains("path"), "{}", text(&o));
}

// Spec (orchestrator, 27 Oct, on F1: "one proof that a git call from inside a hook into another repo reads that
// repo's own index"): a real `git commit -a` in a private stack repo runs its pre-commit hook, whose `check --stack`
// loads the skill another repo tracks, as `git hook run` does. `-a` (as `git commit <paths>`) hands the hook an
// absolute `GIT_INDEX_FILE`, which named the stack repo's index for every repo `lords` read.
#[test]
fn a_commit_hook_reads_another_repo_from_its_own_index() {
    let env = Env::new();
    let d = env.dir.path();
    let (public, private, skilled) = (d.join("pub"), d.join("priv"), d.join("skilled"));
    for p in [&public, &private.join("global/rules"), &skilled.join("skills/tidy")] {
        fs::create_dir_all(p).unwrap();
    }
    fs::write(public.join("stack.toml"), "name = \"pub\"\nvisibility = \"public\"\n").unwrap();
    fs::write(
        private.join("stack.toml"),
        format!(
            "name = \"priv\"\nvisibility = \"private\"\n[repos.skilled]\ncheckouts = ['{}']\nskills = ['skills']\n",
            skilled.display()
        ),
    )
    .unwrap();
    fs::write(
        skilled.join("skills/tidy/SKILL.md"),
        "---\nname: tidy\ndescription: Tidy up.\n---\nTidy.\n",
    )
    .unwrap();
    let cfg = d.join("home/.lords-command");
    fs::create_dir_all(&cfg).unwrap();
    fs::write(cfg.join("config.txt"), format!("public = {}\n", public.display())).unwrap();
    for r in [&skilled, &private] {
        assert!(env.git(r, &["init", "-q"]).status.success());
        env.git(r, &["add", "-A"]);
        assert!(env.git(r, &["commit", "-q", "-m", "start"]).status.success());
    }
    let o = env.lords(&private, &["hooks", "install", "--secrets-only"]);
    assert!(o.status.success(), "{}", text(&o));
    fs::write(private.join("global/rules/t.md"), temporary_rule(true)).unwrap();
    env.git(&private, &["add", "-A"]);
    // The skill and the rule: what `lords check` loads outside any hook.
    let loaded = "0 public and 2 private entries";
    let run = env.git(&private, &["hook", "run", "pre-commit"]);
    assert!(text(&run).contains(loaded), "{}", text(&run));
    let commit = env.git(&private, &["commit", "-a", "-m", "a rule"]);
    assert!(commit.status.success() && text(&commit).contains(loaded), "{}", text(&commit));
}

/// `lords guard queue` as Claude Code runs it, for a session of T3 thread `thread:own` whose T3 CLI is `cli` (never
/// the real one: the session running these tests may carry its own).
fn queue_guard(env: &Env, args: &[&str], cli: &Path, event: &str) -> Output {
    use std::io::Write;
    let mut c = env
        .cmd(env!("CARGO_BIN_EXE_lords"), env.dir.path())
        .args(args)
        .env("T3CODE_THREAD_ID", "thread:own")
        .env("T3CODE_CLI_PATH", cli)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    c.stdin.take().unwrap().write_all(event.as_bytes()).unwrap();
    c.wait_with_output().unwrap()
}

// Spec (brief, 30 Oct: the queue guard fails open, as the rm guard does: "on a timeout, an error, no thread id, or no
// CLI, let the stop through with a one-line note"): a usage error or input it can't read is exit 0, nothing on
// stdout and a note on stderr naming the queue guard; the T3 CLI doesn't run.
#[test]
fn the_queue_guard_fails_open_on_usage_and_input() {
    let env = Env::new();
    let cli = env.dir.path().join("t3.cmd");
    let ran = env.dir.path().join("ran.txt");
    fs::write(&cli, format!("@echo off\r\necho ran>\"{}\"\r\n", ran.display())).unwrap();
    for (args, input) in [
        (&["guard", "queue", "extra"][..], "{}"),
        (&["guard", "queue", "--deadline-ms"], "{}"),
        (&["guard", "queue", "--deadline-ms", "soon"], "{}"),
        (&["guard", "queue"], "not json"),
        (&["guard", "queue"], ""),
    ] {
        let o = queue_guard(&env, args, &cli, input);
        assert_eq!(o.status.code(), Some(0), "{args:?} {input}: {}", text(&o));
        assert!(o.stdout.is_empty(), "{args:?} {input}: {}", text(&o));
        assert!(
            String::from_utf8_lossy(&o.stderr).contains("guard queue"),
            "{args:?} {input}: {}",
            text(&o)
        );
    }
    assert!(!ran.exists(), "the CLI never ran");
}

// Spec (brief, 30 Oct: "give the CLI a short deadline, and kill its process tree when the deadline passes
// (rule.stop-processes-by-tree)"): a T3 CLI still running at the deadline, and a process it started, are gone once
// the guard has answered: the CLI's child never gets to write its file. The guard answers soon after the deadline.
#[cfg(windows)]
#[test]
fn the_queue_guard_kills_the_clis_process_tree_at_its_deadline() {
    let env = Env::new();
    let d = env.dir.path();
    let (started, late) = (d.join("started.txt"), d.join("late.txt"));
    assert!(!late.display().to_string().contains(' '), "an unquoted path in the fake CLI");
    let cli = d.join("t3.cmd");
    // The CLI's child (a second cmd) writes `late.txt` after about 3 s.
    fs::write(
        &cli,
        format!(
            "@echo off\r\necho started>\"{}\"\r\ncmd /d /c \"ping -n 4 127.0.0.1 >nul & echo late>{}\"\r\n",
            started.display(),
            late.display()
        ),
    )
    .unwrap();
    let transcript = d.join("session.jsonl");
    fs::write(&transcript, "").unwrap();
    let event = serde_json::json!({"hook_event_name": "Stop", "session_id": "s1", "transcript_path": transcript, "stop_hook_active": false}).to_string();
    let t0 = std::time::Instant::now();
    let o = queue_guard(&env, &["guard", "queue", "--deadline-ms", "1000"], &cli, &event);
    let took = t0.elapsed();
    assert_eq!(o.status.code(), Some(0), "{}", text(&o));
    assert!(o.stdout.is_empty() && text(&o).contains("deadline"), "{}", text(&o));
    assert!(took < std::time::Duration::from_secs(3), "answered after {took:?}");
    assert!(started.exists(), "the CLI ran");
    std::thread::sleep(std::time::Duration::from_secs(5));
    assert!(!late.exists(), "the CLI's child outlived the guard");
}

// Spec (brief, 30 Oct: "Registration goes through the settings target's existing `guards` list … `queue` becomes a
// `Stop` entry. Foreign hook entries stay"; "SubagentStop … must not be registered"): with `guards = ["rm",
// "queue"]` and the copy installed, sync names the copy in a `PreToolUse` entry on Bash and PowerShell (`guard rm`;
// D16's brief, design note 2026-11-02-lords-git-help-guard §3) and in a `Stop` entry
// without a matcher (`guard queue`) after the user's own `Stop` hook, writes no `SubagentStop`, and the plan says what
// it adds; `lords check` accepts the target; taking `queue` out of the list takes out only Lords' `Stop` hook.
#[test]
fn sync_registers_the_queue_guard_as_a_stop_hook() {
    let env = Env::new();
    let d = env.dir.path();
    let (public, private) = (d.join("pub"), d.join("priv"));
    fs::create_dir_all(&public).unwrap();
    fs::create_dir_all(&private).unwrap();
    fs::create_dir_all(d.join("home/.claude-balanced")).unwrap();
    let theirs = r#"{"hooks": [{"type": "command", "command": "notify"}]}"#;
    fs::write(
        d.join("home/.claude-balanced/settings.json"),
        format!("{{\n  \"hooks\": {{\"Stop\": [{theirs}]}}\n}}\n"),
    )
    .unwrap();
    fs::write(public.join("stack.toml"), "name = \"pub\"\nvisibility = \"public\"\n").unwrap();
    let manifest = |guards: &str| {
        format!("name = \"priv\"\nvisibility = \"private\"\n[[targets]]\nname = \"balanced settings\"\nadapter = \"claude\"\nkind = \"settings\"\npath = \"~/.claude-balanced/settings.json\"\nguards = {guards}\n")
    };
    fs::write(private.join("stack.toml"), manifest(r#"["rm", "queue"]"#)).unwrap();
    assert!(env.git(&private, &["init", "-q"]).status.success());
    env.git(&private, &["add", "-A"]);
    env.git(&private, &["commit", "-q", "-m", "start"]);
    let stacks =
        ["--public-stack", public.to_str().unwrap(), "--private-stack", private.to_str().unwrap()];
    let sync = |apply: bool| {
        let mut args = vec!["sync", "--user"];
        if apply {
            args.push("--apply");
        }
        env.lords(d, &[&args[..], &stacks].concat())
    };
    let settings = || -> serde_json::Value {
        serde_json::from_str(
            &fs::read_to_string(d.join("home/.claude-balanced/settings.json")).unwrap(),
        )
        .unwrap()
    };
    let o = env.lords(d, &["guard", "install"]);
    assert!(o.status.success(), "{}", text(&o));
    let copy = d.join("home/.lords-command/bin/lords-guard.exe");
    let named = copy.display().to_string().replace('\\', "/");
    let lords =
        |g: &str| serde_json::json!({"type": "command", "command": named, "args": ["guard", g]});
    let o = sync(false);
    assert!(text(&o).contains("Lords hook added: the queue guard (Stop)"), "{}", text(&o));
    let o = sync(true);
    assert!(o.status.success(), "{}", text(&o));
    let v = settings();
    let theirs: serde_json::Value = serde_json::from_str(theirs).unwrap();
    assert_eq!(
        v["hooks"]["PreToolUse"],
        serde_json::json!([{"matcher": "Bash|PowerShell", "hooks": [lords("rm")]}])
    );
    assert_eq!(v["hooks"]["Stop"], serde_json::json!([theirs, {"hooks": [lords("queue")]}]));
    assert!(v["hooks"].get("SubagentStop").is_none(), "{v}");
    let out = text(&env.lords(d, &[&["check"][..], &stacks].concat()));
    assert!(!out.contains("settings.json") && !out.contains("guards"), "{out}");
    // `queue` out of the list: only Lords' Stop hook goes.
    fs::write(private.join("stack.toml"), manifest(r#"["rm"]"#)).unwrap();
    env.git(&private, &["commit", "-q", "-a", "-m", "no queue guard"]);
    let o = sync(true);
    assert!(o.status.success(), "{}", text(&o));
    let v = settings();
    assert_eq!(v["hooks"]["Stop"], serde_json::json!([theirs]));
    assert_eq!(
        v["hooks"]["PreToolUse"],
        serde_json::json!([{"matcher": "Bash|PowerShell", "hooks": [lords("rm")]}])
    );
}

/// `lords guard <args>` with this event on stdin, Claude's config dir set to the test's own (never the real one the
/// session running these tests may carry).
fn rules_guard(env: &Env, args: &[&str], event: &str) -> Output {
    use std::io::Write;
    let mut c = env
        .cmd(env!("CARGO_BIN_EXE_lords"), env.dir.path())
        .args(args)
        .env("CLAUDE_CONFIG_DIR", env.dir.path().join("home/.claude"))
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    c.stdin.take().unwrap().write_all(event.as_bytes()).unwrap();
    c.wait_with_output().unwrap()
}

// Spec (the brief: "`lords guard session`, a `SessionStart` hook … `lords guard turn`, a `UserPromptSubmit` hook. It
// adds `additionalContext` of one short line, `Berlin 07:41 Sat 31 Oct`, from the system clock"; the guards' common
// rule: never fail a harness's event): through the binary, `session` answers nothing and `turn` answers context whose
// one line is a clock line (a zone or UTC label, `HH:MM`, weekday, day, month), both exit 0; a usage error or
// unreadable input is exit 0, nothing on stdout and a note naming the guard.
#[test]
fn the_session_and_turn_guards_answer_through_the_binary() {
    let env = Env::new();
    let cwd = env.dir.path().join("work");
    fs::create_dir_all(&cwd).unwrap();
    let ev = |name: &str| {
        serde_json::json!({"hook_event_name": name, "session_id": "s1", "source": "startup", "cwd": cwd, "prompt": "go"})
            .to_string()
    };
    let o = rules_guard(&env, &["guard", "session"], &ev("SessionStart"));
    assert_eq!(o.status.code(), Some(0), "{}", text(&o));
    assert!(o.stdout.is_empty(), "{}", text(&o));
    let o = rules_guard(&env, &["guard", "turn"], &ev("UserPromptSubmit"));
    assert_eq!(o.status.code(), Some(0), "{}", text(&o));
    let v: serde_json::Value = serde_json::from_slice(&o.stdout).unwrap();
    assert!(v.get("decision").is_none(), "{v}");
    assert_eq!(v["hookSpecificOutput"]["hookEventName"], "UserPromptSubmit");
    let line = v["hookSpecificOutput"]["additionalContext"].as_str().unwrap();
    assert!(!line.contains('\n'), "one line: {line}");
    let words: Vec<&str> = line.trim_end_matches(" (local zone unknown)").split(' ').collect();
    let n = words.len();
    assert!(n >= 5, "{line}");
    let hhmm = words[n - 4].as_bytes();
    let clock =
        hhmm.len() == 5 && hhmm[2] == b':' && words[n - 4].replace(':', "").parse::<u32>().is_ok();
    let days = ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"];
    let months =
        ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];
    assert!(clock && days.contains(&words[n - 3]), "{line}");
    assert!(words[n - 2].parse::<u8>().is_ok_and(|d| (1..=31).contains(&d)), "{line}");
    assert!(months.contains(&words[n - 1]), "{line}");
    for (args, input) in [
        (&["guard", "turn", "extra"][..], ev("UserPromptSubmit")),
        (&["guard", "session", "extra"], ev("SessionStart")),
        (&["guard", "turn"], "not json".to_string()),
    ] {
        let o = rules_guard(&env, args, &input);
        assert_eq!(o.status.code(), Some(0), "{args:?}: {}", text(&o));
        assert!(o.stdout.is_empty(), "{args:?}: {}", text(&o));
        let err = String::from_utf8_lossy(&o.stderr);
        assert!(err.contains(&format!("guard {}", args[1])), "{args:?}: {err}");
    }
}

// Spec (the brief: "in `guards`, `session` maps to SessionStart and `turn` to UserPromptSubmit, as you did for `queue`
// (never SubagentStart)"): with `guards = ["session", "turn"]` and the copy installed, sync names the copy in a
// `SessionStart` entry (`guard session`) and a `UserPromptSubmit` entry (`guard turn`), neither with a matcher, after
// the user's own hooks of those events; writes no `SubagentStart`; the plan says what it adds; `lords check` accepts
// the target; taking `turn` out takes out only Lords' `UserPromptSubmit` hook.
#[test]
fn sync_registers_the_session_and_turn_guards() {
    let env = Env::new();
    let d = env.dir.path();
    let (public, private) = (d.join("pub"), d.join("priv"));
    fs::create_dir_all(&public).unwrap();
    fs::create_dir_all(&private).unwrap();
    fs::create_dir_all(d.join("home/.claude-balanced")).unwrap();
    let theirs = r#"{"hooks": [{"type": "command", "command": "notify"}]}"#;
    fs::write(
        d.join("home/.claude-balanced/settings.json"),
        format!("{{\n  \"hooks\": {{\"SessionStart\": [{theirs}], \"UserPromptSubmit\": [{theirs}]}}\n}}\n"),
    )
    .unwrap();
    fs::write(public.join("stack.toml"), "name = \"pub\"\nvisibility = \"public\"\n").unwrap();
    let manifest = |guards: &str| {
        format!("name = \"priv\"\nvisibility = \"private\"\n[[targets]]\nname = \"balanced settings\"\nadapter = \"claude\"\nkind = \"settings\"\npath = \"~/.claude-balanced/settings.json\"\nguards = {guards}\n")
    };
    fs::write(private.join("stack.toml"), manifest(r#"["session", "turn"]"#)).unwrap();
    assert!(env.git(&private, &["init", "-q"]).status.success());
    env.git(&private, &["add", "-A"]);
    env.git(&private, &["commit", "-q", "-m", "start"]);
    let stacks =
        ["--public-stack", public.to_str().unwrap(), "--private-stack", private.to_str().unwrap()];
    let sync = |apply: bool| {
        let mut args = vec!["sync", "--user"];
        if apply {
            args.push("--apply");
        }
        env.lords(d, &[&args[..], &stacks].concat())
    };
    let settings = || -> serde_json::Value {
        serde_json::from_str(
            &fs::read_to_string(d.join("home/.claude-balanced/settings.json")).unwrap(),
        )
        .unwrap()
    };
    let o = env.lords(d, &["guard", "install"]);
    assert!(o.status.success(), "{}", text(&o));
    let copy = d.join("home/.lords-command/bin/lords-guard.exe");
    let named = copy.display().to_string().replace('\\', "/");
    let lords =
        |g: &str| serde_json::json!({"type": "command", "command": named, "args": ["guard", g]});
    let o = sync(false);
    for added in [
        "Lords hook added: the session guard (SessionStart)",
        "Lords hook added: the turn guard (UserPromptSubmit)",
    ] {
        assert!(text(&o).contains(added), "{}", text(&o));
    }
    let o = sync(true);
    assert!(o.status.success(), "{}", text(&o));
    let v = settings();
    let theirs: serde_json::Value = serde_json::from_str(theirs).unwrap();
    assert_eq!(
        v["hooks"]["SessionStart"],
        serde_json::json!([theirs, {"hooks": [lords("session")]}])
    );
    assert_eq!(
        v["hooks"]["UserPromptSubmit"],
        serde_json::json!([theirs, {"hooks": [lords("turn")]}])
    );
    assert!(v["hooks"].get("SubagentStart").is_none(), "{v}");
    let out = text(&env.lords(d, &[&["check"][..], &stacks].concat()));
    assert!(!out.contains("settings.json") && !out.contains("guards"), "{out}");
    fs::write(private.join("stack.toml"), manifest(r#"["session"]"#)).unwrap();
    env.git(&private, &["commit", "-q", "-a", "-m", "no turn guard"]);
    let o = sync(true);
    assert!(o.status.success(), "{}", text(&o));
    let v = settings();
    assert_eq!(v["hooks"]["UserPromptSubmit"], serde_json::json!([theirs]));
    assert_eq!(
        v["hooks"]["SessionStart"],
        serde_json::json!([theirs, {"hooks": [lords("session")]}])
    );
}

// Spec (the brief: "Lords registers the hook through the settings targets, as with the other guards: a new guard name,
// added to the guards list in the schema the way `child` and `role` were"; design note 2026-11-02-lords-time-guard §2,
// §3: `PreToolUse` with the exact-list matcher of the editor tools and both T3 send names; Codex's line takes no
// `--home`): with `guards = ["rm", "time"]` sync writes the `rm` entry and the `time` entry, the plan says so, `lords
// check` accepts the target; through the binary the guard denies a placeholder in a T3 message, exit 0, and a usage
// error is exit 0, nothing on stdout, a note naming the guard.
#[test]
fn sync_registers_the_time_guard_and_the_binary_answers() {
    let env = Env::new();
    let d = env.dir.path();
    let (public, private) = (d.join("pub"), d.join("priv"));
    fs::create_dir_all(&public).unwrap();
    fs::create_dir_all(&private).unwrap();
    fs::create_dir_all(d.join("home/.claude-balanced")).unwrap();
    fs::write(public.join("stack.toml"), "name = \"pub\"\nvisibility = \"public\"\n").unwrap();
    fs::write(
        private.join("stack.toml"),
        "name = \"priv\"\nvisibility = \"private\"\n[[targets]]\nname = \"balanced settings\"\nadapter = \"claude\"\nkind = \"settings\"\npath = \"~/.claude-balanced/settings.json\"\nguards = [\"rm\", \"time\"]\n",
    )
    .unwrap();
    assert!(env.git(&private, &["init", "-q"]).status.success());
    env.git(&private, &["add", "-A"]);
    env.git(&private, &["commit", "-q", "-m", "start"]);
    let stacks =
        ["--public-stack", public.to_str().unwrap(), "--private-stack", private.to_str().unwrap()];
    assert!(env.lords(d, &["guard", "install"]).status.success());
    let matcher = "Write|Edit|mcp__t3-code__t3_thread_send|mcp__t3_code__t3_thread_send";
    let o = env.lords(d, &[&["sync", "--user"][..], &stacks].concat());
    let added = format!("Lords hook added: the time guard (PreToolUse, {matcher})");
    assert!(text(&o).contains(&added), "{}", text(&o));
    let o = env.lords(d, &[&["sync", "--user", "--apply"][..], &stacks].concat());
    assert!(o.status.success(), "{}", text(&o));
    let named =
        d.join("home/.lords-command/bin/lords-guard.exe").display().to_string().replace('\\', "/");
    let lords =
        |g: &str| serde_json::json!({"type": "command", "command": named, "args": ["guard", g]});
    let v: serde_json::Value = serde_json::from_str(
        &fs::read_to_string(d.join("home/.claude-balanced/settings.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(
        v["hooks"]["PreToolUse"],
        serde_json::json!([
            {"matcher": "Bash|PowerShell", "hooks": [lords("rm")]},
            {"matcher": matcher, "hooks": [lords("time")]},
        ])
    );
    let out = text(&env.lords(d, &[&["check"][..], &stacks].concat()));
    assert!(!out.contains("settings.json") && !out.contains("guards"), "{out}");
    let codex = lords_core::adapters::get("codex").unwrap();
    let line = codex.guard_hooks("C:/x/lords-guard.exe", "time", "C:/h")[0]["command"].clone();
    assert!(line.as_str().is_some_and(|l| l.ends_with("guard time")), "{line}");
    let ev = serde_json::json!({"hook_event_name": "PreToolUse", "tool_name": "mcp__t3-code__t3_thread_send",
        "tool_input": {"threadId": "t", "message": "done at 14:xx"}})
    .to_string();
    let o = rules_guard(&env, &["guard", "time"], &ev);
    assert_eq!(o.status.code(), Some(0), "{}", text(&o));
    let v: serde_json::Value = serde_json::from_slice(&o.stdout).unwrap();
    assert_eq!(v["hookSpecificOutput"]["permissionDecision"], "deny", "{v}");
    let o = rules_guard(&env, &["guard", "time", "extra"], &ev);
    assert_eq!(o.status.code(), Some(0), "{}", text(&o));
    assert!(o.stdout.is_empty(), "{}", text(&o));
    assert!(String::from_utf8_lossy(&o.stderr).contains("guard time"), "{}", text(&o));
}

// Spec (design note 2026-11-02-lords-native-helper-guard §2: "A guard `helper` on `PreToolUse`, registered by sync like
// the others: Claude: matcher on the native helper tool names (`Agent`, `Task`)"; the Lords owner, 3 Nov: until H8/H9
// are checked live "the guard does nothing in Codex"; A4: the plan names the matcher, so a rename shows; I5: the other
// guards' entries stay): with `guards = ["rm", "helper"]` sync writes the rm entry and a `PreToolUse` entry `Agent|Task`
// running `guard helper`, the plan says so, `lords check` accepts the target; a Codex hooks target asking for `helper`
// is refused and its file isn't written.
#[test]
fn sync_registers_the_helper_guard_for_claude_code_only() {
    let env = Env::new();
    let d = env.dir.path();
    let (public, private) = (d.join("pub"), d.join("priv"));
    fs::create_dir_all(&public).unwrap();
    fs::create_dir_all(&private).unwrap();
    fs::create_dir_all(d.join("home/.claude-balanced")).unwrap();
    fs::create_dir_all(d.join("home/.codex")).unwrap();
    fs::write(public.join("stack.toml"), "name = \"pub\"\nvisibility = \"public\"\n").unwrap();
    let claude = "[[targets]]\nname = \"balanced settings\"\nadapter = \"claude\"\nkind = \"settings\"\npath = \"~/.claude-balanced/settings.json\"\nguards = [\"rm\", \"helper\"]\n";
    let head = "name = \"priv\"\nvisibility = \"private\"\n";
    fs::write(private.join("stack.toml"), format!("{head}{claude}")).unwrap();
    assert!(env.git(&private, &["init", "-q"]).status.success());
    env.git(&private, &["add", "-A"]);
    env.git(&private, &["commit", "-q", "-m", "start"]);
    let stacks =
        ["--public-stack", public.to_str().unwrap(), "--private-stack", private.to_str().unwrap()];
    assert!(env.lords(d, &["guard", "install"]).status.success());
    let o = env.lords(d, &[&["sync", "--user"][..], &stacks].concat());
    let added = "Lords hook added: the helper guard (PreToolUse, Agent|Task)";
    assert!(text(&o).contains(added), "{}", text(&o));
    let o = env.lords(d, &[&["sync", "--user", "--apply"][..], &stacks].concat());
    assert!(o.status.success(), "{}", text(&o));
    let named =
        d.join("home/.lords-command/bin/lords-guard.exe").display().to_string().replace('\\', "/");
    let lords =
        |g: &str| serde_json::json!({"type": "command", "command": named, "args": ["guard", g]});
    let v: serde_json::Value = serde_json::from_str(
        &fs::read_to_string(d.join("home/.claude-balanced/settings.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(
        v["hooks"]["PreToolUse"],
        serde_json::json!([
            {"matcher": "Bash|PowerShell", "hooks": [lords("rm")]},
            {"matcher": "Agent|Task", "hooks": [lords("helper")]},
        ])
    );
    let out = text(&env.lords(d, &[&["check"][..], &stacks].concat()));
    assert!(!out.contains("settings.json") && !out.contains("guards"), "{out}");
    let codex = "[[targets]]\nname = \"codex hooks\"\nadapter = \"codex\"\nkind = \"settings\"\npath = \"~/.codex/hooks.json\"\nguards = [\"helper\"]\n";
    fs::write(private.join("stack.toml"), format!("{head}{claude}{codex}")).unwrap();
    env.git(&private, &["commit", "-q", "-a", "-m", "a helper guard for Codex"]);
    let o = env.lords(d, &[&["sync", "--user", "--apply"][..], &stacks].concat());
    assert!(!o.status.success(), "{}", text(&o));
    assert!(text(&o).contains("runs no `helper` guard"), "{}", text(&o));
    assert!(!d.join("home/.codex/hooks.json").exists(), "nothing written");
}

/// `lords guard <args>` with this event on stdin and, of the T3 test's variables (`T3CODE_THREAD_ID`, `CLAUDE_PID`),
/// only `vars`: the session running these tests may carry its own, which never reach the guard here.
fn helper_guard(env: &Env, args: &[&str], vars: &[(&str, String)], event: &str) -> Output {
    use std::io::Write;
    use std::process::Stdio;
    let mut c = env.cmd(env!("CARGO_BIN_EXE_lords"), env.dir.path());
    c.args(args).env_remove("T3CODE_THREAD_ID").env_remove("CLAUDE_PID");
    for (k, v) in vars {
        c.env(k, v);
    }
    let mut c =
        c.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().unwrap();
    c.stdin.take().unwrap().write_all(event.as_bytes()).unwrap();
    c.wait_with_output().unwrap()
}

// Spec (design note 2026-11-02-lords-native-helper-guard §5.1, the choice; I1, I2, I4, A1, A2, A10; §2: "Outside T3 it
// allows, always and silently"): through the binary, an `Agent` call goes through silently with no T3 thread (a plain
// CLI) and with a thread whose `CLAUDE_PID` names a process launched without T3's tools (a CLI from a T3 agent's shell;
// here this test process); it is refused, naming `delegate_task`, when `CLAUDE_PID` names a live process launched with
// `--allowedTools mcp__t3-code__*`; a `CLAUDE_PID` that isn't a number or names no process, and a usage error, are
// exit 0, nothing on stdout, a note naming the guard. The reader gives that process's arguments back as they were
// passed (on Windows, the line Rust's quoting wrote: an independent writer).
#[test]
fn the_helper_guard_tells_t3_from_a_cli_through_the_binary() {
    use std::process::Stdio;
    let env = Env::new();
    let d = env.dir.path();
    let ev = serde_json::json!({"hook_event_name": "PreToolUse", "tool_name": "Agent",
        "tool_input": {"subagent_type": "general-purpose", "prompt": "p"}})
    .to_string();
    let guard = &["guard", "helper"][..];
    let thread = || ("T3CODE_THREAD_ID", "thread:delegated-task:x".to_string());
    let pid = |p: String| ("CLAUDE_PID", p);
    let silent = |o: &Output, what: &str| {
        assert_eq!(o.status.code(), Some(0), "{what}: {}", text(o));
        assert!(o.stdout.is_empty() && o.stderr.is_empty(), "{what}: {}", text(o));
    };
    silent(&helper_guard(&env, guard, &[], &ev), "no T3 thread");
    let me = pid(std::process::id().to_string());
    silent(&helper_guard(&env, guard, &[thread(), me], &ev), "a CLI from a T3 agent's shell");
    // A live process launched as T3 launches a session: the Codex stub waits on its stdin and ignores these arguments.
    let args = [
        "app-server",
        "--allowedTools",
        "mcp__t3-code__*",
        "--tools",
        "default",
        "a b",
        r#"x"y"#,
        r"C:\dir with space\",
        "",
    ];
    let mut t3 = env
        .cmd(env!("CARGO_BIN_EXE_lords-codex-stub"), d)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let read = lords_core::process::command_line(t3.id());
    let o = helper_guard(&env, guard, &[thread(), pid(t3.id().to_string())], &ev);
    drop(t3.stdin.take());
    t3.wait().unwrap();
    let want: Vec<String> = args.iter().map(|a| a.to_string()).collect();
    assert_eq!(read.map(|a| a.into_iter().skip(1).collect::<Vec<_>>()), Ok(want));
    assert_eq!(o.status.code(), Some(0), "{}", text(&o));
    let v: serde_json::Value = serde_json::from_slice(&o.stdout).unwrap();
    assert_eq!(v["hookSpecificOutput"]["permissionDecision"], "deny", "{v}");
    let reason = v["hookSpecificOutput"]["permissionDecisionReason"].as_str().unwrap();
    assert!(reason.contains("delegate_task"), "{reason}");
    let mut gone = env.cmd("git", d).arg("--version").stdout(Stdio::null()).spawn().unwrap();
    let gone_pid = gone.id();
    gone.wait().unwrap();
    drop(gone);
    for p in ["x".to_string(), gone_pid.to_string()] {
        let o = helper_guard(&env, guard, &[thread(), pid(p.clone())], &ev);
        assert_eq!(o.status.code(), Some(0), "{p}: {}", text(&o));
        assert!(o.stdout.is_empty(), "{p}: {}", text(&o));
        assert!(String::from_utf8_lossy(&o.stderr).contains("guard helper"), "{p}: {}", text(&o));
    }
    let o = helper_guard(&env, &["guard", "helper", "extra"], &[], &ev);
    assert_eq!(o.status.code(), Some(0), "{}", text(&o));
    assert!(o.stdout.is_empty(), "{}", text(&o));
    assert!(String::from_utf8_lossy(&o.stderr).contains("guard helper"), "{}", text(&o));
}

// Spec: D16's brief: "probe whether Claude's `PowerShell` tool and Codex's shell tool reach a PreToolUse hook with the
// command text. Cover each that does, with the same check" (design note 2026-11-02-lords-git-help-guard §2: both do;
// Codex sends its commands as `Bash`, in PowerShell on Windows; §3: Codex runs the `rm` guard, its line on Windows
// with `--shell powershell`). Through the binary: a form only PowerShell reads as git help is denied for the
// PowerShell tool and under `--shell powershell`, not as Bash text; a bad `--shell` is a usage note, exit 0, nothing
// on stdout.
#[test]
fn the_rm_guard_reads_powershell_for_its_tool_and_for_codex() {
    let env = Env::new();
    let exe = Path::new(env!("CARGO_BIN_EXE_lords"));
    let codex = lords_core::adapters::get("codex").unwrap();
    assert!(codex.guards().contains(&"rm"), "{:?}", codex.guards());
    let line = codex.guard_hooks("C:/x/lords-guard.exe", "rm", "C:/h")[0]["command"].clone();
    let want = if cfg!(windows) { "guard rm --shell powershell" } else { "guard rm" };
    assert!(line.as_str().is_some_and(|l| l.ends_with(want)), "{line}");
    // `"C:\tmp\"` ends its string in PowerShell, so `git help config` runs; in Bash the string never ends.
    let ps = "Get-ChildItem \"C:\\tmp\\\"; git help config";
    let event = |tool: &str, command: &str| {
        serde_json::json!({"hook_event_name": "PreToolUse", "tool_name": tool, "tool_input": {"command": command}})
            .to_string()
    };
    let denies = |o: &Output| String::from_utf8_lossy(&o.stdout).contains("\"deny\"");
    for (args, tool, command, deny) in [
        (&["guard", "rm", "--shell", "powershell"][..], "Bash", ps, true),
        (&["guard", "rm"], "PowerShell", ps, true),
        (&["guard", "rm"], "Bash", ps, false),
        (&["guard", "rm", "--shell", "bash"], "Bash", "git help config", true),
    ] {
        let o = guard(&env, exe, args, &event(tool, command));
        assert_eq!(o.status.code(), Some(0), "{args:?}: {}", text(&o));
        assert_eq!(denies(&o), deny, "{args:?} {tool} `{command}`: {}", text(&o));
    }
    for args in [&["guard", "rm", "--shell"][..], &["guard", "rm", "--shell", "zsh"]] {
        let o = guard(&env, exe, args, &event("Bash", "git help config"));
        assert_eq!(o.status.code(), Some(0), "{args:?}: {}", text(&o));
        assert!(o.stdout.is_empty() && !o.stderr.is_empty(), "{args:?}: {}", text(&o));
    }
}

// Spec: review r1, D16-R1-01 (accepted by the Lords owner, 2 Nov): "sync never registers a guard hook command line
// that the installed guard copy doesn't accept", as FC4 set for the child and role hooks. A copy from before D16 (the
// old-guard stub: `ok` to the self-test, nothing to `guard rm`) is refused for both adapters, naming `lords guard
// install`, and their files stay as they are; once installed, the hooks are written, the copy denies the original
// witness with the exact arguments each hook names, and a second sync changes nothing.
#[test]
fn sync_registers_the_rm_guard_only_with_a_copy_that_speaks_it() {
    let env = Env::new();
    let d = env.dir.path();
    let (public, private) = (d.join("pub"), d.join("priv"));
    for dir in [&public, &private, &d.join("home/.claude-balanced"), &d.join("home/.codex")] {
        fs::create_dir_all(dir).unwrap();
    }
    fs::write(public.join("stack.toml"), "name = \"pub\"\nvisibility = \"public\"\n").unwrap();
    let target = |adapter: &str, path: &str| {
        format!("[[targets]]\nname = \"{adapter}\"\nadapter = \"{adapter}\"\nkind = \"settings\"\npath = \"{path}\"\nguards = [\"rm\"]\n")
    };
    let (claude, codex) = (
        target("claude", "~/.claude-balanced/settings.json"),
        target("codex", "~/.codex/hooks.json"),
    );
    let manifest = format!("name = \"priv\"\nvisibility = \"private\"\n{claude}{codex}");
    fs::write(private.join("stack.toml"), manifest).unwrap();
    assert!(env.git(&private, &["init", "-q"]).status.success());
    env.git(&private, &["add", "-A"]);
    env.git(&private, &["commit", "-q", "-m", "start"]);
    let stacks =
        ["--public-stack", public.to_str().unwrap(), "--private-stack", private.to_str().unwrap()];
    let sync = || env.lords(d, &[&["sync", "--user", "--apply"][..], &stacks].concat());
    let files = [d.join("home/.claude-balanced/settings.json"), d.join("home/.codex/hooks.json")];
    let read = || files.iter().map(|f| fs::read(f).ok()).collect::<Vec<_>>();
    let copy = d.join("home/.lords-command/bin/lords-guard.exe");
    fs::create_dir_all(copy.parent().unwrap()).unwrap();
    fs::copy(env!("CARGO_BIN_EXE_lords-old-guard-stub"), &copy).unwrap();
    let o = sync();
    assert!(!o.status.success() && text(&o).contains("lords guard install"), "{}", text(&o));
    assert_eq!(read(), [None, None], "nothing written");
    assert!(env.lords(d, &["guard", "install"]).status.success());
    assert!(sync().status.success());
    let after = read();
    let json = |b: &Option<Vec<u8>>| -> serde_json::Value {
        serde_json::from_slice(b.as_ref().unwrap()).unwrap()
    };
    let claude_args = json(&after[0])["hooks"]["PreToolUse"][0]["hooks"][0]["args"].clone();
    let codex_line = json(&after[1])["hooks"]["PreToolUse"][0]["hooks"][0]["command"].clone();
    let codex_args =
        format!("guard {}", codex_line.as_str().unwrap().split(" guard ").nth(1).unwrap());
    let claude_args: Vec<&str> =
        claude_args.as_array().unwrap().iter().map(|a| a.as_str().unwrap()).collect();
    let event = r#"{"tool_name":"Bash","tool_input":{"command":"git help config"}}"#;
    for args in [claude_args, codex_args.split(' ').collect()] {
        let o = guard(&env, &copy, &args, event);
        assert!(String::from_utf8_lossy(&o.stdout).contains("\"deny\""), "{args:?}: {}", text(&o));
    }
    assert!(sync().status.success());
    assert_eq!(read(), after, "a second sync changes nothing");
}

/// A repo `r` declared in a private stack with one rule, and its worktree `wts/one`, synced by the CLI: each holds
/// `CLAUDE.local.md` and `.lords`. Gives the repo, the worktree and the stack arguments every call here takes.
fn synced_worktree(env: &Env) -> (PathBuf, PathBuf, Vec<String>) {
    let d = env.dir.path();
    let (public, private) = (d.join("pub"), d.join("priv"));
    fs::create_dir_all(public.join("global/rules")).unwrap();
    fs::create_dir_all(private.join("global/rules")).unwrap();
    fs::write(
        public.join("stack.toml"),
        "name = \"pub\"\nvisibility = \"public\"\nroles = [\"coder\"]\n",
    )
    .unwrap();
    fs::write(
        private.join("stack.toml"),
        format!(
            "name = \"priv\"\nvisibility = \"private\"\nroles = [\"coder\"]\n[repos.r]\ncheckouts = ['{}']\nworktree_roots = ['{}']\nadapters = [\"claude\"]\n",
            d.join("repo").display(),
            d.join("wts").display()
        ),
    )
    .unwrap();
    fs::write(
        private.join("global/rules/r.md"),
        "---\nid: rule.r\nkind: rule\ntitle: r\naudience: [all]\nroles: [coder]\nscope: global\nstatus: active\nvisibility: private\norigin:\n  type: own\n---\nTest before merging.\n",
    )
    .unwrap();
    let repo = env.repo();
    fs::write(repo.join("README.md"), "a repo\n").unwrap();
    env.git(&repo, &["add", "-A"]);
    assert!(env.git(&repo, &["commit", "-q", "-m", "start"]).status.success());
    let wt = d.join("wts").join("one");
    let o = env.git(&repo, &["worktree", "add", "-q", "-b", "one", wt.to_str().unwrap()]);
    assert!(o.status.success(), "{}", text(&o));
    let stacks: Vec<String> = vec![
        "--public-stack".into(),
        public.to_string_lossy().into_owned(),
        "--private-stack".into(),
        private.to_string_lossy().into_owned(),
    ];
    let mut args = vec!["sync", "--repo", "r", "--apply"];
    args.extend(stacks.iter().map(String::as_str));
    let o = env.lords(&repo, &args);
    assert!(o.status.success(), "{}", text(&o));
    for c in [wt.join("CLAUDE.local.md"), wt.join(".lords")] {
        assert!(c.exists(), "synced: {} ({})", c.display(), text(&o));
    }
    (repo, wt, stacks)
}

/// A JSON list of `uninstall --place`'s result as (path, action) pairs, paths compared as Windows compares them.
fn listed(v: &serde_json::Value, list: &str) -> std::collections::BTreeSet<(String, String)> {
    v[list]
        .as_array()
        .unwrap_or_else(|| panic!("no {list}: {v}"))
        .iter()
        .map(|r| (folded(r["path"].as_str().unwrap()), r["action"].as_str().unwrap().to_string()))
        .collect()
}

fn folded(p: &str) -> String {
    let p = p.replace('\\', "/");
    if cfg!(windows) {
        p.to_lowercase()
    } else {
        p
    }
}

// Spec (design note Q2: "The dry run lists every exact path with what would happen to it. --json prints one object
// (format 1) on stdout"; "Exit codes: 0: no record of the place remains (the dry run predicts it; after --apply it is
// read back from the ledger); 1: some record of the place remains, each one named"; Q4's JSON): named relatively from
// inside it, the worktree's dry run lists its two copies as removals, removes nothing, exit 0; with its role file
// edited, the dry run names `.lords` as kept, exit 1; applied, `CLAUDE.local.md` goes and `.lords` stays with the
// edit, exit 1, read back; with `.lords` deleted by hand, applied again its record is dropped, nothing remains, exit
// 0. The text form names both paths.
#[test]
fn uninstall_place_lists_its_paths_and_exits_by_what_remains() {
    let env = Env::new();
    let (_repo, wt, stacks) = synced_worktree(&env);
    let wt_text = wt.to_string_lossy().into_owned();
    let run = |cwd: &Path, place: &str, apply: bool| -> (Option<i32>, serde_json::Value) {
        let mut args = vec!["uninstall", "--repo", "r", "--place", place, "--json"];
        if apply {
            args.push("--apply");
        }
        args.extend(stacks.iter().map(String::as_str));
        let o = env.lords(cwd, &args);
        let v: serde_json::Value =
            serde_json::from_slice(&o.stdout).unwrap_or_else(|e| panic!("{e}: {}", text(&o)));
        (o.status.code(), v)
    };
    let (file, lords) = (
        folded(&wt.join("CLAUDE.local.md").to_string_lossy()),
        folded(&wt.join(".lords").to_string_lossy()),
    );
    let pair = |p: &str, a: &str| (p.to_string(), a.to_string());

    let (code, v) = run(&wt, ".", false);
    assert_eq!(code, Some(0), "{v}");
    assert_eq!(
        (v["format"].as_u64(), v["applied"].as_bool(), v["exit"].as_u64()),
        (Some(1), Some(false), Some(0)),
        "{v}"
    );
    assert!(v["error"].is_null(), "{v}");
    assert_eq!(listed(&v, "planned"), [pair(&file, "remove"), pair(&lords, "remove")].into());
    assert!(listed(&v, "remaining").is_empty(), "{v}");
    assert!(
        wt.join("CLAUDE.local.md").is_file() && wt.join(".lords").is_dir(),
        "a dry run removes nothing"
    );

    let mut args = vec!["uninstall", "--repo", "r", "--place", wt_text.as_str()];
    args.extend(stacks.iter().map(String::as_str));
    let o = env.lords(&wt, &args);
    assert_eq!(o.status.code(), Some(0), "{}", text(&o));
    assert!(text(&o).contains("CLAUDE.local.md") && text(&o).contains(".lords"), "{}", text(&o));

    fs::write(wt.join(".lords").join("role-coder.md"), "OWNER EDIT\n").unwrap();
    let (code, v) = run(&wt, &wt_text, false);
    assert_eq!(code, Some(1), "{v}");
    assert_eq!(listed(&v, "remaining"), [pair(&lords, "keep")].into());

    let (code, v) = run(&wt, &wt_text, true);
    assert_eq!(code, Some(1), "{v}");
    assert_eq!((v["applied"].as_bool(), v["exit"].as_u64()), (Some(true), Some(1)), "{v}");
    assert_eq!(listed(&v, "remaining"), [pair(&lords, "keep")].into());
    assert!(!wt.join("CLAUDE.local.md").exists());
    assert_eq!(
        fs::read_to_string(wt.join(".lords").join("role-coder.md")).unwrap(),
        "OWNER EDIT\n"
    );

    fs::remove_dir_all(wt.join(".lords")).unwrap();
    let (code, v) = run(&wt, &wt_text, true);
    assert_eq!(code, Some(0), "{v}");
    assert_eq!(listed(&v, "planned"), [pair(&lords, "forget")].into());
    assert!(listed(&v, "remaining").is_empty(), "{v}");
}

// Spec (design note Q2: "2: refused or stopped: bad arguments, <tree> not a declared place of NAME … A refusal
// changes nothing"; Q4: "--json, one object on stdout, also for a refusal … place is null when the place was never
// found … remaining is null when it isn't known"): `--place` without `--repo` is refused, exit 2; a plain dir under
// the worktree root, with `--apply`, is refused with exit 2, its JSON giving the error, no place, nothing planned and
// nothing known to remain, and the worktree's copies stay; `--place` with no value, with `--json`, is a bad argument
// that still gives one JSON object with its error, exit 2 (sol-loop r1 S2).
#[test]
fn uninstall_place_refuses_what_is_no_place() {
    let env = Env::new();
    let (_repo, wt, stacks) = synced_worktree(&env);
    let mut args = vec!["uninstall", "--place", wt.to_str().unwrap()];
    args.extend(stacks.iter().map(String::as_str));
    let o = env.lords(&wt, &args);
    assert_eq!(o.status.code(), Some(2), "{}", text(&o));

    let plain = env.dir.path().join("wts").join("plain");
    fs::create_dir_all(&plain).unwrap();
    let mut args =
        vec!["uninstall", "--repo", "r", "--place", plain.to_str().unwrap(), "--json", "--apply"];
    args.extend(stacks.iter().map(String::as_str));
    let o = env.lords(&wt, &args);
    assert_eq!(o.status.code(), Some(2), "{}", text(&o));
    let v: serde_json::Value =
        serde_json::from_slice(&o.stdout).unwrap_or_else(|e| panic!("{e}: {}", text(&o)));
    assert_eq!(v["exit"].as_u64(), Some(2), "{v}");
    assert!(v["error"].as_str().is_some_and(|e| !e.is_empty()), "{v}");
    assert!(v["place"].is_null(), "{v}");
    assert!(listed(&v, "planned").is_empty() && v["remaining"].is_null(), "{v}");
    assert_eq!(v["applied"].as_bool(), Some(false), "{v}");
    assert!(wt.join("CLAUDE.local.md").is_file() && wt.join(".lords").is_dir());

    let mut args = vec!["uninstall", "--repo", "r", "--json"];
    args.extend(stacks.iter().map(String::as_str));
    args.push("--place");
    let o = env.lords(&wt, &args);
    assert_eq!(o.status.code(), Some(2), "{}", text(&o));
    let v: serde_json::Value =
        serde_json::from_slice(&o.stdout).unwrap_or_else(|e| panic!("{e}: {}", text(&o)));
    assert_eq!(v["exit"].as_u64(), Some(2), "{v}");
    assert!(v["error"].as_str().is_some_and(|e| e.contains("--place")), "{v}");
}
