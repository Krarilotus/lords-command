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
    /// A plain copy of a skill the stack made (own or adapted) where it no longer installs (an older install):
    /// backed up, removed. Copies of inherited, third-party skills are the user's own and stay.
    RemoveDir,
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
    Context {
        audience,
        layers,
        public_only: false,
        skip_stores: native_stores(stacks, adapter),
        roles: None,
    }
}

/// A target's role, if it names one.
fn roles_of(t: &Target) -> Option<Vec<String>> {
    (!t.roles.is_empty()).then(|| t.roles.clone())
}

/// Past this share of an adapter's size limit, sync notes it and `check` warns, before the harness cuts text.
pub const SIZE_WARN_PERCENT: usize = 75;

fn over_size(adapter: &dyn Adapter, bytes: usize) -> Option<usize> {
    adapter.byte_limit().filter(|limit| bytes * 100 > limit * SIZE_WARN_PERCENT)
}

fn size_note(item: &mut Item, adapter: &dyn Adapter, text: &str) {
    if let Some(limit) = over_size(adapter, text.len()) {
        item.notes.push(format!(
            "{} bytes: {} reads at most {limit} bytes of instruction files in total, repo files included",
            text.len(),
            adapter.id()
        ));
    }
}

/// An instructions target: the composed text in its file, or (with `via`) in one shared file that the target's
/// file includes, so several profiles read one copy.
fn instructions_items(stacks: &Stacks, t: &Target, adapter: &dyn Adapter) -> Vec<Item> {
    let path = expand_home(&t.path);
    let mut ctx = context(stacks, adapter, audience_of(&t.audience, adapter), Layers::Global);
    ctx.roles = roles_of(t);
    let composed = compose::compose(stacks, &ctx);
    let replace = t.own_text.as_deref() == Some("replace");
    let current = read(&path).unwrap_or_default();
    let text = adapter.render(&composed);
    let mut items = Vec::new();
    let block = match &t.via {
        None => text.clone(),
        Some(via) => {
            let shared = expand_home(via);
            let Some(line) = adapter.import_line(&shared) else {
                let mut item = Item::new(&t.name, path, Action::Conflict);
                item.notes.push(format!(
                    "adapter {} doesn't expand includes, so `via` can't work",
                    adapter.id()
                ));
                return vec![item];
            };
            let mut s =
                file_item(&format!("shared text ({})", t.name), &shared, text.clone(), &composed);
            size_note(&mut s, adapter, &text);
            items.push(s);
            if composed.text.is_empty() {
                String::new()
            } else {
                compose::block(&line)
            }
        }
    };
    let proposed = compose::place_block(&current, &block, replace);
    let mut item = file_item(&t.name, &path, proposed, &composed);
    if t.via.is_none() {
        size_note(&mut item, adapter, &text);
    }
    if replace && !current.is_empty() && compose::current_block(&current).is_none() {
        item.notes.push(
            "own_text = replace: the text now in the file is replaced (it is backed up)".into(),
        );
    }
    items.push(item);
    items
}

fn norm(p: &Path) -> String {
    strip_verbatim(p)
}

/// A settings target: the adapter's exclude list in a JSON settings file holds every path in `exclude`. Only
/// that one key changes; the rest of the file keeps its keys and order.
fn settings_item(t: &Target, adapter: &dyn Adapter) -> Item {
    let path = expand_home(&t.path);
    let Some(key) = adapter.excludes_key() else {
        let mut item = Item::new(&t.name, path, Action::Conflict);
        item.notes.push(format!("adapter {} has no exclude setting", adapter.id()));
        return item;
    };
    let current = read(&path);
    let mut value: serde_json::Value = match current.as_deref().map(serde_json::from_str) {
        None => serde_json::json!({}),
        Some(Ok(v @ serde_json::Value::Object(_))) => v,
        Some(_) => {
            let mut item = Item::new(&t.name, path, Action::Conflict);
            item.notes.push("not a JSON object; left alone".into());
            return item;
        }
    };
    let list =
        value.as_object_mut().expect("an object").entry(key).or_insert(serde_json::json!([]));
    let Some(list) = list.as_array_mut() else {
        let mut item = Item::new(&t.name, path, Action::Conflict);
        item.notes.push(format!("`{key}` isn't a list; left alone"));
        return item;
    };
    let mut added = false;
    for e in &t.exclude {
        let want = adapters::slashes(&expand_home(e));
        if !list.iter().any(|v| v.as_str().is_some_and(|s| s.eq_ignore_ascii_case(&want))) {
            list.push(serde_json::Value::String(want));
            added = true;
        }
    }
    let proposed = match (&current, added) {
        (Some(c), false) => c.clone(),
        _ => serde_json::to_string_pretty(&value).expect("JSON serialises") + "\n",
    };
    file_item(&t.name, &path, proposed, &Composed::default())
}

