//! `lords guard child`, a `SubagentStart` hook (plan L6, an orchestrator's condition 3: "Children start with their own role's
//! core, not the parent's"): it gives a native child its place's `.lords/child.md`, which sync composes for the stack's
//! `child_role`. The child gets an empty home, so no real config, stack or setting is read or touched.

// Tests are outside the filesystem boundary (fsguard.rs): their fixtures write their own temp dirs.
#![allow(clippy::disallowed_methods, clippy::disallowed_types)]

use std::fs;
use std::io::Write;
use std::path::Path;
use std::process::{Command, Output, Stdio};
use tempfile::TempDir;

const BEGIN: &str = "<!-- lords-command:begin -->";
const END: &str = "<!-- lords-command:end -->";

fn run(home: &Path, cwd: &Path, args: &[&str], stdin: &str) -> Output {
    let mut c = Command::new(env!("CARGO_BIN_EXE_lords"))
        .current_dir(cwd)
        .env("HOME", home)
        .env("USERPROFILE", home)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    c.stdin.take().unwrap().write_all(stdin.as_bytes()).unwrap();
    c.wait_with_output().unwrap()
}

fn event(cwd: &Path) -> String {
    serde_json::json!({"hook_event_name": "SubagentStart", "session_id": "s1", "agent_id": "a1",
        "agent_type": "general-purpose", "cwd": cwd})
    .to_string()
}

// Spec (Claude Code's and Codex's hook docs: `SubagentStart` takes `hookSpecificOutput.additionalContext`; the guards'
// common rule, never fail a harness's event): for a child whose cwd lies in a place, the guard answers the place's
// `.lords/child.md` as the child's context, without the marker lines, exit 0; where no place has one it answers
// nothing; an event of another kind, or input it can't read, is exit 0 with nothing on stdout.
#[test]
fn the_child_guard_gives_a_child_its_places_child_file() {
    let d = TempDir::new().unwrap();
    let home = d.path().join("home");
    let cwd = d.path().join("place/a/b");
    fs::create_dir_all(&home).unwrap();
    fs::create_dir_all(&cwd).unwrap();
    let file = format!(
        "{BEGIN}\n<!-- header -->\nYou start as a helper here.\n- rule.x: A line.\n{END}\n"
    );
    fs::create_dir_all(d.path().join("place/.lords")).unwrap();
    fs::write(d.path().join("place/.lords/child.md"), &file).unwrap();
    let o = run(&home, &cwd, &["guard", "child"], &event(&cwd));
    assert_eq!(o.status.code(), Some(0), "{o:?}");
    let v: serde_json::Value = serde_json::from_slice(&o.stdout).unwrap();
    assert_eq!(v["hookSpecificOutput"]["hookEventName"], "SubagentStart", "{v}");
    let context = v["hookSpecificOutput"]["additionalContext"].as_str().unwrap();
    assert_eq!(context, "<!-- header -->\nYou start as a helper here.\n- rule.x: A line.", "{v}");
    let elsewhere = d.path().join("elsewhere");
    fs::create_dir_all(&elsewhere).unwrap();
    for (cwd, input) in [
        (&elsewhere, event(&elsewhere)),
        (&cwd, event(&cwd).replace("SubagentStart", "SessionStart")),
        (&cwd, "not json".to_string()),
    ] {
        let o = run(&home, cwd, &["guard", "child"], &input);
        assert_eq!(o.status.code(), Some(0), "{input}: {o:?}");
        assert!(o.stdout.is_empty(), "{input}: {o:?}");
    }
}

/// The context one `guard child` call answers, or `None` for no output.
fn context(o: &Output) -> Option<String> {
    let v: serde_json::Value = serde_json::from_slice(&o.stdout).ok()?;
    Some(v["hookSpecificOutput"]["additionalContext"].as_str()?.to_string())
}

