//! `lords`: the Lords Command CLI. Small on purpose: the pre-commit hook runs it.

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
  lords hooks install [--repo DIR] [--secrets-only] [--force]   the pre-commit, pre-merge-commit and commit-msg hooks; in a stack repo also `check --stack` and a pre-push
  lords hash PATH
  lords sync [--user | --repo NAME [--place DIR]] [--adapter ID] [--stage install] [--json] [--apply]
  lords sync --restore BACKUP_DIR   undo one apply's owner-file edits, each only while it holds what sync wrote
  lords uninstall [--user | --repo NAME] [--apply]   remove the tool's paths that still match its ledger
  lords adopt [--list | PATH...]   record paths from before the ledger that hold exactly what sync writes there
  lords set-aside PATH... [--yes]   move paths that aren't the tool's into a new backup dir
  (sync, uninstall, adopt, set-aside: --home DIR, the tool's own dir; default ~/.lords-command)
  lords hooks install --post-checkout [--repo DIR]   a new worktree gets its place files (`sync --place`)
  lords compose [--repo NAME [--role ROLE]] [--adapter ID] [--json]   what a harness reads there, with sources
  lords sizes [--json]   every file sync writes, by section; fails past the warning line
  lords guard rm   Claude's PreToolUse hook: refuses an `rm` whose path holds a variable not written as quoted ${VAR:?}
  lords guard install [--home DIR]   install or update the guard copy the hooks run (sync never writes it)

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

/// `lords guard rm`: the tool-call guard Claude Code runs before a Bash call (guard.rs). Reads the event on stdin and
/// prints a denial, or nothing. It never fails the call: a usage error or input it can't read is a note on stderr and
/// exit 0 (orchestrator, 30 Oct: Claude Code takes exit 2 from a PreToolUse hook as a refusal). `lords guard
/// --self-test` prints `ok` when the guard refuses its witness; sync runs it before installing the copy hooks name.
fn cmd_guard(mut a: Args) -> ExitCode {
    let note = |line: &str| say_err(&format!("lords guard: {line}; this call is let through"));
    if a.flag("--help") || a.flag("-h") {
        say("usage: lords guard rm   (Claude Code's PreToolUse hook; the event on stdin)");
        return ExitCode::SUCCESS;
    }
    if a.flag("--self-test") {
        let witness = r#"{"tool_name":"Bash","tool_input":{"command":"rm -f $OUT/x"}}"#;
        let ok = lords_core::guard::rm_hook(witness).contains("\"deny\"");
        say(if ok { "ok" } else { "the guard let its witness through" });
        return if ok { ExitCode::SUCCESS } else { ExitCode::from(1) };
    }
    let which = a.positional();
    if which.as_deref() == Some("install") {
        return guard_install(a);
    }
    if which.as_deref() != Some("rm") || a.done().is_err() {
        note("usage `lords guard rm` (event on stdin)");
        return ExitCode::SUCCESS;
    }
    let input = match std::io::read_to_string(std::io::stdin()) {
        Ok(i) => i,
        Err(e) => {
            note(&format!("the event can't be read ({e})"));
            return ExitCode::SUCCESS;
        }
    };
    let out = lords_core::guard::rm_hook(&input);
    if !out.is_empty() {
        say(&out);
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
        Ok((lords_core::sync::guard_place(&home)?.path().to_path_buf(), wrote))
    });
    match done {
        Ok((place, wrote)) => {
            let what = if wrote { "installed" } else { "already current" };
            say(&format!("guard {what}: {} (passes its self-test)", place.display()));
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
    let stage = match a.value("--stage")?.as_deref() {
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
    let opts = sync::Options { repo, user_only, adapter, place, home, uninstall: false, stage };
    let plan = sync::plan(&stacks, &opts)?;
    if json {
        println!("{}", serde_json::to_string_pretty(&plan).unwrap_or_default());
    } else {
        print_plan(&plan);
    }
    // A place skipped because its file would be cut never reads as success, planned or applied.
    let whole = !plan.over_limit;
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
    // One worktree (its post-checkout hook): only its files, no import or retiring, which commit to the stack.
    if opts.place.is_some() {
        print_applied(&sync::apply(&plan)?);
        return Ok(whole);
    }
    print_applied(&sync::import_and_apply(&p.public, p.private.as_deref(), &opts)?);
    Ok(whole)
}

/// `lords uninstall`: removes every tool-owned path in scope that still matches its ledger record.
fn cmd_uninstall(mut a: Args) -> Result<bool, String> {
    let home = tool_home(&mut a)?;
    let apply = a.flag("--apply");
    let user_only = a.flag("--user");
    let repo = a.value("--repo")?;
    let p = paths(&mut a)?;
    a.done()?;
    let stacks = Stacks::load(&p.public, p.private.as_deref())?;
    let opts = sync::Options { repo, user_only, home, uninstall: true, ..sync::Options::default() };
    let plan = sync::plan(&stacks, &opts)?;
    print_plan(&plan);
    if apply {
        print_applied(&sync::apply(&plan)?);
    } else {
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
        &Retire { id: &id, reason: &reason, superseded_by: superseded_by.as_deref(), date, commit },
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
        println!("installed {} (repo {name})", r.hook.display());
        if let Some(b) = r.backup {
            println!("the hook that was there runs first, from {}", b.display());
        }
        if let Some(g) = r.global_hooks_path {
            println!("note: global core.hooksPath = {g}; its dispatcher must run .git/hooks/post-checkout for this to take effect");
        }
        return Ok(true);
    }
    a.done()?;
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let installed = hooks::install(&repo, &exe, secrets_only, force, &sync::default_home()?)?;
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
    let composed = match (&place, &role) {
        (Some(place), Some(role)) => {
            sync::role_composed(&stacks, sync::place_layers(&stacks, place, true), role)
        }
        (Some(place), None) if !json => {
            let dir = sync::first_path(&stacks, place)
                .ok_or_else(|| format!("no checkout or dir declared for `{place}`"))?;
            let layers = sync::place_layers(&stacks, place, false);
            let (text, _) = sync::place_text(&stacks, layers, adapter, &dir);
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
        "compose" => cmd_compose(a),
        "uninstall" => cmd_uninstall(a),
        "adopt" => cmd_adopt(a),
        "set-aside" => cmd_set_aside(a),
        "sizes" => cmd_sizes(a),
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