/// A user file whose harness also loads another file that hides repos' own instructions, unless a settings
/// target in the same profile dir excludes it (load-order.md).
fn shadow_notes(private: &Stack, plan: &mut Plan) {
    for t in private.manifest.targets.iter().filter(|t| t.kind == "instructions") {
        let Some(adapter) = adapters::get(&t.adapter) else { continue };
        let path = expand_home(&t.path);
        let Some(shadow) = adapter.shadowing_file(&path) else { continue };
        if !shadow.exists() {
            continue;
        }
        let covered = private.manifest.targets.iter().any(|s| {
            s.kind == "settings"
                && s.adapter == t.adapter
                && expand_home(&s.path).parent() == path.parent()
                && s.exclude.iter().any(|e| norm(&expand_home(e)) == norm(&shadow))
        });
        if !covered {
            plan.notes.push(format!(
                "{}: {} also loads as an ancestor file under home and hides AGENTS.md in repos sync doesn't manage; a settings target in this profile with exclude = [it] fixes that",
                t.name,
                shadow.display()
            ));
        }
    }
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
        .filter(|e| !matches!(e.meta.origin, crate::model::Origin::InRepo { .. }))
        .collect();
    let mut items = Vec::new();
    for e in &wanted {
        let name = e.id().trim_start_matches("skill.").to_string();
        items.push(link_item(stacks, &t.name, &dir.join(&name), e));
    }
    let wanted_names: Vec<String> =
        wanted.iter().map(|e| e.id().trim_start_matches("skill.").to_string()).collect();
    let mut unmanaged = Vec::new();
    for child in stack::sorted_children(&dir) {
        let name = stack::file_name(&child);
        if wanted_names.contains(&name) {
            continue;
        }
        let copy_of_ours = || {
            child.is_dir()
                && stacks.entries().any(|e| {
                    e.meta.kind == Kind::Skill
                        && matches!(
                            e.meta.origin,
                            crate::model::Origin::Own | crate::model::Origin::Adapted(_)
                        )
                        && e.id().trim_start_matches("skill.") == name
                })
        };
        match link_target(&child) {
            Some(to) if in_a_stack(stacks, &to) => {
                items.push(Item::new(&t.name, child.clone(), Action::Unlink))
            }
            None if copy_of_ours() => {
                let mut item = Item::new(&t.name, child.clone(), Action::RemoveDir);
                item.notes.push(
                    "a copy of a stack skill that no longer installs here; backed up, then removed"
                        .into(),
                );
                items.push(item);
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

/// The item that makes `link` a link to skill `e`'s dir in its stack (or says it already is).
fn link_item(stacks: &Stacks, target: &str, link: &Path, e: &Entry) -> Item {
    let link = link.to_path_buf();
    let src = e.meta_file.parent().expect("a sidecar has a dir").to_path_buf();
    {
        let mut item = Item::new(target, link.clone(), Action::Link);
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
        item
    }
}

/// Every checkout and (if asked) worktree of a repo, and the worktrees skipped for lying outside
/// `worktree_roots`.
fn checkouts(repo: &stack::Repo) -> (Vec<PathBuf>, Vec<PathBuf>) {
    let roots: Vec<String> = repo.worktree_roots.iter().map(|r| norm(&expand_home(r))).collect();
    let inside = |p: &Path| {
        let p = norm(p);
        roots.iter().any(|r| p == *r || p.starts_with(&format!("{}/", r.trim_end_matches('/'))))
    };
    let mut out: Vec<PathBuf> = Vec::new();
    let mut skipped: Vec<PathBuf> = Vec::new();
    let mut seen: Vec<String> = Vec::new();
    for c in &repo.checkouts {
        let c = expand_home(c);
        seen.push(norm(&c));
        out.push(c.clone());
        if !repo.worktrees {
            continue;
        }
        let Ok(list) = git::run(&c, &["worktree", "list", "--porcelain"]) else { continue };
        for w in list.lines().filter_map(|l| l.strip_prefix("worktree ")).map(PathBuf::from) {
            if seen.contains(&norm(&w)) {
                continue;
            }
            seen.push(norm(&w));
            if roots.is_empty() || inside(&w) {
                out.push(w);
            } else {
                skipped.push(w);
            }
        }
    }
    (out, skipped)
}

/// Where sync writes a place's files: a repo (its checkouts and worktrees) or a workspace (one dir, no git).
struct Place<'a> {
    label: String,
    name: String,
    workspace: bool,
    project: Option<String>,
    adapters: &'a [String],
    paths: Vec<PathBuf>,
}

impl Place<'_> {
    /// The layers a place's files take; `global`: every global entry (a file that must stand alone).
    fn layers(&self, global: bool) -> Layers {
        if self.workspace {
            Layers::Workspace { name: self.name.clone(), project: self.project.clone(), global }
        } else {
            Layers::Repo {
                repo: self.name.clone(),
                project: self.project.clone(),
                sub: None,
                global,
            }
        }
    }
}

/// Every declared role, both stacks, sorted.
pub fn declared_roles(stacks: &Stacks) -> Vec<String> {
    let mut roles: Vec<String> = stacks.all().flat_map(|s| s.manifest.roles.clone()).collect();
    roles.sort();
    roles.dedup();
    roles
}

/// The dir of the role files in a checkout (backbone requirement BB-3: a brief points at one).
pub const ROLE_DIR: &str = ".lords";

pub fn role_file(role: &str) -> String {
    format!("role-{role}.md")
}

/// Everything someone in `role` follows in a place, standing alone: every global entry for the role, the project,
/// the place, and the memory index; for every agent family (lines for only some are labelled).
pub fn role_composed(stacks: &Stacks, layers: Layers, role: &str) -> Composed {
    let ctx = Context {
        audience: vec![Audience::Opus, Audience::Sol, Audience::Astra],
        layers,
        public_only: false,
        skip_stores: Vec::new(),
        roles: Some(vec![role.to_string()]),
    };
    compose::compose(stacks, &ctx)
}

/// The adapters a place's files are written for (default: every adapter); unknown ids are skipped.
fn place_adapters(place: &Place) -> Vec<&'static dyn Adapter> {
    if place.adapters.is_empty() {
        adapters::ALL.to_vec()
    } else {
        place.adapters.iter().filter_map(|id| adapters::get(id)).collect()
    }
}

fn place_items(stacks: &Stacks, place: &Place, plan: &mut Plan) {
    let mut chosen: Vec<&dyn Adapter> = Vec::new();
    if place.adapters.is_empty() {
        chosen.extend(adapters::ALL.iter().copied());
    }
    for id in place.adapters {
        match adapters::get(id) {
            Some(a) => chosen.push(a),
            None => plan.notes.push(format!(
                "{}: unknown adapter `{id}` (known: {})",
                place.label,
                adapters::ids().join(", ")
            )),
        }
    }
    let path_entries = stacks
        .entries()
        .filter(|e| {
            e.active()
                && !place.workspace
                && matches!(&e.scope, crate::model::Scope::Path { repo: r, .. } if *r == place.name)
        })
        .count();
    if path_entries > 0 {
        plan.notes.push(format!(
            "{}: {path_entries} path-scoped entries are not written yet",
            place.label
        ));
    }
    let mut excludes: Vec<(PathBuf, Vec<String>)> = Vec::new();
    let mut add_exclude = |p: &Path, what: &str| {
        if place.workspace {
            return;
        }
        if let Ok(common) =
            git::run(p, &["rev-parse", "--path-format=absolute", "--git-common-dir"])
        {
            let ex = PathBuf::from(common.trim()).join("info").join("exclude");
            match excludes.iter_mut().find(|(e, _)| *e == ex) {
                Some((_, files)) if !files.iter().any(|f| f == what) => {
                    files.push(what.to_string())
                }
                Some(_) => {}
                None => excludes.push((ex, vec![what.to_string()])),
            }
        }
    };
    for adapter in chosen {
        let file = adapter.repo_file();
        let ctx = context(stacks, adapter, adapter.audience(), place.layers(false));
        let composed = compose::compose(stacks, &ctx);
        let text = adapter.render(&composed);
        for p in &place.paths {
            let target = format!("{} ({file})", place.label);
            let path = p.join(file);
            if !p.is_dir() {
                let mut item = Item::new(&target, path, Action::Conflict);
                item.notes.push("checkout not found".into());
                plan.items.push(item);
                continue;
            }
            if !place.workspace && git::run(p, &["ls-files", "--error-unmatch", file]).is_ok() {
                let mut item = Item::new(&target, path, Action::Conflict);
                item.notes
                    .push(format!("{file} is tracked here; sync writes only untracked files"));
                plan.items.push(item);
                continue;
            }
            let current = read(&path).unwrap_or_default();
            // The dir's own instructions still load: the adapter says how (an include, or the text inline).
            let text = match adapter.repo_preamble(p) {
                Some(pre) if !text.is_empty() => compose::with_preamble(&text, &pre),
                _ => text.clone(),
            };
            let proposed = compose::place_block(&current, &text, false);
            let mut item = file_item(&target, &path, proposed, &composed);
            size_note(&mut item, adapter, &text);
            plan.items.push(item);
            add_exclude(p, file);
        }
    }
    // Development skills, linked into each checkout for harnesses that read a skills dir there. Each link's exact
    // path is excluded, never the dir, so skills the repo tracks there stay visible to git.
    let skill_ctx = Context {
        audience: vec![Audience::Opus, Audience::Sol, Audience::Astra],
        layers: place.layers(false),
        public_only: false,
        skip_stores: Vec::new(),
        roles: None,
    };
    let skills: Vec<&Entry> = compose::applicable(stacks, &skill_ctx)
        .into_iter()
        .filter(|e| {
            e.meta.kind == Kind::Skill
                && !matches!(e.meta.origin, crate::model::Origin::InRepo { .. })
        })
        .collect();
    let skill_dirs: Vec<&str> =
        place_adapters(place).iter().filter_map(|a| a.repo_skills_dir()).collect();
    for dir in skill_dirs {
        for p in place.paths.iter().filter(|p| p.is_dir()) {
            for e in &skills {
                let name = e.id().trim_start_matches("skill.").to_string();
                let rel = format!("{dir}/{name}");
                let target = format!("{} (skill {name})", place.label);
                if !place.workspace
                    && git::run(p, &["ls-files", &rel]).is_ok_and(|o| !o.trim().is_empty())
                {
                    let mut item = Item::new(&target, p.join(&rel), Action::Conflict);
                    item.notes
                        .push(format!("the repo tracks a skill at {rel}; rename one of them"));
                    plan.items.push(item);
                    continue;
                }
                plan.items.push(link_item(stacks, &target, &p.join(&rel), e));
                add_exclude(p, &rel);
            }
        }
    }
    // One composed file per role, at a stable untracked path, for briefs to point at (backbone requirement BB-3).
    for role in declared_roles(stacks) {
        let composed = role_composed(stacks, place.layers(true), &role);
        for p in place.paths.iter().filter(|p| p.is_dir()) {
            let path = p.join(ROLE_DIR).join(role_file(&role));
            let current = read(&path).unwrap_or_default();
            let proposed = compose::place_block(&current, &composed.text, false);
            plan.items.push(file_item(
                &format!("{} (role {role})", place.label),
                &path,
                proposed,
                &composed,
            ));
            add_exclude(p, &format!("{ROLE_DIR}/"));
        }
    }
    for (ex, files) in excludes {
        let current = read(&ex).unwrap_or_default();
        let have: Vec<&str> = current.lines().map(str::trim).collect();
        let missing: Vec<&String> = files.iter().filter(|f| !have.contains(&f.as_str())).collect();
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
            &format!("{} (git exclude)", place.label),
            &ex,
            proposed,
            &Composed::default(),
        ));
    }
    if place.workspace {
        return;
    }
    // The tracked AGENTS.md of the first checkout: what the public layers would make it. Shown, never written.
    let tracked = place.paths[0].join("AGENTS.md");
    if let Some(current) = read(&tracked) {
        let ctx = Context {
            audience: vec![Audience::Opus, Audience::Sol, Audience::Astra],
            layers: place.layers(true),
            public_only: true,
            skip_stores: Vec::new(),
            roles: None,
        };
        let composed = compose::compose(stacks, &ctx);
        let proposed = compose::place_block(&current, &composed.text, false);
        let mut item = file_item(
            &format!("{} (tracked AGENTS.md)", place.label),
            &tracked,
            proposed,
            &composed,
        );
        item.action = if item.current == item.proposed { Action::InSync } else { Action::Report };
        item.notes.push("tracked: changes by a commit on the repo's branch, never by sync; agents with a stack read the untracked files instead".into());
        plan.items.push(item);
    }
}

