//! `sync` (design: Sync): a plan of what each target would become, as diffs; `apply` carries a plan out after
//! backing up everything it replaces. Planning only reads.
//!
//! Harness-neutral: each target names an adapter (adapters.rs), which says which agents it reaches and which
//! untracked file it reads in a repo. Those facts are checked in docs/load-order.md.

use crate::adapters::{self, Adapter};
use crate::compose::{self, Composed, Context, Layers, Source};
use crate::config::expand_home;
use crate::diff;
use crate::git;
use crate::model::{Audience, Entry, Kind};
use crate::stack::{self, Stack, Stacks, Target};
use serde::Serialize;
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Action {
    Create,
    Update,
    InSync,
    /// Make a dir link to the stack's skill.
    Link,
    /// A link to an old place in a stack: point it at the current one.
    Relink,
    /// A real dir where the link goes: back it up, then link.
    ReplaceDir,
    /// A link into a stack for a skill that no longer applies.
    Unlink,
    /// Shown, never written (a tracked file: it changes by a commit, not by sync).
    Report,
    /// Can't proceed; the reason is in the notes.
    Conflict,
}

#[derive(Clone, Debug, Serialize)]
pub struct Item {
    pub target: String,
    pub path: PathBuf,
    pub action: Action,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub current: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub proposed: Option<String>,
    /// For `Link`, `Relink` and `ReplaceDir`: the stack dir the link points at.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub link_to: Option<PathBuf>,
    pub diff: String,
    pub notes: Vec<String>,
    #[serde(skip)]
    pub sources: Vec<Source>,
}

impl Item {
    fn new(target: &str, path: PathBuf, action: Action) -> Item {
        Item {
            target: target.to_string(),
            path,
            action,
            current: None,
            proposed: None,
            link_to: None,
            diff: String::new(),
            notes: Vec::new(),
            sources: Vec::new(),
        }
    }

