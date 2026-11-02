//! The time guard (`lords guard time`; design note Harness/notes/2026-11-02-lords-time-guard.md): an editor write to
//! a Markdown or plain-text file, or an outgoing T3 message, that adds a placeholder clock time is denied (P); a clock
//! time more than 5 minutes ahead of the Berlin clock, same day, with no future marker on its line, is flagged (F).
//! Real times, ranges, "about" times, durations, ratios and code never are (A), nor is text a write keeps.

// Tests are outside the filesystem boundary (fsguard.rs): their fixtures write their own temp dirs.
#![allow(clippy::disallowed_methods, clippy::disallowed_types)]

use jiff::tz::TimeZone;
use jiff::Timestamp;
use lords_core::time_guard::{findings, time_hook, Kind};
use serde_json::{json, Value};
use std::fs;
use std::path::{Path, PathBuf};
use tempfile::TempDir;

fn berlin() -> TimeZone {
    TimeZone::get("Europe/Berlin").unwrap()
}

/// A Berlin wall time on 2 Nov 2026 (CET, UTC+1), `HH:MM`.
fn at(hm: &str) -> Timestamp {
    jiff::civil::DateTime::strptime("%Y-%m-%d %H:%M", format!("2026-11-02 {hm}"))
        .unwrap()
        .to_zoned(berlin())
        .unwrap()
        .timestamp()
}

/// The hook's answer to `tool_input` on `tool`, at Berlin `hm`: the denial reason, or `None`.
fn denied_at(tool: &str, input: Value, hm: &str) -> Option<String> {
    let ev = json!({"hook_event_name": "PreToolUse", "tool_name": tool, "tool_input": input});
    let out = time_hook(&ev.to_string(), at(hm), Some(&berlin()));
    if out.is_empty() {
        return None;
    }
    let v: Value = serde_json::from_str(&out).unwrap();
    let h = &v["hookSpecificOutput"];
    assert_eq!(h["hookEventName"], "PreToolUse", "{out}");
    match h["permissionDecision"].as_str() {
        Some("deny") => Some(h["permissionDecisionReason"].as_str().unwrap().to_string()),
        _ => None,
    }
}

/// A T3 message `text` sent at 23:59, when no time that day is ahead: only P can refuse it.
fn message_denied(text: &str) -> Option<String> {
    denied_at("mcp__t3-code__t3_thread_send", json!({"threadId": "t", "message": text}), "23:59")
}

fn placeholder(text: &str) {
    let why = message_denied(text).unwrap_or_else(|| panic!("let through: {text:?}"));
    assert!(why.contains("placeholder"), "{text:?}: {why}");
}

fn passes(text: &str) {
    assert_eq!(message_denied(text), None, "denied: {text:?}");
}

/// The F tokens `text` adds, written at Berlin `hm`.
fn future(text: &str, hm: &str) -> Vec<String> {
    let now = at(hm).to_zoned(berlin());
    findings(text, "", Some(&now))
        .into_iter()
        .filter(|f| f.kind == Kind::Future)
        .map(|f| f.token)
        .collect()
}

struct Dir {
    dir: TempDir,
}

impl Dir {
    fn new() -> Dir {
        Dir { dir: TempDir::new().unwrap() }
    }

    /// A file `name` here, holding `text` when it's `Some`.
    fn file(&self, name: &str, text: Option<&str>) -> PathBuf {
        let p = self.dir.path().join(name);
        if let Some(t) = text {
            fs::write(&p, t).unwrap();
        }
        p
    }
}

fn write(path: &Path, content: &str) -> Option<String> {
    denied_at("Write", json!({"file_path": path, "content": content}), "23:59")
}

fn edit(path: &Path, old: &str, new: &str) -> Option<String> {
    denied_at(
        "Edit",
        json!({"file_path": path, "old_string": old, "new_string": new, "replace_all": false}),
        "23:59",
    )
}

// ---------------------------------------------------------------- P: placeholder forms

