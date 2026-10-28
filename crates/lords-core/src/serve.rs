//! The agent API of v2 (design §2.1, §2.2): brief, context, search and remember over the selected stores. The CLI
//! and the MCP server only print what these return, so both give the same ids and bodies.
//!
//! A session is bound once, by `brief`: it resolves the session's working dir to a project (identity.rs), records a
//! delivery id for that binding in the journal, and every later call names that id. A call can't pick another
//! project by changing a field: the binding comes from the journal, and is resolved again on each call, so a
//! registry change that moves the dir to another project refuses the old id instead of serving the new project.

use crate::compose::{self, Layers};
use crate::fsguard::{self, Part, Root};
use crate::identity::{self, Binding, Place, Registry};
use crate::model::{Audience, Delivery, Entry, Kind, Lifecycle};
use crate::stack::{self, Stack, Stacks};
use serde::Serialize;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

/// The roles that may write a project's shared memory (design §2.1: "Helpers/reviewers report lessons instead of
/// using a shared-store write"; product runs write none).
pub const WRITERS: [&str; 2] = ["orchestrator", "coder"];

/// The context's byte budget when a call names none.
pub const DEFAULT_BUDGET: usize = 24_000;

/// Who is starting, as the hook or launcher knows it (design §2.3, the expected-start key). Only `cwd` and `role`
/// select anything; the rest is recorded.
#[derive(Clone, Debug, Default)]
pub struct Start {
    pub cwd: PathBuf,
    pub role: String,
    pub harness: String,
    pub profile: String,
    pub session: String,
    pub agent: String,
    pub hint: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct Brief {
    pub delivery_id: String,
    pub project: Option<String>,
    pub role: String,
    pub text: String,
    pub revisions: Vec<Revision>,
}

/// A store as it was read: its dir, its git commit (none outside git) and a hash of what was loaded.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Revision {
    pub store: String,
    pub commit: Option<String>,
    pub content: String,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Item {
    pub id: String,
    pub kind: String,
    pub title: String,
    /// "review due since DATE" and the like; none for an entry that simply holds.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub body: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct Context {
    pub delivery_id: String,
    pub project: Option<String>,
    pub role: String,
    /// The role's duties, with their text.
    pub duties: Vec<Item>,
    /// Everything else that applies here, to fetch by id with `search`.
    pub index: Vec<Item>,
    /// Duties left out for the budget: fetch them with `search` by id. Never dropped silently.
    pub omitted: Vec<String>,
    pub revisions: Vec<Revision>,
    pub bytes: usize,
    /// The next date this context changes on its own (an entry ends or falls due): refresh before acting past it.
    pub valid_until: Option<String>,
    /// Problems the selected stores have (load and placement errors).
    pub problems: Vec<String>,
}

/// One bound session, read from the journal.
#[derive(Clone, Debug)]
struct Bound {
    id: String,
    cwd: PathBuf,
    role: String,
    project: Option<String>,
    audience: Vec<Audience>,
}

/// What a working dir sees: its binding and the stores it selects.
struct View {
    binding: Binding,
    stacks: Stacks,
}

fn audience_of(harness: &str) -> Vec<Audience> {
    match harness {
        "claude" => vec![Audience::Opus],
        "codex" => vec![Audience::Sol, Audience::Astra],
        _ => vec![Audience::Opus, Audience::Sol, Audience::Astra],
    }
}

fn project_of(b: &Binding) -> Option<String> {
    match b {
        Binding::Project { project, .. } => Some(project.clone()),
        Binding::CoreOnly { .. } => None,
    }
}

/// The selected stores for a binding: the core's two global layers, and the project's store where there is one. A
/// project store that can't be loaded leaves the core alone, with why (design §2.1: "Missing selected store is a
/// visible error with core-only fallback"); a core that can't be loaded is an error.
fn view(reg: &Registry, binding: Binding) -> Result<View, String> {
    let core = reg.core.as_ref().ok_or("no core registered: run `lords register --core` first")?;
    let Binding::Project { project, store, cwd, .. } = &binding else {
        let stacks = Stacks::selected(&core.public, core.private.as_deref(), None)?;
        return Ok(View { binding, stacks });
    };
    // The loader takes the one decision whether this store may serve this project (Sol fix check 1, F1).
    let loaded = Stack::load_store(store, project)
        .and_then(|s| Stacks::selected(&core.public, core.private.as_deref(), Some(s)));
    match loaded {
        Ok(stacks) => Ok(View { binding, stacks }),
        Err(e) => {
            let stacks = Stacks::selected(&core.public, core.private.as_deref(), None)?;
            let reason = format!("project {project}'s store can't be served: {e}");
            Ok(View { binding: Binding::CoreOnly { cwd: Some(cwd.clone()), reason }, stacks })
        }
    }
}

/// The layers a binding composes: its project and place (in a repo, with the path entries above the working dir),
/// and every global entry (the reader has no user file that carries them). Outside any project, the global layer
/// alone: a repo name no stack can hold selects none.
fn layers(b: &Binding) -> Layers {
    match b {
        Binding::Project { project, place: Place::Repo { name, toplevel }, cwd, .. } => {
            Layers::Repo {
                repo: name.clone(),
                project: Some(project.clone()),
                sub: cwd.strip_prefix(toplevel).ok().and_then(|rel| {
                    let parts: Vec<String> = rel
                        .components()
                        .map(|c| c.as_os_str().to_string_lossy().into_owned())
                        .collect();
                    (!parts.is_empty()).then(|| parts.join("/"))
                }),
                global: true,
            }
        }
        Binding::Project { project, place: Place::Workspace { name }, .. } => {
            Layers::Workspace { name: name.clone(), project: Some(project.clone()), global: true }
        }
        Binding::CoreOnly { .. } => {
            Layers::Repo { repo: String::new(), project: None, sub: None, global: true }
        }
    }
}

fn revisions(stacks: &Stacks) -> Vec<Revision> {
    stacks
        .all()
        .map(|s| {
            let mut h = Sha256::new();
            for e in &s.entries {
                h.update(e.id().as_bytes());
                h.update([0]);
                h.update(serde_json::to_vec(&e.meta).unwrap_or_default());
                h.update([0]);
                h.update(e.body.as_bytes());
                h.update([0]);
            }
            Revision {
                store: s.root.display().to_string(),
                commit: crate::git::run(&s.root, &["rev-parse", "HEAD"])
                    .ok()
                    .map(|c| c.trim().to_string()),
                content: format!("{:x}", h.finalize()),
            }
        })
        .collect()
}

fn new_delivery_id(start: &Start) -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNT: AtomicU64 = AtomicU64::new(0);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let mut h = Sha256::new();
    h.update(format!(
        "{nanos}\0{}\0{}\0{}\0{}\0{}",
        crate::process::id(),
        COUNT.fetch_add(1, Ordering::Relaxed),
        start.cwd.display(),
        start.session,
        start.agent
    ));
    format!("d-{}", &format!("{:x}", h.finalize())[..20])
}

