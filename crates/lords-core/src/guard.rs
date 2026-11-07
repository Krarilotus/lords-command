//! Guards a harness runs as hooks (owner, 30 Oct 2026): `rm` before an agent's tool call, answering at once instead
//! of leaving the call to stop on an approval prompt nobody sees; `queue` when an agent's turn would end; `session`
//! and `turn` when a session and a prompt start; `child` when a native child starts; `role` when a prompt names the
//! session's role at its launch (below). A Claude child in full-access mode sat about 30
//! minutes on Claude Code's "dangerous rm on a possibly-empty variable path" prompt for `rm -f $OUT/$id.json`; its
//! parent saw only "running".
//!
//! `rm` (the Bash guard, registered as `guard rm`) refuses three kinds of shell call (design notes
//! Harness/notes/2026-11-02-lords-bash-guard.md and 2026-11-02-lords-git-help-guard.md; a command that only mentions
//! them is let through); the third alone also in PowerShell text (Claude Code's PowerShell tool, Codex on Windows):
//!
//! - An `rm` whose path holds a shell variable: it deletes the wrong place when the variable is empty (`rm -rf
//!   "$OUT/"` is `rm -rf /`). The only form let through is `"${VAR:?}"`, quoted, which stops the command when `VAR`
//!   is empty or unset. Quoting alone (`"$OUT/x"`) doesn't: an empty `OUT` still makes it `/x`, so it is refused too.
//! - A git call that would run without its hooks (rule.hooks-run-on-every-commit): an option that skips them, a
//!   `core.hooksPath` set on the command line or written by `git config`, or an environment override of them.
//! - Git's help on a topic (`git help X`, `git --help X`, `git <cmd> --help`), which Git for Windows opens as a web page
//!   in the owner's default browser (rule.no-default-browser; defect D16). Listings and usage pass.
//!
//! Both find a command's word the same way: past assignments, the keywords that open a command (`then`, `do`, `!`,
//! `{` …), and the prefix commands that run the command after them (`env`, `sudo`, `timeout` …), with their options;
//! inside `$(…)` and backticks too, and in a heredoc fed to a shell (defect D7: an `rm` after `then` used to go on to
//! Claude Code's own prompt, which a delegated child can't answer). Not covered: strings another program runs (`sh
//! -c`, `eval`, `trap`, `env -S`, `git rebase --exec`), git aliases, options in a variable, leading redirections, other
//! spellings of a name (`$'\x72m'`), `find -delete`. A miss is no regression: the call goes on to the harness's checks.

use crate::fsguard::{Contained, Part, Root};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// What the agent is told to write instead.
pub const RM_FIX: &str = "\"${VAR:?}/name\"";

/// One word of a shell command, as far as the guard needs it: its text and its variable expansions.
#[derive(Debug, Default)]
struct Word {
    text: String,
    /// Each `$` expansion in the word: whether it was inside double quotes, and whether it is `${NAME:?…}`.
    vars: Vec<(bool, bool)>,
    /// Where in `text` the word's first redirection operator is: its first `<` or `>` neither quoted nor escaped.
    redirect: Option<usize>,
}

/// The simple command being read: its words so far and the word in progress. After `<<<` (`data`), the next word to
/// start is the here-string's data, no argument; its variables are kept apart (`dropped`), for an enclosing
/// substitution still counts them.
#[derive(Default)]
struct Simple {
    words: Vec<Word>,
    word: Word,
    started: bool,
    data: bool,
    dropped: Vec<(bool, bool)>,
}

impl Simple {
    fn end_word(&mut self) {
        if self.started {
            let word = std::mem::take(&mut self.word);
            if std::mem::take(&mut self.data) {
                self.dropped.extend(word.vars);
            } else {
                self.words.push(word);
            }
            self.started = false;
        }
    }

    fn end(&mut self, out: &mut Vec<Vec<Word>>) {
        self.end_word();
        self.data = false;
        if !self.words.is_empty() {
            out.push(std::mem::take(&mut self.words));
        }
    }
}

/// A heredoc waiting for its body: the delimiter as written without its quotes, whether `<<-` strips leading tabs,
/// whether the command it feeds runs the body (`runs_body`), and whether any of the delimiter was quoted (the body is
/// then literal, else the outer shell expands it).
struct Heredoc {
    delim: String,
    tabs: bool,
    runs: bool,
    quoted: bool,
}

/// The simple commands of `command`, each as its words, as Bash would run them (design note §2): split at `;`, `&`,
/// `|`, `(`, `)` and newlines outside quotes; the commands inside `$(…)` and backticks are commands of their own; a
/// heredoc's body is skipped as data, unless the command it feeds runs it.
fn commands(command: &str) -> Vec<Vec<Word>> {
    let c: Vec<char> = command.chars().collect();
    let mut out = Vec::new();
    list(&c, &mut 0, None, &mut out);
    out
}

/// Reads commands from `c[*i]` into `out`, to the end or, inside a substitution, to its `close` (`)` or a backtick)
/// outside any quotes and subshells it opened; `*i` is then past it. Returns the variables of the here-string words
/// it read, which are no command's words.
fn list(
    c: &[char],
    i: &mut usize,
    close: Option<char>,
    out: &mut Vec<Vec<Word>>,
) -> Vec<(bool, bool)> {
    let mut s = Simple::default();
    let (mut single, mut double) = (false, false);
    // Whether the character before this one was an active redirection operator (`>` or `<`, unquoted and unescaped):
    // a literal one (`\>`, `">"`) is word text and starts no redirection (rm-guard fix check r3, R9).
    let mut after_op = false;
    // The subshells open in this substitution: a `)` closes one of them before it closes the substitution.
    let mut depth = 0usize;
    let mut heredocs: Vec<Heredoc> = Vec::new();
    while *i < c.len() {
        let ch = c[*i];
        let op_before = std::mem::take(&mut after_op);
        if single {
            if ch == '\'' {
                single = false;
            } else {
                s.word.text.push(ch);
            }
            *i += 1;
            continue;
        }
        if Some(ch) == close && !double && (ch == '`' || depth == 0) {
            *i += 1;
            break;
        }
        match ch {
            '\\' if *i + 1 < c.len() => {
                if c[*i + 1] != '\n' {
                    s.word.text.push(c[*i + 1]);
                    s.started = true;
                }
                *i += 2;
                continue;
            }
            '\'' if !double => {
                single = true;
                s.started = true;
            }
            // `$'…'`: ANSI-C quoting, a literal with backslash escapes (sol-loop r1, S6).
            '$' if !double && c.get(*i + 1) == Some(&'\'') => {
                s.started = true;
                let mut j = *i + 2;
                while j < c.len() && c[j] != '\'' {
                    if c[j] == '\\' && j + 1 < c.len() {
                        s.word.text.push(c[j + 1]);
                        j += 2;
                    } else {
                        s.word.text.push(c[j]);
                        j += 1;
                    }
                }
                *i = j + 1;
                continue;
            }
            // A comment: from an unquoted `#` at a word's start to the line's end (sol-loop r1, S5).
            '#' if !double && !s.started => {
                while *i < c.len() && c[*i] != '\n' {
                    *i += 1;
                }
                continue;
            }
            '"' => {
                double = !double;
                s.started = true;
            }
            // `$((…))`: arithmetic, which runs no command.
            '$' if c.get(*i + 1) == Some(&'(') && c.get(*i + 2) == Some(&'(') => {
                let (start, mut open) = (*i, 0usize);
                *i += 1;
                while *i < c.len() {
                    open = match c[*i] {
                        '(' => open + 1,
                        ')' => open.saturating_sub(1),
                        _ => open,
                    };
                    *i += 1;
                    if open == 0 {
                        break;
                    }
                }
                s.word.text.extend(&c[start..*i]);
                s.started = true;
                continue;
            }
            // `$(…)` and backticks, quoted or not: their commands run too.
            '$' | '`' if ch == '`' || c.get(*i + 1) == Some(&'(') => {
                let start = *i;
                *i += if ch == '`' { 1 } else { 2 };
                substitution(c, i, start, if ch == '`' { '`' } else { ')' }, &mut s.word, out);
                s.started = true;
                continue;
            }
            '$' if *i + 1 < c.len()
                && (c[*i + 1] == '{'
                    || c[*i + 1] == '_'
                    || c[*i + 1].is_ascii_alphanumeric()
                    || "@*#?$!-".contains(c[*i + 1])) =>
            {
                s.started = true;
                if c[*i + 1] == '{' {
                    let end = c[*i..].iter().position(|x| *x == '}').map_or(c.len(), |p| *i + p);
                    let inner: String = c[*i + 2..end.min(c.len())].iter().collect();
                    let name_end = inner
                        .find(|x: char| !(x == '_' || x.is_ascii_alphanumeric()))
                        .unwrap_or(inner.len());
                    let guarded = name_end > 0 && inner[name_end..].starts_with(":?");
                    s.word.vars.push((double, guarded));
                    s.word.text.extend(&c[*i..(end + 1).min(c.len())]);
                    *i = end + 1;
                } else {
                    let mut j = *i + 1;
                    if c[j].is_ascii_alphabetic() || c[j] == '_' {
                        while j < c.len() && (c[j] == '_' || c[j].is_ascii_alphanumeric()) {
                            j += 1;
                        }
                    } else {
                        j += 1;
                    }
                    s.word.vars.push((double, false));
                    s.word.text.extend(&c[*i..j]);
                    *i = j;
                }
                continue;
            }
            // `<<<`: a here-string. Its one word is stdin data, never an argument (a substitution in it is still
            // read); no body follows, and the command goes on (review r1, BG-R1-01; sol-loop r3, R2).
            '<' if !double && c[*i..].starts_with(&['<', '<', '<']) => {
                s.end_word();
                s.data = true;
                *i += 3;
                continue;
            }
            // A heredoc (`<<`, `<<-`): its body starts after this line.
            '<' if !double && c.get(*i + 1) == Some(&'<') => {
                s.end_word();
                *i += 2;
                heredocs.push(heredoc(c, i, runs_body(&s.words)));
                continue;
            }
            ' ' | '\t' if !double => s.end_word(),
            // `&` and `|` that are part of a trailing redirection (`2>&1`, `&>`, `>|`) don't end the command: what
            // follows is still this command's (rm-guard fix check r2, R9).
            '&' | '|' if !double && (op_before || ch == '&' && c.get(*i + 1) == Some(&'>')) => {
                s.word.text.push(ch);
                s.started = true;
            }
            ';' | '&' | '|' | '(' | ')' | '\n' if !double => {
                match ch {
                    '(' => depth += 1,
                    ')' => depth = depth.saturating_sub(1),
                    _ => {}
                }
                s.end(out);
                if ch == '\n' && !heredocs.is_empty() {
                    *i += 1;
                    for h in std::mem::take(&mut heredocs) {
                        let dropped = heredoc_body(c, i, &h, out);
                        s.dropped.extend(dropped);
                    }
                    continue;
                }
            }
            _ => {
                after_op = !double && matches!(ch, '>' | '<');
                if after_op && s.word.redirect.is_none() {
                    s.word.redirect = Some(s.word.text.len());
                }
                s.word.text.push(ch);
                s.started = true;
            }
        }
        *i += 1;
    }
    s.end(out);
    // An unterminated `${`, `$'` or delimiter steps past the end; the caller's slice must not (sol-loop r1, S1).
    *i = (*i).min(c.len());
    s.dropped
}

