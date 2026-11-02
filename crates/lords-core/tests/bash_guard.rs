//! The Bash guard's two refusals past the plain form (design note Harness/notes/2026-11-02-lords-bash-guard.md):
//! a command that would run git without its hooks (I1), and `rm` of an unguarded variable path reached after shell
//! keywords, separators and prefix commands (I2, defect D7). A command that only mentions them passes (I3).

use lords_core::guard::{rm_hook, unguarded_rm};
use serde_json::{json, Value};

/// The guard's denial reason for `command`, or `None` when it lets the call through.
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

fn rm_denied(command: &str) {
    let why = denied(command).unwrap_or_else(|| panic!("let through: {command}"));
    assert!(why.contains("\"${VAR:?}/name\""), "{command}: {why}");
}

fn bypass_denied(command: &str) {
    let why = denied(command).unwrap_or_else(|| panic!("let through: {command}"));
    assert!(why.contains("rule.hooks-run-on-every-commit"), "{command}: {why}");
}

fn passes(command: &str) {
    assert_eq!(denied(command), None, "denied: {command}");
}

// ---------------------------------------------------------------- I2: rm after keywords and prefixes

// Spec: D7 (lords-wakeups.md, 1 Nov): "`if …; then rm -f -- "$D/$n"; fi` passes" while the same `rm` after `x;` is
// denied; a helper then hung 2 h on Claude Code's own prompt. It closes when `rm` after `then`, `do`, `else`, `{`,
// `(`, `!` is found as in the plain form (the brief adds `&&`, `||`, `;`, `|`, and `if`, `elif`, `while`, `until`
// start a command too, by Bash's grammar).
#[test]
fn rm_after_a_shell_keyword_is_checked() {
    for command in [
        "if [ -d \"$D\" ]; then rm -f -- \"$D/$n\"; fi",
        "for f in *.tmp; do rm -- \"$f\"; done",
        "if a; then :; else rm $X; fi",
        "if a; then :; elif rm $X; then :; fi",
        "if rm $X; then :; fi",
        "while rm $X; do :; done",
        "until rm $X; do :; done",
        "! rm $X",
        "{ rm -rf $X; }",
        "(rm -rf $X)",
        "a && rm $X",
        "a || rm $X",
        "a | rm $X",
        "f() { rm -f \"$1\"; }",
        "if a\nthen\n  rm -f $X\nfi",
    ] {
        rm_denied(command);
    }
}

// Spec: D7: "and the prefixes `command`, `exec`, `time`, `nohup`, `env …`, `xargs`" (the brief adds `sudo`), each
// with its options and their values; `timeout DURATION` too, the house form of a deadline
// (rule.remote-calls-have-deadlines). Prefixes chain.
#[test]
fn rm_after_a_prefix_command_is_checked() {
    for command in [
        "command rm $X",
        "command -p rm $X",
        "exec rm $X",
        "exec -a name rm $X",
        "time rm $X",
        "time -p rm $X",
        "nohup rm $X",
        "env rm $X",
        "env -i FOO=1 rm $X",
        "env -u HOME rm $X",
        "env -- rm $X",
        "/usr/bin/env rm $X",
        "xargs rm $X",
        "xargs -0 -n 1 rm -f $X",
        "xargs -I{} rm {} $X",
        "sudo rm $X",
        "sudo -u bob rm $X",
        "sudo -E FOO=1 rm $X",
        "timeout 10 rm $X",
        "timeout -k 5 10 rm $X",
        "timeout --signal=KILL 10 rm $X",
        "sudo --user bob rm \"$X\"",
        "if a; then sudo env FOO=1 nohup timeout 5 rm $X; fi",
    ] {
        rm_denied(command);
    }
}

// Spec: the design note's tokenizer: commands inside `$(…)` and backticks run, quoted or not, so an `rm` there is
// checked; a heredoc fed to a shell or `ssh` runs its body (today's denial of such a body stays).
#[test]
fn rm_inside_a_substitution_or_a_shell_heredoc_is_checked() {
    for command in [
        "echo \"$(rm -rf $X)\"",
        "echo `rm -rf $X`",
        "echo \"`rm -rf $X`\"",
        "x=$(rm $X)",
        "x=$(echo $(rm $X))",
        "rm -f $(ls) $X",
        "bash <<'EOF'\nrm -rf $X\nEOF",
        "ssh host <<EOF\nrm -rf $X\nEOF",
        "sh -s <<-EOF\n\trm -rf $X\n\tEOF",
    ] {
        rm_denied(command);
    }
    // A variable inside a substitution in an `rm` operand counts, as today.
    rm_denied("rm \"$(dirname \"$F\")/x\"");
}