fn repo_items(stacks: &Stacks, name: &str, repo: &stack::Repo, plan: &mut Plan) {
    let (paths, skipped) = checkouts(repo);
    if paths.is_empty() {
        plan.notes.push(format!("repo {name}: no checkouts declared"));
        return;
    }
    if !skipped.is_empty() {
        let list: Vec<String> = skipped.iter().map(|p| p.display().to_string()).collect();
        plan.notes.push(format!(
            "repo {name}: {} worktree(s) outside worktree_roots skipped: {}",
            skipped.len(),
            list.join(", ")
        ));
    }
    let place = Place {
        label: format!("repo {name}"),
        name: name.to_string(),
        workspace: false,
        project: stacks.repo_project(name),
        adapters: &repo.adapters,
        paths,
    };
    place_items(stacks, &place, plan);
}

fn workspace_items(stacks: &Stacks, name: &str, w: &stack::Workspace, plan: &mut Plan) {
    let place = Place {
        label: format!("workspace {name}"),
        name: name.to_string(),
        workspace: true,
        project: w.project.clone(),
        adapters: &w.adapters,
        paths: vec![expand_home(&w.path)],
    };
    place_items(stacks, &place, plan);
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
                "instructions" => plan.items.extend(instructions_items(stacks, t, adapter)),
                "skills" => plan.items.extend(skills_items(stacks, t, adapter)),
                "settings" => plan.items.push(settings_item(t, adapter)),
                k => return Err(format!("target {}: unknown kind {k}", t.name)),
            }
        }
        // Profiles sharing one `via` file plan it once; different texts for one file can't both be written.
        let mut kept: Vec<Item> = Vec::new();
        for item in std::mem::take(&mut plan.items) {
            match kept.iter_mut().find(|k| norm(&k.path) == norm(&item.path)) {
                Some(k) if k.proposed == item.proposed => {}
                Some(k) => {
                    k.action = Action::Conflict;
                    k.notes.push(format!("{} wants different text in the same file", item.target));
                }
                None => kept.push(item),
            }
        }
        plan.items = kept;
        shadow_notes(private, &mut plan);
    }
    if !opts.user_only {
        for (name, repo) in &private.manifest.repos {
            if opts.repo.as_deref().is_some_and(|r| r != name) {
                continue;
            }
            repo_items(stacks, name, repo, &mut plan);
        }
        for (name, w) in &private.manifest.workspaces {
            if opts.repo.as_deref().is_some_and(|r| r != name) {
                continue;
            }
            workspace_items(stacks, name, w, &mut plan);
        }
        if let Some(r) = &opts.repo {
            if !private.manifest.repos.contains_key(r)
                && !private.manifest.workspaces.contains_key(r)
            {
                return Err(format!("no repo or workspace `{r}` in the private stack.toml"));
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
            Action::RemoveDir => {
                fs::create_dir_all(backup_dir).map_err(|e| e.to_string())?;
                fs::rename(&item.path, &backup)
                    .map_err(|e| format!("{}: {e}", item.path.display()))?;
            }
            Action::InSync | Action::Report | Action::Conflict => continue,
        }
        done.push(format!("{:?} {}", item.action, item.path.display()));
    }
    Ok(done)
}