/// A substitution opened at `c[start]`, read from `c[*i]` to past its `close`: its commands go to `out`; the word it
/// stands in gets its text and every variable in it (so `rm "$(dirname "$F")/x"` counts `$F`, as before).
fn substitution(
    c: &[char],
    i: &mut usize,
    start: usize,
    close: char,
    word: &mut Word,
    out: &mut Vec<Vec<Word>>,
) {
    let first = out.len();
    let dropped = list(c, i, Some(close), out);
    word.text.extend(&c[start..*i]);
    word.vars.extend(out[first..].iter().flatten().flat_map(|w| w.vars.iter().copied()));
    word.vars.extend(dropped);
}

/// A heredoc's delimiter, read from `c[*i]` just past `<<`: quotes and backslashes removed, as Bash compares it.
fn heredoc(c: &[char], i: &mut usize, runs: bool) -> Heredoc {
    let tabs = c.get(*i) == Some(&'-');
    *i += usize::from(tabs);
    while matches!(c.get(*i), Some(' ' | '\t')) {
        *i += 1;
    }
    let (mut delim, mut quote, mut quoted) = (String::new(), None, false);
    while let Some(&ch) = c.get(*i) {
        match quote {
            Some(q) if ch == q => quote = None,
            Some(_) => delim.push(ch),
            None if ch == '\'' || ch == '"' => (quote, quoted) = (Some(ch), true),
            // `$'EOF'` and `$"EOF"`: the `$` is quoting, not part of the delimiter (sol-loop r1, S5).
            None if ch == '$' && matches!(c.get(*i + 1), Some('\'' | '"')) => {}
            None if ch == '\\' => {
                *i += 1;
                delim.extend(c.get(*i));
                quoted = true;
            }
            None if " \t\n;&|()<>".contains(ch) => break,
            None => delim.push(ch),
        }
        *i += 1;
    }
    Heredoc { delim, tabs, runs, quoted }
}

/// An unquoted heredoc's body as the shell it feeds receives it, the outer shell's escapes resolved (Bash manual
/// §3.6.6; review r1, BG-R1-02), a whole run of backslashes at a time (sol-loop r3, R1): each `\\` is one backslash,
/// and an odd last one quotes a `$`, `` ` `` or newline after it (the bare character; the newline goes) and is kept
/// before anything else. Expansions are left as written, to be checked as the commands they are part of. One the
/// outer shell runs, after an even run, gets an even run, so the lexer reads it as an expansion too.
fn as_received(body: &[char]) -> Vec<char> {
    let mut out = Vec::with_capacity(body.len());
    let mut k = 0;
    while k < body.len() {
        let n = body[k..].iter().take_while(|c| **c == '\\').count();
        if n == 0 {
            out.push(body[k]);
            k += 1;
            continue;
        }
        let next = body.get(k + n).copied();
        let kept = match next {
            Some('$' | '`' | '\n') if n % 2 == 1 => n / 2,
            Some('$' | '`') => (n / 2).next_multiple_of(2),
            _ => n / 2 + n % 2,
        };
        out.extend(std::iter::repeat_n('\\', kept));
        k += n + usize::from(next == Some('\n') && n % 2 == 1);
    }
    out
}

/// A heredoc's body, from `c[*i]` at a line's start to past the line that is its delimiter (or the end). Data, skipped,
/// unless the command it feeds runs it: then its commands go to `out`, read as that command receives them, and the
/// variables of its here-string words are returned, as `list` returns them (sol-loop r5, P1).
fn heredoc_body(
    c: &[char],
    i: &mut usize,
    h: &Heredoc,
    out: &mut Vec<Vec<Word>>,
) -> Vec<(bool, bool)> {
    let mut body = Vec::new();
    while *i < c.len() {
        let end = c[*i..].iter().position(|x| *x == '\n').map_or(c.len(), |p| *i + p);
        let mut line = &c[*i..end];
        while h.tabs && line.first() == Some(&'\t') {
            line = &line[1..];
        }
        *i = (end + 1).min(c.len());
        if line.iter().collect::<String>().trim_end_matches('\r') == h.delim {
            break;
        }
        body.extend(line);
        body.push('\n');
    }
    if !h.runs {
        return Vec::new();
    }
    let body = if h.quoted { body } else { as_received(&body) };
    list(&body, &mut 0, None, out)
}

/// The commands that run a heredoc's body: shells, and `ssh`, which gives it to the remote shell.
const BODY_RUNNERS: [&str; 6] = ["sh", "bash", "dash", "zsh", "ksh", "ssh"];

fn runs_body(words: &[Word]) -> bool {
    command_at(words).is_some_and(|k| BODY_RUNNERS.contains(&base_name(&words[k].text).as_str()))
}

/// A command's name as the guard compares it: its last path part, lower-case, without `.exe` (rm-guard review r1,
/// R5: `rm.exe` is the same GNU rm on Git Bash).
fn base_name(word: &str) -> String {
    let name = word.rsplit(['/', '\\']).next().unwrap_or(word).to_ascii_lowercase();
    name.strip_suffix(".exe").map(str::to_string).unwrap_or(name)
}

/// Whether `t` is a `NAME=value` assignment.
fn is_assignment(t: &str) -> bool {
    t.split_once('=').is_some_and(|(n, _)| {
        !n.is_empty() && n.chars().all(|x| x == '_' || x.is_ascii_alphanumeric())
    })
}

/// The reserved words after which a command starts (Bash's grammar): its word comes next.
const KEYWORDS: [&str; 9] = ["if", "then", "else", "elif", "do", "while", "until", "!", "{"];

/// A program's options as the guard reads them: the short ones that take a value (the rest of their cluster, or the
/// next word), those whose value can only be attached (the rest of the cluster, `-S<key>`), the long ones that take a
/// value (`--name value`; `--name=value` is one word), and the short ones the guard looks for (`hit`).
struct Opts {
    short: &'static str,
    attached: &'static str,
    long: &'static [&'static str],
    hit: &'static str,
}

impl Opts {
    /// How option word `w` reads: whether its value is the next word, and whether it holds a `hit` letter.
    fn read(&self, w: &str) -> (bool, bool) {
        if let Some(l) = w.strip_prefix("--") {
            return (!l.contains('=') && self.long.contains(&l), false);
        }
        let cluster = w.strip_prefix('-').unwrap_or(w);
        for (k, ch) in cluster.char_indices() {
            if self.hit.contains(ch) {
                return (false, true);
            }
            if self.short.contains(ch) {
                return (k + 1 == cluster.len(), false);
            }
            if self.attached.contains(ch) || !ch.is_ascii_alphanumeric() {
                break;
            }
        }
        (false, false)
    }
}

/// A prefix command, which runs the command after its options (design note I2): its options (a `hit` letter means it
/// runs none: `command -v`, `sudo -l`), and whether an operand comes before the command (`timeout`'s duration).
struct Prefix {
    name: &'static str,
    opts: Opts,
    operand: bool,
}

const fn prefix(
    name: &'static str,
    short: &'static str,
    long: &'static [&'static str],
    hit: &'static str,
    operand: bool,
) -> Prefix {
    Prefix { name, opts: Opts { short, attached: "", long, hit }, operand }
}

/// The prefix commands and their options that take a value (GNU coreutils, findutils, sudo, Bash's `time`).
const PREFIXES: [Prefix; 8] = [
    prefix("command", "", &[], "vV", false),
    prefix("exec", "a", &[], "", false),
    prefix("time", "fo", &["format", "output"], "", false),
    prefix("nohup", "", &[], "", false),
    prefix("env", "uCS", &["unset", "chdir", "split-string"], "", false),
    prefix("xargs", "adEILnPs", &[], "", false),
    prefix(
        "sudo",
        "ugCDprtTU",
        &["user", "group", "chdir", "host", "prompt", "role", "type", "other-user", "close-from"],
        "elvVK",
        false,
    ),
    prefix("timeout", "ks", &["kill-after", "signal"], "", true),
];

/// Where a simple command's own word is: past `NAME=value` assignments, the keywords that open a command, and the
/// prefix commands with their options and operands. `None` where it runs no command (only assignments, `command -v`).
fn command_at(words: &[Word]) -> Option<usize> {
    let mut i = 0;
    loop {
        let w = words.get(i)?.text.as_str();
        if is_assignment(w) || KEYWORDS.contains(&w) {
            i += 1;
            continue;
        }
        let name = base_name(w);
        let Some(p) = PREFIXES.iter().find(|p| p.name == name) else { return Some(i) };
        i += 1;
        while let Some(o) = words.get(i).map(|w| w.text.as_str()).filter(|o| o.starts_with('-')) {
            i += 1;
            if o == "--" {
                break;
            }
            let (value, hit) = p.opts.read(o);
            if hit {
                return None;
            }
            i += usize::from(value);
        }
        i += usize::from(p.operand);
    }
}