// Spec (review r1, F1: Explore and Plan "start without the shared binding core"; Claude Code's hooks docs: a hook's
// `additionalContext` past 10,000 characters becomes a file path and a preview, and "Claude Code measures each string on
// its own, even when several hooks run for the same event"; the parts probe, design note D4): a child that loads no
// place file (`agent_type` Explore or Plan) gets the place's `.lords/child-whole.md`, any other `child.md`. With
// `--part N` each call answers the Nth piece, cut at line ends, each under 10,000 characters, together the whole file
// in order, and nothing past the last; without `--part` (Codex, no such limit), the whole file at once.
#[test]
fn a_child_with_no_place_file_gets_the_whole_start_in_parts() {
    let d = TempDir::new().unwrap();
    let home = d.path().join("home");
    let cwd = d.path().join("place");
    fs::create_dir_all(cwd.join(".lords")).unwrap();
    fs::create_dir_all(&home).unwrap();
    let lines: Vec<String> = (0..300)
        .map(|i| format!("- rule.r{i:03}: Line {i}, about as long as a core line is."))
        .collect();
    fs::write(cwd.join(".lords/child-whole.md"), format!("{BEGIN}\n{}\n{END}\n", lines.join("\n")))
        .unwrap();
    fs::write(cwd.join(".lords/child.md"), format!("{BEGIN}\nONLY THE DELTA.\n{END}\n")).unwrap();
    // Each scenario is its own event (its own agent id): one event's parts share one generation (fix check r1, FC1).
    let with = |agent: &str| {
        event(&cwd).replace("general-purpose", agent).replace("\"a1\"", &format!("\"{agent}-1\""))
    };
    for agent in ["Explore", "Plan"] {
        let parts: Vec<Option<String>> = (1..=4)
            .map(|n| {
                context(&run(
                    &home,
                    &cwd,
                    &["guard", "child", "--part", &n.to_string()],
                    &with(agent),
                ))
            })
            .collect();
        assert!(
            parts[0].is_some() && parts[1].is_some() && parts[3].is_none(),
            "{agent}: {parts:?}"
        );
        let got: Vec<&str> = parts
            .iter()
            .flatten()
            .flat_map(|p| p.lines())
            .filter(|l| l.starts_with("- rule."))
            .collect();
        assert_eq!(
            got,
            lines.iter().map(String::as_str).collect::<Vec<_>>(),
            "{agent}: every line, in order"
        );
        for p in parts.iter().flatten() {
            assert!(
                p.chars().count() < 10_000,
                "{agent}: a part of {} characters",
                p.chars().count()
            );
        }
        let all = context(&run(&home, &cwd, &["guard", "child"], &with(agent))).unwrap();
        assert_eq!(
            all.lines().filter(|l| l.starts_with("- rule.")).count(),
            300,
            "{agent}: whole without --part"
        );
    }
    let first = context(&run(&home, &cwd, &["guard", "child", "--part", "1"], &event(&cwd)));
    assert_eq!(
        first.as_deref().map(|c| c.contains("ONLY THE DELTA.")),
        Some(true),
        "general-purpose: the delta"
    );
    let second = run(&home, &cwd, &["guard", "child", "--part", "2"], &event(&cwd));
    assert!(second.stdout.is_empty(), "nothing past the delta's one part");
    // Fix check r1, FC2: parts are cut only at line ends, so a start holding a line longer than a part is delivered
    // nowhere, with a note (sync never writes one: see core_lines.rs).
    let long = format!("- rule.long: {}", "y".repeat(25_000));
    fs::write(cwd.join(".lords/child-whole.md"), format!("{BEGIN}\n- rule.a: A.\n{long}\n{END}\n"))
        .unwrap();
    let explore = |id: &str| with("Explore").replace("Explore-1", id);
    for n in 1..=4 {
        let o = run(&home, &cwd, &["guard", "child", "--part", &n.to_string()], &explore("long"));
        assert!(o.stdout.is_empty(), "part {n}: nothing: {o:?}");
        let note = String::from_utf8_lossy(&o.stderr);
        assert!(
            note.contains("line") && note.contains("25"),
            "part {n}: the note says why: {note}"
        );
    }
    // Repair sol-loop r1, R4: an Explore child never takes the delta for its whole start.
    fs::remove_file(cwd.join(".lords/child-whole.md")).unwrap();
    let o = run(&home, &cwd, &["guard", "child", "--part", "1"], &explore("no-whole"));
    assert!(o.stdout.is_empty(), "no whole start, no delta in its place: {o:?}");
}

/// A `SubagentStart` event of `agent_type`, in `cwd`, with these ids.
fn start(cwd: &Path, agent_type: &str, session: &str, agent: &str) -> String {
    serde_json::json!({"hook_event_name": "SubagentStart", "session_id": session, "agent_id": agent,
        "agent_type": agent_type, "cwd": cwd})
    .to_string()
}

