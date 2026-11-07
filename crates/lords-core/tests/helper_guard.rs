//! The helper guard (`lords guard helper`; design note Harness/notes/2026-11-02-lords-native-helper-guard.md) and its
//! T3 test (`t3::session`): in a session T3 started, Claude Code's native helper tool is refused with a line naming
//! `delegate_task`; outside T3, in a CLI started from a T3 agent's shell, and whenever the guard can't tell, the call
//! goes through without a word. Each test names the note's invariants (I) and attacks (A) it covers.

use lords_core::helper_guard::helper_hook;
use lords_core::process::{command_line, split_command_line};
use lords_core::t3;
use serde_json::{json, Value};
use std::cell::Cell;

/// T3's launch of a Claude Code session, as the probe read it on 2 Nov (§5.1, T; paths and ids neutral).
const T3_LAUNCH: &str = r#"C:/tools/claude.exe --output-format stream-json --verbose --input-format stream-json --thinking adaptive --thinking-display summarized --effort xhigh --model claude-opus-5-5 --permission-prompt-tool stdio --allowedTools mcp__t3-code__* --tools default --permission-mode bypassPermissions --allow-dangerously-skip-permissions --include-partial-messages --add-dir C:\work --add-dir C:\work\attachments --session-id=00000000-0000-0000-0000-000000000001 --thinking-display summarized --settings "{\"fastMode\":false,\"showThinkingSummaries\":true}""#;

fn words(line: &str) -> Vec<String> {
    line.split(' ').map(str::to_string).collect()
}

/// `t3::session` for a hook whose `T3CODE_THREAD_ID` is `thread` and whose `CLAUDE_PID` is `pid`, every process
/// launched with `argv`; and the ids of the processes it read.
fn session(
    thread: Option<&str>,
    pid: Option<&str>,
    argv: &[String],
) -> (Result<bool, String>, Vec<u32>) {
    let read = std::cell::RefCell::new(Vec::new());
    let answer = t3::session(thread, pid, |p| {
        read.borrow_mut().push(p);
        Ok(argv.to_vec())
    });
    (answer, read.into_inner())
}

