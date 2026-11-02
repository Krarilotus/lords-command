//! `lords guard child`, a `SubagentStart` hook (plan L6, an orchestrator's condition 3: "Children start with their own role's
//! core, not the parent's"): it gives a native child its place's `.lords/child.md`, which sync composes for the stack's
//! `child_role`. The child gets an empty home, so no real config, stack or setting is read or touched.

// Tests are outside the filesystem boundary (fsguard.rs): their fixtures write their own temp dirs.
#![allow(clippy::disallowed_methods, clippy::disallowed_types)]

use std::fs;
use std::io::Write;
use std::path::Path;
use std::process::{Command, Output, Stdio};
use tempfile::TempDir;

const BEGIN: &str = "<!-- lords-command:begin -->";
const END: &str = "<!-- lords-command:end -->";

fn run(home: &Path, cwd: &Path, args: &[&str], stdin: &str) -> Output {
    let mut c = Command::new(env!("CARGO_BIN_EXE_lords"))
        .current_dir(cwd)
        .env("HOME", home)
        .env("USERPROFILE", home)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        // The Codex sync asks after it writes Codex's hooks: the stub, never one on PATH (tests/codex_trust.rs).
        .env("LORDS_CODEX", env!("CARGO_BIN_EXE_lords-codex-stub"))
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    c.stdin.take().unwrap().write_all(stdin.as_bytes()).unwrap();
    c.wait_with_output().unwrap()
}

fn event(cwd: &Path) -> String {
    serde_json::json!({"hook_event_name": "SubagentStart", "session_id": "s1", "agent_id": "a1",
        "agent_type": "general-purpose", "cwd": cwd})
    .to_string()
}

// Spec (Claude Code's and Codex's hook docs: `SubagentStart` takes `hookSpecificOutput.additionalContext`; the guards'
// common rule, never fail a harness's event): for a child whose cwd lies in a place, the guard answers the place's
// `.lords/child.md` as the child's context, without the marker lines, exit 0; where no place has one it answers
// nothing; an event of another kind, or input it can't read, is exit 0 with nothing on stdout.
#[test]
fn the_child_guard_gives_a_child_its_places_child_file() {
    let d = TempDir::new().unwrap();
    let home = d.path().join("home");
    let cwd = d.path().join("place/a/b");
    fs::create_dir_all(&home).unwrap();
    fs::create_dir_all(&cwd).unwrap();
    let file = format!(
        "{BEGIN}\n<!-- header -->\nYou start as a helper here.\n- rule.x: A line.\n{END}\n"
    );
    fs::create_dir_all(d.path().join("place/.lords")).unwrap();
    fs::write(d.path().join("place/.lords/child.md"), &file).unwrap();
    let o = run(&home, &cwd, &["guard", "child"], &event(&cwd));
    assert_eq!(o.status.code(), Some(0), "{o:?}");
    let v: serde_json::Value = serde_json::from_slice(&o.stdout).unwrap();
    assert_eq!(v["hookSpecificOutput"]["hookEventName"], "SubagentStart", "{v}");
    let context = v["hookSpecificOutput"]["additionalContext"].as_str().unwrap();
    assert_eq!(context, "<!-- header -->\nYou start as a helper here.\n- rule.x: A line.", "{v}");
    let elsewhere = d.path().join("elsewhere");
    fs::create_dir_all(&elsewhere).unwrap();
    for (cwd, input) in [
        (&elsewhere, event(&elsewhere)),
        (&cwd, event(&cwd).replace("SubagentStart", "SessionStart")),
        (&cwd, "not json".to_string()),
    ] {
        let o = run(&home, cwd, &["guard", "child"], &input);
        assert_eq!(o.status.code(), Some(0), "{input}: {o:?}");
        assert!(o.stdout.is_empty(), "{input}: {o:?}");
    }
}

/// The context one `guard child` call answers, or `None` for no output.
fn context(o: &Output) -> Option<String> {
    let v: serde_json::Value = serde_json::from_slice(&o.stdout).ok()?;
    Some(v["hookSpecificOutput"]["additionalContext"].as_str()?.to_string())
}

