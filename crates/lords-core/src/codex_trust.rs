//! Codex's hook trust (design note Harness/notes/2026-11-02-lords-codex-trust.md). Codex runs a hook only once it is
//! trusted, and keys that trust by the hook's place in its file (`<file>:pre_tool_use:0:0`), bound to the hook's hash.
//! After sync writes Codex's hooks file, it asks Codex's app-server (`process::codex_ask`, with its deadline) what that
//! file holds, trusts the hooks that are Lords' own guard hooks (I2) through Codex's own writer (`config/batchWrite` of
//! `hooks.state`, as Codex's `/hooks` screen does), and reads trust back, so each hook of that file is trusted or named
//! (I1). Lords never edits Codex's config itself, never computes a hash and never makes a key.

use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// How long Codex's app-server may take to answer one question (0.8 s measured on this PC; probe H4).
pub const DEADLINE: Duration = Duration::from_secs(20);

/// The Codex sync asks, or why it asks none: then no trust is read or written, and no hook is called trusted (I4).
#[derive(Clone, Debug)]
pub enum Codex {
    Ask { exe: PathBuf, deadline: Duration },
    None(String),
}

impl Default for Codex {
    fn default() -> Codex {
        Codex::None("no Codex was given to this sync".into())
    }
}

impl Codex {
    /// The Codex `find` finds, asked within `deadline`.
    pub fn found(deadline: Duration) -> Codex {
        match find() {
            Some(exe) => Codex::Ask { exe, deadline },
            None => Codex::None("no `codex` on PATH".into()),
        }
    }
}

/// The Codex program Lords asks: `LORDS_CODEX` where it is set (the tests' stub), else `codex` on PATH: `codex.exe`,
/// then the npm shim `codex.cmd` on Windows; `codex` elsewhere.
pub fn find() -> Option<PathBuf> {
    if let Some(exe) = std::env::var_os("LORDS_CODEX") {
        return Some(PathBuf::from(exe));
    }
    let names: &[&str] = if cfg!(windows) { &["codex.exe", "codex.cmd"] } else { &["codex"] };
    let dirs: Vec<PathBuf> = std::env::split_paths(&std::env::var_os("PATH")?).collect();
    names.iter().find_map(|n| {
        dirs.iter()
            .map(|d| d.join(n))
            .find(|p| matches!(crate::fsguard::kind(p), Ok(crate::fsguard::Kind::File)))
    })
}

/// One of Lords' guard hooks as sync renders it for a target: its guard, the event Codex lists it under
/// (`preToolUse`), and its command line.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Lords {
    pub guard: String,
    pub event: String,
    pub command: String,
}

/// A hooks-file event as Codex names it in `hooks/list` (`PreToolUse` → `preToolUse`).
pub fn event_name(event: &str) -> String {
    let mut chars = event.chars();
    chars.next().map(|c| c.to_ascii_lowercase().to_string() + chars.as_str()).unwrap_or_default()
}

/// Asks Codex one question about `home`: `initialize`, `initialized`, then `method`; its result, or why not.
fn ask(
    exe: &Path,
    wait: Duration,
    home: &Path,
    method: &str,
    params: Value,
) -> Result<Value, String> {
    let client = json!({"name": "lords", "version": env!("CARGO_PKG_VERSION")});
    let requests = [
        json!({"id": 1, "method": "initialize", "params": {"clientInfo": client}}).to_string(),
        json!({"method": "initialized"}).to_string(),
        json!({"id": 2, "method": method, "params": params}).to_string(),
    ];
    let answer = crate::process::codex_ask(exe, home, &requests, 2, wait)?;
    if let Some(e) = answer.get("error") {
        return Err(format!("it answered an error: {e}"));
    }
    answer.get("result").cloned().ok_or_else(|| "it answered no result".into())
}

