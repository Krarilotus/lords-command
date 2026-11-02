//! Sync and Codex's hook trust (design note Harness/notes/2026-11-02-lords-codex-trust.md): Codex keys a hook's trust by
//! its position and binds it to the hook's hash, so sync keeps every hook in its slot, trusts only Lords' own guard hooks
//! in the hooks file it wrote, through Codex's own writer, and reads trust back after every write. Every run gets an
//! empty home; Codex is the stub (tests/support/codex_stub.rs), so no real config, stack or Codex is read or touched.

// Tests are outside the filesystem boundary (fsguard.rs): their fixtures write their own temp dirs.
#![allow(clippy::disallowed_methods, clippy::disallowed_types)]

use serde_json::{json, Value};
use std::fs;
use std::path::PathBuf;
use std::process::{Command, Output};
use tempfile::TempDir;

const STUB: &str = env!("CARGO_BIN_EXE_lords-codex-stub");

/// A public and a private stack, with one settings target for Codex's `~/.codex/hooks.json`, and a guard copy.
struct T {
    dir: TempDir,
}

impl T {
    fn new(guards: &str) -> T {
        let t = T { dir: TempDir::new().unwrap() };
        for d in ["pub", "priv", "home/.codex"] {
            fs::create_dir_all(t.path(d)).unwrap();
        }
        fs::write(t.path("pub/stack.toml"), "name = \"pub\"\nvisibility = \"public\"\n").unwrap();
        let email = ["test", "example.invalid"].join("@");
        let user = format!("[user]\n\tname = Test\n\temail = {email}\n");
        fs::write(t.path("gitconfig"), user).unwrap();
        t.git(&["init", "-q"]);
        t.guards(guards);
        assert!(t.lords(&["guard", "install"], &[]).status.success());
        t
    }

    fn path(&self, p: &str) -> PathBuf {
        self.dir.path().join(p)
    }

    fn cmd(&self, program: &str) -> Command {
        let mut c = Command::new(program);
        c.current_dir(self.dir.path())
            .env("HOME", self.path("home"))
            .env("USERPROFILE", self.path("home"))
            .env("CODEX_HOME", self.path("home/.codex"))
            .env("GIT_CONFIG_GLOBAL", self.path("gitconfig"))
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("LORDS_CODEX", STUB);
        c
    }

    fn git(&self, args: &[&str]) {
        let o = self.cmd("git").current_dir(self.path("priv")).args(args).output().unwrap();
        assert!(o.status.success(), "git {args:?}: {o:?}");
    }

    fn lords(&self, args: &[&str], env: &[(&str, &str)]) -> Output {
        let mut c = self.cmd(env!("CARGO_BIN_EXE_lords"));
        c.args(args).envs(env.iter().copied());
        c.output().unwrap()
    }

    /// The target's guards (a TOML list), committed.
    fn guards(&self, guards: &str) {
        let manifest = format!("name = \"priv\"\nvisibility = \"private\"\n[[targets]]\nname = \"codex hooks\"\nadapter = \"codex\"\nkind = \"settings\"\npath = \"~/.codex/hooks.json\"\nguards = {guards}\n");
        fs::write(self.path("priv/stack.toml"), manifest).unwrap();
        self.git(&["add", "-A"]);
        self.git(&["commit", "-q", "--allow-empty", "-m", guards]);
    }

    /// `lords sync --user`, applying or not.
    fn sync(&self, apply: bool, env: &[(&str, &str)]) -> Output {
        let (public, private) = (self.path("pub"), self.path("priv"));
        let mut args = vec!["sync", "--user", "--public-stack", public.to_str().unwrap()];
        args.extend(["--private-stack", private.to_str().unwrap()]);
        if apply {
            args.push("--apply");
        }
        self.lords(&args, env)
    }

    fn hooks(&self) -> Value {
        let text = fs::read_to_string(self.path("home/.codex/hooks.json")).unwrap();
        serde_json::from_str(&text).unwrap()
    }

    fn set_hooks(&self, v: &Value) {
        let text = serde_json::to_string_pretty(v).unwrap();
        fs::write(self.path("home/.codex/hooks.json"), text).unwrap();
    }

