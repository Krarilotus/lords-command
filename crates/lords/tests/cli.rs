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
            .env("USERPROFILE", self.dir.path().join("home"));
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
    let o = env.lords(
        &repo,
        &[
            "hooks",
            "install",
            "--post-checkout",
            "--public-stack",
            public.to_str().unwrap(),
            "--private-stack",
            private.to_str().unwrap(),
        ],
    );
    assert!(o.status.success(), "{}", text(&o));
    let log = repo_log(&repo);
    (repo, d.join("wts"), log)
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

/// A user-level fixture for the stale-plan tests: a settings target with the rm guard, a skills target, three
/// `lords` builds told apart by a trailing byte (A: none, B, C), and a plan for each.
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

    fn plan(&self, build: &str) -> lords_core::sync::Plan {
        let r = self.dir.path();
        let stacks = lords_core::Stacks::load(&r.join("pub"), Some(&r.join("priv"))).unwrap();
        let opts = lords_core::sync::Options {
            user_only: true,
            home: r.join("tool"),
            guard_from: Some(r.join(build).join("lords.exe")),
            ..lords_core::sync::Options::default()
        };
        lords_core::sync::plan(&stacks, &opts).unwrap()
    }

    /// The guard copy's last byte, the settings' bytes and the ledger's bytes: what a refused apply must leave.
    fn state(&self) -> (Option<u8>, Vec<u8>, Option<Vec<u8>>) {
        let r = self.dir.path();
        let copy =
            fs::read(r.join("tool/bin/lords-guard.exe")).ok().and_then(|b| b.last().copied());
        let ledger = fs::read(r.join("tool/state/ledger.toml")).ok();
        (copy, fs::read(r.join("home/settings.json")).unwrap(), ledger)
    }
}

// Spec: rm-guard fix check r2, R7 (witness: a stale plan whose only changes are the settings and the guard: the ledger
// revision doesn't move, and the stale plan replaced the live guard): a plan is current only if the settings and the
// guard copy are as it saw them; a stale one changes nothing.
#[test]
fn a_settings_only_stale_plan_never_replaces_the_live_guard() {
    let s = Stale::new();
    lords_core::sync::apply(&s.plan("a")).unwrap();
    let stale = s.plan("a");
    s.write("home/settings.json", b"{\"model\":\"x\"}\n");
    lords_core::sync::apply(&s.plan("b")).unwrap();
    let after_b = s.state();
    assert_eq!(after_b.0, Some(b'B'));
    let _ = lords_core::sync::apply(&stale);
    assert_eq!(s.state(), after_b, "a stale settings plan changed something");
}

// Spec: R7, the guard-only case: settings in sync, only the guard copy changed by another sync since the plan.
#[test]
fn a_guard_only_stale_plan_never_replaces_the_live_guard() {
    let s = Stale::new();
    lords_core::sync::apply(&s.plan("b")).unwrap();
    let stale = s.plan("a");
    lords_core::sync::apply(&s.plan("c")).unwrap();
    let after_c = s.state();
    assert_eq!(after_c.0, Some(b'C'));
    assert!(lords_core::sync::apply(&stale).is_err(), "a stale guard plan is refused");
    assert_eq!(s.state(), after_c);
}

// Spec: rm-guard fix check r3, R7 (witness: after the plan, an unowned file appeared where it would install a skill;
// the ledger, the settings and the guard were unchanged, and apply replaced the guard before the install found it
// stale): apply validates every item first, then acts; a stale tool destination changes nothing.
#[test]
fn a_stale_tool_destination_never_replaces_the_live_guard() {
    let s = Stale::new();
    lords_core::sync::apply(&s.plan("b")).unwrap();
    s.skill("b");
    let stale = s.plan("a");
    s.write("home/skills/b", b"not the tool's\n");
    let before = s.state();
    assert_eq!(before.0, Some(b'B'));
    assert!(lords_core::sync::apply(&stale).is_err(), "a stale destination is refused");
    assert_eq!(s.state(), before);
}

