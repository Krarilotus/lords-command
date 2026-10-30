//! `sync` (design: Sync, Ownership boundary): a plan of what each target would become, as diffs; `apply` carries a
//! plan out. Sync changes only paths its ledger proves are the tool's (replaced whole) and the owner files it edits
//! minimally (backed up); every filesystem change goes through `fsguard`. Planning only reads.
//!
//! Harness-neutral: each target names an adapter (adapters.rs), which says which agents it reaches and which
//! untracked file it reads in a repo. Those facts are checked in docs/load-order.md.

use crate::adapters::{self, Adapter};
use crate::compose::{self, Composed, Context, Layers, Source};
use crate::config::expand_home;
use crate::diff;
use crate::fsguard::{self, Contained, OwnerFile, Part, Root, Tree};
use crate::git;
use crate::ledger::{self, Ledger};
use crate::model::{Audience, Entry, Kind};
use crate::stack::{self, Stack, Stacks, Target};
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Action {
    /// A file where nothing is.
    Create,
    /// A file changes: a tool-owned one is replaced whole, an owner file gets its one managed edit (backed up).
    Update,
    InSync,
    /// A tool-owned dir (a skill copy, `.lords/`) where nothing is.
    Install,
    /// A tool-owned dir that still matches its ledger record, replaced whole by its new version.
    Refresh,
    /// A tool-owned path the stack no longer wants, still matching its ledger record: deleted, following nothing.
    Remove,
    /// A ledger record whose path is gone already (its worktree removed, say): the record goes.
    Forget,
    /// A temporary entry that has ended: `--apply` retires it in its stack (with a commit) before writing.
    RetireEntry,
    /// Shown, never written (a tracked file, or a path that is no longer the tool's).
    Report,
    /// Can't proceed; the reason is in the notes.
    Conflict,
}

/// Whose a path is (design: Ownership boundary).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Class {
    /// Created by sync, recorded in the ledger, replaced whole and never edited.
    Tool,
    /// The owner's file: sync changes only its managed block or one key, with a backup.
    Owner,
}

#[derive(Clone, Debug, Serialize)]
pub struct Item {
    pub target: String,
    pub path: PathBuf,
    pub action: Action,
    pub class: Class,
    /// The ledger scope of a tool-owned path (`target:<name>`, `repo:<name>`, `repo:<name>/<adapter>`, …).
    #[serde(skip_serializing_if = "String::is_empty")]
    pub scope: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub current: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub proposed: Option<String>,
    /// A tool-owned dir's contents, as it will be built.
    #[serde(skip)]
    pub tree: Option<std::sync::Arc<Tree>>,
    /// The hash of what is there now (a tool-owned path: as the ledger records it).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub have: Option<String>,
    /// The hash of what sync will put there.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub want: Option<String>,
    /// A path that isn't in the ledger but holds byte for byte what sync would put there: `lords adopt` records it.
    pub adoptable: bool,
    pub diff: String,
    pub notes: Vec<String>,
    #[serde(skip)]
    pub sources: Vec<Source>,
    /// The target the path lies in, as planned: an owner file is its own; a tool-owned path lies in its scope's
    /// target dir, checkout or workspace. Apply checks after each action that the path is still inside it, with no
    /// link on the way (Sol review r3, R3-05).
    #[serde(skip)]
    pub root: PathBuf,
    /// Where apply acts (design-simplification-r6, rule A): a tool-owned place below a verified root, or an owner
    /// file the config names. An item without one changes nothing on disk.
    #[serde(skip)]
    pub(crate) dest: Dest,
    /// The places the item belongs to, as planning made it there (Sol review r7, R7-03): a user target (its shared
    /// `via` file too), a repo or a workspace. Never read back from a label.
    #[serde(skip)]
    pub(crate) units: Vec<Unit>,
    /// Why an item is a conflict, by type (r7 fix check, FC-01): only `ModifiedCopy` leaves the view complete.
    #[serde(skip)]
    pub(crate) conflict: ConflictKind,
}

/// The kind of a conflict, set where it is made: anything but a copy the owner changed may hide what is wanted.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum ConflictKind {
    /// A tool-owned copy sync planned in full that no longer matches its ledger record.
    ModifiedCopy,
    /// A place's file past its harness's limit, which would be cut: the install fails (orchestrator, 27 Oct).
    OverLimit,
    /// Every other conflict: a place that is missing or can't be read, a path that isn't the tool's, a config clash.
    #[default]
    Other,
}

/// Where an item acts, as fsguard's changes take it.
#[derive(Clone, Debug, Default)]
pub(crate) enum Dest {
    #[default]
    None,
    Tool(Contained),
    Owner(OwnerFile),
}

/// A place a plan is made of, as its config declares it: what the install stage skips whole for a conflict.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Unit {
    /// The user-level target of this name.
    Target(String),
    Repo(String),
    Workspace(String),
}

impl std::fmt::Display for Unit {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Unit::Target(n) => write!(f, "target {n}"),
            Unit::Repo(n) => write!(f, "repo {n}"),
            Unit::Workspace(n) => write!(f, "workspace {n}"),
        }
    }
}

/// What an item changes, by its type (Sol review r7, R7-02): the install stage keeps only the first two.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Change {
    /// A tool-owned path below a verified root.
    ToolOwned,
    /// A checkout's git exclude file (`OwnerKind::Exclude`).
    ExcludeEdit,
    /// Any other owner file.
    OwnerEdit,
    /// Nothing on disk.
    Nothing,
}

/// Proof of a complete view (design-simplification-r6, rule B; Sol review r7, R7-04/R7-05): the stacks loaded with no
/// problem of any level and the plan saw every place it declares. Only planning makes one (a private field), and
/// only with one does a plan remove or forget anything.
#[derive(Clone, Debug)]
pub struct Complete(());

/// What an apply may do (design-simplification-r6, staged first use).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Stage {
    /// Everything a plan holds.
    #[default]
    Full,
    /// Only tool-owned paths (create, install, update, refresh) and each repo's git-exclude lines; no removal, no
    /// other owner file, no import or retirement; a place with a conflict is skipped whole.
    Install,
}

impl Item {
    fn new(target: &str, path: PathBuf, action: Action) -> Item {
        Item {
            target: target.to_string(),
            root: path.clone(),
            dest: Dest::None,
            path,
            action,
            class: Class::Owner,
            scope: String::new(),
            current: None,
            proposed: None,
            tree: None,
            have: None,
            want: None,
            adoptable: false,
            diff: String::new(),
            notes: Vec::new(),
            sources: Vec::new(),
            units: Vec::new(),
            conflict: ConflictKind::Other,
        }
    }

    pub fn changes(&self) -> bool {
        !matches!(self.action, Action::InSync | Action::Report)
    }