/// Parts 1 to 3 of one event's start, read in that order, each as the guard answers it.
fn parts_of(home: &Path, cwd: &Path, ev: &str) -> Vec<String> {
    (1..=3)
        .filter_map(|n| context(&run(home, cwd, &["guard", "child", "--part", &n.to_string()], ev)))
        .collect()
}

// Spec (fix check r1, FC1, the contract: "one child-start event delivers one complete generation of the child's start:
// every part read for that event comes from the same published generation, whatever a concurrent `sync --apply` does
// between the part reads"; its witness): a place synced with a helper start of three parts, `rule.m` in part 2. One
// Explore event reads its part 1; `rule.a` shrinks and a real `lords sync --apply` publishes the next generation, in
// which the unchanged `rule.m` sits in part 1; the event then reads its parts 2 and 3. It gets the first generation
// whole, `rule.m` included. A new event gets the second generation.
#[test]
fn one_start_event_gets_one_generation_across_a_sync() {
    let d = TempDir::new().unwrap();
    let home = d.path().join("home");
    let (public, private, checkout) =
        (d.path().join("pub"), d.path().join("priv"), d.path().join("checkout"));
    for dir in [&home, &public, &private, &checkout] {
        fs::create_dir_all(dir).unwrap();
    }
    let git = |dir: &Path, args: &[&str]| {
        let gitconfig = d.path().join("gitconfig");
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
    };
    fs::write(checkout.join("README.md"), "a repo\n").unwrap();
    git(&checkout, &["init", "-q"]);
    git(&checkout, &["add", "-A"]);
    git(&checkout, &["commit", "-q", "-m", "start"]);
    fs::write(public.join("stack.toml"), "name = \"pub\"\nvisibility = \"public\"\n").unwrap();
    fs::write(
        private.join("stack.toml"),
        format!(
            "name = \"priv\"\nvisibility = \"private\"\nroles = [\"orchestrator\", \"coder\", \"helper\", \"product\"]\nchild_role = \"helper\"\n[projects.beta]\n[repos.beta-core]\nproject = \"beta\"\ncheckouts = ['{}']\nworktrees = false\ndefault_role = \"orchestrator\"\nadapters = [\"claude\"]\n",
            checkout.display()
        ),
    )
    .unwrap();
    let rule = |slug: &str, lines: &[String]| {
        let lines: Vec<String> = lines.iter().map(|l| format!("- line: \"{l}\"")).collect();
        let text = format!(
            "---\nid: rule.{slug}\nkind: rule\ntitle: rule.{slug}\naudience: [all]\nroles: [helper]\nscope: project:beta\nstatus: active\nvisibility: private\norigin:\n  type: own\nstart: core\ncore:\n{}\n---\nThe whole rule.\n",
            lines.join("\n")
        );
        fs::create_dir_all(private.join("project/beta/rules")).unwrap();
        fs::write(private.join(format!("project/beta/rules/{slug}.md")), text).unwrap();
    };
    let long = |n: usize, what: &str| -> Vec<String> {
        (0..n).map(|i| format!("{what} {i:02} {}", "x".repeat(390))).collect()
    };
    rule("a", &long(25, "A"));
    rule("m", &["Never publish the draft.".to_string()]);
    rule("z", &long(30, "Z"));
    git(&private, &["init", "-q"]);
    git(&private, &["add", "-A"]);
    git(&private, &["commit", "-q", "-m", "start"]);
    let stacks =
        ["--public-stack", public.to_str().unwrap(), "--private-stack", private.to_str().unwrap()];
    let sync = || {
        let o = run(
            &home,
            d.path(),
            &[&["sync", "--repo", "beta-core", "--apply"][..], &stacks].concat(),
            "",
        );
        assert!(o.status.success(), "sync: {o:?}");
    };
    sync();
    let m = "- rule.m: Never publish the draft.";
    let part_of_m = |parts: &[String]| parts.iter().position(|p| p.contains(m));
    let first = parts_of(&home, &checkout, &start(&checkout, "Explore", "s1", "before"));
    assert_eq!(
        (first.len(), part_of_m(&first)),
        (3, Some(1)),
        "the first generation: three parts, rule.m in part 2"
    );
    let one = |n: usize| {
        context(&run(
            &home,
            &checkout,
            &["guard", "child", "--part", &n.to_string()],
            &start(&checkout, "Explore", "s1", "event"),
        ))
        .unwrap_or_default()
    };
    let mut got = vec![one(1)];
    rule("a", &long(20, "A"));
    git(&private, &["commit", "-q", "-a", "-m", "a shorter rule.a"]);
    sync();
    let second = parts_of(&home, &checkout, &start(&checkout, "Explore", "s1", "after"));
    assert_eq!(
        (second.len(), part_of_m(&second)),
        (3, Some(0)),
        "the second generation: rule.m in part 1"
    );
    got.push(one(2));
    got.push(one(3));
    assert!(
        part_of_m(&got).is_some(),
        "rule.m reached the child: {:?}",
        got.iter().map(String::len).collect::<Vec<_>>()
    );
    assert_eq!(got, first, "one generation, whole");
}