// Spec: rm-guard review r1, R7 (witness: a stale plan's apply installed its guard, replacing a newer one, before the
// ledger check refused the plan): a stale plan changes nothing, the guard copy included; the guard and the settings
// are installed under one apply lock, after the plan is known current.
#[test]
fn a_stale_plan_never_replaces_the_live_guard() {
    use lords_core::sync::{apply, plan, Options};
    let d = TempDir::new().unwrap();
    let r = d.path();
    let write = |rel: &str, text: &[u8]| {
        let p = r.join(rel);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, text).unwrap();
    };
    let skill = |name: &str| {
        write(
            &format!("pub/global/skills/{name}/SKILL.md"),
            format!("---\nname: {name}\n---\nX\n").as_bytes(),
        );
        write(
            &format!("pub/global/skills/{name}/lords.yaml"),
            format!("id: skill.{name}\nkind: skill\ntitle: {name}\naudience: [all]\nscope: global\nstatus: active\nvisibility: public\norigin:\n  type: own\n").as_bytes(),
        );
    };
    write("pub/stack.toml", b"name = \"pub\"\nvisibility = \"public\"\n");
    skill("a");
    write("home/settings.json", b"{}\n");
    let targets = format!(
        "name = \"priv\"\nvisibility = \"private\"\n[[targets]]\nname = \"settings\"\nadapter = \"claude\"\nkind = \"settings\"\npath = '{}'\nguards = [\"rm\"]\n[[targets]]\nname = \"skills\"\nadapter = \"claude\"\nkind = \"skills\"\npath = '{}'\n",
        r.join("home/settings.json").display(),
        r.join("home/skills").display()
    );
    write("priv/stack.toml", targets.as_bytes());
    let lords = fs::read(env!("CARGO_BIN_EXE_lords")).unwrap();
    write("a/lords.exe", &lords);
    // B: the same program with a trailing byte, so its copy is told apart.
    write("b/lords.exe", &[&lords[..], b"B"].concat());
    let stacks = || lords_core::Stacks::load(&r.join("pub"), Some(&r.join("priv"))).unwrap();
    let opts = |exe: &str| Options {
        user_only: true,
        home: r.join("tool"),
        guard_from: Some(r.join(exe).join("lords.exe")),
        ..Options::default()
    };
    let stale = plan(&stacks(), &opts("a")).unwrap();
    skill("b");
    apply(&plan(&stacks(), &opts("b")).unwrap()).unwrap();
    let copy = r.join("tool/bin/lords-guard.exe");
    assert_eq!(fs::read(&copy).unwrap().last(), Some(&b'B'), "the newer guard is installed");
    let settings = fs::read(r.join("home/settings.json")).unwrap();
    assert!(apply(&stale).is_err(), "the stale plan is refused");
    assert_eq!(fs::read(&copy).unwrap().last(), Some(&b'B'), "the live guard stays");
    assert_eq!(fs::read(r.join("home/settings.json")).unwrap(), settings);
}

// Spec (orchestrator, 30 Oct: "have sync install a stable copy … that the settings name. Copy it only once that exe
// answers … a self-test"): a settings target with guards installs `<tool home>/bin/lords-guard.exe` and names it in
// the hook; the installed copy refuses the witness; a second sync changes nothing.
#[test]
fn sync_installs_a_stable_guard_copy_and_names_it() {
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
    let sync = |apply: bool| {
        let mut args = vec!["sync", "--user"];
        if apply {
            args.push("--apply");
        }
        args.extend([
            "--public-stack",
            public.to_str().unwrap(),
            "--private-stack",
            private.to_str().unwrap(),
        ]);
        env.lords(d, &args)
    };
    let o = sync(true);
    assert!(o.status.success(), "{}", text(&o));
    let copy = d.join("home/.lords-command/bin/lords-guard.exe");
    assert!(copy.is_file(), "{}", text(&o));
    let v: serde_json::Value = serde_json::from_str(
        &fs::read_to_string(d.join("home/.claude-balanced/settings.json")).unwrap(),
    )
    .unwrap();
    let h = &v["hooks"]["PreToolUse"][0]["hooks"][0];
    let named = h["command"].as_str().unwrap().replace('\\', "/").to_lowercase();
    assert_eq!(named, copy.display().to_string().replace('\\', "/").to_lowercase(), "{h}");
    let event = r#"{"tool_name":"Bash","tool_input":{"command":"rm -f $OUT/x"}}"#;
    let o = guard(&env, &copy, &["guard", "rm"], event);
    assert!(String::from_utf8_lossy(&o.stdout).contains("\"deny\""), "{}", text(&o));
    let again = text(&sync(false));
    assert!(!again.contains("installs the guard"), "{again}");
    // Sol-loop r2, S13: a check of the synced settings reports no conflict for them.
    let check = env.lords(
        d,
        &[
            "check",
            "--public-stack",
            public.to_str().unwrap(),
            "--private-stack",
            private.to_str().unwrap(),
        ],
    );
    let out = text(&check);
    assert!(!out.contains("guards need") && !out.contains("settings.json"), "{out}");
    // S12: the copy gone, a sync puts it back (the settings name it).
    fs::remove_file(&copy).unwrap();
    let o = sync(true);
    assert!(o.status.success() && copy.is_file(), "{}", text(&o));
    // S11: a staged copy an interrupted install left doesn't stop the next one.
    fs::remove_file(&copy).unwrap();
    fs::write(copy.with_file_name(".lords-guard.exe.exe"), b"cut").unwrap();
    let o = sync(true);
    assert!(o.status.success() && copy.is_file(), "{}", text(&o));
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
