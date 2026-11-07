//! Delivery integrity through the binary (design note Harness/notes/2026-11-07-lords-delivery-integrity.md): sync claims
//! no hook Lords didn't render (D18). Every child gets the fixture's home and git config, so no real home, setting or
//! guard copy is read or touched.

// Tests are outside the filesystem boundary (fsguard.rs): their fixtures write their own temp dirs.
#![allow(clippy::disallowed_methods, clippy::disallowed_types)]

use serde_json::{json, Value};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use tempfile::TempDir;

const SENTINEL: &str = "SYNTHETIC_SENTINEL";

/// A temp dir with a home (`home/`), a public stack (`pub/`) and a private stack (`priv/`, a git repo).
struct Home {
    dir: TempDir,
}

impl Home {
    /// The public stack holds skills `skills`; the private one declares `targets`.
    fn new(skills: &[&str], targets: &str) -> Home {
        let h = Home { dir: TempDir::new().unwrap() };
        let email = ["test", "example.invalid"].join("@");
        h.write("gitconfig", &format!("[user]\n\tname = Test\n\temail = {email}\n"));
        h.write("pub/stack.toml", "name = \"pub\"\nvisibility = \"public\"\n");
        fs::create_dir_all(h.path("home")).unwrap();
        for s in skills {
            h.skill(s);
        }
        h.declare(targets);
        h
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

    fn skill(&self, name: &str) {
        self.write(
            &format!("pub/global/skills/{name}/SKILL.md"),
            &format!("---\nname: {name}\n---\nX\n"),
        );
        self.write(
            &format!("pub/global/skills/{name}/lords.yaml"),
            &format!("id: skill.{name}\nkind: skill\ntitle: {name}\naudience: [all]\nscope: global\nstatus: active\nvisibility: public\norigin:\n  type: own\n"),
        );
    }

    /// The private stack declaring `targets`, committed.
    fn declare(&self, targets: &str) {
        self.write(
            "priv/stack.toml",
            &format!("name = \"priv\"\nvisibility = \"private\"\n{targets}"),
        );
        let p = self.path("priv");
        if !p.join(".git").exists() {
            assert!(self.git(&p, &["init", "-q"]).status.success());
        }
        self.git(&p, &["add", "-A"]);
        self.git(&p, &["commit", "-q", "-m", "targets"]);
    }

    fn cmd(&self, program: &Path) -> Command {
        let mut c = Command::new(program);
        c.current_dir(self.dir.path())
            .env("GIT_CONFIG_GLOBAL", self.path("gitconfig"))
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("HOME", self.path("home"))
            .env("USERPROFILE", self.path("home"))
            // The Codex sync asks after it writes Codex's hooks: the stub, never one on PATH (tests/codex_trust.rs).
            .env("LORDS_CODEX", env!("CARGO_BIN_EXE_lords-codex-stub"));
        c
    }

    fn git(&self, cwd: &Path, args: &[&str]) -> Output {
        self.cmd(Path::new("git")).current_dir(cwd).args(args).output().unwrap()
    }

    fn lords(&self, args: &[&str]) -> Output {
        self.cmd(Path::new(env!("CARGO_BIN_EXE_lords"))).args(args).output().unwrap()
    }

    /// `lords` with the fixture's two stacks.
    fn stacked(&self, args: &[&str]) -> Output {
        let (p, q) = (self.path("pub"), self.path("priv"));
        let stacks =
            ["--public-stack", p.to_str().unwrap(), "--private-stack", q.to_str().unwrap()];
        self.lords(&[args, &stacks].concat())
    }

    /// The guard copy hooks run, in the fixture's tool home.
    fn copy(&self) -> PathBuf {
        self.path("home/.lords-command/bin/lords-guard.exe")
    }
}

fn text(o: &Output) -> String {
    format!("{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr))
}

fn settings(adapter: &str, path: &Path, guards: &str) -> String {
    format!(
        "[[targets]]\nname = \"{adapter} settings\"\nadapter = \"{adapter}\"\nkind = \"settings\"\npath = '{}'\nguards = [{guards}]\n",
        path.display()
    )
}

// Spec (D18, lords-wakeups.md: "A user's `<Lords cmd>; echo x` in a settings file is rewritten to Lords' command or
// dropped at the next sync … It closes when ownership is byte equality with a command Lords rendered … and anything
// else is left alone and named"; design note §1, A18-2, A18-8): with the rm guard wanted and installed, a user's hook
// running the copy with args of its own, under Lords' matcher, stays byte for byte in its entry; Lords' rm hook goes in
// an entry of its own after it; the plan names the user's hook by its place and never shows its text.
#[test]
fn a_users_hook_on_the_guard_copy_stays_beside_lords_own() {
    let h = Home::new(&[], "");
    let file = h.path("home/.claude/settings.json");
    h.declare(&settings("claude", &file, "\"rm\""));
    assert!(h.lords(&["guard", "install"]).status.success());
    let e = h.copy().display().to_string().replace('\\', "/");
    let mine = json!({"matcher": "Bash|PowerShell", "hooks": [{"type": "command", "command": e, "args": ["guard", "rm", "--log", SENTINEL]}]});
    h.write("home/.claude/settings.json", &json!({"hooks": {"PreToolUse": [mine]}}).to_string());
    let o = h.stacked(&["sync", "--user", "--apply"]);
    let v: Value = serde_json::from_str(&fs::read_to_string(&file).unwrap()).unwrap();
    let lords = json!({"matcher": "Bash|PowerShell", "hooks": [{"type": "command", "command": e, "args": ["guard", "rm"]}]});
    assert_eq!(v["hooks"]["PreToolUse"], json!([mine, lords]), "{}", text(&o));
    assert!(o.status.success(), "{}", text(&o));
    let o = h.stacked(&["sync", "--user"]);
    assert!(text(&o).contains("`hooks.PreToolUse` entry 1"), "{}", text(&o));
    assert!(!text(&o).contains(SENTINEL), "{}", text(&o));
}
