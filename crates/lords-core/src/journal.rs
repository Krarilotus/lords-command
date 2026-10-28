//! The delivery journal (v2 design §2.3): append-only JSON lines under the registry home, written under a lock,
//! with no entry text and no secrets. Each line is one event with its version, time and kind.
//!
//! A record is complete when its newline is written; every complete record must be UTF-8 JSON naming its event, or
//! reading fails. A crash during an append can leave a last record cut off (Sol review r1, F4): reads skip it, and the
//! next append recovers it before appending: it publishes, in one step (`fsguard::publish`), the complete records
//! followed by a `journal_gap` event that keeps the cut bytes. Interrupted at any point, recovery leaves either the
//! journal as it was, cut tail and all, to be recovered again, or the recovered one (Sol fix check 1, F4a); it never
//! leaves a broken record among the complete ones.

use crate::fsguard::{self, Part, Root};
use serde_json::{json, Map, Value};
use std::path::Path;

/// The version every event this build writes carries.
pub const VERSION: u64 = 1;
const FILE: &str = "journal.jsonl";
const LOCK: &str = "journal.lock";
/// The event that records a cut record and keeps its bytes.
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

/// The complete records (each with its newline) and the cut tail after them, if any.
fn split(raw: &[u8]) -> (&[u8], &[u8]) {
    let end = raw.iter().rposition(|b| *b == b'\n').map_or(0, |i| i + 1);
    raw.split_at(end)
}

/// Appends one event (`fields` plus `v`, `at` and `event`) under the journal's lock, after recovering a cut last
/// record if a crash left one.
pub fn append(home: &Path, event: &str, fields: Value) -> Result<(), String> {
    let root = Root::home(home)?;
    root.create()?;
    let text = line(event, fields)?;
    let _held = fsguard::lock(&root.join(&[Part::new(LOCK)?]))?;
    let file = root.join(&[Part::new(FILE)?]);
    if fsguard::last_byte(file.path())?.is_some_and(|b| b != b'\n') {
        let raw = bytes(home)?;
        let (complete, cut) = split(&raw);
        let hex: String = cut.iter().map(|b| format!("{b:02x}")).collect();
        let records = complete.iter().filter(|b| **b == b'\n').count();
        let gap = line(GAP, json!({ "line": records + 1, "bytes": cut.len(), "cut_hex": hex }))?;
        let mut recovered = complete.to_vec();
        recovered.extend_from_slice(gap.as_bytes());
        recovered.push(b'\n');
        fsguard::publish(&file, &recovered)?;
    }
    fsguard::append_or_create_line(&file, &text)
}

/// Every event, in the order written; none when there is no journal yet. Read under the lock, so no append or
/// recovery is half done. A cut last record is skipped (the next append recovers it); any complete record that isn't
/// UTF-8 JSON naming its event is an error.
pub fn read(home: &Path) -> Result<Vec<Value>, String> {
    let path = home.join(FILE);
    if fsguard::kind_here(home)? == fsguard::Kind::Missing {
        return Ok(Vec::new());
    }
    let root = Root::home(home)?;
    let _held = fsguard::lock(&root.join(&[Part::new(LOCK)?]))?;
    let raw = bytes(home)?;
    let (complete, _cut) = split(&raw);
    let mut out = Vec::new();
    let Some(records) = complete.strip_suffix(b"\n") else { return Ok(out) };
    for (n, record) in records.split(|b| *b == b'\n').enumerate() {
        let event = std::str::from_utf8(record)
            .ok()
            .and_then(|s| serde_json::from_str::<Value>(s).ok())
            .filter(|v| v.get("event").is_some_and(Value::is_string))
            .ok_or_else(|| format!("{}:{}: not a journal event", path.display(), n + 1))?;
        out.push(event);
    }
    Ok(out)
}
