//! The helper guard (`lords guard helper`, a `PreToolUse` hook; design note
//! Harness/notes/2026-11-02-lords-native-helper-guard.md). Not built yet: the tests come first.

use crate::guard::Answer;

/// The hook's answer to one `PreToolUse` event; `t3` tells whether T3 started this session.
pub fn helper_hook(_input: &str, _t3: impl FnOnce() -> Result<bool, String>) -> Answer {
    Answer { stdout: String::new(), note: None }
}
