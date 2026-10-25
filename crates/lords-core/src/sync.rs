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
    /// A temporary entry that has ended: `--apply` retires it in its stack (with a commit) before writing.
    RetireEntry,
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
    /// For `Relink` and `Unlink`: where the link points now (checked again before apply touches it, and kept in
    /// the run's backups so the link can be put back).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub link_from: Option<PathBuf>,
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
            link_from: None,
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
    /// Only this adapter's files (its targets, and its file and skill links in each place); role files, which
    /// every adapter's agents read, are always planned. `None`: every adapter.
    pub adapter: Option<String>,
    /// With `repo`: only this one checkout or worktree of it (a new worktree's files, from its post-checkout hook).
    pub place: Option<PathBuf>,
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

/// Best-effort text of a file sync only reads (a skill's SKILL.md for a diff, a tracked file it carries).
fn read(path: &Path) -> Option<String> {
    fs::read_to_string(path).ok()
}

/// A target's text: `None` when nothing is there. Anything sync can't read as UTF-8 text is an error, never taken
/// for a missing file (Sol review, finding 2): sync then leaves it alone.
fn read_target(path: &Path) -> Result<Option<String>, String> {
    match fs::symlink_metadata(path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(format!("can't read it: {e}")),
        Ok(m) if m.is_dir() => return Err("a dir is where a file goes".into()),
        Ok(_) => {}
    }
    let bytes = fs::read(path).map_err(|e| format!("can't read it: {e}"))?;
    String::from_utf8(bytes)
        .map(Some)
        .map_err(|_| "not UTF-8 text (UTF-16 or binary?); sync doesn't edit it".into())
}

/// Why sync may not write `path`, if it may not: the path is a link, or a link in a dir above it would send the
/// write somewhere else (Sol review, finding 5). Sync writes only plain files at the path it was given.
pub fn write_guard(path: &Path) -> Result<(), String> {
    if fs::symlink_metadata(path).is_ok_and(|m| m.file_type().is_symlink()) {
        return Err(format!("{} is a link; sync writes only plain files", display(path)));
    }
    for dir in path.ancestors().skip(1) {
        if fs::symlink_metadata(dir).is_ok_and(|m| m.file_type().is_symlink()) {
            let to = link_target(dir).map(|t| display(&t)).unwrap_or_else(|| "elsewhere".into());
            return Err(format!(
                "{} is a link (to {to}), so a write below it would land there",
                display(dir)
            ));
        }
    }
    Ok(())
}

fn conflict(target: &str, path: &Path, note: impl Into<String>) -> Item {
    let mut item = Item::new(target, path.to_path_buf(), Action::Conflict);
    item.notes.push(note.into());
    item
}

/// The item that puts `block` into the file at `path`, keeping the owner's text around it (or replacing it).
fn block_item(target: &str, path: &Path, block: &str, replace: bool, composed: &Composed) -> Item {
    let current = match read_target(path) {
        Ok(c) => c.unwrap_or_default(),
        Err(e) => return conflict(target, path, e),
    };
    match compose::place_block(&current, block, replace) {
        Ok(proposed) => file_item(target, path, proposed, composed),
        Err(e) => conflict(target, path, e),
    }
}

fn display(p: &Path) -> String {
    p.display().to_string()
}

/// An item that makes `path` hold `proposed` (or says it already does).
fn file_item(target: &str, path: &Path, proposed: String, composed: &Composed) -> Item {
    if let Err(e) = write_guard(path) {
        return conflict(target, path, e);
    }
    let current = match read_target(path) {
        Ok(c) => c,
        Err(e) => return conflict(target, path, e),
    };
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
        by_reference: false,
        role_dir: None,
        relative_to: None,
        default_role: None,
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
    let current = read_target(&path).ok().flatten().unwrap_or_default();
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
            // The shared file keeps its owner's text too (Sol review, finding 4).
            let mut s =
                block_item(&format!("shared text ({})", t.name), &shared, &text, false, &composed);
            size_note(&mut s, adapter, &text);
            items.push(s);
            if composed.text.is_empty() {
                String::new()
            } else {
                compose::block(&line)
            }
        }
    };
    let mut item = block_item(&t.name, &path, &block, replace, &composed);
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