/// What an adapter's user-level file holds: the composition sync writes (the GUI shows it line by line).
pub fn user_composed(stacks: &Stacks, adapter: &dyn Adapter) -> Composed {
    compose::compose(stacks, &context(stacks, adapter, adapter.audience(), Layers::Global))
}

/// What an adapter's untracked file in a repo holds, before the tracked-instructions preamble.
pub fn repo_composed(stacks: &Stacks, place: &str, adapter: &dyn Adapter) -> Composed {
    compose::compose(
        stacks,
        &context(stacks, adapter, adapter.audience(), place_layers(stacks, place, false)),
    )
}

/// A repo's or a workspace's layers, by name.
pub fn place_layers(stacks: &Stacks, place: &str, global: bool) -> Layers {
    if stacks.declares_workspace(place) && !stacks.declares_repo(place) {
        Layers::Workspace { name: place.into(), project: stacks.workspace_project(place), global }
    } else {
        Layers::Repo { repo: place.into(), project: stacks.repo_project(place), sub: None, global }
    }
}

/// `check`'s view of the user-level targets: a warning for each that differs from a fresh composition, and the
/// plan's notes (known gaps, pending imports) as info. Nothing without a private stack.
pub fn drift_findings(stacks: &Stacks) -> Vec<crate::Finding> {
    use crate::Finding;
    if stacks.private.is_none() {
        return Vec::new();
    }
    match plan(stacks, &Options { repo: None, user_only: true }) {
        Ok(plan) => {
            let mut out: Vec<Finding> = plan
                .items
                .iter()
                .filter(|i| i.changes())
                .map(|i| {
                    Finding::warning(
                        "drift",
                        format!("{}: {:?} (`lords sync` shows the diff)", i.target, i.action),
                    )
                    .at(&i.path, None)
                })
                .collect();
            for i in plan.items.iter() {
                for n in i.notes.iter().filter(|n| n.contains(" reads at most ")) {
                    out.push(
                        Finding::warning("size", format!("{}: {n}", i.target)).at(&i.path, None),
                    );
                }
            }
            out.extend(repo_size_findings(stacks));
            out.extend(verbatim_findings(stacks));
            out.extend(plan.notes.iter().map(|n| Finding::info("sync-note", n.clone())));
            out
        }
        Err(e) => vec![Finding::warning("drift", format!("no sync plan: {e}"))],
    }
}

