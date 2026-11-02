//! The time guard (`lords guard time`, a `PreToolUse` hook; design note Harness/notes/2026-11-02-lords-time-guard.md).
//! Agents wrote placeholder or invented clock times into notes and reports (system design C15: "tools stamp times,
//! rendered once in Berlin"). An editor write to a Markdown or plain-text file, or an outgoing T3 message, is judged on
//! the lines it adds: a placeholder clock time (`14:xx`, `HH:MM`) is denied (P); a real one more than 5 minutes after
//! the clock, the same day, with no future marker on its line, gets a one-line reminder (F: as a denial it would have
//! refused about 26 in 200 lines with times, mostly plans; design note §5). Code is never judged. Like every guard it
//! fails open: input it can't read is let through.

use crate::fsguard;
use jiff::tz::TimeZone;
use jiff::{Timestamp, Zoned};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// The tools the hook runs on, as an exact list (letters, digits, `_`, `-`, `|`) in both harnesses' matcher syntax:
/// Claude Code's editor tools and the T3 send under both server spellings; Codex's `apply_patch` answers to `Write` and
/// `Edit` (design note §2). No `MultiEdit`: Claude Code 2.1.316 has none.
pub const MATCHER: &str = "Write|Edit|mcp__t3-code__t3_thread_send|mcp__t3_code__t3_thread_send";

/// The words that mark a time as a plan (the Lords owner's steer, 2 Nov), compared whole and in any case.
pub const MARKERS: [&str; 9] =
    ["next", "until", "by", "eta", "planned", "due", "deadline", "expected", "wait"];

/// How far after the clock a time may be before F objects, in minutes (the steer: "more than 5 minutes later").
const AHEAD: i32 = 5;

/// Past this many minutes after the clock an undated time is the evening before, written after midnight, not later
/// today (measured: `23:50` written at `02:19`; design note §5).
const SAME_DAY: i32 = 12 * 60;

/// What a clock time in added text is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// A digit position written as a letter or `?` (P).
    Placeholder,
    /// A real time more than `AHEAD` minutes after the clock, the same day, not marked as a plan (F).
    Future,
}

/// One clock time the guard objects to, as written.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Finding {
    pub kind: Kind,
    pub token: String,
}

/// The hook's answer to one `PreToolUse` event: a denial naming the fix (P), a one-line reminder that comes back with
/// the tool's result (F), or nothing. `now` and `zone`: the clock and the system's zone (no zone: no F check, as nothing
/// says what "now" is there).
pub fn time_hook(input: &str, now: Timestamp, zone: Option<&TimeZone>) -> String {
    let Ok(ev) = serde_json::from_str::<Value>(input) else { return String::new() };
    let now = zone.map(|z| now.to_zoned(z.clone()));
    let found: Vec<Finding> =
        added(&ev).iter().flat_map(|(new, old)| findings(new, old, now.as_ref())).collect();
    if let Some(f) = found.iter().find(|f| f.kind == Kind::Placeholder) {
        let reason = format!(
            "Lords: `{}` is a placeholder for a clock time. Write the real time: take it from the turn hook's `Berlin \
             HH:MM` line, or from the clock (PowerShell: `[TimeZoneInfo]::ConvertTimeBySystemTimeZoneId([DateTime]::\
             UtcNow, 'W. Europe Standard Time').ToString('HH:mm')`). A format example goes in backticks, as code.",
            f.token
        );
        return json!({ "hookSpecificOutput": {
            "hookEventName": "PreToolUse",
            "permissionDecision": "deny",
            "permissionDecisionReason": reason,
        } })
        .to_string();
    }
    let (Some(f), Some(now)) = (found.first(), &now) else { return String::new() };
    let reminder = format!(
        "Lords: `{}` is more than {AHEAD} minutes after the time now ({}): write the real time from the clock, or mark \
         it as a plan ({}).",
        f.token,
        crate::guard::clock_line(now.timestamp(), Some(now.time_zone())),
        MARKERS.map(|m| if m == "eta" { "ETA" } else { m }).join(", ")
    );
    json!({ "hookSpecificOutput": { "hookEventName": "PreToolUse", "additionalContext": reminder } }).to_string()
}