// Spec (the brief: "a digit position written as a letter, or a whole time written as letters (`13:0x`, `14:xx`,
// `HH:MM`, `hh:mm`, `??:??` and the like)"; the design note §4 adds the forms below): each is denied wherever it
// stands in prose.
#[test]
fn placeholder_clock_times_are_denied() {
    placeholder("Done at 13:0x.");
    placeholder("Started 14:xx");
    placeholder("Finished at HH:MM Berlin");
    placeholder("at hh:mm");
    placeholder("ETA ??:??");
    placeholder("from 1x:30");
    placeholder("XX:XX");
    placeholder("stamp HH:MM:SS here");
    placeholder("about 9:xx");
    placeholder("at H:MM");
    placeholder("| HH:MM | run |");
    placeholder("(13:xx)");
    placeholder("~14:xx");
    placeholder("done 14:xx?");
    placeholder("line one\nwritten 13:4x, then more");
    // A one-line triple-backtick span is no fence: prose after it is judged (sol-loop r1, S3).
    placeholder("```HH:MM``` then\ndone 14:xx");
    // A span doesn't reach across list items (sol-loop r2, S7).
    placeholder("- example `text\n- done 14:xx\n- end`");
}

// Spec (the brief: "The deny reason names the fix: take the time from the turn hook's "Berlin HH:MM" line or from
// the clock (PowerShell's Berlin conversion), and write the real time").
#[test]
fn the_placeholder_reason_names_the_fix() {
    let why = message_denied("at 14:xx").unwrap();
    for part in ["`14:xx`", "turn hook", "Berlin", "ConvertTimeBySystemTimeZoneId", "real time"] {
        assert!(why.contains(part), "{part}: {why}");
    }
}

// ---------------------------------------------------------------- A: never refused

// Spec (the brief: "Real times, ranges (`13:05–13:20`), "about"/`~` times, durations (`1:30 h`), ratios and code are
// never denied"; design note §4, never refused): none of these is denied.
#[test]
fn real_times_durations_ratios_and_code_pass() {
    passes("Done at 13:05.");
    passes("13:05–13:20 review");
    passes("13:05-13:20");
    passes("about 13:05, ~13:10");
    passes("took 1:30 h");
    passes("mixed 3:1, screen 16:9");
    passes("the clock line `Berlin HH:MM Mon 2 Nov`");
    passes("``HH:MM`` in a double span");
    passes("```\nstamp HH:MM\n```");
    passes("~~~text\n14:xx\n~~~");
    passes("std::fs and C:\\x and C:/x and http://host:80/a");
    passes("fe80::1 and xx:xx:xx:xx");
    passes("timing mm:ss and MM:SS");
    passes("hex ab:cd and regex (?:x)");
    // Line citations by a one-letter source (measured: `H:25–35` in a note, design note §5).
    passes("sources H:25–35, h:63 and C:40");
    // A port is no minute, and a code span may wrap (sol-loop r1, S1, S2).
    passes("a `format\nHH:MM\nend` span, wrapped");
    passes("`a\n\u{a0}\nHH:MM\nb`");
    passes("see http://xx:80/ there");
    passes("no times at all");
}

// ---------------------------------------------------------------- each input

// Spec (the brief: "A file write by an agent's editor tools (Claude `Write`/`Edit`/`MultiEdit`; Codex's edit tool …)
// is denied when the new text contains a placeholder"; "Scope it by file type (Markdown and plain text)"; the steer:
// "Run the same check on outgoing T3 messages … its `message`"; design note §2: Codex's `apply_patch` with the patch in
// `command`, the T3 tool under either server name; `MultiEdit` cut, the Lords owner, 2 Nov: Claude Code 2.1.316 has
// none): each input that adds one is denied.
#[test]
fn every_input_that_adds_a_placeholder_is_denied() {
    let d = Dir::new();
    for name in ["new.md", "NEW.MD", "n.markdown", "n.txt"] {
        assert!(write(&d.file(name, None), "# t\nat 14:xx\n").is_some(), "{name}");
    }
    let note = d.file("note.md", Some("# Log\n\nreal 13:05\n"));
    assert!(write(&note, "# Log\n\nreal 13:05\nthen 14:xx\n").is_some());
    assert!(edit(&note, "real 13:05", "real 13:05, then 14:xx").is_some());
    let add = "*** Begin Patch\n*** Add File: notes/a.md\n+# A\n+at 14:xx\n*** End Patch\n";
    assert!(denied_at("apply_patch", json!({"command": add}), "23:59").is_some());
    let update = "*** Begin Patch\n*** Update File: notes/b.txt\n@@\n keep\n-old 13:05\n+new 13:0x\n*** End Patch\n";
    assert!(denied_at("apply_patch", json!({"command": update}), "23:59").is_some());
    for tool in ["mcp__t3-code__t3_thread_send", "mcp__t3_code__t3_thread_send"] {
        let input = json!({"threadId": "t", "mode": "queue", "message": "done 14:xx"});
        assert!(denied_at(tool, input, "23:59").is_some(), "{tool}");
    }
}

