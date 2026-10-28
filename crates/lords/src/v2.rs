//! v2's commands (design §5, "CLI fallback retained in the release"): they print what `lords_core::serve` returns,
//! as the MCP server does, so both give the same ids and bodies. `--registry DIR` names the registry home, else
//! `$LORDS_HOME`, else `~/.lords`.

use crate::Args;
use lords_core::identity::{self, Registry};
use lords_core::serve::{self, Note, Start};
use lords_core::{check, migrate, privacy::Rules, Stack, Stacks};
use serde::Serialize;
use std::path::{Path, PathBuf};

pub const USAGE: &str = "\
v2 (one core and the project store selected for the working dir):
  lords register --core --public DIR [--private DIR]   the core: the public stack and the private overlay's global layer
  lords register --store DIR                           a project store and its repos' and workspaces' bindings
  lords brief --role ROLE [--cwd DIR] [--harness H] [--profile P] [--session S] [--agent A] [--project HINT] [--json]
  lords context --role ROLE --delivery ID [--budget BYTES] [--as-of YYYY-MM-DD] [--json]
  lords search [QUERY] --delivery ID [--id ID]... [--include-expired] [--as-of YYYY-MM-DD] [--json]
  lords remember --delivery ID --text TEXT [--title T] [--kind memory|fact] [--until D] [--review-by D] [--reason R]
  lords check --store DIR [--strict] [--json]          the core's global layers and that store alone
  lords store migrate --from STACK --project ID --to DIR [--json]   copy one project out of an old stack, with a manifest
  (each: --registry DIR, else $LORDS_HOME, else ~/.lords)";

fn home(a: &mut Args) -> Result<PathBuf, String> {
    match a.value("--registry")? {
        Some(h) => Ok(PathBuf::from(h)),
        None => identity::default_home()
            .ok_or_else(|| "no home dir for the registry: pass --registry".into()),
    }
}

fn abs(p: String) -> Result<PathBuf, String> {
    std::path::absolute(&p).map_err(|e| format!("{p}: {e}"))
}

fn as_of(a: &mut Args) -> Result<String, String> {
    Ok(a.value("--as-of")?.unwrap_or_else(lords_core::ops::today))
}

fn print_json(v: &impl Serialize) -> Result<(), String> {
    println!("{}", serde_json::to_string_pretty(v).map_err(|e| e.to_string())?);
    Ok(())
}

pub fn cmd_register(mut a: Args) -> Result<bool, String> {
    let home = home(&mut a)?;
    if a.flag("--core") {
        let public = a.value("--public")?.ok_or("--core needs --public DIR")?;
        let private = a.value("--private")?.map(abs).transpose()?;
        a.done()?;
        identity::set_core(&home, &abs(public)?, private.as_deref())?;
        println!("core registered in {}", home.display());
        return Ok(true);
    }
    let store = a.value("--store")?.ok_or("register needs --core or --store DIR")?;
    a.done()?;
    let r = identity::register_store(&home, &abs(store)?)?;
    println!("{r:#?}");
    Ok(true)
}

pub fn cmd_brief(mut a: Args) -> Result<bool, String> {
    let home = home(&mut a)?;
    let json = a.flag("--json");
    let cwd = match a.value("--cwd")? {
        Some(c) => abs(c)?,
        None => std::env::current_dir().map_err(|e| e.to_string())?,
    };
    let start = Start {
        cwd,
        role: a.value("--role")?.ok_or("brief needs --role ROLE")?,
        harness: a.value("--harness")?.unwrap_or_default(),
        profile: a.value("--profile")?.unwrap_or_default(),
        session: a.value("--session")?.unwrap_or_default(),
        agent: a.value("--agent")?.unwrap_or_else(|| "root".into()),
        hint: a.value("--project")?,
    };
    a.done()?;
    let b = serve::brief(&home, &start)?;
    if json {
        print_json(&b)?;
    } else {
        println!("{}", b.text);
    }
    Ok(true)
}

