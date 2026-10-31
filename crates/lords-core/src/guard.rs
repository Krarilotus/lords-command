//! Guards a harness runs as hooks (owner, 30 Oct 2026): `rm` before an agent's tool call, answering at once instead
//! of leaving the call to stop on an approval prompt nobody sees; `queue` when an agent's turn would end; `session`
//! and `turn` when a session and a prompt start (below). A Claude child in full-access mode sat about 30
//! minutes on Claude Code's "dangerous rm on a possibly-empty variable path" prompt for `rm -f $OUT/$id.json`; its
//! parent saw only "running".
//!
//! `rm`: a path that holds a shell variable deletes the wrong place when the variable is empty (`rm -rf "$OUT/"` is
//! `rm -rf /`). The only form let through is `"${VAR:?}"`, quoted, which stops the command when `VAR` is empty or
//! unset. Quoting alone (`"$OUT/x"`) doesn't: an empty `OUT` still makes it `/x`, so it is refused too.
//!
//! Scope: the plain form only (orchestrator, 30 Oct, after the rm-guard review r1: a guard on the deletion path must
//! stay simple enough to finish reviewing). A simple command whose word after leading `NAME=value` assignments is
//! `rm` or `rm.exe`, or a path to one; every operand after it is checked. Not covered, by design: `rm` behind
//! keywords, wrappers or their options (`then rm`, `sudo -u x rm`, `time -p rm`), leading redirections, `rm` inside a
//! string another command runs (`trap`, `eval`, `sh -c`), other spellings of its name (`$'\x72m'`), `find -delete`,
//! `xargs rm` and `rm $(…)`. A miss is no regression: the call goes on to Claude Code's own check, as before the guard.

use crate::fsguard::{Contained, Part, Root};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// What the agent is told to write instead.
pub const RM_FIX: &str = "\"${VAR:?}/name\"";

/// One word of a shell command, as far as the guard needs it: its text and its variable expansions.
#[derive(Debug, Default)]
struct Word {
    text: String,
    /// Each `$` expansion in the word: whether it was inside double quotes, and whether it is `${NAME:?…}`.
    vars: Vec<(bool, bool)>,
}

/// The simple commands of `command`, each as its words (split at `;`, `&`, `|`, `(`, `)` and newlines outside quotes).
fn commands(command: &str) -> Vec<Vec<Word>> {
    let c: Vec<char> = command.chars().collect();
    let (mut out, mut words, mut word) = (Vec::new(), Vec::new(), Word::default());
    let mut started = false;
    let (mut single, mut double) = (false, false);
    // Whether the character before this one was an active redirection operator (`>` or `<`, unquoted and unescaped):
    // a literal one (`\>`, `">"`) is word text and starts no redirection (rm-guard fix check r3, R9).
    let mut after_op = false;
    let mut i = 0;
    let end_word = |words: &mut Vec<Word>, word: &mut Word, started: &mut bool| {
        if *started {
            words.push(std::mem::take(word));
            *started = false;
        }
    };
    while i < c.len() {
        let ch = c[i];
        let op_before = std::mem::take(&mut after_op);
        if single {
            if ch == '\'' {
                single = false;
            } else {
                word.text.push(ch);
            }
            i += 1;
            continue;
        }
        match ch {
            '\\' if i + 1 < c.len() => {
                if c[i + 1] != '\n' {
                    word.text.push(c[i + 1]);
                    started = true;
                }
                i += 2;
                continue;
            }
            '\'' if !double => {
                single = true;
                started = true;
            }
            // `$'…'`: ANSI-C quoting, a literal with backslash escapes (sol-loop r1, S6).
            '$' if !double && i + 1 < c.len() && c[i + 1] == '\'' => {
                started = true;
                let mut j = i + 2;
                while j < c.len() && c[j] != '\'' {
                    if c[j] == '\\' && j + 1 < c.len() {
                        word.text.push(c[j + 1]);
                        j += 2;
                    } else {
                        word.text.push(c[j]);
                        j += 1;
                    }
                }
                i = j + 1;
                continue;
            }
            // A comment: from an unquoted `#` at a word's start to the line's end (sol-loop r1, S5).
            '#' if !double && !started => {
                while i < c.len() && c[i] != '\n' {
                    i += 1;
                }
                continue;
            }
            '"' => {
                double = !double;
                started = true;
            }
            '$' if i + 1 < c.len()
                && (c[i + 1] == '{'
                    || c[i + 1] == '_'
                    || c[i + 1].is_ascii_alphanumeric()
                    || "@*#?$!-".contains(c[i + 1])) =>
            {
                started = true;
                if c[i + 1] == '{' {
                    let close = c[i..].iter().position(|x| *x == '}').map_or(c.len(), |p| i + p);
                    let inner: String = c[i + 2..close.min(c.len())].iter().collect();
                    let name_end = inner
                        .find(|x: char| !(x == '_' || x.is_ascii_alphanumeric()))
                        .unwrap_or(inner.len());
                    let guarded = name_end > 0 && inner[name_end..].starts_with(":?");
                    word.vars.push((double, guarded));
                    word.text.extend(&c[i..(close + 1).min(c.len())]);
                    i = close + 1;
                } else {
                    let mut j = i + 1;
                    if c[j].is_ascii_alphabetic() || c[j] == '_' {
                        while j < c.len() && (c[j] == '_' || c[j].is_ascii_alphanumeric()) {
                            j += 1;
                        }
                    } else {
                        j += 1;
                    }
                    word.vars.push((double, false));
                    word.text.extend(&c[i..j]);
                    i = j;
                }
                continue;
            }
            ' ' | '\t' if !double => end_word(&mut words, &mut word, &mut started),
            // `&` and `|` that are part of a trailing redirection (`2>&1`, `&>`, `>|`) don't end the command: what
            // follows is still this command's (rm-guard fix check r2, R9).
            '&' | '|' if !double && (op_before || ch == '&' && c.get(i + 1) == Some(&'>')) => {
                word.text.push(ch);
                started = true;
            }
            ';' | '&' | '|' | '(' | ')' | '\n' if !double => {
                end_word(&mut words, &mut word, &mut started);
                if !words.is_empty() {
                    out.push(std::mem::take(&mut words));
                }
            }
            _ => {
                after_op = !double && matches!(ch, '>' | '<');
                word.text.push(ch);
                started = true;
            }
        }
        i += 1;
    }
    end_word(&mut words, &mut word, &mut started);
    if !words.is_empty() {
        out.push(words);
    }
    out
}

