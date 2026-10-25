//! `sync` (design: Sync): a plan of what each target would become, as diffs; `apply` carries a plan out after
//! backing up everything it replaces. Planning only reads.
//!
//! What each family loads (checked against the docs and live, 25 Oct 2026, docs/load-order.md):
//! - Claude Code: the user file, then every `CLAUDE.md`, `.claude/CLAUDE.md` and `CLAUDE.local.md` from the
//!   filesystem root down to the cwd. `AGENTS.md` is read only when none of those three exists in the cwd or
//!   above. So a repo gets an untracked `CLAUDE.local.md`.
//! - Codex: `$CODEX_HOME/AGENTS.md`, then per dir from the git root to the cwd `AGENTS.override.md`, else
//!   `AGENTS.md`. So a repo gets an untracked `AGENTS.override.md`, which replaces the tracked file.

use crate::compose::{self, Composed, Context, Family, Layers, Source};
use crate::config::expand_home;
use crate::diff;
use crate::git;
use crate::model::{Audience, Entry, Kind};
use crate::stack::{self, Stack, Stacks, Target};
use serde::Serialize;
use std::fs;
use std::path::{Path, PathBuf};

/// Codex stops reading instruction files past this many bytes (`project_doc_max_bytes`, default 32 KiB).
pub const CODEX_LIMIT: usize = 32 * 1024;

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

fn audience_of(names: &[String], family: Family) -> Vec<Audience> {
    let parsed: Vec<Audience> = names
        .iter()
        .filter_map(|n| serde_json::from_value(serde_json::Value::String(n.clone())).ok())
        .collect();
    if parsed.is_empty() {
        family.default_audience()
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

fn instructions_item(stacks: &Stacks, t: &Target, family: Family) -> Item {
    let path = expand_home(&t.path);
    let ctx = Context {
        audience: audience_of(&t.audience, family),
        layers: Layers::Global,
        public_only: false,
    };
    let composed = compose::compose(stacks, &ctx);
    let replace = t.own_text.as_deref() == Some("replace");
    let current = read(&path).unwrap_or_default();
    let proposed = compose::place_block(&current, &composed.text, replace);
    let mut item = file_item(&t.name, &path, proposed, &composed);
    if family == Family::Codex && composed.text.len() > CODEX_LIMIT / 2 {
        item.notes.push(format!(
            "{} bytes: Codex reads at most {CODEX_LIMIT} bytes of instruction files in total, repo files included",
            composed.text.len()
        ));
    }
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

fn skills_items(stacks: &Stacks, t: &Target, family: Family) -> Vec<Item> {
    let dir = expand_home(&t.path);
    let ctx = Context {
        audience: audience_of(&t.audience, family),
        layers: Layers::Global,
        public_only: false,
    };
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

const UNTRACKED: [(Family, &str); 2] =
    [(Family::Claude, "CLAUDE.local.md"), (Family::Codex, "AGENTS.override.md")];

fn repo_items(stacks: &Stacks, name: &str, repo: &stack::Repo, plan: &mut Plan) {
    let project = stacks.repo_project(name);
    let families: Vec<Family> = if repo.families.is_empty() {
        vec![Family::Claude, Family::Codex]
    } else {
        repo.families.iter().filter_map(|f| Family::parse(f)).collect()
    };
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
    for family in &families {
        let file = UNTRACKED
            .iter()
            .find(|(f, _)| f == family)
            .map(|(_, n)| *n)
            .expect("each family has a file");
        let ctx = Context {
            audience: family.default_audience(),
            layers: Layers::Repo {
                repo: name.into(),
                project: project.clone(),
                sub: None,
                global: false,
            },
            public_only: false,
        };
        let composed = compose::compose(stacks, &ctx);
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
            let proposed = compose::place_block(&current, &composed.text, false);
            plan.items.push(file_item(&target, &path, proposed, &composed));
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
    if opts.repo.is_none() {
        for t in &private.manifest.targets {
            let family = Family::parse(&t.family)
                .ok_or_else(|| format!("target {}: unknown family {}", t.name, t.family))?;
            match t.kind.as_str() {
                "instructions" => plan.items.push(instructions_item(stacks, t, family)),
                "skills" => plan.items.extend(skills_items(stacks, t, family)),
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
