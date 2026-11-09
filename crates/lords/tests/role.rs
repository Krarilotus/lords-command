//! `lords guard role` and `lords paths` (F10, design note Harness/notes/2026-11-02-lords-role-at-launch.md): a session
//! T3 starts with `delegate_task` gets its own role's start at the first prompt that names the role, or is told why it
//! can't; `paths` says which T3-delegated paths deliver it. Every run gets an empty home: no real config, stack, setting
//! or Codex is read or touched (Codex is a stub, tests/support/codex_stub.rs).

// Tests are outside the filesystem boundary (fsguard.rs): their fixtures write their own temp dirs.
#![allow(clippy::disallowed_methods, clippy::disallowed_types)]

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use tempfile::TempDir;

const BEGIN: &str = "<!-- lords-command:begin -->";
const END: &str = "<!-- lords-command:end -->";

fn run(home: &Path, cwd: &Path, args: &[&str], stdin: &str, env: &[(&str, &str)]) -> Output {
    let mut c = Command::new(env!("CARGO_BIN_EXE_lords"));
    c.current_dir(cwd)
        .env("HOME", home)
        .env("USERPROFILE", home)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env_remove("T3CODE_THREAD_ID")
        // The Codex sync asks after it writes Codex's hooks: the stub, never one on PATH (tests/codex_trust.rs).
        .env("LORDS_CODEX", env!("CARGO_BIN_EXE_lords-codex-stub"))
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for (k, v) in env {
        c.env(k, v);
    }
    let mut c = c.spawn().unwrap();
    // A guard that answers without reading its input may have ended first: a broken pipe off Windows (the race).
    match c.stdin.take().unwrap().write_all(stdin.as_bytes()) {
        Err(e) if e.kind() == std::io::ErrorKind::BrokenPipe => {}
        r => r.unwrap(),
    }
    c.wait_with_output().unwrap()
}

/// A place with a start for the reviewer and the coder, a cwd below it, and a tool home.
struct Place {
    dir: TempDir,
}

impl Place {
    fn new() -> Place {
        let p = Place { dir: TempDir::new().unwrap() };
        fs::create_dir_all(p.cwd()).unwrap();
        fs::create_dir_all(p.home()).unwrap();
        p.start("reviewer", "REVIEWER START.");
        p.start("coder", "CODER START.");
        // Claude's place file, which a start goes beside (F10-R1-03; the session guard is Claude Code's).
        fs::write(p.dir.path().join("place/CLAUDE.local.md"), format!("{BEGIN}\nplace\n{END}\n"))
            .unwrap();
        p
    }
    fn home(&self) -> PathBuf {
        self.dir.path().join("home")
    }
    fn tool_home(&self) -> PathBuf {
        self.home().join(".lords-command")
    }
    fn cwd(&self) -> PathBuf {
        self.dir.path().join("place/a")
    }
    fn start(&self, role: &str, text: &str) {
        let dir = self.dir.path().join("place/.lords");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join(format!("start-{role}.md")), format!("{BEGIN}\n{text}\n{END}\n"))
            .unwrap();
    }
    /// The role guard's answer to a prompt in `session`, in `cwd`, with these env vars.
    fn prompt(&self, session: &str, cwd: &Path, prompt: &str, env: &[(&str, &str)]) -> Output {
        let ev = serde_json::json!({"hook_event_name": "UserPromptSubmit", "session_id": session,
            "cwd": cwd, "prompt": prompt, "transcript_path": "x"});
        let home = self.tool_home();
        run(
            &self.home(),
            cwd,
            &["guard", "role", "--home", home.to_str().unwrap()],
            &ev.to_string(),
            env,
        )
    }
    fn ask(&self, session: &str, prompt: &str) -> Option<String> {
        context(&self.prompt(session, &self.cwd(), prompt, &[]))
    }
}

/// The context one hook call answers, or `None` for no output.
fn context(o: &Output) -> Option<String> {
    assert_eq!(o.status.code(), Some(0), "a hook never fails: {o:?}");
    let v: serde_json::Value = serde_json::from_slice(&o.stdout).ok()?;
    Some(v["hookSpecificOutput"]["additionalContext"].as_str()?.to_string())
}