/// The first operand of a plain `rm` command holding a variable not written `"${VAR:?}"`, if any (the module's
/// scope). Every operand is checked, redirection targets and options included: a false refusal costs a retry.
pub fn unguarded_rm(command: &str) -> Option<String> {
    let assignment = |t: &str| {
        t.contains('=')
            && !t.starts_with('=')
            && t.split('=')
                .next()
                .is_some_and(|n| n.chars().all(|x| x == '_' || x.is_ascii_alphanumeric()))
    };
    for words in commands(command) {
        let mut rest = words.iter().skip_while(|w| assignment(&w.text));
        let Some(first) = rest.next() else { continue };
        if !is_rm(&first.text) {
            continue;
        }
        if let Some(w) =
            rest.find(|w| w.vars.iter().any(|(quoted, guarded)| !(*quoted && *guarded)))
        {
            return Some(w.text.clone());
        }
    }
    None
}

/// Whether a command word names `rm`: `rm` or `rm.exe` (any case), bare or as a path's last part (rm-guard review r1,
/// R5: `rm.exe` is the same GNU rm on Git Bash).
fn is_rm(word: &str) -> bool {
    let name = word.rsplit(['/', '\\']).next().unwrap_or(word);
    name.eq_ignore_ascii_case("rm") || name.eq_ignore_ascii_case("rm.exe")
}

/// The guard hook's answer to one `PreToolUse` event (the harness's stdin JSON): a denial naming the fix, or nothing
/// (empty: the call goes on to the harness's own checks). Only Bash calls are looked at; input that isn't such an
/// event is let through, so the guard never stops a call it can't read.
pub fn rm_hook(input: &str) -> String {
    let Ok(ev) = serde_json::from_str::<Value>(input) else { return String::new() };
    if ev["tool_name"] != "Bash" {
        return String::new();
    }
    let Some(command) = ev["tool_input"]["command"].as_str() else { return String::new() };
    match unguarded_rm(command) {
        None => String::new(),
        Some(word) => json!({ "hookSpecificOutput": {
            "hookEventName": "PreToolUse",
            "permissionDecision": "deny",
            "permissionDecisionReason": format!(
                "Lords: `rm` with a variable path ({word}) deletes the wrong place if the variable is empty. Write \
                 each variable as {RM_FIX}: quoted, with `:?`, so an empty one stops the command."
            ),
        } })
        .to_string(),
    }
}

