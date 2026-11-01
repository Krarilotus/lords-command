//! Test support only: `codex app-server` as `lords paths` asks it (tests/role.rs), answering JSON-RPC on stdin as Codex
//! 0.166 does (probe H4, design note 2026-11-02-lords-role-at-launch): `initialize`, then `hooks/list`, whose answer
//! lists every command hook in `$CODEX_HOME/hooks.json` with the trust status `LORDS_STUB_TRUST` names. With `hang`, it
//! never answers `hooks/list`. It ends at the end of its input, as no real server does before it has answered.

// Test support is outside the filesystem boundary (fsguard.rs), as the tests are.
#![allow(clippy::disallowed_methods, clippy::disallowed_types)]

use std::io::{BufRead, Write};

fn main() {
    if std::env::args().nth(1).as_deref() != Some("app-server") {
        std::process::exit(2);
    }
    let trust = std::env::var("LORDS_STUB_TRUST").unwrap_or_else(|_| "untrusted".into());
    let home = std::path::PathBuf::from(std::env::var("CODEX_HOME").unwrap_or_default());
    let mut out = std::io::stdout();
    for line in std::io::stdin().lock().lines() {
        let Ok(line) = line else { break };
        let Ok(req) = serde_json::from_str::<serde_json::Value>(&line) else { continue };
        let answer = match req["method"].as_str() {
            Some("initialize") => {
                serde_json::json!({"id": req["id"], "result": {"userAgent": "stub"}})
            }
            Some("hooks/list") if trust == "hang" => continue,
            Some("hooks/list") => {
                let hooks: serde_json::Value = std::fs::read_to_string(home.join("hooks.json"))
                    .ok()
                    .and_then(|t| serde_json::from_str(&t).ok())
                    .unwrap_or_default();
                let mut listed = Vec::new();
                for groups in hooks["hooks"].as_object().into_iter().flat_map(|o| o.values()) {
                    for g in groups.as_array().into_iter().flatten() {
                        for h in g["hooks"].as_array().into_iter().flatten() {
                            listed.push(serde_json::json!({"command": h["command"], "handlerType": "command",
                                "currentHash": "sha256:0", "trustStatus": trust}));
                        }
                    }
                }
                serde_json::json!({"id": req["id"], "result": {"data": [{"cwd": ".", "hooks": listed}]}})
            }
            _ => continue,
        };
        let _ = writeln!(out, "{answer}");
        let _ = out.flush();
    }
}