// Spec: the brief: "Every currently allowed form stays allowed, `"${VAR:?}"` included", now in every new position; and
// a prefix that runs nothing (`command -v`, `sudo -l`) runs no `rm`.
#[test]
fn allowed_rm_forms_stay_allowed_after_keywords_and_prefixes() {
    for command in [
        "if [ -d \"${D:?}\" ]; then rm -f -- \"${D:?}/${n:?}\"; fi",
        "for f in a b; do rm -f build/a.o; done",
        "then echo rm $X",
        "echo then rm $X",
        "command -v rm \"$X\"",
        "sudo -l rm $X",
        "xargs -I{} rm -f {}",
        "find . -name '*.tmp' -print0 | xargs -0 rm -f",
        "time rm -rf build",
        "env FOO=1 rm \"${OUT:?}/x\"",
        "timeout 5 rm -f \"${T:?}\"",
        "nohup rm -rf target",
        "rm -rf \"$(mktemp -d)/x\"",
        "rm \"$(pwd)/x\"",
        "x=$((1+(2))); rm -f build/out",
    ] {
        passes(command);
    }
}

// Spec: the design note, I3: a heredoc not fed to a shell is data, so a body that mentions `rm $X` passes; so does
// Claude Code's commit form, a heredoc inside `"$(…)"`.
#[test]
fn a_heredoc_body_that_is_data_passes() {
    for command in [
        "cat > s.sh <<'EOF'\nrm -rf $X\nEOF",
        "cat <<EOF >s.sh\nrm -rf $X\nEOF\nchmod +x s.sh",
        "git commit -F - <<'EOF'\nrm -rf $X was let through\nEOF",
        "git commit -m \"$(cat <<'EOF'\nFix: rm -f -- \"$D/$n\" after then\n\ngit commit --no-verify is refused\nEOF\n)\"",
        "cat <<\\EOF\nrm $X\nEOF",
        "cat <<\"EOF\" <<END\nrm $X\nEOF\nrm $Y\nEND",
    ] {
        passes(command);
    }
    // The heredoc's end: what follows it is a command again, whatever quoting the delimiter has (sol-loop r1, S5).
    rm_denied("cat <<'EOF'\nrm $X\nEOF\nrm $Y");
    rm_denied("cat <<$'EOF'\ndata\nEOF\nrm \"$X\"");
    // An unterminated body is data to its end.
    passes("cat <<EOF\nrm $X");
}

// Spec: review r1, BG-R1-01 (the Lords owner accepted it, 2 Nov): `<<<` is a here-string, not a heredoc. Its one word is
// data, and the command goes on after it, so what follows is checked as usual (Bash manual §3.6.7). The review's
// witnesses.
#[test]
fn a_here_string_is_one_word_of_data() {
    rm_denied("cat <<< \"ready\"\nrm -f \"$OUT/file\"");
    bypass_denied("cat <<< \"ready\"\ngit commit --no-verify -m fix");
    passes("cat <<< 'rm -rf $X'");
    passes("grep -c x <<< \"git commit --no-verify\"");
    // The word is stdin data, never an argument, wherever it stands (sol-loop r3, R2).
    passes("git commit -F - <<< '--no-verify'");
    bypass_denied("git <<<message commit -F - --no-verify");
    rm_denied("echo <<< \"$(rm -rf $X)\"");
    // The word may come after a line continuation; a command that ends first has none (sol-loop r4, Q1).
    bypass_denied("git <<< \\\n  message commit --no-verify");
    rm_denied("cat <<<\nrm -f $X");
}

// Spec: the design note §2: a variable inside a substitution in an `rm` operand counts, and a here-string's word inside
// one is no exception (sol-loop r4, Q2: `cat` hands its value to the path).
#[test]
fn a_here_string_variable_counts_inside_a_substitution() {
    rm_denied("rm -f \"$(cat <<< \"$OUT\")/file\"");
    // Also from the body of a shell heredoc inside it (sol-loop r5, P1).
    rm_denied("rm -f \"$(bash <<'EOF'\ncat <<< \"$OUT\"\nEOF\n)/file\"");
}