// Spec (F10; the Lords owner after fix check r1: "The role guard goes stateless … On each prompt, it gives a role's
// start only when that prompt itself carries the launcher's signal: T3's prefix at the very start, or a `Role:` line
// as the prompt's own first line, never in quoted text. A prompt without one gets nothing … A later prompt that
// carries a signal is the launcher's again"; review r1, F10-R1-01: the prefix wins over a `Role:` line; Claude Code's
// and Codex's `UserPromptSubmit` take `hookSpecificOutput.additionalContext`): each prompt carrying a signal gets that
// role's start from the nearest `.lords`, without its marker lines, every time; a prompt carrying none, or only quoted
// text, gets nothing; nothing is recorded anywhere.
#[test]
fn the_role_guard_gives_the_start_each_prompt_carrying_the_launchers_signal() {
    let p = Place::new();
    assert_eq!(p.ask("s1", "Role: reviewer\nReview the diff.").as_deref(), Some("REVIEWER START."));
    assert_eq!(p.ask("s1", "Role: reviewer\nAgain.").as_deref(), Some("REVIEWER START."), "again");
    assert_eq!(p.ask("s1", "Fix the finding."), None, "no signal");
    assert_eq!(
        p.ask("s1", "Role: coder\nBuild it.").as_deref(),
        Some("CODER START."),
        "the launcher's"
    );
    let t3 = "Act as the review sub-agent for this task.\n\nLook at it.";
    assert_eq!(p.ask("s3", t3).as_deref(), Some("REVIEWER START."));
    let both = p.ask("s4", "Act as the review sub-agent for this task.\n\nRole: coder\nBuild.");
    assert_eq!(both.as_deref(), Some("REVIEWER START."), "the prefix wins");
    let quoted = "Act as the review sub-agent for this task.\n\n> Role: coder\n> Implement the change.\n\nReview whether this brief is safe to send. Do not implement it.";
    assert_eq!(
        p.ask("s7", quoted).as_deref(),
        Some("REVIEWER START."),
        "quoted text names nothing"
    );
    assert_eq!(p.ask("s7", "> Role: coder\n> More."), None, "nor does a later quote");
    assert_eq!(p.ask("s8", "Look at this:\nRole: coder"), None, "only the prompt's own first line");
    // Review of the simplified delta, F10-SR-01, its witnesses: after a reviewer's start, a follow-up quoting a launch
    // instruction indented, through the registered hook of either harness, gives no role context; the signal starts at
    // column 0 of the first line or not at all.
    let tool_home = p.tool_home();
    for adapter in ["claude", "codex"] {
        for quoted in [
            "    Act as the implementation sub-agent for this task.\n\nReview the quoted launch instruction above. Do not implement it.",
            "    Role: coder\n\nReview the quoted brief above. Do not implement it.",
        ] {
            let ev = serde_json::json!({"hook_event_name": "UserPromptSubmit", "session_id": "s9",
                "cwd": p.cwd(), "prompt": quoted, "transcript_path": "x"});
            let args = ["guard", "role", "--home", tool_home.to_str().unwrap(), "--adapter", adapter];
            let o = run(&p.home(), &p.cwd(), &args, &ev.to_string(), &[]);
            assert_eq!(context(&o), None, "{adapter}: {quoted:?}");
        }
    }
    // Fix check r1, F10-FC1-01: no per-session record to lose, so none to keep or sweep.
    assert!(!p.tool_home().join("guard/role").exists(), "nothing recorded");
    for input in [
        serde_json::json!({"hook_event_name": "SessionStart", "session_id": "s5", "cwd": p.cwd(),
            "prompt": "Role: reviewer"})
        .to_string(),
        "not json".into(),
    ] {
        let o = run(&p.home(), &p.cwd(), &["guard", "role"], &input, &[]);
        assert_eq!(context(&o), None, "{input}");
    }
}

// Spec (design note C2: a prompt whose role's lines can't be given is told so, with the role file to read, never left
// to look complete; review r1, F10-R1-04: whatever fails; the Lords owner after fix check r1: "A prompt without [a
// signal] gets nothing"): a role with no start in the place, a start past one hook's 9,500 characters, and a start that
// can't be read (here a dir where the file goes) each get one line naming the role file, at every prompt naming the
// role; a prompt naming none gets nothing, T3-delegated or not, and so does T3's prefix naming no Lords role whatever a
// `Role:` line says; a cwd in no place gets nothing.
#[test]
fn a_prompt_whose_role_cant_be_given_is_told() {
    let p = Place::new();
    let lead = p.ask("s1", "Role: lead\nLead.").unwrap();
    assert_eq!(lead.lines().count(), 1, "{lead}");
    assert!(lead.contains("lead") && lead.contains("role-lead.md"), "{lead}");
    assert_eq!(p.ask("s1", "Role: lead\nLead.").as_deref(), Some(lead.as_str()), "every time");
    let long: Vec<String> =
        (0..300).map(|i| format!("- rule.r{i:03}: Line {i} of the long start.")).collect();
    p.start("reviewer", &long.join("\n"));
    let told = p.ask("s2", "Role: reviewer\n").unwrap();
    assert_eq!(told.lines().count(), 1, "{told}");
    assert!(told.contains("role-reviewer.md") && told.contains("9500"), "{told}");
    assert!(!told.contains("Line 1 of"), "{told}");
    let start_file = p.dir.path().join("place/.lords/start-coder.md");
    fs::remove_file(&start_file).unwrap();
    fs::create_dir(&start_file).unwrap();
    let unread = p.ask("s3", "Role: coder\n").unwrap_or_default();
    assert!(unread.contains("role-coder.md") && unread.lines().count() == 1, "{unread}");
    let delegated = [("T3CODE_THREAD_ID", "thread:delegated-task:command%3Amcp%3Ax")];
    assert_eq!(context(&p.prompt("s4", &p.cwd(), "Fix the bug.", &delegated)), None);
    let design = "Act as the design sub-agent for this task.\n\nRole: coder\nSketch it.";
    assert_eq!(context(&p.prompt("s5", &p.cwd(), design, &delegated)), None);
    let elsewhere = p.dir.path().join("elsewhere");
    fs::create_dir_all(&elsewhere).unwrap();
    assert_eq!(context(&p.prompt("s6", &elsewhere, "Role: reviewer\n", &[])), None);
}