    pub fn changes(&self) -> bool {
        !matches!(self.action, Action::InSync | Action::Report)
    }
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct Plan {
    pub items: Vec<Item>,
    pub notes: Vec<String>,
}

#[derive(Clone, Debug, Default)]
pub struct Options {
    /// Only this repo's files (and no user-level targets). `None`: user-level targets and every repo.
    pub repo: Option<String>,
    /// Only user-level targets.
    pub user_only: bool,
}

fn audience_of(names: &[String], adapter: &dyn Adapter) -> Vec<Audience> {
    let parsed: Vec<Audience> = names
        .iter()
        .filter_map(|n| serde_json::from_value(serde_json::Value::String(n.clone())).ok())
        .collect();
    if parsed.is_empty() {
        adapter.audience()
    } else {
        parsed
    }
}

fn read(path: &Path) -> Option<String> {
    fs::read_to_string(path).ok()
}

fn display(p: &Path) -> String {
    p.display().to_string()
}

/// An item that makes `path` hold `proposed` (or says it already does).
fn file_item(target: &str, path: &Path, proposed: String, composed: &Composed) -> Item {
    let current = read(path);
    let action = match &current {
        None if proposed.is_empty() => Action::InSync,
        None => Action::Create,
        Some(c) if *c == proposed => Action::InSync,
        Some(_) => Action::Update,
    };
    let mut item = Item::new(target, path.to_path_buf(), action);
    item.diff = diff::unified(
        current.as_deref().unwrap_or(""),
        &proposed,
        &display(path),
        &display(path),
        3,
    );
    item.sources = composed.sources.clone();
    if !composed.overridden.is_empty() {
        item.notes.push(format!("overridden here: {}", composed.overridden.join(", ")));
    }
    item.current = current;
    item.proposed = Some(proposed);
    item
}

/// Memory stores the adapter's harness loads by itself (stack.toml `native`).
fn native_stores(stacks: &Stacks, adapter: &dyn Adapter) -> Vec<String> {
    stacks
        .private
        .iter()
        .flat_map(|p| p.manifest.memory_stores.iter())
        .filter(|m| m.native.iter().any(|n| n == adapter.id()))
        .map(|m| m.name.clone())
        .collect()
}

fn context(
    stacks: &Stacks,
    adapter: &dyn Adapter,
    audience: Vec<Audience>,
    layers: Layers,
) -> Context {
    Context { audience, layers, public_only: false, skip_stores: native_stores(stacks, adapter) }
}

fn size_note(item: &mut Item, adapter: &dyn Adapter, text: &str) {
    if let Some(limit) = adapter.byte_limit() {
        if text.len() > limit / 2 {
            item.notes.push(format!(
                "{} bytes: {} reads at most {limit} bytes of instruction files in total, repo files included",
                text.len(),
                adapter.id()
            ));
        }
    }
}

fn instructions_item(stacks: &Stacks, t: &Target, adapter: &dyn Adapter) -> Item {
    let path = expand_home(&t.path);
    let ctx = context(stacks, adapter, audience_of(&t.audience, adapter), Layers::Global);
    let composed = compose::compose(stacks, &ctx);
    let replace = t.own_text.as_deref() == Some("replace");
    let current = read(&path).unwrap_or_default();
    let text = adapter.render(&composed);
    let proposed = compose::place_block(&current, &text, replace);
    let mut item = file_item(&t.name, &path, proposed, &composed);
    size_note(&mut item, adapter, &text);
    if replace && !current.is_empty() && compose::current_block(&current).is_none() {
        item.notes.push(
            "own_text = replace: the text now in the file is replaced (it is backed up)".into(),
        );
    }
    item
}

/// The dir a link points at, if `path` is a link (a junction on Windows).
fn link_target(path: &Path) -> Option<PathBuf> {
    fs::read_link(path).ok().map(|t| {
        if t.is_absolute() {
            t
        } else {
            path.parent().unwrap_or(Path::new(".")).join(t)
        }
    })
}

fn same_place(a: &Path, b: &Path) -> bool {
    match (fs::canonicalize(a), fs::canonicalize(b)) {
        (Ok(x), Ok(y)) => x == y,
        _ => strip_verbatim(a) == strip_verbatim(b),
    }
}

fn strip_verbatim(p: &Path) -> String {
    let s = p.to_string_lossy().replace('\\', "/");
    s.trim_start_matches("//?/").trim_start_matches("/??/").to_ascii_lowercase()
}

fn in_a_stack(stacks: &Stacks, p: &Path) -> bool {
    let p = strip_verbatim(p);
    stacks.all().any(|s| {
        let root = fs::canonicalize(&s.root).unwrap_or(s.root.clone());
        p.starts_with(&strip_verbatim(&root)) || p.starts_with(&strip_verbatim(&s.root))
    })
}

fn skills_items(stacks: &Stacks, t: &Target, adapter: &dyn Adapter) -> Vec<Item> {
    let dir = expand_home(&t.path);
    let ctx = context(stacks, adapter, audience_of(&t.audience, adapter), Layers::Global);
    let wanted: Vec<&Entry> = compose::applicable(stacks, &ctx)
        .into_iter()
        .filter(|e| e.meta.kind == Kind::Skill)
        .collect();
    let mut items = Vec::new();
    for e in &wanted {
        let name = e.id().trim_start_matches("skill.").to_string();
        let src = e.meta_file.parent().expect("a sidecar has a dir").to_path_buf();
        let link = dir.join(&name);
        let mut item = Item::new(&t.name, link.clone(), Action::Link);
        item.link_to = Some(src.clone());
        if let Some(to) = link_target(&link) {
            item.action = if same_place(&to, &src) {
                Action::InSync
            } else if in_a_stack(stacks, &to) {
                Action::Relink
            } else {
                item.notes.push(format!("a link to {} that isn't ours", to.display()));
                Action::Conflict
            };
        } else if link.is_dir() {
            item.action = Action::ReplaceDir;
            let old = read(&link.join(stack::SKILL_FILE)).unwrap_or_default();
            let new = read(&src.join(stack::SKILL_FILE)).unwrap_or_default();
            item.diff = diff::unified(
                &old,
                &new,
                &display(&link.join(stack::SKILL_FILE)),
                &display(&src.join(stack::SKILL_FILE)),
                3,
            );
            let extra: Vec<String> = stack::all_files(&link)
                .iter()
                .filter_map(|f| {
                    f.strip_prefix(&link).ok().map(|r| r.to_string_lossy().replace('\\', "/"))
                })
                .filter(|r| !src.join(r).exists())
                .collect();
            if !extra.is_empty() {
                item.notes.push(format!(
                    "files only in the installed copy (kept in the backup): {}",
                    extra.join(", ")
                ));
            }
        } else if link.exists() {
            item.action = Action::Conflict;
            item.notes.push("a file where the skill's link goes".into());
        }
        items.push(item);
    }
    let wanted_names: Vec<String> =
        wanted.iter().map(|e| e.id().trim_start_matches("skill.").to_string()).collect();
    let mut unmanaged = Vec::new();
    for child in stack::sorted_children(&dir) {
        let name = stack::file_name(&child);
        if wanted_names.contains(&name) {
            continue;
        }
        match link_target(&child) {
            Some(to) if in_a_stack(stacks, &to) => {
                items.push(Item::new(&t.name, child.clone(), Action::Unlink))
            }
            _ => unmanaged.push(name),
        }
    }
    if !unmanaged.is_empty() {
        let mut item = Item::new(&t.name, dir.clone(), Action::InSync);
        item.notes.push(format!("not from a stack, left alone: {}", unmanaged.join(", ")));
        items.push(item);
    }
    items
}

/// Every checkout and (if asked) worktree of a repo.
fn checkouts(repo: &stack::Repo) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = Vec::new();
    for c in &repo.checkouts {
        let c = expand_home(c);
        out.push(c.clone());
        if repo.worktrees {
            if let Ok(list) = git::run(&c, &["worktree", "list", "--porcelain"]) {
                out.extend(
                    list.lines().filter_map(|l| l.strip_prefix("worktree ")).map(PathBuf::from),
                );
            }
        }
    }
    let mut seen: Vec<String> = Vec::new();
    out.retain(|p| {
        let k = strip_verbatim(p);
        let new = !seen.contains(&k);
        seen.push(k);
        new
    });
    out
}

