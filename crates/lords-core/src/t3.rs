//! Whether a hook runs for a session T3 started (design note Harness/notes/2026-11-02-lords-native-helper-guard.md,
//! §5.1: H6, with H2 as prefilter). T3 sets `T3CODE_THREAD_ID` for the sessions it starts, but a CLI run from one of
//! their shells inherits it, and `CLAUDE_CODE_ENTRYPOINT=t3-code` with it, so neither says T3 started this session.
//! What does: Claude Code names its own process in `CLAUDE_PID` for each hook and shell it runs (a nested CLI names its
//! own), and T3 launches every Claude Code session with its tools in `--allowedTools` (`mcp__t3-code__*`; 56 of 56 on
//! 2 Nov). A process's arguments are its launcher's, never inherited.

/// What T3's launch lists in `--allowedTools`: its MCP server's tools.
const T3_TOOLS: &str = "mcp__t3-code__";

/// Whether a hook runs for a session T3 started. `thread` and `claude_pid`: the hook's `T3CODE_THREAD_ID` and
/// `CLAUDE_PID`; `argv_of`: a process's arguments (`process::command_line`). Without a thread it is `Ok(false)` and
/// reads nothing. `Err` when `CLAUDE_PID` names no process or the process can't be read: a caller lets the call through
/// (I4).
pub fn session(
    thread: Option<&str>,
    claude_pid: Option<&str>,
    argv_of: impl FnOnce(u32) -> Result<Vec<String>, String>,
) -> Result<bool, String> {
    if thread.is_none_or(str::is_empty) {
        return Ok(false);
    }
    let pid = claude_pid.ok_or("no CLAUDE_PID")?;
    let pid = pid.parse::<u32>().map_err(|_| format!("CLAUDE_PID `{pid}` isn't a process id"))?;
    Ok(launched_with_t3_tools(&argv_of(pid)?))
}

/// Whether `argv` lists T3's tools in `--allowedTools` (or `--allowed-tools`): its values up to the next flag, each a
/// comma- or space-separated list (`claude -h`, 2.1.316). A prompt or a settings value naming them doesn't count.
fn launched_with_t3_tools(argv: &[String]) -> bool {
    let mut lists = Vec::new();
    let mut taking = false;
    for a in argv.iter().skip(1) {
        if let Some(v) =
            a.strip_prefix("--allowedTools=").or_else(|| a.strip_prefix("--allowed-tools="))
        {
            lists.push(v);
            taking = false;
        } else if a == "--allowedTools" || a == "--allowed-tools" {
            taking = true;
        } else if a.starts_with('-') {
            taking = false;
        } else if taking {
            lists.push(a);
        }
    }
    lists.iter().flat_map(|l| l.split([',', ' '])).any(|t| t.starts_with(T3_TOOLS))
}
