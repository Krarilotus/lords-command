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

// Spec: sol-loop rm guard r1, S5–S6 (witnesses): a comment and an ANSI-C quoted literal aren't deletion paths and
// don't refuse a literal `rm`; a variable operand after them or after `--` still does.
#[test]
fn comments_and_ansi_c_literals_are_no_paths() {
    for command in ["rm -f build/out # $OUT is unused", "rm -f $'it\\'s$OUT'"] {
        assert_eq!(denied(command), None, "{command}");
    }
    for command in ["rm -- $X", "rm -f build/out $X # note", "rm -f $'a' $B"] {
        assert!(denied(command).is_some(), "let through: {command}");
    }
}

// Spec: sol-loop rm guard r2, S7–S10 (witnesses, each let through by r1's redirection and option handling): every
// word after `rm` that holds a variable is checked, redirections and option-like words included, so no parse of
// redirections or options can turn a deletion path into something else. Conservative by design (r1 S4 declined): a
// false refusal costs a retry with "${VAR:?}"; a missed path costs a deletion.
#[test]
fn no_redirection_or_option_hides_a_variable_path() {
    for command in [
        "rm -f 2>&1 $OUT/x",
        "echo ok 2>&1&& rm -f $OUT/x",
        "rm -f &>log $OUT/x",
        "rm -f >|log $OUT/x",
        "X='f /tmp/victim'; rm -$X",
        "rm -f build/out 2>\"$LOG\"",
        "rm --interactive=\"$MODE\" -- build/out",
    ] {
        assert!(denied(command).is_some(), "let through: {command}");
    }
    for command in ["rm -f build/out 2>/dev/null", "rm -f \"${OUT:?}/x\" 2>\"${LOG:?}\""] {
        assert_eq!(denied(command), None, "{command}");
    }
}

// Spec: sol-loop rm guard r3, S17 and S18 (witnesses): an `rm` after a shell keyword (`then`, `do`, `else`, `!`, …) or
// behind a wrapper's options (`command --`, `sudo -n`, `sudo -u bob`, `timeout 5`) is still an `rm`.
#[test]
fn keywords_and_wrapper_options_dont_hide_rm() {
    for command in [
        "OUT=; if true; then rm -f \"$OUT/x\"; fi",
        "for f in a; do rm -f $f; done",
        "if false; then :; else rm $X; fi",
        "! rm $X",
        "{ rm $X; }",
        "OUT=; command -- rm -f \"$OUT/x\"",
        "sudo -n rm -f $OUT/x",
        "sudo -u bob rm -f $OUT/x",
        "env -i rm $X",
        "timeout 5 rm $X",
        "nohup rm $X",
        "xargs rm $X",
        "OUT=; time -p rm -rf \"$OUT/build\"",
        "time rm $X",
    ] {
        assert!(denied(command).is_some(), "let through: {command}");
    }
    for command in ["sudo -u bob ls $X", "if true; then echo $X; fi", "git rm $X"] {
        assert_eq!(denied(command), None, "{command}");
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