/// The (new text, old text) pairs an event writes to a file in scope or sends: only the new text's lines that aren't
/// among the old text's are judged.
fn added(ev: &Value) -> Vec<(String, String)> {
    let tool = ev["tool_name"].as_str().unwrap_or("");
    let input = &ev["tool_input"];
    let text = |key: &str| input[key].as_str().unwrap_or("").to_string();
    match tool {
        "mcp__t3-code__t3_thread_send" | "mcp__t3_code__t3_thread_send" => {
            vec![(text("message"), String::new())]
        }
        "apply_patch" => patch_sections(input["command"].as_str().unwrap_or("")),
        "Write" | "Edit" => {
            let Some(path) = input["file_path"].as_str().map(|p| place(ev, p)) else {
                return Vec::new();
            };
            if !in_scope(&path) {
                return Vec::new();
            }
            // A file that is there but can't be read is let through: what the write keeps is unknown.
            let Ok(old) = fsguard::read_text(&path) else { return Vec::new() };
            let old = old.unwrap_or_default();
            if tool == "Write" {
                return vec![(text("content"), old)];
            }
            // The file after the edit, against the file before; where the old text isn't there (the edit will fail),
            // the edit's own new and old text.
            let (from, to) = (text("old_string"), text("new_string"));
            if from.is_empty() || !old.contains(&from) {
                return vec![(to, from)];
            }
            let new = if input["replace_all"] == true {
                old.replace(&from, &to)
            } else {
                old.replacen(&from, &to, 1)
            };
            vec![(new, old)]
        }
        _ => Vec::new(),
    }
}

/// A file path as the event names it, against the event's `cwd` when relative.
fn place(ev: &Value, p: &str) -> PathBuf {
    let path = PathBuf::from(p);
    match ev["cwd"].as_str() {
        Some(cwd) if path.is_relative() => Path::new(cwd).join(path),
        _ => path,
    }
}

/// Markdown and plain text, by extension in any case: the guard's scope, wherever the file is.
fn in_scope(path: &Path) -> bool {
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase();
    matches!(ext.as_str(), "md" | "markdown" | "txt")
}

/// Each in-scope file section of a Codex patch as (its context and `+` lines, its context and `-` lines): an added
/// file is all new, an updated one judged on what its hunks add. A `*** Move to:` names where the text goes.
fn patch_sections(patch: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut cur: Option<(PathBuf, String, String)> = None;
    let mut flush = |cur: &mut Option<(PathBuf, String, String)>| {
        if let Some((path, new, old)) = cur.take() {
            if in_scope(&path) {
                out.push((new, old));
            }
        }
    };
    for line in patch.lines() {
        let header = ["*** Add File: ", "*** Update File: ", "*** Delete File: "]
            .iter()
            .find_map(|h| line.strip_prefix(h));
        if let Some(path) = header {
            flush(&mut cur);
            cur = (!line.starts_with("*** Delete"))
                .then(|| (path.into(), String::new(), String::new()));
            continue;
        }
        let Some((path, new, old)) = cur.as_mut() else { continue };
        if let Some(to) = line.strip_prefix("*** Move to: ") {
            *path = to.into();
        } else if let Some(l) = line.strip_prefix('+') {
            new.extend([l, "\n"]);
        } else if let Some(l) = line.strip_prefix('-') {
            old.extend([l, "\n"]);
        } else if let Some(l) = line.strip_prefix(' ') {
            new.extend([l, "\n"]);
            old.extend([l, "\n"]);
        }
    }
    flush(&mut cur);
    out
}

/// The clock times the guard objects to among `new`'s lines that aren't `old`'s, outside code: fenced blocks and inline
/// code spans. `now`: the zoned clock; `None` skips F.
pub fn findings(new: &str, old: &str, now: Option<&Zoned>) -> Vec<Finding> {
    // Each old line keeps as many copies of itself as the old text has (sol-loop r1, S4).
    let mut kept: HashMap<&str, usize> = HashMap::new();
    for line in old.lines() {
        *kept.entry(line).or_default() += 1;
    }
    let prose = without_code(new);
    let mut out = Vec::new();
    for (line, plain) in new.lines().zip(prose.lines()) {
        match kept.get_mut(line) {
            Some(n) if *n > 0 => *n -= 1,
            _ => line_findings(plain, now, &mut out),
        }
    }
    out
}

/// A fence line's character and length: up to three spaces, then three or more backticks or tildes; a backtick fence's
/// info string holds no backtick (CommonMark 4.5), so `` ```x``` `` is a code span (sol-loop r1, S3).
fn fence_mark(line: &str) -> Option<(u8, usize)> {
    let t = line.trim_start_matches(' ');
    if line.len() - t.len() > 3 {
        return None;
    }
    let c = *t.as_bytes().first().filter(|c| **c == b'`' || **c == b'~')?;
    let n = t.bytes().take_while(|b| *b == c).count();
    (n >= 3 && !(c == b'`' && t[n..].contains('`'))).then_some((c, n))
}