// Spec (review r1, F1: Explore and Plan "start without the shared binding core"; design note D4; the Lords owner's
// option 1, 1 Nov: one read per start for every agent type, and "Explore and Plan get nothing, with a one-line note,
// wherever their start is longer than one part"): a child that loads no place file (`agent_type` Explore or Plan) gets
// the place's `.lords/child-whole.md` whole where it fits one hook's 9,500 characters, and nothing, with a one-line note
// naming its size, where it doesn't; any other child gets `child.md`. An Explore child never takes the delta for its
// whole start (repair sol-loop r1, R4).
#[test]
fn a_child_with_no_place_file_gets_the_whole_start_or_nothing() {
    let d = TempDir::new().unwrap();
    let home = d.path().join("home");
    let cwd = d.path().join("place");
    fs::create_dir_all(cwd.join(".lords")).unwrap();
    fs::create_dir_all(&home).unwrap();
    let whole = |n: usize| -> Vec<String> {
        (0..n)
            .map(|i| format!("- rule.r{i:03}: Line {i}, about as long as a core line is."))
            .collect()
    };
    fs::write(cwd.join(".lords/child.md"), child_file(&["ONLY THE DELTA.".to_string()])).unwrap();
    for agent in ["Explore", "Plan"] {
        fs::write(cwd.join(".lords/child-whole.md"), child_file(&whole(100))).unwrap();
        let got = context(&run(&home, &cwd, &["guard", "child"], &start(&cwd, agent, "s1", "x")));
        assert_eq!(got, Some(whole(100).join("\n")), "{agent}: the whole start, in one read");
        fs::write(cwd.join(".lords/child-whole.md"), child_file(&whole(300))).unwrap();
        let o = run(&home, &cwd, &["guard", "child"], &start(&cwd, agent, "s1", "y"));
        assert!(o.stdout.is_empty(), "{agent}: past one hook, nothing: {o:?}");
        let note = String::from_utf8_lossy(&o.stderr);
        assert_eq!(note.trim().lines().count(), 1, "{agent}: one line: {note}");
        assert!(note.contains("9500"), "{agent}: the note says why: {note}");
    }
    let got = context(&run(&home, &cwd, &["guard", "child"], &event(&cwd)));
    assert_eq!(got.as_deref(), Some("ONLY THE DELTA."), "general-purpose: the delta");
    fs::remove_file(cwd.join(".lords/child-whole.md")).unwrap();
    let o = run(&home, &cwd, &["guard", "child"], &start(&cwd, "Explore", "s1", "z"));
    assert!(o.stdout.is_empty(), "no whole start, no delta in its place: {o:?}");
}

/// A `SubagentStart` event of `agent_type`, in `cwd`, with these ids.
fn start(cwd: &Path, agent_type: &str, session: &str, agent: &str) -> String {
    serde_json::json!({"hook_event_name": "SubagentStart", "session_id": session, "agent_id": agent,
        "agent_type": agent_type, "cwd": cwd})
    .to_string()
}

// Spec (the Lords owner's option 1, 1 Nov: one hook gives a child at most 9,500 characters, as Claude Code measures a
// hook's context, in characters; Claude Code's hooks docs: past 10,000 a context becomes a file path and a preview):
// a start of exactly 9,500 characters, counted without its marker lines and line-end carriage returns and with
// multi-byte characters, is given whole; one character more is given nowhere, with a note.
#[test]
fn a_start_is_given_whole_up_to_one_hook_and_nowhere_past_it() {
    let d = TempDir::new().unwrap();
    let home = d.path().join("home");
    let place = d.path().join("repo");
    fs::create_dir_all(place.join(".lords")).unwrap();
    fs::create_dir_all(&home).unwrap();
    // 95 lines of 99 characters ("ä" is two bytes) and the 94 line ends between them: 9,499; one more character on the
    // last line makes 9,500.
    let text = |extra: usize| -> String {
        let mut lines: Vec<String> = (0..95).map(|_| "ä".repeat(99)).collect();
        lines[94].push_str(&"ö".repeat(extra));
        format!("{BEGIN}\r\n{}\r\n{END}\r\n", lines.join("\r\n"))
    };
    for (extra, given) in [(1, true), (2, false)] {
        fs::write(place.join(".lords/child-whole.md"), text(extra)).unwrap();
        let o = run(&home, &place, &["guard", "child"], &start(&place, "Explore", "s1", "x"));
        let got = context(&o);
        assert_eq!(got.is_some(), given, "{} characters: {o:?}", 9_499 + extra);
        if let Some(got) = got {
            assert_eq!(got.chars().count(), 9_500, "whole, without the markers and the CRs");
        } else {
            assert!(String::from_utf8_lossy(&o.stderr).contains("9501"), "{o:?}");
        }
    }
}

