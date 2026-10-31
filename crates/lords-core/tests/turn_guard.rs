//! The session and turn guards (`lords guard session`, `lords guard turn`; guard.rs). A `SessionStart` hook records the
//! Lords-written files a session reads; a `UserPromptSubmit` hook gives the agent the clock, and names any of those
//! files that changed since. The design and the inputs attacked here: Harness/notes/2026-10-31-lords-turn-hook.md
//! (D1, A1). The invariant: the time is right, a changed file is always named, an unchanged one never is, and a hook
//! never blocks or slows a turn.

// Tests are outside the filesystem boundary (fsguard.rs): their fixtures write their own temp dirs.
#![allow(clippy::disallowed_methods, clippy::disallowed_types)]

use jiff::tz::{Offset, TimeZone};
use jiff::Timestamp;
use lords_core::compose::{BEGIN, END};
use lords_core::guard::{clock_line, session_hook, turn_hook, RulesEnv, SESSION_RECORD_DAYS};
use serde_json::{json, Value};
use std::fs;
use std::path::{Path, PathBuf};
use tempfile::TempDir;

fn at(s: &str) -> Timestamp {
    s.parse().unwrap()
}

fn berlin() -> TimeZone {
    TimeZone::get("Europe/Berlin").unwrap()
}

// Spec (the Lords owner, 31 Oct: "one short line, `Berlin 07:41 Sat 31 Oct`, from the system clock. Name the zone only
// from what the system reports, never assumed"; design note A1, DST and an unknown zone): the local wall time tzdb
// gives for the zone, labelled by the zone's city; the hours either side of both 2026 DST changes in Berlin (hand
// worked: CEST is UTC+2 until 25 Oct 01:00 UTC and from 29 Mar 01:00 UTC, CET UTC+1 otherwise); a zone without an
// IANA name labelled by its offset; no zone at all is UTC, said so.
#[test]
fn the_clock_line_is_the_systems_local_time() {
    let tz = berlin();
    let cases = [
        ("2026-10-31T06:41:00Z", Some(&tz), "Berlin 07:41 Sat 31 Oct"),
        ("2026-11-01T08:00:00Z", Some(&tz), "Berlin 09:00 Sun 1 Nov"),
        ("2026-10-25T00:30:00Z", Some(&tz), "Berlin 02:30 Sun 25 Oct"),
        ("2026-10-25T01:30:00Z", Some(&tz), "Berlin 02:30 Sun 25 Oct"),
        ("2026-03-29T00:59:00Z", Some(&tz), "Berlin 01:59 Sun 29 Mar"),
        ("2026-03-29T01:30:00Z", Some(&tz), "Berlin 03:30 Sun 29 Mar"),
    ];
    for (now, zone, want) in cases {
        assert_eq!(clock_line(at(now), zone), want, "{now}");
    }
    let ba = TimeZone::get("America/Argentina/Buenos_Aires").unwrap();
    assert_eq!(clock_line(at("2026-10-31T06:41:00Z"), Some(&ba)), "Buenos Aires 03:41 Sat 31 Oct");
    let fixed = TimeZone::fixed(Offset::constant(3));
    assert_eq!(clock_line(at("2026-10-31T06:41:00Z"), Some(&fixed)), "UTC+03:00 09:41 Sat 31 Oct");
    assert_eq!(
        clock_line(at("2026-10-31T06:41:00Z"), None),
        "UTC 06:41 Sat 31 Oct (local zone unknown)"
    );
}

/// A place tree for one session: the tool home, a user config dir whose `CLAUDE.md` includes the tool home's
/// user-level file, and a checkout `root` with a session cwd two levels below it (`root/a/b`).
struct Place {
    dir: TempDir,
}

impl Place {
    fn new() -> Place {
        let p = Place { dir: TempDir::new().unwrap() };
        fs::create_dir_all(p.home()).unwrap();
        fs::create_dir_all(p.cwd()).unwrap();
        p.lords("home/out/claude-global.md", "global rules");
        let include = format!("@{}", p.path("home/out/claude-global.md").display());
        p.write("config/CLAUDE.md", &format!("{BEGIN}\n{include}\n{END}\n"));
        p.lords("root/CLAUDE.local.md", "place rules");
        p.lords("root/.lords/role-coder.md", "coder rules");
        p.lords("root/a/AGENTS.override.md", "codex rules");
        p
    }