/// `text` (a JSON object) with its top-level `key` set to `value`, every other byte as it was: the key's old value
/// is replaced in place, or the key is added after the last member in the file's own line ending and indent (Sol
/// review, P2 finding 20). An error when `text` isn't one JSON object.
fn json_set_key(text: &str, key: &str, value: &serde_json::Value) -> Result<String, String> {
    let b = text.as_bytes();
    let ws = |mut i: usize| {
        while i < b.len() && matches!(b[i], b' ' | b'\t' | b'\r' | b'\n') {
            i += 1;
        }
        i
    };
    // The end of the string starting at `i` (a quote).
    let string_end = |mut i: usize| -> Result<usize, String> {
        i += 1;
        while i < b.len() {
            match b[i] {
                b'\\' => i += 2,
                b'"' => return Ok(i + 1),
                _ => i += 1,
            }
        }
        Err("an unterminated string".into())
    };
    // The end of the value starting at `i`.
    let value_end = |mut i: usize| -> Result<usize, String> {
        let mut depth = 0usize;
        while i < b.len() {
            match b[i] {
                b'"' => {
                    i = string_end(i)?;
                    if depth == 0 {
                        return Ok(i);
                    }
                    continue;
                }
                b'{' | b'[' => depth += 1,
                b'}' | b']' if depth == 0 => return Ok(i),
                b'}' | b']' => {
                    depth -= 1;
                    if depth == 0 {
                        return Ok(i + 1);
                    }
                }
                b',' if depth == 0 => return Ok(i),
                _ => {}
            }
            i += 1;
        }
        Err("an unterminated value".into())
    };
    let bom = if text.starts_with('\u{feff}') { 3 } else { 0 };
    let open = ws(bom);
    if b.get(open) != Some(&b'{') {
        return Err("not a JSON object".into());
    }
    // (key, key start, value start, value end) of each top-level member.
    let mut members: Vec<(String, usize, usize, usize)> = Vec::new();
    let mut i = ws(open + 1);
    let close = loop {
        match b.get(i) {
            Some(b'}') => break i,
            Some(b'"') => {
                let k_end = string_end(i)?;
                let k: String = serde_json::from_str(&text[i..k_end]).map_err(|e| e.to_string())?;
                let colon = ws(k_end);
                if b.get(colon) != Some(&b':') {
                    return Err("a member without a colon".into());
                }
                let v_start = ws(colon + 1);
                let v_end = value_end(v_start)?;
                let v_trim = text[v_start..v_end].trim_end().len() + v_start;
                members.push((k, i, v_start, v_trim));
                i = ws(v_end);
                match b.get(i) {
                    Some(b',') => i = ws(i + 1),
                    Some(b'}') => {}
                    _ => return Err("a member not followed by , or }".into()),
                }
            }
            _ => return Err("not a JSON object".into()),
        }
    };
    let new_value = serde_json::to_string(value).map_err(|e| e.to_string())?;
    let out = if let Some((_, _, vs, ve)) = members.iter().find(|m| m.0 == key) {
        format!("{}{new_value}{}", &text[..*vs], &text[*ve..])
    } else {
        let nl = if text.contains("\r\n") { "\r\n" } else { "\n" };
        let member = format!("\"{key}\": {new_value}");
        match members.last() {
            Some((_, _, _, last_end)) => {
                // The indent of the first member's line, when it starts that line.
                let first = members[0].1;
                let line_start = text[..first].rfind('\n').map_or(0, |n| n + 1);
                let lead = &text[line_start..first];
                let indent = if lead.chars().all(|c| c == ' ' || c == '\t') { lead } else { "  " };
                format!("{},{nl}{indent}{member}{}", &text[..*last_end], &text[*last_end..])
            }
            None => format!("{}{nl}  {member}{nl}{}", &text[..open + 1], &text[close..]),
        }
    };
    // The result must be the same object with only `key` changed.
    let (before, after): (serde_json::Value, serde_json::Value) = (
        serde_json::from_str(text.trim_start_matches('\u{feff}')).map_err(|e| e.to_string())?,
        serde_json::from_str(out.trim_start_matches('\u{feff}')).map_err(|e| e.to_string())?,
    );
    let (mut x, mut y) = (before, after.clone());
    if let (Some(x), Some(y)) = (x.as_object_mut(), y.as_object_mut()) {
        x.remove(key);
        y.remove(key);
    }
    if x != y || after.get(key) != Some(value) {
        return Err("editing the key would change other keys; left alone".into());
    }
    Ok(out)
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
    let current = match read_target(&path) {
        Ok(c) => c,
        Err(e) => return conflict(&t.name, &path, e),
    };
    let mut value: serde_json::Value =
        match current.as_deref().map(|c| serde_json::from_str(c.trim_start_matches('\u{feff}'))) {
            None => serde_json::json!({}),
            Some(Ok(v @ serde_json::Value::Object(_))) => v,
            Some(_) => return conflict(&t.name, &path, "not a JSON object; left alone"),
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
    let list = serde_json::Value::Array(list.clone());
    let proposed = match (&current, added) {
        (Some(c), false) => c.clone(),
        (Some(c), true) => match json_set_key(c, key, &list) {
            Ok(text) => text,
            Err(e) => return conflict(&t.name, &path, e),
        },
        (None, _) => serde_json::to_string_pretty(&value).expect("JSON serialises") + "\n",
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

/// Whether `link` is a link that points at `to`.
pub fn link_points_at(link: &Path, to: &Path) -> bool {
    link_target(link).is_some_and(|t| same_place(&t, to))
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

/// A path as compared for containment: resolved where it exists, else cleaned lexically (`.` and `..` folded).
fn resolved(p: &Path) -> String {
    if let Ok(c) = fs::canonicalize(p) {
        return strip_verbatim(&c).trim_end_matches('/').to_string();
    }
    let mut clean = PathBuf::new();
    for c in p.components() {
        match c {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                clean.pop();
            }
            c => clean.push(c.as_os_str()),
        }
    }
    strip_verbatim(&clean).trim_end_matches('/').to_string()
}

/// Whether `p` lies inside `root` (or is it), dir by dir: a sibling whose name merely starts with the root's is
/// outside (Sol review, finding 9).
fn inside(p: &Path, root: &Path) -> bool {
    let (p, root) = (resolved(p), resolved(root));
    p == root || p.starts_with(&format!("{root}/"))
}

fn in_a_stack(stacks: &Stacks, p: &Path) -> bool {
    stacks.all().any(|s| inside(p, &s.root))
}

/// Whether `dir` is an installed copy of a skill the stacks made (own or adapted) called `name`: a real dir, no
/// links in it, a SKILL.md that names it, and no file the stack's copy doesn't have. A dir that merely shares the
/// name stays the owner's (Sol review, finding 10).
fn copy_of_ours(stacks: &Stacks, dir: &Path, name: &str) -> bool {
    if !fs::symlink_metadata(dir).is_ok_and(|m| m.is_dir()) {
        return false;
    }
    let named = read(&dir.join(stack::SKILL_FILE)).is_some_and(|t| {
        crate::frontmatter::split(&t).yaml.is_some_and(|y| {
            y.lines().any(|l| {
                l.strip_prefix("name:")
                    .is_some_and(|v| v.trim().trim_matches(['"', char::from(39)]) == name)
            })
        })
    });
    if !named || stack::has_links(dir) {
        return false;
    }
    let files: Vec<PathBuf> = stack::all_files(dir)
        .iter()
        .filter_map(|f| f.strip_prefix(dir).ok().map(Path::to_path_buf))
        .collect();
    stacks.entries().any(|e| {
        e.meta.kind == Kind::Skill
            && matches!(e.meta.origin, crate::model::Origin::Own | crate::model::Origin::Adapted(_))
            && e.id().trim_start_matches("skill.") == name
            && e.meta_file.parent().is_some_and(|src| {
                files.iter().all(|r| src.join(r).is_file() || r == Path::new(stack::SIDECAR))
            })
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
        match link_target(&child) {
            Some(to) if in_a_stack(stacks, &to) => {
                let mut item = Item::new(&t.name, child.clone(), Action::Unlink);
                item.link_from = Some(to);
                items.push(item);
            }
            None if copy_of_ours(stacks, &child, &name) => {
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
            item.link_from = Some(to.clone());
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

/// The header line above the exclude patterns sync owns in a repo's `.git/info/exclude`.
const EXCLUDE_HEADER: &str = "# Lords Command: agent files written by sync";

/// Whether an exclude line is one sync writes: the exact anchored paths it writes now, and the broader forms
/// earlier versions wrote under the same header.
fn ours(line: &str) -> bool {
    let l = line.trim().trim_start_matches('/');
    ["CLAUDE.local.md", "AGENTS.override.md"].contains(&l)
        || l.starts_with(&format!("{ROLE_DIR}/"))
        || l.starts_with(".claude/skills/")
}

/// A repo's exclude file with sync's section holding exactly `files`: exact paths from the worktree's root, so a
/// file of the same name deeper down, or the owner's own file beside them, stays visible to git (Sol review, P2
/// finding 21). Every other line keeps its place and bytes; sync's earlier sections under its header merge into
/// one.
fn excludes_text(current: &str, files: &[String]) -> String {
    let mut kept: Vec<&str> = Vec::new();
    let mut in_ours = false;
    for line in current.split_inclusive('\n') {
        let content = line.trim_end_matches('\n').trim_end_matches('\r');
        if content == EXCLUDE_HEADER {
            in_ours = true;
            continue;
        }
        if in_ours && ours(content) {
            continue;
        }
        in_ours = false;
        kept.push(line);
    }
    let mut out: String = kept.concat();
    let mut want: Vec<&String> = files.iter().collect();
    want.sort();
    want.dedup();
    if !want.is_empty() {
        if !out.is_empty() && !out.ends_with('\n') {
            out.push('\n');
        }
        out.push_str(EXCLUDE_HEADER);
        out.push('\n');
        for f in want {
            out.push_str(f);
            out.push('\n');
        }
    }
    out
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
        by_reference: false,
        role_dir: None,
        relative_to: None,
        default_role: None,
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

fn place_items(stacks: &Stacks, place: &Place, only: Option<&str>, plan: &mut Plan) {
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
    chosen.retain(|a| only.is_none_or(|o| o == a.id()));
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
            let (text, composed) = place_text(stacks, place.layers(false), adapter, p);
            let mut item = block_item(&target, &path, &text, false, &composed);
            size_note(&mut item, adapter, &text);
            plan.items.push(item);
            add_exclude(p, &format!("/{file}"));
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
        by_reference: false,
        role_dir: None,
        relative_to: None,
        default_role: None,
    };
    let skills: Vec<&Entry> = compose::applicable(stacks, &skill_ctx)
        .into_iter()
        .filter(|e| {
            e.meta.kind == Kind::Skill
                && !matches!(e.meta.origin, crate::model::Origin::InRepo { .. })
        })
        .collect();
    let skill_dirs: Vec<&str> = place_adapters(place)
        .iter()
        .filter(|a| only.is_none_or(|o| o == a.id()))
        .filter_map(|a| a.repo_skills_dir())
        .collect();
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
                add_exclude(p, &format!("/{rel}"));
            }
            // A link of ours here for a skill that no longer applies (ended, retired, another place's) goes
            // (Sol review, finding 19); anything else in the dir is the repo's or the owner's.
            let wanted: Vec<String> =
                skills.iter().map(|e| e.id().trim_start_matches("skill.").to_string()).collect();
            for child in stack::sorted_children(&p.join(dir)) {
                let name = stack::file_name(&child);
                if wanted.contains(&name) {
                    continue;
                }
                if let Some(to) = link_target(&child).filter(|to| in_a_stack(stacks, to)) {
                    let mut item = Item::new(
                        &format!("{} (skill {name})", place.label),
                        child,
                        Action::Unlink,
                    );
                    item.link_from = Some(to);
                    item.notes
                        .push("a link of ours for a skill that no longer applies here".into());
                    plan.items.push(item);
                    add_exclude(p, &format!("/{dir}/{name}"));
                }
            }
        }
    }
    // One composed file per role, at a stable untracked path, for briefs to point at (backbone requirement BB-3).
    for role in declared_roles(stacks) {
        let composed = role_composed(stacks, place.layers(true), &role);
        for p in place.paths.iter().filter(|p| p.is_dir()) {
            let path = p.join(ROLE_DIR).join(role_file(&role));
            plan.items.push(block_item(
                &format!("{} (role {role})", place.label),
                &path,
                &composed.text,
                false,
                &composed,
            ));
            add_exclude(p, &format!("/{ROLE_DIR}/{}", role_file(&role)));
        }
    }
    for (ex, files) in excludes {
        let current = match read_target(&ex) {
            Ok(c) => c.unwrap_or_default(),
            Err(e) => {
                plan.items.push(conflict(&format!("{} (git exclude)", place.label), &ex, e));
                continue;
            }
        };
        let proposed = excludes_text(&current, &files);
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
            by_reference: false,
            role_dir: None,
            relative_to: None,
            default_role: None,
        };
        let composed = compose::compose(stacks, &ctx);
        let label = format!("{} (tracked AGENTS.md)", place.label);
        let mut item = match compose::place_block(&current, &composed.text, false) {
            Ok(proposed) => file_item(&label, &tracked, proposed, &composed),
            Err(e) => conflict(&label, &tracked, e),
        };
        item.action = match item.action {
            Action::Conflict => Action::Report,
            _ if item.current == item.proposed => Action::InSync,
            _ => Action::Report,
        };
        item.notes.push("tracked: changes by a commit on the repo's branch, never by sync; agents with a stack read the untracked files instead".into());
        plan.items.push(item);
    }
}

fn repo_items(
    stacks: &Stacks,
    name: &str,
    repo: &stack::Repo,
    only: Option<&str>,
    only_path: Option<&Path>,
    plan: &mut Plan,
) {
    let (mut paths, skipped) = checkouts(repo);
    if let Some(one) = only_path {
        paths.retain(|p| norm(p) == norm(one));
        if paths.is_empty() {
            plan.notes.push(format!(
                "repo {name}: {} is none of its checkouts or worktrees under worktree_roots",
                display(one)
            ));
            return;
        }
    }
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
    place_items(stacks, &place, only, plan);
}

fn workspace_items(
    stacks: &Stacks,
    name: &str,
    w: &stack::Workspace,
    only: Option<&str>,
    plan: &mut Plan,
) {
    let place = Place {
        label: format!("workspace {name}"),
        name: name.to_string(),
        workspace: true,
        project: w.project.clone(),
        adapters: &w.adapters,
        paths: vec![expand_home(&w.path)],
    };
    place_items(stacks, &place, only, plan);
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
    // Ended temporary entries: never composed; listed here, retired by import_and_apply.
    for (e, why) in crate::temporary::expired(stacks, &crate::ops::today()) {
        let mut item = Item::new(
            &format!("{} (temporary entry)", e.id()),
            e.meta_file.clone(),
            Action::RetireEntry,
        );
        item.notes.push(format!(
            "expired: {why}; --apply retires it (one commit) and no file composes it"
        ));
        plan.items.push(item);
    }
    if opts.repo.is_none() {
        for t in &private.manifest.targets {
            if opts.adapter.as_deref().is_some_and(|a| a != t.adapter) {
                continue;
            }
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
            repo_items(
                stacks,
                name,
                repo,
                opts.adapter.as_deref(),
                opts.place.as_deref(),
                &mut plan,
            );
        }
        for (name, w) in &private.manifest.workspaces {
            if opts.repo.as_deref().is_some_and(|r| r != name) {
                continue;
            }
            if opts.place.is_none() {
                workspace_items(stacks, name, w, opts.adapter.as_deref(), &mut plan);
            }
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

/// Removes a link (a junction, or a symlink) and never what it points at: a non-recursive removal of the link
/// itself, refused when `link` is no link (Sol review, finding 8; the 25 Oct incident: a recursive delete through a
/// junction emptied the stack's skill dirs).
fn remove_link(link: &Path) -> Result<(), String> {
    if !fs::symlink_metadata(link).is_ok_and(|m| m.file_type().is_symlink()) {
        return Err(format!("{} is no link; sync removes only links it owns", link.display()));
    }
    fs::remove_dir(link)
        .or_else(|_| fs::remove_file(link))
        .map_err(|e| format!("{}: {e}", link.display()))
}

/// Writes `bytes` to `path` through a new file beside it, renamed over the target: a reader sees the old file or
/// the new one, never a part (Sol review, finding 7), and a hardlink to the old file keeps the old bytes (finding 5).
fn write_atomic(path: &Path, bytes: &[u8]) -> Result<(), String> {
    use std::io::Write;
    let dir = path.parent().ok_or_else(|| format!("{}: no parent dir", path.display()))?;
    fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let tmp = dir.join(format!(".{name}.lords-{}.tmp", std::process::id()));
    let written = (|| -> std::io::Result<()> {
        let mut f = fs::OpenOptions::new().write(true).create_new(true).open(&tmp)?;
        f.write_all(bytes)?;
        f.sync_all()
    })();
    let done = written.and_then(|()| fs::rename(&tmp, path));
    if let Err(e) = done {
        let _ = fs::remove_file(&tmp);
        return Err(format!("{}: {e}", path.display()));
    }
    Ok(())
}

/// Where a backed-up path goes inside a run's backup dir: its whole path, dir by dir (`C:\a\b` is `C/a/b`), so two
/// different paths never share a backup (Sol review, finding 1).
fn mirror(path: &Path) -> PathBuf {
    use std::path::Component;
    let mut out = PathBuf::new();
    for c in path.components() {
        match c {
            Component::Prefix(p) => {
                let s: String = p
                    .as_os_str()
                    .to_string_lossy()
                    .chars()
                    .filter(|c| c.is_ascii_alphanumeric())
                    .collect();
                out.push(if s.is_empty() { "root".to_string() } else { s });
            }
            Component::RootDir | Component::CurDir => {}
            Component::ParentDir => out.push("__up__"),
            Component::Normal(n) => out.push(n),
        }
    }
    out
}

/// One apply's backups: the replaced bytes, moved dirs and removed links, and a manifest (`manifest.tsv`, one line
/// per change, written before the change) that `restore` reads back.
struct Backups {
    dir: PathBuf,
    manifest: fs::File,
}

/// A manifest line's fields: tabs and line breaks in paths would break the format, so a path with one is refused.
fn field(p: &Path) -> Result<String, String> {
    let s = p.to_string_lossy().into_owned();
    if s.contains(['\t', '\n', '\r']) {
        return Err(format!("{s:?}: a tab or line break in a path; sync won't touch it"));
    }
    Ok(s)
}

impl Backups {
    /// A new, empty dir: an existing one (a second apply reusing it) is refused, so no backup is overwritten.
    fn open(dir: &Path) -> Result<Backups, String> {
        if let Some(parent) = dir.parent() {
            fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
        }
        fs::create_dir(dir).map_err(|e| {
            format!("backup dir {}: {e} (each apply needs a new one)", dir.display())
        })?;
        let manifest = fs::File::create(dir.join("manifest.tsv"))
            .map_err(|e| format!("{}: {e}", dir.display()))?;
        Ok(Backups { dir: dir.to_path_buf(), manifest })
    }

    fn record(&mut self, fields: &[String]) -> Result<(), String> {
        use std::io::Write;
        writeln!(self.manifest, "{}", fields.join("\t"))
            .and_then(|()| self.manifest.sync_all())
            .map_err(|e| format!("{}: {e}", self.dir.display()))
    }

    /// Keeps `bytes`, what `path` held, before it is replaced.
    fn file(&mut self, path: &Path, bytes: &[u8]) -> Result<(), String> {
        let rel = Path::new("files").join(mirror(path));
        write_atomic(&self.dir.join(&rel), bytes)?;
        self.record(&["file".into(), field(path)?, field(&rel)?])
    }

    /// Where the dir at `path` moves to, recorded first.
    fn dir_for(&mut self, path: &Path) -> Result<PathBuf, String> {
        let rel = Path::new("dirs").join(mirror(path));
        let to = self.dir.join(&rel);
        if let Some(parent) = to.parent() {
            fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
        }
        self.record(&["dir".into(), field(path)?, field(&rel)?])?;
        Ok(to)
    }

    /// A link about to be removed, and where it pointed.
    fn link(&mut self, path: &Path, to: &Path) -> Result<(), String> {
        self.record(&["link".into(), field(path)?, field(to)?])
    }

    /// A file or link apply is about to create, which a restore removes again (a file only while it still holds
    /// what apply wrote).
    fn created(&mut self, path: &Path, what: &str, detail: &str) -> Result<(), String> {
        self.record(&[format!("created-{what}"), field(path)?, detail.to_string()])
    }
}

/// One apply at a time on this machine (a user's sync and a post-checkout hook's): a lock file beside the
/// backups, waited for up to `secs` seconds.
struct ApplyLock(PathBuf);

impl ApplyLock {
    fn take(dir: &Path, secs: u64) -> Result<ApplyLock, String> {
        fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        let lock = dir.join("apply.lock");
        let until = std::time::Instant::now() + std::time::Duration::from_secs(secs);
        loop {
            match fs::OpenOptions::new().write(true).create_new(true).open(&lock) {
                Ok(mut f) => {
                    use std::io::Write;
                    let _ = writeln!(f, "pid {}", std::process::id());
                    return Ok(ApplyLock(lock));
                }
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                    if std::time::Instant::now() >= until {
                        return Err(format!(
                            "another apply holds {} ({}); if none is running, delete that file",
                            lock.display(),
                            fs::read_to_string(&lock).unwrap_or_default().trim()
                        ));
                    }
                    std::thread::sleep(std::time::Duration::from_millis(200));
                }
                Err(e) => return Err(format!("{}: {e}", lock.display())),
            }
        }
    }
}

impl Drop for ApplyLock {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}

fn sha(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    format!("{:x}", Sha256::digest(bytes))
}

/// Whether an item's path is still as the plan saw it; apply refuses the whole plan otherwise (Sol review, finding
/// 3: an edit after planning is never overwritten or lost).
fn unchanged(item: &Item) -> Result<(), String> {
    let stale = |what: &str| {
        Err(format!(
            "{}: {what} since the plan; plan again (nothing was changed)",
            item.path.display()
        ))
    };
    let meta = fs::symlink_metadata(&item.path).ok();
    let is_link = meta.as_ref().is_some_and(|m| m.file_type().is_symlink());
    match item.action {
        Action::Create | Action::Update => {
            write_guard(&item.path)?;
            let now =
                read_target(&item.path).map_err(|e| format!("{}: {e}", item.path.display()))?;
            if now != item.current {
                return stale("changed");
            }
        }
        Action::Link => {
            write_guard(&item.path)?;
            if meta.is_some() {
                return stale("something appeared");
            }
        }
        Action::Relink | Action::Unlink => {
            let now = link_target(&item.path);
            if !is_link || now.as_deref().map(norm) != item.link_from.as_deref().map(norm) {
                return stale("the link changed");
            }
        }
        Action::ReplaceDir | Action::RemoveDir => {
            if !meta.as_ref().is_some_and(|m| m.is_dir()) || is_link {
                return stale("the dir changed");
            }
        }
        Action::InSync | Action::Report | Action::Conflict | Action::RetireEntry => {}
    }
    Ok(())
}

/// Carries out every changing item of a plan, after checking each path is still as planned. Everything it
/// replaces or removes is first kept in `backup_dir` (a new dir), with a manifest that `restore` undoes it from.
/// Returns one line per change.
pub fn apply(plan: &Plan, backup_dir: &Path) -> Result<Vec<String>, String> {
    if plan.items.iter().any(|i| i.action == Action::Conflict) {
        return Err("the plan has conflicts; resolve them first (nothing was changed)".into());
    }
    let changing: Vec<&Item> = plan.items.iter().filter(|i| i.changes()).collect();
    let root = backup_dir.parent().unwrap_or(backup_dir);
    let _lock = ApplyLock::take(root, 60)?;
    for item in &changing {
        unchanged(item)?;
    }
    let mut done = Vec::new();
    if changing.iter().all(|i| i.action == Action::RetireEntry) {
        return Ok(done);
    }
    let mut backups = Backups::open(backup_dir)?;
    for item in changing {
        let path = &item.path;
        match item.action {
            Action::Create => {
                let new = item.proposed.as_deref().unwrap_or("");
                backups.created(path, "file", &sha(new.as_bytes()))?;
                write_atomic(path, new.as_bytes())?;
            }
            Action::Update => {
                backups.file(path, item.current.as_deref().unwrap_or("").as_bytes())?;
                write_atomic(path, item.proposed.as_deref().unwrap_or("").as_bytes())?;
            }
            Action::Link | Action::Relink | Action::ReplaceDir => {
                let to = item.link_to.as_ref().expect("a link item has a target");
                if item.action == Action::Relink {
                    let old = item.link_from.as_ref().expect("a relink knows the old target");
                    backups.link(path, old)?;
                    remove_link(path)?;
                }
                if item.action == Action::ReplaceDir {
                    let dest = backups.dir_for(path)?;
                    fs::rename(path, &dest).map_err(|e| format!("{}: {e}", path.display()))?;
                }
                if let Some(parent) = path.parent() {
                    fs::create_dir_all(parent).map_err(|e| e.to_string())?;
                }
                backups.created(path, "link", &field(to)?)?;
                make_link(path, to)?;
            }
            Action::Unlink => {
                let old = item.link_from.as_ref().expect("an unlink knows the old target");
                backups.link(path, old)?;
                remove_link(path)?;
            }
            Action::RemoveDir => {
                let dest = backups.dir_for(path)?;
                fs::rename(path, &dest).map_err(|e| format!("{}: {e}", path.display()))?;
            }
            // Retired by import_and_apply before this runs; nothing to write here.
            Action::InSync | Action::Report | Action::Conflict | Action::RetireEntry => continue,
        }
        done.push(format!("{:?} {}", item.action, path.display()));
    }
    Ok(done)
}

/// Undoes one apply from its backup dir, last change first: created files (while they still hold what apply
/// wrote) and links go, replaced bytes come back, moved dirs move back and removed links are made again. Anything
/// that has changed since is left as it is and listed. Returns one line per step.
pub fn restore(backup_dir: &Path) -> Result<Vec<String>, String> {
    let manifest = fs::read_to_string(backup_dir.join("manifest.tsv"))
        .map_err(|e| format!("{}: {e}", backup_dir.display()))?;
    let root = backup_dir.parent().unwrap_or(backup_dir);
    let _lock = ApplyLock::take(root, 60)?;
    let mut done = Vec::new();
    for line in manifest.lines().rev() {
        let f: Vec<&str> = line.split('\t').collect();
        let (kind, path) = match f.as_slice() {
            [k, p, ..] => (*k, PathBuf::from(p)),
            _ => continue,
        };
        let detail = f.get(2).copied().unwrap_or("");
        let step = match kind {
            "created-file" => match fs::read(&path) {
                Ok(b) if sha(&b) == detail => {
                    fs::remove_file(&path).map_err(|e| format!("{}: {e}", path.display()))?;
                    format!("removed {}", path.display())
                }
                Ok(_) => format!("kept {} (changed since apply)", path.display()),
                Err(_) => format!("{} is gone already", path.display()),
            },
            "created-link" => match link_target(&path) {
                Some(to) if norm(&to) == norm(Path::new(detail)) => {
                    remove_link(&path)?;
                    format!("unlinked {}", path.display())
                }
                _ => format!("kept {} (not the link apply made)", path.display()),
            },
            "file" => {
                let bytes = fs::read(backup_dir.join(detail))
                    .map_err(|e| format!("{}: {e}", backup_dir.join(detail).display()))?;
                write_guard(&path)?;
                write_atomic(&path, &bytes)?;
                format!("restored {}", path.display())
            }
            "dir" => {
                if fs::symlink_metadata(&path).is_ok() {
                    format!("kept {} (something is there now)", path.display())
                } else {
                    fs::rename(backup_dir.join(detail), &path)
                        .map_err(|e| format!("{}: {e}", path.display()))?;
                    format!("moved back {}", path.display())
                }
            }
            "link" => {
                if fs::symlink_metadata(&path).is_ok() {
                    format!("kept {} (something is there now)", path.display())
                } else {
                    make_link(&path, Path::new(detail))?;
                    format!("relinked {} to {detail}", path.display())
                }
            }
            other => format!("unknown manifest line `{other}`, skipped"),
        };
        done.push(step);
    }
    Ok(done)
}

/// What an adapter's user-level file holds: the composition sync writes (the GUI shows it line by line).
pub fn user_composed(stacks: &Stacks, adapter: &dyn Adapter) -> Composed {
    compose::compose(stacks, &context(stacks, adapter, adapter.audience(), Layers::Global))
}

/// What an adapter's untracked file in a repo holds, before the tracked-instructions preamble, as written in
/// the place's first checkout (or dir).
pub fn repo_composed(stacks: &Stacks, place: &str, adapter: &dyn Adapter) -> Composed {
    let ctx = place_context(
        stacks,
        adapter,
        place_layers(stacks, place, false),
        first_path(stacks, place).as_deref(),
    );
    compose::compose(stacks, &ctx)
}

/// The repo whose checkout (the first one or any other) is `dir`.
pub fn repo_named_by_checkout(stacks: &Stacks, dir: &Path) -> Option<String> {
    let private = stacks.private.as_ref()?;
    private
        .manifest
        .repos
        .iter()
        .find(|(_, r)| r.checkouts.iter().any(|c| norm(&expand_home(c)) == norm(dir)))
        .map(|(n, _)| n.clone())
}

/// A place's first checkout (a repo) or its dir (a workspace).
pub fn first_path(stacks: &Stacks, place: &str) -> Option<PathBuf> {
    let private = stacks.private.as_ref()?;
    match private.manifest.repos.get(place) {
        Some(repo) => repo.checkouts.first().map(|c| expand_home(c)),
        None => private.manifest.workspaces.get(place).map(|w| expand_home(&w.path)),
    }
}

/// The role a session in a place has when no brief names one (stack.toml `default_role`).
fn place_default_role(stacks: &Stacks, layers: &Layers) -> Option<String> {
    let manifest = &stacks.private.as_ref()?.manifest;
    match layers {
        Layers::Repo { repo, .. } => manifest.repos.get(repo)?.default_role.clone(),
        Layers::Workspace { name, .. } => manifest.workspaces.get(name)?.default_role.clone(),
        Layers::Global => None,
    }
}

/// A place's agent files: read whole, under a byte limit for some harnesses, so intent goes by reference there
/// (orchestrator T1, 25 Oct); the role files in `checkout` carry the text.
fn place_context(
    stacks: &Stacks,
    adapter: &dyn Adapter,
    layers: Layers,
    checkout: Option<&Path>,
) -> Context {
    let default_role = place_default_role(stacks, &layers);
    let mut ctx = context(stacks, adapter, adapter.audience(), layers);
    ctx.by_reference = true;
    ctx.role_dir = checkout.map(|c| c.join(ROLE_DIR));
    ctx.default_role = default_role;
    if adapter.relative_paths() {
        ctx.relative_to = checkout.map(Path::to_path_buf);
    }
    ctx
}

/// The text an adapter's file in `checkout` holds, as sync writes it: the composition, after the checkout's
/// tracked instructions where the adapter carries them (an include, or the text inline).
pub fn place_text(
    stacks: &Stacks,
    layers: Layers,
    adapter: &dyn Adapter,
    checkout: &Path,
) -> (String, Composed) {
    let composed =
        compose::compose(stacks, &place_context(stacks, adapter, layers, Some(checkout)));
    let text = adapter.render(&composed);
    let text = match adapter.repo_preamble(checkout) {
        Some(pre) if !text.is_empty() => compose::with_preamble(&text, &pre),
        _ => text,
    };
    (text, composed)
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
    match plan(stacks, &Options { user_only: true, ..Options::default() }) {
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
            out.extend(shadow_findings(stacks));
            out.extend(verbatim_findings(stacks));
            out.extend(plan.notes.iter().map(|n| Finding::info("sync-note", n.clone())));
            out
        }
        Err(e) => vec![Finding::warning("drift", format!("no sync plan: {e}"))],
    }
}

/// Whether `root` is inside a git checkout and nothing under it is uncommitted: then git holds every byte that
/// an import or a retirement replaces there.
fn clean_git(root: &Path) -> bool {
    git::run(root, &["status", "--porcelain", "--", "."]).is_ok_and(|s| s.trim().is_empty())
}

/// `sync --apply` as one step: import every memory store (one commit each), retire ended temporary entries (one
/// commit each), plan again from the updated stacks, then apply with backups under `backup_dir`. A plan that
/// can't apply stops it before any stack file changes, and import and retire run only on a clean git checkout of
/// their stack (Sol review, finding 6); otherwise they're skipped with a note (ended entries stay out of every
/// file anyway). Returns what changed. The CLI and the GUI both call this.
pub fn import_and_apply(
    public: &Path,
    private: Option<&Path>,
    opts: &Options,
    backup_dir: &Path,
) -> Result<Vec<String>, String> {
    let mut done = Vec::new();
    let stacks = Stacks::load(public, private)?;
    let first = plan(&stacks, opts)?;
    if let Some(c) = first.items.iter().find(|i| i.action == Action::Conflict) {
        return Err(format!(
            "the plan has conflicts, e.g. {}: {}; resolve them first (nothing was changed)",
            c.path.display(),
            c.notes.join("; ")
        ));
    }
    if let Some(p) = &stacks.private {
        if !p.manifest.memory_stores.is_empty() {
            if clean_git(&p.root) {
                for r in crate::ops::import(p, None, true)? {
                    if let Some(c) = r.commit {
                        done.push(format!("imported memory from {}: commit {c}", r.store));
                    }
                }
            } else {
                done.push(format!(
                    "memory import skipped: {} has uncommitted changes or no git",
                    p.root.display()
                ));
            }
        }
    }
    // Ended temporary entries leave the stacks' context for good: retired, with a commit each.
    let stacks = Stacks::load(public, private)?;
    let today = crate::ops::today();
    let ended = crate::temporary::expired(&stacks, &today);
    let dirty: Vec<String> = stacks
        .all()
        .filter(|s| ended.iter().any(|(e, _)| e.stack == s.visibility()) && !clean_git(&s.root))
        .map(|s| s.root.display().to_string())
        .collect();
    if dirty.is_empty() {
        for (id, commit) in crate::ops::retire_expired(&stacks, &today, true)? {
            done.push(format!(
                "retired expired {id}{}",
                commit.map(|c| format!(": commit {c}")).unwrap_or_default()
            ));
        }
    } else {
        done.push(format!(
            "retiring {} ended entr(y/ies) skipped: {} has uncommitted changes or no git; no file composes them",
            ended.len(),
            dirty.join(", ")
        ));
    }
    let stacks = Stacks::load(public, private)?;
    done.extend(apply(&plan(&stacks, opts)?, backup_dir)?);
    Ok(done)
}

/// Where `--apply` keeps this run's backups: `~/.lords-command/backups/<date>-<pid>-<n>`, a dir no earlier run
/// used (Sol review, finding 1: two applies in one process never share one).
pub fn backup_dir() -> Result<PathBuf, String> {
    use std::sync::atomic::{AtomicUsize, Ordering};
    static RUNS: AtomicUsize = AtomicUsize::new(0);
    let home = crate::config::home().ok_or("no home dir")?;
    let root = home.join(".lords-command").join("backups");
    loop {
        let n = RUNS.fetch_add(1, Ordering::SeqCst);
        let dir = root.join(format!("{}-{}-{n}", crate::ops::today(), std::process::id()));
        if !dir.exists() {
            return Ok(dir);
        }
    }
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

/// Worktrees that get no place files of their own (outside `worktree_roots`) but lie inside another place that
/// gets a Claude file: Claude reads that ancestor file and then skips the worktree's own `AGENTS.md`
/// (docs/load-order.md). Info, one per worktree.
pub fn shadow_findings(stacks: &Stacks) -> Vec<crate::Finding> {
    let Some(private) = &stacks.private else { return Vec::new() };
    let claude = |ids: &[String]| ids.is_empty() || ids.iter().any(|a| a == "claude");
    let mut places: Vec<(String, PathBuf)> = Vec::new();
    for (name, w) in private.manifest.workspaces.iter().filter(|(_, w)| claude(&w.adapters)) {
        places.push((format!("workspace {name}"), expand_home(&w.path)));
    }
    let mut skipped_all: Vec<(String, PathBuf)> = Vec::new();
    for (name, repo) in &private.manifest.repos {
        let (paths, skipped) = checkouts(repo);
        if claude(&repo.adapters) {
            places.extend(paths.into_iter().map(|p| (format!("repo {name}"), p)));
        }
        skipped_all.extend(skipped.into_iter().map(|w| (format!("repo {name}"), w)));
    }
    let mut out = Vec::new();
    for (repo, w) in skipped_all {
        if !w.join("AGENTS.md").is_file() {
            continue;
        }
        let wn = norm(&w);
        let ancestor = places.iter().find(|(_, p)| {
            let pn = norm(p);
            wn != pn && wn.starts_with(&format!("{}/", pn.trim_end_matches('/')))
        });
        if let Some((place, p)) = ancestor {
            out.push(
                crate::Finding::info(
                    "agents-shadowed",
                    format!(
                        "{repo}: worktree {}: AGENTS.md shadowed by an ancestor place file ({place}, {})",
                        adapters::slashes(&w),
                        adapters::slashes(&p.join("CLAUDE.local.md"))
                    ),
                )
                .at(w.join("AGENTS.md"), None),
            );
        }
    }
    out
}

/// One file sync writes, measured as written (absolute paths, the first checkout's tracked text).
#[derive(Clone, Debug, Serialize)]
pub struct Size {
    /// "user level", "repo <name>" or "workspace <name>".
    pub place: String,
    /// The adapter's file, or a role file.
    pub file: String,
    pub bytes: usize,
    /// What a session there reads in total: this file plus the adapter's user-level file (0 for user-level
    /// files and role files, which are read on their own).
    pub reads: usize,
    /// The adapter's limit on what it reads, if any.
    pub limit: Option<usize>,
    /// Bytes by part: "tracked AGENTS.md", "header" (markers and the lines before the first heading), then each
    /// heading.
    pub sections: Vec<(String, usize)>,
    /// Whether sync writes it yet (the place's `adapters`); measured either way.
    pub enabled: bool,
}

impl Size {
    /// Past the warning line (`SIZE_WARN_PERCENT` of the limit).
    pub fn over_line(&self) -> bool {
        self.limit.is_some_and(|l| self.reads * 100 > l * SIZE_WARN_PERCENT)
    }
}

/// `text`'s bytes by part; `composed`: the part the stacks composed (the rest is the tracked preamble).
fn sections(text: &str, composed: &str) -> Vec<(String, usize)> {
    let mut out = Vec::new();
    let pre = text.len().saturating_sub(composed.len());
    if pre > 0 {
        out.push(("tracked AGENTS.md".to_string(), pre));
    }
    let mut name = "header".to_string();
    let mut n = 0;
    for line in composed.split_inclusive('\n') {
        if let Some(h) = line.strip_prefix("# ") {
            out.push((std::mem::replace(&mut name, h.trim().to_string()), n));
            n = 0;
        }
        n += line.len();
    }
    out.push((name, n));
    out
}

/// Every file sync writes for agents, measured: each adapter's user-level file, each place's file per adapter,
/// and each place's role files.
pub fn sizes(stacks: &Stacks) -> Vec<Size> {
    let Some(private) = &stacks.private else { return Vec::new() };
    let mut out = Vec::new();
    let mut user = std::collections::BTreeMap::new();
    for adapter in adapters::ALL.iter().copied() {
        let text = adapter.render(&user_composed(stacks, adapter));
        user.insert(adapter.id(), text.len());
        out.push(Size {
            place: "user level".into(),
            file: adapter.id().to_string(),
            bytes: text.len(),
            reads: text.len(),
            limit: adapter.byte_limit(),
            sections: sections(&text, &text),
            enabled: true,
        });
    }
    let mut places: Vec<(String, String, &[String])> = Vec::new();
    for (name, repo) in &private.manifest.repos {
        places.push((format!("repo {name}"), name.clone(), &repo.adapters));
    }
    for (name, w) in &private.manifest.workspaces {
        places.push((format!("workspace {name}"), name.clone(), &w.adapters));
    }
    for (label, name, ids) in places {
        let Some(dir) = first_path(stacks, &name) else { continue };
        // Every adapter is measured, also one the place doesn't get yet: the line holds before it is switched on.
        for adapter in adapters::ALL.iter().copied() {
            let enabled = ids.is_empty() || ids.iter().any(|a| a == adapter.id());
            let (text, composed) =
                place_text(stacks, place_layers(stacks, &name, false), adapter, &dir);
            let rendered = adapter.render(&composed);
            out.push(Size {
                place: label.clone(),
                file: adapter.repo_file().to_string(),
                bytes: text.len(),
                reads: text.len() + user[adapter.id()],
                limit: adapter.byte_limit(),
                sections: sections(&text, &rendered),
                enabled,
            });
        }
        for role in declared_roles(stacks) {
            let text = role_composed(stacks, place_layers(stacks, &name, true), &role).text;
            out.push(Size {
                place: label.clone(),
                file: format!("{ROLE_DIR}/{}", role_file(&role)),
                bytes: text.len(),
                reads: 0,
                limit: None,
                sections: sections(&text, &text),
                enabled: true,
            });
        }
    }
    out
}

/// What one session in a place reads together (the user-level text, the place's own, and the tracked
/// instructions it carries), against each adapter's limit.
fn repo_size_findings(stacks: &Stacks) -> Vec<crate::Finding> {
    sizes(stacks)
        .into_iter()
        .filter(|s| s.reads > s.bytes && s.over_line())
        .map(|s| {
            crate::Finding::warning(
                "size",
                format!(
                    "{}: {} reads {} bytes there ({} its file, {} user level); its limit is {}",
                    s.place,
                    s.file,
                    s.reads,
                    s.bytes,
                    s.reads - s.bytes,
                    s.limit.unwrap_or(0)
                ),
            )
        })
        .collect()
}
