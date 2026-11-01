//! `lords paths`: which T3-delegated agent paths start with every core line of their role (F10; design §5.2 L1: "a path
//! that can't is marked incomplete and isn't used for work those rules govern"; design note
//! Harness/notes/2026-11-02-lords-role-at-launch.md, C3, D7). One row per settings target whose harness runs the role
//! guard, and per role, from the starts installed in each declared checkout and workspace. A reading that can't be made
//! is a row marked incomplete, never complete.

use crate::adapters::{self, Adapter};
use crate::stack::Stacks;
use crate::sync;
use serde::Serialize;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// One path and whether it delivers its role's lines.
#[derive(Debug, Serialize)]
pub struct Row {
    pub state: &'static str,
    pub path: String,
    pub why: String,
}

/// Where `paths` looks: the tool home (the guard copy, the hooks' command line), the Codex it asks, and how long it
/// waits for Codex's answer.
pub struct Ask {
    pub home: PathBuf,
    pub codex: Option<PathBuf>,
    pub deadline: Duration,
}

/// How long Codex's app-server may take to answer `hooks/list` (0.8 s measured on this PC; probe H4).
pub const CODEX_DEADLINE: Duration = Duration::from_secs(20);

fn row(complete: bool, path: String, why: impl Into<String>) -> Row {
    Row { state: if complete { "complete" } else { "incomplete" }, path, why: why.into() }
}

/// Every row (module doc).
pub fn paths(stacks: &Stacks, ask: &Ask) -> Vec<Row> {
    let mut rows = Vec::new();
    let guard = sync::guard_place(&ask.home)
        .and_then(|g| sync::guard_speaks_role(&g, &ask.home).map(|()| g));
    let targets = stacks.private.iter().flat_map(|p| p.manifest.targets.iter());
    for t in targets.filter(|t| t.kind == "settings") {
        let Some(adapter) = adapters::get(&t.adapter) else { continue };
        if !adapter.guards().contains(&"role") {
            continue;
        }
        let path = format!("T3-delegated, {} ({})", adapter.id(), t.path);
        rows.push(match target_state(t, adapter, &guard, ask) {
            Ok(why) => row(true, path, why),
            Err(why) => row(false, path, why),
        });
    }
    rows.extend(role_rows(stacks));
    rows
}

/// Whether sessions under one settings target get their role's start: why it is complete, or why not.
fn target_state(
    t: &crate::stack::Target,
    adapter: &dyn Adapter,
    guard: &Result<crate::fsguard::Contained, String>,
    ask: &Ask,
) -> Result<String, String> {
    let file = crate::config::expand_home(&t.path);
    let exe = sync::guard_place(&ask.home)?;
    let text = crate::fsguard::read_text(&file)?.unwrap_or_default();
    let settings: serde_json::Value =
        serde_json::from_str(text.trim_start_matches('\u{feff}')).unwrap_or_default();
    let hooks = settings.get("hooks").cloned().unwrap_or_default();
    if !sync::guard_names(&hooks, adapter, exe.path()).iter().any(|g| g == "role") {
        return Err(format!(
            "{} doesn't register Lords' role hook: add `role` to this target's `guards` in the stack, then `lords sync --user --apply`",
            file.display()
        ));
    }
    guard.as_ref().map_err(|e| e.clone())?;
    if adapter.id() != "codex" {
        return Ok("its role hook is registered and the guard copy speaks its protocol".into());
    }
    let home = adapters::slashes(&ask.home);
    let want = adapter.guard_hooks(&adapters::slashes(exe.path()), "role", &home);
    let want = want.first().and_then(|h| h["command"].as_str()).unwrap_or_default().to_string();
    let codex_home = file.parent().unwrap_or(Path::new(".")).to_path_buf();
    match codex_trust(ask, &codex_home, &want) {
        Ok(Some(s)) if s == "trusted" => Ok("Codex trusts Lords' role hook".into()),
        Ok(Some(s)) => Err(format!(
            "Codex reports Lords' role hook as {s}, so it doesn't run: accept it in Codex under /hooks"
        )),
        Ok(None) => Err("Codex doesn't list Lords' role hook".into()),
        Err(e) => Err(format!("Codex can't be asked whether it trusts Lords' role hook ({e})")),
    }
}

