//! Whether a hook runs for a session T3 started (design note Harness/notes/2026-11-02-lords-native-helper-guard.md,
//! §5.1: H6, with H2 as prefilter). T3 sets `T3CODE_THREAD_ID` for the sessions it starts, but a CLI run from one of
//! their shells inherits it, and `CLAUDE_CODE_ENTRYPOINT=t3-code` with it, so neither says T3 started this session.
//! What does: Claude Code names its own process in `CLAUDE_PID` for each hook and shell it runs (a nested CLI names its
//! own), and T3 launches every Claude Code session with its tools allowed, `--allowedTools mcp__t3-code__*` (56 of 56
//! on 2 Nov, 32 of 32 on 7 Nov). A process's arguments are its launcher's, never inherited.

/// T3's flag as T3 writes it, two arguments: the only form that counts (sol-loop r2; design note §5.4). Reading the
/// `--allowedTools` list more widely was fooled by a permission rule's specifier and by another option's value.
const T3_FLAG: [&str; 2] = ["--allowedTools", "mcp__t3-code__*"];

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
    // Not repeated: the guard's note carries this line, and a harness shows it (sol-loop r4, S1).
    let pid = pid.parse::<u32>().map_err(|_| "CLAUDE_PID isn't a process id".to_string())?;
    Ok(launched_with_t3_tools(&argv_of(pid)?))
}

/// Whether `argv` holds `T3_FLAG`, its two arguments side by side, before `--`, which ends the options (sol-loop r1,
/// S2). Any other spelling of the flag goes quiet (A11).
fn launched_with_t3_tools(argv: &[String]) -> bool {
    let options: Vec<&str> =
        argv.iter().skip(1).map(String::as_str).take_while(|a| *a != "--").collect();
    options.windows(2).any(|pair| pair == T3_FLAG)
}
