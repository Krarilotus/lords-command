//! The Bash guard's refusal of git's help on a topic, which Git for Windows opens as an HTML page in the owner's
//! default browser (defect D16; design note Harness/notes/2026-11-02-lords-git-help-guard.md). What git turns into
//! `git help <topic>` is read from its source at v2.56.0 (design note §1); a command that only mentions it passes.

use lords_core::guard::{rm_hook, shell_hook, Shell};
use serde_json::{json, Value};

/// The guard's denial reason for `command` run by `tool` (`Bash` or `PowerShell`), or `None` when it lets it through.
fn denied_in(tool: &str, command: &str) -> Option<String> {
    let input = json!({"hook_event_name": "PreToolUse", "tool_name": tool, "tool_input": {"command": command}});
    let out = rm_hook(&input.to_string());
    if out.is_empty() {
        return None;
    }
    let v: Value = serde_json::from_str(&out).unwrap();
    assert_eq!(v["hookSpecificOutput"]["permissionDecision"], "deny", "{out}");
    Some(v["hookSpecificOutput"]["permissionDecisionReason"].as_str().unwrap().to_string())
}

/// Denied as git help, with the reason the brief asks for: the rule, `-h`, and the docs to grep.
fn help_denied(tool: &str, command: &str) {
    let why = denied_in(tool, command).unwrap_or_else(|| panic!("{tool} let through: {command}"));
    for part in ["rule.no-default-browser", "-h`", "git-doc"] {
        assert!(why.contains(part), "{tool} `{command}`: no {part} in: {why}");
    }
}

fn passes_in(tool: &str, command: &str) {
    assert_eq!(denied_in(tool, command), None, "{tool} denied: {command}");
}

// Spec: D16 (lords-wakeups.md, 2 Nov): "`git help <topic>` and `git <cmd> --help` open the HTML docs in the default
// browser. My coders did it: `git help config`; its `probe2.sh`: `git help hook`; the D14b coder: `git help config`".
// The third witness is that coder's recorded command line. The reason names rule.no-default-browser and the fix (the
// brief: "`git <cmd> -h` prints to the terminal, or grep the doc file").
#[test]
fn the_d16_witnesses_are_denied() {
    for command in [
        "git help config",
        "git help hook",
        "git --version; git help -a 2>/dev/null | grep -i \"^ *hook\"; git hook -h 2>&1 | head -10; git help config 2>/dev/null | grep -n \"hook\\.<\" | head",
    ] {
        help_denied("Bash", command);
    }
}

// Spec: the brief's denied list (`git help <topic>`, `git <cmd> --help`, `git --help <cmd>`, through `git -C <dir>`
// and `git -c k=v`), read against git's source (design note §1): D1 `help` with an operand, whatever its options, also
// after `--`; D2 git's own `--help` or `-h` before an operand (git.c:171, :973; `-h` is git's too); D3 `--help` as the
// first word after any subcommand, an alias or `help` itself (git.c:762). The command word as the Bash guard reads it.
#[test]
fn every_form_git_turns_into_help_on_a_topic_is_denied() {
    for command in [
        "git help -w config",
        "git help -m config",
        "git help -i config",
        "git help -- config",
        "git help config -w",
        "git --help config",
        "git -h config",
        "git -C repo --help config",
        "git commit --help",
        "git help --help",
        "git ci --help",
        "git -C /x -c a.b=c log --help",
        "git --git-dir=.git status --help",
        "git --no-pager help log",
        "git -c help.format=html help config",
        "git.exe help config",
        "/mingw64/bin/git help config",
        "\"C:/Program Files/Git/cmd/git.exe\" help config",
        "\"git\" help config",
        "g\\it help config",
        "git h\\elp config",
        "git 'help' config",
        "git help \"config",
        "git help 2>/dev/null config",
        "git commit --help 2>&1",
    ] {
        help_denied("Bash", command);
    }
}

// Spec: the brief: "including through … a chain (`;`, `&&`, `||`, `|`), `$(…)` and the other forms the Bash guard
// already parses" (its keywords, prefix commands, subshells, backticks, and a heredoc fed to a shell).
#[test]
fn git_help_is_found_wherever_the_bash_guard_finds_a_command() {
    for command in [
        "a; git help x",
        "a && git help x",
        "a || git help x",
        "a | git help x",
        "a & git help x",
        "(git help x)",
        "echo \"$(git help x)\"",
        "echo `git commit --help`",
        "if a; then git help x; fi",
        "for t in a b; do git help $t; done",
        "env GIT_PAGER=cat git help x",
        "X=1 git help x",
        "command git help x",
        "timeout 5 git help x",
        "a\ngit help x",
        "bash <<EOF\ngit help x\nEOF",
    ] {
        help_denied("Bash", command);
    }
}