// Spec (fix check r1, FC1: "Publication also leaves readers no gap: no reader ever finds an enclosing place or nothing
// because sync is between moving the old `.lords` aside and publishing the new one"; fsguard names the old one
// `..lords.lords-old-*` beside it): a child in a place whose `.lords` is moved aside never gets the enclosing place's
// start: it waits for the new `.lords` and gets that, and while none comes back it gets nothing, with a note. A `--part`
// event naming no agent id can't share one generation across its parts and gets nothing, with a note.
#[test]
fn a_child_never_falls_through_to_an_enclosing_place_mid_publication() {
    let d = TempDir::new().unwrap();
    let home = d.path().join("home");
    let (outer, inner) = (d.path().join("ws"), d.path().join("ws/repo"));
    fs::create_dir_all(&home).unwrap();
    fs::create_dir_all(outer.join(".lords")).unwrap();
    fs::write(outer.join(".lords/child-whole.md"), format!("{BEGIN}\nOUTER START.\n{END}\n"))
        .unwrap();
    let aside = inner.join("..lords.lords-old-1-2-3");
    fs::create_dir_all(&aside).unwrap();
    fs::write(aside.join("child-whole.md"), format!("{BEGIN}\nINNER START.\n{END}\n")).unwrap();
    // While the aside stays: nothing, and never the enclosing place's start.
    let o = run(
        &home,
        &inner,
        &["guard", "child", "--part", "1"],
        &start(&inner, "Explore", "s1", "stuck"),
    );
    assert!(o.stdout.is_empty(), "nothing, never OUTER: {o:?}");
    assert!(String::from_utf8_lossy(&o.stderr).contains("sync"), "the note says why: {o:?}");
    // The new `.lords` comes in while the child waits, as sync puts it in: under the apply lock, `<tool home>/apply.lock`
    // (repair 2 sol-loop r2, W1: a reader that passed a missing `.lords` decides under that lock). It gets that.
    // A tool home other than the default, as `sync --home DIR` uses, named to the guard as sync's hooks name it
    // (repair 2 sol-loop r3, X1): the child takes that home's lock.
    let tool_home = d.path().join("other-home");
    let lock = tool_home.join("apply.lock");
    fs::create_dir_all(&tool_home).unwrap();
    fs::write(&lock, "pid 1\n").unwrap();
    let mut c = Command::new(env!("CARGO_BIN_EXE_lords"))
        .current_dir(&inner)
        .env("HOME", &home)
        .env("USERPROFILE", &home)
        .args(["guard", "child", "--part", "1", "--home", tool_home.to_str().unwrap()])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    c.stdin.take().unwrap().write_all(start(&inner, "Explore", "s1", "waits").as_bytes()).unwrap();
    std::thread::sleep(std::time::Duration::from_millis(300));
    // W1's witness: between two swaps the aside is gone and no `.lords` is there yet; the child mustn't walk on.
    fs::rename(&aside, d.path().join("held-by-sync")).unwrap();
    std::thread::sleep(std::time::Duration::from_millis(300));
    fs::rename(d.path().join("held-by-sync"), inner.join(".lords")).unwrap();
    fs::remove_file(&lock).unwrap();
    let got = context(&c.wait_with_output().unwrap()).unwrap_or_default();
    assert!(got.contains("INNER START.") && !got.contains("OUTER"), "{got}");
    let no_id = start(&inner, "Explore", "s1", "x").replace("\"agent_id\":\"x\",", "");
    assert!(!no_id.contains("agent_id"), "{no_id}");
    let o = run(&home, &inner, &["guard", "child", "--part", "1"], &no_id);
    assert!(o.stdout.is_empty(), "no agent id, no parts: {o:?}");
    assert!(String::from_utf8_lossy(&o.stderr).contains("agent id"), "{o:?}");
}