// ---------------------------------------------------------------- queue
//
// `queue` (owner, 30 Oct about 23:40): agents said they had taken their whole T3 queue while items still sat in it.
// A Claude Code `Stop` hook keeps a turn from ending while something may wait in the session's own T3 queue. T3's
// CLI can't count the queue: `t3 thread show --json` gives the thread's latest and active run and drops the runs'
// states, and a cancelled or promoted item stays the latest run (design note E1, Harness/notes/2026-10-31-lords-
// queue-guard.md). So the guard reads both runs, and the session's transcript for what it listed in one call and
// took (Lords owner, 31 Oct, rules a and b). It fails open: a stop it can't judge goes through with a note. It never
// traps: at most two blocks in a row. Its limit: an item a later run overtook after a stop went through anyway isn't
// held again (`Transcript::may_wait`).

/// What the agent is told when its stop is held: the rule's steps (rule.orchestrator-queue-discipline). "May": the
/// latest run can be one someone else cancelled (design note D1). "In one call": only a listing on one page counts.
pub const QUEUE_REASON: &str = "Lords: item(s) may be queued for this thread: list it in one call (no cursor, a \
    `limit` up to 100, so no next cursor), read the truncated ones in full, cancel the text-only ones, promote those \
    with an attachment, then answer together. If the list is empty, nothing waits: end the turn.";

/// How long the T3 CLI may take: measured 0.8 to 8.3 s on T3 v0.1.5 on threads of 96 and 424 items (design note E2).
pub const QUEUE_DEADLINE: Duration = Duration::from_secs(10);

/// Blocks in a row before a stop goes through anyway (brief, 30 Oct: "Block at most twice in a row").
const MAX_BLOCKS: u32 = 2;

/// A thread's latest and active run as T3's CLI shows them: run ids, compared only for equality (an id's `ordinal:`
/// suffix needn't be the run's ordinal; design note E1-c).
#[derive(Debug, PartialEq, Eq)]
pub struct Runs {
    pub latest: String,
    pub active: String,
}

/// The runs in the answer of `t3 thread show <id> --json --messages 0`: `None` when either is `null` (no run yet,
/// or none active: nothing to compare); an error for anything that isn't such an answer.
pub fn thread_runs(show: &str) -> Result<Option<Runs>, String> {
    let v: Value =
        serde_json::from_str(show).map_err(|e| format!("its answer isn't JSON ({e})"))?;
    let id = |key: &str| match v["thread"].get(key) {
        Some(Value::String(s)) => Ok(Some(s.clone())),
        Some(Value::Null) => Ok(None),
        _ => Err(format!("its answer has no `thread.{key}`")),
    };
    Ok(match (id("latestRunId")?, id("activeRunId")?) {
        (Some(latest), Some(active)) => Some(Runs { latest, active }),
        _ => None,
    })
}

/// A complete listing of this thread's queue: a `t3_queue list` call with no cursor (or cursor 0) whose result has
/// `nextCursor: null`, so it showed the whole queue at once. The line of its result and the queued runs it showed.
/// A page of a longer listing never is one: T3 reads the queue afresh for each page and slices it by offset, so a
/// take or a reorder between pages makes a later page skip runs (sol-loop r1 S1; review r1 R1, R2).
struct Listing {
    line: usize,
    shown: Vec<String>,
}

/// What a session's transcript shows of its own thread's queue, fed line by line (Claude Code's JSONL, one entry a
/// line). Only the session's own `user` and `assistant` entries count, never the copies of requests it also keeps
/// (`api-request-blob`), and only `t3_queue` calls on this thread: no `threadId`, or this one, and no
/// `environmentId`. A `cancel` or `promote_to_steer` takes the run it names, whatever its result.
pub struct Transcript {
    thread: String,
    latest: String,
    active: String,
    /// The lines worth parsing: those naming the latest run, `t3_queue` or `nextCursor` (one pass, not three: a
    /// transcript can be hundreds of MB).
    wanted: regex::bytes::Regex,
    line: usize,
    /// The tool-use ids of the `list` calls with no cursor (or cursor 0).
    lists: std::collections::HashSet<String>,
    /// The latest complete listing.
    listing: Option<Listing>,
    /// Each run taken, with the line of the call.
    taken: Vec<(usize, String)>,
    /// The first line of the session's own entries that names the latest run.
    latest_seen: Option<usize>,
}

