//! The queue guard (`lords guard queue`, guard.rs): a Claude Code `Stop` hook that keeps a turn from ending while
//! messages may wait in the session's own T3 queue (owner, 30 Oct about 23:40: agents said they had taken their whole
//! queue while items still sat in it). Its design and the evidence behind it: Harness/notes/2026-10-31-lords-queue-
//! guard.md (E1 what T3's CLI shows, D1 the design, A1 the inputs attacked here). The invariant: a stop never hides a
//! waiting item, and a stop is never trapped.

// Tests are outside the filesystem boundary (fsguard.rs): their fixtures write their own temp dirs.
#![allow(clippy::disallowed_methods, clippy::disallowed_types)]

use lords_core::guard::{thread_runs, Runs, Transcript, QUEUE_REASON};
use serde_json::{json, Value};

/// The session's own T3 thread in these tests.
const THREAD: &str = "thread:own";

fn runs(latest: &str, active: &str) -> Runs {
    Runs { latest: latest.into(), active: active.into() }
}

/// A `t3_queue` call as Claude Code records it in a session's transcript (design note E1-e: one `tool_use` per
/// assistant entry, the MCP server's prefix on the tool's name).
fn call(id: &str, input: Value) -> String {
    json!({"type": "assistant", "message": {"role": "assistant", "content": [
        {"type": "tool_use", "id": id, "name": "mcp__t3-code__t3_queue", "input": input}
    ]}})
    .to_string()
}

/// A call's result as Claude Code records it: the tool's JSON output as the `tool_result`'s text.
fn result(id: &str, output: Value) -> String {
    json!({"type": "user", "message": {"role": "user", "content": [
        {"type": "tool_result", "tool_use_id": id, "content": output.to_string()}
    ]}})
    .to_string()
}

/// A `list` call and its result showing these queued runs, `more` when the page has a next cursor.
fn list(id: &str, input: Value, shown: &[&str], more: bool) -> [String; 2] {
    let items: Vec<Value> =
        shown.iter().map(|r| json!({"queuedRunId": r, "text": "x", "truncated": false})).collect();
    let next = if more { json!(shown.len()) } else { Value::Null };
    [call(id, input), result(id, json!({"items": items, "nextCursor": next}))]
}

fn take(id: &str, action: &str, run: &str) -> String {
    call(id, json!({"action": action, "queuedRunId": run}))
}

fn may_wait(runs: &Runs, lines: &[String]) -> bool {
    let mut t = Transcript::new(THREAD, runs);
    for l in lines {
        t.line(l.as_bytes());
    }
    t.may_wait()
}

// Spec (design note E1, the real output of `t3 thread show <id> --json --messages 0`, T3 v0.1.5): the answer gives the
// thread's latest and active run as they are; `null` in either means there is nothing to compare; anything without
// both fields isn't the CLI's answer.
#[test]
fn the_cli_answer_gives_the_latest_and_the_active_run() {
    let show = r#"{"thread":{"threadId":"thread:own","status":"running","latestRunId":"run:own:ordinal:3","activeRunId":"run:own:ordinal:1","runCount":3,"pendingRequestCount":0},"messages":[]}"#;
    assert_eq!(thread_runs(show), Ok(Some(runs("run:own:ordinal:3", "run:own:ordinal:1"))));
    for idle in [
        r#"{"thread":{"latestRunId":"run:own:ordinal:1","activeRunId":null},"messages":[]}"#,
        r#"{"thread":{"latestRunId":null,"activeRunId":null},"messages":[]}"#,
    ] {
        assert_eq!(thread_runs(idle), Ok(None), "{idle}");
    }
    for bad in [
        "",
        "not json",
        "{}",
        r#"{"thread":{}}"#,
        r#"{"thread":{"latestRunId":"r2"}}"#,
        r#"{"thread":{"latestRunId":5,"activeRunId":"r1"}}"#,
        r#"{"thread":{"latestRunId":"r2","activeRunId":"r1"#,
    ] {
        assert!(thread_runs(bad).is_err(), "{bad}");
    }
}