fn repo_items(stacks: &Stacks, name: &str, repo: &stack::Repo, plan: &mut Plan) {
    let project = stacks.repo_project(name);
    let mut chosen: Vec<&dyn Adapter> = Vec::new();
    if repo.adapters.is_empty() {
        chosen.extend(adapters::ALL.iter().copied());
    }
    for id in &repo.adapters {
        match adapters::get(id) {
            Some(a) => chosen.push(a),
            None => plan.notes.push(format!(
                "repo {name}: unknown adapter `{id}` (known: {})",
                adapters::ids().join(", ")
            )),
        }
    }
    let paths = checkouts(repo);
    if paths.is_empty() {
        plan.notes.push(format!("repo {name}: no checkouts declared"));
        return;
    }
    let path_entries = stacks
        .entries()
        .filter(|e| {
            e.active() && matches!(&e.scope, crate::model::Scope::Path { repo: r, .. } if r == name)
        })
        .count();
    if path_entries > 0 {
        plan.notes
            .push(format!("repo {name}: {path_entries} path-scoped entries are not written yet"));
    }
    let mut excludes: Vec<(PathBuf, Vec<&str>)> = Vec::new();
    for adapter in chosen {
        let file = adapter.repo_file();
        let layers =
            Layers::Repo { repo: name.into(), project: project.clone(), sub: None, global: false };
        let ctx = context(stacks, adapter, adapter.audience(), layers);
        let composed = compose::compose(stacks, &ctx);
        let text = adapter.render(&composed);
        for p in &paths {
            let target = format!("repo {name} ({file})");
            let path = p.join(file);
            if !p.is_dir() {
                let mut item = Item::new(&target, path, Action::Conflict);
                item.notes.push("checkout not found".into());
                plan.items.push(item);
                continue;
            }
            if git::run(p, &["ls-files", "--error-unmatch", file]).is_ok() {
                let mut item = Item::new(&target, path, Action::Conflict);
                item.notes
                    .push(format!("{file} is tracked here; sync writes only untracked files"));
                plan.items.push(item);
                continue;
            }
            let current = read(&path).unwrap_or_default();
            let proposed = compose::place_block(&current, &text, false);
            let mut item = file_item(&target, &path, proposed, &composed);
            size_note(&mut item, adapter, &text);
            plan.items.push(item);
            if let Ok(common) =
                git::run(p, &["rev-parse", "--path-format=absolute", "--git-common-dir"])
            {
                let ex = PathBuf::from(common.trim()).join("info").join("exclude");
                match excludes.iter_mut().find(|(e, _)| *e == ex) {
                    Some((_, files)) if !files.contains(&file) => files.push(file),
                    Some(_) => {}
                    None => excludes.push((ex, vec![file])),
                }
            }
        }
    }
    for (ex, files) in excludes {
        let current = read(&ex).unwrap_or_default();
        let have: Vec<&str> = current.lines().map(str::trim).collect();
        let missing: Vec<&str> = files.into_iter().filter(|f| !have.contains(f)).collect();
        let mut proposed = current.clone();
        if !missing.is_empty() {
            if !proposed.is_empty() && !proposed.ends_with('\n') {
                proposed.push('\n');
            }
            proposed.push_str("# Lords Command: agent files written by sync\n");
            for f in &missing {
                proposed.push_str(f);
                proposed.push('\n');
            }
        }
        plan.items.push(file_item(
            &format!("repo {name} (git exclude)"),
            &ex,
            proposed,
            &Composed::default(),
        ));
    }
    // The tracked AGENTS.md of the first checkout: what the public layers would make it. Shown, never written.
    let first = &paths[0];
    let tracked = first.join("AGENTS.md");
    if let Some(current) = read(&tracked) {
        let ctx = Context {
            audience: vec![Audience::Opus, Audience::Sol, Audience::Astra],
            layers: Layers::Repo {
                repo: name.into(),
                project: project.clone(),
                sub: None,
                global: true,
            },
            public_only: true,
            skip_stores: Vec::new(),
        };
        let composed = compose::compose(stacks, &ctx);
        let proposed = compose::place_block(&current, &composed.text, false);
        let mut item =
            file_item(&format!("repo {name} (tracked AGENTS.md)"), &tracked, proposed, &composed);
        item.action = if item.current == item.proposed { Action::InSync } else { Action::Report };
        item.notes.push("tracked: changes by a commit on the repo's branch, never by sync; agents with a stack read the untracked files instead".into());
        plan.items.push(item);
    }
}

