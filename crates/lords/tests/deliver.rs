//! F8 through the binary and real git hooks (design note Harness/notes/2026-11-07-lords-f8-delivery.md): a store commit
//! reaches the places with nobody running anything; the delivery's own store commit starts no second one; `sync --apply`
//! gives each declared repo the post-checkout hook, through which a new worktree is delivered; `lords deliver` and
//! `lords paths` name a held place with the command that resolves it. Every child gets the fixture's home and git
//! config, so no real home, setting, hook or guard copy is read or touched.

// Tests are outside the filesystem boundary (fsguard.rs): their fixtures write their own temp dirs.
#![allow(clippy::disallowed_methods, clippy::disallowed_types)]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::{Duration, Instant};
use tempfile::TempDir;

/// A temp dir with a home (`home/`, its tool home `home/.lords-command` and that home's config.txt naming the stacks),
/// a public store (`pub/`) and a private one (`priv/`), both git repos, and repo `r` (`repo/`, worktrees under `wts/`)
/// declared in the private stack with a rule for its default role.
struct World {
    dir: TempDir,
}

impl World {
    /// `more` goes at the end of the private stack.toml.
    fn new(more: &str) -> World {
        let w = World { dir: TempDir::new().unwrap() };
        let email = ["test", "example.invalid"].join("@");
        w.write(
            "gitconfig",
            &format!("[user]\n\tname = Test\n\temail = {email}\n[init]\n\tdefaultBranch = main\n"),
        );
        w.write("pub/stack.toml", "name = \"pub\"\nvisibility = \"public\"\nroles = [\"coder\"]\n");
        w.init("pub");
        w.write("repo/README.md", "a repo\n");
        w.init("repo");
        fs::create_dir_all(w.path("wts")).unwrap();
        w.write(
            "priv/stack.toml",
            &format!(
                "name = \"priv\"\nvisibility = \"private\"\nroles = [\"coder\"]\n[repos.r]\ncheckouts = ['{}']\nworktree_roots = ['{}']\nadapters = [\"claude\"]\ndefault_role = \"coder\"\n{more}",
                w.path("repo").display(),
                w.path("wts").display()
            ),
        );
        w.rule("Rule one.");
        w.init("priv");
        w.write(
            "home/.lords-command/config.txt",
            &format!(
                "public = {}\nprivate = {}\n",
                w.path("pub").display(),
                w.path("priv").display()
            ),
        );
        w
    }

    fn path(&self, rel: &str) -> PathBuf {
        self.dir.path().join(rel)
    }

    fn write(&self, rel: &str, text: &str) -> PathBuf {
        let p = self.path(rel);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(&p, text).unwrap();
        p
    }

    /// The private rule `rule.r`, for the coder role.
    fn rule(&self, body: &str) {
        self.write(
            "priv/global/rules/r.md",
            &format!("---\nid: rule.r\nkind: rule\ntitle: r\naudience: [all]\nroles: [coder]\nscope: global\nstatus: active\nvisibility: private\norigin:\n  type: own\n---\n{body}\n"),
        );
    }

    fn init(&self, rel: &str) {
        let d = self.path(rel);
        assert!(self.git(&d, &["init", "-q"]).status.success());
        self.commit(rel, "start");
    }

    /// Everything in `rel` committed, through its hooks.
    fn commit(&self, rel: &str, message: &str) -> Output {
        let d = self.path(rel);
        assert!(self.git(&d, &["add", "-A"]).status.success());
        let o = self.git(&d, &["commit", "-q", "-m", message]);
        assert!(o.status.success(), "{}", text(&o));
        o
    }

    fn cmd(&self, program: &Path) -> Command {
        let mut c = Command::new(program);
        c.current_dir(self.dir.path())
            .env("GIT_CONFIG_GLOBAL", self.path("gitconfig"))
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("HOME", self.path("home"))
            .env("USERPROFILE", self.path("home"))
            .env_remove("LORDS_DELIVERING");
        c
    }

    fn git(&self, cwd: &Path, args: &[&str]) -> Output {
        self.cmd(Path::new("git")).current_dir(cwd).args(args).output().unwrap()
    }