/// The first operand of an `rm` command holding a variable not written `"${VAR:?}"`, if any (the module's scope).
/// Every operand is checked, redirection targets and options included: a false refusal costs a retry.
pub fn unguarded_rm(command: &str) -> Option<String> {
    commands(command).into_iter().find_map(|words| {
        let at = command_at(&words).filter(|&k| base_name(&words[k].text) == "rm")?;
        let unguarded = |w: &&Word| w.vars.iter().any(|(quoted, guarded)| !(*quoted && *guarded));
        words[at + 1..].iter().find(unguarded).map(|w| w.text.clone())
    })
}

/// The git subcommands that take `--no-verify` (git 2.56, `git <cmd> -h`).
const HOOK_SKIPPERS: [&str; 6] = ["commit", "am", "merge", "pull", "push", "rebase"];

/// Where `-n` is `--no-verify` too (it is `--no-stat` or `--dry-run` elsewhere), with the options that take a value,
/// so a value (`-m -nope`) is never read as an option. On the other four, a value that is itself `--no-verify` is read
/// as the option (cut 1, the Lords owner, 2 Nov).
const DASH_N: [(&str, Opts); 2] = [
    ("commit", Opts { short: "FmcCtU", attached: "Su", long: &["message", "file"], hit: "n" }),
    ("am", Opts { short: "Cp", attached: "S", long: &[], hit: "n" }),
];

const NO_OPTS: Opts = Opts { short: "", attached: "", long: &[], hit: "" };

/// Git's own options that take the next word as their value (git 2.56, `git -h`).
const GIT_VALUE_OPTIONS: [&str; 7] =
    ["-C", "-c", "--git-dir", "--work-tree", "--namespace", "--config-env", "--attr-source"];

fn is_hooks_path(key: &str) -> bool {
    key.eq_ignore_ascii_case("core.hooksPath")
}

/// Whether assignment `w` turns git's hooks off or repoints them: husky's `HUSKY=0`, or git config from the
/// environment that sets `core.hooksPath` (`GIT_CONFIG_KEY_<n>`, `GIT_CONFIG_PARAMETERS`).
fn overrides_hooks(w: &str) -> bool {
    let Some((name, value)) = w.split_once('=').filter(|_| is_assignment(w)) else { return false };
    let numbered = |n: &str| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit());
    match name {
        "HUSKY" => value == "0",
        "GIT_CONFIG_PARAMETERS" => value.to_ascii_lowercase().contains("core.hookspath"),
        _ => name.strip_prefix("GIT_CONFIG_KEY_").is_some_and(numbered) && is_hooks_path(value),
    }
}

/// Whether `git config`'s arguments write `core.hooksPath`: set it (`set`, or the old form's two operands), or
/// `--unset`, `--unset-all`, `--add`, `--replace-all` or `unset` it. A read (`--get`, one operand, `get`) never does.
fn config_writes_hooks_path(args: &[Word]) -> bool {
    const VALUE: [&str; 9] =
        ["-f", "--file", "--blob", "-t", "--type", "--default", "--comment", "--value", "--url"];
    const WRITE: [&str; 4] = ["--unset", "--unset-all", "--add", "--replace-all"];
    let (mut ops, mut write, mut other) = (Vec::new(), false, false);
    let mut i = 0;
    while let Some(w) = args.get(i).map(|w| w.text.as_str()) {
        i += 1;
        if w == "--" {
            ops.extend(args[i..].iter().map(|w| w.text.as_str()));
            break;
        }
        if !w.starts_with('-') {
            ops.push(w);
        }
        i += usize::from(VALUE.contains(&w));
        write |= WRITE.contains(&w);
        // The old form's reads and its editor: none writes a key's value.
        other |= w.starts_with("--get") || matches!(w, "-l" | "--list" | "-e" | "--edit");
    }
    match ops.as_slice() {
        ["set" | "unset", key, ..] => is_hooks_path(key),
        [key, rest @ ..] if is_hooks_path(key) => write || (!other && !rest.is_empty()),
        _ => false,
    }
}

/// The word by which a simple command would run git without its hooks, if any (design note I1).
fn skips_hooks(words: &[Word]) -> Option<String> {
    let at = command_at(words);
    // An environment override: an assignment before the command's word, or an operand of `export`.
    let before = &words[..at.unwrap_or(words.len())];
    let exported = at.filter(|&k| words[k].text == "export").map_or(&[][..], |k| &words[k + 1..]);
    if let Some(w) = before.iter().chain(exported).find(|w| overrides_hooks(&w.text)) {
        return Some(w.text.clone());
    }
    let args = &words[at.filter(|&k| base_name(&words[k].text) == "git")? + 1..];
    let mut i = 0;
    while let Some(w) = args.get(i).map(|w| w.text.as_str()).filter(|w| w.starts_with('-')) {
        i += 1;
        let value = if let Some(v) = w.strip_prefix("--config-env=") {
            Some(v)
        } else if GIT_VALUE_OPTIONS.contains(&w) {
            i += 1;
            args.get(i - 1).map(|v| v.text.as_str()).filter(|_| w == "-c" || w == "--config-env")
        } else {
            None
        };
        if let Some(v) = value.filter(|v| is_hooks_path(v.split('=').next().unwrap_or(v))) {
            return Some(v.to_string());
        }
    }
    let sub = args.get(i)?.text.as_str();
    let rest = &args[i + 1..];
    if sub == "config" {
        return config_writes_hooks_path(rest).then(|| "git config core.hooksPath".to_string());
    }
    if !HOOK_SKIPPERS.contains(&sub) {
        return None;
    }
    let opts = DASH_N.iter().find(|(name, _)| *name == sub).map_or(&NO_OPTS, |(_, o)| o);
    let mut j = 0;
    while let Some(w) = rest.get(j).map(|w| w.text.as_str()) {
        j += 1;
        if w == "--" {
            break;
        }
        if !w.starts_with('-') {
            continue;
        }
        // `--no-verify` or a prefix git takes for it (it accepts a long option's unique abbreviation).
        let no_verify = w.len() >= "--no-v".len() && "--no-verify".starts_with(w);
        let (value, hit) = opts.read(w);
        if no_verify || hit {
            return Some(w.to_string());
        }
        j += usize::from(value);
    }
    None
}

/// The word by which `command` would run git without its hooks, if any (design note I1).
pub fn hook_bypass(command: &str) -> Option<String> {
    commands(command).iter().find_map(|words| skips_hooks(words))
}

/// The language of a shell call's text (`lords guard rm --shell`; design note 2026-11-02-lords-git-help-guard §2):
/// Claude Code's Bash tool runs Bash and its PowerShell tool PowerShell; Codex sends its shell commands as `Bash`, run
/// by PowerShell on Windows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Shell {
    Bash,
    PowerShell,
}

/// PowerShell text as the lexer above reads it (design note §3): `` ` `` escapes the next character, as `\` does in
/// Bash, and a `\` is a literal; a here-string (`@'` or `@"` ending its line, to a line opening with `'@` or `"@`) and
/// a `<# … #>` comment are data, an empty word. Single-quoted text stays as it is. Sol-loop r1, S2-S6: a CR ends a
/// line, a `#` comment is data, assignments and braces start commands, `$((…))` is a subexpression.
fn powershell_as_bash(text: &str) -> String {
    let c: Vec<char> = text.replace("\r\n", "\n").replace('\r', "\n").chars().collect();
    let (mut out, mut i, mut single, mut double) = (String::new(), 0, false, false);
    // The open `$( … )`s: the string state each was opened in, and its open parentheses.
    let mut subs: Vec<(bool, usize)> = Vec::new();
    while let Some(&ch) = c.get(i) {
        i += 1;
        if single {
            single = ch != '\'';
            out.push(ch);
            continue;
        }
        let token_start = !double
            && out.chars().last().is_none_or(|p| p.is_whitespace() || ";|&(){}".contains(p));
        match ch {
            '#' if token_start => {
                i = c[i..].iter().position(|x| *x == '\n').map_or(c.len(), |p| i + p)
            }
            // `${name}`: a variable, braces and all.
            '$' if !double && c.get(i) == Some(&'{') => {
                let end = c[i..].iter().position(|x| *x == '}').map_or(c.len(), |p| i + p + 1);
                out.extend(&c[i - 1..end]);
                i = end;
            }
            '{' | '}' if !double => out.push(';'),
            // `$( … )` is code, in a string too, to its `)` (review r1, D16-R1-02); `$((` holds a group.
            '$' if c.get(i) == Some(&'(') => {
                out.push_str(if c.get(i + 1) == Some(&'(') { "$( " } else { "$(" });
                subs.push((double, 0));
                (double, i) = (false, i + 1);
            }
            '(' | ')' if !double && !subs.is_empty() => {
                let open = &mut subs.last_mut().expect("not empty").1;
                match (ch, *open) {
                    (')', 0) => double = subs.pop().expect("not empty").0,
                    (')', n) => *open = n - 1,
                    _ => *open += 1,
                }
                out.push(ch);
            }
            '=' if !double && assigns(&out) => out.push_str("=;"),
            '`' => {
                out.push('\\');
                out.extend(c.get(i));
                i += 1;
            }
            '\\' => out.push_str("\\\\"),
            '"' => {
                double = !double;
                out.push(ch);
            }
            '\'' if !double => {
                single = true;
                out.push(ch);
            }
            '@' if !double && here_string_opens(&c, i) => {
                let close = [c[i], '@'];
                let next_line =
                    |from: usize| c[from..].iter().position(|x| *x == '\n').map(|p| from + p + 1);
                let mut at = next_line(i).unwrap_or(c.len());
                while at < c.len() && !c[at..].starts_with(&close) {
                    at = next_line(at).unwrap_or(c.len());
                }
                out.push_str("''");
                i = (at + 2).min(c.len());
            }
            '<' if !double && c.get(i) == Some(&'#') => {
                let end = (i + 1..c.len()).find(|&k| c[k] == '#' && c.get(k + 1) == Some(&'>'));
                out.push(' ');
                i = end.map_or(c.len(), |k| k + 2);
            }
            _ => out.push(ch),
        }
    }
    out
}