/// `text` with its code blanked to spaces, line ends kept: fenced blocks with their fence lines, and inline code spans
/// (a run of backticks to the next run of the same length within a paragraph, so across a wrapped line; sol-loop r1,
/// S2). A run with no match stays as text.
fn without_code(text: &str) -> String {
    let mut b = text.as_bytes().to_vec();
    let blank = |b: &mut [u8]| b.iter_mut().filter(|c| **c != b'\n').for_each(|c| *c = b' ');
    let (mut fence, mut para, mut at): (Option<(u8, usize)>, Option<usize>, usize) =
        (None, None, 0);
    for line in text.split_inclusive('\n') {
        let (start, body) = (at, line.trim_end_matches(['\n', '\r']));
        at += line.len();
        let mark = fence_mark(body);
        let code = match (fence, mark) {
            (Some((fc, fnn)), Some((c, n)))
                if fc == c && n >= fnn && body.trim()[n..].is_empty() =>
            {
                fence = None;
                true
            }
            (Some(_), _) => true,
            (None, Some(m)) => {
                fence = Some(m);
                true
            }
            (None, None) => false,
        };
        if code || body.trim().is_empty() {
            if let Some(s) = para.take() {
                blank_spans(&mut b[s..start], blank);
            }
            if code {
                blank(&mut b[start..start + body.len()]);
            }
        } else {
            para.get_or_insert(start);
        }
    }
    if let Some(s) = para {
        blank_spans(&mut b[s..], blank);
    }
    // Only whole characters were blanked, each byte to an ASCII space.
    String::from_utf8(b).unwrap_or_default()
}

/// Blanks each code span in `b`, one paragraph.
fn blank_spans(b: &mut [u8], blank: impl Fn(&mut [u8])) {
    let mut i = 0;
    while i < b.len() {
        if b[i] != b'`' {
            i += 1;
            continue;
        }
        let n = b[i..].iter().take_while(|c| **c == b'`').count();
        let mut j = i + n;
        let close = loop {
            let Some(k) = b[j..].iter().position(|c| *c == b'`') else { break None };
            let m = b[j + k..].iter().take_while(|c| **c == b'`').count();
            if m == n {
                break Some(j + k + m);
            }
            j += k + m;
        };
        match close {
            Some(end) => {
                blank(&mut b[i..end]);
                i = end;
            }
            None => i += n,
        }
    }
}

/// A clock token at byte `i` of `b`: its end and its hour, minute and seconds parts. Not after or before a letter,
/// digit, `_` or `:`; an hour of two characters from `0-9HhXx?` or one from `0-9Hh`, a minute of two from `0-9MmXx?`,
/// optional seconds of two from `0-9SsXx?`.
fn clock_at(b: &[u8], i: usize) -> Option<(usize, &[u8], &[u8])> {
    let word = |c: u8| c.is_ascii_alphanumeric() || c == b'_' || c == b':';
    if i > 0 && word(b[i - 1]) {
        return None;
    }
    let of = |set: &[u8], at: usize, n: usize| {
        b.get(at..at + n).filter(|s| s.iter().all(|c| c.is_ascii_digit() || set.contains(c)))
    };
    let hour =
        of(b"HhXx?", i, 2).filter(|_| b.get(i + 2) == Some(&b':')).or_else(|| of(b"Hh", i, 1))?;
    let mut end = i + hour.len();
    if b.get(end) != Some(&b':') {
        return None;
    }
    let minute = of(b"MmXx?", end + 1, 2)?;
    // A one-letter hour is a time only with a letter minute (`H:MM`); `H:25` cites line 25 of a source H.
    if hour.len() == 1 && !hour[0].is_ascii_digit() && minute.iter().any(u8::is_ascii_digit) {
        return None;
    }
    // Digits a clock can't show there make no clock: `xx:80` is a port (sol-loop r1, S1).
    let tens = |s: &[u8], max: u8| s.len() == 2 && s[0].is_ascii_digit() && s[0] > max;
    if tens(hour, b'2') || tens(minute, b'5') {
        return None;
    }
    end += 3;
    if b.get(end) == Some(&b':') {
        of(b"SsXx?", end + 1, 2)?;
        end += 3;
    }
    if b.get(end).is_some_and(|c| word(*c)) {
        return None;
    }
    Some((end, hour, minute))
}

