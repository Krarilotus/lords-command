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