/// Whether `c[i]` opens a here-string after its `@`: a quote, then only blanks to the end of its line.
fn here_string_opens(c: &[char], i: usize) -> bool {
    let line = c.get(i + 1..).unwrap_or_default();
    matches!(c.get(i), Some('\'' | '"'))
        && line.contains(&'\n')
        && line.iter().take_while(|x| **x != '\n').all(|x| matches!(x, ' ' | '\t' | '\r'))
}

/// Whether `out` ends with a variable that an `=` after it assigns to (`$page`, `${page}`, `$env:X`, the `+` of `+=`)
/// where a statement starts, after only casts (`[string[,]]$p`) and further targets each with its comma (`$a, $b`;
/// sol-loop r2, S4, S8; review r1, D16-R1-02, its sol-loop R1, R2): PowerShell runs the command on its right side.
fn assigns(out: &str) -> bool {
    let t = out.trim_end_matches([' ', '\t', '+', '-', '*', '/', '%', '?']);
    let var = if t.ends_with('}') {
        t.rfind("${")
    } else {
        let name = t.trim_end_matches(|c: char| c.is_alphanumeric() || c == '_' || c == ':');
        (name.len() < t.len() && name.ends_with('$')).then(|| name.len() - 1)
    };
    let Some(k) = var else { return false };
    let start = t[..k].rfind([';', '\n', '(', '&', '|']).map_or(0, |b| b + 1);
    let (mut depth, mut rest) = (0usize, String::new());
    for ch in t[start..k].chars() {
        match ch {
            '[' => depth += 1,
            ']' => depth = depth.saturating_sub(1),
            _ if depth == 0 => rest.push(ch),
            _ => {}
        }
    }
    let rest = rest.trim_end_matches([' ', '\t']);
    (rest.is_empty() || rest.ends_with(','))
        && rest.split([' ', '\t', ',']).all(|w| w.is_empty() || w.starts_with('$'))
}

/// The page one simple command would have git open, if any (design note §1, from git's source at v2.56.0): D1 `git
/// help` with an operand; D2 git's own `--help` or `-h` before one, which git reads as `help` (git.c:171, :973); D3
/// `--help` as the first word after the subcommand (git.c:762). Redirections are no words of git's.
fn help_topic(words: &[Word]) -> Option<String> {
    let at = command_at(words).filter(|&k| base_name(&words[k].text) == "git")?;
    let mut args = Vec::new();
    let mut rest = words[at + 1..].iter();
    // A redirection (`2>/dev/null`, `*>&1`, `>` `out`), alone or touching an argument, which stays git's (sol-loop r1,
    // S1; r2, S7): an operator at its word's end takes the next word as its target (r2, S1).
    while let Some(w) = rest.next() {
        let t = w.text.as_str();
        let (arg, op) = match w.redirect {
            Some(k) if t[..k].bytes().all(|b| b"0123456789&*".contains(&b)) => ("", &t[k..]),
            Some(k) => (t[..k].strip_suffix('&').unwrap_or(&t[..k]), &t[k..]),
            None => (t, ""),
        };
        if !arg.is_empty() {
            args.push(arg);
        }
        if op.ends_with(['<', '>', '&', '|']) {
            rest.next();
        }
    }
    let mut i = 0;
    while let Some(w) =
        args.get(i).filter(|w| w.starts_with('-') && !matches!(**w, "--help" | "-h"))
    {
        i += 1 + usize::from(GIT_VALUE_OPTIONS.contains(w));
    }
    let sub = match *args.get(i)? {
        "--help" | "-h" => "help",
        s => s,
    };
    let rest = &args[i + 1..];
    if rest.first() == Some(&"--help") {
        return Some(sub.to_string());
    }
    if sub != "help" {
        return None;
    }
    // A topic: a word that isn't an option, or any word after `--`.
    let mut ended = false;
    for w in rest {
        if ended || !w.starts_with('-') {
            return Some(w.to_string());
        }
        ended = *w == "--";
    }
    None
}

/// The page `command` would have git open in the browser, if any (design note §1), its text read as `shell` reads it.
pub fn git_help(command: &str, shell: Shell) -> Option<String> {
    let commands = match shell {
        Shell::Bash => commands(command),
        Shell::PowerShell => commands(&powershell_as_bash(command)),
    };
    commands.iter().find_map(|words| help_topic(words))
}

/// The rm protocol this guard speaks, which sync checks in the installed copy with the rm hook's own arguments before it
/// writes one (review r1, D16-R1-01): 1 is D16's, with git's help refused, `--shell` and the PowerShell tool.
pub const RM_PROTOCOL: u32 = 1;

/// Every guard `lords guard` runs, as `lords guard --guards` lists them: sync registers a guard only while the installed
/// copy lists it (D20; design note 2026-11-07-lords-delivery-integrity §2). The CLI dispatches each by this name.
pub const GUARDS: [&str; 8] = ["rm", "queue", "session", "turn", "child", "role", "time", "helper"];

/// The guard's answer to an event from Claude Code's Bash tool, or any tool whose text is Bash's (`shell_hook`).
pub fn rm_hook(input: &str) -> String {
    shell_hook(input, Shell::Bash)
}

/// The shell guard's answer to one `PreToolUse` event (the harness's stdin JSON): a denial naming the fix, or nothing
/// (empty: the call goes on to the harness's own checks). A `Bash` tool's text is in `bash_tool` (`--shell`), a
/// `PowerShell` tool's in PowerShell; any other tool, and input that isn't such an event, is let through, so the guard
/// never stops a call it can't read. Bash text gets every check, PowerShell text git's help only: the `rm` fix is
/// Bash syntax (design note 2026-11-02-lords-git-help-guard §3). The probe event is answered with `RM_PROTOCOL`.
pub fn shell_hook(input: &str, bash_tool: Shell) -> String {
    let Ok(ev) = serde_json::from_str::<Value>(input) else { return String::new() };
    if ev["hook_event_name"] == CHILD_PROBE_EVENT {
        return format!("lords guard rm {RM_PROTOCOL}");
    }
    let shell = match ev["tool_name"].as_str() {
        Some("Bash") => bash_tool,
        Some("PowerShell") => Shell::PowerShell,
        _ => return String::new(),
    };
    let Some(command) = ev["tool_input"]["command"].as_str() else { return String::new() };
    let bash = shell == Shell::Bash;
    let reason = if let Some(word) = bash.then(|| unguarded_rm(command)).flatten() {
        format!(
            "Lords: `rm` with a variable path ({word}) deletes the wrong place if the variable is empty. Write each \
             variable as {RM_FIX}: quoted, with `:?`, so an empty one stops the command."
        )
    } else if let Some(word) = bash.then(|| hook_bypass(command)).flatten() {
        format!(
            "Lords: `{word}` would run git without its hooks. rule.hooks-run-on-every-commit: commit through the \
             normal hooks, never skip them or repoint their path. Run it without that; if a hook fails, fix what it \
             reports, and if the hook itself is broken, tell the repo's owner. A test fixture's own hooks path goes \
             in a script file."
        )
    } else if let Some(topic) = git_help(command, shell) {
        format!(
            "Lords: this opens git's help on `{topic}` as a web page in the owner's default browser \
             (rule.no-default-browser). Print to the terminal instead: `git {topic} -h` gives a command's options, \
             and every page is text to grep under `C:/Program Files/Git/ucrt64/share/doc/git-doc/` \
             (`git-{topic}.adoc`; a guide is `git<name>.adoc`, as `githooks.adoc`)."
        )
    } else {
        return String::new();
    };
    json!({ "hookSpecificOutput": {
        "hookEventName": "PreToolUse",
        "permissionDecision": "deny",
        "permissionDecisionReason": reason,
    } })
    .to_string()
}

// ---------------------------------------------------------------- queue
//
// `queue` (owner, 30 Oct about 23:40): agents said they had taken their whole T3 queue while items still sat in it.
// A Claude Code `Stop` hook keeps a turn from ending while something may wait in the session's own T3 queue. T3's
// CLI can't count the queue: `t3 thread show --json` gives the thread's latest and active run and drops the runs'
// states, and a cancelled or promoted item stays the latest run (design note E1, Harness/notes/2026-10-31-lords-
// queue-guard.md). So the guard reads both runs, and the session's transcript for what it listed in one call and
// took (Lords owner, 31 Oct, rules a and b). It fails open: a stop it can't judge goes through with a note. It never
// traps: at most two blocks in a row. Its limit: an item a later run overtook after a stop went through anyway isn't
// held again (`Transcript::may_wait`).

/// What the agent is told when its stop is held: the rule's steps (rule.orchestrator-queue-discipline). "May": the
/// latest run can be one someone else cancelled (design note D1). "In one call": only a listing on one page counts.
pub const QUEUE_REASON: &str = "Lords: item(s) may be queued for this thread: list it in one call (no cursor, a \
    `limit` up to 100, so no next cursor), read the truncated ones in full, cancel the text-only ones, promote those \
    with an attachment, then answer together. If the list is empty, nothing waits: end the turn.";

/// How long the T3 CLI may take: measured 0.8 to 8.3 s on T3 v0.1.5 on threads of 96 and 424 items (design note E2).
pub const QUEUE_DEADLINE: Duration = Duration::from_secs(10);

/// Blocks in a row before a stop goes through anyway (brief, 30 Oct: "Block at most twice in a row").
const MAX_BLOCKS: u32 = 2;

/// A thread's latest and active run as T3's CLI shows them: run ids, compared only for equality (an id's `ordinal:`
/// suffix needn't be the run's ordinal; design note E1-c).
#[derive(Debug, PartialEq, Eq)]
pub struct Runs {
    pub latest: String,
    pub active: String,
}

/// The runs in the answer of `t3 thread show <id> --json --messages 0`: `None` when either is `null` (no run yet,
/// or none active: nothing to compare); an error for anything that isn't such an answer.
pub fn thread_runs(show: &str) -> Result<Option<Runs>, String> {
    let v: Value =
        serde_json::from_str(show).map_err(|e| format!("its answer isn't JSON ({e})"))?;
    let id = |key: &str| match v["thread"].get(key) {
        Some(Value::String(s)) => Ok(Some(s.clone())),
        Some(Value::Null) => Ok(None),
        _ => Err(format!("its answer has no `thread.{key}`")),
    };
    Ok(match (id("latestRunId")?, id("activeRunId")?) {
        (Some(latest), Some(active)) => Some(Runs { latest, active }),
        _ => None,
    })
}