// Spec (the brief: scope by file type; the design note §4, out of scope; the guards' rule: input it can't read is let
// through): other files, other tools, and input that isn't an event pass.
#[test]
fn other_files_tools_and_input_pass() {
    let d = Dir::new();
    for name in ["a.rs", "a.json", "a.toml", "Makefile", "a.md.bak"] {
        assert_eq!(write(&d.file(name, None), "at 14:xx\n"), None, "{name}");
    }
    let rs = "*** Begin Patch\n*** Add File: src/a.rs\n+// at 14:xx\n*** End Patch\n";
    assert_eq!(denied_at("apply_patch", json!({"command": rs}), "23:59"), None);
    let moved =
        "*** Begin Patch\n*** Update File: a.md\n*** Move to: a.rs\n@@\n+at 14:xx\n*** End Patch\n";
    assert_eq!(denied_at("apply_patch", json!({"command": moved}), "23:59"), None);
    assert_eq!(denied_at("Bash", json!({"command": "echo 14:xx >> a.md"}), "23:59"), None);
    assert_eq!(
        denied_at("mcp__t3-code__t3_thread_read", json!({"message": "14:xx"}), "23:59"),
        None
    );
    assert_eq!(time_hook("not json", at("23:59"), Some(&berlin())), "");
    assert_eq!(time_hook("{}", at("23:59"), Some(&berlin())), "");
}

// Spec (design note §1, A: "Text a write keeps unchanged is never judged: only the lines it adds"): a placeholder
// already in the file passes when a write keeps it, whole or through an edit that carries it along.
#[test]
fn a_placeholder_the_write_keeps_passes() {
    let d = Dir::new();
    let old = "# Template\n\nstamp HH:MM\n";
    let note = d.file("t.md", Some(old));
    assert_eq!(write(&note, &format!("{old}added 13:05\n")), None);
    // A second copy of a kept line is added text (sol-loop r1, S4); a copy in code doesn't use up the kept one (r2, S6).
    assert!(write(&note, &format!("{old}stamp HH:MM\n")).is_some());
    assert_eq!(write(&note, &format!("```\nstamp HH:MM\n```\n{old}")), None);
    // …nor does a kept copy in code exempt the same line added as prose (sol-loop r3, S8).
    let code = d.file("c.md", Some("```\nstamp HH:MM\n```\n"));
    assert!(write(&code, "```\nstamp HH:MM\n```\nstamp HH:MM\n").is_some());
    assert_eq!(edit(&note, "stamp HH:MM\n", "stamp HH:MM\nadded 13:05\n"), None);
    // A fence the edit doesn't touch still holds its lines as code.
    let fenced = d.file("f.md", Some("```\nold\n```\n"));
    assert_eq!(edit(&fenced, "old\n", "old\nHH:MM\n"), None);
}

// ---------------------------------------------------------------- F: future times

// Spec (the steer: "A clock time more than 5 minutes later than the current Berlin time, on the same day, is denied
// unless its line carries an explicit future marker"; its witnesses: "about 04:45" at 04:14, "05:15" at 05:12,
// "about 06:27" at 06:21): the first and third are flagged; the second, 3 minutes ahead, is within the 5 minutes.
#[test]
fn the_witnesses_are_flagged_past_five_minutes() {
    assert_eq!(future("Done, finished about 04:45.", "04:14"), ["04:45"]);
    assert_eq!(future("Gate finished 05:15.", "05:12"), Vec::<String>::new());
    assert_eq!(future("Merged, about 06:27.", "06:21"), ["06:27"]);
    assert_eq!(future("ran 06:10–06:30", "06:21"), ["06:30"]);
    assert_eq!(future("at 06:26", "06:21"), Vec::<String>::new());
}