impl Transcript {
    pub fn new(thread: &str, runs: &Runs) -> Transcript {
        let wanted = format!("{}|t3_queue|nextCursor", regex::escape(&runs.latest));
        Transcript {
            thread: thread.into(),
            latest: runs.latest.clone(),
            active: runs.active.clone(),
            wanted: regex::bytes::Regex::new(&wanted).expect("escaped literals make a regex"),
            line: 0,
            lists: Default::default(),
            listing: None,
            taken: Vec::new(),
            latest_seen: None,
        }
    }

    pub fn line(&mut self, line: &[u8]) {
        let n = self.line;
        self.line += 1;
        // Most lines are none of these: they aren't parsed.
        if !self.wanted.is_match(line) {
            return;
        }
        let names_latest =
            self.wanted.find_iter(line).any(|m| m.as_bytes() == self.latest.as_bytes());
        let Ok(entry) = serde_json::from_slice::<Value>(line) else { return };
        if !matches!(entry["type"].as_str(), Some("user" | "assistant")) {
            return;
        }
        if names_latest && self.latest_seen.is_none() {
            self.latest_seen = Some(n);
        }
        for c in entry["message"]["content"].as_array().into_iter().flatten() {
            match c["type"].as_str() {
                Some("tool_use") if self.own_queue_call(c) => {
                    let input = &c["input"];
                    match input["action"].as_str() {
                        Some("list")
                            if input.get("cursor").is_none_or(|v| v.is_null() || v == 0) =>
                        {
                            if let Some(id) = c["id"].as_str() {
                                self.lists.insert(id.into());
                            }
                        }
                        Some("cancel" | "promote_to_steer") => {
                            if let Some(run) = input["queuedRunId"].as_str() {
                                self.taken.push((n, run.into()));
                            }
                        }
                        _ => {}
                    }
                }
                Some("tool_result")
                    if c["tool_use_id"].as_str().is_some_and(|id| self.lists.contains(id)) =>
                {
                    let Ok(out) = serde_json::from_str::<Value>(&result_text(&c["content"])) else {
                        continue;
                    };
                    let Some(items) = out["items"].as_array() else { continue };
                    if out.get("nextCursor") != Some(&Value::Null) {
                        continue;
                    }
                    let shown =
                        items.iter().filter_map(|i| i["queuedRunId"].as_str().map(String::from));
                    self.listing = Some(Listing { line: n, shown: shown.collect() });
                }
                _ => {}
            }
        }
    }

    /// Whether something may wait that the session hasn't taken. Only while a run later than the active one exists
    /// (design note E1); then (a) no complete listing came at or after the latest run first appeared, or (b) the
    /// latest complete listing showed a run no `cancel` or `promote_to_steer` named after it (Lords owner, 31 Oct,
    /// and after review r1). Pages of a longer listing count for nothing, so a queue listed in pages is held up to
    /// the cap.
    ///
    /// The limit of the invariant (review r1 R3, Lords owner: documented, not repaired): with `latest == active`
    /// nothing is held. Once a stop has gone through with items waiting (at the cap, or failing open), T3 may start
    /// a later run first (an automatic completion goes ahead of older messages); an older item it overtook is then not
    /// held again while that run is the latest. Only T3 itself can say what is still queued (design note F1).
    pub fn may_wait(&self) -> bool {
        if self.latest == self.active {
            return false;
        }
        let Some(l) = &self.listing else { return true };
        if self.latest_seen.is_none_or(|f| l.line < f) {
            return true;
        }
        l.shown.iter().any(|r| !self.taken.iter().any(|(n, t)| *n > l.line && t == r))
    }

    fn own_queue_call(&self, c: &Value) -> bool {
        let name = c["name"].as_str().unwrap_or("");
        let input = &c["input"];
        (name == "t3_queue" || name.ends_with("__t3_queue"))
            && input.get("environmentId").is_none_or(Value::is_null)
            && input.get("threadId").is_none_or(|t| t.is_null() || *t == *self.thread)
    }
}

/// A tool result's text: the content itself, or its text blocks joined.
fn result_text(content: &Value) -> String {
    match content {
        Value::String(s) => s.clone(),
        Value::Array(blocks) => blocks.iter().filter_map(|b| b["text"].as_str()).collect(),
        _ => String::new(),
    }
}