/// Binds a starting session and returns its brief (design §2.2): which project it is in, its delivery id, and how
/// to fetch its context. The delivery is recorded only after the brief is rendered.
pub fn brief(home: &Path, start: &Start) -> Result<Brief, String> {
    if start.role.trim().is_empty() {
        return Err("a brief needs the session's role".into());
    }
    let reg = Registry::load(home)?;
    let binding = identity::resolve(&reg, &start.cwd, start.hint.as_deref());
    let v = view(&reg, binding)?;
    let delivery_id = new_delivery_id(start);
    let project = project_of(&v.binding);
    let role = start.role.trim().to_string();
    let here = match &v.binding {
        Binding::Project { project, .. } => format!("project {project}"),
        Binding::CoreOnly { reason, .. } => format!("no project ({reason}): core only, no writes"),
    };
    let text = format!(
        "Lords: {here}; role {role}; delivery {delivery_id}.\nBefore the first task action, call lords.context \
         with role \"{role}\" and delivery id \"{delivery_id}\". If MCP fails: `lords context --role {role} \
         --delivery {delivery_id}`; if that fails too, tell your lead before project work."
    );
    let revisions = revisions(&v.stacks);
    let cwd = match &v.binding {
        Binding::Project { cwd, .. } => cwd.clone(),
        Binding::CoreOnly { cwd, .. } => cwd.clone().unwrap_or_else(|| start.cwd.clone()),
    };
    crate::journal::append(
        home,
        "brief",
        json!({
            "delivery_id": delivery_id,
            "key": {
                "harness": start.harness, "profile": start.profile, "session": start.session,
                "agent": start.agent, "role": role,
            },
            "cwd": cwd,
            "project": project,
            "revisions": revisions,
            "payload_sha256": fsguard::sha(text.as_bytes()),
            "bytes": text.len(),
        }),
    )?;
    // A brief is usable only if its delivery can be read back (Sol review r1, F4): never report one that can't.
    bound(home, &delivery_id)?;
    Ok(Brief { delivery_id, project, role, text, revisions })
}