/// Every hook Codex lists for `home` (its user-level hooks file among them), each with its key, file and trust; a
/// listing without them is no answer.
pub fn list(exe: &Path, deadline: Duration, home: &Path) -> Result<Vec<Value>, String> {
    let cwds = json!({"cwds": [crate::adapters::slashes(home)]});
    let result = ask(exe, deadline, home, "hooks/list", cwds)?;
    let mut hooks = Vec::new();
    for d in result["data"].as_array().ok_or("it answered no list of hooks")? {
        hooks.extend(d["hooks"].as_array().ok_or("it answered no list of hooks")?.iter().cloned());
    }
    let whole = |h: &Value| ["key", "sourcePath", "trustStatus"].iter().all(|k| h[k].is_string());
    match hooks.iter().all(whole) {
        true => Ok(hooks),
        false => Err("it listed a hook without its key, file or trust".into()),
    }
}

/// Whether a listed hook is in the hooks file `file`, compared as paths (`ledger::key`), never as text.
pub fn in_file(h: &Value, file: &Path) -> bool {
    let key = |p: &Path| crate::ledger::key(p);
    h["sourcePath"].as_str().is_some_and(|s| key(Path::new(s)) == key(file))
}

/// The Lords guard a listed hook runs (I2): its command byte for byte one Lords renders for one of the target's guards,
/// on the event Lords writes that guard under. Nothing else is Lords', whatever its text looks like.
pub fn lords_guard<'a>(h: &Value, lords: &'a [Lords]) -> Option<&'a Lords> {
    let (command, event) = (h["command"].as_str(), h["eventName"].as_str());
    lords.iter().find(|l| command == Some(l.command.as_str()) && event == Some(l.event.as_str()))
}

/// What sync says of a hook of the file it wrote that doesn't run: Lords' own, with why, or another's, never trusted.
fn named(h: &Value, lords: &[Lords]) -> String {
    let (event, status) = (h["eventName"].as_str().unwrap_or("?"), &h["trustStatus"]);
    match lords_guard(h, lords) {
        Some(l) if h["enabled"] == false => format!(
            "Codex hook not trusted: Lords' {} guard ({event}): disabled in Codex, which stays as it is",
            l.guard
        ),
        Some(l) => format!(
            "Codex hook not trusted: Lords' {} guard ({event}): {status} after sync's trust",
            l.guard
        ),
        None => format!(
            "Codex hook not trusted: {} ({event}): not Lords' guard, so sync never trusts it; trust it in Codex under /hooks if it is yours",
            h["key"].as_str().unwrap_or("?")
        ),
    }
}

/// Each hook of a hooks file's text, by event, entry and hook index: its entry's matcher and the hook.
fn slots(text: &Value) -> BTreeMap<(String, usize, usize), (Value, Value)> {
    let mut slots = BTreeMap::new();
    for (event, groups) in text["hooks"].as_object().into_iter().flatten() {
        for (g, group) in groups.as_array().into_iter().flatten().enumerate() {
            for (n, hook) in group["hooks"].as_array().into_iter().flatten().enumerate() {
                slots.insert((event.clone(), g, n), (group["matcher"].clone(), hook.clone()));
            }
        }
    }
    slots
}

/// The Lords guard a hook of a hooks file runs, as `lords_guard` judges the same hook listed.
fn lords_in_file<'a>(event: &str, hook: &Value, lords: &'a [Lords]) -> Option<&'a Lords> {
    lords_guard(&json!({"command": hook["command"], "eventName": event_name(event)}), lords)
}

/// Every hook of the hooks file sync wrote (`text`), named as not known to be trusted, with `why` (I4).
fn unknown(file: &Path, text: &str, lords: &[Lords], why: &str) -> Vec<String> {
    let v: Value = serde_json::from_str(text.trim_start_matches('\u{feff}')).unwrap_or_default();
    let at = file.display();
    let name = |((event, g, n), (_, hook)): ((String, usize, usize), (Value, Value))| {
        let what = match lords_in_file(&event, &hook, lords) {
            Some(l) => format!("Lords' {} guard ({event})", l.guard),
            None => format!("{event} entry {} hook {}", g + 1, n + 1),
        };
        format!("Codex hook not known trusted: {what} in {at}: {why}")
    };
    slots(&v).into_iter().map(name).collect()
}