// Spec (Lords owner, 31 Oct, accepting design D1 with rules (a) and (b); the attack list A1): with `latest != active`
// something may wait when (a) no list of this thread's queue came at or after the latest run's first appearance in
// the transcript, or (b) an id the session's latest listing showed has no `cancel` or `promote_to_steer` naming it
// after it was shown, or that listing stopped at its `limit`. With `latest == active` no later run exists, so
// nothing waits (E1.1). The calls of another thread's queue are not this thread's.
#[test]
fn something_may_wait_unless_the_session_took_everything_it_was_shown() {
    let fresh = || json!({"action": "list"});
    let cases: Vec<(&str, Runs, Vec<String>, bool)> = vec![
        ("an empty queue: no run after the active one", runs("r1", "r1"), vec![], false),
        ("one item, never listed", runs("r2", "r1"), vec![], true),
        (
            "one item listed and cancelled",
            runs("r2", "r1"),
            [&list("l1", fresh(), &["r2"], false)[..], &[take("c1", "cancel", "r2")]].concat(),
            false,
        ),
        (
            "an item with an attachment, listed and promoted",
            runs("r2", "r1"),
            [&list("l1", fresh(), &["r2"], false)[..], &[take("p1", "promote_to_steer", "r2")]]
                .concat(),
            false,
        ),
        (
            "three queued, only the last cancelled",
            runs("r4", "r1"),
            [&list("l1", fresh(), &["r2", "r3", "r4"], false)[..], &[take("c1", "cancel", "r4")]]
                .concat(),
            true,
        ),
        (
            "a list taken, then a new item",
            runs("r3", "r1"),
            [&list("l1", fresh(), &["r2"], false)[..], &[take("c1", "cancel", "r2")]].concat(),
            true,
        ),
        (
            "a list cut by its limit, the shown item taken",
            runs("r3", "r1"),
            [&list("l1", json!({"action": "list", "limit": 1}), &["r2"], true)[..], &[take(
                "c1", "cancel", "r2",
            )]]
            .concat(),
            true,
        ),
        (
            "a list cut by its limit, its last item shown and taken",
            runs("r3", "r1"),
            [
                &list("l1", json!({"action": "list", "limit": 2}), &["r2", "r3"], true)[..],
                &[take("c1", "cancel", "r2"), take("c2", "cancel", "r3")],
            ]
            .concat(),
            true,
        ),
        (
            "every page listed and every item taken",
            runs("r3", "r1"),
            [
                &list("l1", json!({"action": "list", "limit": 1}), &["r2"], true)[..],
                &list("l2", json!({"action": "list", "cursor": 1, "limit": 1}), &["r3"], false),
                &[take("c1", "cancel", "r2"), take("p1", "promote_to_steer", "r3")],
            ]
            .concat(),
            false,
        ),
        (
            "a later run someone else cancelled after the session listed it, then listed again",
            runs("r2", "r1"),
            [&list("l1", fresh(), &["r2"], false)[..], &list("l2", fresh(), &[], false)].concat(),
            false,
        ),
        (
            "a later run the session never saw (it may be cancelled: one needless block, design D1)",
            runs("r2", "r1"),
            list("l1", fresh(), &[], false).to_vec(),
            true,
        ),
        (
            "another thread's queue listed and taken",
            runs("r2", "r1"),
            [
                &list("l1", json!({"action": "list", "threadId": "thread:child"}), &["r2"], false)[..],
                &[call("c1", json!({"action": "cancel", "threadId": "thread:child", "queuedRunId": "r2"}))],
            ]
            .concat(),
            true,
        ),
        (
            "this thread named explicitly",
            runs("r2", "r1"),
            [
                &list("l1", json!({"action": "list", "threadId": THREAD}), &["r2"], false)[..],
                &[call("c1", json!({"action": "cancel", "threadId": THREAD, "queuedRunId": "r2"}))],
            ]
            .concat(),
            false,
        ),
        (
            "an old listing whose item was delivered as a later turn",
            runs("r3", "r3"),
            list("l1", fresh(), &["r2"], false).to_vec(),
            false,
        ),
        (
            "a cancel before the list that still shows the run",
            runs("r2", "r1"),
            [&[take("c1", "cancel", "r2")][..], &list("l1", fresh(), &["r2"], false)].concat(),
            true,
        ),
        (
            "a read or an edit doesn't take an item",
            runs("r2", "r1"),
            [
                &list("l1", fresh(), &["r2"], false)[..],
                &[take("r1x", "read", "r2"), call("e1", json!({"action": "edit", "queuedRunId": "r2", "text": "y"}))],
            ]
            .concat(),
            true,
        ),
    ];
    let wrong: Vec<String> = cases
        .iter()
        .filter(|(_, r, lines, want)| may_wait(r, lines) != *want)
        .map(|(name, _, _, want)| format!("{name}: want may_wait = {want}"))
        .collect();
    assert!(wrong.is_empty(), "{}", wrong.join("\n"));
}