    fn path(&self, rel: &str) -> PathBuf {
        self.dir.path().join(rel)
    }

    fn home(&self) -> PathBuf {
        self.path("home")
    }

    fn cwd(&self) -> PathBuf {
        self.path("root/a/b")
    }

    fn write(&self, rel: &str, text: &str) {
        let p = self.path(rel);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, text).unwrap();
    }

    /// A file as Lords writes it: its text between Lords' markers.
    fn lords(&self, rel: &str, text: &str) {
        self.write(rel, &format!("{BEGIN}\n{text}\n{END}\n"));
    }

    fn env(&self) -> RulesEnv {
        RulesEnv { home: self.home(), user_config: Some(self.path("config")) }
    }

    fn start(&self, session: &str, source: &str) {
        self.start_in(session, source, &self.cwd());
    }

    fn start_in(&self, session: &str, source: &str, cwd: &Path) {
        let ev = json!({"hook_event_name": "SessionStart", "session_id": session, "source": source, "cwd": cwd,
            "transcript_path": self.path("t.jsonl")});
        let a = session_hook(&ev.to_string(), &self.env());
        assert!(a.stdout.is_empty(), "SessionStart adds nothing: {a:?}");
    }

    /// The context one prompt gets, as its lines; the answer is checked to be context only, never a block.
    fn turn(&self, session: &str) -> Vec<String> {
        self.turn_in(session, &self.cwd())
    }

    fn turn_in(&self, session: &str, cwd: &Path) -> Vec<String> {
        let ev = json!({"hook_event_name": "UserPromptSubmit", "session_id": session, "cwd": cwd, "prompt": "go",
            "transcript_path": self.path("t.jsonl")});
        let a = turn_hook(&ev.to_string(), &self.env(), at(NOW), Some(&berlin()));
        context(&a.stdout)
    }

    /// A path as the guard names it.
    fn named(&self, rel: &str) -> String {
        self.path(rel).display().to_string().replace('\\', "/")
    }
}

const NOW: &str = "2026-10-31T06:41:00Z";
const CLOCK: &str = "Berlin 07:41 Sat 31 Oct";

/// The lines of a `UserPromptSubmit` answer's context; it must be exactly that shape, with no decision.
fn context(stdout: &str) -> Vec<String> {
    let v: Value = serde_json::from_str(stdout).unwrap_or_else(|e| panic!("{e}: {stdout:?}"));
    assert!(v.get("decision").is_none(), "{stdout}");
    assert_eq!(v["hookSpecificOutput"]["hookEventName"], "UserPromptSubmit", "{stdout}");
    let text = v["hookSpecificOutput"]["additionalContext"].as_str().unwrap();
    text.lines().map(String::from).collect()
}

/// The rules line a context names, if any: the paths after "re-read ".
fn rereads(lines: &[String]) -> Option<Vec<String>> {
    let l = lines.iter().find(|l| l.contains("re-read "))?;
    Some(l.split("re-read ").nth(1).unwrap().split(", ").map(String::from).collect())
}

// Spec (the brief: "It adds `additionalContext` of one short line … Only when a recorded file changed, it adds a second
// line"): a session whose files didn't change gets the clock line only, at every prompt.
#[test]
fn an_unchanged_session_hears_only_the_clock() {
    let p = Place::new();
    p.start("s1", "startup");
    assert_eq!(p.turn("s1"), [CLOCK]);
    assert_eq!(p.turn("s1"), [CLOCK]);
}