/// P and F on one line of prose (code already blanked).
fn line_findings(line: &str, now: Option<&Zoned>, out: &mut Vec<Finding>) {
    let b = line.as_bytes();
    let lower = line.to_ascii_lowercase();
    let words: Vec<&str> = lower.split(|c: char| !c.is_ascii_alphanumeric()).collect();
    let plan = words.iter().any(|w| MARKERS.contains(w));
    let other_day = now.is_some_and(|n| names_another_day(&lower, &words, n));
    let mut i = 0;
    while i < b.len() {
        let Some((end, hour, minute)) = clock_at(b, i) else {
            i += 1;
            continue;
        };
        let token = line[i..end].to_string();
        i = end;
        if token.bytes().any(|c| c != b':' && !c.is_ascii_digit()) {
            out.push(Finding { kind: Kind::Placeholder, token });
            continue;
        }
        let digits = |s: &[u8]| std::str::from_utf8(s).ok()?.parse::<i32>().ok();
        let (Some(h), Some(m), Some(now)) = (digits(hour), digits(minute), now) else { continue };
        if plan || other_day || h > 23 || m > 59 || unit_or_zone_follows(&lower[end..]) {
            continue;
        }
        let ahead = h * 60 + m - (i32::from(now.hour()) * 60 + i32::from(now.minute()));
        if ahead > AHEAD && ahead <= SAME_DAY {
            out.push(Finding { kind: Kind::Future, token });
        }
    }
}

/// Whether a duration unit or a zone other than local follows a time: `1:30 h`, `16:00 UTC`.
fn unit_or_zone_follows(rest: &str) -> bool {
    let word = rest.trim_start().split(|c: char| !c.is_ascii_alphabetic()).next().unwrap_or("");
    let units = ["h", "hr", "hrs", "hour", "hours", "min", "mins", "minute", "minutes"];
    units.contains(&word) || ["utc", "gmt"].contains(&word)
}

const MONTHS: [&str; 12] =
    ["jan", "feb", "mar", "apr", "may", "jun", "jul", "aug", "sep", "oct", "nov", "dec"];

/// A month's number from its English name or three-letter (or `sept`) abbreviation.
fn month(w: &str) -> Option<i8> {
    let i = MONTHS.iter().position(|m| w.starts_with(m))?;
    let full = [
        "january",
        "february",
        "march",
        "april",
        "may",
        "june",
        "july",
        "august",
        "september",
        "october",
        "november",
        "december",
    ];
    (w.len() == 3 || w == "sept" || w == full[i]).then_some(i as i8 + 1)
}

/// Whether a line names a day other than `now`'s: an ISO date, a day and month either way round, `yesterday` or
/// `tomorrow`. A month followed by a time (`Nov 14:00`) names no day.
fn names_another_day(lower: &str, words: &[&str], now: &Zoned) -> bool {
    let today = (now.month(), now.day());
    if words.iter().any(|w| *w == "yesterday" || *w == "tomorrow") {
        return true;
    }
    let b = lower.as_bytes();
    let mut iso = (0..b.len().saturating_sub(9)).filter_map(|i| {
        let s = &b[i..i + 10];
        let shape = s.iter().enumerate().all(|(k, c)| {
            if k == 4 || k == 7 {
                *c == b'-'
            } else {
                c.is_ascii_digit()
            }
        });
        let n = |r: std::ops::Range<usize>| std::str::from_utf8(&s[r]).ok()?.parse::<i16>().ok();
        shape.then(|| Some((n(0..4)?, n(5..7)?, n(8..10)?))).flatten()
    });
    // An ISO date names its year too (sol-loop r1, S5).
    let date = (now.year(), i16::from(now.month()), i16::from(now.day()));
    if iso.any(|d| d != date) {
        return true;
    }
    let day = |w: &str| w.parse::<i8>().ok().filter(|d| (1..=31).contains(d));
    let mut spoken =
        words.windows(2).filter(|w| !w[0].is_empty() && !w[1].is_empty()).filter_map(|w| {
            match (day(w[0]), month(w[0]), day(w[1]), month(w[1])) {
                (Some(d), _, _, Some(m)) => Some((m, d)),
                (_, Some(m), Some(d), _) => {
                    // `Nov 14:00` is a month and a time: the number is followed by `:`.
                    let at =
                        lower.find(&format!("{} {}", w[0], w[1]))? + w[0].len() + 1 + w[1].len();
                    (lower.as_bytes().get(at) != Some(&b':')).then_some((m, d))
                }
                _ => None,
            }
        });
    spoken.any(|d| d != today)
}