/// The delivery `id` names, as `brief` recorded it.
fn bound(home: &Path, id: &str) -> Result<Bound, String> {
    let ev = crate::journal::read(home)?
        .into_iter()
        .find(|e| e["event"] == "brief" && e["delivery_id"] == id)
        .ok_or_else(|| format!("unknown delivery id `{id}`: run `lords brief` for this session"))?;
    let text = |v: &Value| v.as_str().map(str::to_string);
    Ok(Bound {
        id: id.to_string(),
        cwd: text(&ev["cwd"]).map(PathBuf::from).ok_or("the delivery has no cwd")?,
        role: text(&ev["key"]["role"]).ok_or("the delivery has no role")?,
        project: text(&ev["project"]),
        audience: audience_of(ev["key"]["harness"].as_str().unwrap_or_default()),
    })
}

/// The view a delivery selects, resolved again now: it must still be the delivery's project.
fn rebind(home: &Path, d: &Bound) -> Result<View, String> {
    let reg = Registry::load(home)?;
    let v = view(&reg, identity::resolve(&reg, &d.cwd, None))?;
    if project_of(&v.binding) != d.project {
        let why = match &v.binding {
            Binding::CoreOnly { reason, .. } => format!(" ({reason})"),
            Binding::Project { .. } => String::new(),
        };
        return Err(format!(
            "delivery {} was for {}, but its dir now resolves to {}{why}: run `lords brief` again",
            d.id,
            d.project.as_deref().unwrap_or("no project"),
            project_of(&v.binding).as_deref().unwrap_or("no project")
        ));
    }
    Ok(v)
}

/// An ended entry says why; a live temporary one carries the clause composed text has (temporary.rs owns it).
fn label(e: &Entry, l: &Lifecycle, stacks: &Stacks, as_of: &str) -> Option<String> {
    match l {
        Lifecycle::Expired { why } => Some(format!("expired: {why}")),
        Lifecycle::Invalid { why } => Some(format!("invalid: {why}")),
        _ => crate::temporary::label(e, stacks, as_of),
    }
}

fn item(e: &Entry, label: Option<String>, body: bool) -> Item {
    Item {
        id: e.id().to_string(),
        kind: e.meta.kind.name().to_string(),
        title: e.meta.title.clone(),
        label,
        body: body.then(|| e.body.clone()),
    }
}

/// A role's duty: its role and rule entries, and anything marked `delivery: always`.
fn is_duty(e: &Entry, role: &str) -> bool {
    e.meta.delivery == Delivery::Always
        || (matches!(e.meta.kind, Kind::Role | Kind::Rule)
            && e.meta.roles.iter().any(|r| r == role))
}

/// Entries that apply to the binding for `roles` (None: any role), with their lifecycle at `as_of`; ended and
/// invalid ones only with `ended`.
fn applicable<'a>(
    v: &'a View,
    audience: &[Audience],
    roles: Option<Vec<String>>,
    as_of: &str,
    ended: bool,
) -> Vec<(&'a Entry, Lifecycle)> {
    let ctx = compose::Context {
        audience: audience.to_vec(),
        layers: layers(&v.binding),
        public_only: false,
        skip_stores: Vec::new(),
        roles,
        by_reference: false,
        role_dir: None,
        relative_to: None,
        default_role: None,
        default_role_inline: false,
    };
    let judged: Vec<(&Entry, Lifecycle)> = compose::candidates(&v.stacks, &ctx)
        .into_iter()
        .map(|e| (e, crate::temporary::lifecycle(e, &v.stacks, as_of)))
        .collect();
    let holds = |l: &Lifecycle| matches!(l, Lifecycle::Active | Lifecycle::ReviewDue { .. });
    // Only entries that hold override others, as the composer has it.
    let (live, _) = compose::without_overridden(
        judged.iter().filter(|(_, l)| holds(l)).map(|(e, _)| *e).collect(),
    );
    judged
        .into_iter()
        .filter(|(e, l)| match l {
            Lifecycle::Active | Lifecycle::ReviewDue { .. } => {
                live.iter().any(|x| std::ptr::eq(*x, *e))
            }
            Lifecycle::Expired { .. } | Lifecycle::Invalid { .. } => ended,
            Lifecycle::Retired => false,
        })
        .collect()
}

fn problems(stacks: &Stacks) -> Vec<String> {
    stacks.all().flat_map(|s| s.problems.iter().map(|p| p.to_string())).collect()
}