    fn lords(&self, cwd: &Path, args: &[&str]) -> Output {
        self.cmd(Path::new(env!("CARGO_BIN_EXE_lords")))
            .current_dir(cwd)
            .args(args)
            .output()
            .unwrap()
    }

    /// The repo's place file.
    fn place(&self) -> PathBuf {
        self.path("repo/CLAUDE.local.md")
    }

    /// The private store's post-commit log.
    fn log(&self) -> PathBuf {
        self.path("priv/.git/lords-post-commit.log")
    }
}

fn text(o: &Output) -> String {
    format!("{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr))
}

fn read(p: &Path) -> String {
    fs::read_to_string(p).unwrap_or_default()
}

/// Waits up to 90 s for `path` to hold `what` (the hooks deliver in the background).
fn holds_within(path: &Path, what: &str) -> bool {
    let until = Instant::now() + Duration::from_secs(90);
    while Instant::now() < until {
        if read(path).contains(what) {
            return true;
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    false
}

/// Waits until a hook's log has as many end lines as start lines and stops growing (at most 120 s).
fn settled(log: &Path) {
    let until = Instant::now() + Duration::from_secs(120);
    let mut before = read(log);
    while Instant::now() < until {
        std::thread::sleep(Duration::from_millis(1500));
        let now = read(log);
        let starts = now.lines().filter(|l| l.ends_with(": lords deliver")).count();
        let ends = now.lines().filter(|l| l.contains(": lords deliver exit ")).count();
        if now == before && starts == ends {
            return;
        }
        before = now;
    }
}

// Spec (brief item 1; note C1: "A commit in a store's own checkout, not a linked worktree, starts one background
// delivery. The commit never waits for it"): with the stores' hooks installed, a commit in the private store reaches the
// repo's place file with nobody running anything; a commit in a linked worktree of the store starts nothing.
#[test]
fn a_store_commit_reaches_its_places_with_nobody_running_anything() {
    let w = World::new("");
    let o = w.lords(w.dir.path(), &["sync", "--apply"]);
    assert!(read(&w.place()).contains("Rule one."), "{}", text(&o));
    let o = w.lords(&w.path("priv"), &["hooks", "install", "--secrets-only"]);
    assert!(o.status.success(), "{}", text(&o));

    w.rule("Rule two.");
    let start = Instant::now();
    w.commit("priv", "rule two");
    eprintln!("the commit, its hooks included: {:?}", start.elapsed());
    assert!(holds_within(&w.place(), "Rule two."), "log: {}", read(&w.log()));
    settled(&w.log());

    let before = read(&w.log());
    let side = w.path("priv-side");
    let o =
        w.git(&w.path("priv"), &["worktree", "add", "-q", "-b", "side", side.to_str().unwrap()]);
    assert!(o.status.success(), "{}", text(&o));
    w.rule("Rule side.");
    fs::copy(w.path("priv/global/rules/r.md"), side.join("global/rules/r.md")).unwrap();
    w.git(&w.path("priv"), &["checkout", "-q", "--", "global/rules/r.md"]);
    w.commit("priv-side", "a side commit");
    std::thread::sleep(Duration::from_secs(3));
    assert_eq!(read(&w.log()), before, "a linked worktree's commit starts no delivery");
    assert!(!read(&w.place()).contains("Rule side."));
}

// Spec (brief item 5; note C7: "The commit runs with LORDS_DELIVERING set, and the post-commit hook exits at once when
// it is set. So the import's commit starts no delivery: no loop"): a store commit whose delivery imports an agent's new
// memory note commits that import in the private store, and the post-commit log shows one delivery, not two.
#[test]
fn the_deliverys_own_store_commit_starts_no_delivery() {
    let mem = TempDir::new().unwrap();
    let store = format!(
        "[[memory_stores]]\nname = \"agent\"\npath = '{}'\nscope = \"global\"\n",
        mem.path().display()
    );
    let w = World::new(&store);
    let o = w.lords(w.dir.path(), &["sync", "--apply"]);
    assert!(read(&w.place()).contains("Rule one."), "{}", text(&o));
    let o = w.lords(&w.path("priv"), &["hooks", "install", "--secrets-only"]);
    assert!(o.status.success(), "{}", text(&o));

    fs::write(mem.path().join("note.md"), "---\nname: a note\n---\nRemember this.\n").unwrap();
    w.rule("Rule two.");
    w.commit("priv", "rule two");
    assert!(holds_within(&w.place(), "Rule two."), "log: {}", read(&w.log()));
    settled(&w.log());
    let commits = text(&w.git(&w.path("priv"), &["log", "--format=%s"]));
    assert!(commits.contains("Import memory from agent"), "{commits}");
    let log = read(&w.log());
    let starts = log.lines().filter(|l| l.ends_with(": lords deliver")).count();
    assert_eq!(starts, 1, "{log}");
}

// Spec (brief item 2; note C3, D-3, A15): `lords sync --apply` gives a declared repo Lords' post-checkout hook, which runs
// the guard copy's `deliver` for the new worktree; a second sync leaves it as it is; `git worktree add` then gets the
// worktree its place file and role start with nobody running anything.
#[test]
fn sync_gives_each_repo_its_post_checkout_and_a_new_worktree_is_delivered() {
    let w = World::new("");
    let o = w.lords(w.dir.path(), &["guard", "install"]);
    assert!(o.status.success(), "{}", text(&o));
    let copy = w.path("home/.lords-command/bin/lords-guard.exe");
    let o = w.cmd(&copy).args(["deliver", "--protocol"]).output().unwrap();
    assert_eq!(String::from_utf8_lossy(&o.stdout).trim(), "lords deliver 1", "{}", text(&o));

    let o = w.lords(w.dir.path(), &["sync", "--apply"]);
    let hook = w.path("repo/.git/hooks/post-checkout");
    let script = read(&hook);
    assert!(
        script.contains("lords-guard.exe")
            && script.contains("deliver")
            && script.contains("--new-worktree"),
        "{}\n{script}",
        text(&o)
    );
    let o = w.lords(w.dir.path(), &["sync", "--apply"]);
    assert_eq!(read(&hook), script, "a current hook stays as it is: {}", text(&o));

    let one = w.path("wts/one");
    let o = w.git(&w.path("repo"), &["worktree", "add", "-q", "-b", "one", one.to_str().unwrap()]);
    assert!(o.status.success(), "{}", text(&o));
    let log = w.path("repo/.git/lords-post-checkout.log");
    assert!(holds_within(&one.join("CLAUDE.local.md"), "Rule one."), "log: {}", read(&log));
    assert!(holds_within(&one.join(".lords/start-coder.md"), "Rule one."), "log: {}", read(&log));
}

// Spec (brief item 4; note C6: "each held place … and the command that resolves the place"): a place its owner changed
// is held by `lords deliver` (exit 1), which names it with the sync by hand that resolves it; `lords paths` shows it
// stale with that command (exit 1). Nobody reads a log to learn it.
#[test]
fn deliver_and_paths_name_a_held_place_with_the_command_that_resolves_it() {
    let w = World::new("");
    let o = w.lords(w.dir.path(), &["sync", "--apply"]);
    assert!(read(&w.place()).contains("Rule one."), "{}", text(&o));
    let mine = format!("{}My own line.\n", read(&w.place()));
    fs::write(w.place(), &mine).unwrap();
    w.rule("Rule two.");
    w.commit("priv", "rule two");

    let o = w.lords(w.dir.path(), &["deliver"]);
    assert_eq!(o.status.code(), Some(1), "{}", text(&o));
    assert!(text(&o).contains("lords sync --repo r --place"), "{}", text(&o));
    assert_eq!(read(&w.place()), mine, "the owner's copy is never overwritten");

    let o = w.lords(w.dir.path(), &["paths"]);
    let out = text(&o);
    assert_eq!(o.status.code(), Some(1), "{out}");
    assert!(
        out.lines().any(|l| l.starts_with("stale") && l.contains("lords sync --repo r --place")),
        "{out}"
    );
}
