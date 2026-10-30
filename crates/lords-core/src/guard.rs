//! Tool-call guards (owner, 30 Oct 2026): checks a harness runs before an agent's tool call, answering at once instead
//! of leaving the call to stop on an approval prompt nobody sees. A Claude child in full-access mode sat about 30
//! minutes on Claude Code's "dangerous rm on a possibly-empty variable path" prompt for `rm -f $OUT/$id.json`; its
//! parent saw only "running".
//!
//! `rm`: a path that holds a shell variable deletes the wrong place when the variable is empty (`rm -rf "$OUT/"` is
//! `rm -rf /`). The only form let through is `"${VAR:?}"`, quoted, which stops the command when `VAR` is empty or
//! unset. Quoting alone (`"$OUT/x"`) doesn't: an empty `OUT` still makes it `/x`, so it is refused too.

use serde_json::{json, Value};

/// What the agent is told to write instead.
pub const RM_FIX: &str = "\"${VAR:?}/name\"";

/// One word of a shell command, as far as the guard needs it: its text and its variable expansions.
#[derive(Debug, Default)]
struct Word {
    text: String,
    /// Each `$` expansion in the word: whether it was inside double quotes, and whether it is `${NAME:?…}`.
    vars: Vec<(bool, bool)>,
}

/// The simple commands of `command`, each as its words (split at `;`, `&`, `|`, `(`, `)` and newlines outside quotes).
fn commands(command: &str) -> Vec<Vec<Word>> {
    let c: Vec<char> = command.chars().collect();
    let (mut out, mut words, mut word) = (Vec::new(), Vec::new(), Word::default());
    let mut started = false;
    let (mut single, mut double) = (false, false);
    let mut i = 0;
    let end_word = |words: &mut Vec<Word>, word: &mut Word, started: &mut bool| {
        if *started {
            words.push(std::mem::take(word));
            *started = false;
        }
    };
    while i < c.len() {
        let ch = c[i];
        if single {
            if ch == '\'' {
                single = false;
            } else {
                word.text.push(ch);
            }
            i += 1;
            continue;
        }
        match ch {
            '\\' if i + 1 < c.len() => {
                if c[i + 1] != '\n' {
                    word.text.push(c[i + 1]);
                    started = true;
                }
                i += 2;
                continue;
            }
            '\'' if !double => {
                single = true;
                started = true;
            }
            // `$'…'`: ANSI-C quoting, a literal with backslash escapes (sol-loop r1, S6).
            '$' if !double && i + 1 < c.len() && c[i + 1] == '\'' => {
                started = true;
                let mut j = i + 2;
                while j < c.len() && c[j] != '\'' {
                    if c[j] == '\\' && j + 1 < c.len() {
                        word.text.push(c[j + 1]);
                        j += 2;
                    } else {
                        word.text.push(c[j]);
                        j += 1;
                    }
                }
                i = j + 1;
                continue;
            }
            // A comment: from an unquoted `#` at a word's start to the line's end (sol-loop r1, S5).
            '#' if !double && !started => {
                while i < c.len() && c[i] != '\n' {
                    i += 1;
                }
                continue;
            }
            '"' => {
                double = !double;
                started = true;
            }
            '$' if i + 1 < c.len()
                && (c[i + 1] == '{'
                    || c[i + 1] == '_'
                    || c[i + 1].is_ascii_alphanumeric()
                    || "@*#?$!-".contains(c[i + 1])) =>
            {
                started = true;
                if c[i + 1] == '{' {
                    let close = c[i..].iter().position(|x| *x == '}').map_or(c.len(), |p| i + p);
                    let inner: String = c[i + 2..close.min(c.len())].iter().collect();
                    let name_end = inner
                        .find(|x: char| !(x == '_' || x.is_ascii_alphanumeric()))
                        .unwrap_or(inner.len());
                    let guarded = name_end > 0 && inner[name_end..].starts_with(":?");
                    word.vars.push((double, guarded));
                    word.text.extend(&c[i..(close + 1).min(c.len())]);
                    i = close + 1;
                } else {
                    let mut j = i + 1;
                    if c[j].is_ascii_alphabetic() || c[j] == '_' {
                        while j < c.len() && (c[j] == '_' || c[j].is_ascii_alphanumeric()) {
                            j += 1;
                        }
                    } else {
                        j += 1;
                    }
                    word.vars.push((double, false));
                    word.text.extend(&c[i..j]);
                    i = j;
                }
                continue;
            }
            ' ' | '\t' if !double => end_word(&mut words, &mut word, &mut started),
            // `&` and `|` that are part of a redirection operator (`2>&1`, `&>`, `>|`) don't end the command: the
            // words after it are still this command's (sol-loop r2, S7–S9).
            '&' | '|'
                if !double
                    && (i > 0 && matches!(c[i - 1], '>' | '<')
                        || ch == '&' && c.get(i + 1) == Some(&'>')) =>
            {
                word.text.push(ch);
                started = true;
            }
            ';' | '&' | '|' | '(' | ')' | '\n' if !double => {
                end_word(&mut words, &mut word, &mut started);
                if !words.is_empty() {
                    out.push(std::mem::take(&mut words));
                }
            }
            _ => {
                word.text.push(ch);
                started = true;
            }
        }
        i += 1;
    }
    end_word(&mut words, &mut word, &mut started);
    if !words.is_empty() {
        out.push(words);
    }
    out
}