/// A complete listing of this thread's queue: a `t3_queue list` call with no cursor (or cursor 0) whose result has
/// `nextCursor: null`, so it showed the whole queue at once. The line of its result and the queued runs it showed.
/// A page of a longer listing never is one: T3 reads the queue afresh for each page and slices it by offset, so a
/// take or a reorder between pages makes a later page skip runs (sol-loop r1 S1; review r1 R1, R2).
struct Listing {
    line: usize,
    shown: Vec<String>,
}

/// What a session's transcript shows of its own thread's queue, fed line by line (Claude Code's JSONL, one entry a
/// line). Only the session's own `user` and `assistant` entries count, never the copies of requests it also keeps
/// (`api-request-blob`), and only `t3_queue` calls on this thread: no `threadId`, or this one, and no
/// `environmentId`. A `cancel` or `promote_to_steer` takes the run it names, whatever its result.
pub struct Transcript {
    thread: String,
    latest: String,
    active: String,
    /// The lines worth parsing: those naming the latest run, `t3_queue` or `nextCursor` (one pass, not three: a
    /// transcript can be hundreds of MB).
    wanted: regex::bytes::Regex,
    line: usize,
    /// The tool-use ids of the `list` calls with no cursor (or cursor 0).
    lists: std::collections::HashSet<String>,
    /// The latest complete listing.
    listing: Option<Listing>,
    /// Each run taken, with the line of the call.
    taken: Vec<(usize, String)>,
    /// The first line of the session's own entries that names the latest run.
    latest_seen: Option<usize>,
}

impl Transcript {
    pub fn new(thread: &str, runs: &Runs) -> Transcript {
        let wanted = format!("{}|t3_queue|nextCursor", regex::escape(&runs.latest));
        Transcript {
            thread: thread.into(),
            latest: runs.latest.clone(),
            active: runs.active.clone(),
            wanted: regex::bytes::Regex::new(&wanted).expect("escaped literals make a regex"),
            line: 0,
            lists: Default::default(),
            listing: None,
            taken: Vec::new(),
            latest_seen: None,
        }
    }

    pub fn line(&mut self, line: &[u8]) {
        let n = self.line;
        self.line += 1;
        // Most lines are none of these: they aren't parsed.
        if !self.wanted.is_match(line) {
            return;
        }
        let names_latest =
            self.wanted.find_iter(line).any(|m| m.as_bytes() == self.latest.as_bytes());
        let Ok(entry) = serde_json::from_slice::<Value>(line) else { return };
        if !matches!(entry["type"].as_str(), Some("user" | "assistant")) {
            return;
        }
        if names_latest && self.latest_seen.is_none() {
            self.latest_seen = Some(n);
        }
        for c in entry["message"]["content"].as_array().into_iter().flatten() {
            match c["type"].as_str() {
                Some("tool_use") if self.own_queue_call(c) => {
                    let input = &c["input"];
                    match input["action"].as_str() {
                        Some("list")
                            if input.get("cursor").is_none_or(|v| v.is_null() || v == 0) =>
                        {
                            if let Some(id) = c["id"].as_str() {
                                self.lists.insert(id.into());
                            }
                        }
                        Some("cancel" | "promote_to_steer") => {
                            if let Some(run) = input["queuedRunId"].as_str() {
                                self.taken.push((n, run.into()));
                            }
                        }
                        _ => {}
                    }
                }
                Some("tool_result")
                    if c["tool_use_id"].as_str().is_some_and(|id| self.lists.contains(id)) =>
                {
                    let Ok(out) = serde_json::from_str::<Value>(&result_text(&c["content"])) else {
                        continue;
                    };
                    let Some(items) = out["items"].as_array() else { continue };
                    if out.get("nextCursor") != Some(&Value::Null) {
                        continue;
                    }
                    let shown =
                        items.iter().filter_map(|i| i["queuedRunId"].as_str().map(String::from));
                    self.listing = Some(Listing { line: n, shown: shown.collect() });
                }
                _ => {}
            }
        }
    }

    /// Whether something may wait that the session hasn't taken. Only while a run later than the active one exists
    /// (design note E1); then (a) no complete listing came at or after the latest run first appeared, or (b) the
    /// latest complete listing showed a run no `cancel` or `promote_to_steer` named after it (Lords owner, 31 Oct,
    /// and after review r1). Pages of a longer listing count for nothing, so a queue listed in pages is held up to
    /// the cap.
    ///
    /// The limit of the invariant (review r1 R3, Lords owner: documented, not repaired): with `latest == active`
    /// nothing is held. Once a stop has gone through with items waiting (at the cap, or failing open), T3 may start
    /// a later run first (an automatic completion goes ahead of older messages); an older item it overtook is then not
    /// held again while that run is the latest. Only T3 itself can say what is still queued (design note F1).
    pub fn may_wait(&self) -> bool {
        if self.latest == self.active {
            return false;
        }
        let Some(l) = &self.listing else { return true };
        if self.latest_seen.is_none_or(|f| l.line < f) {
            return true;
        }
        l.shown.iter().any(|r| !self.taken.iter().any(|(n, t)| *n > l.line && t == r))
    }

    fn own_queue_call(&self, c: &Value) -> bool {
        let name = c["name"].as_str().unwrap_or("");
        let input = &c["input"];
        (name == "t3_queue" || name.ends_with("__t3_queue"))
            && input.get("environmentId").is_none_or(Value::is_null)
            && input.get("threadId").is_none_or(|t| t.is_null() || *t == *self.thread)
    }
}

/// A tool result's text: the content itself, or its text blocks joined.
fn result_text(content: &Value) -> String {
    match content {
        Value::String(s) => s.clone(),
        Value::Array(blocks) => blocks.iter().filter_map(|b| b["text"].as_str()).collect(),
        _ => String::new(),
    }
}

/// Where the queue guard looks: the session's T3 thread and CLI (`T3CODE_THREAD_ID` and `T3CODE_CLI_PATH`, which T3
/// sets for the sessions it starts), the tool's home (for the count of blocks in a row) and the CLI's deadline.
pub struct QueueEnv {
    pub thread: Option<String>,
    pub cli: Option<PathBuf>,
    pub home: PathBuf,
    pub deadline: Duration,
}

/// The queue guard's answer: what Claude Code reads on stdout (a block, or nothing) and a note for stderr.
#[derive(Debug)]
pub struct Answer {
    pub stdout: String,
    pub note: Option<String>,
}

/// The queue guard's answer to one Claude Code event (its stdin JSON). Only a `Stop` is looked at; a subagent's
/// (`SubagentStop`) or any other goes through without a word. A stop goes through with a note whenever the guard
/// can't judge it: no thread or CLI, a CLI that fails, is late or answers something else, or a transcript it can't
/// read. It is held, with `QUEUE_REASON`, when something may wait (`Transcript::may_wait`), at most twice in a row.
pub fn queue_hook(input: &str, env: &QueueEnv) -> Answer {
    let quiet = || Answer { stdout: String::new(), note: None };
    let through = |why: String| Answer { stdout: String::new(), note: Some(why) };
    let Ok(ev) = serde_json::from_str::<Value>(input) else {
        return through("the event can't be read".into());
    };
    if ev["hook_event_name"] != "Stop" {
        return quiet();
    }
    let Some(thread) = env.thread.as_deref().filter(|t| !t.is_empty()) else {
        return through("no T3CODE_THREAD_ID: a session T3 didn't start".into());
    };
    let Some(cli) = env.cli.as_deref() else {
        return through("no T3CODE_CLI_PATH".into());
    };
    let streak = Streak::of(&env.home, ev["session_id"].as_str());
    let blocks = if ev["stop_hook_active"] == true { streak.blocks() } else { 0 };
    let verdict = if blocks >= MAX_BLOCKS {
        Err("held twice in a row already".into())
    } else {
        waits(&ev, thread, cli, env.deadline)
    };
    match verdict {
        Ok(true) => match streak.set(blocks + 1) {
            Ok(()) => Answer {
                stdout: json!({"decision": "block", "reason": QUEUE_REASON}).to_string(),
                note: None,
            },
            Err(e) => {
                streak.clear();
                through(format!("can't count the stops held in a row ({e})"))
            }
        },
        Ok(false) => {
            streak.clear();
            quiet()
        }
        Err(why) => {
            streak.clear();
            through(why)
        }
    }
}

/// Whether something may wait in `thread`'s queue: the CLI's two runs, then the session's transcript.
fn waits(ev: &Value, thread: &str, cli: &Path, deadline: Duration) -> Result<bool, String> {
    let args = ["thread", "show", thread, "--json", "--messages", "0"];
    let show =
        crate::process::t3_cli(cli, &args, deadline).map_err(|e| format!("T3's CLI: {e}"))?;
    let Some(runs) = thread_runs(&show).map_err(|e| format!("T3's CLI: {e}"))? else {
        return Ok(false);
    };
    if runs.latest == runs.active {
        return Ok(false);
    }
    let path = ev["transcript_path"].as_str().ok_or("the event names no transcript")?;
    let mut t = Transcript::new(thread, &runs);
    crate::fsguard::read_lines(path, |l| t.line(l))
        .map_err(|e| format!("the transcript can't be read ({path}: {e})"))?;
    Ok(t.may_wait())
}

/// The stops held in a row for one session: `<tool home>/guard/queue/<session id>`, a file that is there only while
/// they are being held. A session id that can't be a file name has no place: nothing could count its held stops, so
/// none is held.
struct Streak {
    place: Option<Contained>,
}

impl Streak {
    fn of(home: &Path, session: Option<&str>) -> Streak {
        let place = (|| {
            let root = Root::home(home).ok()?;
            let parts =
                [Part::new("guard").ok()?, Part::new("queue").ok()?, Part::new(session?).ok()?];
            Some(root.join(&parts))
        })();
        Streak { place }
    }

    /// Unknown counts as the most: a count it can't read never holds a stop again.
    fn blocks(&self) -> u32 {
        let Some(p) = &self.place else { return MAX_BLOCKS };
        match crate::fsguard::read_text(p.path()) {
            Ok(None) => 0,
            Ok(Some(t)) => t.trim().parse().unwrap_or(MAX_BLOCKS),
            Err(_) => MAX_BLOCKS,
        }
    }