/// A role's context for a delivery (design §2.1): its duties with their text within `budget` bytes, the index of
/// everything else here, the store revisions and the date it is valid until.
pub fn context(
    home: &Path,
    delivery_id: &str,
    role: &str,
    budget: Option<usize>,
    as_of: &str,
) -> Result<Context, String> {
    let d = bound(home, delivery_id)?;
    if d.role != role {
        return Err(format!(
            "delivery {delivery_id} was issued for role {}, not {role}: run `lords brief` for this role",
            d.role
        ));
    }
    let v = rebind(home, &d)?;
    let budget = budget.unwrap_or(DEFAULT_BUDGET);
    let (mut duties, mut index, mut omitted, mut used) = (Vec::new(), Vec::new(), Vec::new(), 0);
    let mut valid_until: Option<String> = None;
    for (e, l) in applicable(&v, &d.audience, Some(vec![role.to_string()]), as_of, false) {
        if let Some(b) = e.next_boundary(as_of) {
            if valid_until.as_ref().is_none_or(|v| b < *v) {
                valid_until = Some(b);
            }
        }
        if is_duty(e, role) {
            let it = item(e, label(e, &l, &v.stacks, as_of), true);
            let size = it.body.as_ref().map_or(0, String::len) + it.title.len();
            if used + size <= budget {
                used += size;
                duties.push(it);
                continue;
            }
            omitted.push(it.id);
        }
        let it = item(e, label(e, &l, &v.stacks, as_of), false);
        used += it.title.len() + it.id.len();
        index.push(it);
    }
    Ok(Context {
        delivery_id: d.id,
        project: d.project,
        role: d.role,
        duties,
        index,
        omitted,
        revisions: revisions(&v.stacks),
        bytes: used,
        valid_until,
        problems: problems(&v.stacks),
    })
}

/// Entries of the delivery's binding, its role included as in `context`, that match `query` (in id, title, topics or text, any case) or are named in
/// `ids`, with their text. Expired and invalid entries only with `include_expired`; retired ones never.
pub fn search(
    home: &Path,
    delivery_id: &str,
    query: &str,
    ids: &[String],
    include_expired: bool,
    as_of: &str,
) -> Result<Vec<Item>, String> {
    let d = bound(home, delivery_id)?;
    let v = rebind(home, &d)?;
    let q = query.trim().to_lowercase();
    if q.is_empty() && ids.is_empty() {
        return Err("search needs a query or ids".into());
    }
    let hit = |e: &Entry| {
        ids.iter().any(|i| i == e.id())
            || (!q.is_empty()
                && (e.id().to_lowercase().contains(&q)
                    || e.meta.title.to_lowercase().contains(&q)
                    || e.meta.topics.iter().any(|t| t.to_lowercase() == q)
                    || e.body.to_lowercase().contains(&q)))
    };
    Ok(applicable(&v, &d.audience, Some(vec![d.role.clone()]), as_of, include_expired)
        .into_iter()
        .filter(|(e, _)| hit(e))
        .map(|(e, l)| item(e, label(e, &l, &v.stacks, as_of), true))
        .collect())
}