/// The first word of an `rm` command holding a variable not written `"${VAR:?}"`, if any. Leading assignments (`X=y`)
/// and the wrappers `sudo`, `command`, `env` and `builtin` are looked through. Every word after `rm` is checked,
/// redirections and option-like words too: an unquoted variable can split into paths anywhere (`rm -$X`), and no
/// parse of them can turn a deletion path into something else. Conservative by design (sol-loop r1 S4, declined; r2
/// S7–S10): a false refusal costs a retry with `"${VAR:?}"`, a missed path a deletion.
pub fn unguarded_rm(command: &str) -> Option<String> {
    for words in commands(command) {
        let mut rest = words.iter().skip_while(|w| {
            let t = &w.text;
            (t.contains('=')
                && !t.starts_with('=')
                && t.split('=')
                    .next()
                    .is_some_and(|n| n.chars().all(|x| x == '_' || x.is_ascii_alphanumeric())))
                || ["sudo", "command", "env", "builtin"].contains(&t.as_str())
        });
        let Some(cmd) = rest.next() else { continue };
        if cmd.text != "rm" && !cmd.text.ends_with("/rm") {
            continue;
        }
        if let Some(w) =
            rest.find(|w| w.vars.iter().any(|(quoted, guarded)| !(*quoted && *guarded)))
        {
            return Some(w.text.clone());
        }
    }
    None
}

/// The guard hook's answer to one `PreToolUse` event (the harness's stdin JSON): a denial naming the fix, or nothing
/// (empty: the call goes on to the harness's own checks). Only Bash calls are looked at; input that isn't such an
/// event is let through, so the guard never stops a call it can't read.
pub fn rm_hook(input: &str) -> String {
    let Ok(ev) = serde_json::from_str::<Value>(input) else { return String::new() };
    if ev["tool_name"] != "Bash" {
        return String::new();
    }
    let Some(command) = ev["tool_input"]["command"].as_str() else { return String::new() };
    match unguarded_rm(command) {
        None => String::new(),
        Some(word) => json!({ "hookSpecificOutput": {
            "hookEventName": "PreToolUse",
            "permissionDecision": "deny",
            "permissionDecisionReason": format!(
                "Lords: `rm` with a variable path ({word}) deletes the wrong place if the variable is empty. Write \
                 each variable as {RM_FIX}: quoted, with `:?`, so an empty one stops the command."
            ),
        } })
        .to_string(),
    }
}
