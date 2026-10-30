//! The `rm` guard (owner, 30 Oct 2026; `guard.rs`).

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
// names the fix: `"${VAR:?}/name"`." The witness that hung a child for 30 minutes, and its kin.
#[test]
fn an_unquoted_variable_path_is_rejected_with_the_fix() {
    for command in [
        "rm -f $OUT/$id.json",
        "rm -rf ${OUT}/build",
        "rm -rf ${OUT:?}/build",
        "cd x && rm -r $DIR",
        "FOO=1 rm $1",
        "sudo rm -rf $TMP/*",
        "/usr/bin/rm $X",
        "true; rm -f a $B c",
    ] {
        let why = denied(command).unwrap_or_else(|| panic!("let through: {command}"));
        assert!(why.contains("\"${VAR:?}/name\""), "{command}: {why}");
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

// Spec: a literal path passes, and so does anything that isn't an `rm` with a variable.
#[test]
fn a_literal_path_and_other_commands_pass() {
    for command in [
        "rm -rf build/out",
        "rm -f '$OUT/x'",
        "echo $OUT && ls $DIR",
        "grep rm $FILE",
        "rmdir $X",
        "git rm --cached x",
    ] {
        assert_eq!(denied(command), None, "{command}");
    }
    let other = json!({"tool_name": "Write", "tool_input": {"file_path": "x"}});
    assert_eq!(rm_hook(&other.to_string()), "");
    assert_eq!(rm_hook("not json"), "");
}

// Spec: sol-loop rm guard r1, S4–S6 (witnesses): only the paths `rm` deletes are checked. A redirection target, an
// option's value, a comment and an ANSI-C quoted literal are not deletion paths and don't refuse a literal `rm`; a
// variable operand after `--` still does.
#[test]
fn only_the_paths_rm_deletes_are_checked() {
    for command in [
        "rm -f build/out 2>\"$LOG\"",
        "rm -f build/out > $LOG 2>&1",
        "rm --interactive=\"$MODE\" -- build/out",
        "rm -f build/out # $OUT is unused",
        "rm -f $'it\\'s$OUT'",
    ] {
        assert_eq!(denied(command), None, "{command}");
    }
    for command in ["rm -- $X", "rm -f build/out $X # note", "rm -f $'a' $B"] {
        assert!(denied(command).is_some(), "let through: {command}");
    }
}

// Spec: the reasoned call (owner, 30 Oct: "a variable inside quotes without `:?` gets your reasoned call"): rejected.
// Quoting stops word splitting, not emptiness: with OUT empty, `rm -rf "$OUT/x"` is `rm -rf /x`. Only `:?` stops that.
#[test]
fn a_quoted_variable_without_the_check_is_rejected() {
    for command in [
        "rm -rf \"$OUT/x\"",
        "rm -rf \"${OUT}/x\"",
        "rm -f \"${OUT:?}/$id\"",
        "rm \"${OUT:-/tmp}/x\"",
    ] {
        assert!(denied(command).is_some(), "let through: {command}");
    }
    assert_eq!(unguarded_rm("rm -f \"${OUT:?}/$id\"").as_deref(), Some("${OUT:?}/$id"));
}
