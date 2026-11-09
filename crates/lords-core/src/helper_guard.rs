//! The helper guard (`lords guard helper`, a `PreToolUse` hook; design note
//! Harness/notes/2026-11-02-lords-native-helper-guard.md). In T3, every helper an agent starts is a T3 `delegate_task`
//! thread, never a native subagent (rule.t3-subagents): only T3 threads are visible to the owner and can be handed
//! over. In a session T3 started (`t3::session`), Claude Code's native helper tool is refused with a line naming
//! `delegate_task`; anywhere else, and whenever the guard can't tell, the call goes through without a word (I1, I4).
//! Only that tool: `SendMessage` to a running agent, the `Task…` tools and `Workflow` go through (A5, A6). Codex has
//! no helper guard yet: whether its `spawn_agent` reaches `PreToolUse`, and what its hook sees, is unprobed (§5.1).

use crate::guard::Answer;
use serde_json::{json, Value};

/// Claude Code's native helper tool as an exact list in its matcher syntax: `Agent` (2.1.316), `Task` before it.
pub const MATCHER: &str = "Agent|Task";

/// The refusal, one line: the tool to use instead, and the rule (A7).
const REASON: &str = "Lords: in T3, start helpers with the t3-code `delegate_task` tool (mcp__t3-code__delegate_task), not a native subagent (rule.t3-subagents).";

/// The hook's answer to one `PreToolUse` event: a denial when it starts a native helper and `t3` says T3 started the
/// session; otherwise nothing, with a note when `t3` can't tell. `t3` is asked only for a helper start.
pub fn helper_hook(input: &str, t3: impl FnOnce() -> Result<bool, String>) -> Answer {
    let through = |note: Option<String>| Answer { stdout: String::new(), note };
    let Ok(ev) = serde_json::from_str::<Value>(input) else { return through(None) };
    let tool = ev["tool_name"].as_str();
    if !MATCHER.split('|').any(|t| tool == Some(t)) {
        return through(None);
    }
    match t3() {
        Ok(true) => Answer {
            stdout: json!({ "hookSpecificOutput": {
                "hookEventName": "PreToolUse",
                "permissionDecision": "deny",
                "permissionDecisionReason": REASON,
            } })
            .to_string(),
            note: None,
        },
        Ok(false) => through(None),
        // A harness shows the note: `t3::session`'s own lines as they are, anything else (a process's) as a category,
        // compared by equality, never parsed (design note privacy-own-allow, sol-loop r5, S1).
        Err(e) => {
            let why = if e == crate::t3::NO_PID || e == crate::t3::NOT_A_PID {
                e.as_str()
            } else {
                "its process's command line can't be read"
            };
            through(Some(format!("whether T3 started this session can't be told ({why})")))
        }
    }
}