/// What writing Codex's hooks from `before` to `after` does to trust, as a plan says it (design note §2.5): each Lords
/// hook whose slot changes, which sync trusts after the write, and each other hook whose slot changes, which falls.
pub fn foresee(before: &Value, after: &Value, lords: &[Lords]) -> Vec<String> {
    let was = slots(before);
    let mut notes = Vec::new();
    for (slot, now) in slots(after) {
        if was.get(&slot) == Some(&now) {
            continue;
        }
        let (event, g, n) = &slot;
        notes.push(match lords_in_file(event, &now.1, lords) {
            Some(l) => format!("Codex: sync trusts Lords' {} guard ({event}) after the write", l.guard),
            None => format!(
                "Codex: the hook now at {event} entry {} hook {} isn't Lords', and falls to untrusted there: trust it in Codex under /hooks",
                g + 1,
                n + 1
            ),
        });
    }
    notes
}

/// After sync wrote Codex's hooks file `file`, whose text is `text`: trusts each hook Codex lists there that is Lords'
/// own (`lords`, I2), enabled and not trusted at its listed hash, once `backup` has kept Codex's config.toml; then one
/// line per hook of that file that doesn't run, read from a list made after the last write (I1), or, where Codex can't
/// be asked or its write fails, one per hook of `text`, with why (I4). Asked once each: no retry (A3).
pub fn settle(
    codex: &Codex,
    file: &Path,
    text: &str,
    lords: &[Lords],
    backup: &mut dyn FnMut(&Path) -> Result<(), String>,
) -> Vec<String> {
    let (exe, deadline) = match codex {
        Codex::Ask { exe, deadline } => (exe, *deadline),
        Codex::None(why) => return unknown(file, text, lords, why),
    };
    let Some(home) = file.parent() else { return unknown(file, text, lords, "it has no dir") };
    let asked = |e: String| unknown(file, text, lords, &format!("Codex can't be asked ({e})"));
    let mine =
        |all: Vec<Value>| -> Vec<Value> { all.into_iter().filter(|h| in_file(h, file)).collect() };
    let mut now = match list(exe, deadline, home) {
        Ok(all) => mine(all),
        Err(e) => return asked(e),
    };
    // Lords' own (I2), not switched off, with a hash as listed, and not trusted at it.
    let pick = |h: &&Value| {
        let untrusted = matches!(h["trustStatus"].as_str(), Some("untrusted" | "modified"));
        let on = h["enabled"] != false && h["currentHash"].is_string();
        untrusted && on && lords_guard(h, lords).is_some()
    };
    let picks: Vec<&Value> = now.iter().filter(pick).collect();
    if !picks.is_empty() {
        let key = |h: &&Value| h["key"].as_str().unwrap_or_default().to_string();
        let value: serde_json::Map<String, Value> =
            picks.iter().map(|h| (key(h), json!({"trusted_hash": h["currentHash"]}))).collect();
        let edit = json!({"keyPath": "hooks.state", "value": value, "mergeStrategy": "upsert"});
        if let Err(e) = backup(&home.join("config.toml")) {
            let why = format!("Codex's config.toml can't be backed up ({e}); nothing was trusted");
            return unknown(file, text, lords, &why);
        }
        let write = json!({"edits": [edit], "reloadUserConfig": true});
        if let Err(e) = ask(exe, deadline, home, "config/batchWrite", write) {
            return unknown(file, text, lords, &format!("Codex's config write failed ({e})"));
        }
        now = match list(exe, deadline, home) {
            Ok(all) => mine(all),
            Err(e) => return asked(e),
        };
    }
    let off = |h: &&Value| {
        h["trustStatus"] != "trusted" || (h["enabled"] == false && lords_guard(h, lords).is_some())
    };
    now.iter().filter(off).map(|h| named(h, lords)).collect()
}
