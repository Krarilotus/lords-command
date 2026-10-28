//! The delivery journal (v2 design §2.3): append-only JSON lines under the registry home, written under a lock,
//! with no entry text and no secrets. Each line is one event with its version, time and kind; readers skip
//! nothing silently: a line that isn't an event is an error.

use crate::fsguard::{self, Part, Root};
use serde_json::{json, Map, Value};
use std::path::Path;

/// The version every event this build writes carries.
pub const VERSION: u64 = 1;
const FILE: &str = "journal.jsonl";
const LOCK: &str = "journal.lock";

/// Appends one event (`fields` plus `v`, `at` and `event`) under the journal's lock.
pub fn append(home: &Path, event: &str, fields: Value) -> Result<(), String> {
    let root = Root::home(home)?;
    root.create()?;
    let mut line = Map::new();
    line.insert("v".into(), json!(VERSION));
    line.insert("at".into(), json!(crate::ops::now()));
    line.insert("event".into(), json!(event));
    match fields {
        Value::Object(m) => line.extend(m),
        Value::Null => {}
        other => return Err(format!("a journal event's fields are an object, not {other}")),
    }
    let text = serde_json::to_string(&Value::Object(line)).map_err(|e| e.to_string())?;
    let _held = fsguard::lock(&root.join(&[Part::new(LOCK)?]))?;
    fsguard::append_or_create_line(&root.join(&[Part::new(FILE)?]), &text)
}

/// Every event, in the order written; none when there is no journal yet.
pub fn read(home: &Path) -> Result<Vec<Value>, String> {
    let path = home.join(FILE);
    let text = match fsguard::read_string(&path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(format!("{}: {e}", path.display())),
    };
    text.lines()
        .enumerate()
        .filter(|(_, l)| !l.trim().is_empty())
        .map(|(n, l)| {
            serde_json::from_str::<Value>(l)
                .ok()
                .filter(|v| v.get("event").is_some_and(Value::is_string))
                .ok_or_else(|| format!("{}:{}: not a journal event", path.display(), n + 1))
        })
        .collect()
}