// Spec: the brief's allowed list: "`git <cmd> -h`; `git help -a`, `git help -g` and `git help --all`, which print
// lists; a bare `git --help`, which prints usage to the terminal" (git-help.adoc: "With no options and no '<command>'
// or '<doc>' given, the synopsis … printed on the standard output"; help.c:736 whatever the format option). A
// redirection is no topic (the D14b line's `git help -a 2>/dev/null`). `--help` later among a builtin's options prints
// usage (parse-options.c:1130); after `--` it is a path; as an option's value it is the value.
#[test]
fn listings_usage_and_short_help_pass() {
    for command in [
        "git commit -h",
        "git -C repo commit -h",
        "git help",
        "git --help",
        "git -h",
        "git help -a",
        "git help -g",
        "git help --all",
        "git help -c",
        "git help --guides",
        "git help --config",
        "git help -a --verbose",
        "git help -w",
        "git --help -a",
        "git help -a 2>/dev/null | grep -i hook",
        "git help -a > commands.txt",
        "git help -g 2>&1",
        "git --help 2>&1 | head",
        "git commit -a --help",
        "git commit -- --help",
        "git commit -m --help",
        "git grep -e --help",
        "git log --grep=--help",
        "git status",
        "git",
        "git -C",
        "git -C repo",
        "git -c",
        "",
    ] {
        passes_in("Bash", command);
    }
}

// Spec: the brief: "Allowed: … any mention in a message, a heredoc body, a here-string or a grep pattern".
#[test]
fn a_mention_passes() {
    for command in [
        "git commit -m \"see git help config\"",
        "git commit -m 'run git commit --help'",
        "echo git help config",
        "printf '%s\\n' \"git --help config\"",
        "grep -n 'git help config' notes.md",
        "grep -rn -E 'git help [a-z]|git [a-z-]+ --help' notes",
        "rg -- '--help' src",
        "rg \"git commit --help\"",
        "cat <<'EOF'\ngit help config\nEOF",
        "git commit -F - <<'EOF'\nUse git help config, not the web page.\ngit help hook\nEOF",
        "git commit -m \"$(cat <<'EOF'\ngit help config\nEOF\n)\"",
        "cat <<< \"git help config\"",
        "# git help config",
        "echo ok # git help config",
    ] {
        passes_in("Bash", command);
    }
}

// Spec: the brief: "probe whether Claude's `PowerShell` tool … reach[es] a PreToolUse hook with the command text. Cover
// each that does, with the same check" (design note §2: it does, `tool_input.command`). Its text is PowerShell's
// (design note §3): `` ` `` escapes, a `\` is literal (so `"C:\tmp\"` ends its string), and the call operator `&`
// runs a quoted path.
#[test]
fn the_powershell_tool_is_checked_as_powershell() {
    for command in [
        "git help config",
        "git help config 2>$null",
        "git commit --help *>&1",
        "& 'C:\\Program Files\\Git\\cmd\\git.exe' help config",
        "& \"C:\\Program Files\\Git\\cmd\\git.exe\" help config",
        "Get-ChildItem \"C:\\tmp\\\"; git help config",
        "$page = $(git help config)",
        "@(git --help config)",
        "Write-Host \"a`\"b\"; git commit --help",
        "git help `\nconfig",
        "git -C C:\\repo --help config",
        "if ($x) { git help config }",
    ] {
        help_denied("PowerShell", command);
    }
}

// Spec: as above, the brief's allowed list in PowerShell's own forms: a here-string (`@'…'@`, `@"…"@`, whose body may
// hold quotes), a block comment, a pattern, a message. The rm check isn't run on PowerShell text: its fix,
// `"${VAR:?}"`, is Bash syntax (design note §3).
#[test]
fn powershell_mentions_and_listings_pass() {
    for command in [
        "git commit -h",
        "git help -a",
        "git --help",
        "git help -a 2>$null | Select-String hook",
        "$m = @'\ndon't run\ngit help config\n'@",
        "$m = @\"\nsay \"git help config\"\n\"@\nWrite-Host $m",
        "<#\ngit help config\n#>\nGet-Date",
        "Select-String -Pattern 'git help config' -Path notes.md",
        "git commit -m \"see git help config\"",
        "Write-Host 'git commit --help'",
        "rm $x",
    ] {
        passes_in("PowerShell", command);
    }
}

