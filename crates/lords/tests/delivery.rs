//! Delivery integrity through the binary (design note Harness/notes/2026-11-07-lords-delivery-integrity.md): sync claims
//! no hook Lords didn't render (D18) and registers no guard the installed copy can't run (D20); `lords uninstall
//! --target` removes one dropped target's unchanged copies and nothing else (D21); `lords guard install` replaces a copy
//! a hook is running (§4). Every child gets the fixture's home and git config, so no real home, setting or guard copy is
//! read or touched.

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
        self.stacked_with(args, &[])
    }

    /// `lords` with the fixture's two stacks and these variables set (the processes it starts get them too).
    fn stacked_with(&self, args: &[&str], vars: &[(&str, &str)]) -> Output {
        let (p, q) = (self.path("pub"), self.path("priv"));
        let stacks =
            ["--public-stack", p.to_str().unwrap(), "--private-stack", q.to_str().unwrap()];
        let mut c = self.cmd(Path::new(env!("CARGO_BIN_EXE_lords")));
        for (k, v) in vars {
            c.env(k, v);
        }
        c.args([args, &stacks].concat()).output().unwrap()
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

/// The guard copy is a build's from before `guard --guards` (the old-guard stub: `ok` to the self-test, nothing else).
fn old_copy(h: &Home) {
    fs::create_dir_all(h.copy().parent().unwrap()).unwrap();
    fs::copy(env!("CARGO_BIN_EXE_lords-old-guard-stub"), h.copy()).unwrap();
}

// Spec (D20, lords-wakeups.md: "a guard copy older than the build answers a new subcommand … with a usage note and allows
// everything. A sync ahead of `guard install` therefore registers a guard that does nothing, without saying so. It
// closes when sync asks the installed guard copy which subcommands it runs, and refuses or names a hook it can't";
// design note §2, I20, A20-1, A20-2): with a copy from before `guard --guards`, which passes its self-test, a sync that
// would register the time guard writes nothing and fails, naming `lords guard install`; so does one whose copy lists
// guards but not `time`, naming `time`. Once `lords guard install` put this build's copy there, the same sync writes it.
#[test]
fn sync_registers_no_guard_the_installed_copy_cant_run() {
    let h = Home::new(&[], "");
    let file = h.write("home/.claude/settings.json", "{}\n");
    h.declare(&settings("claude", &file, "\"time\""));
    old_copy(&h);
    for list in ["", "rm queue session turn child role helper"] {
        let o = h.stacked_with(&["sync", "--user", "--apply"], &[("LORDS_OLD_GUARD_LIST", list)]);
        assert_eq!(fs::read_to_string(&file).unwrap(), "{}\n", "{list:?}: {}", text(&o));
        assert!(!o.status.success(), "{list:?}: {}", text(&o));
        assert!(text(&o).contains("lords guard install"), "{list:?}: {}", text(&o));
        if !list.is_empty() {
            assert!(text(&o).contains("`time`"), "{list:?}: {}", text(&o));
        }
    }
    assert!(h.lords(&["guard", "install"]).status.success());
    let o = h.stacked(&["sync", "--user", "--apply"]);
    assert!(fs::read_to_string(&file).unwrap().contains("\"time\""), "{}", text(&o));
    assert!(o.status.success(), "{}", text(&o));
}

// Spec (design note §2, I20, A20-3; as the child, role and rm protocols are asked again under the apply lock): a plan
// made while this build's copy was installed doesn't apply once a build's from before `guard --guards` is in its place:
// apply refuses, naming `lords guard install`, and the settings stay as they were.
#[test]
fn a_plan_whose_copy_was_swapped_for_an_older_one_changes_nothing() {
    let h = Home::new(&[], "");
    let file = h.write("home/settings.json", "{}\n");
    h.declare(&settings("claude", &file, "\"helper\""));
    let home = h.path("home/.lords-command");
    lords_core::sync::install_guard(&home, Path::new(env!("CARGO_BIN_EXE_lords"))).unwrap();
    let stacks = lords_core::Stacks::load(&h.path("pub"), Some(&h.path("priv"))).unwrap();
    let opts = lords_core::sync::Options { user_only: true, home, ..Default::default() };
    let plan = lords_core::sync::plan(&stacks, &opts).unwrap();
    old_copy(&h);
    let r = lords_core::sync::apply(&plan);
    assert_eq!(fs::read_to_string(&file).unwrap(), "{}\n", "{r:?}");
    assert!(r.as_ref().is_err_and(|e| e.contains("lords guard install")), "{r:?}");
}

// Spec (design note §2, I20: "a listed guard is one the copy's dispatcher runs"): `lords guard --guards` answers one
// line, `lords guards` and the guards it runs, exit 0: every guard an adapter registers (`Adapter::guards`). Each of them
// answers a usage error with its own usage note, naming it, exit 0 and nothing on stdout.
#[test]
fn the_guard_copy_lists_the_guards_it_runs() {
    let h = Home::new(&[], "");
    let o = h.lords(&["guard", "--guards"]);
    let out = String::from_utf8_lossy(&o.stdout).into_owned();
    let listed: std::collections::BTreeSet<&str> =
        out.trim().strip_prefix("lords guards ").unwrap_or("").split_whitespace().collect();
    let registered: std::collections::BTreeSet<&str> =
        lords_core::adapters::ALL.iter().flat_map(|a| a.guards().iter().copied()).collect();
    assert_eq!(listed, registered, "{}", text(&o));
    assert_eq!(o.status.code(), Some(0), "{}", text(&o));
    for g in listed {
        let o = h.lords(&["guard", g, "--lords-no-such-option"]);
        assert!(
            String::from_utf8_lossy(&o.stderr).contains(&format!("lords guard {g}")),
            "{g}: {}",
            text(&o)
        );
        assert_eq!(o.status.code(), Some(0), "{g}: {}", text(&o));
        assert!(o.stdout.is_empty(), "{g}: {}", text(&o));
    }
}

fn skills_target(name: &str, dir: &Path) -> String {
    format!(
        "[[targets]]\nname = \"{name}\"\nadapter = \"claude\"\nkind = \"skills\"\npath = '{}'\n",
        dir.display()
    )
}

/// Every file below `dir` by its path there (`/` between parts), with its bytes.
fn files(dir: &Path) -> std::collections::BTreeMap<String, Vec<u8>> {
    fn walk(root: &Path, dir: &Path, out: &mut std::collections::BTreeMap<String, Vec<u8>>) {
        for e in fs::read_dir(dir).unwrap() {
            let p = e.unwrap().path();
            if p.is_dir() {
                walk(root, &p, out);
            } else {
                let rel = p.strip_prefix(root).unwrap().to_string_lossy().replace('\\', "/");
                out.insert(rel, fs::read(&p).unwrap());
            }
        }
    }
    let mut out = std::collections::BTreeMap::new();
    walk(dir, dir, &mut out);
    out
}

/// The parts of each ledger record of `scope`, sorted.
fn recorded(h: &Home, scope: &str) -> Vec<String> {
    let ledger = lords_core::ledger::Ledger::load(&h.path("home/.lords-command")).unwrap();
    let mut v: Vec<String> =
        ledger.owned.values().filter(|o| o.scope == scope).map(|o| o.parts.join("/")).collect();
    v.sort();
    v
}

// Spec (D21, lords-wakeups.md: "It closes when sync can uninstall one dropped target … so the legacy `~/.codex/skills`
// target goes and Codex sees one copy"; the Lords owner's brief, 7 Nov: "a way to remove one dropped target's recorded
// paths, the ones that still hold exactly what Lords wrote, and nothing else"; design note §3, I21, A21-1, A21-4, A21-8):
// for a target the stack no longer declares, `lords uninstall --target NAME --apply` removes each copy that holds what
// its record says, with its record; keeps a changed copy with its record; forgets a gone one; and changes nothing else,
// neither a dir of the owner's beside them nor another target's copies or records. Its dry run changes nothing; both
// exit 1 while a record of the target remains. A target the stack declares is refused, exit 2, nothing changed.
#[test]
fn uninstall_target_removes_only_a_dropped_targets_unchanged_copies() {
    let h = Home::new(&["a", "b", "c"], "");
    let (old, new) = (h.path("home/old"), h.path("home/new"));
    h.declare(&format!(
        "{}{}",
        skills_target("old skills", &old),
        skills_target("new skills", &new)
    ));
    let o = h.stacked(&["sync", "--user", "--apply"]);
    assert!(o.status.success() && old.join("a/SKILL.md").is_file(), "{}", text(&o));
    h.declare(&skills_target("new skills", &new));
    h.write("home/old/b/SKILL.md", "mine now\n");
    fs::remove_dir_all(old.join("c")).unwrap();
    h.write("home/old/mine/SKILL.md", "the owner's own\n");
    let before = (files(&old), files(&new));
    let dry = h.stacked(&["uninstall", "--target", "old skills"]);
    assert_eq!((files(&old), files(&new)), before, "a dry run changes nothing: {}", text(&dry));
    let o = h.stacked(&["uninstall", "--target", "old skills", "--apply"]);
    let mut want = before.clone();
    want.0.retain(|k, _| !k.starts_with("a/"));
    assert_eq!((files(&old), files(&new)), want, "{}", text(&o));
    assert_eq!(recorded(&h, "target:old skills"), ["b"], "{}", text(&o));
    assert_eq!(recorded(&h, "target:new skills"), ["a", "b", "c"], "{}", text(&o));
    assert_eq!(o.status.code(), Some(1), "b's record remains: {}", text(&o));
    assert_eq!(dry.status.code(), Some(1), "{}", text(&dry));
    let o = h.stacked(&["uninstall", "--target", "new skills", "--apply"]);
    assert_eq!((files(&old), files(&new)), want, "{}", text(&o));
    assert_eq!(o.status.code(), Some(2), "a declared target: {}", text(&o));
}

// Spec (design note §3, I21, A21-7: "a target has one dir: records naming two refuse the whole run (a false record)"; Sol
// review r3, R3-03: a well-formed but false record must never make sync remove a path outside every target): a ledger
// record of the dropped target under another dir, holding just what it says, refuses the run, exit 2, and nothing is
// removed, neither there nor in the target's own dir. Written after its fix: before `--target` every call was refused.
#[test]
fn uninstall_target_refuses_records_naming_two_dirs() {
    let h = Home::new(&["a"], "");
    let old = h.path("home/old");
    h.declare(&skills_target("old skills", &old));
    assert!(h.stacked(&["sync", "--user", "--apply"]).status.success());
    h.declare("");
    h.write("home/elsewhere/x/SKILL.md", "not a copy of Lords'\n");
    // A record forged by hand into the ledger (format 2): the dir `x` under another root, its tree hash as it is now.
    let (root, path) = (h.path("home/elsewhere"), h.path("home/elsewhere/x"));
    let sha = lords_core::fsguard::tree_sha(&lords_core::fsguard::read_tree(&path).unwrap());
    let file = lords_core::ledger::file(&h.path("home/.lords-command"));
    let mut ledger = fs::read_to_string(&file).unwrap();
    ledger.push_str(&format!(
        "\n[owned.'{}']\nroot = '{}'\nparts = ['x']\nkind = 'dir'\nsha = '{sha}'\nscope = 'target:old skills'\n",
        lords_core::ledger::key(&path),
        root.display()
    ));
    fs::write(&file, ledger).unwrap();
    let before = (files(&old), files(&root));
    let o = h.stacked(&["uninstall", "--target", "old skills", "--apply"]);
    assert_eq!((files(&old), files(&root)), before, "{}", text(&o));
    assert_eq!(o.status.code(), Some(2), "{}", text(&o));
}

// Spec (sol-loop r2, S4: "`uninstall --target NAME --apply` plans Forget for a missing recorded skill; before apply
// obtains its lock, another process restores that skill with edited content … Apply drops its record"; design note §3,
// I21: every record but a removed copy's or a gone path's stays with its path): a record planned to be forgotten
// because nothing was at its path keeps its record once something is there by the apply, which refuses, changing
// nothing.
#[test]
fn a_forget_whose_path_came_back_changes_nothing() {
    let h = Home::new(&["a"], "");
    let old = h.path("home/old");
    h.declare(&skills_target("old skills", &old));
    assert!(h.stacked(&["sync", "--user", "--apply"]).status.success());
    h.declare("");
    fs::remove_dir_all(old.join("a")).unwrap();
    let stacks = lords_core::Stacks::load(&h.path("pub"), Some(&h.path("priv"))).unwrap();
    let opts = lords_core::sync::Options {
        target: Some("old skills".into()),
        uninstall: true,
        home: h.path("home/.lords-command"),
        ..Default::default()
    };
    let plan = lords_core::sync::plan(&stacks, &opts).unwrap();
    h.write("home/old/a/SKILL.md", "mine, back\n");
    let r = lords_core::sync::apply(&plan);
    assert_eq!(recorded(&h, "target:old skills"), ["a"], "{r:?}");
    assert!(r.is_err(), "{r:?}");
    assert_eq!(fs::read_to_string(old.join("a/SKILL.md")).unwrap(), "mine, back\n");
}

// Spec (the Lords owner's brief, 7 Nov: "`guard install` while the guard copy is busy: today it fails with os error 5
// when a hook run holds `lords-guard.exe`. Windows lets a running image be renamed, so rename the busy copy aside, copy,
// and remove the aside copies once they're free. It must never leave no guard in place"; design note §4, I4, A4-1,
// A4-2): with the copy running (a hook waiting for its event), `lords guard install` of another build succeeds and the
// copy's place holds that build; on Windows the running copy is set aside beside it while it runs. Once it has ended,
// the next install finds the copy current and leaves the copy alone in its dir.
#[test]
fn guard_install_replaces_a_copy_a_hook_is_running() {
    use std::process::Stdio;
    let h = Home::new(&[], "");
    let lords = fs::read(env!("CARGO_BIN_EXE_lords")).unwrap();
    // Another build: a trailing byte, past what Windows loads.
    let old = [&lords[..], b"B"].concat();
    let bin = h.copy().parent().unwrap().to_path_buf();
    fs::create_dir_all(&bin).unwrap();
    fs::write(h.copy(), &old).unwrap();
    let names = || {
        let mut v: Vec<String> = fs::read_dir(&bin)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        v.sort();
        v
    };
    // A hook running the copy: the rm guard, waiting for its event on stdin.
    let mut running = h
        .cmd(&h.copy())
        .args(["guard", "rm"])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let o = h.lords(&["guard", "install"]);
    let (now, beside) = (fs::read(h.copy()).unwrap(), names());
    let aside: Vec<Vec<u8>> = beside
        .iter()
        .filter(|n| n.starts_with(".lords-guard.exe.lords-busy-"))
        .map(|n| fs::read(bin.join(n)).unwrap())
        .collect();
    drop(running.stdin.take());
    running.wait().unwrap();
    assert!(now == lords, "this build is in place: {}", text(&o));
    assert!(o.status.success(), "{}", text(&o));
    if cfg!(windows) {
        assert!(aside == [old.clone()], "the running copy, set aside: {beside:?}");
    }
    let o = h.lords(&["guard", "install"]);
    assert!(o.status.success() && text(&o).contains("already current"), "{}", text(&o));
    assert_eq!(names(), ["lords-guard.exe"], "{}", text(&o));
}
