//! Whether a hook runs for a session T3 started (design note Harness/notes/2026-11-02-lords-native-helper-guard.md,
//! §5.1). Not built yet: the tests come first.

/// `Ok(true)`: T3 started this session; `Ok(false)`: it didn't; `Err`: it can't be told.
pub fn session(
    _thread: Option<&str>,
    _claude_pid: Option<&str>,
    _argv_of: impl FnOnce(u32) -> Result<Vec<String>, String>,
) -> Result<bool, String> {
    Ok(false)
}
