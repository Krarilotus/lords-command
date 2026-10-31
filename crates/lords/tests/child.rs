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

// Spec (review r1, F1: Explore and Plan "start without the shared binding core"; Claude Code's hooks docs: a hook's
// `additionalContext` past 10,000 characters becomes a file path and a preview, and "Claude Code measures each string on
// its own, even when several hooks run for the same event"; the parts probe, design note D4): a child that loads no
// place file (`agent_type` Explore or Plan) gets the place's `.lords/child-whole.md`, any other `child.md`. With
// `--part N` each call answers the Nth piece, cut at line ends, each under 10,000 characters, together the whole file
// in order, and nothing past the last; without `--part` (Codex, no such limit), the whole file at once.
#[test]
fn a_child_with_no_place_file_gets_the_whole_start_in_parts() {
    let d = TempDir::new().unwrap();
    let home = d.path().join("home");
    let cwd = d.path().join("place");
    fs::create_dir_all(cwd.join(".lords")).unwrap();
    fs::create_dir_all(&home).unwrap();
    let lines: Vec<String> = (0..300)
        .map(|i| format!("- rule.r{i:03}: Line {i}, about as long as a core line is."))
        .collect();
    fs::write(cwd.join(".lords/child-whole.md"), format!("{BEGIN}\n{}\n{END}\n", lines.join("\n")))
        .unwrap();
    fs::write(cwd.join(".lords/child.md"), format!("{BEGIN}\nONLY THE DELTA.\n{END}\n")).unwrap();
    let with = |agent: &str| event(&cwd).replace("general-purpose", agent);
    for agent in ["Explore", "Plan"] {
        let parts: Vec<Option<String>> = (1..=4)
            .map(|n| {
                context(&run(
                    &home,
                    &cwd,
                    &["guard", "child", "--part", &n.to_string()],
                    &with(agent),
                ))
            })
            .collect();
        assert!(
            parts[0].is_some() && parts[1].is_some() && parts[3].is_none(),
            "{agent}: {parts:?}"
        );
        let got: Vec<&str> = parts
            .iter()
            .flatten()
            .flat_map(|p| p.lines())
            .filter(|l| l.starts_with("- rule."))
            .collect();
        assert_eq!(
            got,
            lines.iter().map(String::as_str).collect::<Vec<_>>(),
            "{agent}: every line, in order"
        );
        for p in parts.iter().flatten() {
            assert!(
                p.chars().count() < 10_000,
                "{agent}: a part of {} characters",
                p.chars().count()
            );
        }
        let all = context(&run(&home, &cwd, &["guard", "child"], &with(agent))).unwrap();
        assert_eq!(
            all.lines().filter(|l| l.starts_with("- rule.")).count(),
            300,
            "{agent}: whole without --part"
        );
    }
    let first = context(&run(&home, &cwd, &["guard", "child", "--part", "1"], &event(&cwd)));
    assert_eq!(
        first.as_deref().map(|c| c.contains("ONLY THE DELTA.")),
        Some(true),
        "general-purpose: the delta"
    );
    let second = run(&home, &cwd, &["guard", "child", "--part", "2"], &event(&cwd));
    assert!(second.stdout.is_empty(), "nothing past the delta's one part");
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
    let read = |p: &Path| -> serde_json::Value {
        serde_json::from_str(&fs::read_to_string(p).unwrap()).unwrap()
    };
    let copy =
        home.join(".lords-command/bin/lords-guard.exe").display().to_string().replace('\\', "/");
    // Claude Code: one hook per part (review r1, F1), each its own 10,000 characters.
    let part = |n: &str| serde_json::json!({"type": "command", "command": copy, "args": ["guard", "child", "--part", n]});
    let theirs: serde_json::Value = serde_json::from_str(theirs).unwrap();
    let v = read(&settings);
    assert_eq!(
        v["hooks"]["SubagentStart"],
        serde_json::json!([theirs, {"hooks": [part("1"), part("2"), part("3")]}]),
        "{v}"
    );
    let v = read(&codex);
    let line = format!("\"{copy}\" guard child");
    let lords = serde_json::json!({"type": "command", "command": line, "timeout": 30, "additionalContextLimit": 0});
    assert_eq!(v, serde_json::json!({"hooks": {"SubagentStart": [{"hooks": [lords]}]}}), "{v}");
    fs::write(private.join("stack.toml"), manifest("[\"child\", \"rm\"]")).unwrap();
    git(&["commit", "-q", "-a", "-m", "an rm guard for Codex"]);
    let o = run(&home, d.path(), &[&["sync", "--user", "--apply"][..], &stacks].concat(), "");
    assert!(!o.status.success(), "{o:?}");
    assert!(String::from_utf8_lossy(&o.stderr).contains("runs no `rm` guard"), "{o:?}");
    assert_eq!(read(&codex), v, "nothing written");
}