/// `sync --apply` as one step: import every memory store (one commit each), plan again from the updated stacks,
/// then apply with backups under `backup_dir`. Returns what changed. The CLI and the GUI both call this.
pub fn import_and_apply(
    public: &Path,
    private: Option<&Path>,
    opts: &Options,
    backup_dir: &Path,
) -> Result<Vec<String>, String> {
    let mut done = Vec::new();
    let stacks = Stacks::load(public, private)?;
    if let Some(p) = &stacks.private {
        if !p.manifest.memory_stores.is_empty() {
            for r in crate::ops::import(p, None, true)? {
                if let Some(c) = r.commit {
                    done.push(format!("imported memory from {}: commit {c}", r.store));
                }
            }
        }
    }
    let stacks = Stacks::load(public, private)?;
    done.extend(apply(&plan(&stacks, opts)?, backup_dir)?);
    Ok(done)
}

/// Where `--apply` keeps this run's backups: `~/.lords-command/backups/<date>-<pid>`.
pub fn backup_dir() -> Result<PathBuf, String> {
    let home = crate::config::home().ok_or("no home dir")?;
    let stamp = format!("{}-{}", crate::ops::today(), std::process::id());
    Ok(home.join(".lords-command").join("backups").join(stamp))
}

/// Whether every line of `tracked` appears in `file`, in order (other lines may come between).
pub fn keeps_in_order(tracked: &str, file: &str) -> Result<(), (usize, String)> {
    let mut lines = file.lines().map(|l| l.trim_end_matches('\r'));
    for (i, t) in tracked.lines().map(|l| l.trim_end_matches('\r')).enumerate() {
        if !lines.any(|l| l == t) {
            return Err((i + 1, t.to_string()));
        }
    }
    Ok(())
}