// Spec (fix check r2, FC1: "one child-start event delivers one complete generation of the child's start, or the
// whole-event refusal"; Claude Code fires `SubagentStart` again with the same agent id when a child resumes; the Lords
// owner's option 1, 1 Nov), on a place a real `lords sync --apply` wrote: a general-purpose child gets its `child.md`
// whole; resumed with the same agent id after a sync changed its rule, it gets the new `child.md` whole, each time
// exactly the file sync wrote. Its Explore sibling, whose whole start the long every-role rules put past one hook,
// gets nothing, with a note.
#[test]
fn a_resumed_child_gets_the_start_current_at_its_one_read() {
    let d = TempDir::new().unwrap();
    let home = d.path().join("home");
    let (public, private, checkout) =
        (d.path().join("pub"), d.path().join("priv"), d.path().join("checkout"));
    for dir in [&home, &public, &private, &checkout] {
        fs::create_dir_all(dir).unwrap();
    }
    let git = |dir: &Path, args: &[&str]| {
        let gitconfig = d.path().join("gitconfig");
        // Built at run time, so the repo's own privacy check doesn't see an address in this source.
        let email = ["test", "example.invalid"].join("@");
        fs::write(&gitconfig, format!("[user]\n\tname = Test\n\temail = {email}\n")).unwrap();
        let o = Command::new("git")
            .current_dir(dir)
            .env("GIT_CONFIG_GLOBAL", &gitconfig)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .args(args)
            .output()
            .unwrap();
        assert!(o.status.success(), "git {args:?}: {o:?}");
    };
    fs::write(checkout.join("README.md"), "a repo\n").unwrap();
    git(&checkout, &["init", "-q"]);
    git(&checkout, &["add", "-A"]);
    git(&checkout, &["commit", "-q", "-m", "start"]);
    fs::write(public.join("stack.toml"), "name = \"pub\"\nvisibility = \"public\"\n").unwrap();
    fs::write(
        private.join("stack.toml"),
        format!(
            "name = \"priv\"\nvisibility = \"private\"\nroles = [\"orchestrator\", \"coder\", \"helper\", \"product\"]\nchild_role = \"helper\"\n[projects.beta]\n[repos.beta-core]\nproject = \"beta\"\ncheckouts = ['{}']\nworktrees = false\ndefault_role = \"orchestrator\"\nadapters = [\"claude\"]\n",
            checkout.display()
        ),
    )
    .unwrap();
    let rule = |slug: &str, lines: &[String]| {
        let roles = if slug == "m" { "roles: [helper]\n" } else { "" };
        let lines: Vec<String> = lines.iter().map(|l| format!("- line: \"{l}\"")).collect();
        let text = format!(
            "---\nid: rule.{slug}\nkind: rule\ntitle: rule.{slug}\naudience: [all]\n{roles}scope: project:beta\nstatus: active\nvisibility: private\norigin:\n  type: own\nstart: core\ncore:\n{}\n---\nThe whole rule.\n",
            lines.join("\n")
        );
        fs::create_dir_all(private.join("project/beta/rules")).unwrap();
        fs::write(private.join(format!("project/beta/rules/{slug}.md")), text).unwrap();
    };
    let long = |n: usize, what: &str| -> Vec<String> {
        (0..n).map(|i| format!("{what} {i:02} {}", "x".repeat(390))).collect()
    };
    rule("a", &long(25, "A"));
    rule("m", &["Never publish the draft.".to_string()]);
    rule("z", &long(30, "Z"));
    git(&private, &["init", "-q"]);
    git(&private, &["add", "-A"]);
    git(&private, &["commit", "-q", "-m", "start"]);
    let stacks =
        ["--public-stack", public.to_str().unwrap(), "--private-stack", private.to_str().unwrap()];
    let sync = || {
        let o = run(
            &home,
            d.path(),
            &[&["sync", "--repo", "beta-core", "--apply"][..], &stacks].concat(),
            "",
        );
        assert!(o.status.success(), "sync: {o:?}");
    };
    sync();
    let written = || {
        let text = fs::read_to_string(checkout.join(".lords/child.md")).unwrap();
        lords_core::guard::child_context(&text)
    };
    let child = || {
        let ev = start(&checkout, "general-purpose", "s1", "a1");
        context(&run(&home, &checkout, &["guard", "child"], &ev)).unwrap_or_default()
    };
    let first = child();
    assert!(first.contains("- rule.m: Never publish the draft."), "{first}");
    assert_eq!(first, written(), "the file sync wrote, whole");
    let o = run(&home, &checkout, &["guard", "child"], &start(&checkout, "Explore", "s1", "e1"));
    assert!(o.stdout.is_empty(), "Explore's whole start is past one hook: {o:?}");
    assert!(String::from_utf8_lossy(&o.stderr).contains("9500"), "{o:?}");
    rule("m", &["Never publish the draft or its notes.".to_string()]);
    git(&private, &["commit", "-q", "-a", "-m", "a wider rule.m"]);
    sync();
    let resumed = child();
    assert!(resumed.contains("- rule.m: Never publish the draft or its notes."), "{resumed}");
    assert_eq!(resumed, written(), "the resume's one read: the new file, whole");
}

