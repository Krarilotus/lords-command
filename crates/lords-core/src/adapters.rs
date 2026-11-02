//! Adapters: everything one agent harness needs that the stack format doesn't know about. The stack is plain
//! entries; an adapter says which agents its files reach, which untracked file it reads in a repo, how big its
//! instruction text may get, and how composed text is written for it. A third harness is one more adapter here.
//!
//! The facts each adapter encodes are checked in docs/load-order.md.

use crate::compose::Composed;
use crate::fsguard;
use crate::model::Audience;
use std::path::{Path, PathBuf};

pub trait Adapter: Sync {
    /// The id stack.toml uses (`adapter = "…"`).
    fn id(&self) -> &'static str;
    /// The agents its files reach, when a target names none.
    fn audience(&self) -> Vec<Audience>;
    /// The untracked file it reads in every checkout, after (or instead of) the tracked ones.
    fn repo_file(&self) -> &'static str;
    /// The most text it reads of a project's instruction files, past which it cuts them, if it has a limit; its
    /// user-level file isn't counted.
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
    /// Where a checkout's own skills live for this harness, if it reads such a dir (sync links development
    /// skills there).
    fn repo_skills_dir(&self) -> Option<&'static str> {
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
    /// Whether a place's file may show paths relative to its checkout: only once a marker run has shown the
    /// harness's agents resolve them there (docs/load-order.md). Until then every path is absolute.
    fn relative_paths(&self) -> bool {
        false
    }
    /// Whether a place's file carries the place's default role's entries and notes inline (orchestrator, 25 Oct,
    /// option 1). Where it doesn't, they go by reference like every other role's, and the role-files line names
    /// the default role's file first.
    fn default_role_inline(&self) -> bool {
        true
    }
    /// The Lords guards its harness runs as hooks (sync.rs `guard_event`); none: it runs none.
    fn guards(&self) -> &'static [&'static str] {
        &[]
    }
    /// The hook entries that run guard `guard` of Lords' copy `exe`: one.
    /// `home`: the tool home sync runs with, which `child` takes (its apply lock; repair 2 sol-loop r3, X1).
    fn guard_hooks(&self, exe: &str, guard: &str, _home: &str) -> Vec<serde_json::Value> {
        vec![serde_json::json!({ "type": "command", "command": exe, "args": ["guard", guard] })]
    }
    /// The guard a hook entry runs, when it runs Lords' copy `exe` (`guard_hook`): a user's own hook, whatever its
    /// args, is never Lords'.
    fn guard_of(&self, hook: &serde_json::Value, exe: &str) -> Option<String> {
        hook["command"]
            .as_str()
            .is_some_and(|c| c.replace('\\', "/").eq_ignore_ascii_case(exe))
            .then(|| hook["args"][1].as_str().unwrap_or("").to_string())
    }
    /// What a settings plan that adds a guard hook says, where the harness runs it only after a step of its own.
    fn guard_note(&self) -> Option<&'static str> {
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

    fn guards(&self) -> &'static [&'static str] {
        &["rm", "queue", "session", "turn", "child", "role"]
    }

    /// A child's start goes in one hook, one read (fix check r2, FC1), with sync's tool home and the deadline the
    /// guard's waits end inside (`guard::CHILD_HOOK_TIMEOUT_SECS`; fix check r2, FC5); a launched session's role start
    /// alike (F10), naming its harness, whose place file the start goes beside (F10-R1-03).
    fn guard_hooks(&self, exe: &str, guard: &str, home: &str) -> Vec<serde_json::Value> {
        if guard != "child" && guard != "role" {
            return vec![
                serde_json::json!({ "type": "command", "command": exe, "args": ["guard", guard] }),
            ];
        }
        let mut args = vec!["guard", guard, "--home", home];
        if guard == "role" {
            args.extend(["--adapter", self.id()]);
        }
        vec![
            serde_json::json!({ "type": "command", "command": exe, "args": args, "timeout": crate::guard::CHILD_HOOK_TIMEOUT_SECS }),
        ]
    }

    fn repo_skills_dir(&self) -> Option<&'static str> {
        Some(".claude/skills")
    }

    /// With another profile (`CLAUDE_CONFIG_DIR`), `~/.claude/CLAUDE.md` still loads as an ancestor project file
    /// under home, and a non-empty one stops Claude reading any repo's `AGENTS.md` (load-order.md).
    fn shadowing_file(&self, user_file: &Path) -> Option<PathBuf> {
        let default = crate::config::home()?.join(".claude").join("CLAUDE.md");
        (slashes(user_file).to_lowercase() != slashes(&default).to_lowercase()).then_some(default)
    }
}