// Spec: review r1, BG-R1-02 (accepted): an unquoted heredoc fed to a shell is what that shell runs once the outer shell
// has expanded it. Its escapes `\$`, `` \` ``, `\\` and `\`-newline are resolved first (Bash manual §3.6.6), so `\$X`
// can't hide an unguarded variable. A quoted delimiter keeps the body literal, and data stays data. The review's
// witness and allowances; then `\\$X`, where the outer shell expands `$X` itself, and `\\\$X`, where it doesn't.
#[test]
fn an_unquoted_shell_heredoc_is_checked_as_its_shell_receives_it() {
    rm_denied("bash <<EOF\nrm -f \"\\$OUT/file\"\nEOF");
    rm_denied("ssh build-host <<EOF\nrm -f \"\\$OUT/file\"\nEOF");
    passes("bash <<'EOF'\nrm -f \"\\$OUT/file\"\nEOF");
    passes("bash <<EOF\nrm -f \"\\${OUT:?}/file\"\nEOF");
    passes("cat <<EOF\nrm -f \"\\$OUT/file\"\nEOF");
    rm_denied("bash <<EOF\nrm -rf \\\\$X/\nEOF");
    passes("bash <<EOF\nrm -f \"\\\\\\$OUT/file\"\nEOF");
    // A whole run of backslashes: four before `$` leave the outer shell's expansion; five hand the child an
    // unescaped `$` (sol-loop r3, R1).
    rm_denied("bash <<EOF\nrm -f \"\\\\\\\\$OUT/file\"\nEOF");
    rm_denied("bash <<EOF\nrm -f \"\\\\\\\\\\$OUT/file\"\nEOF");
}

// ---------------------------------------------------------------- I1: git without its hooks

// Spec: the brief: "`--no-verify` or `-n` on `git commit`/`git merge`/`git push`/`git am`" gets a deny naming
// rule.hooks-run-on-every-commit. Git 2.56 (`git <cmd> -h`): `-n` is `--no-verify` for `commit` and `am`; `pull` and
// `rebase` take `--no-verify` too; git accepts a unique prefix of a long option, and options after operands.
#[test]
fn skipping_hooks_by_option_is_denied() {
    for command in [
        "git commit --no-verify -m x",
        "git commit -n -m x",
        "git commit -nm x",
        "git commit -anm x",
        "git commit -am x -n",
        "git commit --no-verif -m x",
        "git commit --no-v -m x",
        "git -C repo commit --no-verify",
        "git -c user.name=x commit -n -m y",
        "git --git-dir .git --work-tree . commit -n",
        "git merge --no-verify topic",
        "git pull --no-verify",
        "git push --no-verify origin main",
        "git push origin main --no-verify",
        "git am -n x.patch",
        "git am --no-verify < p",
        "git rebase --no-verify main",
        "git.exe commit -n -m x",
        "/mingw64/bin/git commit -n",
    ] {
        bypass_denied(command);
    }
}

// Spec: the design note's attack list: quoting and escapes are removed as Bash removes them, a line continuation joins
// the line, and the keywords and prefixes of I2 reach git too.
#[test]
fn quoting_escapes_and_positions_do_not_hide_a_hook_skip() {
    for command in [
        "\"git\" commit \"--no-verify\"",
        "g\\it commit --no\\-verify",
        "git commit $'--no-verify' -m x",
        "git commit \\\n  --no-verify -m x",
        "if true; then git commit -n -m x; fi",
        "timeout 600 git commit --no-verify -m x",
        "cd r && git commit -n -m x",
        "x=$(git commit --no-verify -m y)",
        "echo \"$(git push --no-verify)\"",
        "env FOO=1 git commit -n",
    ] {
        bypass_denied(command);
    }
}

// Spec: the brief: "`-c core.hooksPath=…` on any git call, `git config core.hooksPath …`" (a write: set, unset, add,
// replace), git's key names compared case-insensitively.
#[test]
fn repointing_the_hooks_path_is_denied() {
    for command in [
        "git -c core.hooksPath=/dev/null commit -m x",
        "git -c CORE.HOOKSPATH= commit -m x",
        "git -c core.hooksPath status",
        "git --config-env=core.hooksPath=H commit -m x",
        "git --config-env core.hooksPath=H push",
        "git config core.hooksPath /dev/null",
        "git config --global core.hooksPath x",
        "git config --unset core.hooksPath",
        "git config --local --unset-all core.hooksPath",
        "git config set core.hooksPath x",
        "git config unset core.hooksPath",
        "git config -f .git/config core.hooksPath x",
        "git config --replace-all core.hooksPath x",
        "git config --add core.hooksPath x",
        "git -C repo config core.hookspath .githooks",
    ] {
        bypass_denied(command);
    }
}