pub fn plan(stacks: &Stacks, opts: &Options) -> Result<Plan, String> {
    let private: &Stack = stacks
        .private
        .as_ref()
        .ok_or("targets are declared in the private stack.toml; no private stack loaded")?;
    let mut plan = Plan::default();
    // The memory index points at the stack's copies: say what an import would bring first.
    if !private.manifest.memory_stores.is_empty() {
        match crate::ops::import_preview(private, None) {
            Ok(reports) => {
                for r in reports.iter().filter(|r| !r.added.is_empty() || !r.changed.is_empty()) {
                    plan.notes.push(format!(
                        "memory store {}: {} new, {} changed since the last import; --apply imports them (one commit) before writing",
                        r.store,
                        r.added.len(),
                        r.changed.len()
                    ));
                }
            }
            Err(e) => plan.notes.push(format!("memory stores: {e}")),
        }
    }
    if opts.repo.is_none() {
        for t in &private.manifest.targets {
            let adapter = adapters::get(&t.adapter).ok_or_else(|| {
                format!(
                    "target {}: unknown adapter `{}` (known: {})",
                    t.name,
                    t.adapter,
                    adapters::ids().join(", ")
                )
            })?;
            match t.kind.as_str() {
                "instructions" => plan.items.push(instructions_item(stacks, t, adapter)),
                "skills" => plan.items.extend(skills_items(stacks, t, adapter)),
                k => return Err(format!("target {}: unknown kind {k}", t.name)),
            }
        }
    }
    if !opts.user_only {
        for (name, repo) in &private.manifest.repos {
            if opts.repo.as_deref().is_some_and(|r| r != name) {
                continue;
            }
            repo_items(stacks, name, repo, &mut plan);
        }
        if let Some(r) = &opts.repo {
            if !private.manifest.repos.contains_key(r) {
                return Err(format!("no repo `{r}` in the private stack.toml"));
            }
        }
    }
    Ok(plan)
}