    fn set(&self, n: u32) -> Result<(), String> {
        let p = self.place.as_ref().ok_or("the session id is no file name")?;
        let dir = p.root().join(&p.parts()[..p.parts().len() - 1]);
        crate::fsguard::create_dir_all(&dir)?;
        let text = format!("{n}\n");
        match crate::fsguard::kind(p.path())? {
            crate::fsguard::Kind::Missing => crate::fsguard::create_file(p, text.as_bytes()),
            _ => crate::fsguard::replace_own_file(p, text.as_bytes()),
        }
    }

    fn clear(&self) {
        if let Some(p) = &self.place {
            if !matches!(crate::fsguard::kind(p.path()), Ok(crate::fsguard::Kind::Missing)) {
                let _ = crate::fsguard::delete_file(p);
            }
        }
    }
}

// ---------------------------------------------------------------- session and turn
//
// `session` and `turn` (the Lords owner, 31 Oct): a running session keeps the Lords rules it started with, so rule
// changes never reached long-running sessions; and agents guessed clock times. `session`, a `SessionStart` hook,
// records the hash of each Lords-written file the session reads. `turn`, a `UserPromptSubmit` hook, gives the agent
// the clock in one line and, only when one of those files changed since, names it, once. A few small reads per
// prompt: no transcript, no network, no T3 (design note: Harness/notes/2026-10-31-lords-turn-hook.md).

/// Days after which a session's record is stale: removed at any session's next start. A later prompt of that session
/// finds no record and is told once, as a session that started before the guard was installed.
pub const SESSION_RECORD_DAYS: u64 = 7;

/// Where the session and turn guards look: the tool home (their records) and the user's Claude config dir, whose
/// `CLAUDE.md` includes the user-level Lords files (`CLAUDE_CONFIG_DIR`, else `~/.claude`).
pub struct RulesEnv {
    pub home: PathBuf,
    pub user_config: Option<PathBuf>,
}

/// The clock line: the local time in the system's zone, labelled by the zone's city (`Berlin 07:41 Sat 31 Oct`), by
/// its offset when it has no IANA name, and as UTC, said so, when the system reports no zone. Never an assumed zone.
pub fn clock_line(now: jiff::Timestamp, zone: Option<&jiff::tz::TimeZone>) -> String {
    let Some(tz) = zone else {
        let utc = now.to_zoned(jiff::tz::TimeZone::UTC);
        return format!("UTC {} (local zone unknown)", utc.strftime("%H:%M %a %-d %b"));
    };
    let local = now.to_zoned(tz.clone());
    let label = match tz.iana_name() {
        Some(name) => name.rsplit('/').next().unwrap_or(name).replace('_', " "),
        None => {
            let s = local.offset().seconds();
            let sign = if s < 0 { '-' } else { '+' };
            format!("UTC{sign}{:02}:{:02}", s.abs() / 3600, s.abs() % 3600 / 60)
        }
    };
    format!("{label} {}", local.strftime("%H:%M %a %-d %b"))
}

/// The system's clock and zone, for the turn guard.
pub fn system_clock() -> (jiff::Timestamp, Option<jiff::tz::TimeZone>) {
    (jiff::Timestamp::now(), jiff::tz::TimeZone::try_system().ok())
}

/// How a session came to read a file: Claude Code loads it itself, again after a compaction and on resume (the place
/// `CLAUDE.local.md` files, the user `CLAUDE.md`'s includes), or the agent read it on its own (a role file; Codex's
/// `AGENTS.override.md`).
#[derive(Clone, Copy, Debug, PartialEq)]
enum Load {
    Claude,
    Agent,
}