pub fn cmd_context(mut a: Args) -> Result<bool, String> {
    let home = home(&mut a)?;
    let json = a.flag("--json");
    let role = a.value("--role")?.ok_or("context needs --role ROLE")?;
    let id = a.value("--delivery")?.ok_or("context needs --delivery ID (from `lords brief`)")?;
    let budget = a
        .value("--budget")?
        .map(|b| b.parse::<usize>().map_err(|e| format!("--budget: {e}")))
        .transpose()?;
    let as_of = as_of(&mut a)?;
    a.done()?;
    let c = serve::context(&home, &id, &role, budget, &as_of)?;
    if json {
        print_json(&c)?;
        return Ok(true);
    }
    println!(
        "# Lords context: {}, role {} (delivery {})\n",
        c.project.as_deref().unwrap_or("no project"),
        c.role,
        c.delivery_id
    );
    for d in &c.duties {
        let label = d.label.as_ref().map(|l| format!(" ({l})")).unwrap_or_default();
        println!(
            "## {} `{}`{label}\n\n{}\n",
            d.title,
            d.id,
            d.body.as_deref().unwrap_or("").trim()
        );
    }
    if !c.omitted.is_empty() {
        println!(
            "Left out for the budget (fetch with `lords search --id`): {}\n",
            c.omitted.join(", ")
        );
    }
    println!("## Index (fetch with `lords search --id ID`)\n");
    for i in &c.index {
        let label = i.label.as_ref().map(|l| format!(" ({l})")).unwrap_or_default();
        println!("- `{}` {}{label}", i.id, i.title);
    }
    if let Some(v) = &c.valid_until {
        println!("\nValid until {v}: fetch again before acting past it.");
    }
    for p in &c.problems {
        eprintln!("problem: {p}");
    }
    Ok(true)
}

pub fn cmd_search(mut a: Args) -> Result<bool, String> {
    let home = home(&mut a)?;
    let json = a.flag("--json");
    let include_expired = a.flag("--include-expired");
    let id = a.value("--delivery")?.ok_or("search needs --delivery ID")?;
    let mut ids = Vec::new();
    while let Some(i) = a.value("--id")? {
        ids.push(i);
    }
    let as_of = as_of(&mut a)?;
    let mut words = Vec::new();
    while let Some(w) = a.positional() {
        words.push(w);
    }
    a.done()?;
    let found = serve::search(&home, &id, &words.join(" "), &ids, include_expired, &as_of)?;
    if json {
        print_json(&found)?;
        return Ok(true);
    }
    for i in &found {
        let label = i.label.as_ref().map(|l| format!(" ({l})")).unwrap_or_default();
        println!(
            "## {} `{}`{label}\n\n{}\n",
            i.title,
            i.id,
            i.body.as_deref().unwrap_or("").trim()
        );
    }
    Ok(true)
}

pub fn cmd_remember(mut a: Args) -> Result<bool, String> {
    let home = home(&mut a)?;
    let id = a.value("--delivery")?.ok_or("remember needs --delivery ID")?;
    let note = Note {
        text: a.value("--text")?.ok_or("remember needs --text TEXT")?,
        title: a.value("--title")?,
        kind: a.value("--kind")?,
        until: a.value("--until")?,
        review_by: a.value("--review-by")?,
        reason: a.value("--reason")?,
    };
    let as_of = as_of(&mut a)?;
    a.done()?;
    print_json(&serve::remember(&home, &id, &note, &as_of)?)?;
    Ok(true)
}

pub fn cmd_check_store(mut a: Args, store: &Path) -> Result<bool, String> {
    let home = home(&mut a)?;
    let strict = a.flag("--strict");
    let json = a.flag("--json");
    a.done()?;
    let reg = Registry::load(&home)?;
    let core = reg.core.as_ref().ok_or("no core registered: run `lords register --core` first")?;
    let stacks =
        Stacks::selected(&core.public, core.private.as_deref(), Some(Stack::check_store(store)?))?;
    let rules =
        Rules::public().with_allow_from(&core.public).with_private(stacks.private.as_ref())?;
    let findings = check::check_stack(&stacks, &rules);
    crate::print_findings(&findings, json);
    Ok(!check::has_errors(&findings, strict))
}

pub fn cmd_store(mut a: Args) -> Result<bool, String> {
    match a.positional().as_deref() {
        Some("migrate") => {}
        other => {
            return Err(format!(
                "`lords store {}`: the one store command is `migrate`",
                other.unwrap_or("")
            ))
        }
    }
    let json = a.flag("--json");
    let from = a.value("--from")?.ok_or("migrate needs --from STACK")?;
    let project = a.value("--project")?.ok_or("migrate needs --project ID")?;
    let to = a.value("--to")?.ok_or("migrate needs --to DIR")?;
    a.done()?;
    let m = migrate::migrate(&abs(from)?, &project, &abs(to)?)?;
    if json {
        print_json(&m)?;
    } else {
        println!(
            "{} entries ({} files) of {} copied to {}; manifest {}",
            m.ids.len(),
            m.files.len(),
            m.project,
            m.store.display(),
            migrate::RECORD
        );
        for p in &m.problems {
            println!("problem: {p}");
        }
    }
    Ok(m.problems.is_empty())
}