// Spec (fix check r1, FC1: "Publication also leaves readers no gap: no reader ever finds an enclosing place or nothing
// because sync is between moving the old `.lords` aside and publishing the new one"; fsguard names the old one
// `..lords.lords-old-*` beside it): a child in a place whose `.lords` is moved aside never gets the enclosing place's
// start: it waits for the new `.lords` and gets that, and while none comes back it gets nothing, with a note. One read
// needs no id: an event naming no agent id gets its start too.
#[test]
fn a_child_never_falls_through_to_an_enclosing_place_mid_publication() {
    let d = TempDir::new().unwrap();
    let home = d.path().join("home");
    let (outer, inner) = (d.path().join("ws"), d.path().join("ws/repo"));
    fs::create_dir_all(&home).unwrap();
    fs::create_dir_all(outer.join(".lords")).unwrap();
    fs::write(outer.join(".lords/child-whole.md"), format!("{BEGIN}\nOUTER START.\n{END}\n"))
        .unwrap();
    let aside = inner.join("..lords.lords-old-1-2-3");
    fs::create_dir_all(&aside).unwrap();
    fs::write(aside.join("child-whole.md"), format!("{BEGIN}\nINNER START.\n{END}\n")).unwrap();
    // While the aside stays: nothing, and never the enclosing place's start.
    let o = run(&home, &inner, &["guard", "child"], &start(&inner, "Explore", "s1", "stuck"));
    assert!(o.stdout.is_empty(), "nothing, never OUTER: {o:?}");
    assert!(String::from_utf8_lossy(&o.stderr).contains("sync"), "the note says why: {o:?}");
    // The new `.lords` comes in while the child waits, as sync puts it in: under the apply lock, `<tool home>/apply.lock`
    // (repair 2 sol-loop r2, W1: a reader that passed a missing `.lords` decides under that lock). It gets that.
    // A tool home other than the default, as `sync --home DIR` uses, named to the guard as sync's hooks name it
    // (repair 2 sol-loop r3, X1): the child takes that home's lock.
    let tool_home = d.path().join("other-home");
    let lock = tool_home.join("apply.lock");
    fs::create_dir_all(&tool_home).unwrap();
    fs::write(&lock, "pid 1\n").unwrap();
    let mut c = Command::new(env!("CARGO_BIN_EXE_lords"))
        .current_dir(&inner)
        .env("HOME", &home)
        .env("USERPROFILE", &home)
        .args(["guard", "child", "--home", tool_home.to_str().unwrap()])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    c.stdin.take().unwrap().write_all(start(&inner, "Explore", "s1", "waits").as_bytes()).unwrap();
    std::thread::sleep(std::time::Duration::from_millis(300));
    // W1's witness: between two swaps the aside is gone and no `.lords` is there yet; the child mustn't walk on.
    fs::rename(&aside, d.path().join("held-by-sync")).unwrap();
    std::thread::sleep(std::time::Duration::from_millis(300));
    fs::rename(d.path().join("held-by-sync"), inner.join(".lords")).unwrap();
    fs::remove_file(&lock).unwrap();
    let got = context(&c.wait_with_output().unwrap()).unwrap_or_default();
    assert!(got.contains("INNER START.") && !got.contains("OUTER"), "{got}");
    let no_id = start(&inner, "Explore", "s1", "x").replace("\"agent_id\":\"x\",", "");
    assert!(!no_id.contains("agent_id"), "{no_id}");
    let got = context(&run(&home, &inner, &["guard", "child"], &no_id)).unwrap_or_default();
    assert!(got.contains("INNER START."), "one read needs no id: {got}");
}