/// Where the queue guard looks: the session's T3 thread and CLI (`T3CODE_THREAD_ID` and `T3CODE_CLI_PATH`, which T3
/// sets for the sessions it starts), the tool's home (for the count of blocks in a row) and the CLI's deadline.
pub struct QueueEnv {
    pub thread: Option<String>,
    pub cli: Option<PathBuf>,
    pub home: PathBuf,
    pub deadline: Duration,
}

/// The queue guard's answer: what Claude Code reads on stdout (a block, or nothing) and a note for stderr.
#[derive(Debug)]
pub struct Answer {
    pub stdout: String,
    pub note: Option<String>,
}

/// The queue guard's answer to one Claude Code event (its stdin JSON). Only a `Stop` is looked at; a subagent's
/// (`SubagentStop`) or any other goes through without a word. A stop goes through with a note whenever the guard
/// can't judge it: no thread or CLI, a CLI that fails, is late or answers something else, or a transcript it can't
/// read. It is held, with `QUEUE_REASON`, when something may wait (`Transcript::may_wait`), at most twice in a row.
pub fn queue_hook(input: &str, env: &QueueEnv) -> Answer {
    let quiet = || Answer { stdout: String::new(), note: None };
    let through = |why: String| Answer { stdout: String::new(), note: Some(why) };
    let Ok(ev) = serde_json::from_str::<Value>(input) else {
        return through("the event can't be read".into());
    };
    if ev["hook_event_name"] != "Stop" {
        return quiet();
    }
    let Some(thread) = env.thread.as_deref().filter(|t| !t.is_empty()) else {
        return through("no T3CODE_THREAD_ID: a session T3 didn't start".into());
    };
    let Some(cli) = env.cli.as_deref() else {
        return through("no T3CODE_CLI_PATH".into());
    };
    let streak = Streak::of(&env.home, ev["session_id"].as_str());
    let blocks = if ev["stop_hook_active"] == true { streak.blocks() } else { 0 };
    let verdict = if blocks >= MAX_BLOCKS {
        Err("held twice in a row already".into())
    } else {
        waits(&ev, thread, cli, env.deadline)
    };
    match verdict {
        Ok(true) => match streak.set(blocks + 1) {
            Ok(()) => Answer {
                stdout: json!({"decision": "block", "reason": QUEUE_REASON}).to_string(),
                note: None,
            },
            Err(e) => {
                streak.clear();
                through(format!("can't count the stops held in a row ({e})"))
            }
        },
        Ok(false) => {
            streak.clear();
            quiet()
        }
        Err(why) => {
            streak.clear();
            through(why)
        }
    }
}

/// Whether something may wait in `thread`'s queue: the CLI's two runs, then the session's transcript.
fn waits(ev: &Value, thread: &str, cli: &Path, deadline: Duration) -> Result<bool, String> {
    let args = ["thread", "show", thread, "--json", "--messages", "0"];
    let show =
        crate::process::t3_cli(cli, &args, deadline).map_err(|e| format!("T3's CLI: {e}"))?;
    let Some(runs) = thread_runs(&show).map_err(|e| format!("T3's CLI: {e}"))? else {
        return Ok(false);
    };
    if runs.latest == runs.active {
        return Ok(false);
    }
    let path = ev["transcript_path"].as_str().ok_or("the event names no transcript")?;
    let mut t = Transcript::new(thread, &runs);
    crate::fsguard::read_lines(path, |l| t.line(l))
        .map_err(|e| format!("the transcript can't be read ({path}: {e})"))?;
    Ok(t.may_wait())
}

/// The stops held in a row for one session: `<tool home>/guard/queue/<session id>`, a file that is there only while
/// they are being held. A session id that can't be a file name has no place: nothing could count its held stops, so
/// none is held.
struct Streak {
    place: Option<Contained>,
}

impl Streak {
    fn of(home: &Path, session: Option<&str>) -> Streak {
        let place = (|| {
            let root = Root::home(home).ok()?;
            let parts =
                [Part::new("guard").ok()?, Part::new("queue").ok()?, Part::new(session?).ok()?];
            Some(root.join(&parts))
        })();
        Streak { place }
    }

    /// Unknown counts as the most: a count it can't read never holds a stop again.
    fn blocks(&self) -> u32 {
        let Some(p) = &self.place else { return MAX_BLOCKS };
        match crate::fsguard::read_text(p.path()) {
            Ok(None) => 0,
            Ok(Some(t)) => t.trim().parse().unwrap_or(MAX_BLOCKS),
            Err(_) => MAX_BLOCKS,
        }
    }