// Spec (design note E1, this session's own transcript: Claude Code also writes copies of earlier messages into
// `api-request-blob` entries, later in the file): only the session's own entries count, so a copy of an old, empty
// listing never stands for a new one; a line cut short (a transcript still being written) and other tools' results
// are skipped; a result whose text comes in blocks reads as one.
#[test]
fn only_the_sessions_own_entries_count() {
    let r = runs("r2", "r1");
    let mut lines = list("l1", json!({"action": "list"}), &["r2"], false).to_vec();
    lines.push(
        json!({"type": "api-request-blob", "message": {"role": "user", "content": [
            {"type": "tool_result", "tool_use_id": "l1", "content": "{\"items\":[],\"nextCursor\":null}"}
        ]}})
        .to_string(),
    );
    lines.push(
        json!({"type": "api-request-blob", "message": {"role": "assistant", "content": [
            {"type": "tool_use", "id": "c9", "name": "mcp__t3-code__t3_queue", "input": {"action": "cancel", "queuedRunId": "r2"}}
        ]}})
        .to_string(),
    );
    assert!(may_wait(&r, &lines), "copies count for nothing");
    let mut lines = list("l1", json!({"action": "list"}), &["r2"], false).to_vec();
    lines.push(take("c1", "cancel", "r2"));
    lines.push("{\"type\":\"assistant\",\"message\":{\"content\":[{\"type\":\"tool_use\"".into());
    lines.push(result("other", json!({"items": [{"queuedRunId": "r9"}], "nextCursor": null})));
    assert!(!may_wait(&r, &lines), "a cut line and another tool's result are skipped");
    let blocks = json!({"type": "user", "message": {"role": "user", "content": [
        {"type": "tool_result", "tool_use_id": "l1", "content": [
            {"type": "text", "text": "{\"items\":[{\"queuedRunId\":\"r2\"}],\"nextCursor\":null}"}
        ]}
    ]}});
    let lines = vec![call("l1", json!({"action": "list"})), blocks.to_string()];
    assert!(may_wait(&r, &lines), "listed in blocks, not taken");
    let lines = [&lines[..], &[take("c1", "cancel", "r2")]].concat();
    assert!(!may_wait(&r, &lines), "listed in blocks, taken");
}

// Spec (brief, 30 Oct: "its reason tells the agent what to do, in the rule's words"; Lords owner, 31 Oct: "item(s)
// may be queued… If the list is empty, nothing waits: end the turn"): the reason names the rule's steps.
#[test]
fn the_reason_is_the_rules_steps() {
    for words in [
        "item(s) may be queued for this thread",
        "list them",
        "read the truncated ones in full",
        "cancel the text-only ones",
        "promote those with an attachment",
        "then answer together",
        "If the list is empty, nothing waits: end the turn",
    ] {
        assert!(QUEUE_REASON.contains(words), "{words}");
    }
}

/// The hook end to end, with a fake T3 CLI: a Windows `.cmd` file, as T3's own CLI is (`t3.cmd`).
#[cfg(windows)]
mod hook {
    use super::*;
    use lords_core::guard::{queue_hook, QueueEnv};
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::time::{Duration, Instant};
    use tempfile::TempDir;

    struct Fake {
        dir: TempDir,
    }