// Spec (the brief, slice 4: "through each provider's subagent-start hook"; the Codex probe, design note: Codex's child
// takes a command hook's context once the hook is trusted): with `guards = ["child"]` and the copy installed, sync
// names the copy in one `SubagentStart` entry (`guard child`, no matcher, so every agent type), after the user's own,
// and the plan says so: in Claude Code's settings in its exec form; in Codex's `hooks.json` as a command line with the
// whole context passed (`additionalContextLimit` 0) and a note that Codex runs it once reviewed under /hooks. A guard
// Codex isn't shown to run is refused for a Codex target, and nothing is written.
#[test]
fn sync_registers_the_child_guard_on_subagent_start() {
    let d = TempDir::new().unwrap();
    let home = d.path().join("home");
    let (public, private) = (d.path().join("pub"), d.path().join("priv"));
    for dir in [&public, &private, &home.join(".claude-balanced"), &home.join(".codex")] {
        fs::create_dir_all(dir).unwrap();
    }
    let theirs = r#"{"hooks": [{"type": "command", "command": "notify"}]}"#;
    let settings = home.join(".claude-balanced/settings.json");
    fs::write(&settings, format!("{{\"hooks\": {{\"SubagentStart\": [{theirs}]}}}}\n")).unwrap();
    let codex = home.join(".codex/hooks.json");
    fs::write(public.join("stack.toml"), "name = \"pub\"\nvisibility = \"public\"\n").unwrap();
    let manifest = |codex_guards: &str| {
        format!("name = \"priv\"\nvisibility = \"private\"\n[[targets]]\nname = \"balanced settings\"\nadapter = \"claude\"\nkind = \"settings\"\npath = \"~/.claude-balanced/settings.json\"\nguards = [\"child\"]\n[[targets]]\nname = \"codex hooks\"\nadapter = \"codex\"\nkind = \"settings\"\npath = \"~/.codex/hooks.json\"\nguards = {codex_guards}\n")
    };
    fs::write(private.join("stack.toml"), manifest("[\"child\"]")).unwrap();
    let git = |args: &[&str]| {
        let gitconfig = d.path().join("gitconfig");
        // Built at run time, so the repo's own privacy check doesn't see an address in this source.
        let email = ["test", "example.invalid"].join("@");
        fs::write(&gitconfig, format!("[user]\n\tname = Test\n\temail = {email}\n")).unwrap();
        Command::new("git")
            .current_dir(&private)
            .env("GIT_CONFIG_GLOBAL", &gitconfig)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .args(args)
            .output()
            .unwrap()
    };
    git(&["init", "-q"]);
    git(&["add", "-A"]);
    git(&["commit", "-q", "-m", "start"]);
    let o = run(&home, d.path(), &["guard", "install"], "");
    assert!(o.status.success(), "{o:?}");
    let stacks =
        ["--public-stack", public.to_str().unwrap(), "--private-stack", private.to_str().unwrap()];
    let o = run(&home, d.path(), &[&["sync", "--user"][..], &stacks].concat(), "");
    let shown = String::from_utf8_lossy(&o.stdout);
    assert_eq!(
        shown.matches("Lords hook added: the child guard (SubagentStart)").count(),
        2,
        "{o:?}"
    );
    assert!(shown.contains("/hooks"), "Codex's review is named: {o:?}");
    let o = run(&home, d.path(), &[&["sync", "--user", "--apply"][..], &stacks].concat(), "");
    assert!(o.status.success(), "{o:?}");
    // Fix check r2, FC4: an older guard copy, one that passes the rm self-test but takes no `--home` (as `b0f5d93`'s and
    // `742a9bd`'s), is refused with the step that fixes it, and both settings keep the hooks they have.
    let before = (fs::read(&settings).unwrap(), fs::read(&codex).unwrap());
    fs::copy(
        env!("CARGO_BIN_EXE_lords-old-guard-stub"),
        home.join(".lords-command/bin/lords-guard.exe"),
    )
    .unwrap();
    let o = run(&home, d.path(), &[&["sync", "--user", "--apply"][..], &stacks].concat(), "");
    assert!(!o.status.success(), "refused: {o:?}");
    let said =
        format!("{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr));
    assert!(said.contains("lords guard install"), "names the install: {said}");
    assert_eq!((fs::read(&settings).unwrap(), fs::read(&codex).unwrap()), before, "the hooks stay");
    let o = run(&home, d.path(), &["guard", "install"], "");
    assert!(o.status.success(), "{o:?}");
    let read = |p: &Path| -> serde_json::Value {
        serde_json::from_str(&fs::read_to_string(p).unwrap()).unwrap()
    };
    let copy =
        home.join(".lords-command/bin/lords-guard.exe").display().to_string().replace('\\', "/");
    // The tool home sync ran with, so the guard takes that home's apply lock (repair 2 sol-loop r3, X1).
    let tool_home = home.join(".lords-command").display().to_string().replace('\\', "/");
    // Claude Code: one hook, one read (fix check r2, FC1; the Lords owner's option 1), with the deadline the guard's
    // waits end inside (fix check r2, FC5).
    let hook = serde_json::json!({"type": "command", "command": copy, "args": ["guard", "child", "--home", tool_home], "timeout": 30});
    let theirs: serde_json::Value = serde_json::from_str(theirs).unwrap();
    let v = read(&settings);
    assert_eq!(v["hooks"]["SubagentStart"], serde_json::json!([theirs, {"hooks": [hook]}]), "{v}");
    let v = read(&codex);
    // On Windows Codex runs a hook's command with PowerShell, where a line starting with a quoted path is no command
    // (F10 design note 2026-11-02-lords-role-at-launch, H8, H9): PowerShell's call, literal paths.
    let line = if cfg!(windows) {
        format!("& '{copy}' guard child --home '{tool_home}'")
    } else {
        format!("\"{copy}\" guard child --home \"{tool_home}\"")
    };
    let lords = serde_json::json!({"type": "command", "command": line, "timeout": 30, "additionalContextLimit": 0});
    assert_eq!(v, serde_json::json!({"hooks": {"SubagentStart": [{"hooks": [lords]}]}}), "{v}");
    // `queue`: Codex runs `rm` since D16 (design note 2026-11-02-lords-git-help-guard §2), not a `Stop` guard.
    fs::write(private.join("stack.toml"), manifest("[\"child\", \"queue\"]")).unwrap();
    git(&["commit", "-q", "-a", "-m", "a queue guard for Codex"]);
    let o = run(&home, d.path(), &[&["sync", "--user", "--apply"][..], &stacks].concat(), "");
    assert!(!o.status.success(), "{o:?}");
    assert!(String::from_utf8_lossy(&o.stderr).contains("runs no `queue` guard"), "{o:?}");
    assert_eq!(read(&codex), v, "nothing written");
}

/// A child file of these lines, between the marker lines, as sync writes one.
fn child_file(lines: &[String]) -> String {
    format!("{BEGIN}\n{}\n{END}\n", lines.join("\n"))
}

// Spec (fix check r2, FC4: the guard copy sync names must take the command line sync writes; design note R3): asked
// with exactly the hook's arguments and the probe event, the guard answers its child protocol, 4 since one read per
// start for every agent type (the Lords owner's option 1).
#[test]
fn the_guard_answers_the_probe_with_its_child_protocol() {
    let d = TempDir::new().unwrap();
    let home = d.path().join("home");
    fs::create_dir_all(&home).unwrap();
    let tool_home = d.path().join("tool-home");
    let o = run(
        &home,
        d.path(),
        &["guard", "child", "--home", tool_home.to_str().unwrap()],
        r#"{"hook_event_name":"LordsGuardProbe"}"#,
    );
    assert_eq!(String::from_utf8_lossy(&o.stdout).trim(), "lords guard child 4", "{o:?}");
}

// Spec (fix check r2, FC5: "No hook reader killed at any instant leaves sync or later readers blocked"; its witness, a
// reader killed while it holds the apply lock): the lock file such a reader leaves (this build's record, its holder
// gone) doesn't hold the next reader, which gets its start at once, nor its writer.
#[test]
fn a_dead_readers_lock_holds_no_one() {
    let d = TempDir::new().unwrap();
    let home = d.path().join("home");
    let place = d.path().join("repo");
    let below = place.join("src");
    fs::create_dir_all(place.join(".lords")).unwrap();
    fs::create_dir_all(&below).unwrap();
    fs::write(place.join(".lords/child-whole.md"), child_file(&["THE START.".to_string()]))
        .unwrap();
    let lock = home.join(".lords-command/apply.lock");
    fs::create_dir_all(lock.parent().unwrap()).unwrap();
    fs::write(&lock, "lords-lock 2 pid 4294967295\n").unwrap();
    let t = std::time::Instant::now();
    let ev = start(&below, "Explore", "s1", "after-a-dead-reader");
    let got = context(&run(&home, &below, &["guard", "child"], &ev));
    assert_eq!(got.as_deref(), Some("THE START."), "{got:?}");
    assert!(
        t.elapsed() < std::time::Duration::from_secs(10),
        "at once, not after a wait: {:?}",
        t.elapsed()
    );
}
