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

    /// `lords <first…> <the stacks> <more…>`.
    fn with_stacks(&self, first: &[&str], more: &[&str], env: &[(&str, &str)]) -> Output {
        let (public, private) = (self.path("pub"), self.path("priv"));
        let stacks = ["--public-stack", public.to_str().unwrap()];
        let stacks = [&stacks[..], &["--private-stack", private.to_str().unwrap()]].concat();
        self.lords(&[first, &stacks, more].concat(), env)
    }

    /// `lords sync --user`, applying or not.
    fn sync(&self, apply: bool, env: &[(&str, &str)]) -> Output {
        let more: &[&str] = if apply { &["--apply"] } else { &[] };
        self.with_stacks(&["sync", "--user"], more, env)
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

fn out(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
}

/// The count in sync's line `applied, N Codex hook(s) not trusted`; 0 without one.
fn not_trusted(o: &Output) -> usize {
    let text = out(o);
    let count = text.lines().find_map(|l| {
        l.strip_prefix("applied, ")?.strip_suffix(" Codex hook(s) not trusted")?.parse().ok()
    });
    count.unwrap_or(0)
}

fn own(command: &str) -> Value {
    json!({"hooks": [{"type": "command", "command": command}]})
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

// Spec (design note §3 I3; sol-loop r1 S1): a user's own hook added to Lords' `time` entry, after Lords' hook, keeps its
// slot when sync adds `rm`: that entry stays as it was, and `rm` comes after it.
#[test]
fn a_hook_sharing_lords_entry_keeps_its_slot() {
    let t = T::new("[\"time\"]");
    assert!(t.sync(true, &[]).status.success());
    let mut hooks = t.hooks();
    let audit = json!({"type": "command", "command": "audit"});
    hooks["hooks"]["PreToolUse"][0]["hooks"].as_array_mut().unwrap().push(audit);
    t.set_hooks(&hooks);
    t.guards("[\"rm\", \"time\"]");
    let o = t.sync(true, &[]);
    let pre = t.hooks()["hooks"]["PreToolUse"].clone();
    assert_eq!(pre[0], hooks["hooks"]["PreToolUse"][0], "{}", said(&o));
    assert!(pre[1]["hooks"][0]["command"].as_str().unwrap().contains(" guard rm"), "{pre}");
}

// Spec (design note §3 I1: every hook of the file sync wrote is trusted or named; sol-loop r1 S2): when Codex lists no
// hook of that file (its hooks switched off), nothing is trusted and each hook of the file is named as one Codex doesn't
// list, so it doesn't run; sync's last line counts them, exit 1.
#[test]
fn a_hook_codex_doesnt_list_is_named() {
    let t = T::new("[\"rm\", \"time\"]");
    let o = t.sync(true, &[("LORDS_STUB_MODE", "empty")]);
    assert_eq!((o.status.code(), not_trusted(&o)), (Some(1), 2), "{}", said(&o));
    let named = out(&o).lines().filter(|l| l.contains("Codex doesn't list it")).count();
    assert_eq!(named, 2, "{}", said(&o));
    assert!(t.calls("config/batchWrite").is_empty(), "{}", said(&o));
}

// Spec (design note §2.2, §3 I1, I2, §4 A1, A2, A4, A10; the owner's grant F11, 2 Nov 18:58: re-trust "limited to hooks
// that call Lords' own guard program"): after sync writes Codex's hooks file, it trusts exactly the hooks Codex
// lists in that file whose command is byte for byte one Lords renders for a declared guard, on that guard's event. Here
// that is Lords' new `time` hook, appended after a user's two entries, under the key Codex lists for it
// (`pre_tool_use:2:0`) and the hash Codex lists, in one `config/batchWrite` of `hooks.state` (upsert, config reloaded)
// that holds nothing else. Every lookalike of Lords' role command another writer added (another dir, `;`, `&&`, `|`, a
// second line, a redirection, other quoting, an extra or a missing argument, another guard, a trailing space, another
// case), Lords' role command on another event, and the user's own hooks are never trusted: each is named, and sync's last
// line counts them, exit 1. Lords' exact command in another hooks file (a project's) is never trusted. Codex's
// config.toml is in the run's backup as it was before the write, and `--restore` leaves it as Codex wrote it.
#[test]
fn sync_trusts_only_lords_own_guard_hooks_in_the_file_it_wrote() {
    let t = T::new("[\"role\"]");
    assert!(t.sync(true, &[]).status.success());
    let role = t.hooks()["hooks"]["UserPromptSubmit"][0]["hooks"][0]["command"].clone();
    let role = role.as_str().unwrap().to_string();
    let mut hooks = t.hooks();
    hooks["hooks"]["PreToolUse"] = json!([own("own-a"), own("own-b")]);
    t.set_hooks(&hooks);
    let quoted =
        if role.contains('\'') { role.replace('\'', "\"") } else { role.replace('"', "'") };
    let lookalikes = [
        role.replace("/bin/lords-guard.exe", "/other/lords-guard.exe"),
        format!("{role}; echo x"),
        format!("{role} && echo x"),
        format!("{role} | echo x"),
        format!("{role}\necho x"),
        format!("{role} > out.txt"),
        quoted,
        format!("{role} --x"),
        role.replace(" --adapter codex", ""),
        role.replace(" guard role", " guard roles"),
        format!("{role} "),
        role.replace("lords-guard.exe", "LORDS-GUARD.EXE"),
    ];
    assert!(lookalikes.iter().all(|l| *l != role), "{lookalikes:?}");
    let add: Vec<Value> = lookalikes.iter().map(|c| own(c)).collect();
    let add = json!({"UserPromptSubmit": add, "PreToolUse": [own(&role)]});
    fs::write(t.path("add.json"), add.to_string()).unwrap();
    fs::create_dir_all(t.path("home/.codex/.codex")).unwrap();
    let project = json!({"hooks": {"UserPromptSubmit": [own(&role)]}});
    fs::write(t.path("home/.codex/.codex/hooks.json"), project.to_string()).unwrap();
    let config = fs::read(t.path("home/.codex/config.toml")).unwrap_or_default();

    t.guards("[\"role\", \"time\"]");
    let add = t.path("add.json");
    let o = t.sync(true, &[("LORDS_STUB_ADD", add.to_str().unwrap())]);
    let writes = t.calls("config/batchWrite");
    assert_eq!(writes.len(), 2, "role's at the first sync, time's now: {}", said(&o));
    let listed = t.stub("list");
    let at = |h: &&Value| h["key"].as_str().unwrap().ends_with(":pre_tool_use:2:0");
    let time = listed.as_array().unwrap().iter().find(at).unwrap_or_else(|| panic!("{listed}"));
    assert!(time["command"].as_str().unwrap().contains(" guard time"), "{listed}");
    let mut want = serde_json::Map::new();
    want.insert(time["key"].as_str().unwrap().into(), json!({"trusted_hash": time["currentHash"]}));
    let edit = json!({"keyPath": "hooks.state", "value": want, "mergeStrategy": "upsert"});
    let want = json!({"edits": [edit], "reloadUserConfig": true});
    assert_eq!(writes[1]["params"], want, "Lords' time hook alone, as listed");
    assert_eq!(time["trustStatus"], "trusted", "{listed}");
    let others = lookalikes.len() + 3;
    assert_eq!((o.status.code(), not_trusted(&o)), (Some(1), others), "{}", said(&o));
    let text = out(&o);
    assert_eq!(text.lines().filter(|l| l.contains("not Lords'")).count(), others, "{text}");
    assert!(!text.contains(".codex/.codex") && !text.contains(".codex\\.codex"), "{text}");

    let line = text.lines().find_map(|l| l.strip_prefix("backups of the owner files changed: "));
    let backups = PathBuf::from(line.unwrap().split(" (").next().unwrap());
    let mut kept = fs::read_dir(&backups).unwrap().map(|f| fs::read(f.unwrap().path()).unwrap());
    assert!(kept.any(|b| b == config), "config.toml as it was, in {}", backups.display());
    let written = fs::read(t.path("home/.codex/config.toml")).unwrap();
    let o = t.with_stacks(&["sync", "--restore", backups.to_str().unwrap()], &[], &[]);
    assert!(o.status.success(), "{}", said(&o));
    assert_eq!(fs::read(t.path("home/.codex/config.toml")).unwrap(), written, "Codex's own");
}

// Spec (design note §3 I4, §4 A7; rule.remote-calls-have-deadlines: "A missing reading fails closed, never as fine"): when
// Codex can't be asked after sync wrote its hooks file (no such program; it ends without answering; it answers past the
// deadline, and is ended; it has no `hooks/list`; it answers a list that isn't one) or its config write fails, the hooks
// file keeps what sync wrote, no hook is called trusted, each is named with the reason, and sync's last line counts
// them, exit 1. Only the refused write was attempted, once.
#[test]
fn sync_fails_closed_when_codex_cant_be_asked() {
    let t = T::new("[\"time\"]");
    let missing = t.path("no-codex.exe");
    let cases: [(&str, (&str, &str), &str); 6] = [
        ("no program", ("LORDS_CODEX", missing.to_str().unwrap()), "no-codex.exe"),
        ("ends at once", ("LORDS_STUB_MODE", "exit"), "ended without answering"),
        ("past its deadline", ("LORDS_STUB_TRUST", "hang"), "deadline"),
        ("no hooks/list", ("LORDS_STUB_MODE", "nolist"), "error"),
        ("no list", ("LORDS_STUB_MODE", "malformed"), "list"),
        ("its write refused", ("LORDS_STUB_MODE", "writeerr"), "error"),
    ];
    let mut guards = ["[\"rm\", \"time\"]", "[\"time\"]"].into_iter().cycle();
    for (what, env, why) in cases {
        let wanted = guards.next().unwrap();
        t.guards(wanted);
        let started = std::time::Instant::now();
        let o = t.with_stacks(&["sync", "--user", "--apply", "--deadline-ms", "1000"], &[], &[env]);
        let held = wanted.matches(',').count() + 1;
        assert_eq!((o.status.code(), not_trusted(&o)), (Some(1), held), "{what}: {}", said(&o));
        let written = t.hooks()["hooks"]["PreToolUse"].as_array().unwrap().len();
        assert_eq!(written, held, "{what}: the write stays");
        let text = out(&o);
        let named: Vec<&str> = text.lines().filter(|l| l.contains("not known trusted")).collect();
        assert!(named.len() == held && named.iter().all(|l| l.contains(why)), "{what}: {text}");
        assert!(started.elapsed() < std::time::Duration::from_secs(15), "{what}: {text}");
    }
    assert_eq!(t.calls("config/batchWrite").len(), 1, "the refused write alone");
}

// Spec (design note §2.5, §4 A6, A11): with `rm` and `time` synced and trusted and a user's own PreToolUse hook after
// them, taking `rm` off the stack moves `time` and the user's hook up one slot. A dry run says it would trust Lords'
// `time` hook and that the user's hook falls, and neither writes nor asks Codex anything; the apply trusts `time` at its
// new slot, alone, and names the user's hook as not trusted, exit 1.
#[test]
fn taking_a_guard_off_names_the_hook_it_moves() {
    let t = T::new("[\"rm\", \"time\"]");
    assert!(t.sync(true, &[]).status.success());
    let mut hooks = t.hooks();
    let mine = json!({"matcher": "Bash", "hooks": [{"type": "command", "command": "own-check"}]});
    hooks["hooks"]["PreToolUse"].as_array_mut().unwrap().push(mine.clone());
    t.set_hooks(&hooks);
    t.stub("trust-all");
    t.guards("[\"time\"]");
    let log = t.path("home/.codex/stub-calls.jsonl");
    let (asked, before) = (fs::read(&log).unwrap_or_default(), t.hooks());
    let o = t.sync(false, &[]);
    let text = out(&o);
    let says = |a: &str, b: &str| {
        text.lines().any(|l| l.contains("Codex") && l.contains(a) && l.contains(b))
    };
    assert!(says("time guard", "trust") && says("PreToolUse", "falls"), "{text}");
    assert_eq!((fs::read(&log).unwrap_or_default(), t.hooks()), (asked, before), "a dry run");

    let o = t.sync(true, &[]);
    assert_eq!(t.hooks()["hooks"]["PreToolUse"][1], mine);
    let writes = t.calls("config/batchWrite");
    let keys = writes.last().unwrap()["params"]["edits"][0]["value"].as_object().unwrap().clone();
    assert!(keys.len() == 1 && keys.keys().all(|k| k.ends_with(":pre_tool_use:0:0")), "{keys:?}");
    assert_eq!((o.status.code(), not_trusted(&o)), (Some(1), 1), "{}", said(&o));
    let named = |l: &&str| l.contains("pre_tool_use:1:0") && l.contains("not Lords'");
    assert!(out(&o).lines().any(|l| named(&l)), "{}", said(&o));
}

// Spec (design note §2.2, §4 A8: "enabled is false. Not re-trusted, reported as disabled"): Lords' own hook that Codex
// lists switched off is never trusted: no config write; sync names it as disabled and counts it, exit 1.
#[test]
fn a_switched_off_lords_hook_stays_off() {
    let t = T::new("[\"time\"]");
    let o = t.sync(true, &[("LORDS_STUB_DISABLED", "preToolUse")]);
    assert!(t.calls("config/batchWrite").is_empty(), "{}", said(&o));
    assert_eq!((o.status.code(), not_trusted(&o)), (Some(1), 1), "{}", said(&o));
    let named = out(&o).lines().any(|l| l.contains("time guard") && l.contains("disabled"));
    assert!(named, "{}", said(&o));
}

// Spec (design note §4 A3: "Lords writes the listed hash, so Codex treats the changed hook as modified. The read-back
// names it. No retry loop"): another writer changes Lords' new `time` hook between Codex's list and Lords' write. Lords'
// one write carries the hash it was listed with; the read-back finds the hook modified and no longer Lords' command, and
// names it, exit 1; no second write.
#[test]
fn a_hook_changed_after_the_list_is_named() {
    let t = T::new("[\"time\"]");
    let o = t.sync(true, &[("LORDS_STUB_MODE", "race")]);
    let writes = t.calls("config/batchWrite");
    assert_eq!(writes.len(), 1, "{}", said(&o));
    let listed = t.stub("list");
    let raced = &listed[0];
    assert!(raced["command"].as_str().unwrap().ends_with(" --raced"), "{listed}");
    assert_eq!(raced["trustStatus"], "modified", "{listed}");
    assert_eq!((o.status.code(), not_trusted(&o)), (Some(1), 1), "{}", said(&o));
    let key = raced["key"].as_str().unwrap();
    assert!(out(&o).lines().any(|l| l.contains(key) && l.contains("not Lords'")), "{}", said(&o));
}

// Spec (design note §2.4; D17: "`lords paths` checks only the role hook"): `lords paths` gives Codex's row complete when
// Codex trusts every Lords hook in its hooks file, and incomplete, naming each Lords hook that doesn't run by guard,
// event and status, when Codex lists the PreToolUse ones switched off, though the role hook is trusted.
#[test]
fn paths_reads_trust_for_every_lords_codex_hook() {
    let t = T::new("[\"rm\", \"role\", \"time\"]");
    assert!(t.sync(true, &[]).status.success());
    t.stub("trust-all");
    let paths = |env: &[(&str, &str)]| {
        let o = t.with_stacks(&["paths", "--json", "--codex", STUB], &[], env);
        let rows: Value = serde_json::from_slice(&o.stdout).unwrap();
        let codex = |r: &&Value| r["path"].as_str().unwrap().contains("codex");
        let codex = rows.as_array().unwrap().iter().find(codex).unwrap_or_else(|| panic!("{rows}"));
        let s = |k: &str| codex[k].as_str().unwrap().to_string();
        (o.status.code(), s("state"), s("why"))
    };
    let (code, state, why) = paths(&[]);
    assert_eq!((code, state.as_str()), (Some(0), "complete"), "{why}");
    let (code, state, why) = paths(&[("LORDS_STUB_DISABLED", "preToolUse")]);
    assert_eq!((code, state.as_str()), (Some(1), "incomplete"), "{why}");
    let names = |g: &str| why.contains(&format!("{g} (preToolUse) disabled"));
    assert!(names("rm") && names("time") && !why.contains("role ("), "{why}");
}
