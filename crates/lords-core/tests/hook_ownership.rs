//! Whose a settings hook is (D18; design note Harness/notes/2026-11-07-lords-delivery-integrity.md §1): Lords' only
//! when its command is byte for byte one a Lords build rendered for its guard copy and tool home. Any other hook stays
//! where it is, and one that runs Lords' copy all the same is named by its place, never shown.

// Tests are outside the filesystem boundary (fsguard.rs): their fixtures write their own temp dirs.
#![allow(clippy::disallowed_methods, clippy::disallowed_types)]

mod common;
use common::*;
use lords_core::sync::{apply, plan, Options};
use serde_json::{json, Value};
use std::fs;

const SENTINEL: &str = "SYNTHETIC_SENTINEL";

fn read(path: &std::path::Path) -> Value {
    serde_json::from_str(&fs::read_to_string(path).unwrap()).unwrap()
}

// Spec (D18, lords-wakeups.md: "`guard_of` treats any hook whose command starts with Lords' exact command
// (case-insensitive) as Lords' own. A user's `<Lords cmd>; echo x` in a settings file is rewritten to Lords' command or
// dropped at the next sync, and the user's text is lost without a word. It closes when ownership is byte equality with
// a command Lords rendered (any version it has written), and anything else is left alone and named"; design note §1,
// I18, A18-1 to A18-5, A18-8): with no guard wanted, sync takes out every hook a Lords build wrote, its earlier forms
// included and one whose timeout the user changed, and leaves every other hook byte for byte where it was: a user's
// Codex line that starts with Lords' and goes on, a Claude Code hook running the copy with args Lords never wrote, or
// with the copy's path spelt otherwise. The plan names each of those by event and entry, and never shows its text.
#[test]
fn only_a_hook_lords_rendered_is_lords() {
    let f = Fixture::new();
    let e = f.home().join("bin").join("lords-guard.exe").display().to_string().replace('\\', "/");
    let h = f.home().display().to_string().replace('\\', "/");
    let lords = |args: Value| json!({"type": "command", "command": e, "args": args});
    // Claude Code, exec form.
    let mine = json!({"matcher": "Bash|PowerShell", "hooks": [lords(json!(["guard", "rm", "--log", SENTINEL]))]});
    let spelt = json!({"matcher": "Bash|PowerShell", "hooks": [{"type": "command", "command": e.to_uppercase(), "args": ["guard", "rm"]}]});
    let show = json!({"hooks": [lords(json!(["show", "rule.x"]))]});
    let claude = json!({"hooks": {
        "PreToolUse": [{"matcher": "Bash|PowerShell", "hooks": [lords(json!(["guard", "rm"]))]}, mine, spelt],
        "SessionStart": [show],
        "SubagentStart": [
            {"hooks": [{"type": "command", "command": e, "args": ["guard", "child", "--part", "2", "--home", h], "timeout": 30}]},
            {"hooks": [{"type": "command", "command": e, "args": ["guard", "child", "--home", h], "timeout": 99}]},
        ],
        "UserPromptSubmit": [{"hooks": [lords(json!(["guard", "role", "--home", h]))]}],
    }});
    // Codex, a command line: on Windows PowerShell's call, elsewhere a quoted path.
    let (line, quoted) = if cfg!(windows) {
        (format!("& '{e}' guard"), format!("& '{e}' guard rm --shell powershell; echo {SENTINEL}"))
    } else {
        (format!("\"{e}\" guard"), format!("\"{e}\" guard rm; echo {SENTINEL}"))
    };
    let cmd = |c: String| json!({"type": "command", "command": c, "timeout": 30, "additionalContextLimit": 0});
    let theirs = json!({"hooks": [cmd(quoted)]});
    let codex = json!({"hooks": {
        "PreToolUse": [{"matcher": "Write", "hooks": [cmd(format!("{line} time"))]}, theirs],
        "SubagentStart": [
            {"hooks": [cmd(format!("\"{e}\" guard child"))]},
            {"hooks": [cmd(format!("\"{e}\" guard child --home \"{h}\""))]},
        ],
    }});
    let (cs, xs) =
        (f.path("home/.claude-balanced/settings.json"), f.path("home/.codex/hooks.json"));
    f.write("home/.claude-balanced/settings.json", &claude.to_string());
    f.write("home/.codex/hooks.json", &codex.to_string());
    let target = |name: &str, adapter: &str, path: &std::path::Path| {
        format!("[[targets]]\nname = \"{name}\"\nadapter = \"{adapter}\"\nkind = \"settings\"\npath = '{}'\n", path.display())
    };
    f.write(
        "priv/stack.toml",
        &format!(
            "name = \"priv\"\nvisibility = \"private\"\n{}{}",
            target("claude", "claude", &cs),
            target("codex", "codex", &xs)
        ),
    );
    let p = plan(&f.stacks(), &Options { user_only: true, ..f.opts() }).unwrap();
    apply(&p).unwrap();
    let (c, x) = (read(&cs), read(&xs));
    assert_eq!(
        c["hooks"]["PreToolUse"],
        json!([claude["hooks"]["PreToolUse"][1], claude["hooks"]["PreToolUse"][2]])
    );
    assert_eq!(c["hooks"]["SessionStart"], claude["hooks"]["SessionStart"]);
    assert_eq!(c["hooks"]["SubagentStart"], json!([]), "Lords' earlier and current child hooks");
    assert_eq!(c["hooks"]["UserPromptSubmit"], json!([]), "Lords' earlier role hook");
    assert_eq!(x["hooks"]["PreToolUse"], json!([codex["hooks"]["PreToolUse"][1]]));
    assert_eq!(x["hooks"]["SubagentStart"], json!([]), "Lords' earlier child lines");
    // Named by place, never by text.
    let notes = |file: &std::path::Path| {
        p.items.iter().find(|i| i.path == file).map(|i| i.notes.join("\n")).unwrap_or_default()
    };
    for (file, places) in [
        (
            &cs,
            &[
                "`hooks.PreToolUse` entry 2",
                "`hooks.PreToolUse` entry 3",
                "`hooks.SessionStart` entry 1",
            ][..],
        ),
        (&xs, &["`hooks.PreToolUse` entry 2"][..]),
    ] {
        for at in places {
            assert!(notes(file).contains(at), "{at}: {}", notes(file));
        }
    }
    let shown = serde_json::to_string(&p).unwrap();
    assert!(!shown.contains(SENTINEL), "{shown}");
}