    impl Fake {
        fn new() -> Fake {
            let f = Fake { dir: TempDir::new().unwrap() };
            fs::create_dir_all(f.home()).unwrap();
            f
        }

        fn path(&self, rel: &str) -> PathBuf {
            self.dir.path().join(rel)
        }

        fn home(&self) -> PathBuf {
            self.path("tool-home")
        }

        /// The marker a fake CLI writes when it runs.
        fn ran(&self) -> bool {
            self.path("ran").exists()
        }

        /// A fake CLI that records it ran, then runs `body`.
        fn cli(&self, body: &str) -> PathBuf {
            let p = self.path("t3.cmd");
            let marker = self.path("ran");
            fs::write(&p, format!("@echo off\r\necho ran>\"{}\"\r\n{body}\r\n", marker.display()))
                .unwrap();
            p
        }

        /// A fake CLI that answers `latest` and `active` and exits 0.
        fn answering(&self, latest: &str, active: &str) -> PathBuf {
            let show = self.path("show.json");
            fs::write(
                &show,
                json!({"thread": {"threadId": THREAD, "status": "running", "latestRunId": latest, "activeRunId": active}, "messages": []})
                    .to_string(),
            )
            .unwrap();
            self.cli(&format!("type \"{}\"", show.display()))
        }

        fn env(&self, cli: Option<PathBuf>) -> QueueEnv {
            QueueEnv {
                thread: Some(THREAD.into()),
                cli,
                home: self.home(),
                deadline: Duration::from_secs(5),
            }
        }

        fn transcript(&self, lines: &[String]) -> PathBuf {
            let p = self.path("session.jsonl");
            fs::write(&p, lines.iter().map(|l| format!("{l}\n")).collect::<String>()).unwrap();
            p
        }

        /// A `Stop` event for session `s1` with this transcript.
        fn stop(&self, transcript: &Path, active: bool) -> String {
            json!({"hook_event_name": "Stop", "session_id": "s1", "transcript_path": transcript, "stop_hook_active": active, "cwd": "."})
                .to_string()
        }
    }

    fn blocked(stdout: &str) -> bool {
        if stdout.is_empty() {
            return false;
        }
        let v: Value = serde_json::from_str(stdout).unwrap();
        assert_eq!(v["reason"], QUEUE_REASON, "{stdout}");
        v["decision"] == "block"
    }

    // Spec (brief, 30 Oct: "on a timeout, an error, no thread id, or no CLI, let the stop through with a one-line
    // note"; "SubagentStop … must not be registered"; design note A1): each lets the stop through with a note, and
    // the CLI never runs when there is no thread to ask about or the event isn't a session's stop.
    #[test]
    fn every_failure_lets_the_stop_through_with_a_note() {
        let f = Fake::new();
        let t = f.transcript(&[]);
        let event = f.stop(&t, false);
        // A native subagent's stop, or any other event: not this hook's; the CLI doesn't run.
        let cli = f.answering("r2", "r1");
        let sub = event.replace("\"Stop\"", "\"SubagentStop\"");
        let a = queue_hook(&sub, &f.env(Some(cli.clone())));
        assert!(a.stdout.is_empty() && !f.ran(), "{a:?}");
        let a = queue_hook("not json", &f.env(Some(cli.clone())));
        assert!(a.stdout.is_empty() && a.note.is_some() && !f.ran(), "{a:?}");
        // A plain terminal session: no thread id; the CLI doesn't run.
        let a = queue_hook(&event, &QueueEnv { thread: None, ..f.env(Some(cli.clone())) });
        assert!(a.stdout.is_empty() && !f.ran(), "{a:?}");
        assert!(a.note.as_deref().unwrap_or("").contains("T3CODE_THREAD_ID"), "{a:?}");
        // No CLI, or one that isn't there.
        for env in [f.env(None), f.env(Some(f.path("gone/t3.cmd")))] {
            let a = queue_hook(&event, &env);
            assert!(a.stdout.is_empty() && a.note.is_some(), "{a:?}");
        }
        // T3 down: the CLI fails.
        let a = queue_hook(&event, &f.env(Some(f.cli("echo T3 is not running 1>&2\r\nexit /b 3"))));
        assert!(a.stdout.is_empty() && a.note.is_some() && f.ran(), "{a:?}");
        // Malformed output.
        let a = queue_hook(&event, &f.env(Some(f.cli("echo {\"thread\":"))));
        assert!(a.stdout.is_empty() && a.note.is_some(), "{a:?}");
        // The transcript can't be read.
        let gone = f.stop(&f.path("no-such.jsonl"), false);
        let a = queue_hook(&gone, &f.env(Some(f.answering("r2", "r1"))));
        assert!(a.stdout.is_empty() && a.note.is_some(), "{a:?}");
    }

