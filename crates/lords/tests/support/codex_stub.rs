//! Test support only: `codex app-server` as Lords asks it (`lords paths`, tests/role.rs; sync's trust,
//! tests/codex_trust.rs), answering JSON-RPC on stdin as Codex 0.166 does (probe H4, design note
//! 2026-11-02-lords-role-at-launch): `initialize`; `hooks/list`, which lists every command hook in `$CODEX_HOME/hooks.json`
//! and in each asked cwd's `.codex/hooks.json` with its key (`<file>:<event>:<group>:<handler>`: Codex keys trust by
//! position), event, command, hash, enabled and trust; and `config/batchWrite` of `hooks.state`, kept as JSON in
//! `$CODEX_HOME/config.toml` (Lords never reads it). Every request is logged to `$CODEX_HOME/stub-calls.jsonl`.
//! `LORDS_STUB_TRUST` forces every hook's trust (`hang`: never answers `hooks/list`); `LORDS_STUB_DISABLED` switches one
//! event's hooks off; `LORDS_STUB_MODE`: `exit` ends at once, `nolist` and `writeerr` answer an error, `malformed` a list
//! that isn't one, `empty` lists no hook (Codex's hooks switched off), `race` edits the hooks file once after a list
//! (another writer); `LORDS_STUB_ADD` names a JSON object
//! of hook entries per event that another writer appends to the hooks file once, before a list. Run as `trust-all` it
//! trusts every hook listed, as the owner would under `/hooks`; as `list`, it prints the list. It ends at the end of
//! its input, as no real server does before it has answered.

// Test support is outside the filesystem boundary (fsguard.rs), as the tests are.
#![allow(clippy::disallowed_methods, clippy::disallowed_types)]

use serde_json::{json, Value};
use std::io::{BufRead, Write};
use std::path::Path;

fn read(path: &Path) -> Value {
    let text = std::fs::read_to_string(path).unwrap_or_default();
    serde_json::from_str(&text).unwrap_or_default()
}

/// The hook as written, its group's matcher with it: a stand-in for Codex's sha256.
fn hash(matcher: &Value, hook: &Value) -> String {
    let text = format!("{matcher}{hook}");
    let fnv = |h: u64, b: u8| (h ^ b as u64).wrapping_mul(0x100_0000_01b3);
    format!("sha256:{:016x}", text.bytes().fold(0xcbf2_9ce4_8422_2325, fnv))
}