// Spec (the Lords owner after fix check r1: "After a compaction, the session hook gives one fixed line: re-read your
// role's file in that place's `.lords`. It doesn't guess a role"; Claude Code's hooks docs: `SessionStart` runs with
// source `compact` after a compaction and takes `additionalContext`): after a compaction in a place, one line naming
// that place's `.lords` and no role, whatever role a prompt named before; at any other start, or outside every place,
// nothing.
#[test]
fn the_session_guard_says_to_reread_the_role_file_after_a_compaction() {
    let p = Place::new();
    assert_eq!(p.ask("s1", "Role: reviewer\n").as_deref(), Some("REVIEWER START."));
    let start = |source: &str, cwd: &Path| {
        let ev = serde_json::json!({"hook_event_name": "SessionStart", "session_id": "s1",
            "cwd": cwd, "source": source, "transcript_path": "x"});
        context(&run(&p.home(), cwd, &["guard", "session"], &ev.to_string(), &[]))
    };
    let told = start("compact", &p.cwd()).unwrap_or_default();
    let lords = p.dir.path().join("place").join(".lords").display().to_string();
    assert!(told.lines().count() == 1 && told.contains(&lords), "{told}");
    assert!(
        !told.contains("reviewer") && !told.contains("REVIEWER START."),
        "no role guessed: {told}"
    );
    // Review of the simplified delta, F10-SR-02 (the Lords owner: "a readable place with a readable role file always
    // gets the fixed line, whatever `child.md`'s state"): with `child.md` unreadable (here a dir where the file goes),
    // the same line.
    let child = p.dir.path().join("place/.lords/child.md");
    fs::create_dir_all(&child).unwrap();
    assert_eq!(start("compact", &p.cwd()).as_deref(), Some(told.as_str()), "child.md unread");
    assert_eq!(start("startup", &p.cwd()), None);
    assert_eq!(start("resume", &p.cwd()), None, "a resumed session keeps its history");
    let elsewhere = p.dir.path().join("elsewhere");
    fs::create_dir_all(&elsewhere).unwrap();
    assert_eq!(start("compact", &elsewhere), None);
}

// Spec (review r1, F10-R1-03, the Lords owner: "complete means the whole start is present: the place file for that
// adapter as well as the role's supplement. If either is missing or unreadable, the path is incomplete"; design note
// C2): the hook knows its harness (`--adapter`, as sync writes it); where that harness's place file (`CLAUDE.local.md`,
// `AGENTS.override.md`) isn't in the place, the session gets one line naming its role file instead of the supplement.
#[test]
fn a_start_is_given_only_beside_its_harnesss_place_file() {
    let p = Place::new();
    fs::write(p.dir.path().join("place/CLAUDE.local.md"), format!("{BEGIN}\nplace\n{END}\n"))
        .unwrap();
    let ask = |session: &str, adapter: &str| {
        let ev = serde_json::json!({"hook_event_name": "UserPromptSubmit", "session_id": session,
            "cwd": p.cwd(), "prompt": "Role: reviewer\nReview it.", "transcript_path": "x"});
        let home = p.tool_home();
        let args = ["guard", "role", "--home", home.to_str().unwrap(), "--adapter", adapter];
        context(&run(&p.home(), &p.cwd(), &args, &ev.to_string(), &[]))
    };
    assert_eq!(ask("s1", "claude").as_deref(), Some("REVIEWER START."));
    let told = ask("s2", "codex").unwrap_or_default();
    assert!(told.contains("AGENTS.override.md") && told.contains("role-reviewer.md"), "{told}");
    assert!(!told.contains("REVIEWER START.") && told.lines().count() == 1, "{told}");
}