/// Each Lords-written file a session in `cwd` reads, by path, with how it was read and its hash: the
/// `CLAUDE.local.md`, `AGENTS.override.md` and `.lords/role-*.md` of the cwd and every dir above it, and the includes
/// of the user `CLAUDE.md` that lie in the tool home. A file counts only with Lords' marker in it. Any read that fails
/// other than for a missing file is an error: a file that can't be read now is no change (sol-loop r1, S2).
fn rules_files(cwd: &Path, env: &RulesEnv) -> Result<BTreeMap<String, (Load, String)>, String> {
    let read = |p: &Path| match crate::fsguard::read_bytes(p) {
        Ok(b) => Ok(Some(b)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(format!("{}: {e}", p.display())),
    };
    let mut out = BTreeMap::new();
    let mut add = |p: &Path, load: Load| -> Result<(), String> {
        let marker = crate::compose::BEGIN.as_bytes();
        if let Some(b) = read(p)?.filter(|b| b.windows(marker.len()).any(|w| w == marker)) {
            out.insert(crate::adapters::slashes(p), (load, crate::fsguard::sha(&b)));
        }
        Ok(())
    };
    for d in cwd.ancestors() {
        add(&d.join("CLAUDE.local.md"), Load::Claude)?;
        add(&d.join("AGENTS.override.md"), Load::Agent)?;
        for p in crate::fsguard::list_dir(&d.join(crate::sync::ROLE_DIR))? {
            let name = p.file_name().and_then(|n| n.to_str()).unwrap_or("");
            if name.starts_with("role-") && name.ends_with(".md") {
                add(&p, Load::Agent)?;
            }
        }
    }
    let home = crate::adapters::slashes(&env.home).to_lowercase() + "/";
    let user = match &env.user_config {
        Some(c) => read(&c.join("CLAUDE.md"))?.unwrap_or_default(),
        None => Vec::new(),
    };
    for line in String::from_utf8_lossy(&user).lines() {
        let Some(include) = line.trim().strip_prefix('@') else { continue };
        let p = crate::config::expand_home(include.trim());
        if crate::adapters::slashes(&p).to_lowercase().starts_with(&home) {
            add(&p, Load::Claude)?;
        }
    }
    Ok(out)
}

/// What a session read at its start: its cwd and its files (`rules_files`).
struct Record {
    cwd: PathBuf,
    files: BTreeMap<String, (Load, String)>,
}

const RECORD_HEADER: &str = "lords session record 1";

/// A session's record: `<tool home>/guard/session/<session id>`.
fn record_place(home: &Path, session: &str) -> Result<Contained, String> {
    Ok(Root::home(home)?.join(&[Part::new("guard")?, Part::new("session")?, Part::new(session)?]))
}

/// The record, or `None` when there is none (or it isn't one: it is then made again).
fn read_record(place: &Contained) -> Result<Option<Record>, String> {
    let Some(text) = crate::fsguard::read_text(place.path())? else { return Ok(None) };
    let mut lines = text.lines();
    let (Some(RECORD_HEADER), Some(cwd)) =
        (lines.next(), lines.next().and_then(|l| l.strip_prefix("cwd\t")))
    else {
        return Ok(None);
    };
    let mut files = BTreeMap::new();
    for l in lines {
        let mut f = l.splitn(3, '\t');
        let load = match f.next() {
            Some("claude") => Load::Claude,
            Some("agent") => Load::Agent,
            _ => return Ok(None),
        };
        let (Some(hash), Some(path)) = (f.next(), f.next()) else { return Ok(None) };
        files.insert(path.to_string(), (load, hash.to_string()));
    }
    Ok(Some(Record { cwd: PathBuf::from(cwd), files }))
}

fn write_record(
    place: &Contained,
    cwd: &Path,
    files: &BTreeMap<String, (Load, String)>,
) -> Result<(), String> {
    let mut text = format!("{RECORD_HEADER}\ncwd\t{}\n", cwd.display());
    for (path, (load, hash)) in files {
        let load = if *load == Load::Claude { "claude" } else { "agent" };
        text.push_str(&format!("{load}\t{hash}\t{path}\n"));
    }
    crate::fsguard::create_dir_all(&place.root().join(&place.parts()[..place.parts().len() - 1]))?;
    match crate::fsguard::kind(place.path())? {
        crate::fsguard::Kind::Missing => crate::fsguard::create_file(place, text.as_bytes()),
        _ => crate::fsguard::replace_own_file(place, text.as_bytes()),
    }
}

/// The cwds of the sessions with a live record, one written within `SESSION_RECORD_DAYS` (`lords paths`: a place with
/// one is in use; review r1, D14D15-R1-03). `None` when a record can't be listed or read, so no place is shown to have
/// none.
pub fn live_session_cwds(home: &Path) -> Option<Vec<PathBuf>> {
    let dir = Root::home(home).ok()?.join(&[Part::new("guard").ok()?, Part::new("session").ok()?]);
    let max = Duration::from_secs(SESSION_RECORD_DAYS * 24 * 3600);
    let mut cwds = Vec::new();
    for p in crate::fsguard::list_dir(dir.path()).ok()? {
        if crate::fsguard::modified(&p).ok()?.elapsed().is_ok_and(|age| age > max) {
            continue;
        }
        let name = Part::new(p.file_name()?.to_str()?).ok()?;
        cwds.push(read_record(&dir.child(name)).ok()??.cwd);
    }
    Some(cwds)
}

/// Removes every session record older than `SESSION_RECORD_DAYS`. Best effort: a record left is only a small file.
fn sweep_records(home: &Path) {
    let Ok(dir) =
        Root::home(home).and_then(|r| Ok(r.join(&[Part::new("guard")?, Part::new("session")?])))
    else {
        return;
    };
    let max = Duration::from_secs(SESSION_RECORD_DAYS * 24 * 3600);
    for p in crate::fsguard::list_dir(dir.path()).unwrap_or_default() {
        let stale = crate::fsguard::modified(&p)
            .ok()
            .and_then(|t| t.elapsed().ok())
            .is_some_and(|age| age > max);
        let name = p.file_name().and_then(|n| n.to_str()).and_then(|n| Part::new(n).ok());
        if let (true, Some(name)) = (stale, name) {
            let _ = crate::fsguard::delete_file(&dir.child(name));
        }
    }
}

fn quiet() -> Answer {
    Answer { stdout: String::new(), note: None }
}

fn noted(why: impl Into<String>) -> Answer {
    Answer { stdout: String::new(), note: Some(why.into()) }
}

/// The event's session id and cwd.
fn session_of(ev: &Value) -> Result<(&str, &Path), String> {
    let session = ev["session_id"].as_str().ok_or("the event names no session")?;
    let cwd = ev["cwd"].as_str().ok_or("the event names no cwd")?;
    Ok((session, Path::new(cwd)))
}

/// The session guard's answer to one `SessionStart` event: always nothing on stdout. It records the files the
/// session reads (`rules_files`). On `compact`, `resume` and `fork` (a fork carries its parent's conversation under a
/// new id; review r1, R1) Claude reloads its own files, so they are recorded anew, while the files the agent read
/// itself keep the hash recorded before: a change to one of them is still named. With no record from before, nothing
/// is known of those, so the session stays unrecorded and its next prompt is told (sol-loop r1, S1). Then stale
/// records go (`sweep_records`). On `compact`, in a place, it gives one fixed line, its only output: re-read your role's
/// file in that place's `.lords` (F10, the Lords owner after fix check r1: no role is guessed). Any failure is a note;
/// the session goes on.
pub fn session_hook(input: &str, env: &RulesEnv) -> Answer {
    let Ok(ev) = serde_json::from_str::<Value>(input) else {
        return noted("the event can't be read");
    };
    if ev["hook_event_name"] != "SessionStart" {
        return quiet();
    }
    let recorded = session_of(&ev).and_then(|(session, cwd)| {
        let place = record_place(&env.home, session)?;
        let mut files = rules_files(cwd, env)?;
        if matches!(ev["source"].as_str(), Some("compact" | "resume" | "fork")) {
            let Some(before) = read_record(&place)? else { return Ok(()) };
            files.retain(|_, (load, _)| *load == Load::Claude);
            files.extend(before.files.into_iter().filter(|(_, (load, _))| *load == Load::Agent));
        }
        write_record(&place, cwd, &files)
    });
    sweep_records(&env.home);
    let mut answer = match recorded {
        Ok(()) => quiet(),
        Err(e) => noted(format!("the session's files aren't recorded ({e})")),
    };
    // The nearest `.lords` at or above the cwd, a dir, found without reading any file in it (F10-SR-02).
    let place = || {
        let lords =
            Path::new(ev["cwd"].as_str()?).ancestors().map(|d| d.join(crate::sync::ROLE_DIR));
        lords.into_iter().find(|l| matches!(crate::fsguard::kind(l), Ok(crate::fsguard::Kind::Dir)))
    };
    if let Some(lords) = (ev["source"] == "compact").then(place).flatten() {
        let line = format!(
            "Lords: after the compaction, if your launch named a role, re-read your role's file `role-<role>.md` in `{}` now; it binds you.",
            lords.display()
        );
        answer.stdout = json!({"hookSpecificOutput": {
            "hookEventName": "SessionStart",
            "additionalContext": line,
        }})
        .to_string();
    }
    answer
}

/// The turn guard's answer to one `UserPromptSubmit` event: context of the clock line and, only when a file the
/// session read changed since it was recorded, a line naming each such file, once (the record then holds it as it
/// is). A session with no record (it started before the guard) is told once that the rules may have changed, with
/// every file it reads. Never a block: a failure in the rules part leaves the clock line, with a note.
pub fn turn_hook(
    input: &str,
    env: &RulesEnv,
    now: jiff::Timestamp,
    zone: Option<&jiff::tz::TimeZone>,
) -> Answer {
    let Ok(ev) = serde_json::from_str::<Value>(input) else {
        return noted("the event can't be read");
    };
    if ev["hook_event_name"] != "UserPromptSubmit" {
        return quiet();
    }
    let mut lines = vec![clock_line(now, zone)];
    let mut note = None;
    match rules_found(&ev, env) {
        Ok(Some(found)) => {
            // Said first, saved second: a record that can't be saved keeps the old one, so it is said again
            // (review r1, R4).
            lines.extend(found.line);
            if let Err(e) = write_record(&found.place, &found.cwd, &found.files) {
                note = Some(format!("the record isn't saved, so this is said again ({e})"));
            }
        }
        Ok(None) => {}
        Err(e) => note = Some(format!("the rules aren't checked ({e})")),
    }
    let out = json!({"hookSpecificOutput": {
        "hookEventName": "UserPromptSubmit",
        "additionalContext": lines.join("\n"),
    }});
    Answer { stdout: out.to_string(), note }
}

// ---------------------------------------------------------------- child
//
// `child` (plan L6, an orchestrator's condition 3: "Children start with their own role's core, not the parent's"), a
// `SubagentStart` hook in Claude Code and in Codex: it gives a native child the start file sync wrote for the stack's
// `child_role` in the child's place, `.lords/child.md`, as the child's context. One small read, no stack loaded.

/// The agent types that load no place file (Claude Code's docs, sub-agents: "Explore and Plan start without your
/// CLAUDE.md files"; probed 31 Oct, design note D4): they get the role's whole start, every other the part beyond the
/// place's file.
pub const NO_PLACE_FILE_AGENTS: [&str; 2] = ["Explore", "Plan"];

/// A child file's text as context: without its marker lines, trimmed. Its size is counted in characters, as Claude
/// Code measures a hook's context (`sync::CHILD_START_CHARS`).
pub fn child_context(text: &str) -> String {
    let lines: Vec<&str> = text
        .lines()
        .map(|l| l.trim_end_matches('\r'))
        .filter(|l| *l != crate::compose::BEGIN && *l != crate::compose::END)
        .collect();
    lines.join("\n").trim().to_string()
}

/// The deadline sync registers on the child hook, Claude Code's and Codex's (fix check r2, FC5): every wait in the
/// guard ends inside it.
pub const CHILD_HOOK_TIMEOUT_SECS: u64 = 30;

/// How long a reader that passed a missing `.lords` waits for the apply lock (`child_text`): a running sync holds it
/// until it is done; past that, the child gets nothing, with a note. Half the hook's deadline, so the reads after it
/// end inside it too.
pub const CHILD_LOCK_WAIT_SECS: u64 = CHILD_HOOK_TIMEOUT_SECS / 2;

/// The child protocol this guard speaks, which sync checks in the installed copy before it writes the child hook (fix
/// check r2, FC4): 4 is `guard child --home DIR`, one read per start for every agent type, at most
/// `sync::CHILD_START_CHARS` (the Lords owner's option 1, 1 Nov). 3, this round's first build, read some starts in
/// parts; it was never installed.
pub const CHILD_PROTOCOL: u32 = 4;

/// The event sync's probe gives the child and role guards (`process::guard_protocol`), answered with the protocol.
pub const CHILD_PROBE_EVENT: &str = "LordsGuardProbe";

/// The text of the child file the event's agent type takes in its place, or "" where there is none. The place is the
/// nearest `.lords` at or above `cwd`; a file is read with one open whose own answer says whether `.lords` was there
/// (`fsguard::read_text_in`): sync never edits a child file in place (a whole `.lords` is renamed in), so one read is
/// one generation, and "no file" comes only from a `.lords` that was there without it (repair 2 sol-loop r1, V1).
/// A `.lords` at `cwd` itself is decided at once. Past a missing one, which may be no place or a place between its two
/// versions (fix check r1, FC1), the walk is decided under the apply lock, which every sync holds while it moves a
/// `.lords` aside and puts the new one in: then no swap is under way, and an old `.lords` aside with no new one is a
/// sync that stopped, so the child gets nothing rather than an enclosing place's start (repair 2 sol-loop r2, W1).
fn child_text(ev: &Value, cwd: &Path, home: &Path) -> Result<String, String> {
    // Never the part beyond the place's file in place of a whole start (repair sol-loop r1, R4).
    let whole = ev["agent_type"].as_str().is_some_and(|t| NO_PLACE_FILE_AGENTS.contains(&t));
    let name = if whole { crate::sync::CHILD_WHOLE_FILE } else { crate::sync::CHILD_FILE };
    Ok(place_file(cwd, home, name)?.map(|(_, t)| t).unwrap_or_default())
}

/// File `name` of the place `cwd` lies in, read as `child_text` says: the place's `.lords` dir and the file's text, ""
/// where that dir holds no such file (or one without Lords' marker); `None` where `cwd` lies in no place.
fn place_file(cwd: &Path, home: &Path, name: &str) -> Result<Option<(PathBuf, String)>, String> {
    use crate::fsguard::InDir;
    let found = |r: InDir| match r {
        InDir::Text(t) if t.contains(crate::compose::BEGIN) => Some(t),
        InDir::Text(_) | InDir::NoFile => Some(String::new()),
        InDir::NoDir => None,
    };
    let here = cwd.join(crate::sync::ROLE_DIR);
    if let Some(t) = found(crate::fsguard::read_text_in(&here, name)?) {
        return Ok(Some((here, t)));
    }
    let _lock = crate::sync::ApplyLock::take(home, CHILD_LOCK_WAIT_SECS)
        .map_err(|e| format!("a sync is running, so the place can't be told ({e})"))?;
    for d in cwd.ancestors() {
        let dir = d.join(crate::sync::ROLE_DIR);
        if let Some(t) = found(crate::fsguard::read_text_in(&dir, name)?) {
            return Ok(Some((dir, t)));
        }
        if crate::fsguard::being_replaced(&dir)? {
            return Err(format!(
                "{}: its old version is aside with no new one, from a sync that stopped; sync again",
                dir.display()
            ));
        }
    }
    Ok(None)
}

/// The child guard's answer to one `SubagentStart` event: the child file its agent type takes in its place
/// (`child-whole.md` for one that loads no place file, else `child.md`; the nearest `.lords` is the place, never an
/// enclosing one), without its marker lines, as `additionalContext`, from one read. Claude Code fires the event again
/// with the same agent id when a child resumes and gives no id per firing, so one read in one hook is the only
/// delivery that is always one generation (fix check r2, FC1; the Lords owner's option 1, 1 Nov). Nothing where no
/// place has one. A start longer than one hook delivers (`sync::CHILD_START_CHARS`) is given nowhere, with a note:
/// sync refuses such a `child.md`, and Explore's and Plan's whole start waits for the corpus to fit. Never a block: a
/// failure is a note and no context. The probe event (`CHILD_PROBE_EVENT`) is answered with `CHILD_PROTOCOL`.
pub fn child_hook(input: &str, home: &Path) -> Answer {
    let Ok(ev) = serde_json::from_str::<Value>(input) else {
        return noted("the event can't be read");
    };
    if ev["hook_event_name"] == CHILD_PROBE_EVENT {
        return Answer { stdout: format!("lords guard child {CHILD_PROTOCOL}"), note: None };
    }
    if ev["hook_event_name"] != "SubagentStart" {
        return quiet();
    }
    let Some(cwd) = ev["cwd"].as_str() else { return noted("the event names no cwd") };
    let text = match child_text(&ev, Path::new(cwd), home) {
        Ok(t) => t,
        Err(e) => return noted(format!("no start given ({e})")),
    };
    if text.is_empty() {
        return quiet();
    }
    let context = child_context(&text);
    let (n, max) = (context.chars().count(), crate::sync::CHILD_START_CHARS);
    if n > max {
        return noted(format!(
            "no start given: it is {n} characters, past the {max} one hook gives a child"
        ));
    }
    let out = json!({"hookSpecificOutput": {
        "hookEventName": "SubagentStart",
        "additionalContext": context,
    }});
    Answer { stdout: out.to_string(), note: None }
}

// ---------------------------------------------------------------- role
//
// `role` (F10, design note Harness/notes/2026-11-02-lords-role-at-launch.md): a session T3 starts with `delegate_task`
// reads only its place's file, which carries the place's default role. Its launch names its role in the prompt: the
// brief's first line, or T3's `role` prefix.

/// A role a session's launch names (`launch_role`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Launch {
    pub role: String,
}