    fn set(&self, n: u32) -> Result<(), String> {
        let p = self.place.as_ref().ok_or("the session id is no file name")?;
        let dir = p.root().join(&p.parts()[..p.parts().len() - 1]);
        crate::fsguard::create_dir_all(&dir)?;
        let text = format!("{n}\n");
        match crate::fsguard::kind(p.path())? {
            crate::fsguard::Kind::Missing => crate::fsguard::create_file(p, text.as_bytes()),
            _ => crate::fsguard::replace_own_file(p, text.as_bytes()),
        }
    }

    fn clear(&self) {
        if let Some(p) = &self.place {
            if !matches!(crate::fsguard::kind(p.path()), Ok(crate::fsguard::Kind::Missing)) {
                let _ = crate::fsguard::delete_file(p);
            }
        }
    }
}

// ---------------------------------------------------------------- session and turn
//
// `session` and `turn` (the Lords owner, 31 Oct): a running session keeps the Lords rules it started with, so rule
// changes never reached long-running sessions; and agents guessed clock times. `session`, a `SessionStart` hook,
// records the hash of each Lords-written file the session reads. `turn`, a `UserPromptSubmit` hook, gives the agent
// the clock in one line and, only when one of those files changed since, names it, once. A few small reads per
// prompt: no transcript, no network, no T3 (design note: Harness/notes/2026-10-31-lords-turn-hook.md).

/// Days after which a session's record is stale: removed at any session's next start. A later prompt of that session
/// finds no record and is told once, as a session that started before the guard was installed.
pub const SESSION_RECORD_DAYS: u64 = 7;

/// Where the session and turn guards look: the tool home (their records) and the user's Claude config dir, whose
/// `CLAUDE.md` includes the user-level Lords files (`CLAUDE_CONFIG_DIR`, else `~/.claude`).
pub struct RulesEnv {
    pub home: PathBuf,
    pub user_config: Option<PathBuf>,
}

/// The clock line: the local time in the system's zone, labelled by the zone's city (`Berlin 07:41 Sat 31 Oct`), by
/// its offset when it has no IANA name, and as UTC, said so, when the system reports no zone. Never an assumed zone.
pub fn clock_line(now: jiff::Timestamp, zone: Option<&jiff::tz::TimeZone>) -> String {
    let Some(tz) = zone else {
        let utc = now.to_zoned(jiff::tz::TimeZone::UTC);
        return format!("UTC {} (local zone unknown)", utc.strftime("%H:%M %a %-d %b"));
    };
    let local = now.to_zoned(tz.clone());
    let label = match tz.iana_name() {
        Some(name) => name.rsplit('/').next().unwrap_or(name).replace('_', " "),
        None => {
            let s = local.offset().seconds();
            let sign = if s < 0 { '-' } else { '+' };
            format!("UTC{sign}{:02}:{:02}", s.abs() / 3600, s.abs() % 3600 / 60)
        }
    };
    format!("{label} {}", local.strftime("%H:%M %a %-d %b"))
}

/// The system's clock and zone, for the turn guard.
pub fn system_clock() -> (jiff::Timestamp, Option<jiff::tz::TimeZone>) {
    (jiff::Timestamp::now(), jiff::tz::TimeZone::try_system().ok())
}

/// How a session came to read a file: Claude Code loads it itself, again after a compaction and on resume (the place
/// `CLAUDE.local.md` files, the user `CLAUDE.md`'s includes), or the agent read it on its own (a role file; Codex's
/// `AGENTS.override.md`).
#[derive(Clone, Copy, Debug, PartialEq)]
enum Load {
    Claude,
    Agent,
}

