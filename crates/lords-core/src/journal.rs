//! The delivery journal (v2 design §2.3): append-only JSON lines under the registry home, written under a lock,
//! with no entry text and no secrets. Each line is one event with its version, time and kind.
//!
//! A crash during an append can leave the last line cut off (Sol review r1, F4). Reads skip such an unterminated last
//! line; the next append ends it and records a `journal_gap` event naming it, so the cut stays visible and the
//! completed records stay usable. Any other line that isn't an event is corruption, and reading fails.

use crate::fsguard::{self, Part, Root};
use serde_json::{json, Map, Value};
use std::path::Path;

/// The version every event this build writes carries.
pub const VERSION: u64 = 1;
const FILE: &str = "journal.jsonl";
const LOCK: &str = "journal.lock";
/// The event that names a cut line before it.
pub const GAP: &str = "journal_gap";

fn line(event: &str, fields: Value) -> Result<String, String> {
    let mut line = Map::new();
    line.insert("v".into(), json!(VERSION));
    line.insert("at".into(), json!(crate::ops::now()));
    line.insert("event".into(), json!(event));
    match fields {
        Value::Object(m) => line.extend(m),
        Value::Null => {}
        other => return Err(format!("a journal event's fields are an object, not {other}")),
    }
    serde_json::to_string(&Value::Object(line)).map_err(|e| e.to_string())
}

/// The journal's bytes, none when there is none yet.
fn bytes(home: &Path) -> Result<Vec<u8>, String> {
    let path = home.join(FILE);
    match fsguard::read_bytes(&path) {
        Ok(b) => Ok(b),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(e) => Err(format!("{}: {e}", path.display())),
    }
}

/// Appends one event (`fields` plus `v`, `at` and `event`) under the journal's lock, after ending and naming a cut
/// last line if a crash left one.
pub fn append(home: &Path, event: &str, fields: Value) -> Result<(), String> {
    let root = Root::home(home)?;
    root.create()?;
    let text = line(event, fields)?;
    let _held = fsguard::lock(&root.join(&[Part::new(LOCK)?]))?;
    let file = root.join(&[Part::new(FILE)?]);
    let old = bytes(home)?;
    if !old.is_empty() && !old.ends_with(b"\n") {
        let cut_line = old.iter().filter(|b| **b == b'\n').count() + 1;
        let start = old.iter().rposition(|b| *b == b'\n').map_or(0, |i| i + 1);
        let gap = line(GAP, json!({ "line": cut_line, "bytes": old.len() - start }))?;
        fsguard::append_or_create_line(&file, &format!("\n{gap}"))?;
    }
    fsguard::append_or_create_line(&file, &text)
}

/// Every event, in the order written; none when there is no journal yet. Read under the lock, so no append is
/// half done. A cut line is skipped when it is the unterminated last one or a `journal_gap` right after it names it;
/// any other line that isn't an event is an error.
pub fn read(home: &Path) -> Result<Vec<Value>, String> {
    let path = home.join(FILE);
    if fsguard::kind_here(home)? == fsguard::Kind::Missing {
        return Ok(Vec::new());
    }
    let root = Root::home(home)?;
    let _held = fsguard::lock(&root.join(&[Part::new(LOCK)?]))?;
    let raw = bytes(home)?;
    let text = String::from_utf8_lossy(&raw);
    let terminated = text.ends_with('\n');
    let lines: Vec<&str> = text.lines().collect();
    let event = |l: &str| {
        serde_json::from_str::<Value>(l)
            .ok()
            .filter(|v| v.get("event").is_some_and(Value::is_string))
    };
    let mut out = Vec::new();
    for (n, l) in lines.iter().enumerate() {
        if l.trim().is_empty() {
            continue;
        }
        if let Some(v) = event(l) {
            out.push(v);
            continue;
        }
        let last = n + 1 == lines.len();
        let named = lines
            .get(n + 1)
            .and_then(|next| event(next))
            .is_some_and(|g| g["event"] == GAP && g["line"] == json!(n + 1));
        if !(named || (last && !terminated)) {
            return Err(format!("{}:{}: not a journal event", path.display(), n + 1));
        }
    }
    Ok(out)
}