// Spec (the brief: "Lords rules changed since this session started: re-read <paths>"; A1: a changed, a deleted and an
// added file are each named once; a file without Lords' marker never is): each kind of change to the files the
// session reads is named at the next prompt, once, with exactly the changed paths.
#[test]
fn a_changed_file_is_named_once() {
    let p = Place::new();
    p.start("s1", "startup");
    // Changed: the place file, the role file, the user-level include.
    p.lords("root/CLAUDE.local.md", "place rules, new");
    p.lords("home/out/claude-global.md", "global rules, new");
    let lines = p.turn("s1");
    assert_eq!(lines[0], CLOCK);
    assert!(
        lines[1].starts_with("Lords rules changed since this session started: re-read "),
        "{lines:?}"
    );
    let mut got = rereads(&lines).unwrap();
    got.sort();
    let mut want = vec![p.named("home/out/claude-global.md"), p.named("root/CLAUDE.local.md")];
    want.sort();
    assert_eq!(got, want);
    assert_eq!(p.turn("s1"), [CLOCK], "named once");
    // Deleted.
    fs::remove_file(p.path("root/.lords/role-coder.md")).unwrap();
    assert_eq!(
        rereads(&p.turn("s1")).unwrap(),
        [format!("{} (removed)", p.named("root/.lords/role-coder.md"))]
    );
    assert_eq!(p.turn("s1"), [CLOCK]);
    // Added, in the cwd itself and as a new role file.
    p.lords("root/a/b/CLAUDE.local.md", "deeper rules");
    p.lords("root/.lords/role-reviewer.md", "reviewer rules");
    let mut got = rereads(&p.turn("s1")).unwrap();
    got.sort();
    let mut want =
        vec![p.named("root/.lords/role-reviewer.md"), p.named("root/a/b/CLAUDE.local.md")];
    want.sort();
    assert_eq!(got, want);
    // A user's own file, without Lords' marker, is never one of them.
    p.write("root/a/CLAUDE.local.md", "my own notes\n");
    assert_eq!(p.turn("s1"), [CLOCK]);
    p.write("root/a/CLAUDE.local.md", "my own notes, edited\n");
    assert_eq!(p.turn("s1"), [CLOCK]);
}

// Spec (A1: "a missing record (a session that started before install)"): with no record, the guard can't tell what
// the session read, so it says the rules may have changed and names every Lords file the session reads, once; then it
// records them, and later prompts are quiet until something changes.
#[test]
fn a_session_without_a_record_is_told_once() {
    let p = Place::new();
    let lines = p.turn("old");
    assert_eq!(lines[0], CLOCK);
    assert!(
        lines[1].starts_with("Lords rules may have changed since this session started: re-read "),
        "{lines:?}"
    );
    let mut got = rereads(&lines).unwrap();
    got.sort();
    let mut want = vec![
        p.named("home/out/claude-global.md"),
        p.named("root/.lords/role-coder.md"),
        p.named("root/CLAUDE.local.md"),
        p.named("root/a/AGENTS.override.md"),
    ];
    want.sort();
    assert_eq!(got, want);
    assert_eq!(p.turn("old"), [CLOCK]);
    p.lords("root/CLAUDE.local.md", "place rules, new");
    assert_eq!(rereads(&p.turn("old")).unwrap(), [p.named("root/CLAUDE.local.md")]);
}

// Spec (A1: "two sessions in one place"): each session has its own record; a change is named once to each.
#[test]
fn two_sessions_in_one_place_each_hear_a_change_once() {
    let p = Place::new();
    p.start("s1", "startup");
    p.start("s2", "startup");
    p.lords("root/.lords/role-coder.md", "coder rules, new");
    assert_eq!(rereads(&p.turn("s1")).unwrap(), [p.named("root/.lords/role-coder.md")]);
    assert_eq!(p.turn("s1"), [CLOCK]);
    assert_eq!(rereads(&p.turn("s2")).unwrap(), [p.named("root/.lords/role-coder.md")]);
    assert_eq!(p.turn("s2"), [CLOCK]);
}

// Spec (A1, from Claude Code's docs: instruction files are re-loaded after a compaction, and on resume): on
// `compact` and `resume` the files Claude reloads itself (place `CLAUDE.local.md` files, the user `CLAUDE.md`'s
// includes) are recorded anew, so their change before it isn't named; a file the agent read itself (a role file)
// keeps its recorded hash, so its change before it still is. `startup` and `clear` record afresh.
#[test]
fn a_compaction_or_resume_keeps_what_claude_does_not_reload() {
    for source in ["compact", "resume"] {
        let p = Place::new();
        p.start("s1", "startup");
        p.lords("root/CLAUDE.local.md", "place rules, new");
        p.lords("home/out/claude-global.md", "global rules, new");
        p.lords("root/.lords/role-coder.md", "coder rules, new");
        p.start("s1", source);
        assert_eq!(
            rereads(&p.turn("s1")).unwrap(),
            [p.named("root/.lords/role-coder.md")],
            "{source}"
        );
        assert_eq!(p.turn("s1"), [CLOCK], "{source}");
    }
    for source in ["startup", "clear"] {
        let p = Place::new();
        p.start("s1", "startup");
        p.lords("root/.lords/role-coder.md", "coder rules, new");
        p.start("s1", source);
        assert_eq!(p.turn("s1"), [CLOCK], "{source}");
    }
}

