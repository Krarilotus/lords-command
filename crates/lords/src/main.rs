//! `lords`: the Lords Command CLI. Small on purpose: the pre-commit hook runs it.

use lords_core::check;
use lords_core::config::Config;
use lords_core::model::Visibility;
use lords_core::ops::{self, Promote, Retire};
use lords_core::privacy::{self, Rules};
use lords_core::{git, hooks, Finding, Severity, Stack, Stacks};
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
  lords privacy FILE...
  lords hooks install [--repo DIR] [--secrets-only] [--force]
  lords hash PATH

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
    Ok(Paths { public, private })
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
    let p = paths(&mut a)?;
    a.done()?;
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
    let findings = check::check(&stacks, &rules);
    print_findings(&findings, json);
    if !json {
        let n = |v| stacks.stack(v).map(|s| s.entries.len()).unwrap_or(0);
        println!("{} public and {} private entries", n(Visibility::Public), n(Visibility::Private));
    }
    Ok(!check::has_errors(&findings, strict))
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
    let secrets_only = a.flag("--secrets-only");
    let cwd = std::env::current_dir().map_err(|e| e.to_string())?;
    let rules = if secrets_only {
        Rules::secrets_only()
    } else {
        let cfg = Config::discover();
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
