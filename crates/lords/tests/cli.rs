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
