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
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for (k, v) in env {
        c.env(k, v);
    }
    let mut c = c.spawn().unwrap();
    c.stdin.take().unwrap().write_all(stdin.as_bytes()).unwrap();
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

// Spec (F10: "the role comes from a signal the launcher already gives, such as the brief's first line or T3's `role`,
// read by the turn hook on the first prompt. The hook then adds that role's lines"; design note D3, D4; Claude Code's and
// Codex's `UserPromptSubmit` take `hookSpecificOutput.additionalContext`): the first prompt naming a role gets that
// role's start from the nearest `.lords`, without its marker lines; the same role named again in the session gets
// nothing; another role named later gets that one's; another session gets it again; a prompt naming no role gets
// nothing. T3's prefix names the role too, and where the brief's line differs the brief wins, said in one line.
#[test]
fn the_role_guard_gives_the_named_roles_start_once_a_session() {
    let p = Place::new();
    assert_eq!(p.ask("s1", "Role: reviewer\nReview the diff.").as_deref(), Some("REVIEWER START."));
    assert_eq!(p.ask("s1", "Role: reviewer\nAnd again."), None, "once a session");
    assert_eq!(p.ask("s1", "Fix the finding."), None, "no role named");
    assert_eq!(p.ask("s1", "Role: coder\nBuild it.").as_deref(), Some("CODER START."));
    assert_eq!(p.ask("s2", "Role: reviewer\n").as_deref(), Some("REVIEWER START."));
    let t3 = "Act as the review sub-agent for this task.\n\nLook at it.";
    assert_eq!(p.ask("s3", t3).as_deref(), Some("REVIEWER START."));
    let both =
        p.ask("s4", "Act as the review sub-agent for this task.\n\nRole: coder\nBuild.").unwrap();
    assert!(both.contains("CODER START.") && !both.contains("REVIEWER START."), "{both}");
    assert_eq!(both.lines().count(), 2, "the start and one line: {both}");
    assert!(both.contains("reviewer"), "names T3's role it overrules: {both}");
    // Sol-loop r1 S3: a record that can't be read (here a dir where the file goes) is no reason to give nothing.
    fs::create_dir_all(p.tool_home().join("guard/role/s6")).unwrap();
    assert_eq!(p.ask("s6", "Role: reviewer\n").as_deref(), Some("REVIEWER START."));
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

// Spec (design note C2: a session that can't get its role's lines is told so at its first prompt, with the role file to
// read, never left to look complete; C4: nothing for a session T3 didn't delegate and whose launch names no role): a
// role with no start in the place, a start past one hook's 9,500 characters, and a delegated session (`T3CODE_THREAD_ID`
// `thread:delegated-task:…`) whose launch names no role each get one line saying so, naming the role file to read where
// there is a role, once; a root session naming no role gets nothing and leaves no record; a cwd in no place gets
// nothing.
#[test]
fn a_session_whose_role_cant_be_given_is_told_once() {
    let p = Place::new();
    let lead = p.ask("s1", "Role: lead\nLead.").unwrap();
    assert_eq!(lead.lines().count(), 1, "{lead}");
    assert!(lead.contains("lead") && lead.contains("role-lead.md"), "{lead}");
    assert_eq!(p.ask("s1", "Role: lead\nLead."), None, "once");
    let long: Vec<String> =
        (0..300).map(|i| format!("- rule.r{i:03}: Line {i} of the long start.")).collect();
    p.start("reviewer", &long.join("\n"));
    let told = p.ask("s2", "Role: reviewer\n").unwrap();
    assert_eq!(told.lines().count(), 1, "{told}");
    assert!(told.contains("role-reviewer.md") && told.contains("9500"), "{told}");
    assert!(!told.contains("Line 1 of"), "{told}");
    let delegated = [("T3CODE_THREAD_ID", "thread:delegated-task:command%3Amcp%3Ax")];
    let first = context(&p.prompt("s3", &p.cwd(), "Fix the bug.", &delegated)).unwrap();
    assert_eq!(first.lines().count(), 1, "{first}");
    assert!(first.contains("no role") && first.contains("Role:"), "{first}");
    assert_eq!(context(&p.prompt("s3", &p.cwd(), "And this.", &delegated)), None, "once");
    let root = [("T3CODE_THREAD_ID", "thread:4a1b")];
    assert_eq!(context(&p.prompt("s4", &p.cwd(), "Fix the bug.", &root)), None);
    assert!(
        !p.tool_home().join("guard/role/s4").exists(),
        "no record for a root session naming no role"
    );
    let elsewhere = p.dir.path().join("elsewhere");
    fs::create_dir_all(&elsewhere).unwrap();
    assert_eq!(context(&p.prompt("s5", &elsewhere, "Role: reviewer\n", &delegated)), None);
}

// Spec (design note C5, D5; Claude Code's hooks docs: `SessionStart` runs with source `compact` after a compaction and
// takes `additionalContext`): after a compaction the session guard gives the role recorded for the session again; at
// any other start, or for a session given no role, nothing.
#[test]
fn the_session_guard_gives_the_role_again_after_a_compaction() {
    let p = Place::new();
    assert_eq!(p.ask("s1", "Role: reviewer\n").as_deref(), Some("REVIEWER START."));
    let start = |session: &str, source: &str| {
        let ev = serde_json::json!({"hook_event_name": "SessionStart", "session_id": session,
            "cwd": p.cwd(), "source": source, "transcript_path": "x"});
        context(&run(&p.home(), &p.cwd(), &["guard", "session"], &ev.to_string(), &[]))
    };
    assert_eq!(start("s1", "compact").as_deref(), Some("REVIEWER START."));
    assert_eq!(start("s1", "startup"), None);
    assert_eq!(start("s1", "resume"), None, "a resumed session keeps its history");
    assert_eq!(start("s9", "compact"), None, "no role given");
}

// Spec (design note D6, as for the child guard, fix check r2, FC4): asked with the hook's arguments and the probe
// event, the guard answers its role protocol, 1.
#[test]
fn the_guard_answers_the_probe_with_its_role_protocol() {
    let p = Place::new();
    let home = p.tool_home();
    let o = run(
        &p.home(),
        &p.cwd(),
        &["guard", "role", "--home", home.to_str().unwrap()],
        r#"{"hook_event_name":"LordsGuardProbe"}"#,
        &[],
    );
    assert_eq!(String::from_utf8_lossy(&o.stdout).trim(), "lords guard role 1", "{o:?}");
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
    let hook = serde_json::json!({"type": "command", "command": copy, "args": ["guard", "role", "--home", tool_home], "timeout": 30});
    let v = read("home/.claude-balanced/settings.json");
    assert_eq!(v["hooks"]["UserPromptSubmit"], serde_json::json!([{"hooks": [hook]}]), "{v}");
    // On Windows Codex runs a hook's command with PowerShell (design note H9): PowerShell's call, literal paths.
    let line = if cfg!(windows) {
        format!("& '{copy}' guard role --home '{tool_home}'")
    } else {
        format!("\"{copy}\" guard role --home \"{tool_home}\"")
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