// Spec: sol-loop r1 (witnesses, probed with the binary): a redirection touching its argument still leaves the argument
// to git (S1); in PowerShell a CR ends a line (S2), a line comment is data whatever quotes it holds (S3), an assignment
// runs its right-hand side (S4), a script block's braces need no spaces (S5), and `$((…))` is a subexpression, not
// arithmetic (S6). The must-allow forms next to each still pass.
#[test]
fn sol_loop_r1_forms() {
    for command in ["git commit --help>/dev/null", "git commit --help&>/dev/null"] {
        help_denied("Bash", command);
    }
    for command in ["git help -a>commands.txt", "git help -a> commands.txt", "git help -g 2>&1"] {
        passes_in("Bash", command);
    }
    for command in [
        "git commit --help\r\n",
        "# don't\r\nGet-ChildItem \"C:\\tmp\\\"; git help config",
        "$page = git help config",
        "$page=git help config",
        "[string]$page = git help config",
        "if ($true) {git help config}",
        "try { git help config } catch { }",
        "Get-Content topics.txt | ForEach-Object {git help $_}",
        "git -C ${env:TEMP} help config",
        "$page = $((git help config))",
    ] {
        help_denied("PowerShell", command);
    }
    for command in [
        "git help `\r\n-a",
        "# \"quoted\n<#\ngit help config\n#>",
        "# don't run git help config\nGet-Date",
        "$m = @{ a = 'git help config' }",
        "git -C ${env:TEMP} help -a",
        "$n = $((1 + 2))",
    ] {
        passes_in("PowerShell", command);
    }
}

// Spec: sol-loop r2 (witnesses, probed with the binary): only an unquoted, unescaped `<` or `>` is a redirection, and
// one at a word's end takes the next word as its target (S1, S7); in PowerShell an assignment is one only where a
// statement starts, to `$name`, `${name}` or a cast variable (S4, S8).
#[test]
fn sol_loop_r2_forms() {
    for command in ["git -c 'test.value=>' help config", "git -c a.b=\\< help config"] {
        help_denied("Bash", command);
    }
    passes_in("Bash", "git help -a>commands.txt< /dev/null");
    for command in [
        "git -c 'test.value=>' help config",
        "${page} = git help config",
        "Get-Date; [string]$page = git help config",
    ] {
        help_denied("PowerShell", command);
    }
    for command in ["Write-Output $name=git help config", "Write-Output $name = git help config"] {
        passes_in("PowerShell", command);
    }
}

// Spec: review r1, D16-R1-02 (accepted by the Lords owner, 2 Nov): "the right side of a PowerShell assignment is a
// command, wherever the assignment sits": in a quoted `$( … )` (the review's witness), typed (S10) or to several
// targets (S11); a script block in a quoted `$( … )` is code. For the PowerShell tool and for a `Bash` event under
// `--shell powershell`. Literal single-quoted text, an escaped `$` and plain quoted mentions pass.
#[test]
fn powershell_code_in_quoted_subexpressions_and_any_assignment() {
    let denied = |command: &str| {
        ["PowerShell", "Bash"].map(|tool| {
            let ev = json!({"tool_name": tool, "tool_input": {"command": command}});
            shell_hook(&ev.to_string(), Shell::PowerShell).contains("rule.no-default-browser")
        })
    };
    for command in [
        "Write-Host \"help: $($page = git help config; $page)\"",
        "Write-Host \"$(if ($true) {git help config})\"",
        "[string[]]$page = git help config",
        "$a, $b = git help config",
    ] {
        assert_eq!(denied(command), [true, true], "{command}");
    }
    for command in [
        "Write-Host '$($page = git help config)'",
        "Write-Host \"`$($page = git help config)\"",
        "Write-Host \"see git help config\"",
        "Write-Host \"$($env:USERNAME) ran git help config\"",
    ] {
        assert_eq!(denied(command), [false, false], "{command}");
    }
}
