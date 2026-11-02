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
    let targets = stacks.private.iter().flat_map(|p| p.manifest.targets.iter());
    for t in targets.filter(|t| t.kind == "settings") {
        let Some(adapter) = adapters::get(&t.adapter) else { continue };
        if !adapter.guards().contains(&"role") {
            continue;
        }
        let path = format!("T3-delegated, {} ({})", adapter.id(), t.path);
        rows.push(match target_state(t, adapter, ask) {
            Ok(why) => row(true, path, why),
            Err(why) => row(false, path, why),
        });
    }
    rows.extend(role_rows(stacks, &ask.home));
    rows
}

/// The event a role hook must be under to run at a session's first prompt (F10-R1-02).
const PROMPT_EVENT: &str = "UserPromptSubmit";

/// Whether sessions under one settings target get their role's start: why it is complete, or why not. Complete needs
/// the very hook sync writes for this adapter under `UserPromptSubmit` (another event's, or another form's, is none:
/// F10-R1-02), the harness's hooks not switched off (Claude's `disableAllHooks`), the guard copy answering that hook's
/// protocol, and for Codex its app-server listing that command, on that event, enabled and trusted.
fn target_state(
    t: &crate::stack::Target,
    adapter: &dyn Adapter,
    ask: &Ask,
) -> Result<String, String> {
    let file = crate::config::expand_home(&t.path);
    let exe = sync::guard_place(&ask.home)?;
    let text = crate::fsguard::read_text(&file)?.unwrap_or_default();
    let settings: serde_json::Value =
        serde_json::from_str(text.trim_start_matches('\u{feff}')).unwrap_or_default();
    let home = adapters::slashes(&ask.home);
    let want = adapter.guard_hooks(&adapters::slashes(exe.path()), "role", &home);
    let want = want.first().cloned().unwrap_or_default();
    let on_prompt = settings["hooks"][PROMPT_EVENT]
        .as_array()
        .into_iter()
        .flatten()
        .flat_map(|e| e["hooks"].as_array().into_iter().flatten())
        .any(|h| *h == want);
    if !on_prompt {
        return Err(format!(
            "{} doesn't register Lords' role hook on {PROMPT_EVENT} as sync writes it: add `role` to this target's `guards` in the stack, then `lords sync --user --apply`",
            file.display()
        ));
    }
    if settings["disableAllHooks"] == true {
        return Err(format!("{} switches every hook off (`disableAllHooks`)", file.display()));
    }
    sync::guard_speaks_role(&exe, &ask.home, adapter.id())?;
    if adapter.id() != "codex" {
        return Ok("its role hook is registered and the guard copy speaks its protocol".into());
    }
    let want = want["command"].as_str().unwrap_or_default().to_string();
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
    // The command on the prompt event (Codex names it `userPromptSubmit`; F10-R1-02), and not switched off.
    let hooks = answer["result"]["data"].as_array().into_iter().flatten();
    let found = hooks.flat_map(|d| d["hooks"].as_array().into_iter().flatten()).find(|h| {
        h["command"].as_str() == Some(want) && h["eventName"].as_str() == Some("userPromptSubmit")
    });
    Ok(found.map(|h| match h["enabled"].as_bool() {
        Some(false) => "disabled".to_string(),
        _ => h["trustStatus"].as_str().unwrap_or("unknown").to_string(),
    }))
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

/// Per role but `product`: one row per checkout, worktree or workspace dir sync writes for whose installed start for it
/// the guard can't give (missing, unreadable, or past what one hook gives; sol-loop r1 S1: what is delivered, not what
/// would be composed), or one complete row. The checkouts and worktrees are sync's own (`sync::checkouts`, r2 S1); a
/// repo whose can't be listed is a row of its own, incomplete. Beside the starts, each harness the place has needs its
/// place file there, which carries the lines the starts leave out (F10-R1-03), wherever sync writes one (its
/// composition there isn't empty): one row per dir and harness without it.
fn role_rows(stacks: &Stacks, home: &Path) -> Vec<Row> {
    use crate::fsguard::InDir;
    let Some(private) = &stacks.private else { return Vec::new() };
    let mut rows = Vec::new();
    let mut dirs: Vec<(String, PathBuf, &String, &[String])> = Vec::new();
    for (name, repo) in &private.manifest.repos {
        match sync::checkouts(repo) {
            Ok((roots, _, _)) => dirs.extend(roots.iter().map(|r| {
                let p = r.path().to_path_buf();
                let label = format!("repo {name} ({})", crate::adapters::slashes(&p));
                (label, p, name, &repo.adapters[..])
            })),
            Err(e) => rows.push(row(
                false,
                format!("T3-delegated, repo {name}"),
                format!("its checkouts can't be listed ({e})"),
            )),
        }
    }
    for (name, w) in &private.manifest.workspaces {
        let dir = crate::config::expand_home(&w.path);
        dirs.push((format!("workspace {name}"), dir, name, &w.adapters[..]));
    }
    let mut place_files = Vec::new();
    for (label, dir, name, ids) in &dirs {
        let has = |a: &dyn Adapter| ids.is_empty() || ids.iter().any(|i| i == a.id());
        for a in adapters::ALL.iter().copied().filter(|a| has(*a)) {
            let layers = sync::place_layers(stacks, name, false);
            if sync::place_text(stacks, layers, a, dir, home).0.is_empty() {
                continue;
            }
            let file = a.repo_file();
            let why = match crate::fsguard::read_text(&dir.join(file)) {
                Ok(Some(t)) if t.contains(crate::compose::BEGIN) => continue,
                Ok(_) => format!("no Lords {file} there, which carries the lines the starts leave out: sync the place"),
                Err(e) => format!("{file} can't be read ({e})"),
            };
            place_files.push(row(false, format!("T3-delegated with {file}, {label}"), why));
        }
    }
    if place_files.is_empty() {
        rows.push(row(
            true,
            "T3-delegated, place files".into(),
            "every place has its harnesses' files",
        ));
    }
    rows.extend(place_files);
    let dirs: Vec<(String, PathBuf)> = dirs.into_iter().map(|(l, d, _, _)| (l, d)).collect();
    let max = sync::CHILD_START_CHARS;
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