/// T3's `delegate_task` prefix (t3code `OrchestratorMcpService.ts` `taskPrompt`, installed build of 31 Oct): `Act as the
/// ${role} sub-agent for this task.`, then a blank line and the task; none for `general` or no role.
const T3_PREFIX: (&str, &str) = ("Act as the ", " sub-agent for this task.");

/// T3's roles that name a Lords role (design note D1, provisional: the Lords owner to confirm). `design`, `test` and
/// `general` name none.
const T3_ROLES: [(&str, &str); 3] =
    [("implementation", "coder"), ("review", "reviewer"), ("research", "helper")];

/// The role a session's launch names (design note D1, revised after review r1, F10-R1-01: the launcher's signal wins).
/// Where the prompt opens with T3's prefix, the prefix alone decides: the Lords role its role maps to, or none. Without
/// it, the first word after `Role:` on the prompt's first line, the brief's own (a heading mark, Markdown emphasis and
/// backticks around it ignored, never a quote mark), lower-cased. Either starts at column 0 of the first line: an
/// indented or later line is quoted text, never a signal (F10-SR-01). A word that can't be a role's file name
/// (`role_word`) names nothing.
pub fn launch_role(prompt: &str) -> Option<Launch> {
    let first = prompt.trim_start_matches('\u{feff}').lines().next()?;
    if first.starts_with(char::is_whitespace) {
        return None;
    }
    if let Some(word) =
        first.trim_end().strip_prefix(T3_PREFIX.0).and_then(|r| r.strip_suffix(T3_PREFIX.1))
    {
        let role = T3_ROLES.iter().find(|(t, _)| *t == word)?.1.to_string();
        return Some(Launch { role });
    }
    let l = first.trim_start_matches(|c: char| c.is_whitespace() || "#*_".contains(c));
    let rest = l.get(..5).filter(|p| p.eq_ignore_ascii_case("role:")).map(|_| &l[5..])?;
    let rest = rest.trim_start_matches(|c: char| c.is_whitespace() || "*_`".contains(c));
    let word: String = rest
        .chars()
        .take_while(|c| c.is_ascii_alphanumeric() || *c == '_' || *c == '-')
        .collect::<String>()
        .to_ascii_lowercase();
    role_word(&word).then_some(Launch { role: word })
}

/// Whether `w` can name a role's files: a lower-case letter, then up to 31 letters, digits, `_` or `-`.
fn role_word(w: &str) -> bool {
    w.len() <= 32
        && w.starts_with(|c: char| c.is_ascii_lowercase())
        && w.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-')
}

/// The role protocol this guard speaks, which sync checks in the installed copy before it writes a role hook (design
/// note D6): 1 is `guard role --home DIR` on `UserPromptSubmit`, the starts `.lords/start-<role>.md`.
pub const ROLE_PROTOCOL: u32 = 1;

/// Where the role guard looks: the tool home (sync's apply lock), and the harness it runs for (`--adapter`, as sync
/// writes it), whose place file the start goes beside (F10-R1-03).
pub struct RoleEnv {
    pub home: PathBuf,
    pub adapter: Option<String>,
}

/// Whether the start of `role` can be given for harness `adapter` (none named: not checked) from a place's `.lords` dir
/// `lords`, whose `start-<role>.md` reads `start` ("" where there is none): the start without its marker lines, or why
/// not. The one decision the role guard and `lords paths` share (F10-FC1-02): the start is there with Lords' marker,
/// the harness's place file beside `.lords` is there with Lords' marker, as it carries the lines the start leaves out
/// (F10-R1-03), and the start fits one hook.
pub fn deliverable(
    lords: &Path,
    start: &str,
    role: &str,
    adapter: Option<&str>,
) -> Result<String, String> {
    if !start.contains(crate::compose::BEGIN) {
        return Err(format!("no {} here", crate::sync::start_file(role)));
    }
    if let Some(id) = adapter {
        let place = crate::adapters::get(id)
            .map(|a| a.repo_file())
            .ok_or_else(|| format!("no harness `{id}` is known"))?;
        match crate::fsguard::read_text(&lords.parent().unwrap_or(lords).join(place)) {
            Ok(Some(t)) if t.contains(crate::compose::BEGIN) => {}
            Ok(_) => {
                return Err(format!(
                    "no Lords {place} here, which carries what the start leaves out"
                ))
            }
            Err(e) => return Err(format!("its {place} can't be read: {e}")),
        }
    }
    let text = child_context(start);
    let (n, max) = (text.chars().count(), crate::sync::CHILD_START_CHARS);
    if n > max {
        return Err(format!("it is {n} characters, past the {max} one hook gives"));
    }
    Ok(text)
}

/// What a prompt naming `role` in `cwd` is given (design note C1, C2): the start, where `deliverable` says it can be;
/// else one line saying why not and which file to read, whatever failed (F10-R1-04). `None` where `cwd` lies in no
/// place.
fn role_lines(cwd: &Path, home: &Path, role: &str, adapter: Option<&str>) -> Option<String> {
    let file = crate::sync::role_file(role);
    let not_given = |why: String, at: String| {
        format!("Lords: your role's start ({role}) isn't given here ({why}): read `{at}` now; it binds you.")
    };
    match place_file(cwd, home, &crate::sync::start_file(role)) {
        Ok(None) => None,
        Ok(Some((lords, text))) => Some(
            deliverable(&lords, &text, role, adapter)
                .unwrap_or_else(|why| not_given(why, lords.join(&file).display().to_string())),
        ),
        Err(e) => Some(not_given(
            format!("it can't be read: {e}"),
            format!("{file}` in this place's `{}` dir", crate::sync::ROLE_DIR),
        )),
    }
}

/// The role guard's answer to one `UserPromptSubmit` event (design note D3), Claude Code's and Codex's alike. Stateless
/// (the Lords owner after fix check r1): a prompt carrying the launcher's signal (`launch_role`) gets that role's start,
/// or the line saying why not (`role_lines`), every time; any other prompt gets nothing. The launcher owns the role, so
/// a later prompt carrying a signal is the launcher's again. Never a block. The probe event is answered with
/// `ROLE_PROTOCOL`.
pub fn role_hook(input: &str, env: &RoleEnv) -> Answer {
    let Ok(ev) = serde_json::from_str::<Value>(input) else {
        return noted("the event can't be read");
    };
    if ev["hook_event_name"] == CHILD_PROBE_EVENT {
        return Answer { stdout: format!("lords guard role {ROLE_PROTOCOL}"), note: None };
    }
    if ev["hook_event_name"] != "UserPromptSubmit" {
        return quiet();
    }
    let Some(launch) = launch_role(ev["prompt"].as_str().unwrap_or("")) else { return quiet() };
    let Some(cwd) = ev["cwd"].as_str().map(Path::new) else {
        return noted("the event names no cwd");
    };
    let Some(lines) = role_lines(cwd, &env.home, &launch.role, env.adapter.as_deref()) else {
        return quiet();
    };
    let out = json!({"hookSpecificOutput": {
        "hookEventName": "UserPromptSubmit",
        "additionalContext": lines,
    }});
    Answer { stdout: out.to_string(), note: None }
}

/// What one prompt finds (`rules_found`): the line to say, if any, and the record to save.
struct Found {
    line: Option<String>,
    place: Contained,
    cwd: PathBuf,
    files: BTreeMap<String, (Load, String)>,
}

/// What one prompt finds, or `None` when nothing changed (`turn_hook`).
fn rules_found(ev: &Value, env: &RulesEnv) -> Result<Option<Found>, String> {
    let (session, cwd) = session_of(ev)?;
    let place = record_place(&env.home, session)?;
    let Some(before) = read_record(&place)? else {
        let files = rules_files(cwd, env)?;
        let paths: Vec<&str> = files.keys().map(String::as_str).collect();
        let line = (!paths.is_empty()).then(|| {
            format!(
                "Lords rules may have changed since this session started: re-read {}",
                paths.join(", ")
            )
        });
        return Ok(Some(Found { line, place, cwd: cwd.to_path_buf(), files }));
    };
    let now = rules_files(&before.cwd, env)?;
    let mut changed: Vec<String> = now
        .iter()
        .filter(|(path, (_, hash))| before.files.get(*path).is_none_or(|(_, h)| h != hash))
        .map(|(path, _)| path.clone())
        .collect();
    changed.extend(
        before.files.keys().filter(|p| !now.contains_key(*p)).map(|p| format!("{p} (removed)")),
    );
    if changed.is_empty() {
        return Ok(None);
    }
    let line =
        format!("Lords rules changed since this session started: re-read {}", changed.join(", "));
    Ok(Some(Found { line: Some(line), place, cwd: before.cwd, files: now }))
}
