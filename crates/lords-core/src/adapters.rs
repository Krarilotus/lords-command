//! Adapters: everything one agent harness needs that the stack format doesn't know about. The stack is plain
//! entries; an adapter says which agents its files reach, which untracked file it reads in a repo, how big its
//! instruction text may get, and how composed text is written for it. A third harness is one more adapter here.
//!
//! The facts each adapter encodes are checked in docs/load-order.md.

use crate::compose::Composed;
use crate::model::Audience;

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
}

pub static ALL: [&dyn Adapter; 2] = [&ClaudeCode, &Codex];

pub fn get(id: &str) -> Option<&'static dyn Adapter> {
    ALL.iter().copied().find(|a| a.id() == id)
}

pub fn ids() -> Vec<&'static str> {
    ALL.iter().map(|a| a.id()).collect()
}