/// Each Lords-written file a session in `cwd` reads, by path, with how it was read and its hash: the
/// `CLAUDE.local.md`, `AGENTS.override.md` and `.lords/role-*.md` of the cwd and every dir above it, and the includes
/// of the user `CLAUDE.md` that lie in the tool home. A file counts only with Lords' marker in it; one that can't be
/// read counts as not there.
fn rules_files(cwd: &Path, env: &RulesEnv) -> BTreeMap<String, (Load, String)> {
    let mut out = BTreeMap::new();
    let mut add = |p: &Path, load: Load| {
        if let Ok(b) = crate::fsguard::read_bytes(p) {
            let marker = crate::compose::BEGIN.as_bytes();
            if b.windows(marker.len()).any(|w| w == marker) {
                out.insert(crate::adapters::slashes(p), (load, crate::fsguard::sha(&b)));
            }
        }
    };
    for d in cwd.ancestors() {
        add(&d.join("CLAUDE.local.md"), Load::Claude);
        add(&d.join("AGENTS.override.md"), Load::Agent);
        for p in crate::fsguard::list_dir(&d.join(crate::sync::ROLE_DIR)).unwrap_or_default() {
            let name = p.file_name().and_then(|n| n.to_str()).unwrap_or("");
            if name.starts_with("role-") && name.ends_with(".md") {
                add(&p, Load::Agent);
            }
        }
    }
    let home = crate::adapters::slashes(&env.home).to_lowercase() + "/";
    let user = env.user_config.as_ref().map(|c| c.join("CLAUDE.md"));
    let text = user.and_then(|u| crate::fsguard::read_string(u).ok()).unwrap_or_default();
    for line in text.lines() {
        let Some(include) = line.trim().strip_prefix('@') else { continue };
        let p = crate::config::expand_home(include.trim());
        if crate::adapters::slashes(&p).to_lowercase().starts_with(&home) {
            add(&p, Load::Claude);
        }
    }
    out
}

/// What a session read at its start: its cwd and its files (`rules_files`).
struct Record {
    cwd: PathBuf,
    files: BTreeMap<String, (Load, String)>,
}

const RECORD_HEADER: &str = "lords session record 1";

/// A session's record: `<tool home>/guard/session/<session id>`.
fn record_place(home: &Path, session: &str) -> Result<Contained, String> {
    Ok(Root::home(home)?.join(&[Part::new("guard")?, Part::new("session")?, Part::new(session)?]))
}

/// The record, or `None` when there is none (or it isn't one: it is then made again).
fn read_record(place: &Contained) -> Result<Option<Record>, String> {
    let Some(text) = crate::fsguard::read_text(place.path())? else { return Ok(None) };
    let mut lines = text.lines();
    let (Some(RECORD_HEADER), Some(cwd)) =
        (lines.next(), lines.next().and_then(|l| l.strip_prefix("cwd\t")))
    else {
        return Ok(None);
    };
    let mut files = BTreeMap::new();
    for l in lines {
        let mut f = l.splitn(3, '\t');
        let load = match f.next() {
            Some("claude") => Load::Claude,
            Some("agent") => Load::Agent,
            _ => return Ok(None),
        };
        let (Some(hash), Some(path)) = (f.next(), f.next()) else { return Ok(None) };
        files.insert(path.to_string(), (load, hash.to_string()));
    }
    Ok(Some(Record { cwd: PathBuf::from(cwd), files }))
}

fn write_record(
    place: &Contained,
    cwd: &Path,
    files: &BTreeMap<String, (Load, String)>,
) -> Result<(), String> {
    let mut text = format!("{RECORD_HEADER}\ncwd\t{}\n", cwd.display());
    for (path, (load, hash)) in files {
        let load = if *load == Load::Claude { "claude" } else { "agent" };
        text.push_str(&format!("{load}\t{hash}\t{path}\n"));
    }
    crate::fsguard::create_dir_all(&place.root().join(&place.parts()[..place.parts().len() - 1]))?;
    match crate::fsguard::kind(place.path())? {
        crate::fsguard::Kind::Missing => crate::fsguard::create_file(place, text.as_bytes()),
        _ => crate::fsguard::replace_own_file(place, text.as_bytes()),
    }
}

/// Removes every session record older than `SESSION_RECORD_DAYS`. Best effort: a record left is only a small file.
fn sweep_records(home: &Path) {
    let Ok(dir) =
        Root::home(home).and_then(|r| Ok(r.join(&[Part::new("guard")?, Part::new("session")?])))
    else {
        return;
    };
    let max = Duration::from_secs(SESSION_RECORD_DAYS * 24 * 3600);
    for p in crate::fsguard::list_dir(dir.path()).unwrap_or_default() {
        let stale = crate::fsguard::modified(&p)
            .ok()
            .and_then(|t| t.elapsed().ok())
            .is_some_and(|age| age > max);
        let name = p.file_name().and_then(|n| n.to_str()).and_then(|n| Part::new(n).ok());
        if let (true, Some(name)) = (stale, name) {
            let _ = crate::fsguard::delete_file(&dir.child(name));
        }
    }
}

