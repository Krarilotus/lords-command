//! `lords`: the Lords Command CLI. Small on purpose: the pre-commit hook runs it.

use lords_core::check;
use lords_core::config::Config;
use lords_core::model::Visibility;
use lords_core::ops::{self, Promote, Retire};
use lords_core::privacy::{self, Rules};
use lords_core::{git, hooks, sync, Finding, Severity, Stack, Stacks};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

const USAGE: &str = "\
lords: one home for how you work with agents

usage:
  lords check [--public] [--strict] [--json]
  lords import [--store NAME] [--no-commit]
  lords retire ID --reason TEXT [--superseded-by ID] [--date YYYY-MM-DD] [--no-commit]
  lords promote ID [--strip | --split] [--no-commit]
  lords privacy --staged [--secrets-only]
  lords privacy --history [--private-stack DIR]   every commit's message and patch (the pre-push check)
  lords privacy FILE...
  lords hooks install [--repo DIR] [--secrets-only] [--force]
  lords hash PATH
  lords sync [--user | --repo NAME] [--adapter ID] [--json] [--apply]
  lords compose [--repo NAME [--role ROLE]] [--adapter ID] [--json]   what a harness reads there, with sources
  lords sizes [--json]   every file sync writes, by section; fails past the warning line

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
    let cfg = Config::discover();
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

fn public_rules(public: &Path, private: Option<&Stack>) -> Rules {
    Rules::public().with_allow_from(public).with_private(private)
}

fn cmd_check(mut a: Args) -> Result<bool, String> {
    let public_only = a.flag("--public");
    let strict = a.flag("--strict");
    let json = a.flag("--json");
    let explicit = a.words.iter().any(|w| w == "--public-stack");
    let mut p = paths(&mut a)?;
    a.done()?;
    // `--public` checks the repo it runs in (the pre-push check), not whatever config.txt points at.
    if public_only && !explicit {
        let here =
            std::env::current_dir().ok().and_then(|d| git::toplevel(&d)).map(|t| t.join("stack"));
        if let Some(stack) = here.filter(|s| s.join("stack.toml").is_file()) {
            p.public = stack;
        }
    }
    let private = p.private.as_deref().map(Stack::load).transpose()?;
    let rules = public_rules(&p.public, private.as_ref());
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
    let stacks = Stacks { public: Stack::load(&p.public)?, private };
    let mut findings = check::check(&stacks, &rules);
    findings.extend(drift(&stacks));
    print_findings(&findings, json);
    if !json {
        let n = |v| stacks.stack(v).map(|s| s.entries.len()).unwrap_or(0);
        println!("{} public and {} private entries", n(Visibility::Public), n(Visibility::Private));
    }
    Ok(!check::has_errors(&findings, strict))
}

fn drift(stacks: &Stacks) -> Vec<Finding> {
    sync::drift_findings(stacks)
}

fn cmd_sync(mut a: Args) -> Result<bool, String> {
    let apply = a.flag("--apply");
    let json = a.flag("--json");
    let user_only = a.flag("--user");
    let repo = a.value("--repo")?;
    let adapter = a.value("--adapter")?;
    let p = paths(&mut a)?;
    a.done()?;
    if let Some(id) = adapter.as_deref().filter(|id| lords_core::adapters::get(id).is_none()) {
        return Err(format!(
            "unknown adapter `{id}` (known: {})",
            lords_core::adapters::ids().join(", ")
        ));
    }
    let stacks = Stacks::load(&p.public, p.private.as_deref())?;
    let opts = sync::Options { repo, user_only, adapter };
    let plan = sync::plan(&stacks, &opts)?;
    if json {
        println!("{}", serde_json::to_string_pretty(&plan).unwrap_or_default());
    } else {
        print_plan(&plan);
    }
    if !apply {
        if !json {
            println!("dry run: nothing written (`--apply` writes, after backing up every target)");
        }
        return Ok(true);
    }
    let backups = sync::backup_dir()?;
    for line in sync::import_and_apply(&p.public, p.private.as_deref(), &opts, &backups)? {
        println!("{line}");
    }
    println!("backups: {}", backups.display());
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
                if let Some(to) = &i.link_to {
                    println!("   -> {}", to.display());
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
    let rules = public_rules(&p.public, stacks.private.as_ref());
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
    let public_stack = a.value("--public-stack")?.map(PathBuf::from);
    let private_stack = a.value("--private-stack")?.map(PathBuf::from);
    let cwd = std::env::current_dir().map_err(|e| e.to_string())?;
    let rules = if secrets_only {
        Rules::secrets_only()
    } else {
        let mut cfg = Config::discover();
        if private_stack.is_some() {
            cfg.private = private_stack;
        }
        if public_stack.is_some() {
            cfg.public = public_stack;
        }
        let private = cfg.private.as_deref().and_then(|p| Stack::load(p).ok());
        let mut rules = Rules::public().with_private(private.as_ref());
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
    let scan = if staged {
        a.done()?;
        let top = git::toplevel(&cwd).ok_or("not in a git repo")?;
        privacy::scan_staged(&top, &rules)?
    } else {
        let mut scan = privacy::Scan::default();
        while let Some(f) = a.positional() {
            let text = std::fs::read_to_string(&f).map_err(|e| format!("{f}: {e}"))?;
            scan.files_scanned += 1;
            scan.hits
                .extend(privacy::scan(&text, &rules).into_iter().map(|h| (PathBuf::from(&f), h)));
        }
        a.done()?;
        scan
    };
    for (p, h) in &scan.hits {
        eprintln!("{}", check::privacy_finding(p, h));
    }
    if !scan.hits.is_empty() {
        eprintln!(
            "lords privacy: {} finding(s) in {} file(s) scanned; nothing committed",
            scan.hits.len(),
            scan.files_scanned
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
    a.done()?;
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let r = hooks::install(&repo, &exe, secrets_only, force)?;
    println!("installed {}", r.hook.display());
    if let Some(b) = r.backup {
        println!("the old hook is at {}", b.display());
    }
    if let Some(g) = r.global_hooks_path {
        println!("note: global core.hooksPath = {g}; its dispatcher must run .git/hooks/pre-commit for this to take effect");
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

/// Every file sync writes for agents, measured as written: bytes by section, and what a session reads in total
/// against the adapter's limit.
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
            Some(l) if s.reads > s.bytes => {
                let mark = if s.over_line() { "  OVER the warning line" } else { "" };
                over |= s.over_line();
                format!(", a session reads {} of {l} ({}%){mark}", s.reads, s.reads * 100 / l)
            }
            _ => String::new(),
        };
        println!("{} {}: {} bytes{reads}", s.place, s.file, s.bytes);
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