/// The trust status Codex's app-server gives the hook whose command is `want`, `None` where it lists no such hook.
fn codex_trust(ask: &Ask, codex_home: &Path, want: &str) -> Result<Option<String>, String> {
    let codex = match &ask.codex {
        Some(c) => c.clone(),
        None => find_codex().ok_or("no `codex` on PATH")?,
    };
    let cwd = adapters::slashes(codex_home);
    let requests = [
        serde_json::json!({"id": 1, "method": "initialize",
            "params": {"clientInfo": {"name": "lords", "version": env!("CARGO_PKG_VERSION")}}})
        .to_string(),
        serde_json::json!({"method": "initialized"}).to_string(),
        serde_json::json!({"id": 2, "method": "hooks/list", "params": {"cwds": [cwd]}}).to_string(),
    ];
    let answer = crate::process::codex_ask(&codex, codex_home, &requests, 2, ask.deadline)?;
    if let Some(e) = answer.get("error") {
        return Err(format!("it answered an error: {e}"));
    }
    let hooks = answer["result"]["data"].as_array().into_iter().flatten();
    let found = hooks
        .flat_map(|d| d["hooks"].as_array().into_iter().flatten())
        .find(|h| h["command"].as_str() == Some(want));
    Ok(found.map(|h| h["trustStatus"].as_str().unwrap_or("unknown").to_string()))
}

/// `codex` on PATH: `codex.exe`, then the npm shim `codex.cmd` on Windows; `codex` elsewhere.
fn find_codex() -> Option<PathBuf> {
    let names: &[&str] = if cfg!(windows) { &["codex.exe", "codex.cmd"] } else { &["codex"] };
    let dirs: Vec<PathBuf> = std::env::split_paths(&std::env::var_os("PATH")?).collect();
    names.iter().find_map(|n| {
        dirs.iter()
            .map(|d| d.join(n))
            .find(|p| matches!(crate::fsguard::kind(p), Ok(crate::fsguard::Kind::File)))
    })
}

/// Per role but `product`: one row per declared checkout or workspace dir whose installed start for it the guard can't
/// give (missing, unreadable, or past what one hook gives; sol-loop r1 S1: what is delivered, not what would be
/// composed), or one complete row.
fn role_rows(stacks: &Stacks) -> Vec<Row> {
    use crate::fsguard::InDir;
    let Some(private) = &stacks.private else { return Vec::new() };
    let mut dirs: Vec<(String, PathBuf)> = Vec::new();
    for (name, repo) in &private.manifest.repos {
        let label = |c: &String| (format!("repo {name} ({c})"), crate::config::expand_home(c));
        dirs.extend(repo.checkouts.iter().map(label));
    }
    for (name, w) in &private.manifest.workspaces {
        dirs.push((format!("workspace {name}"), crate::config::expand_home(&w.path)));
    }
    let max = sync::CHILD_START_CHARS;
    let mut rows = Vec::new();
    for role in sync::declared_roles(stacks).iter().filter(|r| *r != crate::compose::PRODUCT_ROLE) {
        let file = sync::start_file(role);
        let long: Vec<Row> = dirs
            .iter()
            .filter_map(|(label, dir)| {
                let why = match crate::fsguard::read_text_in(&dir.join(sync::ROLE_DIR), &file) {
                    Ok(InDir::Text(t)) if t.contains(crate::compose::BEGIN) => {
                        let n = crate::guard::child_context(&t).chars().count();
                        (n > max).then(|| format!(
                            "its start is {n} characters, past the {max} one hook gives: such a session is told to read role-{role}.md instead"
                        ))?
                    }
                    Ok(_) => format!("no {file} there: sync the place"),
                    Err(e) => format!("{file} can't be read ({e})"),
                };
                Some(row(false, format!("T3-delegated as {role}, {label}"), why))
            })
            .collect();
        if long.is_empty() {
            rows.push(row(
                true,
                format!("T3-delegated as {role}"),
                format!("every place has its start, within one hook ({max} characters)"),
            ));
        }
        rows.extend(long);
    }
    rows
}