fn quiet() -> Answer {
    Answer { stdout: String::new(), note: None }
}

fn noted(why: impl Into<String>) -> Answer {
    Answer { stdout: String::new(), note: Some(why.into()) }
}

/// The event's session id and cwd.
fn session_of(ev: &Value) -> Result<(&str, &Path), String> {
    let session = ev["session_id"].as_str().ok_or("the event names no session")?;
    let cwd = ev["cwd"].as_str().ok_or("the event names no cwd")?;
    Ok((session, Path::new(cwd)))
}

/// The session guard's answer to one `SessionStart` event: always nothing on stdout. It records the files the
/// session reads (`rules_files`). On `compact` and `resume` Claude reloads its own files, so they are recorded anew,
/// while the files the agent read itself keep the hash recorded before: a change to one of them is still named. Then
/// stale records go (`sweep_records`). Any failure is a note; the session goes on.
pub fn session_hook(input: &str, env: &RulesEnv) -> Answer {
    let Ok(ev) = serde_json::from_str::<Value>(input) else {
        return noted("the event can't be read");
    };
    if ev["hook_event_name"] != "SessionStart" {
        return quiet();
    }
    let recorded = session_of(&ev).and_then(|(session, cwd)| {
        let place = record_place(&env.home, session)?;
        let mut files = rules_files(cwd, env);
        if matches!(ev["source"].as_str(), Some("compact" | "resume")) {
            if let Some(before) = read_record(&place)? {
                files.retain(|_, (load, _)| *load == Load::Claude);
                files
                    .extend(before.files.into_iter().filter(|(_, (load, _))| *load == Load::Agent));
            }
        }
        write_record(&place, cwd, &files)
    });
    sweep_records(&env.home);
    match recorded {
        Ok(()) => quiet(),
        Err(e) => noted(format!("the session's files aren't recorded ({e})")),
    }
}

/// The turn guard's answer to one `UserPromptSubmit` event: context of the clock line and, only when a file the
/// session read changed since it was recorded, a line naming each such file, once (the record then holds it as it
/// is). A session with no record (it started before the guard) is told once that the rules may have changed, with
/// every file it reads. Never a block: a failure in the rules part leaves the clock line, with a note.
pub fn turn_hook(
    input: &str,
    env: &RulesEnv,
    now: jiff::Timestamp,
    zone: Option<&jiff::tz::TimeZone>,
) -> Answer {
    let Ok(ev) = serde_json::from_str::<Value>(input) else {
        return noted("the event can't be read");
    };
    if ev["hook_event_name"] != "UserPromptSubmit" {
        return quiet();
    }
    let mut lines = vec![clock_line(now, zone)];
    let mut note = None;
    match rules_line(&ev, env) {
        Ok(Some(line)) => lines.push(line),
        Ok(None) => {}
        Err(e) => note = Some(format!("the rules aren't checked ({e})")),
    }
    let out = json!({"hookSpecificOutput": {
        "hookEventName": "UserPromptSubmit",
        "additionalContext": lines.join("\n"),
    }});
    Answer { stdout: out.to_string(), note }
}

/// The rules line for one prompt, if any (`turn_hook`).
fn rules_line(ev: &Value, env: &RulesEnv) -> Result<Option<String>, String> {
    let (session, cwd) = session_of(ev)?;
    let place = record_place(&env.home, session)?;
    let Some(before) = read_record(&place)? else {
        let files = rules_files(cwd, env);
        write_record(&place, cwd, &files)?;
        let paths: Vec<&str> = files.keys().map(String::as_str).collect();
        return Ok((!paths.is_empty()).then(|| {
            format!(
                "Lords rules may have changed since this session started: re-read {}",
                paths.join(", ")
            )
        }));
    };
    let now = rules_files(&before.cwd, env);
    let mut changed: Vec<String> = now
        .iter()
        .filter(|(path, (_, hash))| before.files.get(*path).is_none_or(|(_, h)| h != hash))
        .map(|(path, _)| path.clone())
        .collect();
    changed.extend(
        before.files.keys().filter(|p| !now.contains_key(*p)).map(|p| format!("{p} (removed)")),
    );
    if changed.is_empty() {
        return Ok(None);
    }
    write_record(&place, &before.cwd, &now)?;
    Ok(Some(format!(
        "Lords rules changed since this session started: re-read {}",
        changed.join(", ")
    )))
}