/// Codex: `$CODEX_HOME/AGENTS.md`; `AGENTS.override.md` per checkout, which replaces the tracked `AGENTS.md`.
/// It reads at most `project_doc_max_bytes` (32 KiB) of the project's files, from the repo root down to the working
/// dir, and cuts the rest without a word; `$CODEX_HOME/AGENTS.md` is read whole, outside that budget (openai/codex
/// 0d68383: codex-rs/core/src/agents_md.rs:68-92 and 142-178, codex-rs/codex-home/src/instructions/mod.rs:41-78).
/// It doesn't expand `@path` imports, so memory reaches it as an index it reads on demand.
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

    /// Codex stops at its byte limit, so the default role's text stays in its role file, named first.
    fn default_role_inline(&self) -> bool {
        false
    }

    /// Codex's hooks (`~/.codex/hooks.json`) run a command line; `child` is the guard a probe showed it runs (design
    /// note 2026-10-31-lords-core-lines-composer, P2): the child took its context, once the hook was trusted. `role`
    /// too: its `UserPromptSubmit` event and context are Claude's (design note 2026-11-02-lords-role-at-launch, H3).
    fn guards(&self) -> &'static [&'static str] {
        &["child", "role"]
    }

    /// A command line, and the whole context passed on (`additionalContextLimit` 0): no parts. On Windows Codex runs it
    /// with PowerShell, where a line starting with a quoted path is no command (design note 2026-11-02-lords-role-at-
    /// launch, H8, H9): there it is PowerShell's call, `& 'exe' …`, each path a literal string.
    fn guard_hooks(&self, exe: &str, guard: &str, home: &str) -> Vec<serde_json::Value> {
        let mut line = if cfg!(windows) {
            let lit = |s: &str| format!("'{}'", s.replace('\'', "''"));
            format!("& {} guard {guard} --home {}", lit(exe), lit(home))
        } else {
            format!("\"{exe}\" guard {guard} --home \"{home}\"")
        };
        // The role hook names its harness, whose place file the start goes beside (F10-R1-03).
        if guard == "role" {
            line.push_str(" --adapter codex");
        }
        vec![serde_json::json!({
            "type": "command",
            "command": line,
            "timeout": crate::guard::CHILD_HOOK_TIMEOUT_SECS,
            "additionalContextLimit": 0,
        })]
    }

    /// The guard's name: the word after `guard` (its options follow), in either form `guard_hooks` writes or wrote
    /// (an earlier build's `"exe" guard …` too, so sync replaces it).
    fn guard_of(&self, hook: &serde_json::Value, exe: &str) -> Option<String> {
        let line = hook["command"].as_str()?.replace('\\', "/");
        let quoted =
            [format!("\"{exe}\" guard "), format!("& '{}' guard ", exe.replace('\'', "''"))];
        let prefix = quoted
            .iter()
            .find(|p| line.get(..p.len()).is_some_and(|head| head.eq_ignore_ascii_case(p)))?;
        Some(line[prefix.len()..].split_whitespace().next().unwrap_or("").to_string())
    }

    fn guard_note(&self) -> Option<&'static str> {
        Some("Codex runs a new or changed hook once it is reviewed: open Codex and accept it under /hooks")
    }

    /// The override replaces the tracked `AGENTS.md` and Codex doesn't expand includes, so the tracked text is
    /// carried inline (and re-planned when a checkout's `AGENTS.md` changes).
    fn repo_preamble(&self, checkout: &Path) -> Option<String> {
        // Lossy here; sync's writer refuses a checkout whose AGENTS.md is there but can't be read (r5).
        let text = fsguard::read_string_lossy(checkout.join("AGENTS.md"))?;
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