// Spec (sol-loop r5, S1: "`guard role --adapter sentinelvalueq`, with a marked role start present, embeds 'no harness
// `sentinelvalueq` is known' in stdout JSON ... render typed, content-free diagnostics in both guard JSON and notes";
// design note privacy-own-allow, attack item 53): with a marked start and a harness no adapter is, the prompt is told
// in one line naming its role file, and neither stream repeats the harness given.
#[test]
fn an_unknown_harness_is_named_not_repeated() {
    let sentinel = "sentinelvalueq";
    let p = Place::new();
    let ev = serde_json::json!({"hook_event_name": "UserPromptSubmit", "session_id": "s1",
        "cwd": p.cwd(), "prompt": "Role: reviewer\nReview it.", "transcript_path": "x"});
    let home = p.tool_home();
    let args = ["guard", "role", "--home", home.to_str().unwrap(), "--adapter", sentinel];
    let o = run(&p.home(), &p.cwd(), &args, &ev.to_string(), &[]);
    let all =
        format!("{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr));
    assert!(!all.contains(sentinel), "not repeated: {all}");
    let told = context(&o).unwrap_or_default();
    assert!(told.contains("role-reviewer.md") && told.lines().count() == 1, "{told}");
}

// Spec (design note D6, as for the child guard, fix check r2, FC4): asked with the hook's arguments (its tool home and
// its harness, F10-R1-03) and the probe event, the guard answers its role protocol, 1.
#[test]
fn the_guard_answers_the_probe_with_its_role_protocol() {
    let p = Place::new();
    let home = p.tool_home();
    for adapter in ["claude", "codex"] {
        let o = run(
            &p.home(),
            &p.cwd(),
            &["guard", "role", "--home", home.to_str().unwrap(), "--adapter", adapter],
            r#"{"hook_event_name":"LordsGuardProbe"}"#,
            &[],
        );
        assert_eq!(String::from_utf8_lossy(&o.stdout).trim(), "lords guard role 1", "{o:?}");
    }
}

/// Two stacks in `d`: a public one, and a private one, a git repo, with a repo `beta-core` at `checkout` and settings
/// targets for Claude and Codex whose guards are `guards` (TOML lists); `extra` is more of it.
fn stacks(d: &Path, claude_guards: &str, codex_guards: &str) -> [String; 4] {
    let (public, private) = (d.join("pub"), d.join("priv"));
    for dir in [
        &public,
        &private,
        &d.join("home/.claude-balanced"),
        &d.join("home/.codex"),
        &d.join("checkout"),
    ] {
        fs::create_dir_all(dir).unwrap();
    }
    fs::write(public.join("stack.toml"), "name = \"pub\"\nvisibility = \"public\"\n").unwrap();
    let checkout = d.join("checkout").display().to_string().replace('\\', "/");
    fs::write(
        private.join("stack.toml"),
        format!("name = \"priv\"\nvisibility = \"private\"\nroles = [\"orchestrator\", \"coder\", \"reviewer\", \"product\"]\n[repos.beta-core]\ncheckouts = ['{checkout}']\nworktrees = false\ndefault_role = \"coder\"\n[[targets]]\nname = \"balanced settings\"\nadapter = \"claude\"\nkind = \"settings\"\npath = \"~/.claude-balanced/settings.json\"\nguards = {claude_guards}\n[[targets]]\nname = \"codex hooks\"\nadapter = \"codex\"\nkind = \"settings\"\npath = \"~/.codex/hooks.json\"\nguards = {codex_guards}\n"),
    )
    .unwrap();
    fs::write(d.join("checkout/README.md"), "a repo\n").unwrap();
    for repo in [&private, &d.join("checkout")] {
        git(d, repo, &["init", "-q"]);
        git(d, repo, &["add", "-A"]);
        git(d, repo, &["commit", "-q", "-m", "start"]);
    }
    [
        "--public-stack".into(),
        public.display().to_string(),
        "--private-stack".into(),
        private.display().to_string(),
    ]
}

fn git(d: &Path, dir: &Path, args: &[&str]) {
    let gitconfig = d.join("gitconfig");
    // Built at run time, so the repo's own privacy check doesn't see an address in this source.
    let email = ["test", "example.invalid"].join("@");
    fs::write(&gitconfig, format!("[user]\n\tname = Test\n\temail = {email}\n")).unwrap();
    let o = Command::new("git")
        .current_dir(dir)
        .env("GIT_CONFIG_GLOBAL", &gitconfig)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .args(args)
        .output()
        .unwrap();
    assert!(o.status.success(), "git {args:?}: {o:?}");
}

fn lords(d: &Path, args: &[&str], env: &[(&str, &str)]) -> Output {
    run(&d.join("home"), d, args, "", env)
}

fn said(o: &Output) -> String {
    format!("{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr))
}

/// A repo-wide rule for every role in `beta-core`, committed: the place's files then have Lords content to carry, the
/// lines the starts leave out.
fn wide_rule(d: &Path) {
    let rule = "---\nid: rule.wide\nkind: rule\ntitle: rule.wide\naudience: [all]\nscope: repo:beta-core\nstatus: active\nvisibility: private\norigin:\n  type: own\nstart: core\ncore: REPO-WIDE line.\n---\nWide.\n";
    fs::create_dir_all(d.join("priv/repo/beta-core/rules")).unwrap();
    fs::write(d.join("priv/repo/beta-core/rules/wide.md"), rule).unwrap();
    git(d, &d.join("priv"), &["add", "-A"]);
    git(d, &d.join("priv"), &["commit", "-q", "-m", "a repo-wide rule"]);
}

// Spec (design note D3, D6; the child guard's registration, tests/child.rs): with `role` in a settings target's guards
// and a copy that speaks role protocol 1, sync writes one `UserPromptSubmit` entry: Claude Code's exec form with the
// tool home and a 30 s deadline; Codex's command line with the whole context passed (`additionalContextLimit` 0); the
// plan names it, and Codex's /hooks review. A copy that doesn't speak the protocol is refused with the step that fixes
// it, and the settings keep what they have.
#[test]
fn sync_registers_the_role_guard_on_user_prompt_submit() {
    let d = TempDir::new().unwrap();
    let d = d.path();
    let stacks = stacks(d, "[\"role\"]", "[\"role\"]");
    let stacks: Vec<&str> = stacks.iter().map(String::as_str).collect();
    assert!(lords(d, &["guard", "install"], &[]).status.success());
    // An earlier build's child hook, in the quoted-path form: still Lords' own, so sync takes it out (not wanted here).
    let copy =
        d.join("home/.lords-command/bin/lords-guard.exe").display().to_string().replace('\\', "/");
    let tool_home = d.join("home/.lords-command").display().to_string().replace('\\', "/");
    let old = format!("\"{copy}\" guard child --home \"{tool_home}\"");
    let old = serde_json::json!({"hooks": {"SubagentStart": [{"hooks": [{"type": "command", "command": old}]}]}});
    fs::write(d.join("home/.codex/hooks.json"), old.to_string()).unwrap();
    let o = lords(d, &[&["sync", "--user"][..], &stacks].concat(), &[]);
    assert_eq!(
        said(&o).matches("Lords hook added: the role guard (UserPromptSubmit)").count(),
        2,
        "{}",
        said(&o)
    );
    assert!(said(&o).contains("/hooks"), "{}", said(&o));
    let o = lords(d, &[&["sync", "--user", "--apply"][..], &stacks].concat(), &[]);
    assert!(o.status.success(), "{}", said(&o));
    let read = |rel: &str| -> serde_json::Value {
        serde_json::from_str(&fs::read_to_string(d.join(rel)).unwrap()).unwrap()
    };
    let hook = serde_json::json!({"type": "command", "command": copy, "args": ["guard", "role", "--home", tool_home, "--adapter", "claude"], "timeout": 30});
    let v = read("home/.claude-balanced/settings.json");
    assert_eq!(v["hooks"]["UserPromptSubmit"], serde_json::json!([{"hooks": [hook]}]), "{v}");
    // On Windows Codex runs a hook's command with PowerShell (design note H9): PowerShell's call, literal paths.
    let line = if cfg!(windows) {
        format!("& '{copy}' guard role --home '{tool_home}' --adapter codex")
    } else {
        format!("\"{copy}\" guard role --home \"{tool_home}\" --adapter codex")
    };
    let lords_hook = serde_json::json!({"type": "command", "command": line, "timeout": 30, "additionalContextLimit": 0});
    let v = read("home/.codex/hooks.json");
    assert_eq!(v["hooks"]["UserPromptSubmit"], serde_json::json!([{"hooks": [lords_hook]}]), "{v}");
    assert!(v["hooks"]["SubagentStart"].as_array().is_none_or(Vec::is_empty), "{v}");
    // Run as Codex runs it there, the line reaches the guard with its event (H8: the quoted-path form never did).
    if cfg!(windows) {
        let mut ps = Command::new("powershell")
            .args(["-NoProfile", "-NonInteractive", "-Command", &line])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        ps.stdin.take().unwrap().write_all(br#"{"hook_event_name":"LordsGuardProbe"}"#).unwrap();
        let o = ps.wait_with_output().unwrap();
        assert_eq!(String::from_utf8_lossy(&o.stdout).trim(), "lords guard role 1", "{o:?}");
    }
    let before = (read("home/.claude-balanced/settings.json"), read("home/.codex/hooks.json"));
    fs::copy(
        env!("CARGO_BIN_EXE_lords-old-guard-stub"),
        d.join("home/.lords-command/bin/lords-guard.exe"),
    )
    .unwrap();
    let o = lords(d, &[&["sync", "--user", "--apply"][..], &stacks].concat(), &[]);
    assert!(!o.status.success() && said(&o).contains("lords guard install"), "{}", said(&o));
    assert_eq!(
        (read("home/.claude-balanced/settings.json"), read("home/.codex/hooks.json")),
        before
    );
}

/// `lords paths --json` as rows `(state, path, why)`, with Codex answered by the stub, trusting as `trust` says.
fn paths(
    d: &Path,
    stacks: &[&str],
    codex: &str,
    trust: &str,
) -> (Option<i32>, Vec<(String, String, String)>) {
    let args = ["paths", "--json", "--codex", codex, "--deadline-ms", "3000"];
    let o = lords(d, &[&args[..], stacks].concat(), &[("LORDS_STUB_TRUST", trust)]);
    let v: serde_json::Value =
        serde_json::from_slice(&o.stdout).unwrap_or_else(|_| panic!("{}", said(&o)));
    let rows = v
        .as_array()
        .unwrap()
        .iter()
        .map(|r| {
            let s = |k: &str| r[k].as_str().unwrap_or_default().to_string();
            (s("state"), s("path"), s("why"))
        })
        .collect();
    (o.status.code(), rows)
}

fn row<'a>(rows: &'a [(String, String, String)], path: &str) -> &'a (String, String, String) {
    rows.iter()
        .find(|r| r.1.contains(path))
        .unwrap_or_else(|| panic!("no row for {path}: {rows:?}"))
}

// Spec (design note C3, D7; L1: "a path that can't is marked incomplete"; rule.remote-calls-have-deadlines: a missing
// reading fails closed): `lords paths` rows each T3-delegated path complete or incomplete with its reason, exit 0 only
// when all are complete: Claude's when its settings register the role guard; Codex's when they do and Codex's
// `hooks/list` reports Lords' exact role command `trusted` (not when `untrusted`, nor when Codex can't be asked); and per
// role, from the starts installed in each checkout (sol-loop r1 S1): one not there yet, or past one hook, is incomplete
// for that role there.
#[test]
fn paths_marks_each_t3_delegated_path_complete_or_incomplete() {
    let d = TempDir::new().unwrap();
    let d = d.path();
    let stacks = stacks(d, "[\"role\"]", "[\"role\"]");
    let stacks: Vec<&str> = stacks.iter().map(String::as_str).collect();
    wide_rule(d);
    let stub = env!("CARGO_BIN_EXE_lords-codex-stub");
    assert!(lords(d, &["guard", "install"], &[]).status.success());
    let (code, rows) = paths(d, &stacks, stub, "trusted");
    assert_eq!(code, Some(1), "{rows:?}");
    assert_eq!(row(&rows, "claude").0, "incomplete", "not registered yet: {rows:?}");
    assert!(row(&rows, "claude").2.contains("sync"), "{rows:?}");
    assert_eq!(row(&rows, "codex").0, "incomplete", "{rows:?}");
    let coder = row(&rows, "coder");
    assert!(coder.0 == "incomplete" && coder.2.contains("start-coder.md"), "not synced: {rows:?}");
    let sync = || lords(d, &[&["sync", "--apply"][..], &stacks].concat(), &[]);
    assert!(sync().status.success());
    let (code, rows) = paths(d, &stacks, stub, "trusted");
    assert_eq!(code, Some(0), "{rows:?}");
    assert!(rows.iter().all(|r| r.0 == "complete"), "{rows:?}");
    let (code, rows) = paths(d, &stacks, stub, "untrusted");
    assert_eq!(code, Some(1), "{rows:?}");
    assert_eq!(row(&rows, "codex").0, "incomplete");
    assert!(
        row(&rows, "codex").2.contains("/hooks") && row(&rows, "codex").2.contains("untrusted"),
        "{rows:?}"
    );
    assert_eq!(row(&rows, "claude").0, "complete", "{rows:?}");
    let missing = d.join("no-codex.exe");
    let (code, rows) = paths(d, &stacks, missing.to_str().unwrap(), "trusted");
    assert_eq!(
        (code, row(&rows, "codex").0.as_str()),
        (Some(1), "incomplete"),
        "fails closed: {rows:?}"
    );
    let (_, rows) = paths(d, &stacks, stub, "hang");
    assert_eq!(row(&rows, "codex").0, "incomplete", "past its deadline: {rows:?}");
    let lines: Vec<String> =
        (0..30).map(|i| format!("- line: \"{i:02} {}\"", "x".repeat(390))).collect();
    let rule = format!(
        "---\nid: rule.long\nkind: rule\ntitle: rule.long\naudience: [all]\nscope: repo:beta-core\nstatus: active\nvisibility: private\norigin:\n  type: own\nroles: [reviewer]\nstart: core\ncore:\n{}\n---\nLong.\n",
        lines.join("\n")
    );
    fs::create_dir_all(d.join("priv/repo/beta-core/rules")).unwrap();
    fs::write(d.join("priv/repo/beta-core/rules/long.md"), rule).unwrap();
    git(d, &d.join("priv"), &["add", "-A"]);
    git(d, &d.join("priv"), &["commit", "-q", "-m", "a long reviewer rule"]);
    assert!(sync().status.success());
    let (code, rows) = paths(d, &stacks, stub, "trusted");
    assert_eq!(code, Some(1), "{rows:?}");
    let reviewer = row(&rows, "reviewer");
    assert_eq!(reviewer.0, "incomplete", "{rows:?}");
    assert!(reviewer.1.contains("beta-core") && reviewer.2.contains("9500"), "{rows:?}");
    assert_eq!(row(&rows, "coder").0, "complete", "{rows:?}");
    fs::remove_file(d.join("checkout/.lords/start-coder.md")).unwrap();
    let (_, rows) = paths(d, &stacks, stub, "trusted");
    let coder = row(&rows, "coder");
    assert!(coder.0 == "incomplete" && coder.2.contains("start-coder.md"), "gone: {rows:?}");
    // Sol-loop r2 S1: a worktree sync writes for, as git lists it, is judged too; one without its starts is incomplete.
    let manifest = fs::read_to_string(d.join("priv/stack.toml")).unwrap();
    fs::write(d.join("priv/stack.toml"), manifest.replace("worktrees = false", "worktrees = true"))
        .unwrap();
    git(d, &d.join("priv"), &["commit", "-q", "-a", "-m", "worktrees"]);
    git(d, &d.join("checkout"), &["worktree", "add", "-q", "../wt"]);
    let (code, rows) = paths(d, &stacks, stub, "trusted");
    let wt = rows.iter().find(|r| r.1.contains("as reviewer") && r.1.contains("/wt"));
    assert!(code == Some(1) && wt.is_some_and(|r| r.0 == "incomplete"), "{rows:?}");
}

// Spec (review r1, F10-R1-02, the Lords owner: "`lords paths` reports a path complete only when the hook is registered
// under the event that actually fires for that path"): a Lords role hook moved to `SessionStart`, in Claude's settings
// and in Codex's hooks.json, leaves both paths incomplete, exit 1; so do Claude's hooks switched off
// (`disableAllHooks`), with the hook where it belongs.
#[test]
fn paths_needs_the_role_hook_on_the_prompt_event() {
    let d = TempDir::new().unwrap();
    let d = d.path();
    let stacks = stacks(d, "[\"role\"]", "[\"role\"]");
    let stacks: Vec<&str> = stacks.iter().map(String::as_str).collect();
    wide_rule(d);
    let stub = env!("CARGO_BIN_EXE_lords-codex-stub");
    assert!(lords(d, &["guard", "install"], &[]).status.success());
    assert!(lords(d, &[&["sync", "--apply"][..], &stacks].concat(), &[]).status.success());
    assert_eq!(paths(d, &stacks, stub, "trusted").0, Some(0), "complete as synced");
    let (claude, codex) =
        (d.join("home/.claude-balanced/settings.json"), d.join("home/.codex/hooks.json"));
    let read = |p: &Path| -> serde_json::Value {
        serde_json::from_str(&fs::read_to_string(p).unwrap()).unwrap()
    };
    let synced = read(&claude);
    for f in [&claude, &codex] {
        let mut v = read(f);
        let entry = v["hooks"]["UserPromptSubmit"].take();
        v["hooks"]["UserPromptSubmit"] = serde_json::json!([]);
        v["hooks"]["SessionStart"] = entry;
        fs::write(f, v.to_string()).unwrap();
    }
    let (code, rows) = paths(d, &stacks, stub, "trusted");
    assert_eq!(code, Some(1), "{rows:?}");
    assert_eq!(
        (row(&rows, "claude").0.as_str(), row(&rows, "codex").0.as_str()),
        ("incomplete", "incomplete"),
        "{rows:?}"
    );
    let mut off = synced;
    off["disableAllHooks"] = serde_json::json!(true);
    fs::write(&claude, off.to_string()).unwrap();
    let (_, rows) = paths(d, &stacks, stub, "trusted");
    assert_eq!(row(&rows, "claude").0, "incomplete", "hooks switched off: {rows:?}");
}

// Spec (review r1, F10-R1-03, the Lords owner: "complete means the whole start is present: the place file for that
// adapter as well as the role's supplement"; its witness, a supported adapter-filtered sync): after `sync --repo NAME
// --adapter claude --apply`, the checkout has every role's start and CLAUDE.local.md, which carries a repo-wide line the
// starts leave out, but no AGENTS.override.md: Codex's path there is incomplete, exit 1.
#[test]
fn paths_needs_each_harnesss_place_file_beside_the_starts() {
    let d = TempDir::new().unwrap();
    let d = d.path();
    let stacks = stacks(d, "[\"role\"]", "[\"role\"]");
    let stacks: Vec<&str> = stacks.iter().map(String::as_str).collect();
    wide_rule(d);
    let stub = env!("CARGO_BIN_EXE_lords-codex-stub");
    assert!(lords(d, &["guard", "install"], &[]).status.success());
    assert!(lords(d, &[&["sync", "--user", "--apply"][..], &stacks].concat(), &[])
        .status
        .success());
    let claude_only = ["sync", "--repo", "beta-core", "--adapter", "claude", "--apply"];
    assert!(lords(d, &[&claude_only[..], &stacks].concat(), &[]).status.success());
    let start = fs::read_to_string(d.join("checkout/.lords/start-reviewer.md")).unwrap();
    let place = fs::read_to_string(d.join("checkout/CLAUDE.local.md")).unwrap();
    assert!(!start.contains("REPO-WIDE line.") && place.contains("REPO-WIDE line."), "{start}");
    assert!(!d.join("checkout/AGENTS.override.md").exists());
    let (code, rows) = paths(d, &stacks, stub, "trusted");
    assert_eq!(code, Some(1), "{rows:?}");
    let missing =
        rows.iter().find(|r| r.1.contains("AGENTS.override.md") && r.1.contains("beta-core"));
    assert!(missing.is_some_and(|r| r.0 == "incomplete"), "{rows:?}");
    assert!(
        !rows.iter().any(|r| r.0 == "incomplete" && r.1.contains("CLAUDE.local.md")),
        "{rows:?}"
    );
}

// Spec (fix check r1, F10-FC1-02, the Lords owner: "`lords paths` and the hook decide 'can deliver here' with one shared
// predicate. paths can't call a path complete where the hook would refuse"; its witness, a place whose own composition
// is empty, after an adapter-filtered sync): where the registered Codex hook gives the role-file line instead of the
// start (no Lords AGENTS.override.md there), `lords paths` marks that path incomplete, exit 1.
#[test]
fn paths_and_the_hook_agree_where_a_place_file_is_missing() {
    let d = TempDir::new().unwrap();
    let d = d.path();
    let stacks = stacks(d, "[\"role\"]", "[\"role\"]");
    let stacks: Vec<&str> = stacks.iter().map(String::as_str).collect();
    let stub = env!("CARGO_BIN_EXE_lords-codex-stub");
    assert!(lords(d, &["guard", "install"], &[]).status.success());
    assert!(lords(d, &[&["sync", "--user", "--apply"][..], &stacks].concat(), &[])
        .status
        .success());
    let claude_only = ["sync", "--repo", "beta-core", "--adapter", "claude", "--apply"];
    assert!(lords(d, &[&claude_only[..], &stacks].concat(), &[]).status.success());
    let ev = serde_json::json!({"hook_event_name": "UserPromptSubmit", "session_id": "s1",
        "cwd": d.join("checkout"), "prompt": "Role: reviewer\nReview it.", "transcript_path": "x"});
    let tool_home = d.join("home/.lords-command");
    let args = ["guard", "role", "--home", tool_home.to_str().unwrap(), "--adapter", "codex"];
    let told = context(&run(&d.join("home"), &d.join("checkout"), &args, &ev.to_string(), &[]));
    let told = told.unwrap_or_default();
    assert!(told.contains("AGENTS.override.md") && told.contains("role-reviewer.md"), "{told}");
    let (code, rows) = paths(d, &stacks, stub, "trusted");
    let refused =
        rows.iter().find(|r| r.1.contains("as reviewer") && r.1.contains("AGENTS.override.md"));
    assert!(code == Some(1) && refused.is_some_and(|r| r.0 == "incomplete"), "{rows:?}");
}

/// Sets `path`'s modification time `days` days back.
fn age(path: &Path, days: u64) {
    let then = std::time::SystemTime::now() - std::time::Duration::from_secs(days * 86_400);
    fs::File::options().write(true).open(path).unwrap().set_modified(then).unwrap();
}

/// Ages the git files that mark a checkout or worktree in use (design note P1): its HEAD and index in `git_dir`, and
/// its branch's loose ref in the repo's `common` dir.
fn age_git_files(git_dir: &Path, common: &Path, days: u64) {
    let head = fs::read_to_string(git_dir.join("HEAD")).unwrap();
    let branch = head.trim().strip_prefix("ref: ").unwrap();
    for p in [git_dir.join("HEAD"), git_dir.join("index"), common.join(branch)] {
        age(&p, days);
    }
}

// Spec (D15, the Lords owner's contract, 2 Nov: "By default, `paths` shows the places in use and folds the rest into
// one count line, naming the flag that lists them … `--all` shows every row, as today … The machine-readable output
// (`--json`) keeps every row, with an `in_use` field"; design note 2026-11-02-lords-hooks-dispatcher, P1: in use is a
// declared checkout, a workspace, or a worktree whose HEAD, index or branch ref changed within 3 days): of two
// worktrees without their starts, the one untouched for 10 days is folded into one count line naming `--all`, the
// fresh one is listed, and so is the declared checkout, aged 10 days too; `--all` lists all three; `--json` has every
// row with `in_use`; exit 1 (rows incomplete) in every form.
#[test]
fn paths_folds_the_places_not_in_use_into_one_line() {
    let d = TempDir::new().unwrap();
    let d = d.path();
    let stacks = stacks(d, "[]", "[]");
    let stacks: Vec<&str> = stacks.iter().map(String::as_str).collect();
    let manifest = fs::read_to_string(d.join("priv/stack.toml")).unwrap();
    fs::write(d.join("priv/stack.toml"), manifest.replace("worktrees = false", "worktrees = true"))
        .unwrap();
    git(d, &d.join("priv"), &["commit", "-q", "-a", "-m", "worktrees"]);
    git(d, &d.join("checkout"), &["worktree", "add", "-q", "../wt-old"]);
    git(d, &d.join("checkout"), &["worktree", "add", "-q", "../wt-new"]);
    let common = d.join("checkout/.git");
    age_git_files(&common.join("worktrees/wt-old"), &common, 10);
    age_git_files(&common, &common, 10);

    let o = lords(d, &[&["paths"][..], &stacks].concat(), &[]);
    let out = String::from_utf8_lossy(&o.stdout).into_owned();
    assert_eq!(o.status.code(), Some(1), "{}", said(&o));
    assert!(out.contains("/wt-new)") && out.contains("/checkout)"), "{out}");
    assert!(!out.contains("/wt-old)"), "{out}");
    let fold: Vec<&str> = out.lines().filter(|l| l.contains("--all")).collect();
    assert_eq!(fold.len(), 1, "{out}");
    let per_place = out.lines().filter(|l| l.contains("/wt-new)")).count();
    assert!(per_place > 0 && fold[0].starts_with(&format!("{per_place} rows")), "{out}");

    let o = lords(d, &[&["paths", "--all"][..], &stacks].concat(), &[]);
    let all = String::from_utf8_lossy(&o.stdout).into_owned();
    assert_eq!(o.status.code(), Some(1), "{}", said(&o));
    assert!(all.contains("/wt-old)") && all.contains("/wt-new)") && all.contains("/checkout)"));
    assert!(!all.contains("--all"), "{all}");

    let o = lords(d, &[&["paths", "--json"][..], &stacks].concat(), &[]);
    assert_eq!(o.status.code(), Some(1), "{}", said(&o));
    let v: serde_json::Value = serde_json::from_slice(&o.stdout).unwrap();
    let rows = v.as_array().unwrap();
    assert_eq!(rows.len(), all.lines().count(), "every row: {rows:?}");
    for r in rows {
        let path = r["path"].as_str().unwrap();
        let in_use = r["in_use"].as_bool().unwrap_or_else(|| panic!("no in_use: {r}"));
        assert_eq!(in_use, !path.contains("/wt-old)"), "{r}");
    }

    // Review r1, R1-03, its witness: a session opened in the aged worktree (Lords' session hook records it, as Claude
    // Code's SessionStart runs it), with no git write, makes that place in use: its rows are listed, nothing folds. Its
    // cwd names the worktree through an alias, a junction or symlink (sol-loop repair r1, R1-03), and is removed after
    // the session started (sol-loop repair r2, R2-01): the record still names that place.
    fs::create_dir_all(d.join("wt-old/sub")).unwrap();
    #[cfg(windows)]
    let o = Command::new("cmd")
        .args(["/c", "mklink", "/J"])
        .arg(d.join("alias"))
        .arg(d.join("wt-old"))
        .output();
    #[cfg(unix)]
    let o = std::os::unix::fs::symlink(d.join("wt-old"), d.join("alias"));
    assert!(o.is_ok() && d.join("alias/sub").is_dir(), "{o:?}");
    let ev = serde_json::json!({"hook_event_name": "SessionStart", "session_id": "s-old",
        "cwd": d.join("alias/sub"), "source": "startup", "transcript_path": "x"});
    assert!(run(&d.join("home"), d, &["guard", "session"], &ev.to_string(), &[]).status.success());
    fs::remove_dir(d.join("wt-old/sub")).unwrap();
    let o = lords(d, &[&["paths"][..], &stacks].concat(), &[]);
    let out = String::from_utf8_lossy(&o.stdout).into_owned();
    assert_eq!(o.status.code(), Some(1), "{}", said(&o));
    assert!(out.contains("/wt-old)") && !out.contains("--all"), "{out}");
}