    /// What the item changes, from where it acts.
    pub(crate) fn change(&self) -> Change {
        match &self.dest {
            Dest::Tool(_) => Change::ToolOwned,
            Dest::Owner(o) if o.kind() == fsguard::OwnerKind::Exclude => Change::ExcludeEdit,
            Dest::Owner(_) => Change::OwnerEdit,
            Dest::None => Change::Nothing,
        }
    }
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct Plan {
    pub items: Vec<Item>,
    pub notes: Vec<String>,
    /// The tool's own dir the plan was made for (its ledger, backups and lock).
    #[serde(skip)]
    pub home: PathBuf,
    /// The ledger's revision when planned: apply refuses a plan made against another ledger.
    #[serde(skip)]
    pub ledger_revision: String,
    /// Every root the plan verified: `set-aside` takes a path only within one (design review, point 2).
    #[serde(skip)]
    pub roots: Vec<Root>,
    /// Every owner file the config names now: `restore` re-derives each manifest line against them (point 3).
    #[serde(skip)]
    pub owner_files: Vec<OwnerFile>,
    /// Present only with a complete view (rule B, `Complete`).
    #[serde(skip)]
    pub complete: Option<Complete>,
    /// A place's file isn't written because its harness would cut it, so the sync that planned it fails. Set
    /// before the install stage drops a blocked place's items.
    #[serde(skip)]
    pub over_limit: bool,
}

#[derive(Clone, Debug, Default)]
pub struct Options {
    /// Only this repo's files (and no user-level targets). `None`: user-level targets and every repo.
    pub repo: Option<String>,
    /// Only user-level targets.
    pub user_only: bool,
    /// Only this adapter's files (its targets, and its file and skill copies in each place); role files, which
    /// every adapter's agents read, are always planned. `None`: every adapter.
    pub adapter: Option<String>,
    /// With `repo`: only this one checkout or worktree of it (a new worktree's files, from its post-checkout hook).
    pub place: Option<PathBuf>,
    /// The tool's own dir: its ledger, backups and lock (`default_home()` on a machine; a temp dir in tests).
    /// Planning refuses an empty one, so nothing ever reads or writes a home it wasn't given.
    pub home: PathBuf,
    /// Plan the removal of every tool-owned path in scope that still matches its ledger record (`lords uninstall`).
    pub uninstall: bool,
    /// What the apply may do: everything, or the install stage only.
    pub stage: Stage,
}

/// `~/.lords-command`, the tool's own dir on this machine.
pub fn default_home() -> Result<PathBuf, String> {
    Ok(crate::config::home().ok_or("no home dir")?.join(".lords-command"))
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

/// The text of a file sync only shows (the tracked `AGENTS.md` it reports, the lock's holder), `None` on any
/// error: for display only, never a decision (Sol review r5, R5-04's class; the inventory names each caller).
fn read_lossy(path: &Path) -> Option<String> {
    fsguard::read_text(path).ok().flatten()
}

/// A file's text for a check: `None` when it isn't there; an error is a finding, never taken for a missing file.
fn read_or_finding(path: &Path, out: &mut Vec<crate::Finding>) -> Option<String> {
    match fsguard::read_text(path) {
        Ok(t) => t,
        Err(e) => {
            out.push(crate::Finding::error("unreadable", e).at(path, None));
            None
        }
    }
}

fn conflict(target: &str, path: &Path, note: impl Into<String>) -> Item {
    let mut item = Item::new(target, path.to_path_buf(), Action::Conflict);
    item.notes.push(note.into());
    item
}

/// A tool-owned copy the owner changed since sync wrote it: the one conflict that hides nothing wanted (FC-01).
fn modified_copy(target: &str, path: &Path, note: &str) -> Item {
    let mut item = conflict(target, path, note);
    item.conflict = ConflictKind::ModifiedCopy;
    item
}

fn display(p: &Path) -> String {
    p.display().to_string()
}

fn sources_into(item: &mut Item, composed: &Composed) {
    item.sources = composed.sources.clone();
    if !composed.overridden.is_empty() {
        item.notes.push(format!("overridden here: {}", composed.overridden.join(", ")));
    }
}

/// The item that gives an owner file `proposed` (or says it already holds it). The owner's file: sync makes this one
/// edit with a backup and nothing else.
fn owner_file_item(
    target: &str,
    path: &Path,
    dest: Dest,
    proposed: String,
    composed: &Composed,
) -> Item {
    let current = match fsguard::read_text(path) {
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
    item.dest = dest;
    item.diff = diff::unified(
        current.as_deref().unwrap_or(""),
        &proposed,
        &display(path),
        &display(path),
        3,
    );
    sources_into(&mut item, composed);
    item.have = current.as_deref().map(|c| fsguard::sha(c.as_bytes()));
    item.want = Some(fsguard::sha(proposed.as_bytes()));
    item.current = current;
    item.proposed = Some(proposed);
    item
}

/// The item that puts `block` into an owner file, keeping the owner's text around it (or replacing it).
fn owner_block_item(
    target: &str,
    owner: &OwnerFile,
    block: &str,
    replace: bool,
    composed: &Composed,
) -> Item {
    let path = owner.path();
    let current = match fsguard::read_text(path) {
        Ok(c) => c.unwrap_or_default(),
        Err(e) => return conflict(target, path, e),
    };
    match compose::place_block(&current, block, replace) {
        Ok(proposed) => {
            owner_file_item(target, path, Dest::Owner(owner.clone()), proposed, composed)
        }
        Err(e) => conflict(target, path, e),
    }
}

/// A stack skill's files as a copy holds them: its plain files, without the sidecar, plus a `.gitignore` of `*` so
/// the copy keeps itself out of git. A link anywhere in the skill's dir is an error: a copy would silently lack what
/// it points at (Sol review r2, R2-23).
pub fn skill_tree(src: &Path) -> Result<Tree, String> {
    let mut tree = fsguard::read_tree(src)?;
    tree.remove(stack::SIDECAR);
    tree.insert(".gitignore".into(), b"*\n".to_vec());
    Ok(tree)
}

/// What planning carries besides the stacks: the ledger, which tool-owned paths the plan wants, and which ledger
/// scopes it covers (only those can lose a path the plan no longer wants).
struct Planner<'a> {
    stacks: &'a Stacks,
    ledger: Ledger,
    plan: Plan,
    wanted: BTreeSet<String>,
    /// A scope the plan covers, and with `--place` the one dir it's limited to.
    covered: Vec<(String, Option<PathBuf>)>,
    trees: BTreeMap<String, Result<std::sync::Arc<Tree>, String>>,
    /// Each scope's roots and the slots in them, worked out once per plan.
    places: BTreeMap<String, Result<Places, String>>,
    /// Each repo's checkouts and worktrees, verified once per plan: the ledger check and the repo's own items
    /// (its two scopes and itself) all ask, and each answer costs a git call per worktree.
    checkouts: BTreeMap<String, Result<Checkouts, String>>,
}

/// Every place sync puts a tool-owned path (design: Ownership boundary), as one grammar: a scope names roots (a
/// target's dir, a repo's checkouts and worktrees, a workspace's dir), each verified when it is made
/// (design-simplification-r6, rule A), and the slots in them. Sync asks a slot where to write; the ledger check
/// accepts a record only for a root, parts and kind that a slot of that scope produces (Sol review r4, R4-01).
/// Neither keeps its own idea of the layout.
#[derive(Clone)]
enum Slot {
    /// A user-level skills target's copy of one skill: `<target dir>/<skill>`.
    TargetSkill,
    /// The shared file of an instructions target with `via`: `<its dir>/<its name>`.
    Shared(Part),
    /// An adapter's file in a place: `<checkout>/CLAUDE.local.md`, `<checkout>/AGENTS.override.md`.
    AdapterFile(&'static dyn Adapter),
    /// A place's role files: `<checkout>/.lords`.
    RoleDir,
    /// An adapter's copy of one skill in a place: `<checkout>/<skills dir>/<skill>`.
    AdapterSkill(&'static dyn Adapter),
}

impl Slot {
    /// This slot's place in `root`; `name` is the skill's, for the two skill slots, and must be a name a skill can
    /// have: a slug, as the schema's ids (Sol review r5, R5-02). `None` for any other name.
    fn place(&self, root: &Root, name: &str) -> Option<Contained> {
        let fixed = |names: &[&str]| -> Option<Vec<Part>> {
            names.iter().map(|n| Part::new(n).ok()).collect::<Option<Vec<_>>>()
        };
        let parts = match self {
            Slot::TargetSkill => vec![Part::skill(name).ok()?],
            Slot::Shared(file) => vec![file.clone()],
            Slot::AdapterFile(a) => fixed(&[a.repo_file()])?,
            Slot::RoleDir => fixed(&[ROLE_DIR])?,
            Slot::AdapterSkill(a) => {
                let mut v = fixed(&a.repo_skills_dir()?.split('/').collect::<Vec<_>>())?;
                v.push(Part::skill(name).ok()?);
                v
            }
        };
        Some(root.join(&parts))
    }

    fn kind(&self) -> &'static str {
        match self {
            Slot::Shared(_) | Slot::AdapterFile(_) => "file",
            Slot::TargetSkill | Slot::RoleDir | Slot::AdapterSkill(_) => "dir",
        }
    }
}

/// A scope's verified roots and the slots in them.
type Places = (Vec<Root>, Vec<Slot>);

/// A place's ledger scope: `repo:<name>` (or `workspace:<name>`) for what every adapter's agents read, with
/// `/<adapter>` for one adapter's files.
fn place_scope_of(workspace: bool, name: &str, adapter: Option<&dyn Adapter>) -> String {
    let base = format!("{}:{name}", if workspace { "workspace" } else { "repo" });
    match adapter {
        Some(a) => format!("{base}/{}", a.id()),
        None => base,
    }
}

/// The adapters a place writes for (all of them when it names none).
fn place_adapters(names: &[String]) -> Vec<&'static dyn Adapter> {
    if names.is_empty() {
        adapters::ALL.to_vec()
    } else {
        names.iter().filter_map(|n| adapters::get(n)).collect()
    }
}

/// A `via` target's shared file: its dir is the root, its name the one part.
fn shared_part(t: &Target) -> Result<Part, String> {
    let via = expand_home(t.via.as_deref().unwrap_or(""));
    let name = via.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    Part::new(&name)
}

impl Planner<'_> {
    fn cover(&mut self, scope: &str, under: Option<&Path>) {
        self.covered.push((scope.to_string(), under.map(Path::to_path_buf)));
    }

    /// Keeps a root the plan verified, for `set-aside` (design review, point 2: only within one of these).
    fn verified(&mut self, root: &Root) {
        if !self.plan.roots.iter().any(|r| ledger::key(r.path()) == ledger::key(root.path())) {
            self.plan.roots.push(root.clone());
        }
    }

    /// The roots a scope names and the slots sync writes there; none for a scope that names nothing declared, or
    /// an adapter its place doesn't write for. A repo's roots are its real checkouts and worktrees, verified by
    /// `Root::checkout` (R5-01, R6-02), never the dirs worktrees are kept in (R4-01); a configured checkout that is
    /// none is an error. Target names are unique (R5-06), so `target:<name>` is one target.
    fn places_of(&mut self, scope: &str) -> Result<Places, String> {
        let stacks = self.stacks;
        let fresh = !self.places.contains_key(scope);
        let repo_name = scope.split('/').next().and_then(|b| b.strip_prefix("repo:"));
        let repo_roots = match repo_name {
            Some(name) if fresh => self.checkouts_of(name),
            _ => None,
        };
        let found = self
            .places
            .entry(scope.to_string())
            .or_insert_with(|| {
                let none = Ok((Vec::new(), Vec::new()));
                let Some(m) = stacks.private.as_ref().map(|p| &p.manifest) else { return none };
                let (base, adapter) = match scope.split_once('/') {
                    Some((b, a)) => (b, Some(a)),
                    None => (scope, None),
                };
                let (kind, name) = base.split_once(':').unwrap_or(("", ""));
                let place = |roots: Vec<Root>, declared: &[String]| match adapter {
                    None => Ok((roots, vec![Slot::RoleDir])),
                    Some(id) => match place_adapters(declared).into_iter().find(|a| a.id() == id) {
                        Some(a) => {
                            let mut slots = vec![Slot::AdapterFile(a)];
                            if a.repo_skills_dir().is_some() {
                                slots.push(Slot::AdapterSkill(a));
                            }
                            Ok((roots, slots))
                        }
                        None => Ok((Vec::new(), Vec::new())),
                    },
                };
                match (kind, adapter) {
                    ("target", None) => match m.targets.iter().find(|t| t.name == name) {
                        Some(t) if t.kind == "skills" => {
                            Ok((vec![Root::target(t)?], vec![Slot::TargetSkill]))
                        }
                        Some(t) if t.kind == "instructions" && t.via.is_some() => {
                            Ok((vec![Root::target(t)?], vec![Slot::Shared(shared_part(t)?)]))
                        }
                        _ => none,
                    },
                    ("repo", _) => match (m.repos.get(name), repo_roots) {
                        (Some(r), Some(found)) => {
                            place(found.map_err(|e| format!("repo {name}: {e}"))?.0, &r.adapters)
                        }
                        _ => none,
                    },
                    ("workspace", _) => match m.workspaces.get(name) {
                        Some(w) => place(vec![Root::workspace(w)?], &w.adapters),
                        None => none,
                    },
                    _ => none,
                }
            })
            .clone();
        // Kept once, when the scope is first worked out: the ledger check asks again for each of its records, and
        // keeping every root again per record made the check quadratic in the worktrees.
        if let (true, Ok((roots, _))) = (fresh, &found) {
            for r in roots {
                self.verified(r);
            }
        }
        found
    }

    /// A declared repo's checkouts and worktrees (`checkouts`), worked out once per plan; `None` for a repo the
    /// stack doesn't declare.
    fn checkouts_of(&mut self, name: &str) -> Option<Result<Checkouts, String>> {
        let repo = self.stacks.private.as_ref()?.manifest.repos.get(name)?;
        Some(self.checkouts.entry(name.to_string()).or_insert_with(|| checkouts(repo)).clone())
    }

    /// A record's place, when a slot of its scope, of its kind, produces exactly its root and parts (Sol review r3,
    /// R3-03; r4, R4-01): its root one this plan verified for the scope, its parts the slot's (a skill's name a slug:
    /// R5-02). An error when the scope's place is misconfigured (R5-01).
    fn placed(&mut self, o: &ledger::Owned) -> Result<Option<Contained>, String> {
        if let Some(dir) = o.scope.strip_prefix("hooks:") {
            // The ledger checked the hook's name and dir when it loaded; the dir is git's hooks dir of a checkout,
            // which this plan doesn't verify, so a hook record is only ever forgotten or left, never removed here.
            let _ = dir;
            return Ok(None);
        }
        let name = o.parts.last().cloned().unwrap_or_default();
        let (roots, slots) = self.places_of(&o.scope)?;
        let root =
            roots.into_iter().find(|r| ledger::key(r.path()) == ledger::key(Path::new(&o.root)));
        Ok(root.and_then(|root| {
            slots.iter().filter(|s| s.kind() == o.kind).find_map(|s| {
                s.place(&root, &name).filter(|c| {
                    c.parts().iter().map(Part::as_str).eq(o.parts.iter().map(String::as_str))
                })
            })
        }))
    }

    /// Whether the stack still declares what a scope names (a target, repo or workspace; a hooks dir always is).
    fn declared(&self, scope: &str) -> bool {
        let base = scope.split('/').next().unwrap_or(scope);
        let Some(m) = self.stacks.private.as_ref().map(|p| &p.manifest) else { return false };
        match base.split_once(':') {
            Some(("target", n)) => m.targets.iter().any(|t| t.name == n),
            Some(("repo", n)) => m.repos.contains_key(n),
            Some(("workspace", n)) => m.workspaces.contains_key(n),
            Some(("hooks", _)) => true,
            _ => false,
        }
    }

    /// Every ledger record lies where its scope says (Sol review r3, R3-03: a well-formed but false record could
    /// otherwise make sync remove a path outside every target). A record whose path is gone only loses its record;
    /// any other is an error, and nothing is planned or applied until the ledger is put right. A record of a scope
    /// the stack no longer declares is never acted on (no plan covers it): it is noted, and checked again if its
    /// target comes back. A hook's record is the hook installer's: sync leaves it.
    fn check_ledger(&mut self) -> Result<(), String> {
        let records: Vec<ledger::Owned> = self.ledger.owned.values().cloned().collect();
        for o in records {
            let path = o.path();
            if o.scope.starts_with("hooks:") {
                continue;
            }
            if !self.declared(&o.scope) {
                self.plan.notes.push(format!(
                    "the ledger records {} for {}, which the stack no longer declares: left as it is, and never acted on; declare it again and `lords uninstall` removes it",
                    path.display(),
                    o.scope
                ));
                continue;
            }
            let placed = self.placed(&o).map_err(|e| {
                format!(
                    "the ledger ({}) records {} for {}, whose place is misconfigured: {e}; sync does nothing until it is put right (nothing was changed)",
                    ledger::file(&self.plan.home).display(),
                    path.display(),
                    o.scope
                )
            })?;
            if placed.is_some() || fsguard::kind(&path).is_ok_and(|k| k == fsguard::Kind::Missing) {
                continue;
            }
            return Err(format!(
                "the ledger ({}) records {} for {}, but that is no place {} has; sync does nothing until the ledger is put right (nothing was changed)",
                ledger::file(&self.plan.home).display(),
                path.display(),
                o.scope,
                o.scope
            ));
        }
        Ok(())
    }
    /// A skill's copy, read once per plan.
    fn tree_of(&mut self, e: &Entry) -> Result<std::sync::Arc<Tree>, String> {
        let src = e.meta_file.parent().expect("a sidecar has a dir").to_path_buf();
        self.trees
            .entry(e.id().to_string())
            .or_insert_with(|| skill_tree(&src).map(std::sync::Arc::new))
            .clone()
    }

    /// What a path that isn't in the ledger means where sync wants to write: a conflict. When it holds exactly what
    /// sync would write, `lords adopt` records it; otherwise a human decides (`lords set-aside` moves it away).
    fn not_ours(item: &mut Item, same: bool) {
        item.action = Action::Conflict;
        item.adoptable = same;
        item.notes.push(if same {
            "not in the ledger, though exactly what sync writes here: `lords adopt <path>` records it as the tool's".into()
        } else {
            "not the tool's (no ledger record): sync never claims a path by its name or contents; move its text into the stack, or `lords set-aside <path>` once you've checked it".into()
        });
    }

    /// The item that makes the tool-owned file `path` hold `content`, replaced whole.
    fn tool_file(
        &mut self,
        target: &str,
        scope: &str,
        place: &Contained,
        content: String,
        composed: &Composed,
    ) -> Item {
        let path = place.path();
        self.wanted.insert(ledger::key(path));
        let want = fsguard::sha(content.as_bytes());
        let mut item = Item::new(target, path.to_path_buf(), Action::Create);
        item.class = Class::Tool;
        item.scope = scope.to_string();
        // Below a root this plan verified, by construction (rule A).
        item.dest = Dest::Tool(place.clone());
        item.root = place.root().path().to_path_buf();
        item.want = Some(want.clone());
        let mut current = String::new();
        match fsguard::kind(path) {
            Err(e) => return conflict(target, path, e),
            Ok(fsguard::Kind::Dir) => return conflict(target, path, "a dir where the file goes"),
            Ok(fsguard::Kind::Missing) => {}
            Ok(fsguard::Kind::File) => {
                let bytes = match fsguard::read(path) {
                    Ok(b) => b.unwrap_or_default(),
                    Err(e) => return conflict(target, path, e),
                };
                let have = fsguard::sha(&bytes);
                current = String::from_utf8_lossy(&bytes).into_owned();
                match self.ledger.get(path) {
                    Some(o) if o.sha == have => {
                        item.have = Some(have.clone());
                        item.action = if have == want { Action::InSync } else { Action::Update };
                    }
                    Some(_) => {
                        return modified_copy(
                            target,
                            path,
                            "changed since sync wrote it: move the change into the stack, or delete the file and sync again",
                        )
                    }
                    None => Self::not_ours(&mut item, have == want),
                }
                item.current = Some(current.clone());
            }
        }
        item.diff = diff::unified(&current, &content, &display(path), &display(path), 3);
        item.proposed = Some(content);
        sources_into(&mut item, composed);
        item
    }

    /// The item that makes the tool-owned dir `path` hold `tree`, replaced whole.
    fn tool_dir(
        &mut self,
        target: &str,
        scope: &str,
        place: &Contained,
        tree: std::sync::Arc<Tree>,
    ) -> Item {
        let path = place.path();
        self.wanted.insert(ledger::key(path));
        let want = fsguard::tree_sha(&tree);
        let mut item = Item::new(target, path.to_path_buf(), Action::Install);
        item.class = Class::Tool;
        item.scope = scope.to_string();
        item.want = Some(want.clone());
        item.dest = Dest::Tool(place.clone());
        item.root = place.root().path().to_path_buf();
        let mut old = Tree::new();
        match fsguard::kind(path) {
            Err(e) => return conflict(target, path, e),
            Ok(fsguard::Kind::File) => return conflict(target, path, "a file where the dir goes"),
            Ok(fsguard::Kind::Missing) => {}
            Ok(fsguard::Kind::Dir) => {
                old = match fsguard::read_tree(path) {
                    Ok(t) => t,
                    Err(e) => return conflict(target, path, e),
                };
                let have = fsguard::tree_sha(&old);
                match self.ledger.get(path) {
                    Some(o) if o.sha == have => {
                        item.have = Some(have.clone());
                        item.action = if have == want { Action::InSync } else { Action::Refresh };
                    }
                    Some(_) => {
                        return modified_copy(
                            target,
                            path,
                            "changed since sync wrote it (a file edited, added or removed): move the change into the stack, or delete the dir and sync again",
                        )
                    }
                    None => Self::not_ours(&mut item, have == want),
                }
            }
        }
        // The diff shows what changes in each file, by name, and each dir that comes or goes.
        let text = |t: &Tree, r: &str| t.get(r).map(|b| String::from_utf8_lossy(b).into_owned());
        let names: BTreeSet<&String> = old.keys().chain(tree.keys()).collect();
        for r in names {
            if fsguard::is_dir_entry(r) {
                match (old.contains_key(r), tree.contains_key(r)) {
                    (true, false) => {
                        item.diff.push_str(&format!("- dir {}\n", display(&path.join(r))))
                    }
                    (false, true) => {
                        item.diff.push_str(&format!("+ dir {}\n", display(&path.join(r))))
                    }
                    _ => {}
                }
                continue;
            }
            let (a, b) = (text(&old, r).unwrap_or_default(), text(&tree, r).unwrap_or_default());
            if a != b {
                let label = display(&path.join(r));
                item.diff.push_str(&diff::unified(&a, &b, &label, &label, 3));
            }
        }
        item.tree = Some(tree);
        item
    }

    /// Every ledger record in a covered scope that the plan no longer wants: removed while it still matches what
    /// sync put there, its record dropped when the path is gone or changed (the path then is no longer the tool's).
    fn unwanted(&mut self) {
        let records: Vec<ledger::Owned> = self.ledger.owned.values().cloned().collect();
        for o in records {
            let path = o.path();
            let key = ledger::key(&path);
            let covered = self.covered.iter().any(|(scope, under)| {
                *scope == o.scope
                    && under.as_deref().is_none_or(|u| {
                        let u = ledger::key(u);
                        key == u || key.starts_with(&format!("{u}/"))
                    })
            });
            if !covered || self.wanted.contains(&key) {
                continue;
            }
            let target = format!("{} (no longer wanted)", o.scope);
            let mut item = Item::new(&target, path.clone(), Action::Forget);
            item.class = Class::Tool;
            item.scope = o.scope.clone();
            item.have = Some(o.sha.clone());
            // `check_ledger` let through only records in place, or whose path is gone (which only lose the record):
            // only a placed record can be removed, by construction (rule A); the others have nowhere to act.
            match self.placed(&o).ok().flatten() {
                Some(at) => {
                    item.root = at.root().path().to_path_buf();
                    item.dest = Dest::Tool(at);
                }
                None => item.root = path.clone(),
            }
            let now = match fsguard::kind(&path) {
                Err(e) => {
                    item.action = Action::Report;
                    item.notes.push(format!("can't check it ({e}); left as it is"));
                    self.plan.items.push(item);
                    continue;
                }
                Ok(fsguard::Kind::Missing) => None,
                // What can't be read is reported and left, never taken for gone (Sol review r5, R5-04's class).
                Ok(fsguard::Kind::File) if o.kind == "file" => match fsguard::read(&path) {
                    Ok(b) => Some(fsguard::sha(&b.unwrap_or_default())),
                    Err(e) => {
                        item.action = Action::Report;
                        item.notes.push(format!("can't read it ({e}); left as it is"));
                        self.plan.items.push(item);
                        continue;
                    }
                },
                Ok(fsguard::Kind::Dir) if o.kind == "dir" => match fsguard::read_tree(&path) {
                    Ok(t) => Some(fsguard::tree_sha(&t)),
                    Err(e) => {
                        item.action = Action::Report;
                        item.notes.push(format!("can't read it ({e}); left as it is"));
                        self.plan.items.push(item);
                        continue;
                    }
                },
                Ok(_) => Some(String::new()),
            };
            match now {
                None => item.notes.push("gone already; its ledger record goes".into()),
                Some(h) if h == o.sha => {
                    item.action = Action::Remove;
                    item.notes.push("sync's, unchanged, and no longer wanted here: deleted".into());
                }
                Some(_) => item.notes.push(
                    "changed since sync wrote it: left as it is, and no longer the tool's (its record goes)".into(),
                ),
            }
            self.plan.items.push(item);
        }
    }
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
        default_role_inline: false,
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

/// A place's file past the warning line: a note; past the harness's hard limit: a conflict, so the file isn't
/// written and the old one stays, as the harness would cut its end off without a word (orchestrator, 27 Oct).
fn size_note(item: &mut Item, adapter: &dyn Adapter, text: &str, composed: &Composed) {
    if let Some(limit) = adapter.byte_limit().filter(|limit| text.len() > *limit) {
        let mut largest = sections(text, &adapter.render(composed));
        largest.sort_by(|a, b| b.1.cmp(&a.1));
        let largest: Vec<String> =
            largest.iter().take(3).map(|(s, n)| format!("{s} {n}")).collect();
        item.action = Action::Conflict;
        item.conflict = ConflictKind::OverLimit;
        item.notes.insert(
            0,
            format!(
                "{} bytes, past {}'s limit of {limit}, which would cut the end off: not written, the old file stays; largest sections: {}",
                text.len(),
                adapter.id(),
                largest.join(", ")
            ),
        );
        return;
    }
    if let Some(limit) = over_size(adapter, text.len()) {
        item.notes.push(format!(
            "{} bytes: {} reads at most {limit} bytes of a project's instruction files, from the repo root down to the working dir, and cuts the rest",
            text.len(),
            adapter.id()
        ));
    }
}

/// An instructions target: the composed text in its file (an owner file: the managed block), or (with `via`) in
/// one shared, tool-owned file under the tool's own dir that the target's file includes, so several profiles read
/// one copy.
fn instructions_items(pl: &mut Planner, t: &Target, adapter: &dyn Adapter) -> Vec<Item> {
    let path = expand_home(&t.path);
    let mut ctx = context(pl.stacks, adapter, audience_of(&t.audience, adapter), Layers::Global);
    ctx.roles = roles_of(t);
    let composed = compose::compose(pl.stacks, &ctx);
    let replace = t.own_text.as_deref() == Some("replace");
    // Only for the `own_text = replace` note; `owner_block_item` reads the file strictly.
    let current = read_lossy(&path).unwrap_or_default();
    let text = adapter.render(&composed);
    let mut items = Vec::new();
    let scope = format!("target:{}", t.name);
    pl.cover(&scope, None);
    let block = match &t.via {
        None => text.clone(),
        Some(via) => {
            let shared = match Root::target(t).and_then(|root| Ok(root.join(&[shared_part(t)?]))) {
                Ok(c) => c,
                Err(e) => return vec![conflict(&t.name, &expand_home(via), e)],
            };
            pl.verified(shared.root());
            let Some(line) = adapter.import_line(shared.path()) else {
                return vec![conflict(
                    &t.name,
                    &path,
                    format!(
                        "adapter {} doesn't expand includes, so `via` can't work",
                        adapter.id()
                    ),
                )];
            };
            let s = pl.tool_file(
                &format!("shared text ({})", t.name),
                &scope,
                &shared,
                text.clone(),
                &composed,
            );
            items.push(s);
            if composed.text.is_empty() {
                String::new()
            } else {
                compose::block(&line)
            }
        }
    };
    let owner = OwnerFile::target(t);
    pl.plan.owner_files.push(owner.clone());
    // No size note: a harness's limit is on a place's project files, and a user-level file is read whole (Codex).
    let mut item = owner_block_item(&t.name, &owner, &block, replace, &composed);
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

/// A settings target (an owner file): the adapter's exclude list in a JSON settings file holds every path in
/// `exclude`. Only that one key changes; the rest of the file keeps its bytes.
fn settings_item(pl: &mut Planner, t: &Target, adapter: &dyn Adapter) -> Item {
    let owner = OwnerFile::target(t);
    pl.plan.owner_files.push(owner.clone());
    let path = owner.path().to_path_buf();
    let Some(key) = adapter.excludes_key() else {
        return conflict(
            &t.name,
            &path,
            format!("adapter {} has no exclude setting", adapter.id()),
        );
    };
    let current = match fsguard::read_text(&path) {
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
        return conflict(&t.name, &path, format!("`{key}` isn't a list; left alone"));
    };
    // One path, however its separators and case are spelt (Sol review r2, R2-20).
    let same = |a: &str, b: &str| ledger::key(Path::new(a)) == ledger::key(Path::new(b));
    let mut added = false;
    for e in &t.exclude {
        let want = adapters::slashes(&expand_home(e));
        if !list.iter().any(|v| v.as_str().is_some_and(|s| same(s, &want))) {
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
    owner_file_item(&t.name, &path, Dest::Owner(owner), proposed, &Composed::default())
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

fn same_place(a: &Path, b: &Path) -> bool {
    ledger::key(a) == ledger::key(b)
}

fn strip_verbatim(p: &Path) -> String {
    let s = p.to_string_lossy().replace('\\', "/");
    s.trim_start_matches("//?/").trim_start_matches("/??/").to_ascii_lowercase()
}

/// A user-level skills target: a tool-owned copy of each skill that suits every run. What sync copied there before
/// and the stack no longer wants goes by its ledger record; nothing else in the dir is looked at.
fn skills_items(pl: &mut Planner, t: &Target, adapter: &dyn Adapter) -> Vec<Item> {
    let dir = expand_home(&t.path);
    let root = match Root::target(t) {
        Ok(r) => r,
        Err(e) => return vec![conflict(&t.name, &dir, e)],
    };
    pl.verified(&root);
    let ctx = context(pl.stacks, adapter, audience_of(&t.audience, adapter), Layers::Global);
    let wanted: Vec<&Entry> = compose::applicable(pl.stacks, &ctx)
        .into_iter()
        .filter(|e| e.meta.kind == Kind::Skill)
        .filter(|e| !matches!(e.meta.origin, crate::model::Origin::InRepo { .. }))
        .collect();
    let scope = format!("target:{}", t.name);
    pl.cover(&scope, None);
    let mut items = Vec::new();
    for e in wanted {
        let name = e.id().trim_start_matches("skill.").to_string();
        let Some(dest) = Slot::TargetSkill.place(&root, &name) else {
            items.push(conflict(
                &t.name,
                &dir.join(&name),
                format!("skill {}: no skill name sync writes", e.id()),
            ));
            continue;
        };
        match pl.tree_of(e) {
            Ok(tree) => items.push(pl.tool_dir(&t.name, &scope, &dest, tree)),
            Err(err) => {
                items.push(conflict(&t.name, dest.path(), format!("skill {}: {err}", e.id())))
            }
        }
    }
    items
}

/// The header line above the exclude patterns sync owns in a repo's `.git/info/exclude`.
const EXCLUDE_HEADER: &str = "# Lords Command: agent files written by sync";

/// Whether an exclude line is one sync writes now (its two root files) or wrote under its header in earlier
/// versions (the broader forms and per-file paths, no longer needed: tool-owned dirs ignore themselves).
fn ours(line: &str) -> bool {
    let l = line.trim().trim_start_matches('/');
    ["CLAUDE.local.md", "AGENTS.override.md"].contains(&l)
        || l.starts_with(&format!("{ROLE_DIR}/"))
        || l.starts_with(".claude/skills/")
}

/// A repo's exclude file with sync's section holding the lines of `files` that no other line has already (Sol
/// review r2, R2-19): exact paths from the worktree's root. Every other line keeps its place and bytes; sync's
/// earlier sections under its header merge into one.
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
    let there: BTreeSet<&str> = kept.iter().map(|l| l.trim()).collect();
    let mut want: Vec<&String> = files.iter().filter(|f| !there.contains(f.as_str())).collect();
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

/// Whether an exclude edit from `current` to `proposed` only adds patterns: none goes, and none added is a negation
/// (`!…`). Such an edit can only hide paths, never show one, so it needs no listing (orchestrator, 26 Oct).
/// Comments and blank lines aren't patterns.
fn only_adds(current: &str, proposed: &str) -> bool {
    let patterns = |text: &str| -> BTreeSet<String> {
        text.lines()
            .map(|l| l.trim_end_matches('\r').to_string())
            .filter(|l| !l.trim().is_empty() && !l.starts_with('#'))
            .collect()
    };
    let (before, after) = (patterns(current), patterns(proposed));
    before.is_subset(&after) && after.difference(&before).all(|p| !p.starts_with('!'))
}

/// The untracked paths an exclude edit from `current` to `proposed` would newly show in `git status`, in every
/// worktree that shares the file (git lists them from `checkout`; orchestrator, 26 Oct: "plan exclude edits per
/// common dir, so a place's dry run sees what its edit does to the other places"). Each place is listed twice by
/// `git ls-files --others --directory`, with each version's lines as its patterns and the same per-dir
/// `.gitignore` and global excludes, so the difference is the edit's alone. A git that fails is an error.
fn newly_shown(checkout: &Path, current: &str, proposed: &str) -> Result<Vec<String>, String> {
    let patterns = |text: &str| -> Vec<String> {
        text.lines()
            .map(|l| l.trim_end_matches('\r'))
            .filter(|l| !l.trim().is_empty() && !l.starts_with('#'))
            .map(str::to_string)
            .collect()
    };
    let (before, after) = (patterns(current), patterns(proposed));
    let global = match git::run(checkout, &["config", "--path", "--get", "core.excludesFile"]) {
        Ok(p) => Some(PathBuf::from(p.trim())),
        Err(_) => crate::config::home().map(|h| h.join(".config").join("git").join("ignore")),
    }
    .filter(|p| p.is_file());
    let list = git::run(checkout, &["worktree", "list", "--porcelain"])?;
    let mut shown = Vec::new();
    for place in list.lines().filter_map(|l| l.strip_prefix("worktree ")).map(PathBuf::from) {
        if !place.is_dir() {
            continue;
        }
        let listed = |pats: &[String]| -> Result<BTreeSet<String>, String> {
            let mut args: Vec<String> =
                ["ls-files", "-z", "--others", "--directory", "--no-empty-directory"]
                    .iter()
                    .map(|s| s.to_string())
                    .collect();
            args.push("--exclude-per-directory=.gitignore".into());
            if let Some(g) = &global {
                args.push(format!("--exclude-from={}", g.display()));
            }
            for p in pats {
                args.push(format!("--exclude={p}"));
            }
            let args: Vec<&str> = args.iter().map(String::as_str).collect();
            Ok(git::run(&place, &args)?
                .split('\0')
                .filter(|s| !s.is_empty())
                .map(str::to_string)
                .collect())
        };
        let (now, then) = (listed(&before)?, listed(&after)?);
        shown.extend(then.difference(&now).map(|p| format!("{}/{p}", place.display())));
    }
    Ok(shown)
}

/// The files the repo tracks at or below `paths` in the checkout at `dir`, relative to it, from one git call.
fn tracked_at(dir: &Path, paths: &[&str]) -> Result<Vec<String>, String> {
    let mut args = vec!["ls-files", "-z", "--"];
    args.extend_from_slice(paths);
    Ok(git::run(dir, &args)?.split('\0').filter(|s| !s.is_empty()).map(str::to_string).collect())
}

/// Every checkout and (if asked) worktree of a repo, as roots verified by `Root::checkout`, the worktrees skipped
/// for lying outside `worktree_roots`, and one note per worktree git lists that is no checkout (stale).
/// Configured and discovered roots get the same check (design-simplification-r6, rule A; Sol review r6, R6-02). A
/// configured checkout that is none refuses the whole place (R5-01): it is the config that is wrong. A discovered
/// one is git's stale registration: skipped, and said. A git call that fails refuses the place.
/// A repo's verified roots, the worktrees skipped for lying outside `worktree_roots`, and the stale ones noted.
type Checkouts = (Vec<Root>, Vec<PathBuf>, Vec<String>);

fn checkouts(repo: &stack::Repo) -> Result<Checkouts, String> {
    let roots: Vec<String> = repo.worktree_roots.iter().map(|r| norm(&expand_home(r))).collect();
    let inside = |p: &Path| {
        let p = norm(p);
        roots.iter().any(|r| p == *r || p.starts_with(&format!("{}/", r.trim_end_matches('/'))))
    };
    let mut out: Vec<Root> = Vec::new();
    let mut skipped: Vec<PathBuf> = Vec::new();
    let mut stale: Vec<String> = Vec::new();
    let mut seen: Vec<String> = Vec::new();
    for c in &repo.checkouts {
        let c = expand_home(c);
        let root = Root::checkout(&c).map_err(|e| {
            format!("{e}; put the checkout's root in `checkouts` of the private stack.toml")
        })?;
        seen.push(norm(&c));
        out.push(root);
        if !repo.worktrees {
            continue;
        }
        let list = git::run(&c, &["worktree", "list", "--porcelain"])?;
        for w in list.lines().filter_map(|l| l.strip_prefix("worktree ")).map(PathBuf::from) {
            if seen.contains(&norm(&w)) {
                continue;
            }
            seen.push(norm(&w));
            if !(roots.is_empty() || inside(&w)) {
                skipped.push(w);
                continue;
            }
            match Root::checkout(&w) {
                Ok(root) => out.push(root),
                Err(e) => stale.push(format!(
                    "worktree {} skipped: {e} (`git worktree prune` drops it)",
                    w.display()
                )),
            }
        }
    }
    Ok((out, skipped, stale))
}
/// Where sync writes a place's files: a repo (its checkouts and worktrees) or a workspace (one dir, no git).
struct Place<'a> {
    label: String,
    name: String,
    workspace: bool,
    project: Option<String>,
    adapters: &'a [String],
    paths: Vec<Root>,
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
        default_role_inline: false,
    };
    compose::compose(stacks, &ctx)
}

fn place_scope(place: &Place, adapter: Option<&dyn Adapter>) -> String {
    place_scope_of(place.workspace, &place.name, adapter)
}

fn place_items(pl: &mut Planner, place: &Place, only: Option<&str>, under: Option<&Path>) {
    let mut chosen: Vec<&'static dyn Adapter> = Vec::new();
    if place.adapters.is_empty() {
        chosen.extend(adapters::ALL.iter().copied());
    }
    for id in place.adapters {
        match adapters::get(id) {
            Some(a) => chosen.push(a),
            None => pl.plan.notes.push(format!(
                "{}: unknown adapter `{id}` (known: {})",
                place.label,
                adapters::ids().join(", ")
            )),
        }
    }
    chosen.retain(|a| only.is_none_or(|o| o == a.id()));
    pl.cover(&place_scope(place, None), under);
    for a in &chosen {
        pl.cover(&place_scope(place, Some(*a)), under);
    }
    let path_entries = pl
        .stacks
        .entries()
        .filter(|e| {
            e.active()
                && !place.workspace
                && matches!(&e.scope, crate::model::Scope::Path { repo: r, .. } if *r == place.name)
        })
        .count();
    if path_entries > 0 {
        pl.plan.notes.push(format!(
            "{}: {path_entries} path-scoped entries are not written yet",
            place.label
        ));
    }
    // The two root files are the only lines sync keeps in a repo's exclude file; its dirs ignore themselves.
    // Each with a checkout it serves, from which git lists every worktree that shares it.
    let mut excludes: Vec<(OwnerFile, PathBuf, Vec<String>)> = Vec::new();
    // A git that fails here is a conflict, never a repo without an exclude file (Sol review r5, decision 1). The
    // exclude file is the one git names for this verified checkout (rule A).
    let mut add_exclude = |root: &Root, what: &str| -> Result<(), String> {
        if place.workspace {
            return Ok(());
        }
        let ex = OwnerFile::exclude(root)?;
        match excludes.iter_mut().find(|(e, _, _)| ledger::key(e.path()) == ledger::key(ex.path()))
        {
            Some((_, _, files)) if !files.iter().any(|f| f == what) => files.push(what.to_string()),
            Some(_) => {}
            None => excludes.push((ex, root.path().to_path_buf(), vec![what.to_string()])),
        }
        Ok(())
    };
    let skills = place_skills(pl.stacks, place.layers(false));
    // What the repo tracks at the paths sync writes in each checkout (its adapters' files and skills dirs), from one
    // git call per checkout; a git that fails is the error of every item there.
    let mut asked: Vec<&str> = chosen.iter().map(|a| a.repo_file()).collect();
    if !skills.is_empty() {
        asked.extend(chosen.iter().filter_map(|a| a.repo_skills_dir()));
    }
    let tracked_in: BTreeMap<String, Result<Vec<String>, String>> = if place.workspace {
        BTreeMap::new()
    } else {
        place
            .paths
            .iter()
            .filter(|r| r.path().is_dir())
            .map(|r| (ledger::key(r.path()), tracked_at(r.path(), &asked)))
            .collect()
    };
    // Whether the repo tracks `rel` (a file, or anything below it) in the checkout at `p`.
    let tracked = |p: &Path, rel: &str| -> Result<bool, String> {
        match tracked_in.get(&ledger::key(p)) {
            None => Ok(false),
            Some(Err(e)) => Err(e.clone()),
            Some(Ok(files)) => Ok(files.iter().any(|f| {
                f == rel || f.strip_prefix(rel).is_some_and(|rest| rest.starts_with('/'))
            })),
        }
    };
    for adapter in chosen.iter().copied() {
        let file = adapter.repo_file();
        let scope = place_scope(place, Some(adapter));
        for root in &place.paths {
            pl.verified(root);
            let p = root.path();
            let target = format!("{} ({file})", place.label);
            let at = Slot::AdapterFile(adapter).place(root, "").expect("a slot without a name");
            let path = at.path().to_path_buf();
            if !p.is_dir() {
                pl.plan.items.push(conflict(&target, &path, "checkout not found"));
                continue;
            }
            match tracked(p, file) {
                Ok(false) => {}
                Ok(true) => {
                    pl.plan.items.push(conflict(
                        &target,
                        &path,
                        format!("{file} is tracked here; sync writes only untracked files"),
                    ));
                    continue;
                }
                Err(e) => {
                    pl.plan.items.push(conflict(&target, &path, e));
                    continue;
                }
            }
            // The tracked AGENTS.md this file may carry: one that is there but can't be read is a conflict, never
            // left out (r5, R5-04's class).
            let agents = p.join("AGENTS.md");
            if let Err(e) = fsguard::read_string(&agents) {
                if e.kind() != std::io::ErrorKind::NotFound {
                    pl.plan.items.push(conflict(
                        &target,
                        &path,
                        format!("{}: {e}", agents.display()),
                    ));
                    continue;
                }
            }
            if let Err(e) = add_exclude(root, &format!("/{file}")) {
                pl.plan.items.push(conflict(&target, &path, e));
                continue;
            }
            let (text, composed) = place_text(pl.stacks, place.layers(false), adapter, p);
            let mut item = pl.tool_file(&target, &scope, &at, text.clone(), &composed);
            size_note(&mut item, adapter, &text, &composed);
            pl.plan.items.push(item);
        }
    }
    // Development skills, copied into each checkout for harnesses that read a skills dir there.
    let skill_dirs: Vec<(&'static dyn Adapter, &str)> =
        chosen.iter().copied().filter_map(|a| a.repo_skills_dir().map(|d| (a, d))).collect();
    for (adapter, dir) in skill_dirs {
        let scope = place_scope(place, Some(adapter));
        for root in place.paths.iter().filter(|r| r.path().is_dir()) {
            let p = root.path();
            for e in &skills {
                let name = e.id().trim_start_matches("skill.").to_string();
                let rel = format!("{dir}/{name}");
                let target = format!("{} (skill {name})", place.label);
                let Some(dest) = Slot::AdapterSkill(adapter).place(root, &name) else {
                    pl.plan.items.push(conflict(
                        &target,
                        &p.join(&rel),
                        format!("skill {}: no skill name sync writes", e.id()),
                    ));
                    continue;
                };
                // A skill the repo tracks in a dir it indexes in place is the repo's own: its tracked copy wins there,
                // and the stack's isn't installed over it (orchestrator, 27 Oct; check reports a copy that differs).
                let indexed = |rel: &str| {
                    pl.stacks.entries().any(|x| {
                        matches!(&x.meta.origin, crate::model::Origin::InRepo { repo, path, .. }
                            if *repo == place.name && path == rel)
                    })
                };
                match tracked(p, &rel) {
                    Ok(false) => {}
                    Ok(true) if indexed(&rel) => {
                        pl.plan.notes.push(format!(
                            "{}: the repo tracks its own `{name}` at {rel}, indexed in place; the stack's copy isn't installed over it",
                            place.label
                        ));
                        continue;
                    }
                    Ok(true) => {
                        pl.plan.items.push(conflict(
                            &target,
                            dest.path(),
                            format!("the repo tracks a skill at {rel}; rename one of them"),
                        ));
                        continue;
                    }
                    Err(e) => {
                        pl.plan.items.push(conflict(&target, dest.path(), e));
                        continue;
                    }
                }
                match pl.tree_of(e) {
                    Ok(tree) => {
                        let item = pl.tool_dir(&target, &scope, &dest, tree);
                        pl.plan.items.push(item);
                    }
                    Err(err) => pl.plan.items.push(conflict(
                        &target,
                        dest.path(),
                        format!("skill {}: {err}", e.id()),
                    )),
                }
            }
        }
    }
    // One composed file per role, at a stable untracked path, for briefs to point at (backbone requirement BB-3);
    // all of them in one tool-owned dir that ignores itself.
    let roles: Vec<(String, Composed)> = declared_roles(pl.stacks)
        .into_iter()
        .map(|role| {
            let c = role_composed(pl.stacks, place.layers(true), &role);
            (role, c)
        })
        .collect();
    let mut tree = Tree::new();
    for (role, c) in &roles {
        if !c.text.is_empty() {
            tree.insert(role_file(role), c.text.clone().into_bytes());
        }
    }
    tree.insert(".gitignore".into(), b"*\n".to_vec());
    let tree = std::sync::Arc::new(tree);
    let scope = place_scope(place, None);
    for root in place.paths.iter().filter(|r| r.path().is_dir()) {
        let item = pl.tool_dir(
            &format!("{} (role files)", place.label),
            &scope,
            &Slot::RoleDir.place(root, "").expect("a slot without a name"),
            tree.clone(),
        );
        pl.plan.items.push(item);
    }
    for (ex, checkout, files) in excludes {
        pl.plan.owner_files.push(ex.clone());
        let label = format!("{} (git exclude)", place.label);
        let current = match fsguard::read_text(ex.path()) {
            Ok(c) => c.unwrap_or_default(),
            Err(e) => {
                pl.plan.items.push(conflict(&label, ex.path(), e));
                continue;
            }
        };
        let proposed = excludes_text(&current, &files);
        // The file is shared by every worktree of the repo: an edit that would show a path in any of their `git
        // status` is planned as a conflict, whichever place asked for it (orchestrator, 26 Oct).
        if proposed != current && !only_adds(&current, &proposed) {
            match newly_shown(&checkout, &current, &proposed) {
                Ok(shown) if shown.is_empty() => {}
                Ok(shown) => {
                    pl.plan.items.push(conflict(
                        &label,
                        ex.path(),
                        format!(
                            "the edit would show {} untracked path(s) in git status where this file is shared, e.g. {}; set them aside first (`lords set-aside`)",
                            shown.len(),
                            shown.iter().take(3).cloned().collect::<Vec<_>>().join(", ")
                        ),
                    ));
                    continue;
                }
                Err(e) => {
                    pl.plan.items.push(conflict(&label, ex.path(), e));
                    continue;
                }
            }
        }
        pl.plan.items.push(owner_file_item(
            &label,
            ex.path(),
            Dest::Owner(ex.clone()),
            proposed,
            &Composed::default(),
        ));
    }
    if place.workspace {
        return;
    }
    // The tracked AGENTS.md of the first checkout: what the public layers would make it. Shown, never written.
    let Some(first) = place.paths.first() else { return };
    let tracked = first.path().join("AGENTS.md");
    if let Some(current) = read_lossy(&tracked) {
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
            default_role_inline: false,
        };
        let composed = compose::compose(pl.stacks, &ctx);
        let label = format!("{} (tracked AGENTS.md)", place.label);
        let mut item = match compose::place_block(&current, &composed.text, false) {
            Ok(proposed) => owner_file_item(&label, &tracked, Dest::None, proposed, &composed),
            Err(e) => conflict(&label, &tracked, e),
        };
        item.action = match item.action {
            Action::Conflict => Action::Report,
            _ if item.current == item.proposed => Action::InSync,
            _ => Action::Report,
        };
        item.notes.push("tracked: changes by a commit on the repo's branch, never by sync; agents with a stack read the untracked files instead".into());
        pl.plan.items.push(item);
    }
}

fn repo_items(
    pl: &mut Planner,
    name: &str,
    repo: &stack::Repo,
    only: Option<&str>,
    only_path: Option<&Path>,
) {
    let (mut paths, skipped, stale) = match pl.checkouts_of(name).unwrap_or_else(|| checkouts(repo))
    {
        Ok(c) => c,
        Err(e) => {
            // The place refuses whole (Sol review r5, R5-01): nothing is planned in any of its checkouts.
            let first = repo.checkouts.first().map(|c| expand_home(c)).unwrap_or_default();
            pl.plan.items.push(conflict(&format!("repo {name}"), &first, e));
            return;
        }
    };
    for s in stale {
        pl.plan.notes.push(format!("repo {name}: {s}"));
    }
    if let Some(one) = only_path {
        paths.retain(|p| same_place(p.path(), one));
        if paths.is_empty() {
            pl.plan.notes.push(format!(
                "repo {name}: {} is none of its checkouts or worktrees under worktree_roots",
                display(one)
            ));
            return;
        }
    }
    if paths.is_empty() {
        pl.plan.notes.push(format!("repo {name}: no checkouts declared"));
        return;
    }
    if !skipped.is_empty() {
        let list: Vec<String> = skipped.iter().map(|p| p.display().to_string()).collect();
        pl.plan.notes.push(format!(
            "repo {name}: {} worktree(s) outside worktree_roots skipped: {}",
            skipped.len(),
            list.join(", ")
        ));
    }
    let place = Place {
        label: format!("repo {name}"),
        name: name.to_string(),
        workspace: false,
        project: pl.stacks.repo_project(name),
        adapters: &repo.adapters,
        paths,
    };
    place_items(pl, &place, only, only_path);
}

fn workspace_items(pl: &mut Planner, name: &str, w: &stack::Workspace, only: Option<&str>) {
    let root = match Root::workspace(w) {
        Ok(r) => r,
        Err(e) => {
            pl.plan.items.push(conflict(&format!("workspace {name}"), &expand_home(&w.path), e));
            return;
        }
    };
    let place = Place {
        label: format!("workspace {name}"),
        name: name.to_string(),
        workspace: true,
        project: w.project.clone(),
        adapters: &w.adapters,
        paths: vec![root],
    };
    place_items(pl, &place, only, None);
}

pub fn plan(stacks: &Stacks, opts: &Options) -> Result<Plan, String> {
    let private: &Stack = stacks
        .private
        .as_ref()
        .ok_or("targets are declared in the private stack.toml; no private stack loaded")?;
    if opts.home.as_os_str().is_empty() {
        return Err("no tool home given (Options.home): sync reads its ledger there".into());
    }
    let ledger = Ledger::load(&opts.home)?;
    let mut pl = Planner {
        stacks,
        plan: Plan {
            home: opts.home.clone(),
            ledger_revision: ledger.revision(),
            ..Plan::default()
        },
        ledger,
        wanted: BTreeSet::new(),
        covered: Vec::new(),
        trees: BTreeMap::new(),
        places: BTreeMap::new(),
        checkouts: BTreeMap::new(),
    };
    pl.check_ledger()?;
    // The memory index points at the stack's copies: say what an import would bring first.
    if !private.manifest.memory_stores.is_empty() {
        match crate::ops::import_preview(private, None) {
            Ok(reports) => {
                for r in reports.iter().filter(|r| !r.added.is_empty() || !r.changed.is_empty()) {
                    pl.plan.notes.push(format!(
                        "memory store {}: {} new, {} changed since the last import; --apply imports them (one commit) before writing",
                        r.store,
                        r.added.len(),
                        r.changed.len()
                    ));
                }
            }
            Err(e) => pl.plan.notes.push(format!("memory stores: {e}")),
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
        pl.plan.items.push(item);
    }
    if opts.repo.is_none() {
        let mut items = Vec::new();
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
            let start = items.len();
            match t.kind.as_str() {
                "instructions" => items.extend(instructions_items(&mut pl, t, adapter)),
                "skills" => items.extend(skills_items(&mut pl, t, adapter)),
                "settings" => items.push(settings_item(&mut pl, t, adapter)),
                k => return Err(format!("target {}: unknown kind {k}", t.name)),
            }
            for i in &mut items[start..] {
                i.units = vec![Unit::Target(t.name.clone())];
            }
        }
        // Profiles sharing one `via` file plan it once, as part of each of them; different texts for one file
        // can't both be written.
        let mut kept: Vec<Item> = Vec::new();
        for item in items {
            match kept.iter_mut().find(|k| same_place(&k.path, &item.path)) {
                Some(k) => {
                    if k.proposed != item.proposed {
                        k.action = Action::Conflict;
                        k.conflict = ConflictKind::Other;
                        k.notes
                            .push(format!("{} wants different text in the same file", item.target));
                    } else if item.action == Action::Conflict {
                        // The same file planned twice keeps the graver conflict of the two (FC-01).
                        if k.action != Action::Conflict {
                            k.action = Action::Conflict;
                            k.notes.extend(item.notes.clone());
                        }
                        if item.conflict == ConflictKind::Other {
                            k.conflict = ConflictKind::Other;
                        }
                    }
                    k.units.extend(item.units);
                }
                None => kept.push(item),
            }
        }
        pl.plan.items.extend(kept);
        shadow_notes(private, &mut pl.plan);
    }
    if !opts.user_only {
        for (name, repo) in &private.manifest.repos {
            if opts.repo.as_deref().is_some_and(|r| r != name) {
                continue;
            }
            let start = pl.plan.items.len();
            repo_items(&mut pl, name, repo, opts.adapter.as_deref(), opts.place.as_deref());
            for i in &mut pl.plan.items[start..] {
                i.units = vec![Unit::Repo(name.clone())];
            }
        }
        for (name, w) in &private.manifest.workspaces {
            if opts.repo.as_deref().is_some_and(|r| r != name) {
                continue;
            }
            if opts.place.is_none() {
                let start = pl.plan.items.len();
                workspace_items(&mut pl, name, w, opts.adapter.as_deref());
                for i in &mut pl.plan.items[start..] {
                    i.units = vec![Unit::Workspace(name.clone())];
                }
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
    // Removal only from a complete view (design-simplification-r6, rule B; Sol review r7, R7-04/R7-05): a problem of
    // any level in the stacks, or any conflict but a copy the owner changed (by its kind, r7 fix check FC-01), means
    // something wanted may not have been seen (a declared checkout or workspace that isn't there, a target file that
    // can't be read), so nothing is removed or forgotten; installs go on.
    let problems = stacks.all().map(|s| s.problems.len()).sum::<usize>();
    let conflicts = pl
        .plan
        .items
        .iter()
        .filter(|i| i.action == Action::Conflict && i.conflict != ConflictKind::ModifiedCopy)
        .count();
    let complete = (problems == 0 && conflicts == 0).then_some(Complete(()));
    pl.plan.over_limit = pl.plan.items.iter().any(|i| i.conflict == ConflictKind::OverLimit);
    let why = format!(
        "the stacks loaded with {problems} problem(s) and the plan has {conflicts} conflict(s) where it couldn't see what is wanted (`lords check` and `lords sync` list them)"
    );
    // Uninstall: nothing is wanted, so every tool-owned path in scope goes (owner files keep their blocks; `restore`
    // takes those back).
    if opts.uninstall {
        if complete.is_none() {
            return Err(format!("uninstall needs a complete view, and {why}; nothing was planned"));
        }
        pl.plan.items.clear();
        pl.wanted.clear();
    }
    match (opts.stage, &complete) {
        (Stage::Install, _) => staged(&mut pl.plan),
        (Stage::Full, Some(_)) => pl.unwanted(),
        (Stage::Full, None) => pl.plan.notes.push(format!(
            "removal held: {why}, so nothing is removed or forgotten until both are none"
        )),
    }
    pl.plan.complete = complete;
    Ok(pl.plan)
}

/// The install stage (design-simplification-r6, staged first use): of a plan, only tool-owned paths and each
/// repo's git-exclude lines, place by place, all or nothing. What an item may do is its type (`Item::change`), the
/// place it belongs to is its `units`, as planning set them (Sol review r7, R7-02/R7-03); no label decides either.
/// A place with any conflict is skipped whole, and said; a conflict that belongs to no place skips everything. No
/// other owner file is edited and nothing is retired or removed.
fn staged(plan: &mut Plan) {
    let mut blocked: BTreeMap<Option<&Unit>, Vec<String>> = BTreeMap::new();
    for i in plan.items.iter().filter(|i| i.action == Action::Conflict) {
        let why = format!("{}: {}", i.path.display(), i.notes.first().cloned().unwrap_or_default());
        if i.units.is_empty() {
            blocked.entry(None).or_default().push(why.clone());
        }
        for u in &i.units {
            blocked.entry(Some(u)).or_default().push(why.clone());
        }
    }
    let notes: Vec<String> = blocked
        .iter()
        .map(|(u, why)| {
            let what =
                u.map_or("everything (a conflict of no one place)".into(), |u| u.to_string());
            format!(
                "install stage: {what} skipped whole, {} conflict(s), e.g. {}",
                why.len(),
                why[0]
            )
        })
        .collect();
    let all = blocked.contains_key(&None);
    let blocked: BTreeSet<Unit> = blocked.into_keys().flatten().cloned().collect();
    plan.notes.extend(notes);
    plan.items.retain(|i| {
        !all && !i.units.iter().any(|u| blocked.contains(u))
            && !matches!(i.action, Action::RetireEntry | Action::Remove | Action::Forget)
            && matches!(i.change(), Change::ToolOwned | Change::ExcludeEdit)
    });
}

// ---------------------------------------------------------------- apply

/// One writer of the ledger at a time on this machine (a user's sync, a post-checkout hook's, a hook
/// installation): a lock file in the tool's home, waited for up to `secs` seconds.
pub(crate) struct ApplyLock(Contained);

impl ApplyLock {
    pub(crate) fn take(home: &Path, secs: u64) -> Result<ApplyLock, String> {
        let root = Root::home(home)?;
        root.create()?;
        let lock = root.join(&[Part::new("apply.lock")?]);
        let until = std::time::Instant::now() + std::time::Duration::from_secs(secs);
        loop {
            match fsguard::create_file(&lock, format!("pid {}\n", crate::process::id()).as_bytes())
            {
                Ok(()) => return Ok(ApplyLock(lock)),
                Err(_) if lock.path().exists() && std::time::Instant::now() < until => {
                    std::thread::sleep(std::time::Duration::from_millis(200));
                }
                Err(_) if lock.path().exists() => {
                    return Err(format!(
                        "another apply holds {} ({}); if none is running, delete that file",
                        lock.path().display(),
                        read_lossy(lock.path()).unwrap_or_default().trim()
                    ))
                }
                Err(e) => return Err(e),
            }
        }
    }
}

impl Drop for ApplyLock {
    fn drop(&mut self) {
        let _ = fsguard::delete_file(&self.0);
    }
}

/// One apply's record of the owner files it changed: the replaced bytes as numbered files and a manifest line per
/// change (written before the change) with the hash sync wrote, which `restore` checks. All of it below the tool's
/// home.
struct Run {
    home: Root,
    dir: Option<Contained>,
    n: usize,
}

impl Run {
    fn new(home: &Path) -> Result<Run, String> {
        Ok(Run { home: Root::home(home)?, dir: None, n: 0 })
    }

    /// The run's backup dir, made on first use: a new dir no earlier run used.
    fn dir(&mut self) -> Result<Contained, String> {
        if let Some(d) = &self.dir {
            return Ok(d.clone());
        }
        let backups = self.home.join(&[Part::new("backups")?]);
        fsguard::create_dir_all(&backups)?;
        for n in 0.. {
            let d = backups.child(Part::new(&format!(
                "{}-{}-{n}",
                crate::ops::today(),
                crate::process::id()
            ))?);
            if fsguard::kind(d.path())? == fsguard::Kind::Missing {
                fsguard::create_dir_all(&d)?;
                fsguard::create_file(&d.child(Part::new("manifest.tsv")?), b"")?;
                self.dir = Some(d.clone());
                return Ok(d);
            }
        }
        unreachable!()
    }

    fn line(&mut self, fields: &[&str]) -> Result<(), String> {
        if fields.iter().any(|f| f.contains(['\t', '\n', '\r'])) {
            return Err(format!("{fields:?}: a tab or line break in a path; sync won't touch it"));
        }
        let dir = self.dir()?;
        fsguard::append_line(&dir.child(Part::new("manifest.tsv")?), &fields.join("\t"))
    }

    /// Keeps `bytes` and `attrs`, what an owner file held, and notes the edit about to be made.
    fn edit(
        &mut self,
        path: &Path,
        bytes: &[u8],
        attrs: &fsguard::Attrs,
        written: &str,
    ) -> Result<(), String> {
        let dir = self.dir()?;
        self.n += 1;
        let name = format!("{:04}", self.n);
        fsguard::create_file(&dir.child(Part::new(&name)?), bytes)?;
        self.line(&["edit", &path.to_string_lossy(), &name, written, &attrs.text()])
    }

    fn created(&mut self, path: &Path, written: &str) -> Result<(), String> {
        self.line(&["created", &path.to_string_lossy(), written])
    }

    /// Notes a path moved here by `set-aside`, under its number, and where it goes.
    fn aside(&mut self, path: &Path) -> Result<Contained, String> {
        let dir = self.dir()?;
        self.n += 1;
        let name = format!("{:04}", self.n);
        self.line(&["aside", &path.to_string_lossy(), &name])?;
        Ok(dir.child(Part::new(&name)?))
    }
}

/// What an apply did: one line per change, and the backup dir, if it changed an owner file.
#[derive(Clone, Debug, Default)]
pub struct Applied {
    pub done: Vec<String>,
    pub backups: Option<PathBuf>,
}

fn stale(item: &Item, what: &str) -> String {
    format!("{}: {what} since the plan; plan again (it was left as it is)", item.path.display())
}

/// The place in `roots` that contains `path`: below the deepest root that holds it, never a root made from the
/// path (design review, point 2). `None` when no verified root holds it.
fn containing(roots: &[Root], path: &Path) -> Option<Contained> {
    roots
        .iter()
        .filter_map(|r| r.below(path).map(|c| (ledger::key(r.path()).len(), c)))
        .max_by_key(|(depth, _)| *depth)
        .map(|(_, c)| c)
}

/// Puts an owner file's new text in. Its bytes are compared with what was planned and kept in the run first, so a
/// failed backup changes nothing (Sol review r3, R3-02); then it moves aside, what moved is compared again (an edit
/// made since, even mid-apply, stops it), and the new file goes in with the old one's attributes, or the item
/// refuses for an attribute sync can't set back (rule C). Any error after the move puts the old file back.
fn owner_update(item: &Item, run: &mut Run) -> Result<(), String> {
    let Dest::Owner(owner) = &item.dest else {
        return Err(format!("{}: no owner file the config names", item.path.display()));
    };
    let path = owner.path();
    let new = item.proposed.as_deref().unwrap_or("").as_bytes();
    let want = item.want.clone().unwrap_or_default();
    if item.action == Action::Create {
        if fsguard::kind(path)? != fsguard::Kind::Missing {
            return Err(stale(item, "something appeared"));
        }
        run.created(path, &want)?;
        return fsguard::put_file(owner, new);
    }
    let planned = item.current.as_deref().map(str::as_bytes);
    let now = fsguard::read(path)?;
    if now.as_deref() != planned {
        return Err(stale(item, "it changed"));
    }
    let now = now.unwrap_or_default();
    let attrs = fsguard::attrs(path)?.keep().map_err(|e| format!("{}: {e}", path.display()))?;
    run.edit(path, &now, &attrs, &want)?;
    let aside = fsguard::move_aside(owner)?;
    let put = fsguard::read(aside.path()).and_then(|moved| {
        if moved.as_deref() != Some(now.as_slice()) {
            return Err(stale(item, "it changed"));
        }
        fsguard::put_file_as(owner, new, Some(&attrs))
    });
    if let Err(e) = put {
        return Err(match aside.back() {
            Ok(()) => e,
            Err(back) => format!(
                "{e}; and the old file couldn't go back ({back}): it is at {}",
                aside.path().display()
            ),
        });
    }
    aside.delete()
}

/// Replaces or removes a tool-owned path: what is there moves aside and must still match its ledger record
/// (otherwise it goes back and apply stops); the new version goes in; the old one is deleted, following nothing.
fn tool_change(item: &Item, ledger: &mut Ledger) -> Result<(), String> {
    if item.action == Action::Forget {
        ledger.forget(&item.path);
        return Ok(());
    }
    let Dest::Tool(place) = &item.dest else {
        return Err(format!("{}: no place below a verified root", item.path.display()));
    };
    let path = place.path();
    let have = |p: &Path| -> Result<String, String> {
        Ok(match fsguard::kind(p)? {
            fsguard::Kind::Dir => fsguard::tree_sha(&fsguard::read_tree(p)?),
            fsguard::Kind::File => fsguard::sha(&fsguard::read(p)?.unwrap_or_default()),
            fsguard::Kind::Missing => String::new(),
        })
    };
    let record = ledger.get(path).cloned();
    let recorded = record.as_ref().map(|o| o.sha.clone());
    let old = match item.action {
        Action::Create | Action::Install => {
            if fsguard::kind(path)? != fsguard::Kind::Missing {
                return Err(stale(item, "something appeared"));
            }
            None
        }
        Action::Update | Action::Refresh | Action::Remove => {
            if recorded.is_none() || recorded != item.have {
                return Err(stale(item, "its ledger record changed"));
            }
            // What is there must be what the record says: a file where it records a dir is refused, and the other
            // way round (Sol review r4, R4-01).
            let on_disk = match fsguard::kind(path)? {
                fsguard::Kind::File => "file",
                fsguard::Kind::Dir => "dir",
                fsguard::Kind::Missing => "nothing",
            };
            if record.as_ref().is_some_and(|o| o.kind != on_disk) {
                return Err(stale(
                    item,
                    &format!(
                        "a {on_disk} is where its ledger record says a {}",
                        record.map(|o| o.kind).unwrap_or_default()
                    ),
                ));
            }
            let aside = fsguard::move_aside(place)?;
            if Some(have(aside.path())?) != recorded {
                aside.back()?;
                return Err(stale(item, "it changed"));
            }
            Some(aside)
        }
        _ => return Ok(()),
    };
    let put = match (item.action.clone(), &item.tree) {
        (Action::Remove, _) => Ok(None),
        (_, Some(tree)) => fsguard::put_tree(place, tree).map(Some),
        (_, None) => {
            let bytes = item.proposed.as_deref().unwrap_or("").as_bytes();
            fsguard::put_file(place, bytes).map(|()| Some(fsguard::sha(bytes)))
        }
    };
    let put = match put {
        Ok(p) => p,
        Err(e) => {
            if let Some(aside) = &old {
                aside.back()?;
            }
            return Err(e);
        }
    };
    match put {
        Some(sha) => {
            let kind = if item.tree.is_some() { "dir" } else { "file" };
            ledger.record(place, kind, &sha, &item.scope);
        }
        None => ledger.forget(path),
    }
    if let Some(aside) = old {
        aside.delete()?;
    }
    Ok(())
}

/// Carries out a plan: every change goes through the filesystem guard, at the place the plan built for it;
/// tool-owned paths change only while they match their ledger record, owner files only while they hold what was
/// planned, and nothing is removed or forgotten without the plan's proof of a complete view (rule B). Nothing to
/// change touches nothing, not even a lock (Sol review r2, R2-21).
pub fn apply(plan: &Plan) -> Result<Applied, String> {
    if plan.items.iter().any(|i| i.action == Action::Conflict) {
        return Err("the plan has conflicts; resolve them first (nothing was changed)".into());
    }
    let changing: Vec<&Item> =
        plan.items.iter().filter(|i| i.changes() && i.action != Action::RetireEntry).collect();
    if changing.is_empty() {
        return Ok(Applied::default());
    }
    if plan.complete.is_none()
        && changing.iter().any(|i| matches!(i.action, Action::Remove | Action::Forget))
    {
        return Err("the plan removes or forgets a path but has no proof the stacks loaded whole; plan again (nothing was changed)".into());
    }
    // The place an item acts on is the one the plan built for its path: an item altered since doesn't apply.
    for i in &changing {
        let at = match &i.dest {
            Dest::Tool(c) => Some(c.path()),
            Dest::Owner(o) => Some(o.path()),
            Dest::None => None,
        };
        if at.is_some_and(|a| ledger::key(a) != ledger::key(&i.path)) {
            return Err(format!(
                "{}: not the place the plan built for it; plan again (nothing was changed)",
                i.path.display()
            ));
        }
    }
    let _lock = ApplyLock::take(&plan.home, 60)?;
    let mut ledger = Ledger::load(&plan.home)?;
    if ledger.revision() != plan.ledger_revision {
        return Err(
            "the ledger changed since the plan (another sync?); plan again (nothing was changed)"
                .into(),
        );
    }
    let mut run = Run::new(&plan.home)?;
    let mut applied = Applied::default();
    for item in changing {
        let result = match item.class {
            Class::Owner => owner_update(item, &mut run),
            Class::Tool => tool_change(item, &mut ledger).and_then(|()| ledger.save(&plan.home)),
        };
        applied.backups = run.dir.as_ref().map(|d| d.path().to_path_buf());
        result?;
        // The check after each action (design: Ownership boundary, "Operating assumption"; Sol review r3, R3-05).
        if item.action != Action::Forget {
            fsguard::verify_inside(&item.path, &item.root).map_err(|e| {
                format!(
                    "{e}. Apply stopped there; {} change(s) were made before it ({}). Check what happened at that path before you sync again",
                    applied.done.len() + 1,
                    applied.backups.as_ref().map_or("no owner file changed".into(), |b| format!("backups in {}", b.display()))
                )
            })?;
        }
        applied.done.push(format!("{:?} {}", item.action, item.path.display()));
    }
    Ok(applied)
}

/// One manifest line, re-derived under the current config.
enum Undo {
    Edit { owner: OwnerFile, name: String, written: String, attrs: fsguard::Attrs },
    Created { owner: OwnerFile, written: String },
    Aside { to: Contained, from: Contained },
}

/// One manifest line, parsed whole (r7 fix check, FC-02): each kind's every field, as `Run` writes it. A path is
/// absolute, a backup name a `Part`, a hash 64 hex digits, attributes `attrs:<hex>`; anything else is no record.
enum Record<'a> {
    Edit { path: &'a Path, name: Part, written: &'a str, attrs: fsguard::Attrs },
    Created { path: &'a Path, written: &'a str },
    Aside { path: &'a Path, name: Part },
}

impl<'a> Record<'a> {
    fn parse(line: &'a str) -> Option<Record<'a>> {
        let path = |p: &'a str| Some(Path::new(p)).filter(|p| p.is_absolute());
        let hash = |h: &'a str| {
            Some(h).filter(|h| {
                h.len() == 64 && h.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
            })
        };
        let fields: Vec<&'a str> = line.split('\t').collect();
        Some(match fields.as_slice() {
            ["edit", p, name, written, attrs] => Record::Edit {
                path: path(p)?,
                name: Part::new(name).ok()?,
                written: hash(written)?,
                attrs: fsguard::Attrs::parse(attrs)?,
            },
            ["created", p, written] => Record::Created { path: path(p)?, written: hash(written)? },
            ["aside", p, name] => Record::Aside { path: path(p)?, name: Part::new(name).ok()? },
            _ => return None,
        })
    }
}

/// Undoes one apply's owner-file edits from its backup dir, last first: a file gets its old bytes back, and a file
/// apply created goes, each only while it still holds exactly what sync wrote (Sol review r2, R2-03). A path moved
/// there by `set-aside` comes back where nothing is. Every line must re-derive under `plan`, the current config
/// (design review, point 3): an edit's file one the config names now, a set-aside path within one of the plan's
/// verified roots, and an edited file one whose attributes as they are now sync may replace (rule C, as apply). If
/// any doesn't, or a line doesn't parse, nothing changes, and each such line is listed with its path and where its
/// bytes are in the backup; the tool never guesses or skips a line (Sol review r7, R7-06/R7-07). Tool-owned paths aren't restored: sync regenerates them, or
/// `lords uninstall` removes them. Returns one line per step.
pub fn restore(plan: &Plan, backup_dir: &Path) -> Result<Vec<String>, String> {
    let home = Root::home(&plan.home)?;
    let bdir = home.below(backup_dir).ok_or_else(|| {
        format!(
            "{}: no backup dir of this tool's home ({})",
            backup_dir.display(),
            plan.home.display()
        )
    })?;
    let manifest = fsguard::read_text(&backup_dir.join("manifest.tsv"))?
        .ok_or_else(|| format!("{}: no manifest", backup_dir.display()))?;
    let owner = |path: &Path| {
        plan.owner_files.iter().find(|o| ledger::key(o.path()) == ledger::key(path)).cloned()
    };
    let mut undo = Vec::new();
    let mut refused = Vec::new();
    for line in manifest.lines().rev() {
        let kept = |name: &str| format!("{}", backup_dir.join(name).display());
        // A record that doesn't parse completely refuses the whole restore, never is skipped (R7-07, FC-02).
        let Some(record) = Record::parse(line) else {
            refused.push(format!("a manifest line that doesn't parse: {line:?}"));
            continue;
        };
        match record {
            Record::Edit { path, name, written, attrs } => {
                let (path, name, written) = (path.to_path_buf(), name.as_str(), written);
                match owner(&path) {
                    Some(o) => {
                        // The file as it is now must be one sync may replace, as apply checks it (Sol review r7,
                        // R7-06): one with an attribute it can't set back is refused here, before any change.
                        let now = match fsguard::kind(&path)? {
                            fsguard::Kind::File => fsguard::attrs(&path)?.keep().err(),
                            _ => None,
                        };
                        match now {
                            Some(e) => refused.push(format!(
                                "{}: {e}; its old bytes are in {}",
                                path.display(),
                                kept(name)
                            )),
                            None => undo.push((
                                path.clone(),
                                Undo::Edit {
                                    owner: o,
                                    name: name.to_string(),
                                    written: written.to_string(),
                                    attrs,
                                },
                            )),
                        }
                    }
                    None => refused.push(format!(
                        "{}: no file the config names now; its old bytes are in {}",
                        path.display(),
                        kept(name)
                    )),
                }
            }
            Record::Created { path, written } => match owner(path) {
                Some(o) => undo.push((
                    path.to_path_buf(),
                    Undo::Created { owner: o, written: written.to_string() },
                )),
                None => refused.push(format!(
                    "{}: no file the config names now (apply created it; the backup {} holds no bytes of it)",
                    path.display(),
                    backup_dir.display()
                )),
            },
            Record::Aside { path, name } => match containing(&plan.roots, path) {
                Some(to) => undo.push((
                    path.to_path_buf(),
                    Undo::Aside { to, from: bdir.child(name) },
                )),
                None => refused.push(format!(
                    "{}: in no root this config verifies; it is in {}",
                    path.display(),
                    kept(name.as_str())
                )),
            },
        }
    }
    if !refused.is_empty() {
        return Err(format!(
            "restore refused, nothing changed: {} line(s) of {} don't re-derive under the current config; recover them by hand: {}",
            refused.len(),
            backup_dir.display(),
            refused.join("; ")
        ));
    }
    let _lock = ApplyLock::take(&plan.home, 60)?;
    let mut done = Vec::new();
    for (path, step) in undo {
        let holds = |sha: &str| -> Result<bool, String> {
            Ok(fsguard::read(&path)?.is_some_and(|b| fsguard::sha(&b) == sha))
        };
        let step = match step {
            Undo::Edit { owner, name, written, attrs } if holds(&written)? => {
                // A numbered backup that isn't there stops the restore, never puts an empty file back (r5).
                let old = fsguard::read(&backup_dir.join(&name))?.ok_or_else(|| {
                    format!("{}: the backup {name} is missing", backup_dir.display())
                })?;
                // Checked again as it is replaced, like apply (rule C; R7-06).
                fsguard::attrs(&path)?.keep().map_err(|e| format!("{}: {e}", path.display()))?;
                let aside = fsguard::move_aside(&owner)?;
                if fsguard::read(aside.path())?.is_some_and(|b| fsguard::sha(&b) == written) {
                    // The owner file's attributes as apply found them (Sol review r4, R4-03).
                    if let Err(e) = fsguard::put_file_as(&owner, &old, Some(&attrs)) {
                        aside.back()?;
                        return Err(e);
                    }
                    aside.delete()?;
                    format!("restored {}", path.display())
                } else {
                    aside.back()?;
                    format!("kept {} (changed while restoring)", path.display())
                }
            }
            // A file the owner has marked since (anything but what sync leaves) is theirs to remove (rule C).
            Undo::Created { written, .. }
                if holds(&written)? && !fsguard::attrs(&path)?.as_sync_leaves() =>
            {
                format!(
                    "kept {} (marked {} since apply)",
                    path.display(),
                    fsguard::attrs(&path)?.describe()
                )
            }
            Undo::Created { owner, written } if holds(&written)? => {
                fsguard::move_aside(&owner)?.delete()?;
                format!("removed {}", path.display())
            }
            Undo::Aside { to, from } => match fsguard::kind(to.path())? {
                fsguard::Kind::Missing => {
                    fsguard::move_to(&from, &to)?;
                    format!("moved back {}", path.display())
                }
                _ => format!("kept {} (something is there now)", path.display()),
            },
            Undo::Edit { .. } | Undo::Created { .. } => {
                format!("kept {} (changed since apply)", path.display())
            }
        };
        done.push(step);
    }
    Ok(done)
}

/// Records paths as the tool's (design: Ownership boundary, the changeover): each must be in the plan as a path
/// that isn't in the ledger but holds byte for byte what sync writes there. A human runs this for paths written
/// before the ledger existed.
pub fn adopt(plan: &Plan, paths: &[PathBuf]) -> Result<Vec<String>, String> {
    let _lock = ApplyLock::take(&plan.home, 60)?;
    let mut ledger = Ledger::load(&plan.home)?;
    if ledger.revision() != plan.ledger_revision {
        return Err("the ledger changed since the plan; plan again".into());
    }
    let mut done = Vec::new();
    for p in paths {
        let item =
            plan.items.iter().find(|i| same_place(&i.path, p) && i.adoptable).ok_or_else(|| {
                format!(
                    "{}: not adoptable (it must hold exactly what sync writes there)",
                    p.display()
                )
            })?;
        let Dest::Tool(place) = &item.dest else {
            return Err(format!("{}: no place below a verified root", p.display()));
        };
        let (kind, now) = match fsguard::kind(place.path())? {
            fsguard::Kind::Dir => ("dir", fsguard::tree_sha(&fsguard::read_tree(place.path())?)),
            fsguard::Kind::File => {
                ("file", fsguard::sha(&fsguard::read(place.path())?.unwrap_or_default()))
            }
            fsguard::Kind::Missing => return Err(format!("{}: gone", p.display())),
        };
        if Some(&now) != item.want.as_ref() {
            return Err(format!("{}: changed since the plan", p.display()));
        }
        ledger.record(place, kind, &now, &item.scope);
        done.push(format!("adopted {}", item.path.display()));
    }
    ledger.save(&plan.home)?;
    Ok(done)
}

/// Moves paths that aren't the tool's into a new backup dir, whole (a human's step for leftovers from before the
/// ledger, design: Ownership boundary). Each path must lie within one of `plan`'s verified roots (design review,
/// point 2): a target's dir, a checkout or worktree, a workspace; anything else is refused, as is a ledger path,
/// anything holding a link, and a move across volumes (it moves, never copies and deletes). `restore` moves them
/// back.
pub fn set_aside(plan: &Plan, paths: &[PathBuf]) -> Result<Applied, String> {
    let _lock = ApplyLock::take(&plan.home, 60)?;
    let ledger = Ledger::load(&plan.home)?;
    let mut run = Run::new(&plan.home)?;
    let mut applied = Applied::default();
    for p in paths {
        if ledger.get(p).is_some() {
            return Err(format!(
                "{} is the tool's (in the ledger): `lords uninstall` removes it",
                p.display()
            ));
        }
        let from = containing(&plan.roots, p).ok_or_else(|| {
            format!(
                "{}: in no root this plan verified (a target's dir, a checkout or worktree, a workspace); set-aside moves nothing else",
                p.display()
            )
        })?;
        match fsguard::kind(p)? {
            fsguard::Kind::Missing => return Err(format!("{}: nothing there", p.display())),
            fsguard::Kind::Dir => {
                fsguard::read_tree(p)?;
            }
            fsguard::Kind::File => {}
        }
        let to = run.aside(p)?;
        fsguard::move_to(&from, &to)?;
        applied.done.push(format!("set aside {} (to {})", p.display(), to.path().display()));
        applied.backups = run.dir.as_ref().map(|d| d.path().to_path_buf());
    }
    Ok(applied)
}
/// The stack skills a place installs (copied into each checkout for harnesses that read a skills dir there).
fn place_skills(stacks: &Stacks, layers: Layers) -> Vec<&Entry> {
    let ctx = Context {
        audience: vec![Audience::Opus, Audience::Sol, Audience::Astra],
        layers,
        public_only: false,
        skip_stores: Vec::new(),
        roles: None,
        by_reference: false,
        role_dir: None,
        relative_to: None,
        default_role: None,
        default_role_inline: false,
    };
    compose::applicable(stacks, &ctx)
        .into_iter()
        .filter(|e| {
            e.meta.kind == Kind::Skill
                && !matches!(e.meta.origin, crate::model::Origin::InRepo { .. })
        })
        .collect()
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
    ctx.default_role_inline = adapter.default_role_inline();
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
pub fn drift_findings(stacks: &Stacks, home: &Path) -> Vec<crate::Finding> {
    use crate::Finding;
    if stacks.private.is_none() {
        return Vec::new();
    }
    match plan(stacks, &Options { user_only: true, home: home.to_path_buf(), ..Options::default() })
    {
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

/// `sync --apply` as one step, under one lock: a plan that can't apply stops it before anything changes; then every
/// memory store is imported (one commit each) and ended temporary entries are retired (one commit each), each
/// writing only stack files git tracks and holds unmodified, or new ones (Sol review r2, R2-05, in ops.rs); then a
/// fresh plan is applied. Returns what changed. The CLI and the GUI both call this.
pub fn import_and_apply(
    public: &Path,
    private: Option<&Path>,
    opts: &Options,
) -> Result<Applied, String> {
    let stacks = Stacks::load(public, private)?;
    let first = plan(&stacks, opts)?;
    if let Some(c) = first.items.iter().find(|i| i.action == Action::Conflict) {
        return Err(format!(
            "the plan has conflicts, e.g. {}: {}; resolve them first (nothing was changed)",
            c.path.display(),
            c.notes.join("; ")
        ));
    }
    let mut done = Vec::new();
    // The install stage imports and retires nothing.
    if opts.stage == Stage::Full {
        let _lock = ApplyLock::take(&opts.home, 60)?;
        if let Some(p) = &stacks.private {
            if !p.manifest.memory_stores.is_empty() {
                for r in crate::ops::import(p, None, true)? {
                    if let Some(c) = r.commit {
                        done.push(format!("imported memory from {}: commit {c}", r.store));
                    }
                }
            }
        }
        // Ended temporary entries leave the stacks' context for good: retired, with a commit each.
        let stacks = Stacks::load(public, private)?;
        for (id, commit) in crate::ops::retire_expired(&stacks, &crate::ops::today(), true)? {
            done.push(format!(
                "retired expired {id}{}",
                commit.map(|c| format!(": commit {c}")).unwrap_or_default()
            ));
        }
    }
    let stacks = Stacks::load(public, private)?;
    let mut applied = apply(&plan(&stacks, opts)?)?;
    done.append(&mut applied.done);
    applied.done = done;
    Ok(applied)
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
    let mut out = Vec::new();
    for (name, repo) in &private.manifest.repos {
        match checkouts(repo) {
            Ok((paths, _, _)) => dirs.extend(paths.into_iter().map(|r| r.path().to_path_buf())),
            Err(e) => out.push(crate::Finding::error("checkouts", format!("repo {name}: {e}"))),
        }
    }
    dirs.extend(private.manifest.workspaces.values().map(|w| expand_home(&w.path)));
    for d in dirs {
        let Some(tracked) = read_or_finding(&d.join("AGENTS.md"), &mut out) else { continue };
        if let Some(over) = read_or_finding(&d.join("AGENTS.override.md"), &mut out) {
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
        if let Some(local) = read_or_finding(&d.join("CLAUDE.local.md"), &mut out) {
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
        // A misconfigured repo is an error in `verbatim_findings`; it has no worktrees to list here.
        let Ok((paths, skipped, _)) = checkouts(repo) else { continue };
        if claude(&repo.adapters) {
            places.extend(
                paths.into_iter().map(|p| (format!("repo {name}"), p.path().to_path_buf())),
            );
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
    /// The adapter whose agents read it (its id); empty for what every adapter's agents read (role files, skill
    /// copies).
    pub adapter: String,
    /// The adapter's file, or a role file.
    pub file: String,
    pub bytes: usize,
    /// What counts against the limit: this file (the user-level file is outside a place's budget; 0 for role
    /// files and skill copies).
    pub reads: usize,
    /// The adapter's limit on a place's project files, if any; none at the user level.
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

    /// One line for a reader who has to make it smaller: the place, the adapter and its file, what a session
    /// there reads against the limit, and each section's bytes (orchestrator, 25 Oct).
    pub fn describe(&self) -> String {
        let who = if self.adapter.is_empty() {
            "every adapter".to_string()
        } else {
            self.adapter.clone()
        };
        let against = match self.limit {
            Some(l) => format!("{} of {l} bytes ({} %)", self.reads, self.reads * 100 / l),
            None => format!("{} bytes", self.bytes),
        };
        let parts: Vec<String> = self.sections.iter().map(|(s, n)| format!("{s} {n}")).collect();
        format!(
            "{}, {who} ({}): {against}; by section: {}",
            self.place,
            self.file,
            parts.join(", ")
        )
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
    for adapter in adapters::ALL.iter().copied() {
        let text = adapter.render(&user_composed(stacks, adapter));
        // The limit is on a place's project files; the user-level file is read whole (adapters.rs, `byte_limit`).
        out.push(Size {
            place: "user level".into(),
            adapter: adapter.id().to_string(),
            file: adapter.id().to_string(),
            bytes: text.len(),
            reads: text.len(),
            limit: None,
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
                adapter: adapter.id().to_string(),
                file: adapter.repo_file().to_string(),
                bytes: text.len(),
                reads: text.len(),
                limit: adapter.byte_limit(),
                sections: sections(&text, &rendered),
                enabled,
            });
        }
        // The skill copies in each checkout and worktree (the adapters that read a skills dir there).
        let copying = adapters::ALL.iter().any(|a| {
            a.repo_skills_dir().is_some() && (ids.is_empty() || ids.iter().any(|i| i == a.id()))
        });
        let skills = place_skills(stacks, place_layers(stacks, &name, false));
        if copying && !skills.is_empty() {
            let mut parts: Vec<(String, usize)> = skills
                .iter()
                .map(|e| {
                    let src = e.meta_file.parent().unwrap_or(Path::new("."));
                    let n = skill_tree(src).map(|t| t.values().map(Vec::len).sum()).unwrap_or(0);
                    (e.id().trim_start_matches("skill.").to_string(), n)
                })
                .collect();
            let each: usize = parts.iter().map(|(_, n)| n).sum();
            let paths = match private.manifest.repos.get(&name) {
                Some(repo) => checkouts(repo).map_or(repo.checkouts.len(), |c| c.0.len()).max(1),
                None => 1,
            };
            parts.push((format!("in all {paths}"), each * paths));
            out.push(Size {
                place: label.clone(),
                adapter: String::new(),
                file: "skill copies".into(),
                bytes: each,
                reads: 0,
                limit: None,
                sections: parts,
                enabled: true,
            });
        }
        for role in declared_roles(stacks) {
            let text = role_composed(stacks, place_layers(stacks, &name, true), &role).text;
            out.push(Size {
                place: label.clone(),
                adapter: String::new(),
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

/// Each place's file (with the tracked instructions it carries) against its adapter's limit.
fn repo_size_findings(stacks: &Stacks) -> Vec<crate::Finding> {
    let all = sizes(stacks);
    let copies = all.iter().filter(|s| s.file == "skill copies").map(|s| {
        let total = s.sections.last().map_or(s.bytes, |(_, n)| *n);
        crate::Finding::info(
            "skill-copies",
            format!(
                "{}: skill copies take {} KB in each checkout or worktree, {} KB {}",
                s.place,
                s.bytes.div_ceil(1024),
                total.div_ceil(1024),
                s.sections.last().map(|(w, _)| w.as_str()).unwrap_or("")
            ),
        )
    });
    let copies: Vec<crate::Finding> = copies.collect();
    let mut out: Vec<crate::Finding> = all
        .into_iter()
        .filter(|s| s.over_line())
        .map(|s| crate::Finding::warning("size", s.describe()))
        .collect();
    out.extend(copies);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    // Spec (orchestrator, 26 Oct: "an edit that only adds lines can't reveal anything, so it needs no untracked
    // listing at all"): an edit only adds when it removes no pattern and adds no negation (`!`), which could reveal
    // a path; comments and blank lines don't count.
    #[test]
    fn an_exclude_edit_only_adds_when_it_removes_nothing_and_negates_nothing() {
        assert!(only_adds("# git's own\n", "# git's own\n# Lords Command\n/CLAUDE.local.md\n"));
        assert!(only_adds("a\n", "a\n\n# a comment\nb\n"));
        assert!(only_adds("a\n# old comment\n", "a\nb\n"), "a comment going is no pattern going");
        assert!(!only_adds("a\nb\n", "a\n"), "a pattern going can reveal a path");
        assert!(!only_adds("a\n", "a\n!b\n"), "a negation can reveal a path");
        assert!(!only_adds(".lords/\n", "/CLAUDE.local.md\n"));
    }

    // Spec (Sol review r4, R4-04; r5, decision 7: "a `cfg(test)` seam that injects `Unsupported` at `hard_link`.
    // Assert the final path is absent or the old file is back with its bytes and attributes"): an owner update
    // whose link fails leaves the old file in place, its bytes and attributes as they were, and nothing beside it.
    #[test]
    fn a_failed_link_puts_the_owner_file_back_whole() {
        let dir = tempfile::TempDir::new().unwrap();
        let (path, home) = (dir.path().join("out").join("owner.md"), dir.path().join("home"));
        // Read-only and hidden (Windows), read-only (elsewhere).
        let marked =
            fsguard::Attrs::parse(if cfg!(windows) { "attrs:3" } else { "attrs:124" }).unwrap();
        let owner = OwnerFile::for_test(&path);
        fsguard::put_file_as(&owner, b"OLD\n", Some(&marked)).unwrap();
        // The attributes an owner sets (read-only, hidden, system; the mode elsewhere): Windows itself sets
        // "archive" on a rename.
        let marks = |a: fsguard::Attrs| a.bits() & if cfg!(windows) { 0x7 } else { 0o777 };
        let before = fsguard::attrs(&path).unwrap();
        assert_eq!(marks(before), marks(marked));
        let mut item = Item::new("t", path.clone(), Action::Update);
        item.dest = Dest::Owner(owner);
        item.current = Some("OLD\n".into());
        item.proposed = Some("NEW\n".into());
        item.want = Some(fsguard::sha(b"NEW\n"));
        fsguard::fail_links(true);
        let result = owner_update(&item, &mut Run::new(&home).unwrap());
        fsguard::fail_links(false);
        assert!(result.is_err(), "{result:?}");
        assert_eq!(fsguard::read(&path).unwrap().as_deref(), Some(&b"OLD\n"[..]));
        assert_eq!(marks(fsguard::attrs(&path).unwrap()), marks(before));
        assert_eq!(fsguard::list_dir(path.parent().unwrap()).map(|v| v.len()).ok(), Some(1));
    }
}