// Spec (the steer's fixed list: next, until, by, ETA, planned, due, deadline, expected, wait; "on the same day"; the
// design note §3: another day named on the line, a zone other than local, tomorrow's early hours, no zone known): none
// is flagged.
#[test]
fn plans_other_days_and_other_zones_are_not_flagged() {
    for marker in ["next", "until", "by", "ETA", "planned", "due", "deadline", "expected", "wait"] {
        assert_eq!(future(&format!("{marker} 14:00"), "08:00"), Vec::<String>::new(), "{marker}");
    }
    assert_eq!(future("Eta: 14:00", "08:00"), Vec::<String>::new());
    assert_eq!(future("on 3 Nov at 14:00", "08:00"), Vec::<String>::new());
    assert_eq!(future("Oct 28, 17:10", "08:00"), Vec::<String>::new());
    assert_eq!(future("2026-11-01 17:10", "08:00"), Vec::<String>::new());
    assert_eq!(future("2025-11-02 at 14:00", "08:00"), Vec::<String>::new());
    assert_eq!(future("yesterday 23:10", "08:00"), Vec::<String>::new());
    assert_eq!(future("16:00 UTC", "08:00"), Vec::<String>::new());
    assert_eq!(future("00:05", "23:58"), Vec::<String>::new());
    // More than 12 hours ahead is the evening before, written after midnight (design note §5).
    assert_eq!(future("ran 23:50–23:56", "02:19"), Vec::<String>::new());
    assert_eq!(future("at 14:00", "02:19"), ["14:00"]);
    assert_eq!(future("at 14:20", "02:19"), Vec::<String>::new());
    assert_eq!(future("`14:00` in code", "08:00"), Vec::<String>::new());
    // Today's date named on the line keeps the check.
    assert_eq!(future("2 Nov, 14:00", "08:00"), ["14:00"]);
    assert_eq!(future("2026-11-02 14:00", "08:00"), ["14:00"]);
    // A word holding a marker isn't the marker.
    assert_eq!(future("nearby 14:00", "08:00"), ["14:00"]);
    let none = findings("at 14:00", "", None);
    assert!(none.iter().all(|f| f.kind != Kind::Future), "{none:?}");
}

// Spec (the steer: "If false denies are more than about 1 in 200 lines with times, make this part add context (a
// one-line reminder) instead of denying. The placeholder part stays a deny"; measured about 26 in 200, design note §5;
// the reason: "write the real time from the clock, or mark it as a plan"): F answers with one line of context and no
// decision, naming the time, the clock and the fix; with a placeholder too, the answer is P's denial.
#[test]
fn a_future_time_is_a_reminder_not_a_denial() {
    let send = |text: &str, hm: &str| {
        let ev = json!({"hook_event_name": "PreToolUse", "tool_name": "mcp__t3-code__t3_thread_send",
            "tool_input": {"threadId": "t", "message": text}});
        let out = time_hook(&ev.to_string(), at(hm), Some(&berlin()));
        serde_json::from_str::<Value>(&out).unwrap()["hookSpecificOutput"].clone()
    };
    let h = send("Merged, about 06:27.", "06:21");
    assert_eq!(h["hookEventName"], "PreToolUse", "{h}");
    assert!(h.get("permissionDecision").is_none(), "{h}");
    let line = h["additionalContext"].as_str().unwrap();
    assert!(!line.contains('\n'), "{line}");
    for part in [
        "`06:27`",
        "Berlin 06:21 Mon 2 Nov",
        "write the real time from the clock, or mark it as a plan",
    ] {
        assert!(line.contains(part), "{part}: {line}");
    }
    let h = send("Merged, about 06:27, then 06:3x.", "06:21");
    assert_eq!(h["permissionDecision"], "deny", "{h}");
}

// Spec (design note §1, F: "Text a write keeps unchanged is never judged"): a future time already in the old text
// isn't flagged again.
#[test]
fn a_future_time_the_write_keeps_is_not_flagged() {
    let now = at("08:00").to_zoned(berlin());
    let got = findings("meeting 14:00\nnew 07:55\n", "meeting 14:00\n", Some(&now));
    assert!(got.is_empty(), "{got:?}");
}