// Spec: the brief: "`GIT_CONFIG_*`/`HOOKS`-style environment overrides that set the hooks path, and `HUSKY=0`-style
// skips if you find one in use here" (husky is used in UCP3-GUI): an assignment before a command's word, or an
// `export` operand.
#[test]
fn an_environment_override_of_the_hooks_is_denied() {
    for command in [
        "GIT_CONFIG_COUNT=1 GIT_CONFIG_KEY_0=core.hooksPath GIT_CONFIG_VALUE_0=/dev/null git commit -m x",
        "export GIT_CONFIG_KEY_0=core.hooksPath",
        "env GIT_CONFIG_PARAMETERS=\"'core.hooksPath'='x'\" git commit -m y",
        "HUSKY=0 git commit -m x",
        "HUSKY=0 npm run release",
        "export HUSKY=0",
        "HUSKY=0",
    ] {
        bypass_denied(command);
    }
}

// Spec: the brief: "A command that only reads or mentions those strings never gets a deny: one inside a quoted
// message, a `grep` for them, or `git config --get core.hooksPath`". The value of an option that takes one is a
// mention; so is an operand after `--`, a read of the key, a heredoc body, and an operand of another command.
#[test]
fn mentioning_a_hook_skip_passes() {
    for command in [
        "git commit -m \"never use --no-verify or -n\"",
        "git commit -m --no-verify",
        "git commit -m -nope",
        "git commit --message --no-verify",
        "git commit -F -n",
        "git commit -- -n",
        "git commit -Sn -m x",
        "git commit --verify -m x",
        "git merge --no-verify-signatures topic",
        "grep -rn -- --no-verify .",
        "rg \"core.hooksPath\"",
        "git grep -e --no-verify",
        "git log --grep --no-verify",
        "git log -S core.hooksPath",
        "git config --get core.hooksPath",
        "git config core.hooksPath",
        "git config --global --get core.hooksPath",
        "git config get core.hooksPath",
        "git config --list --show-origin",
        "git config --get-regexp hookspath",
        "git -C r config --get core.hooksPath",
        "git -C core.hooksPath status",
        "echo HUSKY=0",
        "echo GIT_CONFIG_KEY_0=core.hooksPath",
        "printf '%s\\n' \"git commit -n\"",
        "HUSKY=1 git commit -m x",
        "GIT_CONFIG_COUNT=1 GIT_CONFIG_KEY_0=user.name GIT_CONFIG_VALUE_0=x git commit -m y",
        "git -c user.name=x commit -m y",
        "git commit -F - <<'EOF'\ngit commit --no-verify is now refused\nEOF",
        "cat <<EOF\ngit config core.hooksPath x\nEOF",
    ] {
        passes(command);
    }
}

// Spec: git 2.56 `-h`: `-n` is `--no-stat` for merge, pull and rebase and `--dry-run` for push; they skip no hook, so
// they pass (the design note's deviation from the brief).
#[test]
fn n_where_it_skips_no_hook_passes() {
    for command in
        ["git merge -n topic", "git pull -n", "git rebase -n main", "git push -n origin main"]
    {
        passes(command);
    }
}

// Spec: cut 1 (the Lords owner, 2 Nov): option values are known only where `-n` is checked (commit, am). On merge,
// pull, push and rebase a value that is itself `--no-verify` is read as the option: a refusal with its reason.
#[test]
fn an_option_value_is_known_only_where_n_is_checked() {
    bypass_denied("git merge -m --no-verify topic");
    passes("git commit -m --no-verify");
}

// Spec: the design note §4, boundary inputs: commands with nothing to run, and a git option missing its value, are
// let through without a panic.
#[test]
fn boundary_commands_pass() {
    for command in [
        "",
        "then",
        "env",
        "sudo -l",
        "timeout 5",
        "git",
        "git -c",
        "git commit --no-",
        "rm",
        "$(",
        "echo \"$(",
        "`",
        "cat <<",
        "x=$((",
        // Unterminated inside a substitution (sol-loop r1, S1: these panicked).
        "echo $(${X",
        "echo \"$($'x\"",
        "echo $(cat <<\\",
        "echo `${X",
    ] {
        passes(command);
    }
    assert_eq!(unguarded_rm("then rm -f -- \"$D/$n\"").as_deref(), Some("$D/$n"));
}