// Spec (fix check r1, FC1: one event gets one generation whole; repair 2 sol-loop r1, V1: a part that finds its place
// mid-publication must not leave its event to later parts of another generation): an event whose first part waited
// out a place whose `.lords` stayed aside gets nothing in every part, also once the new `.lords` is in; a new event
// gets the new start whole.
#[test]
fn an_event_whose_first_part_failed_gets_nothing_in_any_part() {
    let d = TempDir::new().unwrap();
    let home = d.path().join("home");
    let place = d.path().join("repo");
    fs::create_dir_all(&home).unwrap();
    let aside = place.join("..lords.lords-old-1-2-3");
    fs::create_dir_all(&aside).unwrap();
    let lines: Vec<String> = (0..300)
        .map(|i| format!("- rule.r{i:03}: Line {i}, about as long as a core line is."))
        .collect();
    fs::write(aside.join("child-whole.md"), format!("{BEGIN}\n{}\n{END}\n", lines.join("\n")))
        .unwrap();
    let ev = start(&place, "Explore", "s1", "failed");
    let o = run(&home, &place, &["guard", "child", "--part", "1"], &ev);
    assert!(o.stdout.is_empty(), "part 1 waited and gave nothing: {o:?}");
    fs::rename(&aside, place.join(".lords")).unwrap();
    for n in ["2", "3"] {
        let o = run(&home, &place, &["guard", "child", "--part", n], &ev);
        assert!(
            o.stdout.is_empty(),
            "part {n} of the failed event: nothing, never another generation's part: {o:?}"
        );
    }
    let fresh = parts_of(&home, &place, &start(&place, "Explore", "s1", "fresh"));
    let got: Vec<&str> =
        fresh.iter().flat_map(|p| p.lines()).filter(|l| l.starts_with("- rule.")).collect();
    assert_eq!(got.len(), 300, "a new event gets the whole start");
}

// Spec (fix check r1, FC2: the guard delivers no child start holding a line longer than a part; repair 2 sol-loop r1,
// V2, its witnesses): not without `--part` (Codex) either, and not when the whole start fits one part.
#[test]
fn a_start_with_a_line_past_a_part_is_delivered_nowhere() {
    let d = TempDir::new().unwrap();
    let home = d.path().join("home");
    let place = d.path().join("repo");
    fs::create_dir_all(place.join(".lords")).unwrap();
    fs::create_dir_all(&home).unwrap();
    let line = |n: usize| format!("- rule.long: {}", "y".repeat(n - "- rule.long: ".len()));
    for (case, text, args) in [
        ("whole, no --part", line(12_000), &["guard", "child"][..]),
        ("one part's worth", line(9_469), &["guard", "child", "--part", "1"][..]),
    ] {
        fs::write(place.join(".lords/child-whole.md"), format!("{BEGIN}\n{text}\n{END}\n"))
            .unwrap();
        let o = run(&home, &place, args, &start(&place, "Explore", "s1", case));
        assert!(o.stdout.is_empty(), "{case}: nothing: {o:?}");
        assert!(
            String::from_utf8_lossy(&o.stderr).contains("line"),
            "{case}: the note says why: {o:?}"
        );
    }
}