/// backbone requirement BB-2: a file that hides a dir's own `AGENTS.md` must not drop a line of it. Codex's override carries it
/// verbatim, every line in order; Claude's local file includes it with `@AGENTS.md`. Checked in every checkout
/// and worktree sync writes, and in every workspace.
pub fn verbatim_findings(stacks: &Stacks) -> Vec<crate::Finding> {
    let Some(private) = &stacks.private else { return Vec::new() };
    let mut dirs: Vec<PathBuf> = Vec::new();
    for repo in private.manifest.repos.values() {
        dirs.extend(checkouts(repo).0);
    }
    dirs.extend(private.manifest.workspaces.values().map(|w| expand_home(&w.path)));
    let mut out = Vec::new();
    for d in dirs {
        let Some(tracked) = read(&d.join("AGENTS.md")) else { continue };
        if let Some(over) = read(&d.join("AGENTS.override.md")) {
            if let Err((n, line)) = keeps_in_order(&tracked, &over) {
                out.push(
                    crate::Finding::error(
                        "verbatim",
                        format!("AGENTS.override.md drops line {n} of AGENTS.md (or its order): `{line}`; `lords sync` rewrites it"),
                    )
                    .at(d.join("AGENTS.override.md"), None),
                );
            }
        }
        if let Some(local) = read(&d.join("CLAUDE.local.md")) {
            if !local.lines().any(|l| l.trim() == "@AGENTS.md") {
                out.push(
                    crate::Finding::error(
                        "verbatim",
                        "CLAUDE.local.md hides AGENTS.md without including it (`@AGENTS.md`)",
                    )
                    .at(d.join("CLAUDE.local.md"), None),
                );
            }
        }
    }
    out
}

/// What one session in a repo reads together (the user-level text, the repo's own, and the tracked-instructions
/// preamble of its first checkout), against each adapter's limit.
fn repo_size_findings(stacks: &Stacks) -> Vec<crate::Finding> {
    let Some(private) = &stacks.private else { return Vec::new() };
    let mut out = Vec::new();
    for (name, repo) in &private.manifest.repos {
        let first = repo.checkouts.first().map(|c| expand_home(c));
        for adapter in adapters::ALL.iter().copied() {
            if !repo.adapters.is_empty() && !repo.adapters.iter().any(|a| a == adapter.id()) {
                continue;
            }
            let global = adapter.render(&user_composed(stacks, adapter)).len();
            let own = adapter.render(&repo_composed(stacks, name, adapter)).len();
            let pre =
                first.as_deref().and_then(|c| adapter.repo_preamble(c)).map_or(0, |p| p.len());
            let total = global + own + pre;
            if let Some(limit) = over_size(adapter, total) {
                out.push(crate::Finding::warning(
                    "size",
                    format!(
                        "repo {name}: {} reads {total} bytes there ({global} user level, {own} repo, {pre} tracked); its limit is {limit}",
                        adapter.id()
                    ),
                ));
            }
        }
    }
    out
}