    // Spec (brief, 30 Oct: "give the CLI a short deadline … on a timeout … let the stop through with a one-line
    // note"): a CLI slower than the deadline lets the stop through soon after the deadline.
    #[test]
    fn a_slow_cli_lets_the_stop_through_at_its_deadline() {
        let f = Fake::new();
        let cli = f.cli("ping -n 8 127.0.0.1 >nul");
        let event = f.stop(&f.transcript(&[]), false);
        let started = Instant::now();
        let a =
            queue_hook(&event, &QueueEnv { deadline: Duration::from_secs(1), ..f.env(Some(cli)) });
        let took = started.elapsed();
        assert!(a.stdout.is_empty(), "{a:?}");
        assert!(a.note.as_deref().unwrap_or("").contains("deadline"), "{a:?}");
        assert!(took < Duration::from_secs(4), "took {took:?}");
    }

    // Spec (brief, 30 Oct: "Never a loop: … Block at most twice in a row, then let it through with a note"; design
    // D1: the count is per session, kept only during a streak): with something waiting, the first stop and the next
    // (`stop_hook_active`) are blocked, the third goes through with a note, and a stop that isn't a hook's
    // continuation starts the count again; nothing is left in the tool's home once a stop goes through.
    #[test]
    fn it_blocks_at_most_twice_in_a_row() {
        let f = Fake::new();
        let env = f.env(Some(f.answering("r2", "r1")));
        let t = f.transcript(&[]);
        let first = queue_hook(&f.stop(&t, false), &env);
        assert!(blocked(&first.stdout), "{first:?}");
        let second = queue_hook(&f.stop(&t, true), &env);
        assert!(blocked(&second.stdout), "{second:?}");
        let third = queue_hook(&f.stop(&t, true), &env);
        assert!(third.stdout.is_empty(), "{third:?}");
        assert!(third.note.as_deref().unwrap_or("").contains("twice"), "{third:?}");
        let left: Vec<PathBuf> = walk(&f.home()).into_iter().filter(|p| p.is_file()).collect();
        assert!(left.is_empty(), "{left:?}");
        let again = queue_hook(&f.stop(&t, false), &env);
        assert!(blocked(&again.stdout), "{again:?}");
    }

    // Spec (design D1, A1: "an item queued, not taken: block"; "taken: through"): the hook decides from the CLI's
    // answer and the session's transcript.
    #[test]
    fn it_blocks_only_while_something_may_wait() {
        let f = Fake::new();
        let empty = f.transcript(&[]);
        let a = queue_hook(&f.stop(&empty, false), &f.env(Some(f.answering("r1", "r1"))));
        assert!(a.stdout.is_empty(), "{a:?}");
        let a = queue_hook(&f.stop(&empty, false), &f.env(Some(f.answering("r2", "r1"))));
        assert!(blocked(&a.stdout), "{a:?}");
        let taken = f.transcript(
            &[
                &list("l1", json!({"action": "list"}), &["r2"], false)[..],
                &[take("c1", "cancel", "r2")],
            ]
            .concat(),
        );
        let a = queue_hook(&f.stop(&taken, false), &f.env(Some(f.answering("r2", "r1"))));
        assert!(a.stdout.is_empty(), "{a:?}");
    }

    fn walk(dir: &Path) -> Vec<PathBuf> {
        let mut out = Vec::new();
        for e in fs::read_dir(dir).into_iter().flatten().flatten() {
            let p = e.path();
            if p.is_dir() {
                out.extend(walk(&p));
            }
            out.push(p);
        }
        out
    }
}