    /// The stub run as the owner would trust under `/hooks` (`trust-all`), or listing (`list`).
    fn stub(&self, what: &str) -> Value {
        let o = self.cmd(STUB).arg(what).output().unwrap();
        assert!(o.status.success(), "{o:?}");
        serde_json::from_slice(&o.stdout).unwrap_or_default()
    }

    /// The requests the stub was sent with this method, in order.
    fn calls(&self, method: &str) -> Vec<Value> {
        let log = fs::read_to_string(self.path("home/.codex/stub-calls.jsonl")).unwrap_or_default();
        let calls = log.lines().map(|l| serde_json::from_str::<Value>(l).unwrap());
        calls.filter(|r| r["method"] == method).collect()
    }
}

fn said(o: &Output) -> String {
    format!("{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr))
}

/// The trust each listed hook of the user's hooks file has, by its event, group and handler.
fn trust(listed: &Value) -> Vec<(String, String)> {
    let mut v: Vec<(String, String)> = listed
        .as_array()
        .unwrap()
        .iter()
        .map(|h| {
            let mut slot: Vec<&str> = h["key"].as_str().unwrap().rsplitn(4, ':').take(3).collect();
            slot.reverse();
            (slot.join(":"), h["trustStatus"].as_str().unwrap().to_string())
        })
        .collect();
    v.sort();
    v
}

// Spec (design note §2.1, §3 I3, §4 A5; D17, lords-wakeups.md, 2 Nov: "D16's sync put `rm` at PreToolUse slot 0, which
// moved `time` to slot 1, so `time` fell to untrusted without a word"): D17's exact sequence. With `time` synced, a
// user's own PreToolUse hook after it, and both trusted, `rm` added before `time` in the stack's guards leaves `time` in
// slot 0 and the user's hook in slot 1, both still trusted, and `rm` comes after them, in slot 2. Taking `rm` off again
// keeps the other two and their trust; a sync with nothing to change asks Codex nothing.
#[test]
fn adding_rm_before_time_keeps_every_codex_hook_in_its_slot() {
    let t = T::new("[\"time\"]");
    let o = t.sync(true, &[]);
    assert!(o.status.success(), "{}", said(&o));
    let mut hooks = t.hooks();
    let time = hooks["hooks"]["PreToolUse"][0].clone();
    assert!(time["hooks"][0]["command"].as_str().unwrap().contains(" guard time"), "{hooks}");
    let own = json!({"matcher": "Bash", "hooks": [{"type": "command", "command": "own-check"}]});
    hooks["hooks"]["PreToolUse"].as_array_mut().unwrap().push(own.clone());
    t.set_hooks(&hooks);
    t.stub("trust-all");

    t.guards("[\"rm\", \"time\"]");
    let o = t.sync(true, &[]);
    let pre = t.hooks()["hooks"]["PreToolUse"].clone();
    assert_eq!((&pre[0], &pre[1]), (&time, &own), "time, then the user's hook, in place: {pre}");
    assert!(pre[2]["hooks"][0]["command"].as_str().unwrap().contains(" guard rm"), "{pre}");
    assert_eq!(pre.as_array().unwrap().len(), 3, "{pre}");
    assert!(o.status.success(), "{}", said(&o));
    let listed = trust(&t.stub("list"));
    let kept: Vec<_> = listed.iter().filter(|(slot, _)| slot != "pre_tool_use:2:0").collect();
    assert!(kept.iter().all(|(_, s)| s == "trusted") && kept.len() == 2, "{listed:?}");

    t.guards("[\"time\"]");
    let o = t.sync(true, &[]);
    assert!(o.status.success(), "{}", said(&o));
    assert_eq!(t.hooks()["hooks"]["PreToolUse"], json!([time, own]), "rm, the last, goes alone");
    assert_eq!(trust(&t.stub("list")).iter().filter(|(_, s)| s == "trusted").count(), 2);
    let asked = t.calls("hooks/list").len();
    assert!(t.sync(true, &[]).status.success());
    assert_eq!(t.calls("hooks/list").len(), asked, "nothing written, nothing asked");
}