// ---------------------------------------------------------------- apply

fn backup_name(path: &Path) -> String {
    strip_verbatim(path).replace([':', '/'], "_").trim_matches('_').to_string()
}

fn make_link(link: &Path, to: &Path) -> Result<(), String> {
    #[cfg(windows)]
    {
        // cmd reads a `/` inside a path as a switch, so both paths go in with backslashes only.
        let win = |p: &Path| p.to_string_lossy().replace('/', "\\");
        let out = std::process::Command::new("cmd")
            .args(["/C", "mklink", "/J"])
            .arg(win(link))
            .arg(win(to))
            .output()
            .map_err(|e| e.to_string())?;
        if !out.status.success() {
            return Err(format!(
                "mklink /J {}: {}",
                link.display(),
                String::from_utf8_lossy(&out.stderr).trim()
            ));
        }
        Ok(())
    }
    #[cfg(not(windows))]
    {
        std::os::unix::fs::symlink(to, link).map_err(|e| e.to_string())
    }
}

fn remove_link(link: &Path) -> Result<(), String> {
    // A junction is removed as a dir, a symlink as a file; neither touches what it points at.
    fs::remove_dir(link)
        .or_else(|_| fs::remove_file(link))
        .map_err(|e| format!("{}: {e}", link.display()))
}

/// Carries out every changing item of a plan. Each file or dir it replaces is first copied or moved under
/// `backup_dir`. Returns one line per change.
pub fn apply(plan: &Plan, backup_dir: &Path) -> Result<Vec<String>, String> {
    let mut done = Vec::new();
    if plan.items.iter().any(|i| i.action == Action::Conflict) {
        return Err("the plan has conflicts; resolve them first (nothing was changed)".into());
    }
    for item in plan.items.iter().filter(|i| i.changes()) {
        let backup = backup_dir.join(backup_name(&item.path));
        match item.action {
            Action::Create | Action::Update => {
                if let Some(cur) = &item.current {
                    fs::create_dir_all(backup_dir).map_err(|e| e.to_string())?;
                    fs::write(&backup, cur).map_err(|e| e.to_string())?;
                }
                if let Some(parent) = item.path.parent() {
                    fs::create_dir_all(parent).map_err(|e| e.to_string())?;
                }
                fs::write(&item.path, item.proposed.as_deref().unwrap_or(""))
                    .map_err(|e| format!("{}: {e}", item.path.display()))?;
            }
            Action::Link | Action::Relink | Action::ReplaceDir => {
                let to = item.link_to.as_ref().expect("a link item has a target");
                if item.action == Action::Relink {
                    remove_link(&item.path)?;
                }
                if item.action == Action::ReplaceDir {
                    fs::create_dir_all(backup_dir).map_err(|e| e.to_string())?;
                    fs::rename(&item.path, &backup)
                        .map_err(|e| format!("{}: {e}", item.path.display()))?;
                }
                if let Some(parent) = item.path.parent() {
                    fs::create_dir_all(parent).map_err(|e| e.to_string())?;
                }
                make_link(&item.path, to)?;
            }
            Action::Unlink => remove_link(&item.path)?,
            Action::InSync | Action::Report | Action::Conflict => continue,
        }
        done.push(format!("{:?} {}", item.action, item.path.display()));
    }
    Ok(done)
}
