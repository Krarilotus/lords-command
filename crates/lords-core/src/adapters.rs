//! Adapters: everything one agent harness needs that the stack format doesn't know about. The stack is plain
//! entries; an adapter says which agents its files reach, which untracked file it reads in a repo, how big its
//! instruction text may get, and how composed text is written for it. A third harness is one more adapter here.
//!
//! The facts each adapter encodes are checked in docs/load-order.md.

use crate::compose::Composed;
use crate::model::Audience;
use std::path::{Path, PathBuf};

pub trait Adapter: Sync {
    /// The id stack.toml uses (`adapter = "…"`).
    fn id(&self) -> &'static str;
    /// The agents its files reach, when a target names none.
    fn audience(&self) -> Vec<Audience>;
    /// The untracked file it reads in every checkout, after (or instead of) the tracked ones.
    fn repo_file(&self) -> &'static str;
    /// The most instruction text it reads in total, if it has a limit.
    fn byte_limit(&self) -> Option<usize> {
        None
    }
    /// The composed text as this harness's file holds it.
    fn render(&self, composed: &Composed) -> String {
        composed.text.clone()
    }
    /// The line that includes another file, if the harness expands includes (then targets may share one composed
    /// file through `via`).
    fn import_line(&self, _path: &Path) -> Option<String> {
        None
    }
    /// What a repo file starts with so the checkout's tracked instructions still load beside the stack's text.
    fn repo_preamble(&self, _checkout: &Path) -> Option<String> {
        None
    }
    /// The settings key that lists instruction files the harness must skip, if it has one.
    fn excludes_key(&self) -> Option<&'static str> {
        None
    }
    /// A file the harness also loads when `user_file` is its user file, which hides repos' own instructions.
    fn shadowing_file(&self, _user_file: &Path) -> Option<PathBuf> {
        None
    }
}

/// Claude Code: user file `CLAUDE.md` in the profile dir; `CLAUDE.local.md` per checkout, which also keeps a
/// tracked `AGENTS.md` from loading.
pub struct ClaudeCode;

impl Adapter for ClaudeCode {
    fn id(&self) -> &'static str {
        "claude"
    }

    fn audience(&self) -> Vec<Audience> {
        vec![Audience::Opus]
    }

    fn repo_file(&self) -> &'static str {
        "CLAUDE.local.md"
    }

    fn import_line(&self, path: &Path) -> Option<String> {
        Some(format!("@{}", slashes(path)))
    }

    /// A `CLAUDE.local.md` stops a tracked `AGENTS.md` from loading unless it imports it (load-order.md).
    fn repo_preamble(&self, checkout: &Path) -> Option<String> {
        checkout.join("AGENTS.md").is_file().then(|| "@AGENTS.md".to_string())
    }

    fn excludes_key(&self) -> Option<&'static str> {
        Some("claudeMdExcludes")
    }

    /// With another profile (`CLAUDE_CONFIG_DIR`), `~/.claude/CLAUDE.md` still loads as an ancestor project file
    /// under home, and a non-empty one stops Claude reading any repo's `AGENTS.md` (load-order.md).
    fn shadowing_file(&self, user_file: &Path) -> Option<PathBuf> {
        let default = crate::config::home()?.join(".claude").join("CLAUDE.md");
        (slashes(user_file).to_lowercase() != slashes(&default).to_lowercase()).then_some(default)
    }
}

/// Codex: `$CODEX_HOME/AGENTS.md`; `AGENTS.override.md` per checkout, which replaces the tracked `AGENTS.md`.
/// It reads at most `project_doc_max_bytes` (32 KiB) of instruction files in total, and doesn't expand `@path`
/// imports, so memory reaches it as an index it reads on demand.
pub struct Codex;

impl Adapter for Codex {
    fn id(&self) -> &'static str {
        "codex"
    }

    fn audience(&self) -> Vec<Audience> {
        vec![Audience::Sol, Audience::Astra]
    }

    fn repo_file(&self) -> &'static str {
        "AGENTS.override.md"
    }

    fn byte_limit(&self) -> Option<usize> {
        Some(32 * 1024)
    }

    /// The override replaces the tracked `AGENTS.md` and Codex doesn't expand includes, so the tracked text is
    /// carried inline (and re-planned when a checkout's `AGENTS.md` changes).
    fn repo_preamble(&self, checkout: &Path) -> Option<String> {
        let text = std::fs::read_to_string(checkout.join("AGENTS.md")).ok()?;
        let text = text.trim_end().to_string();
        (!text.is_empty()).then_some(text)
    }
}

/// A path with forward slashes, the form both harnesses accept in instruction files and settings.
pub fn slashes(p: &Path) -> String {
    p.to_string_lossy().replace('\\', "/")
}

pub static ALL: [&dyn Adapter; 2] = [&ClaudeCode, &Codex];

pub fn get(id: &str) -> Option<&'static dyn Adapter> {
    ALL.iter().copied().find(|a| a.id() == id)
}

pub fn ids() -> Vec<&'static str> {
    ALL.iter().map(|a| a.id()).collect()
}