// Spec (A1: "a cwd outside every place"; "a huge ancestor chain"): with no Lords file in the cwd's chain and no
// include, only the clock; a 60-level chain is scanned, and a file at its top is still named when it changes.
#[test]
fn a_cwd_outside_every_place_and_a_deep_one() {
    let p = Place::new();
    let outside = p.path("elsewhere/x");
    fs::create_dir_all(&outside).unwrap();
    let bare = RulesEnv { home: p.home(), user_config: None };
    let ev = |name: &str| {
        json!({"hook_event_name": name, "session_id": "s9", "source": "startup", "cwd": outside, "prompt": "go"})
            .to_string()
    };
    assert!(session_hook(&ev("SessionStart"), &bare).stdout.is_empty());
    let a = turn_hook(&ev("UserPromptSubmit"), &bare, at(NOW), Some(&berlin()));
    assert_eq!(context(&a.stdout), [CLOCK]);
    let deep: PathBuf = (0..60).fold(p.path("root"), |d, i| d.join(format!("d{i}")));
    fs::create_dir_all(&deep).unwrap();
    p.start_in("s3", "startup", &deep);
    assert_eq!(p.turn_in("s3", &deep), [CLOCK]);
    p.lords("root/CLAUDE.local.md", "place rules, new");
    assert_eq!(rereads(&p.turn_in("s3", &deep)).unwrap(), [p.named("root/CLAUDE.local.md")]);
}

// Spec (the brief: "Everything goes in the tool home, small, and is removed when it's stale"): a session record older
// than SESSION_RECORD_DAYS is removed at the next session start of any session; a recent one stays.
#[test]
fn a_stale_record_is_removed_at_the_next_start() {
    let p = Place::new();
    p.start("old", "startup");
    p.start("recent", "startup");
    let dir = p.home().join("guard").join("session");
    let old = dir.join("old");
    assert!(old.is_file() && dir.join("recent").is_file(), "records in the tool home");
    let then = std::time::SystemTime::now()
        - std::time::Duration::from_secs((SESSION_RECORD_DAYS + 1) * 24 * 3600);
    fs::File::options().write(true).open(&old).unwrap().set_modified(then).unwrap();
    p.start("new", "startup");
    assert!(!old.exists() && dir.join("recent").is_file() && dir.join("new").is_file());
}

// Spec (the brief: "a hook never blocks or slows a turn"; A1: malformed input, other events, errors): another event or
// unreadable input gets nothing; a rules part that can't work (the tool home is a file) still gives the clock line,
// with a note; no answer is ever a block.
#[test]
fn a_hook_never_blocks() {
    let p = Place::new();
    for input in ["", "not json", r#"{"hook_event_name":"SubagentStart","session_id":"s1"}"#] {
        let a = turn_hook(input, &p.env(), at(NOW), Some(&berlin()));
        assert!(a.stdout.is_empty(), "{input}: {a:?}");
        assert!(session_hook(input, &p.env()).stdout.is_empty(), "{input}");
    }
    p.write("filehome", "not a dir");
    let broken = RulesEnv { home: p.path("filehome"), user_config: Some(p.path("config")) };
    let ev = json!({"hook_event_name": "UserPromptSubmit", "session_id": "s1", "cwd": p.cwd(), "prompt": "go"});
    let a = turn_hook(&ev.to_string(), &broken, at(NOW), Some(&berlin()));
    assert_eq!(context(&a.stdout)[0], CLOCK, "{a:?}");
    assert!(a.note.is_some(), "{a:?}");
}