// Spec (design note §5.1, the choice, H6 with H2): T3 iff `T3CODE_THREAD_ID` is set and the process `CLAUDE_PID` names
// was launched with T3's tools in `--allowedTools` (`mcp__t3-code__…`); `claude -h` (2.1.316): "--allowedTools,
// --allowed-tools <tools...> Comma or space-separated list of tool names to allow". A CLI started from a T3 agent's
// shell (N1) inherits the thread but is its own process, launched without them (A2), whatever its prompt, settings or
// permission rules say (A13); a T3 launch without the flag goes quiet (A11).
#[test]
fn a_t3_launch_is_t3_and_a_cli_started_from_its_shell_is_not() {
    let t3_launch = words(
        "C:/tools/claude.exe --output-format stream-json --input-format stream-json --permission-prompt-tool stdio --allowedTools mcp__t3-code__* --tools default --permission-mode bypassPermissions",
    );
    let mut eq_form = t3_launch.clone();
    eq_form.retain(|w| w != "--allowedTools" && w != "mcp__t3-code__*");
    eq_form.push("--allowedTools=mcp__t3-code__*".into());
    let nested = words(
        "C:/tools/claude.exe -p probe --settings C:/scratch/probe.json --max-turns 2 --model claude-haiku-5-5 --effort low --allowedTools Bash Agent --output-format json",
    );
    let mut no_flag = t3_launch.clone();
    no_flag.retain(|w| w != "--allowedTools" && w != "mcp__t3-code__*");
    let s = |v: &[&str]| v.iter().map(|w| w.to_string()).collect::<Vec<_>>();
    let rows: Vec<(&str, Vec<String>, bool)> = vec![
        ("T3's launch", t3_launch, true),
        ("`--allowedTools=`", eq_form, true),
        ("`--allowed-tools`", s(&["claude", "--allowed-tools", "mcp__t3-code__*"]), true),
        (
            "a comma list",
            s(&["claude", "--allowedTools", "Bash,mcp__t3-code__delegate_task"]),
            true,
        ),
        (
            "a space list in one argument",
            s(&["claude", "--allowedTools", "Bash mcp__t3-code__*"]),
            true,
        ),
        (
            "a later value of the list",
            s(&["claude", "--allowedTools", "Bash", "mcp__t3-code__*", "-p"]),
            true,
        ),
        ("N1, a nested `claude -p`", nested, false),
        (
            "a nested prompt naming the tool",
            s(&[
                "claude",
                "-p",
                "use mcp__t3-code__delegate_task",
                "--allowedTools",
                "Bash",
                "Agent",
            ]),
            false,
        ),
        (
            "nested settings naming the tool",
            s(&[
                "claude",
                "-p",
                "x",
                "--settings",
                r#"{"permissions":{"allow":["mcp__t3-code__*"]}}"#,
            ]),
            false,
        ),
        ("a T3 launch without the flag", no_flag, false),
        ("`--allowedTools` with no value", s(&["claude", "-p", "x", "--allowedTools"]), false),
        (
            "the flag's value is the next flag",
            s(&["claude", "--allowedTools", "--tools", "mcp__t3-code__*"]),
            false,
        ),
        // Sol-loop r1, S1 (Claude Code's permission rules, `Tool` or `Tool(specifier)`, its docs: `--allowedTools
        // "Bash(git log *)" "Read"`): a rule's specifier is part of the rule, not a tool.
        (
            "a Bash rule whose command names the tool",
            s(&[
                "claude",
                "-p",
                "x",
                "--allowedTools",
                "Bash(echo mcp__t3-code__delegate_task)",
                "Agent",
            ]),
            false,
        ),
        (
            "a Bash rule with blanks, then T3's tools",
            s(&["claude", "--allowedTools", "Bash(git log *),mcp__t3-code__*"]),
            true,
        ),
        // Sol-loop r1, S2 (`--` ends the options; what follows is the prompt).
        (
            "a prompt after `--`",
            s(&[
                "claude",
                "-p",
                "--allowedTools",
                "Bash",
                "Agent",
                "--",
                "--allowedTools=mcp__t3-code__*",
            ]),
            false,
        ),
        (
            "the flag and its value after `--`",
            s(&["claude", "-p", "--", "--allowedTools", "mcp__t3-code__*"]),
            false,
        ),
    ];
    for (what, argv, want) in rows {
        let (answer, read) = session(Some("thread:delegated-task:x"), Some("4242"), &argv);
        assert_eq!(answer, Ok(want), "{what}: {argv:?}");
        assert_eq!(read, [4242], "{what}: the process CLAUDE_PID names, once");
    }
}

// Spec (design note I1, A1, A12; §5.1: "outside T3 the guard reads nothing more"): with no `T3CODE_THREAD_ID`, or an
// empty one, the session isn't T3's, even where `CLAUDE_PID` names a process T3 launched (a `claude -p` from a T3
// Codex agent's shell carries no thread), and no process is read.
#[test]
fn outside_t3_no_process_is_read() {
    let t3_launch = words("claude --allowedTools mcp__t3-code__*");
    for thread in [None, Some("")] {
        for pid in [None, Some("4242"), Some("x")] {
            let (answer, read) = session(thread, pid, &t3_launch);
            assert_eq!(answer, Ok(false), "{thread:?} {pid:?}");
            assert!(read.is_empty(), "{thread:?} {pid:?}: read {read:?}");
        }
    }
}

// Spec (design note I4, A10): in a session with a T3 thread, a missing `CLAUDE_PID`, one that isn't a process id, or a
// process that can't be read (gone, access denied, no reader on this system) can't be told: an error, never "T3".
#[test]
fn a_session_that_cant_be_read_is_unknown() {
    let argv = words("claude --allowedTools mcp__t3-code__*");
    for pid in [None, Some(""), Some("abc"), Some("-1"), Some("4294967296"), Some(" 42")] {
        let (answer, read) = session(Some("thread:x"), pid, &argv);
        assert!(answer.is_err(), "{pid:?}: {answer:?}");
        assert!(read.is_empty(), "{pid:?}: read {read:?}");
    }
    let answer =
        t3::session(Some("thread:x"), Some("4242"), |_| Err("process 4242 is gone".into()));
    assert!(answer.as_ref().is_err_and(|e| e.contains("4242 is gone")), "{answer:?}");
}

