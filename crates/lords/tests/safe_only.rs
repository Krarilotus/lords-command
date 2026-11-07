//! `lords sync --safe-only` end to end (D13, design note Harness/notes/2026-11-07-lords-d13-typed-safety.md): it writes
//! only what the planner typed safe, holds every place where anything else is wanted, names each with its reasons, and
//! exits 0 when no place is held, 1 when one is, 2 when refused. Child processes get an empty home and global git
//! config, so no real home dir, user hook or setting is read or touched.

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
            format!("[user]\n\tname = Test\n\temail = {email}\n[init]\n\tdefaultBranch = main\n"),
        )
        .unwrap();
        fs::create_dir_all(dir.path().join("home")).unwrap();
        Env { dir }
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

    /// `lords sync` on this env's stacks, with `args`.
    fn sync(&self, args: &[&str]) -> Output {
        let (public, private) = (self.path("pub"), self.path("priv"));
        let stacks = [
            "--public-stack",
            public.to_str().unwrap(),
            "--private-stack",
            private.to_str().unwrap(),
        ];
        let all = [&["sync"][..], &stacks[..], args].concat();
        self.cmd(env!("CARGO_BIN_EXE_lords"), self.dir.path()).args(all).output().unwrap()
    }

    fn git(&self, cwd: &Path, args: &[&str]) -> String {
        let o = self.cmd("git", cwd).args(args).output().unwrap();
        assert!(o.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&o.stderr));
        String::from_utf8_lossy(&o.stdout).into_owned()
    }

    fn checkout(&self, name: &str) -> PathBuf {
        let p = self.path(name);
        self.write(&format!("{name}/README.md"), "a place\n");
        self.git(&p, &["init", "-q"]);
        self.git(&p, &["add", "-A"]);
        self.git(&p, &["commit", "-q", "-m", "start"]);
        p
    }

    fn rule(&self, body: &str) {
        self.write(
            "pub/global/rules/r.md",
            &format!("---\nid: rule.r\nkind: rule\ntitle: rule.r\naudience: [all]\nroles: [coder]\nscope: global\nstatus: active\nvisibility: public\norigin:\n  type: own\n---\n{body}"),
        );
    }

    /// Checkouts `a` and `b` of repo `beta-core`, with a public rule, synced once by hand; `more` goes in the private
    /// stack.toml.
    fn synced(&self, more: &str) -> (PathBuf, PathBuf) {
        self.write(
            "pub/stack.toml",
            "name = \"pub\"\nvisibility = \"public\"\nroles = [\"coder\"]\n",
        );
        self.rule("Rule one.\n");
        let (a, b) = (self.checkout("a"), self.checkout("b"));
        self.write(
            "priv/stack.toml",
            &format!(
                "name = \"priv\"\nvisibility = \"private\"\nroles = [\"coder\"]\n[projects.beta]\n[repos.beta-core]\nproject = \"beta\"\ndefault_role = \"coder\"\ncheckouts = ['{}', '{}']\nworktrees = false\n{more}",
                a.display(),
                b.display()
            ),
        );
        let o = self.sync(&["--apply"]);
        assert!(o.status.success(), "{}", text(&o));
        assert!(read(&a.join("CLAUDE.local.md")).contains("Rule one."), "{}", text(&o));
        (a, b)
    }
}