/// What `remember` writes.
#[derive(Clone, Debug, Default)]
pub struct Note {
    pub text: String,
    pub title: Option<String>,
    /// `memory` (default) or `fact`.
    pub kind: Option<String>,
    pub until: Option<String>,
    pub review_by: Option<String>,
    pub reason: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct Remembered {
    pub id: String,
    /// The file, relative to the store.
    pub file: String,
    pub sha256: String,
    pub label: Option<String>,
}

const STORE_LOCK: &str = ".lords.lock";
const REMEMBERED: &str = "remembered";

/// Writes a note to the delivery's project store (design §4, "remember"; atomic write): only in a project, only
/// for a writing role, only a memory or fact, and a temporary note with a reason and a valid date that hasn't
/// passed. Under the store's lock the candidate is checked as an entry by the same schema and model, then put in
/// place whole, never over another file.
pub fn remember(
    home: &Path,
    delivery_id: &str,
    note: &Note,
    as_of: &str,
) -> Result<Remembered, String> {
    let d = bound(home, delivery_id)?;
    if !WRITERS.contains(&d.role.as_str()) {
        return Err(format!(
            "role {} doesn't write shared memory: report the lesson to your lead",
            d.role
        ));
    }
    let v = rebind(home, &d)?;
    let (project, store) = match &v.binding {
        Binding::Project { project, store, .. } => (project.clone(), store.clone()),
        Binding::CoreOnly { reason, .. } => {
            return Err(format!("no project here ({reason}): nothing can be remembered"))
        }
    };
    let kind = match note.kind.as_deref().unwrap_or("memory") {
        "memory" => Kind::Memory,
        "fact" => Kind::Fact,
        other => return Err(format!("remember writes a memory or a fact, not a {other}")),
    };
    let text = note.text.trim();
    if text.is_empty() {
        return Err("nothing to remember: the text is empty".into());
    }
    let title = note
        .title
        .as_deref()
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .unwrap_or_else(|| text.lines().next().unwrap_or(text))
        .chars()
        .take(100)
        .collect::<String>();
    let base =
        stack::slug(&title).chars().take(60).collect::<String>().trim_matches('-').to_string();
    let base = if stack::is_slug(&base) { base } else { "note".to_string() };
    let mut meta = json!({
        "id": format!("{}.{base}", kind.name()), "kind": kind.name(), "title": title, "audience": ["all"],
        "scope": format!("project:{project}"), "status": "active", "visibility": "private",
        "origin": { "type": "own" },
        "metadata": { "remembered": { "delivery": d.id, "role": d.role, "at": crate::ops::now() } },
    });
    let temporary: serde_json::Map<String, Value> =
        [("until", &note.until), ("review_by", &note.review_by), ("reason", &note.reason)]
            .into_iter()
            .filter_map(|(k, v)| v.as_ref().map(|v| (k.to_string(), json!(v))))
            .collect();
    if !temporary.is_empty() {
        meta["temporary"] = Value::Object(temporary);
    }
    // The whole candidate is judged before anything is touched: the schema, then the model's lifecycle on `as_of`.
    let dir = [
        Part::new("project")?,
        Part::new(&project)?,
        Part::new(kind.dir())?,
        Part::new(REMEMBERED)?,
    ];
    let root = Root::stack(&store)?;
    let entry = candidate(&meta, text, root.join(&dir).path())?;
    let life = entry.lifecycle(as_of, &|id| v.stacks.find(id));
    match &life {
        Lifecycle::Invalid { why } => return Err(format!("not remembered: {why}")),
        Lifecycle::Expired { why } => {
            return Err(format!("not remembered: it has already ended ({why})"))
        }
        _ => {}
    }
    // Under the store's lock: the first free id, then the file put in place whole.
    let _held = fsguard::lock(&root.join(&[Part::new(STORE_LOCK)?]))?;
    let fresh = Stack::load_store(&store, &project).map_err(|e| format!("not remembered: {e}"))?;
    for n in 1.. {
        let slug = if n == 1 { base.clone() } else { format!("{base}-{n}") };
        let id = format!("{}.{slug}", kind.name());
        let file = root.join(&dir).child(Part::new(&format!("{slug}.md"))?);
        let taken = fresh.find(&id).is_some() || v.stacks.find(&id).is_some();
        if taken || fsguard::kind_here(file.path())? != fsguard::Kind::Missing {
            continue;
        }
        meta["id"] = json!(id);
        let yaml = serde_yaml_ng::to_string(&meta).map_err(|e| e.to_string())?;
        let bytes = format!(
            "---
{yaml}---
{text}
"
        );
        fsguard::create_dir_all(&root.join(&dir))?;
        fsguard::put_file(&file, bytes.as_bytes())?;
        let sha256 = fsguard::sha(bytes.as_bytes());
        crate::journal::append(
            home,
            "remember",
            json!({ "delivery_id": d.id, "project": project, "id": id, "sha256": sha256 }),
        )?;
        return Ok(Remembered {
            id,
            file: format!("project/{project}/{}/{REMEMBERED}/{slug}.md", kind.dir()),
            sha256,
            label: label(&entry, &life, &v.stacks, as_of),
        });
    }
    unreachable!("the loop returns or tries the next name")
}

/// The note as the loader would read it: the entry schema, then the model.
fn candidate(meta: &Value, body: &str, file: &Path) -> Result<Entry, String> {
    let errs = crate::schema::entry_errors(meta);
    if !errs.is_empty() {
        return Err(format!("not remembered: {}", errs.join("; ")));
    }
    let m: crate::model::Meta = serde_json::from_value(meta.clone()).map_err(|e| e.to_string())?;
    let scope = crate::model::Scope::parse(&m.scope).ok_or("bad scope")?;
    Ok(Entry {
        meta: m,
        scope,
        stack: crate::model::Visibility::Private,
        meta_file: file.to_path_buf(),
        body_file: file.to_path_buf(),
        body: body.to_string(),
        body_line: 1,
        files: vec![file.to_path_buf()],
    })
}