/// The hook's answer to a call of `tool` with `input`, in a session `t3` says is (or isn't) T3's; and how often `t3`
/// was asked.
fn hook(tool: &str, input: Value, t3: Result<bool, String>) -> (lords_core::guard::Answer, u32) {
    let asked = Cell::new(0);
    let ev = json!({"hook_event_name": "PreToolUse", "session_id": "s", "tool_name": tool, "tool_input": input});
    let answer = helper_hook(&ev.to_string(), || {
        asked.set(asked.get() + 1);
        t3.clone()
    });
    (answer, asked.get())
}

// Spec (design note I2, A4, A7; §2: "In a T3 session it refuses with a message that names the tool to use:
// `delegate_task`, plus `rule.t3-subagents` by id"): in T3, `Agent` (Claude Code 2.1.316) and `Task` (earlier) are
// refused whatever their `subagent_type`, in the deny form Claude's PreToolUse honours, with a one-line reason naming
// `delegate_task` and `rule.t3-subagents`.
#[test]
fn in_t3_every_native_helper_start_is_refused_naming_delegate_task() {
    let kinds = [
        Some("general-purpose"),
        Some("Explore"),
        Some("Plan"),
        Some("fork"),
        Some("my-reviewer"),
        None,
    ];
    for tool in ["Agent", "Task"] {
        for kind in kinds {
            for background in [false, true] {
                let mut input =
                    json!({"description": "d", "prompt": "p", "run_in_background": background});
                if let Some(k) = kind {
                    input["subagent_type"] = json!(k);
                }
                let (answer, asked) = hook(tool, input, Ok(true));
                let what = format!("{tool} {kind:?} background {background}");
                assert!(!answer.stdout.is_empty(), "{what}: let through");
                let v: Value = serde_json::from_str(&answer.stdout).unwrap();
                let h = &v["hookSpecificOutput"];
                assert_eq!(h["permissionDecision"], "deny", "{what}: {v}");
                assert_eq!(h["hookEventName"], "PreToolUse", "{what}: {v}");
                let reason = h["permissionDecisionReason"].as_str().unwrap();
                assert!(reason.contains("delegate_task"), "{what}: {reason}");
                assert!(reason.contains("rule.t3-subagents"), "{what}: {reason}");
                assert!(
                    !reason.contains('\n') && !answer.stdout.trim_end().contains('\n'),
                    "{what}: {reason}"
                );
                assert_eq!(answer.note, None, "{what}");
                assert_eq!(asked, 1, "{what}");
            }
        }
    }
}

// Spec (design note I1, A1, A2; §2: "Outside T3 it allows, always and silently"): a helper start in a session that
// isn't T3's (a plain CLI, sol-loop's outside-T3 route, a CLI started from a T3 agent's shell) goes through, nothing on
// stdout, no note.
#[test]
fn outside_t3_a_helper_start_goes_through_silently() {
    for tool in ["Agent", "Task"] {
        let (answer, asked) =
            hook(tool, json!({"subagent_type": "general-purpose", "prompt": "p"}), Ok(false));
        assert_eq!(answer.stdout, "", "{tool}");
        assert_eq!(answer.note, None, "{tool}");
        assert_eq!(asked, 1, "{tool}");
    }
}

// Spec (design note A4, A5, A6): only the native helper-start tool is refused: an MCP tool whose name holds `Agent`,
// T3's own `delegate_task`, `SendMessage` to a running agent, the `Task…` tools, `Workflow`, another case or an unknown
// name all go through in T3, and the T3 test isn't run for them (§5.1: "only for an `Agent`/`Task` call").
#[test]
fn lookalikes_and_workflow_go_through_unasked() {
    for tool in [
        "mcp__x__Agent",
        "mcp__t3-code__delegate_task",
        "SendMessage",
        "TaskStop",
        "TaskCreate",
        "TaskOutput",
        "TaskUpdate",
        "Workflow",
        "agent",
        "task",
        "Subagent",
        "Agent ",
        "Bash",
    ] {
        let (answer, asked) =
            hook(tool, json!({"subagent_type": "general-purpose", "prompt": "p"}), Ok(true));
        assert_eq!(answer.stdout, "", "{tool:?}");
        assert_eq!(asked, 0, "{tool:?}");
    }
}