fn text(o: &Output) -> String {
    format!("{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr))
}

fn read(p: &Path) -> String {
    fs::read_to_string(p).unwrap_or_default()
}

// Spec (note C5; brief point 2: "Any place holding a needs-owner item is held whole: none of its files change. The
// command prints each held place with its reasons, and exits 0 when nothing is held, 1 when something is"): a checkout
// whose place file its owner changed is held and named with its reason, dry run and apply alike, exit 1; the other
// checkout is written. Once the owner deletes the file, nothing is held, both are written, and it exits 0. `--json`
// gives each item its class and the plan what it held.
#[test]
fn a_held_place_is_named_left_whole_and_exits_1() {
    let env = Env::new();
    let (a, b) = env.synced("");
    let mine = format!("{}My own line.\n", read(&a.join("CLAUDE.local.md")));
    fs::write(a.join("CLAUDE.local.md"), &mine).unwrap();
    let codex = read(&a.join("AGENTS.override.md"));
    env.rule("Rule two.\n");

    let dry = env.sync(&["--safe-only"]);
    assert_eq!(dry.status.code(), Some(1), "{}", text(&dry));
    let out = text(&dry);
    let line = out.lines().find(|l| l.starts_with("held") && l.contains(&*a.to_string_lossy()));
    assert!(line.is_some(), "{out}");
    assert!(out.contains("changed-copy"), "{out}");
    assert!(read(&b.join("CLAUDE.local.md")).contains("Rule one."), "a dry run writes nothing");

    let json = env.sync(&["--safe-only", "--json"]);
    assert_eq!(json.status.code(), Some(1), "{}", text(&json));
    let v: serde_json::Value = serde_json::from_slice(&json.stdout).unwrap();
    let places = v["held"]["places"].as_array().unwrap();
    assert_eq!(places.len(), 1, "{v}");
    assert_eq!(places[0]["items"][0]["reason"], "changed-copy");
    let items = v["items"].as_array().unwrap();
    assert!(!items.is_empty() && items.iter().all(|i| i["safety"]["class"] == "safe"), "{v}");

    let o = env.sync(&["--safe-only", "--apply"]);
    assert_eq!(o.status.code(), Some(1), "{}", text(&o));
    assert_eq!(read(&a.join("CLAUDE.local.md")), mine);
    assert_eq!(read(&a.join("AGENTS.override.md")), codex, "the held place is left whole");
    assert!(read(&b.join("CLAUDE.local.md")).contains("Rule two."), "{}", text(&o));

    fs::remove_file(a.join("CLAUDE.local.md")).unwrap();
    let o = env.sync(&["--safe-only", "--apply"]);
    assert_eq!(o.status.code(), Some(0), "{}", text(&o));
    for file in ["CLAUDE.local.md", "AGENTS.override.md"] {
        assert!(read(&a.join(file)).contains("Rule two."), "{file}: {}", text(&o));
    }
}

// Spec (note D3, the Lords owner 7 Nov: "Refusing `--adapter` and `--stage`: yes"): `--safe-only` judges each place
// whole, so it refuses `--adapter`, which plans part of one, and `--stage`, another policy: exit 2, nothing written.
#[test]
fn safe_only_refuses_a_part_of_a_place_and_another_stage() {
    let env = Env::new();
    let (_, b) = env.synced("");
    env.rule("Rule two.\n");
    for extra in [&["--adapter", "claude"][..], &["--stage", "install"], &["--stage", "full"]] {
        let o = env.sync(&[&["--safe-only", "--apply"][..], extra].concat());
        assert_eq!(o.status.code(), Some(2), "{extra:?}: {}", text(&o));
        assert!(read(&b.join("CLAUDE.local.md")).contains("Rule one."), "{extra:?}");
    }
}

// Spec (note D1, the Lords owner 7 Nov: "Memory imports and retirements are listed separately as pending store work,
// and `--safe-only` never does them: a delivery triggered by a commit must not itself commit, or it loops"): a memory
// store with a new note is listed as store work and not imported, no commit lands in the store, the places are
// written, and with no place held it exits 0.
#[test]
fn store_work_is_listed_never_done_and_holds_nothing() {
    let env = Env::new();
    let mem = env.path("agent-memory");
    fs::create_dir_all(&mem).unwrap();
    let store = format!(
        "[[memory_stores]]\nname = \"agent\"\npath = '{}'\nscope = \"global\"\n",
        mem.display()
    );
    let (a, _) = env.synced(&store);
    let private = env.path("priv");
    env.git(&private, &["init", "-q"]);
    env.git(&private, &["add", "-A"]);
    env.git(&private, &["commit", "-q", "-m", "start"]);
    let head = env.git(&private, &["rev-parse", "HEAD"]);
    env.write("agent-memory/note.md", "---\nname: note\ndescription: a note\n---\nThe note.\n");
    env.rule("Rule two.\n");

    let o = env.sync(&["--safe-only", "--apply"]);
    assert_eq!(o.status.code(), Some(0), "{}", text(&o));
    let out = text(&o);
    let rest = out.split_once("store work").map(|x| x.1).unwrap_or_else(|| panic!("{out}"));
    assert!(rest.lines().any(|l| l.contains("memory store agent") && l.contains("1 new")), "{out}");
    assert!(!out.contains("imports them"), "--safe-only never imports: {out}");
    assert!(!out.lines().any(|l| l.starts_with("held")), "{out}");
    assert!(read(&a.join("CLAUDE.local.md")).contains("Rule two."), "{out}");
    assert_eq!(env.git(&private, &["rev-parse", "HEAD"]), head, "no commit in the store");
    assert_eq!(env.git(&private, &["status", "--porcelain"]), "", "nothing imported");
}
