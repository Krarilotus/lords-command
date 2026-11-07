//! `lords`: the Lords Command CLI. Small on purpose: the pre-commit hook runs it.

mod panel;

use lords_core::check;
use lords_core::config::Config;
use lords_core::model::Visibility;
use lords_core::ops::{self, Promote, Retire};
use lords_core::privacy::{self, Rules};
use lords_core::process::ExitCode;
use lords_core::{git, hooks, sync, Finding, Severity, Stack, Stacks};
use std::path::{Path, PathBuf};

const USAGE: &str = "\
lords: one home for how you work with agents

usage:
  lords check [--public | --stack] [--strict] [--json]   --stack: the entries only (a stack repo's pre-commit)
  lords import [--store NAME] [--no-commit]
  lords retire ID --reason TEXT [--superseded-by ID] [--date YYYY-MM-DD] [--no-commit]
  lords promote ID [--strip | --split] [--no-commit]
  lords privacy --staged [--secrets-only]   staged files and their paths; a binary needs `lords-binary` in .gitattributes
  lords privacy --message FILE [--staged] [--secrets-only]   a commit message (the commit-msg hook)
  lords privacy --history [--private-stack DIR]   every commit's message and patch (the pre-push check)
  lords privacy FILE...
  lords hooks install [--repo DIR] [--secrets-only] [--force]   the pre-commit, pre-merge-commit and commit-msg hooks; in a stack repo also `check --stack`, a pre-push and a post-commit that runs `deliver`
  lords hash PATH
  lords sync [--user | --repo NAME [--place DIR]] [--adapter ID] [--stage install] [--json] [--apply] [--deadline-ms N]
    (a write of Codex's hooks: trusts Lords' own through Codex's app-server, names each hook that isn't trusted, exit 1)
  lords sync --safe-only [--user | --repo NAME [--place DIR]] [--json] [--apply]   only what needs no one: a place
    with anything else is held whole and named with why; no import or retirement (listed); exit 1 while a place is held
  lords sync --restore BACKUP_DIR   undo one apply's owner-file edits, each only while it holds what sync wrote
  lords deliver [--on-commit DIR | --repo NAME --place DIR --new-worktree] [--home DIR]   the stores' committed
    entries into every place (or one new worktree), safe-only, under the apply lock, place by place; what it held, or
    why it failed, goes into the state file `lords paths` reads (exit 0 nothing held, 1 held, 2 failed). The stores'
    post-commit and each repo's post-checkout hook run it. --protocol: the protocol this copy speaks
  lords uninstall [--user | --repo NAME] [--apply]   remove the tool's paths that still match its ledger
  lords uninstall --repo NAME --place DIR [--apply] [--json]   one checkout or worktree's recorded copies
    (a changed copy is kept with its record; exit 0 no record of the place remains, 1 one does, 2 refused or stopped)
  lords uninstall --target NAME [--apply]   a target the stack no longer declares: its copies that still hold what
    sync wrote, with their records (a changed copy is kept with its record; exit codes as for --place)
  lords adopt [--list | PATH...]   record paths from before the ledger that hold exactly what sync writes there
  lords set-aside PATH... [--yes]   move paths that aren't the tool's into a new backup dir
  (sync, deliver, uninstall, adopt, set-aside: --home DIR, the tool's own dir; default ~/.lords-command)
  (sync --apply also installs each declared repo's post-checkout hook, which runs `deliver` for a new worktree)
  lords hooks install --post-checkout [--repo DIR]   a new worktree gets its place files (`sync --place`)
  lords compose [--repo NAME [--role ROLE | --child | --child-whole]] [--adapter ID] [--json]   what a harness reads there, with sources
  lords show ID   an entry's whole text, which its core line stands for at start (without frontmatter)
  lords sizes [--json]   every file sync writes, by section; fails past the warning line
  lords paths [--all] [--json] [--codex PATH] [--deadline-ms N]   which T3-delegated sessions start with their role's lines;
    exit 1 when one doesn't (Codex: asks its app-server whether it trusts Lords' hook); places not in use are one
    line unless --all
  lords guard rm [--shell bash|powershell]   the PreToolUse hook on shell calls: refuses an `rm` whose path holds a
    variable not written as quoted ${VAR:?}, a git call that would skip or repoint its hooks, and git's help on a topic
    (it opens a browser); --shell: the language of a `Bash` tool's text (Codex on Windows: powershell)
  lords guard queue [--deadline-ms N]   Claude's Stop hook: holds a turn's end while items may wait in its T3 queue
  lords guard session   Claude's SessionStart hook: records the Lords files the session reads
  lords guard turn   Claude's UserPromptSubmit hook: the clock, and the Lords files changed since the session started
  lords guard child [--home DIR]   the SubagentStart hook: a native child starts with its role's start file in its place
  lords guard role [--home DIR] [--adapter ID]   the UserPromptSubmit hook: a session gets the start of the role its launch names
  lords guard time   the PreToolUse hook on editor writes to Markdown and text files and on T3 messages: refuses a
    placeholder clock time; reminds of one more than 5 minutes after the clock not marked as a plan
  lords guard install [--home DIR]   install or update the guard copy the hooks run (sync never writes it)
  lords guard --guards   the guards this copy runs; sync names one in a hook only while the installed copy lists it
  lords panel serve [--port N]   the window on another PC reads and edits these stacks through it (127.0.0.1:7879)
  lords panel add ALIAS --bridge DIR [--embedded] [--apply]   set a PC this one reaches as ALIAS up for the panel;
    prints the exact targets on both PCs, and with --apply carries them out (backups first, the route checked last);
    --embedded: for a client embedded in another app, no lords-command.exe, README, LICENSE or Start-menu entry

stacks: --public-stack DIR, --private-stack DIR, else config.txt (beside the exe, or ~/.lords-command/),
else the public stack is <this git repo>/stack.

check --public scans every file of the public repo (tracked and untracked, not ignored) and the public
stack alone; it is the pre-push check.";

struct Args {
    words: Vec<String>,
}

impl Args {
    fn flag(&mut self, name: &str) -> bool {
        let before = self.words.len();
        self.words.retain(|w| w != name);
        self.words.len() != before
    }

    fn value(&mut self, name: &str) -> Result<Option<String>, String> {
        let Some(i) = self.words.iter().position(|w| w == name) else { return Ok(None) };
        if i + 1 >= self.words.len() {
            return Err(format!("{name} needs a value"));
        }
        let v = self.words.remove(i + 1);
        self.words.remove(i);
        Ok(Some(v))
    }

    fn positional(&mut self) -> Option<String> {
        let i = self.words.iter().position(|w| !w.starts_with("--"))?;
        Some(self.words.remove(i))
    }

    fn done(&self) -> Result<(), String> {
        match self.words.first() {
            Some(w) => Err(format!("unexpected `{w}`")),
            None => Ok(()),
        }
    }
}

struct Paths {
    public: PathBuf,
    private: Option<PathBuf>,
}

fn paths(args: &mut Args) -> Result<Paths, String> {
    let cfg = Config::load()?;
    let public = args.value("--public-stack")?.map(PathBuf::from).or(cfg.public).or_else(|| {
        let cwd = std::env::current_dir().ok()?;
        let top = git::toplevel(&cwd)?;
        let s = top.join("stack");
        s.join("stack.toml").is_file().then_some(s)
    });
    let public =
        public.ok_or("no public stack: pass --public-stack or set `public` in config.txt")?;
    let private = args.value("--private-stack")?.map(PathBuf::from).or(cfg.private);
    // Absolute, as sync writes them: every path an agent file shows is then the one a session can open.
    let abs = |p: PathBuf| std::path::absolute(&p).unwrap_or(p);
    Ok(Paths { public: abs(public), private: private.map(abs) })
}

fn print_findings(findings: &[Finding], json: bool) {
    if json {
        println!("{}", serde_json::to_string_pretty(findings).unwrap_or_default());
        return;
    }
    for f in findings {
        println!("{f}");
    }
    let count = |s| findings.iter().filter(|f| f.severity == s).count();
    println!(
        "{} errors, {} warnings, {} candidates for the owner",
        count(Severity::Error),
        count(Severity::Warning),
        count(Severity::Candidate)
    );
}

fn public_rules(public: &Path, private: Option<&Stack>) -> Result<Rules, String> {
    Rules::public().with_allow_from(public).with_private(private)
}

/// `lords guard rm`: the tool-call guard a harness runs before a shell call (guard.rs). Reads the event on stdin and
/// prints a denial, or nothing. It never fails the call: a usage error or input it can't read is a note on stderr and
/// exit 0 (orchestrator, 30 Oct: Claude Code takes exit 2 from a PreToolUse hook as a refusal). `lords guard
/// --self-test` prints `ok` when the guard refuses its witnesses, one per refusal; sync runs it before installing the
/// copy hooks name.
fn cmd_guard(mut a: Args) -> ExitCode {
    let note = |line: &str| say_err(&format!("lords guard: {line}; this call is let through"));
    if a.flag("--help") || a.flag("-h") {
        say("usage: lords guard rm [--shell bash|powershell]   (the PreToolUse hook on shell calls: rm, git's hooks, git's help; the event on stdin)");
        say("       lords guard queue [--deadline-ms N]   (Claude Code's Stop hook; the event on stdin)");
        say("       lords guard session | turn   (Claude Code's SessionStart and UserPromptSubmit hooks)");
        say("       lords guard child   (the SubagentStart hook: a native child's start file)");
        say("       lords guard role   (the UserPromptSubmit hook: the start of the role a launch names)");
        say("       lords guard time   (the PreToolUse hook on editor writes and T3 messages: clock times)");
        say("       lords guard helper   (the PreToolUse hook on Claude Code's native helper tool: refused in T3)");
        return ExitCode::SUCCESS;
    }
    if a.flag("--self-test") {
        let witnesses = [
            ("Bash", "rm -f $OUT/x"),
            ("Bash", "git commit --no-verify -m x"),
            ("Bash", "git help config"),
            ("PowerShell", "git commit --help"),
        ];
        let ok = witnesses.iter().all(|(tool, c)| {
            let event = serde_json::json!({"tool_name": tool, "tool_input": {"command": c}});
            lords_core::guard::rm_hook(&event.to_string()).contains("\"deny\"")
        });
        let time = serde_json::json!({"tool_name": "mcp__t3-code__t3_thread_send", "tool_input": {"message": "14:xx"}});
        let (now, zone) = lords_core::guard::system_clock();
        let ok = ok
            && lords_core::time_guard::time_hook(&time.to_string(), now, zone.as_ref())
                .contains("\"deny\"");
        let ok = ok
            && lords_core::helper_guard::helper_hook(r#"{"tool_name": "Agent"}"#, || Ok(true))
                .stdout
                .contains("\"deny\"");
        say(if ok { "ok" } else { "the guard let its witness through" });
        return if ok { ExitCode::SUCCESS } else { ExitCode::from(1) };
    }
    // The guards this copy runs, which sync asks before it names one in a hook (D20): an earlier copy answers this with
    // the rm usage note and nothing on stdout.
    if a.flag("--guards") {
        say(&format!("lords guards {}", lords_core::guard::GUARDS.join(" ")));
        return ExitCode::SUCCESS;
    }
    let which = a.positional();
    if which.as_deref() == Some("install") {
        return guard_install(a);
    }
    if which.as_deref() == Some("queue") {
        return guard_queue(a);
    }
    if let Some(w @ ("session" | "turn")) = which.as_deref() {
        return guard_rules(w, a);
    }
    if let Some(w @ ("child" | "role")) = which.as_deref() {
        return guard_child(w, a);
    }
    if which.as_deref() == Some("time") {
        return guard_time(a);
    }
    if which.as_deref() == Some("helper") {
        return guard_helper(a);
    }
    let shell = match a.value("--shell").as_ref().map(|s| s.as_deref()) {
        Ok(None | Some("bash")) => Some(lords_core::guard::Shell::Bash),
        Ok(Some("powershell")) => Some(lords_core::guard::Shell::PowerShell),
        _ => None,
    };
    let (true, Ok(()), Some(shell)) = (which.as_deref() == Some("rm"), a.done(), shell) else {
        note("usage `lords guard rm [--shell bash|powershell]` (event on stdin)");
        return ExitCode::SUCCESS;
    };
    let input = match std::io::read_to_string(std::io::stdin()) {
        Ok(i) => i,
        Err(e) => {
            note(&format!("the event can't be read ({e})"));
            return ExitCode::SUCCESS;
        }
    };
    let out = lords_core::guard::shell_hook(&input, shell);
    if !out.is_empty() {
        say(&out);
    }
    ExitCode::SUCCESS
}

/// `lords guard time`: the `PreToolUse` hook on editor writes and T3 messages (time_guard.rs). Reads the event on stdin
/// and the system clock, and prints a denial or nothing; like the other guards it never fails: every problem is a note
/// on stderr and exit 0.
fn guard_time(a: Args) -> ExitCode {
    let note = |line: &str| say_err(&format!("lords guard time: {line}; this call is let through"));
    if a.done().is_err() {
        note("usage `lords guard time` (event on stdin)");
        return ExitCode::SUCCESS;
    }
    let input = match std::io::read_to_string(std::io::stdin()) {
        Ok(i) => i,
        Err(e) => {
            note(&format!("the event can't be read ({e})"));
            return ExitCode::SUCCESS;
        }
    };
    let (now, zone) = lords_core::guard::system_clock();
    let out = lords_core::time_guard::time_hook(&input, now, zone.as_ref());
    if !out.is_empty() {
        say(&out);
    }
    ExitCode::SUCCESS
}

/// `lords guard helper`: the `PreToolUse` hook on Claude Code's native helper tool (helper_guard.rs). Reads the event on
/// stdin, and only for a helper start this session's `T3CODE_THREAD_ID`, `CLAUDE_PID` and that process's arguments
/// (t3.rs); prints a denial in a session T3 started, or nothing. Like the other guards it never fails: every problem is
/// a note on stderr and exit 0.
fn guard_helper(a: Args) -> ExitCode {
    let note =
        |line: &str| say_err(&format!("lords guard helper: {line}; this call is let through"));
    if a.done().is_err() {
        note("usage `lords guard helper` (event on stdin)");
        return ExitCode::SUCCESS;
    }
    let input = match std::io::read_to_string(std::io::stdin()) {
        Ok(i) => i,
        Err(e) => {
            note(&format!("the event can't be read ({e})"));
            return ExitCode::SUCCESS;
        }
    };
    let var = |name: &str| std::env::var(name).ok();
    let answer = lords_core::helper_guard::helper_hook(&input, || {
        let (thread, pid) = (var("T3CODE_THREAD_ID"), var("CLAUDE_PID"));
        lords_core::t3::session(
            thread.as_deref(),
            pid.as_deref(),
            lords_core::process::command_line,
        )
    });
    if let Some(n) = answer.note {
        note(&n);
    }
    if !answer.stdout.is_empty() {
        say(&answer.stdout);
    }
    ExitCode::SUCCESS
}

/// `lords guard session` and `lords guard turn`: the hooks Claude Code runs when a session and a prompt start
/// (guard.rs). Each reads the event on stdin; `turn` prints the prompt's context, `session` nothing. Like the other
/// guards they never fail: every problem is a note on stderr and exit 0.
fn guard_rules(which: &str, a: Args) -> ExitCode {
    use lords_core::guard::{session_hook, system_clock, turn_hook, RulesEnv};
    let note = |line: &str| say_err(&format!("lords guard {which}: {line}"));
    if a.done().is_err() {
        note(&format!("usage `lords guard {which}` (event on stdin)"));
        return ExitCode::SUCCESS;
    }
    let input = match std::io::read_to_string(std::io::stdin()) {
        Ok(i) => i,
        Err(e) => {
            note(&format!("the event can't be read ({e})"));
            return ExitCode::SUCCESS;
        }
    };
    let home = match lords_core::sync::default_home() {
        Ok(h) => h,
        Err(e) => {
            note(&e);
            return ExitCode::SUCCESS;
        }
    };
    let user_config = std::env::var_os("CLAUDE_CONFIG_DIR")
        .map(PathBuf::from)
        .or_else(|| lords_core::config::home().map(|h| h.join(".claude")));
    let env = RulesEnv { home, user_config };
    let answer = if which == "session" {
        session_hook(&input, &env)
    } else {
        let (now, zone) = system_clock();
        turn_hook(&input, &env, now, zone.as_ref())
    };
    if let Some(n) = answer.note {
        note(&n);
    }
    if !answer.stdout.is_empty() {
        say(&answer.stdout);
    }
    ExitCode::SUCCESS
}

/// `lords guard child [--home DIR]`: the `SubagentStart` hook (Claude Code, Codex) that gives a native child its
/// place's start file (guard.rs), in one read. `lords guard role [--home DIR] [--adapter ID]`: the `UserPromptSubmit`
/// hook that gives a prompt carrying the launcher's role signal that role's start (guard.rs, `role_hook`); `--adapter`
/// names its harness. `--home`: the tool home sync ran with, whose apply lock it takes (sync names it in the hook it writes;
/// default ~/.lords-command). Reads the event on stdin; like the other guards it never fails: every problem is a note
/// on stderr and exit 0.
fn guard_child(which: &str, mut a: Args) -> ExitCode {
    let note = |line: &str| say_err(&format!("lords guard {which}: {line}"));
    let usage = if which == "role" {
        "usage `lords guard role [--home DIR] [--adapter ID]` (event on stdin)".to_string()
    } else {
        format!("usage `lords guard {which} [--home DIR]` (event on stdin)")
    };
    let home = match a.value("--home") {
        Ok(h) => h.map(PathBuf::from),
        Err(_) => {
            note(&usage);
            return ExitCode::SUCCESS;
        }
    };
    // The harness the role hook runs for, whose place file its start goes beside (F10-R1-03).
    let adapter = if which != "role" {
        None
    } else {
        match a.value("--adapter") {
            Ok(id) => id,
            Err(_) => {
                note(&usage);
                return ExitCode::SUCCESS;
            }
        }
    };
    if a.done().is_err() {
        note(&usage);
        return ExitCode::SUCCESS;
    }
    let input = match std::io::read_to_string(std::io::stdin()) {
        Ok(i) => i,
        Err(e) => {
            note(&format!("the event can't be read ({e})"));
            return ExitCode::SUCCESS;
        }
    };
    // The tool home holds sync's apply lock (guard.rs `child_text`).
    let home = match home.map(Ok).unwrap_or_else(lords_core::sync::default_home) {
        Ok(h) => h,
        Err(e) => {
            note(&format!("no start given ({e})"));
            return ExitCode::SUCCESS;
        }
    };
    let answer = if which == "role" {
        let env = lords_core::guard::RoleEnv { home, adapter };
        lords_core::guard::role_hook(&input, &env)
    } else {
        lords_core::guard::child_hook(&input, &home)
    };
    if let Some(n) = answer.note {
        note(&n);
    }
    if !answer.stdout.is_empty() {
        say(&answer.stdout);
    }
    ExitCode::SUCCESS
}

/// `lords guard queue [--deadline-ms N]`: the Stop hook Claude Code runs when a turn would end (guard.rs). Reads the
/// event on stdin and prints a block, or nothing; like `guard rm` it never fails: every problem is a note on stderr
/// and exit 0. `--deadline-ms`: the T3 CLI's deadline, `QUEUE_DEADLINE` unless given.
fn guard_queue(mut a: Args) -> ExitCode {
    use lords_core::guard::{queue_hook, QueueEnv, QUEUE_DEADLINE};
    let note = |line: &str| say_err(&format!("lords guard queue: {line}; this stop goes through"));
    let deadline = match a.value("--deadline-ms").map(|v| v.map(|ms| ms.parse::<u64>())) {
        Ok(None) => QUEUE_DEADLINE,
        Ok(Some(Ok(ms))) => std::time::Duration::from_millis(ms),
        Ok(Some(Err(_))) => {
            note("--deadline-ms takes a number of milliseconds");
            return ExitCode::SUCCESS;
        }
        Err(e) => {
            note(&e);
            return ExitCode::SUCCESS;
        }
    };
    if a.done().is_err() {
        note("usage `lords guard queue [--deadline-ms N]` (event on stdin)");
        return ExitCode::SUCCESS;
    }
    let input = match std::io::read_to_string(std::io::stdin()) {
        Ok(i) => i,
        Err(e) => {
            note(&format!("the event can't be read ({e})"));
            return ExitCode::SUCCESS;
        }
    };
    let home = match lords_core::sync::default_home() {
        Ok(h) => h,
        Err(e) => {
            note(&e);
            return ExitCode::SUCCESS;
        }
    };
    let env = QueueEnv {
        thread: std::env::var("T3CODE_THREAD_ID").ok(),
        cli: std::env::var_os("T3CODE_CLI_PATH").map(PathBuf::from),
        home,
        deadline,
    };
    let answer = queue_hook(&input, &env);
    if let Some(n) = answer.note {
        note(&n);
    }
    if !answer.stdout.is_empty() {
        say(&answer.stdout);
    }
    ExitCode::SUCCESS
}

/// `lords guard install [--home DIR]`: installs or updates the guard copy hooks run from this `lords` (sync only
/// refers to it; orchestrator, 30 Oct). A user's command, with the usual exit codes: 0 done, 2 refused (the old copy
/// stays).
fn guard_install(mut a: Args) -> ExitCode {
    let done = tool_home(&mut a).and_then(|home| {
        a.done()?;
        let me = std::env::current_exe().map_err(|e| e.to_string())?;
        let wrote = lords_core::sync::install_guard(&home, &me)?;
        let aside = lords_core::sync::guard_aside(&home)?;
        Ok((lords_core::sync::guard_place(&home)?.path().to_path_buf(), wrote, aside))
    });
    match done {
        Ok((place, wrote, aside)) => {
            let what = if wrote { "installed" } else { "already current" };
            say(&format!("guard {what}: {} (passes its self-test)", place.display()));
            for kept in &aside {
                say(&format!("beside it: {kept}"));
            }
            ExitCode::SUCCESS
        }
        Err(e) => {
            say_err(&format!("lords guard install: {e}"));
            ExitCode::from(2)
        }
    }
}

// Every `lords guard` write may fail (a harness or caller that closed its pipe): never a panic, the exit code stands
// (rm-guard review r1, R8; fix check r5, R10).
fn say(line: &str) {
    use std::io::Write;
    let _ = writeln!(std::io::stdout().lock(), "{line}").and_then(|()| std::io::stdout().flush());
}

fn say_err(line: &str) {
    use std::io::Write;
    let _ = writeln!(std::io::stderr().lock(), "{line}");
}

fn cmd_check(mut a: Args) -> Result<bool, String> {
    let public_only = a.flag("--public");
    let stack_only = a.flag("--stack");
    let strict = a.flag("--strict");
    let json = a.flag("--json");
    let explicit = a.words.iter().any(|w| w == "--public-stack");
    let explicit_private = a.words.iter().any(|w| w == "--private-stack");
    let mut p = paths(&mut a)?;
    a.done()?;
    // `--public` and `--stack` check the repo they run in (its hooks), not whatever config.txt points at.
    let top = std::env::current_dir().ok().and_then(|d| git::toplevel(&d));
    if (public_only || stack_only) && !explicit {
        if let Some(stack) =
            top.as_ref().map(|t| t.join("stack")).filter(|s| s.join("stack.toml").is_file())
        {
            p.public = stack;
        }
    }
    if stack_only && !explicit_private {
        if let Some(t) = top.filter(|t| t.join("stack.toml").is_file()) {
            p.private = Some(t);
        }
    }
    let private = p.private.as_deref().map(Stack::load).transpose()?;
    let rules = public_rules(&p.public, private.as_ref())?;
    if public_only {
        let stacks = Stacks::load(&p.public, None)?;
        let mut findings = check::check(&stacks, &rules);
        let repo = git::toplevel(&p.public).ok_or("the public stack is not in a git repo")?;
        let (scan, tree) = check::public_tree(&repo, &rules)?;
        // Entries are scanned as part of the tree; keep one finding per hit.
        findings.retain(|f| f.code != "privacy");
        findings.extend(tree);
        print_findings(&findings, json);
        if !json {
            println!(
                "public: {} entries; {} files scanned, {} not text: {}",
                stacks.public.entries.len(),
                scan.files_scanned,
                scan.binary.len(),
                scan.binary.iter().map(|b| b.display().to_string()).collect::<Vec<_>>().join(", ")
            );
        }
        return Ok(!check::has_errors(&findings, strict));
    }
    // Loaded as sync loads them (the repos' own skills are entries too), so check measures what sync writes.
    drop(private);
    let stacks = Stacks::load(&p.public, p.private.as_deref())?;
    // `--stack`: the entries only, without drift (the slow part) or the hook probe, for a stack repo's pre-commit.
    let findings = if stack_only {
        check::check_stack(&stacks, &rules)
    } else {
        let mut f = check::check(&stacks, &rules);
        f.extend(drift(&stacks));
        f
    };
    print_findings(&findings, json);
    if !json {
        let n = |v| stacks.stack(v).map(|s| s.entries.len()).unwrap_or(0);
        println!("{} public and {} private entries", n(Visibility::Public), n(Visibility::Private));
    }
    Ok(!check::has_errors(&findings, strict))
}

fn drift(stacks: &Stacks) -> Vec<Finding> {
    match sync::default_home() {
        Ok(home) => sync::drift_findings(stacks, &home),
        Err(e) => vec![Finding::warning("drift", format!("no sync plan: {e}"))],
    }
}

/// The tool's own dir: `--home DIR`, else `~/.lords-command`.
fn tool_home(a: &mut Args) -> Result<PathBuf, String> {
    match a.value("--home")? {
        Some(h) => Ok(std::path::absolute(PathBuf::from(&h)).unwrap_or_else(|_| PathBuf::from(h))),
        None => sync::default_home(),
    }
}

fn print_applied(applied: &sync::Applied) {
    for line in &applied.done {
        println!("{line}");
    }
    if let Some(b) = &applied.backups {
        println!(
            "backups of the owner files changed: {} (`lords sync --restore` that dir)",
            b.display()
        );
    }
    // Last, so a caller sees it without reading every line (design note 2026-11-02-lords-codex-trust, §2.3).
    for line in &applied.codex {
        println!("{line}");
    }
    if !applied.codex.is_empty() {
        println!("applied, {} Codex hook(s) not trusted", applied.codex.len());
    }
}

/// What a safe-only plan left out (D13): each place held, with its items' reasons, then the store work it never does.
fn print_held(plan: &sync::Plan, held: &sync::Held) {
    let item = |i: &sync::HeldItem| {
        let note = if i.note.is_empty() { String::new() } else { format!(": {}", i.note) };
        println!("   {}  {}{note}", i.reason, i.path.display());
    };
    for p in &held.places {
        println!("\nheld {}", p.place);
        p.items.iter().for_each(item);
    }
    if !held.everything.is_empty() {
        println!("\nheld everywhere, by what belongs to no place:");
        held.everything.iter().for_each(item);
    }
    if !plan.imports.is_empty() || !held.retirements.is_empty() {
        println!("\nstore work, which only `lords sync --apply` does:");
        for m in &plan.imports {
            println!("   memory store {}: {} new, {} changed", m.store, m.added, m.changed);
        }
        held.retirements.iter().for_each(item);
    }
}

/// `lords deliver` (deliver.rs; F8): what changed, then each held place, repo hook, failure and piece of store work
/// with the command that resolves it, and where the state file is. Exit 0 with nothing held, 1 with a place or a repo's
/// hook held, 2 when the delivery failed (the state file says why, too).
fn cmd_deliver(mut a: Args) -> Result<bool, String> {
    use lords_core::deliver;
    if a.flag("--protocol") {
        a.done()?;
        println!("lords deliver {}", deliver::PROTOCOL);
        return Ok(true);
    }
    // The commits a delivery makes itself (its store work) start no delivery: its stores' post-commit hook, which every
    // such commit runs, exits at once with this set (design note C7).
    std::env::set_var(deliver::MARKER, "1");
    let home = tool_home(&mut a)?;
    let on_commit = a.value("--on-commit")?.map(PathBuf::from);
    let repo = a.value("--repo")?;
    let place = a.value("--place")?.map(PathBuf::from);
    let new_worktree = a.flag("--new-worktree");
    let p = paths(&mut a)?;
    a.done()?;
    let mut opts = match (repo, place, new_worktree) {
        (Some(r), Some(d), true) => deliver::Options::worktree(&home, &r, &d),
        (None, None, false) => deliver::Options::whole(&home),
        _ => return Err("--repo, --place and --new-worktree go together: one new worktree".into()),
    };
    if on_commit.is_some() && opts.worktree.is_some() {
        return Err("--on-commit is a store commit's delivery, of the whole machine".into());
    }
    opts.on_commit = on_commit;
    let r = deliver::run(&p.public, p.private.as_deref(), &opts)?;
    for line in &r.done {
        println!("{line}");
    }
    let s = &r.state;
    for h in &s.places {
        println!("held {}: {}", h.place, h.command);
        for i in &h.items {
            let note = if i.note.is_empty() { String::new() } else { format!(": {}", i.note) };
            println!("   {}  {}{note}", i.reason, i.path);
        }
    }
    for i in &s.everything {
        println!("held everywhere: {}  {}: {}", i.reason, i.path, i.note);
    }
    for n in s.hooks.iter().chain(&s.store_work) {
        println!("held {}: {}; {}", n.what, n.why, n.command);
    }
    if r.not_a_trigger {
        println!("not delivered: [delivery] on_commit doesn't list this store");
    } else if r.unchanged {
        println!(
            "delivered already: the stores' committed stacks are as the last delivery found them"
        );
    } else {
        println!("delivered: {} change(s); {} place(s) held", r.done.len(), s.places.len());
    }
    println!("state: {}", r.file.display());
    Ok(!r.holds())
}

/// A plan of the current config for every target and place: what `restore` and `set-aside` check paths against.
fn whole_plan(a: &mut Args, home: PathBuf) -> Result<sync::Plan, String> {
    let p = paths(a)?;
    let stacks = Stacks::load(&p.public, p.private.as_deref())?;
    sync::plan(&stacks, &sync::Options { home, ..sync::Options::default() })
}

fn cmd_sync(mut a: Args) -> Result<bool, String> {
    let home = tool_home(&mut a)?;
    if let Some(dir) = a.value("--restore")? {
        let plan = whole_plan(&mut a, home)?;
        a.done()?;
        for line in sync::restore(&plan, Path::new(&dir))? {
            println!("{line}");
        }
        return Ok(true);
    }
    let safe_only = a.flag("--safe-only");
    let stage = match a.value("--stage")?.as_deref() {
        None if safe_only => sync::Stage::SafeOnly,
        Some(_) if safe_only => {
            return Err("--safe-only is a stage of its own: drop --stage".into());
        }
        None | Some("full") => sync::Stage::Full,
        Some("install") => sync::Stage::Install,
        Some(other) => return Err(format!("--stage {other}: `install` or `full`")),
    };
    let apply = a.flag("--apply");
    let json = a.flag("--json");
    let user_only = a.flag("--user");
    let repo = a.value("--repo")?;
    let adapter = a.value("--adapter")?;
    let place = a.value("--place")?.map(PathBuf::from);
    let deadline = match a.value("--deadline-ms")? {
        None => lords_core::codex_trust::DEADLINE,
        Some(ms) => std::time::Duration::from_millis(
            ms.parse().map_err(|_| "--deadline-ms takes a number of milliseconds")?,
        ),
    };
    let p = paths(&mut a)?;
    a.done()?;
    if place.is_some() && repo.is_none() {
        return Err("--place needs --repo (a checkout or worktree of that repo)".into());
    }
    if let Some(id) = adapter.as_deref().filter(|id| lords_core::adapters::get(id).is_none()) {
        return Err(format!(
            "unknown adapter `{id}` (known: {})",
            lords_core::adapters::ids().join(", ")
        ));
    }
    let stacks = Stacks::load(&p.public, p.private.as_deref())?;
    let codex = lords_core::codex_trust::Codex::found(deadline);
    let opts = sync::Options {
        repo,
        user_only,
        adapter,
        place,
        home,
        uninstall: false,
        target: None,
        stage,
        codex,
        new_worktree: false,
    };
    // An apply of one place or of a safe-only plan writes this very plan: it is made under the lock it applies with,
    // so no other writer's half-done apply is in it (F8, C5; D23). A full apply's own plans are made under its lock
    // (`import_and_apply`); the one shown here is the dry run's.
    let settings = stacks.delivery();
    let lock = match apply && (opts.place.is_some() || safe_only) {
        true => Some(sync::ApplyLock::wait(&opts.home, settings.lock_wait())??),
        false => None,
    };
    let plan = sync::plan(&stacks, &opts)?;
    if json {
        println!("{}", serde_json::to_string_pretty(&plan).unwrap_or_default());
    } else {
        print_plan(&plan);
        if let Some(held) = &plan.held {
            print_held(&plan, held);
        }
    }
    // A place skipped because its file would be cut never reads as success, planned or applied; nor does a place a
    // safe-only plan held (D13: exit 1), whichever reason held it.
    let whole = !plan.over_limit && !plan.held.as_ref().is_some_and(sync::Held::holds);
    if !apply {
        if !json {
            match plan.applies() {
                Ok(r) => println!(
                    "applies: yes; it removes {} path(s) and forgets {} record(s) (a forget leaves the path as it is)",
                    r.removes, r.forgets
                ),
                Err(why) => println!("applies: no: {why}"),
            }
            println!(
                "dry run: nothing written (`--apply` writes; owner files are backed up first)"
            );
        }
        return Ok(whole);
    }
    // One worktree: only its files, no import or retiring, which commit to the stack. Nor does a safe-only plan: it
    // lists that store work instead, holds what isn't safe (D13), and writes place by place; what it held goes into the
    // delivery's state where delivery is set up (F8, D5).
    let applied = match lock {
        Some(lock) => {
            let applied = sync::apply_held(&plan, &lock).map_err(|e| e.to_string())?;
            if safe_only {
                lords_core::deliver::settle_safe_only(&opts.home, &settings, &plan, &applied)?;
            }
            applied
        }
        None => sync::import_and_apply(&p.public, p.private.as_deref(), &opts)?,
    };
    print_applied(&applied);
    for h in &applied.held {
        println!("\nheld {}, put back whole: {}", h.place, h.sync);
        for i in &h.items {
            println!("   {}  {}: {}", i.reason, i.path.display(), i.note);
        }
    }
    let whole = whole && applied.held.is_empty();
    // A Codex hook that doesn't run never reads as success either (design note 2026-11-02-lords-codex-trust, §2.3).
    Ok(whole && applied.codex.is_empty())
}

/// One record of a place, as `uninstall --place` lists it.
struct PlaceRow {
    path: String,
    action: &'static str,
    note: String,
}

/// What `uninstall --place` found and did, for its text and its JSON (format 1).
#[derive(Default)]
struct PlaceRun {
    repo: Option<String>,
    place: Option<PathBuf>,
    applied: bool,
    planned: Vec<PlaceRow>,
    /// What would remain (a dry run) or what the ledger still records after the apply; `None` when that couldn't be
    /// read back.
    remaining: Option<Vec<PlaceRow>>,
}

fn place_rows<'a>(items: impl IntoIterator<Item = &'a sync::Item>) -> Vec<PlaceRow> {
    items
        .into_iter()
        .map(|i| PlaceRow {
            path: i.path.display().to_string(),
            action: match i.action {
                sync::Action::Remove => "remove",
                sync::Action::RemoveBlock => "remove-block",
                sync::Action::Forget => "forget",
                _ => "keep",
            },
            note: i.notes.join("; "),
        })
        .collect()
}

impl PlaceRun {
    fn json(&self, exit: u8, error: Option<&String>) -> String {
        let rows = |r: &[PlaceRow]| -> Vec<serde_json::Value> {
            r.iter()
                .map(|r| serde_json::json!({"path": r.path, "action": r.action, "note": r.note}))
                .collect()
        };
        serde_json::to_string_pretty(&serde_json::json!({
            "format": 1,
            "repo": self.repo,
            "place": self.place.as_ref().map(|p| p.display().to_string()),
            "applied": self.applied,
            "exit": exit,
            "error": error,
            "planned": rows(&self.planned),
            "remaining": self.remaining.as_deref().map(rows),
        }))
        .unwrap_or_default()
    }

    fn print(&self) {
        let Some(place) = &self.place else { return };
        println!(
            "repo {}, place {}: {} record(s) of it in the ledger",
            self.repo.as_deref().unwrap_or_default(),
            place.display(),
            self.planned.len()
        );
        for r in &self.planned {
            println!("  {:<12} {}\n               {}", r.action, r.path, r.note);
        }
        let head = if self.applied {
            "applied; read back from the ledger"
        } else {
            "dry run: nothing changed (`--apply` acts); after it"
        };
        match &self.remaining {
            None => println!("{head}: couldn't be read"),
            Some(r) if r.is_empty() => println!("{head}: no record of the place remains"),
            Some(r) => {
                println!("{head}: {} record(s) of the place remain:", r.len());
                for r in r {
                    println!("  {:<12} {}\n               {}", r.action, r.path, r.note);
                }
            }
        }
    }
}

/// `lords uninstall --repo NAME --place DIR`: one checkout or worktree's records (design note
/// 2026-11-01-lords-uninstall-place Q2). `Ok(true)` when no record of the place remains, `Ok(false)` while one does.
fn uninstall_place(a: &mut Args, dir: &Path, run: &mut PlaceRun) -> Result<bool, String> {
    let home = tool_home(a)?;
    let apply = a.flag("--apply");
    let user_only = a.flag("--user");
    run.repo = a.value("--repo")?;
    let p = paths(a)?;
    a.done()?;
    if run.repo.is_none() {
        return Err("--place needs --repo (a checkout or worktree of that repo)".into());
    }
    let stacks = Stacks::load(&p.public, p.private.as_deref())?;
    let opts = sync::Options {
        repo: run.repo.clone(),
        user_only,
        place: Some(dir.to_path_buf()),
        home,
        uninstall: true,
        ..sync::Options::default()
    };
    let plan = sync::plan(&stacks, &opts)?;
    run.place = plan.roots.first().map(|r| r.path().to_path_buf());
    run.planned = place_rows(&plan.items);
    if !apply {
        run.remaining =
            Some(place_rows(plan.items.iter().filter(|i| i.action == sync::Action::Report)));
        return Ok(run.remaining.as_ref().is_some_and(Vec::is_empty));
    }
    run.applied = true;
    let done = sync::apply(&plan);
    // The answer is what the ledger still records of the place, read back with a fresh plan: after an error too, so
    // what went before it is known.
    let after = sync::plan(&stacks, &opts);
    if let Ok(after) = &after {
        run.remaining = Some(place_rows(&after.items));
    }
    done?;
    after.map_err(|e| format!("applied, but the place couldn't be read back: {e}"))?;
    Ok(run.remaining.as_ref().is_some_and(Vec::is_empty))
}

/// `lords uninstall --target NAME`: one target the stack no longer declares, its copies that still hold what their
/// records say (D21, design note 2026-11-07-lords-delivery-integrity §3). `Ok(true)` when no record of the target
/// remains (a dry run: would remain), `Ok(false)` while one does.
fn uninstall_target(a: &mut Args, name: String) -> Result<bool, String> {
    let home = tool_home(a)?;
    let apply = a.flag("--apply");
    let p = paths(a)?;
    a.done()?;
    let stacks = Stacks::load(&p.public, p.private.as_deref())?;
    let opts = sync::Options {
        target: Some(name.clone()),
        home,
        uninstall: true,
        ..sync::Options::default()
    };
    let plan = sync::plan(&stacks, &opts)?;
    print_plan(&plan);
    let left = if apply {
        let done = sync::apply(&plan);
        if let Ok(applied) = &done {
            print_applied(applied);
        }
        done?;
        // What the ledger still records of the target, read back with a fresh plan.
        sync::plan(&stacks, &opts)
            .map_err(|e| format!("applied, but target {name} couldn't be read back: {e}"))?
            .items
            .len()
    } else {
        plan.items.iter().filter(|i| i.action == sync::Action::Report).count()
    };
    let head =
        if apply { "applied" } else { "dry run: nothing changed (`--apply` acts); after it" };
    match left {
        0 => println!("{head}: no record of target {name} remains"),
        n => println!("{head}: {n} record(s) of target {name} remain, each kept with its path"),
    }
    Ok(left == 0)
}

/// `lords uninstall`: removes every tool-owned path in scope that still matches its ledger record; with `--place`,
/// one checkout or worktree's (`uninstall_place`); with `--target`, one dropped target's (`uninstall_target`).
fn cmd_uninstall(mut a: Args) -> Result<bool, String> {
    let json = a.flag("--json");
    // A bad `--place` is a refusal too, which `--json` gives as its one object (sol-loop r1 S2).
    let place = match a.value("--place") {
        Err(e) if json => {
            println!("{}", PlaceRun::default().json(2, Some(&e)));
            return Err(e);
        }
        r => r?.map(PathBuf::from),
    };
    if let Some(dir) = place {
        let mut run = PlaceRun::default();
        let r = uninstall_place(&mut a, &dir, &mut run);
        let exit = match &r {
            Ok(true) => 0,
            Ok(false) => 1,
            Err(_) => 2,
        };
        if json {
            println!("{}", run.json(exit, r.as_ref().err()));
        } else {
            run.print();
        }
        return r;
    }
    if json {
        return Err("--json goes with --place: it is one place's result".into());
    }
    if let Some(name) = a.value("--target")? {
        return uninstall_target(&mut a, name);
    }
    let home = tool_home(&mut a)?;
    let apply = a.flag("--apply");
    let user_only = a.flag("--user");
    let repo = a.value("--repo")?;
    let p = paths(&mut a)?;
    a.done()?;
    let stacks = Stacks::load(&p.public, p.private.as_deref())?;
    let opts = sync::Options {
        repo: repo.clone(),
        user_only,
        home: home.clone(),
        uninstall: true,
        ..sync::Options::default()
    };
    let plan = sync::plan(&stacks, &opts)?;
    print_plan(&plan);
    if apply {
        print_applied(&sync::apply(&plan)?);
    }
    // A repo's config hook (its post-checkout where the hooks dir isn't reached): its own ledger kind.
    if !user_only {
        for line in hooks::config::uninstall(&home, repo.as_deref(), apply)? {
            println!("{line}");
        }
    }
    if !apply {
        println!("dry run: nothing removed (`--apply` removes)");
    }
    Ok(true)
}

/// `lords adopt`: `--list` shows the paths sync would write that aren't in the ledger but already hold exactly
/// that; naming paths records them as the tool's. A human's step, for paths from before the ledger.
fn cmd_adopt(mut a: Args) -> Result<bool, String> {
    let home = tool_home(&mut a)?;
    let list = a.flag("--list");
    let p = paths(&mut a)?;
    let mut named = Vec::new();
    while let Some(x) = a.positional() {
        named.push(PathBuf::from(x));
    }
    a.done()?;
    let stacks = Stacks::load(&p.public, p.private.as_deref())?;
    let plan = sync::plan(&stacks, &sync::Options { home, ..sync::Options::default() })?;
    if list || named.is_empty() {
        let adoptable: Vec<&sync::Item> = plan.items.iter().filter(|i| i.adoptable).collect();
        for i in &adoptable {
            println!("{}", i.path.display());
        }
        println!("{} adoptable (`lords adopt PATH...` records them)", adoptable.len());
        return Ok(true);
    }
    for line in sync::adopt(&plan, &named)? {
        println!("{line}");
    }
    Ok(true)
}

/// `lords set-aside PATH... --yes`: moves paths that aren't the tool's into a new backup dir (a human's step for
/// leftovers from before the ledger).
fn cmd_set_aside(mut a: Args) -> Result<bool, String> {
    let home = tool_home(&mut a)?;
    let yes = a.flag("--yes");
    let plan = whole_plan(&mut a, home)?;
    let mut named = Vec::new();
    while let Some(x) = a.positional() {
        named.push(PathBuf::from(x));
    }
    a.done()?;
    if named.is_empty() {
        return Err("set-aside needs the paths to move".into());
    }
    if !yes {
        for p in &named {
            println!("would set aside {}", p.display());
        }
        println!("nothing moved: check each path, then run again with --yes");
        return Ok(true);
    }
    print_applied(&sync::set_aside(&plan, &named)?);
    Ok(true)
}

/// One block per distinct change; the same change in many worktrees is printed once with the others listed.
fn print_plan(plan: &sync::Plan) {
    let mut shown: Vec<(String, String, String, Vec<PathBuf>)> = Vec::new();
    for i in &plan.items {
        let action = format!("{:?}", i.action);
        let file = i.path.file_name().map(|f| f.to_string_lossy().into_owned()).unwrap_or_default();
        let key_diff = i.diff.replace(&i.path.display().to_string(), &file);
        let key = format!("{}|{action}|{}", i.target, i.notes.join(";"));
        match shown.iter_mut().find(|(k, d, _, _)| *k == key && *d == key_diff) {
            Some((_, _, _, more)) => more.push(i.path.clone()),
            None => {
                println!(
                    "
== {} :: {} :: {action}",
                    i.target,
                    i.path.display()
                );
                if i.class == sync::Class::Tool && !i.scope.is_empty() {
                    println!("   the tool's ({})", i.scope);
                }
                for n in &i.notes {
                    println!("   note: {n}");
                }
                if !i.diff.is_empty() {
                    print!("{}", i.diff);
                }
                shown.push((key, key_diff, action, Vec::new()));
            }
        }
    }
    for (key, _, action, more) in &shown {
        if !more.is_empty() {
            let target = key.split('|').next().unwrap_or("");
            println!(
                "
== {target} :: the same {action} in {} more place(s):",
                more.len()
            );
            for m in more.iter().take(5) {
                println!("   {}", m.display());
            }
            if more.len() > 5 {
                println!("   … and {} more", more.len() - 5);
            }
        }
    }
    for n in &plan.notes {
        println!(
            "
note: {n}"
        );
    }
    let count =
        |f: &dyn Fn(&sync::Action) -> bool| plan.items.iter().filter(|i| f(&i.action)).count();
    println!(
        "
{} item(s): {} to change, {} in sync, {} report-only, {} conflict(s)",
        plan.items.len(),
        count(&|a| !matches!(
            a,
            sync::Action::InSync | sync::Action::Report | sync::Action::Conflict
        )),
        count(&|a| *a == sync::Action::InSync),
        count(&|a| *a == sync::Action::Report),
        count(&|a| *a == sync::Action::Conflict)
    );
}

fn cmd_import(mut a: Args) -> Result<bool, String> {
    let store = a.value("--store")?;
    let commit = !a.flag("--no-commit");
    let p = paths(&mut a)?;
    a.done()?;
    let private = Stack::load(p.private.as_deref().ok_or("import needs the private stack")?)?;
    for r in ops::import(&private, store.as_deref(), commit)? {
        println!(
            "{}: {} new, {} changed, {} unchanged{}{}",
            r.store,
            r.added.len(),
            r.changed.len(),
            r.unchanged,
            if r.absent_in_source.is_empty() {
                String::new()
            } else {
                format!("; kept, gone from the source: {}", r.absent_in_source.join(", "))
            },
            r.commit.map(|c| format!("; commit {c}")).unwrap_or_default()
        );
    }
    Ok(true)
}

fn cmd_retire(mut a: Args) -> Result<bool, String> {
    let reason = a.value("--reason")?.ok_or("retire needs --reason")?;
    let superseded_by = a.value("--superseded-by")?;
    let date = a.value("--date")?.unwrap_or_else(ops::today);
    let commit = !a.flag("--no-commit");
    let p = paths(&mut a)?;
    let id = a.positional().ok_or("retire needs an entry id")?;
    a.done()?;
    let stacks = Stacks::load(&p.public, p.private.as_deref())?;
    let r = ops::retire(
        &stacks,
        &Retire {
            id: &id,
            reason: &reason,
            superseded_by: superseded_by.as_deref(),
            date,
            commit,
            base: None,
        },
    )?;
    println!(
        "retired {id} in {}{}",
        r.file.display(),
        r.commit.map(|c| format!("; commit {c}")).unwrap_or_default()
    );
    Ok(true)
}

fn cmd_promote(mut a: Args) -> Result<bool, String> {
    let mode = match (a.flag("--strip"), a.flag("--split")) {
        (true, true) => return Err("--strip or --split, not both".into()),
        (true, false) => Promote::Strip,
        (false, true) => Promote::Split,
        _ => Promote::Clean,
    };
    let commit = !a.flag("--no-commit");
    let p = paths(&mut a)?;
    let id = a.positional().ok_or("promote needs an entry id")?;
    a.done()?;
    let stacks = Stacks::load(&p.public, p.private.as_deref())?;
    let rules = public_rules(&p.public, stacks.private.as_ref())?;
    let r = ops::promote(&stacks, &id, mode, &rules, commit)?;
    println!("promoted {id} to {}", r.to.display());
    if r.stripped > 0 {
        println!("{} detail(s) replaced by <private:…>: reword them before committing", r.stripped);
    }
    if let Some(pp) = r.private_part {
        println!("private lines kept in {}", pp.display());
    }
    for c in r.commits {
        println!("commit {c}");
    }
    Ok(true)
}

fn cmd_privacy(mut a: Args) -> Result<bool, String> {
    let staged = a.flag("--staged");
    let history = a.flag("--history");
    let secrets_only = a.flag("--secrets-only");
    let message = a.value("--message")?;
    let public_stack = a.value("--public-stack")?.map(PathBuf::from);
    let private_stack = a.value("--private-stack")?.map(PathBuf::from);
    let cwd = std::env::current_dir().map_err(|e| e.to_string())?;
    let rules = if secrets_only {
        Rules::secrets_only()
    } else {
        // A config.txt that is there but can't be read fails the check: its private stack would be skipped.
        let mut cfg = Config::load()?;
        if private_stack.is_some() {
            cfg.private = private_stack;
        }
        if public_stack.is_some() {
            cfg.public = public_stack;
        }
        // A configured private stack that doesn't load fails the check: its deny and fleet lists would be skipped.
        let private = cfg.private.as_deref().map(Stack::load).transpose()?;
        let mut rules = Rules::public().with_private(private.as_ref())?;
        if let Some(top) = git::toplevel(&cwd) {
            rules = rules.with_allow_from(&top.join("stack"));
        }
        if let Some(p) = &cfg.public {
            rules = rules.with_allow_from(p);
        }
        rules
    };
    for n in &rules.notes {
        eprintln!("lords privacy: {n}");
    }
    if history {
        a.done()?;
        let top = git::toplevel(&cwd).ok_or("not in a git repo")?;
        let h = privacy::scan_history(&top, &rules)?;
        for x in &h.hits {
            println!(
                "{} {}:{} {}: `{}`",
                x.commit,
                x.place,
                x.hit.line,
                x.hit.class.name(),
                x.hit.text
            );
        }
        println!("history: {} commits scanned, {} finding(s)", h.commits, h.hits.len());
        return Ok(h.hits.is_empty());
    }
    let mut scan = if staged {
        a.done()?;
        let top = git::toplevel(&cwd).ok_or("not in a git repo")?;
        privacy::scan_staged(&top, &rules)?
    } else {
        let mut scan = privacy::Scan::default();
        let files: Vec<String> = std::iter::from_fn(|| a.positional()).collect();
        a.done()?;
        for f in &files {
            let bytes = lords_core::fsguard::read_bytes(f).map_err(|e| format!("{f}: {e}"))?;
            scan.add(PathBuf::from(f), &bytes, &rules);
        }
        // Named files have no .gitattributes to mark them: one that isn't text is refused.
        scan.unmarked = scan.binary.clone();
        scan
    };
    // The message is scanned with the staged or named files too. A commit message has no NUL bytes, and its lossy
    // reading keeps every readable part: never refused.
    if let Some(m) = &message {
        let bytes = lords_core::fsguard::read_bytes(m).map_err(|e| format!("{m}: {e}"))?;
        scan.add(PathBuf::from(m), &bytes, &rules);
    }
    for (p, h) in &scan.hits {
        eprintln!("{}", check::privacy_finding(p, h));
    }
    for p in &scan.unmarked {
        eprintln!(
            "lords privacy: {} is not text, so only its readable bytes were scanned; if it is meant to be binary, \
             mark it `{}` in .gitattributes",
            p.display(),
            privacy::BINARY_ATTR
        );
    }
    if !scan.hits.is_empty() || !scan.unmarked.is_empty() {
        eprintln!(
            "lords privacy: {} finding(s) and {} unmarked binary file(s) in {} file(s) scanned; nothing committed",
            scan.hits.len(),
            scan.unmarked.len(),
            scan.files_scanned + scan.binary.len()
        );
        return Ok(false);
    }
    Ok(true)
}

fn cmd_hooks(mut a: Args) -> Result<bool, String> {
    if a.positional().as_deref() != Some("install") {
        return Err("usage: lords hooks install [--repo DIR] [--secrets-only] [--force]".into());
    }
    let repo = a
        .value("--repo")?
        .map(PathBuf::from)
        .unwrap_or(std::env::current_dir().map_err(|e| e.to_string())?);
    let secrets_only = a.flag("--secrets-only");
    let force = a.flag("--force");
    if a.flag("--post-checkout") {
        let explicit_public = a.value("--public-stack")?.map(PathBuf::from);
        let explicit_private = a.value("--private-stack")?.map(PathBuf::from);
        let cfg = Config::discover();
        let public = explicit_public
            .clone()
            .or(cfg.public)
            .ok_or("no public stack: pass --public-stack or set it in config.txt")?;
        let private = explicit_private
            .clone()
            .or(cfg.private)
            .ok_or("no private stack: the repos are declared there")?;
        a.done()?;
        let stacks = Stacks::load(&public, Some(&private))?;
        let top = git::toplevel(&repo).ok_or("not in a git repo")?;
        let name = sync::repo_named_by_checkout(&stacks, &top).ok_or_else(|| {
            format!("{} is no checkout of a repo in the private stack.toml", top.display())
        })?;
        let exe = std::env::current_exe().map_err(|e| e.to_string())?;
        let args = hooks::SyncArgs {
            repo: name.clone(),
            public_stack: explicit_public.map(|p| std::path::absolute(&p).unwrap_or(p)),
            private_stack: explicit_private.map(|p| std::path::absolute(&p).unwrap_or(p)),
        };
        let r = hooks::install_post_checkout(&top, &exe, &args, &sync::default_home()?)?;
        match &r.config {
            Some(why) => println!(
                "installed {} in {} (repo {name}), as a config hook: {why}",
                hooks::config::SECTION,
                r.hook.display()
            ),
            None => println!("installed {} (repo {name})", r.hook.display()),
        }
        if let Some(b) = r.backup {
            println!("the hook that was there runs first, from {}", b.display());
        }
        match r.via {
            Some(p) => println!(
                "checked: a new worktree's post-checkout reaches it through core.hooksPath = {p}"
            ),
            None => println!("checked: a new worktree's post-checkout reaches it"),
        }
        return Ok(true);
    }
    // A stack repo's post-commit delivers from the stacks given here, else config.txt's (F8, C1).
    let cfg = Config::discover();
    let public = a.value("--public-stack")?.map(PathBuf::from).or(cfg.public);
    let private = a.value("--private-stack")?.map(PathBuf::from).or(cfg.private);
    a.done()?;
    let abs = |p: PathBuf| std::path::absolute(&p).unwrap_or(p);
    let stacks = hooks::SyncArgs {
        repo: String::new(),
        public_stack: public.map(abs),
        private_stack: private.map(abs),
    };
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let home = sync::default_home()?;
    let installed = hooks::install_with(&repo, &exe, secrets_only, force, &home, &stacks)?;
    for r in &installed {
        println!("installed {}", r.hook.display());
        if let Some(b) = &r.backup {
            println!("the old hook is at {}", b.display());
        }
    }
    if let Some(g) = installed.first().and_then(|r| r.global_hooks_path.as_ref()) {
        println!(
            "note: global core.hooksPath = {g}; its dispatcher must run .git/hooks/{} for this to take effect",
            hooks::PRIVACY.join(", ")
        );
    }
    Ok(true)
}

/// What an adapter reads in one place: the user level, a repo or workspace's shared files, or one role's file.
fn cmd_compose(mut a: Args) -> Result<bool, String> {
    let json = a.flag("--json");
    let child = if a.flag("--child-whole") {
        Some(lords_core::compose::ChildStart::Whole)
    } else {
        a.flag("--child").then_some(lords_core::compose::ChildStart::Beyond)
    };
    let place = a.value("--repo")?;
    let role = a.value("--role")?;
    let adapter_id = a.value("--adapter")?.unwrap_or_else(|| "codex".into());
    let p = paths(&mut a)?;
    a.done()?;
    let stacks = Stacks::load(&p.public, p.private.as_deref())?;
    let adapter = lords_core::adapters::get(&adapter_id).ok_or_else(|| {
        format!(
            "unknown adapter `{adapter_id}` (known: {})",
            lords_core::adapters::ids().join(", ")
        )
    })?;
    let home = sync::default_home()?;
    let composed = match (&place, &role) {
        (Some(place), None) if child.is_some() => {
            let role = sync::child_role(&stacks).ok_or("--child needs a stack's `child_role`")?;
            let layers = sync::place_layers(&stacks, place, true);
            sync::child_composed(&stacks, layers, role, &home, child.expect("matched"))
        }
        (Some(place), Some(role)) => {
            sync::role_composed(&stacks, sync::place_layers(&stacks, place, true), role, &home)
        }
        (Some(place), None) if !json => {
            let dir = sync::first_path(&stacks, place)
                .ok_or_else(|| format!("no checkout or dir declared for `{place}`"))?;
            let layers = sync::place_layers(&stacks, place, false);
            let (text, _) = sync::place_text(&stacks, layers, adapter, &dir, &home);
            print!("{text}");
            eprintln!("{} bytes", text.len());
            return Ok(true);
        }
        (Some(place), None) => sync::repo_composed(&stacks, place, adapter),
        (None, None) => sync::user_composed(&stacks, adapter),
        (None, Some(_)) => return Err("--role needs --repo (role files live in a place)".into()),
    };
    if json {
        let view = lords_core::view::composed(&stacks, &composed);
        println!("{}", serde_json::to_string_pretty(&view).unwrap_or_default());
    } else {
        print!("{}", adapter.render(&composed));
        eprintln!("{} bytes", adapter.render(&composed).len());
    }
    Ok(true)
}

/// Every file sync writes for agents, measured as written: bytes by section, and a place's file against its
/// adapter's limit.
fn cmd_sizes(mut a: Args) -> Result<bool, String> {
    let json = a.flag("--json");
    let p = paths(&mut a)?;
    a.done()?;
    let stacks = Stacks::load(&p.public, p.private.as_deref())?;
    let sizes = sync::sizes(&stacks);
    if json {
        println!("{}", serde_json::to_string_pretty(&sizes).unwrap_or_default());
        return Ok(true);
    }
    let mut over = false;
    for s in &sizes {
        let reads = match s.limit {
            Some(l) => {
                let mark = if s.over_line() { "  OVER the warning line" } else { "" };
                over |= s.over_line();
                format!(", {} of its limit {l} ({}%){mark}", s.reads, s.reads * 100 / l)
            }
            _ => String::new(),
        };
        let later = if s.enabled { "" } else { " (not written yet: the place's adapters)" };
        println!("{} {}: {} bytes{reads}{later}", s.place, s.file, s.bytes);
        let parts: Vec<String> = s.sections.iter().map(|(n, b)| format!("{n} {b}")).collect();
        println!("  {}", parts.join(", "));
    }
    Ok(!over)
}

/// `lords paths [--all] [--json] [--codex PATH] [--deadline-ms N] [--home DIR]`: which T3-delegated paths start with
/// every core line of their role (paths.rs). The rows of places not in use are one count line unless `--all`; `--json`
/// has every row, with `in_use` (D15). Exit 0 when every row is complete, shown or not, 1 when one isn't.
fn cmd_paths(mut a: Args) -> Result<bool, String> {
    let json = a.flag("--json");
    let all = a.flag("--all");
    let codex = a.value("--codex")?.map(PathBuf::from);
    let deadline = match a.value("--deadline-ms")? {
        None => lords_core::paths::CODEX_DEADLINE,
        Some(ms) => std::time::Duration::from_millis(
            ms.parse().map_err(|_| "--deadline-ms takes a number of milliseconds")?,
        ),
    };
    let home = match a.value("--home")? {
        Some(h) => PathBuf::from(h),
        None => sync::default_home()?,
    };
    let p = paths(&mut a)?;
    a.done()?;
    let stacks = Stacks::load(&p.public, p.private.as_deref())?;
    let ask = lords_core::paths::Ask { home, codex, deadline };
    let rows = lords_core::paths::paths(&stacks, &ask);
    if json {
        println!("{}", serde_json::to_string_pretty(&rows).unwrap_or_default());
    } else {
        let (shown, folded): (Vec<_>, Vec<_>) = rows.iter().partition(|r| all || r.in_use);
        for r in &shown {
            println!("{:<10}  {}: {}", r.state, r.path, r.why);
        }
        if !folded.is_empty() {
            let incomplete = folded.iter().filter(|r| r.state != "complete").count();
            println!(
                "{} rows of places not in use (no live session, no HEAD, index or branch change in {} days), {incomplete} incomplete: --all lists them",
                folded.len(),
                lords_core::paths::IN_USE.as_secs() / 86_400
            );
        }
    }
    Ok(rows.iter().all(|r| r.state == "complete"))
}

/// `lords show <id>`: the whole text on stdout; a refusal (no such entry, two of them, retired or ended) on stderr,
/// exit 1.
fn cmd_show(mut a: Args) -> Result<bool, String> {
    let id = a.positional().ok_or("show needs an id")?;
    let p = paths(&mut a)?;
    a.done()?;
    let stacks = Stacks::load(&p.public, p.private.as_deref())?;
    match lords_core::core_lines::show(&stacks, &id) {
        Ok(body) => {
            print!("{body}");
            Ok(true)
        }
        Err(why) => {
            say_err(&format!("lords show: {why}"));
            Ok(false)
        }
    }
}

fn cmd_hash(mut a: Args) -> Result<bool, String> {
    let p = a.positional().ok_or("hash needs a path")?;
    a.done()?;
    println!("{}", check::path_hash(Path::new(&p))?);
    Ok(true)
}

fn main() -> ExitCode {
    let mut words: Vec<String> = std::env::args().skip(1).collect();
    // The guard answers a harness, so it has its own exit codes and help, before anything else (cmd_guard; rm-guard
    // fix check r2, R8).
    if words.first().is_some_and(|w| w == "guard") {
        words.remove(0);
        return cmd_guard(Args { words });
    }
    if words.is_empty() || words.iter().any(|w| w == "--help" || w == "-h") {
        println!("{USAGE}");
        return ExitCode::SUCCESS;
    }
    let cmd = words.remove(0);
    let a = Args { words };
    let result = match cmd.as_str() {
        "check" => cmd_check(a),
        "import" => cmd_import(a),
        "retire" => cmd_retire(a),
        "promote" => cmd_promote(a),
        "privacy" => cmd_privacy(a),
        "hooks" => cmd_hooks(a),
        "hash" => cmd_hash(a),
        "sync" => cmd_sync(a),
        "deliver" => cmd_deliver(a),
        "compose" => cmd_compose(a),
        "show" => cmd_show(a),
        "uninstall" => cmd_uninstall(a),
        "adopt" => cmd_adopt(a),
        "set-aside" => cmd_set_aside(a),
        "sizes" => cmd_sizes(a),
        "paths" => cmd_paths(a),
        "panel" => panel::cmd(a),
        other => Err(format!("unknown command `{other}`\n\n{USAGE}")),
    };
    match result {
        Ok(true) => ExitCode::SUCCESS,
        Ok(false) => ExitCode::from(1),
        Err(e) => {
            eprintln!("lords: {e}");
            ExitCode::from(2)
        }
    }
}