// Spec (design note I4, A8): empty stdin, text that isn't JSON, no `tool_name` or one that isn't a string: the call
// goes through, and the T3 test isn't run.
#[test]
fn malformed_events_go_through_unasked() {
    for input in [
        "",
        "not json",
        "{}",
        "[]",
        "null",
        r#"{"tool_name": 5}"#,
        r#"{"tool_input": {"subagent_type": "Explore"}}"#,
    ] {
        let asked = Cell::new(0);
        let answer = helper_hook(input, || {
            asked.set(asked.get() + 1);
            Ok(true)
        });
        assert_eq!(answer.stdout, "", "{input:?}");
        assert_eq!(asked.get(), 0, "{input:?}");
    }
}

// Spec (design note I4, A10): when the T3 test can't tell, the helper start goes through, with a note saying why.
#[test]
fn when_t3_cant_be_told_the_call_goes_through_with_a_note() {
    let (answer, asked) = hook("Agent", json!({"prompt": "p"}), Err("no CLAUDE_PID".into()));
    assert_eq!(answer.stdout, "");
    assert!(
        answer.note.as_deref().is_some_and(|n| n.contains("no CLAUDE_PID")),
        "{:?}",
        answer.note
    );
    assert_eq!(asked, 1);
}

// Spec (Microsoft Learn, "Parsing C++ command-line arguments": the examples table and its rules, "Within a quoted
// string, a pair of double quote marks is interpreted as a single escaped double quote mark", and for the program
// name, "parts surrounded by double quote marks ... prevent interpretation of a space or tab character as the end of
// the argument"; claude.exe 2.1.316 reads its own line by these rules where `CommandLineToArgvW` doesn't, design note
// §5.3, probes Q1, Q2): a Windows command line splits into the arguments the started program sees.
#[test]
fn a_windows_command_line_splits_as_its_program_reads_it() {
    let rows: [(&str, &[&str]); 13] = [
        (r#"prog "abc" d e"#, &["prog", "abc", "d", "e"]),
        (r#"prog a\\b d"e f"g h"#, &["prog", r"a\\b", "de fg", "h"]),
        (r#"prog a\\\"b c d"#, &["prog", r#"a\"b"#, "c", "d"]),
        (r#"prog a\\\\"b c" d e"#, &["prog", r"a\\b c", "d", "e"]),
        (r#"prog a"b"" c d"#, &["prog", r#"ab" c d"#]),
        // A doubled quote inside quotes is one quote, and the quotes go on (Q1).
        (r#"prog a"b c"d "" x """ y """" z"#, &["prog", "ab cd", "", "x", "\" y \"\" z"]),
        (r#"prog "a""b" "a"""b" a""b "#, &["prog", "a\"b", "a\"b a\"b "]),
        // Seven quotes: one opens, three pairs are three quotes.
        ("prog \"\"\"\"\"\"\"", &["prog", "\"\"\""]),
        // In the program name a quote only opens and closes, and the name runs to a blank outside them (Q2).
        (r#""C:\a b"c d"#, &[r"C:\a bc", "d"]),
        (r#"C:\a"b c"d e"#, &[r"C:\ab cd", "e"]),
        (r#"C:\x\"y" z"#, &[r"C:\x\y", "z"]),
        (
            r#""C:\Program Files\x\claude.exe" -p "a b""#,
            &[r"C:\Program Files\x\claude.exe", "-p", "a b"],
        ),
        ("prog\t a  \"\"  b ", &["prog", "a", "", "b"]),
    ];
    for (line, want) in rows {
        assert_eq!(split_command_line(line), want, "{line}");
    }
    let argv = split_command_line(T3_LAUNCH);
    assert_eq!(argv[0], "C:/tools/claude.exe");
    let at = argv.iter().position(|a| a == "--allowedTools").unwrap();
    assert_eq!(argv[at + 1], "mcp__t3-code__*");
    assert_eq!(argv.last().unwrap(), r#"{"fastMode":false,"showThinkingSummaries":true}"#);
    assert_eq!(t3::session(Some("thread:x"), Some("1"), |_| Ok(argv)), Ok(true));
}

/// The arguments the real `CommandLineToArgvW` (shell32) reads in `line`: the splitter's oracle, in this test binary
/// only (the Lords owner, 3 Nov: "Production never loads shell32").
#[cfg(windows)]
fn windows_argv(line: &str) -> Vec<String> {
    use windows_sys::Win32::Foundation::LocalFree;
    use windows_sys::Win32::UI::Shell::CommandLineToArgvW;
    let wide: Vec<u16> = line.encode_utf16().chain([0]).collect();
    let mut n = 0;
    // SAFETY: a NUL-ended string that outlives the call; the array it returns holds `n` NUL-ended strings, each read
    // up to its NUL, and is freed once, after the last read.
    unsafe {
        let argv = CommandLineToArgvW(wide.as_ptr(), &mut n);
        assert!(!argv.is_null(), "{line:?}: {}", std::io::Error::last_os_error());
        let args = (0..n as usize)
            .map(|i| {
                let p = *argv.add(i);
                let len = (0..).take_while(|&j| *p.add(j) != 0).count();
                String::from_utf16_lossy(std::slice::from_raw_parts(p, len))
            })
            .collect();
        LocalFree(argv.cast());
        args
    }
}

// Spec (the Lords owner, 3 Nov: "Test your splitter against the real `CommandLineToArgvW`, called from the test binary
// only, over a table of quoting and backslash cases"): every line here splits as `CommandLineToArgvW` splits it. Not
// the empty line, for which it gives this program's own path instead of reading the line, and not the two classes
// where it and the C runtime differ and claude.exe reads its line as the C runtime does (a doubled quote inside
// quotes; a quote in the program name before its end: design note §5.3, Q1, Q2), which the test above holds.
#[cfg(windows)]
#[test]
fn the_splitter_reads_a_line_as_command_line_to_argv_does() {
    let lines = [
        // Backslashes: before a quote, 2n are n and the quote counts, 2n+1 are n and a literal quote; else literal.
        r"prog a\b c\\d \\\e f\",
        r#"prog a\"b c\\"d e" \\\"f\\\\"g h"#,
        r#"prog a\\\\\"b "c\\\\\"d e" \"x"#,
        r#"prog "C:\dir with space\\" x "C:\dir\" y" z"#,
        r#"prog "tail\\\\""#,
        // Quotes: open and close anywhere in an argument; empty and unbalanced ones.
        r#"prog a"b c"d "" x "y z""#,
        r#"prog "open to the end"#,
        r#"prog "a b"c" d"#,
        // Blanks: tabs, runs of them, trailing and leading ones.
        "prog\ta\t\tb  c \t",
        "  prog a",
        // The program name: quoted, with backslashes, with a quote after its end.
        r#""C:\Program Files\x\claude.exe" -p "a b""#,
        r#""C:\x\\" y"#,
        r#"C:\x\y "z"#,
        // Not ASCII, and the launch the probe read.
        "prog \"ä ö\" ü 😀",
        T3_LAUNCH,
    ];
    let differ: Vec<String> = lines
        .iter()
        .map(|l| (l, split_command_line(l), windows_argv(l)))
        .filter(|(_, ours, real)| ours != real)
        .map(|(l, ours, real)| format!("{l}\n  ours {ours:?}\n  real {real:?}"))
        .collect();
    assert!(
        differ.is_empty(),
        "{} of {} lines differ:\n{}",
        differ.len(),
        lines.len(),
        differ.join("\n")
    );
}

// Spec (design note §5.1, the reader: Windows `NtQueryInformationProcess`; §2: on other systems it reads nothing, so
// the guard lets the call through; A10): on Windows this test process's command line reads back as the arguments it
// was started with (std's own reading, `std::env::args`); elsewhere it is an error; a process id no process has is an
// error, never an empty line.
#[test]
fn a_process_command_line_reads_back_as_started() {
    let own = command_line(std::process::id());
    if cfg!(windows) {
        assert_eq!(own, Ok(std::env::args().collect::<Vec<_>>()));
    } else {
        assert!(own.is_err(), "{own:?}");
    }
    assert!(command_line(u32::MAX).is_err());
}