fn list(home: &Path, cwds: &[&str]) -> Vec<Value> {
    let state = read(&home.join("config.toml"));
    let forced = std::env::var("LORDS_STUB_TRUST").ok();
    let off = std::env::var("LORDS_STUB_DISABLED").unwrap_or_default();
    let mut files = vec![home.join("hooks.json")];
    files.extend(cwds.iter().map(|c| Path::new(c).join(".codex").join("hooks.json")));
    let mut listed = Vec::new();
    for file in files {
        // One spelling of the file, however `CODEX_HOME` spells it (Codex's keys read `C:\…\.codex\hooks.json:…`).
        let source = std::fs::canonicalize(&file).unwrap_or(file).display().to_string();
        let source = source.trim_start_matches(r"\\?\").to_string();
        let file = Path::new(&source);
        for (event, groups) in read(file)["hooks"].as_object().into_iter().flatten() {
            // Named as Codex names it: `PreToolUse` → `preToolUse`, and `pre_tool_use` in its key.
            let mut name = event.clone();
            name[..1].make_ascii_lowercase();
            let snake: String = event
                .chars()
                .enumerate()
                .flat_map(|(i, c)| match (i, c.is_ascii_uppercase()) {
                    (0, _) | (_, false) => vec![c.to_ascii_lowercase()],
                    _ => vec!['_', c.to_ascii_lowercase()],
                })
                .collect();
            for (g, group) in groups.as_array().into_iter().flatten().enumerate() {
                for (h, hook) in group["hooks"].as_array().into_iter().flatten().enumerate() {
                    let key = format!("{source}:{snake}:{g}:{h}");
                    let hash = hash(&group["matcher"], hook);
                    let kept = &state["hooks"]["state"][&key];
                    let trust = forced.clone().unwrap_or_else(|| {
                        match kept["trusted_hash"].as_str() {
                            Some(t) if t == hash => "trusted",
                            Some(_) => "modified",
                            None => "untrusted",
                        }
                        .into()
                    });
                    let enabled = off != name && kept["enabled"] != false;
                    listed.push(json!({"key": key, "command": hook["command"],
                        "handlerType": "command", "eventName": name, "sourcePath": source,
                        "enabled": enabled, "currentHash": hash, "trustStatus": trust}));
                }
            }
        }
    }
    listed
}

/// `hooks.state` upserted, key by key, as Codex's writer does.
fn write(home: &Path, params: &Value) {
    let mut state = read(&home.join("config.toml"));
    for edit in params["edits"].as_array().into_iter().flatten() {
        if edit["keyPath"] == "hooks.state" && edit["mergeStrategy"] == "upsert" {
            for (k, v) in edit["value"].as_object().into_iter().flatten() {
                state["hooks"]["state"][k] = v.clone();
            }
        }
    }
    std::fs::write(home.join("config.toml"), state.to_string()).unwrap();
}

fn main() {
    let home = std::path::PathBuf::from(std::env::var("CODEX_HOME").unwrap_or_default());
    let mode = std::env::var("LORDS_STUB_MODE").unwrap_or_default();
    match std::env::args().nth(1).as_deref() {
        Some("app-server") => {}
        Some("trust-all") => {
            let mut value = serde_json::Map::new();
            for h in list(&home, &[]) {
                let key = h["key"].as_str().unwrap().to_string();
                value.insert(key, json!({"trusted_hash": h["currentHash"]}));
            }
            let edit = json!({"keyPath": "hooks.state", "value": value, "mergeStrategy": "upsert"});
            return write(&home, &json!({"edits": [edit]}));
        }
        Some("list") => return println!("{}", Value::from(list(&home, &[]))),
        _ => std::process::exit(2),
    }
    if mode == "exit" {
        return;
    }
    let hang = std::env::var("LORDS_STUB_TRUST").as_deref() == Ok("hang");
    let mut out = std::io::stdout();
    for line in std::io::stdin().lock().lines() {
        let Ok(line) = line else { break };
        let Ok(req) = serde_json::from_str::<Value>(&line) else { continue };
        let log = home.join("stub-calls.jsonl");
        if let Ok(mut log) = std::fs::OpenOptions::new().create(true).append(true).open(log) {
            let _ = writeln!(log, "{req}");
        }
        let failed = json!({"code": -32601, "message": "stub: refused"});
        let (result, error) = match req["method"].as_str() {
            Some("initialize") => (json!({"userAgent": "stub"}), Value::Null),
            Some("hooks/list") if hang => continue,
            Some("hooks/list") if mode == "nolist" => (Value::Null, failed),
            Some("hooks/list") if mode == "malformed" => (json!({"data": "a"}), Value::Null),
            Some("hooks/list") if mode == "empty" => (json!({"data": []}), Value::Null),
            Some("hooks/list") => {
                let added = home.join("stub-added");
                if let (Ok(add), false) = (std::env::var("LORDS_STUB_ADD"), added.exists()) {
                    let mut hooks = read(&home.join("hooks.json"));
                    for (event, groups) in read(Path::new(&add)).as_object().into_iter().flatten() {
                        let list = hooks["hooks"][event].as_array().cloned().unwrap_or_default();
                        let more = groups.as_array().into_iter().flatten().cloned();
                        hooks["hooks"][event] = list.into_iter().chain(more).collect();
                    }
                    std::fs::write(home.join("hooks.json"), hooks.to_string()).unwrap();
                    std::fs::write(&added, "").unwrap();
                }
                let cwds = req["params"]["cwds"].as_array().into_iter().flatten();
                let cwds: Vec<&str> = cwds.filter_map(Value::as_str).collect();
                let listed = list(&home, &cwds);
                // Another writer edits the hooks file once, once listed: its first PreToolUse hook changes. Before the
                // answer: Lords ends this process as soon as it has its answer.
                let raced = home.join("stub-raced");
                if mode == "race" && !raced.exists() {
                    let mut hooks = read(&home.join("hooks.json"));
                    let first = &mut hooks["hooks"]["PreToolUse"][0]["hooks"][0]["command"];
                    *first = Value::from(format!("{} --raced", first.as_str().unwrap_or_default()));
                    std::fs::write(home.join("hooks.json"), hooks.to_string()).unwrap();
                    std::fs::write(&raced, "").unwrap();
                }
                (json!({"data": [{"cwd": ".", "hooks": listed}]}), Value::Null)
            }
            Some("config/batchWrite") if mode == "writeerr" => (Value::Null, failed),
            Some("config/batchWrite") => {
                write(&home, &req["params"]);
                (json!({"status": "ok"}), Value::Null)
            }
            _ => continue,
        };
        let answer = match error {
            Value::Null => json!({"id": req["id"], "result": result}),
            e => json!({"id": req["id"], "error": e}),
        };
        let _ = writeln!(out, "{answer}");
        let _ = out.flush();
    }
}