// Spec (the brief, slice 4: "through each provider's subagent-start hook"; the Codex probe, design note: Codex's child
// takes a command hook's context once the hook is trusted): with `guards = ["child"]` and the copy installed, sync
// names the copy in one `SubagentStart` entry (`guard child`, no matcher, so every agent type), after the user's own,
// and the plan says so: in Claude Code's settings in its exec form; in Codex's `hooks.json` as a command line with the
// whole context passed (`additionalContextLimit` 0) and a note that Codex runs it once reviewed under /hooks. A guard
// Codex isn't shown to run is refused for a Codex target, and nothing is written.
#[test]
fn sync_registers_the_child_guard_on_subagent_start() {
    let d = TempDir::new().unwrap();
    let home = d.path().join("home");
    let (public, private) = (d.path().join("pub"), d.path().join("priv"));
    for dir in [&public, &private, &home.join(".claude-balanced"), &home.join(".codex")] {
        fs::create_dir_all(dir).unwrap();
    }
    let theirs = r#"{"hooks": [{"type": "command", "command": "notify"}]}"#;
    let settings = home.join(".claude-balanced/settings.json");
    fs::write(&settings, format!("{{\"hooks\": {{\"SubagentStart\": [{theirs}]}}}}\n")).unwrap();
    let codex = home.join(".codex/hooks.json");
    fs::write(public.join("stack.toml"), "name = \"pub\"\nvisibility = \"public\"\n").unwrap();
    let manifest = |codex_guards: &str| {
        format!("name = \"priv\"\nvisibility = \"private\"\n[[targets]]\nname = \"balanced settings\"\nadapter = \"claude\"\nkind = \"settings\"\npath = \"~/.claude-balanced/settings.json\"\nguards = [\"child\"]\n[[targets]]\nname = \"codex hooks\"\nadapter = \"codex\"\nkind = \"settings\"\npath = \"~/.codex/hooks.json\"\nguards = {codex_guards}\n")
    };
    fs::write(private.join("stack.toml"), manifest("[\"child\"]")).unwrap();
    let git = |args: &[&str]| {
        let gitconfig = d.path().join("gitconfig");
        // Built at run time, so the repo's own privacy check doesn't see an address in this source.
        let email = ["test", "example.invalid"].join("@");
        fs::write(&gitconfig, format!("[user]\n\tname = Test\n\temail = {email}\n")).unwrap();
        Command::new("git")
            .current_dir(&private)
            .env("GIT_CONFIG_GLOBAL", &gitconfig)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .args(args)
            .output()
            .unwrap()
    };
    git(&["init", "-q"]);
    git(&["add", "-A"]);
    git(&["commit", "-q", "-m", "start"]);
    let o = run(&home, d.path(), &["guard", "install"], "");
    assert!(o.status.success(), "{o:?}");
    let stacks =
        ["--public-stack", public.to_str().unwrap(), "--private-stack", private.to_str().unwrap()];
    let o = run(&home, d.path(), &[&["sync", "--user"][..], &stacks].concat(), "");
    let shown = String::from_utf8_lossy(&o.stdout);
    assert_eq!(
        shown.matches("Lords hook added: the child guard (SubagentStart)").count(),
        2,
        "{o:?}"
    );
    assert!(shown.contains("/hooks"), "Codex's review is named: {o:?}");
    let o = run(&home, d.path(), &[&["sync", "--user", "--apply"][..], &stacks].concat(), "");
    assert!(o.status.success(), "{o:?}");
    let read = |p: &Path| -> serde_json::Value {
        serde_json::from_str(&fs::read_to_string(p).unwrap()).unwrap()
    };
    let copy =
        home.join(".lords-command/bin/lords-guard.exe").display().to_string().replace('\\', "/");
    // The tool home sync ran with, so the guard takes that home's apply lock and snapshots (repair 2 sol-loop r3, X1).
    let tool_home = home.join(".lords-command").display().to_string().replace('\\', "/");
    // Claude Code: one hook per part (review r1, F1), each its own 10,000 characters.
    let part = |n: &str| serde_json::json!({"type": "command", "command": copy, "args": ["guard", "child", "--part", n, "--home", tool_home]});
    let theirs: serde_json::Value = serde_json::from_str(theirs).unwrap();
    let v = read(&settings);
    assert_eq!(
        v["hooks"]["SubagentStart"],
        serde_json::json!([theirs, {"hooks": [part("1"), part("2"), part("3")]}]),
        "{v}"
    );
    let v = read(&codex);
    let line = format!("\"{copy}\" guard child --home \"{tool_home}\"");
    let lords = serde_json::json!({"type": "command", "command": line, "timeout": 30, "additionalContextLimit": 0});
    assert_eq!(v, serde_json::json!({"hooks": {"SubagentStart": [{"hooks": [lords]}]}}), "{v}");
    fs::write(private.join("stack.toml"), manifest("[\"child\", \"rm\"]")).unwrap();
    git(&["commit", "-q", "-a", "-m", "an rm guard for Codex"]);
    let o = run(&home, d.path(), &[&["sync", "--user", "--apply"][..], &stacks].concat(), "");
    assert!(!o.status.success(), "{o:?}");
    assert!(String::from_utf8_lossy(&o.stderr).contains("runs no `rm` guard"), "{o:?}");
    assert_eq!(read(&codex), v, "nothing written");
}
