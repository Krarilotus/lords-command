//! The `rm` guard (owner, 30 Oct 2026; `guard.rs`). Its scope is the plain form only (orchestrator, 30 Oct, after the
//! rm-guard review r1): the command word, after leading `NAME=value` assignments, is `rm` or `rm.exe`, or a path to
//! one; then any operand holding a variable not written `"${VAR:?}"` is refused. Every other form falls back to
//! Claude Code's own check, as before the guard; it is not covered and not tested here as a bypass.

use lords_core::guard::{rm_hook, unguarded_rm};
use serde_json::{json, Value};

fn denied(command: &str) -> Option<String> {
    let input = json!({"hook_event_name": "PreToolUse", "tool_name": "Bash", "tool_input": {"command": command}});
    let out = rm_hook(&input.to_string());
    if out.is_empty() {
        return None;
    }
    let v: Value = serde_json::from_str(&out).unwrap();
    assert_eq!(v["hookSpecificOutput"]["permissionDecision"], "deny", "{out}");
    Some(v["hookSpecificOutput"]["permissionDecisionReason"].as_str().unwrap().to_string())
}

// Spec: owner, 30 Oct: "rejects, at once, an `rm` whose path contains an unquoted shell variable. The rejection message
// names the fix: `"${VAR:?}/name"`." The witness that hung a child for 30 minutes, and its kin in the plain form.
#[test]
fn an_unquoted_variable_path_is_rejected_with_the_fix() {
    for command in [
        "rm -f $OUT/$id.json",
        "rm -rf ${OUT}/build",
        "rm -rf ${OUT:?}/build",
        "cd x && rm -r $DIR",
        "FOO=1 rm $1",
        "/usr/bin/rm $X",
        "true; rm -f a $B c",
        "rm -- $X",
        "rm -f build/out $X # note",
        "rm -f $'a' $B",
        "X='f /tmp/victim'; rm -$X",
    ] {
        let why = denied(command).unwrap_or_else(|| panic!("let through: {command}"));
        assert!(why.contains("\"${VAR:?}/name\""), "{command}: {why}");
    }
}

// Spec: rm-guard review r1, R5 (witnesses `rm.exe -rf "$OUT/build"`, `/usr/bin/rm.exe -rf "$OUT/build"`: the real GNU
// rm on Git Bash): `rm.exe` is `rm`, bare or by path.
#[test]
fn rm_exe_is_rm() {
    for command in ["rm.exe -rf \"$OUT/build\"", "/usr/bin/rm.exe -rf \"$OUT/build\"", "RM.EXE $X"]
    {
        assert!(denied(command).is_some(), "let through: {command}");
    }
}

// Spec: the form the fix names passes: quoted, with `:?`, every variable in the path.
#[test]
fn a_quoted_guarded_variable_passes() {
    for command in [
        "rm -f \"${OUT:?}/${id:?}.json\"",
        "rm -rf \"${OUT:?}\"",
        "rm -rf \"${OUT:?out is unset}/x\"",
    ] {
        assert_eq!(denied(command), None, "{command}");
    }
}

// Spec: a literal path passes, and so does anything whose command word isn't `rm`: a comment, an ANSI-C literal, and
// (rm-guard review r1, R2's false refusal) a command that only names `rm` as data.
#[test]
fn a_literal_path_and_other_commands_pass() {
    for command in [
        "rm -rf build/out",
        "rm -f '$OUT/x'",
        "rm -f build/out # $OUT is unused",
        "rm -f $'it\\'s$OUT'",
        "echo $OUT && ls $DIR",
        "grep rm $FILE",
        "rmdir $X",
        "git rm --cached x",
        "env printf \"%s\\n\" rm \"$OUT\"",
        "sudo -u bob ls $X",
    ] {
        assert_eq!(denied(command), None, "{command}");
    }
    let other = json!({"tool_name": "Write", "tool_input": {"file_path": "x"}});
    assert_eq!(rm_hook(&other.to_string()), "");
    assert_eq!(rm_hook("not json"), "");
}

// Spec: rm-guard fix check r2, R9 (witnesses): a trailing redirection inside a plain `rm` command (`2>&1`, `&>`, `>|`)
// is part of that command, so its operands are still checked; and a redirection target named `rm` is no command.
#[test]
fn trailing_redirections_stay_in_the_rm_command() {
    for command in ["rm -f 2>&1 \"$OUT/x\"", "rm -f &>log \"$OUT/x\"", "rm -f >|log \"$OUT/x\""] {
        assert!(denied(command).is_some(), "let through: {command}");
    }
    assert_eq!(denied("printf '%s\\n' >|rm \"$OUT\""), None);
    assert_eq!(denied("rm -f build/out 2>&1"), None);
}

// Spec: rm-guard fix check r3, R9 (witnesses): an escaped `\>` or `\<` is a literal character, no redirection, so the
// `|` or `&` after it separates commands as Bash has it: the `rm` after it is checked, and a variable after it belongs
// to the next command, not to `rm`.
#[test]
fn an_escaped_angle_bracket_is_no_redirection() {
    for command in
        ["printf \\>|rm \"$OUT\"", "printf \\>&rm \"$OUT\"", "printf \\<|rm.exe \"$OUT\""]
    {
        assert!(denied(command).is_some(), "let through: {command}");
    }
    assert_eq!(denied("rm literal\\>&printf \"%s\" \"$OUT\""), None);
}

// Spec: the reasoned call (owner, 30 Oct: "a variable inside quotes without `:?` gets your reasoned call"): rejected.
// Quoting stops word splitting, not emptiness: with OUT empty, `rm -rf "$OUT/x"` is `rm -rf /x`. Only `:?` stops that.
// Every operand after `rm` is checked, redirection targets and options included (a false refusal costs a retry).
#[test]
fn a_quoted_variable_without_the_check_is_rejected() {
    for command in [
        "rm -rf \"$OUT/x\"",
        "rm -rf \"${OUT}/x\"",
        "rm -f \"${OUT:?}/$id\"",
        "rm \"${OUT:-/tmp}/x\"",
        "rm -f build/out 2>\"$LOG\"",
    ] {
        assert!(denied(command).is_some(), "let through: {command}");
    }
    assert_eq!(unguarded_rm("rm -f \"${OUT:?}/$id\"").as_deref(), Some("${OUT:?}/$id"));
}
