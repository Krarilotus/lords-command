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
use crate::fsguard::{self, Tree};
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
}

impl Item {
    fn new(target: &str, path: PathBuf, action: Action) -> Item {
        Item {
            target: target.to_string(),
            root: path.clone(),
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
    /// The tool's own dir the plan was made for (its ledger, backups and lock).
    #[serde(skip)]
    pub home: PathBuf,
    /// The ledger's revision when planned: apply refuses a plan made against another ledger.
    #[serde(skip)]
    pub ledger_revision: String,
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
fn owner_file_item(target: &str, path: &Path, proposed: String, composed: &Composed) -> Item {
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
    path: &Path,
    block: &str,
    replace: bool,
    composed: &Composed,
) -> Item {
    let current = match fsguard::read_text(path) {
        Ok(c) => c.unwrap_or_default(),
        Err(e) => return conflict(target, path, e),
    };
    match compose::place_block(&current, block, replace) {
        Ok(proposed) => owner_file_item(target, path, proposed, composed),
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
}

/// Every place sync puts a tool-owned path (design: Ownership boundary), as one grammar: a scope names roots (a
/// target's dir or shared file, a repo's checkouts and worktrees, a workspace's dir) and the slots in them. Sync
/// asks a slot where to write; the ledger check accepts a record only for a path, kind and scope that a slot of
/// that scope produces (Sol review r4, R4-01). Neither keeps its own idea of the layout.
#[derive(Clone, Copy)]
enum Slot {
    /// A user-level skills target's copy of one skill: `<target dir>/<skill>`.
    TargetSkill,
    /// The shared file of an instructions target with `via`: the root itself.
    Shared,
    /// An adapter's file in a place: `<checkout>/CLAUDE.local.md`, `<checkout>/AGENTS.override.md`.
    AdapterFile(&'static dyn Adapter),
    /// A place's role files: `<checkout>/.lords`.
    RoleDir,
    /// An adapter's copy of one skill in a place: `<checkout>/<skills dir>/<skill>`.
    AdapterSkill(&'static dyn Adapter),
}

impl Slot {
    /// The path of this slot in `root`; `name` is the skill's, for the two skill slots, and must be a name a skill
    /// can have: a slug, as the schema's ids (Sol review r5, R5-02). `None` for any other name.
    fn path(self, root: &Path, name: &str) -> Option<PathBuf> {
        let skill = |dir: PathBuf| stack::is_slug(name).then(|| dir.join(name));
        match self {
            Slot::TargetSkill => skill(root.to_path_buf()),
            Slot::Shared => Some(root.to_path_buf()),
            Slot::AdapterFile(a) => Some(root.join(a.repo_file())),
            Slot::RoleDir => Some(root.join(ROLE_DIR)),
            Slot::AdapterSkill(a) => skill(root.join(a.repo_skills_dir()?)),
        }
    }

    fn kind(self) -> &'static str {
        match self {
            Slot::Shared | Slot::AdapterFile(_) => "file",
            Slot::TargetSkill | Slot::RoleDir | Slot::AdapterSkill(_) => "dir",
        }
    }
}

/// A scope's roots and the slots in them.
type Places = (Vec<PathBuf>, Vec<Slot>);

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

impl Planner<'_> {
    fn cover(&mut self, scope: &str, under: Option<&Path>) {
        self.covered.push((scope.to_string(), under.map(Path::to_path_buf)));
    }

    /// The roots a scope names and the slots sync writes there; none for a scope that names nothing declared, or
    /// an adapter its place doesn't write for. A repo's roots are its real checkouts and worktrees, never the
    /// dirs worktrees are kept in (R4-01), and a repo whose `checkouts` name anything else is an error (R5-01).
    /// Target names are unique (the stack refuses two of one name: R5-06), so `target:<name>` is one target.
    fn places_of(&mut self, scope: &str) -> Result<Places, String> {
        let stacks = self.stacks;
        self.places
            .entry(scope.to_string())
            .or_insert_with(|| {
                let none = Ok((Vec::new(), Vec::new()));
                let Some(m) = stacks.private.as_ref().map(|p| &p.manifest) else { return none };
                let (base, adapter) = match scope.split_once('/') {
                    Some((b, a)) => (b, Some(a)),
                    None => (scope, None),
                };
                let (kind, name) = base.split_once(':').unwrap_or(("", ""));
                let place = |roots: Vec<PathBuf>, declared: &[String]| match adapter {
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
                            Ok((vec![expand_home(&t.path)], vec![Slot::TargetSkill]))
                        }
                        Some(t) if t.kind == "instructions" => {
                            Ok((t.via.iter().map(|v| expand_home(v)).collect(), vec![Slot::Shared]))
                        }
                        _ => none,
                    },
                    ("repo", _) => match m.repos.get(name) {
                        Some(r) => place(
                            checkouts(r).map_err(|e| format!("repo {name}: {e}"))?.0,
                            &r.adapters,
                        ),
                        None => none,
                    },
                    ("workspace", _) => match m.workspaces.get(name) {
                        Some(w) => place(vec![expand_home(&w.path)], &w.adapters),
                        None => none,
                    },
                    _ => none,
                }
            })
            .clone()
    }

    /// The root of `scope` in which a slot of that scope, of `kind`, produces exactly `path` (Sol review r3,
    /// R3-03; r4, R4-01). A skill slot takes the path's last name as the skill's, and only a name a skill can have
    /// (r5, R5-02). An error when the scope's place is misconfigured (R5-01).
    fn placed(&mut self, scope: &str, path: &Path, kind: &str) -> Result<Option<PathBuf>, String> {
        if let Some(dir) = scope.strip_prefix("hooks:") {
            // The ledger checked the hook's name and dir when it loaded.
            return Ok(path
                .parent()
                .filter(|p| kind == "file" && ledger::key(p) == dir)
                .map(Path::to_path_buf));
        }
        let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let want = ledger::key(path);
        let (roots, slots) = self.places_of(scope)?;
        Ok(roots.into_iter().find(|root| {
            slots.iter().any(|s| {
                s.kind() == kind && s.path(root, &name).is_some_and(|p| ledger::key(&p) == want)
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
    /// target comes back.
    fn check_ledger(&mut self) -> Result<(), String> {
        let records: Vec<ledger::Owned> = self.ledger.owned.values().cloned().collect();
        for o in records {
            let path = PathBuf::from(&o.path);
            if !self.declared(&o.scope) {
                self.plan.notes.push(format!(
                    "the ledger records {} for {}, which the stack no longer declares: left as it is, and never acted on; declare it again and `lords uninstall` removes it",
                    o.path, o.scope
                ));
                continue;
            }
            let placed = self.placed(&o.scope, &path, &o.kind).map_err(|e| {
                format!(
                    "the ledger ({}) records {} for {}, whose place is misconfigured: {e}; sync does nothing until it is put right (nothing was changed)",
                    ledger::file(&self.plan.home).display(),
                    o.path,
                    o.scope
                )
            })?;
            if placed.is_some() || fsguard::kind(&path).is_ok_and(|k| k == fsguard::Kind::Missing) {
                continue;
            }
            return Err(format!(
                "the ledger ({}) records {} for {}, but that is no place {} has; sync does nothing until the ledger is put right (nothing was changed)",
                ledger::file(&self.plan.home).display(),
                o.path,
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
        path: &Path,
        content: String,
        composed: &Composed,
    ) -> Item {
        self.wanted.insert(ledger::key(path));
        let want = fsguard::sha(content.as_bytes());
        let mut item = Item::new(target, path.to_path_buf(), Action::Create);
        item.class = Class::Tool;
        item.scope = scope.to_string();
        item.want = Some(want.clone());
        match self.placed(scope, path, "file") {
            Ok(Some(root)) => item.root = root,
            Ok(None) => return conflict(target, path, format!("no place {scope} has")),
            Err(e) => return conflict(target, path, e),
        }
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
                        return conflict(
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
        path: &Path,
        tree: std::sync::Arc<Tree>,
    ) -> Item {
        self.wanted.insert(ledger::key(path));
        let want = fsguard::tree_sha(&tree);
        let mut item = Item::new(target, path.to_path_buf(), Action::Install);
        item.class = Class::Tool;
        item.scope = scope.to_string();
        item.want = Some(want.clone());
        match self.placed(scope, path, "dir") {
            Ok(Some(root)) => item.root = root,
            Ok(None) => return conflict(target, path, format!("no place {scope} has")),
            Err(e) => return conflict(target, path, e),
        }
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
                        return conflict(
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
            let path = PathBuf::from(&o.path);
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
            // `check_ledger` let through only records in place, or whose path is gone (which only lose the record).
            item.root = self
                .placed(&o.scope, &path, &o.kind)
                .ok()
                .flatten()
                .unwrap_or_else(|| path.clone());
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
            let shared = Slot::Shared.path(&expand_home(via), "").expect("a slot without a name");
            let Some(line) = adapter.import_line(&shared) else {
                return vec![conflict(
                    &t.name,
                    &path,
                    format!(
                        "adapter {} doesn't expand includes, so `via` can't work",
                        adapter.id()
                    ),
                )];
            };
            let mut s = pl.tool_file(
                &format!("shared text ({})", t.name),
                &scope,
                &shared,
                text.clone(),
                &composed,
            );
            size_note(&mut s, adapter, &text);
            items.push(s);
            if composed.text.is_empty() {
                String::new()
            } else {
                compose::block(&line)
            }
        }
    };
    let mut item = owner_block_item(&t.name, &path, &block, replace, &composed);
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

/// A settings target (an owner file): the adapter's exclude list in a JSON settings file holds every path in
/// `exclude`. Only that one key changes; the rest of the file keeps its bytes.
fn settings_item(t: &Target, adapter: &dyn Adapter) -> Item {
    let path = expand_home(&t.path);
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
    owner_file_item(&t.name, &path, proposed, &Composed::default())
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
        let Some(dest) = Slot::TargetSkill.path(&dir, &name) else {
            items.push(conflict(
                &t.name,
                &dir.join(&name),
                format!("skill {}: no skill name sync writes", e.id()),
            ));
            continue;
        };
        match pl.tree_of(e) {
            Ok(tree) => items.push(pl.tool_dir(&t.name, &scope, &dest, tree)),
            Err(err) => items.push(conflict(&t.name, &dest, format!("skill {}: {err}", e.id()))),
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

/// Every checkout and (if asked) worktree of a repo, and the worktrees skipped for lying outside
/// `worktree_roots`. A configured checkout counts only where git's own top level is exactly that path (Sol review
/// r5, R5-01; decision 1): a dir that is no checkout, or one inside a checkout, refuses the whole place, as does a
/// git call that fails. A worktree is one git lists, at the root git gives.
fn checkouts(repo: &stack::Repo) -> Result<(Vec<PathBuf>, Vec<PathBuf>), String> {
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
        let top = git::run(&c, &["rev-parse", "--show-toplevel"]).map_err(|e| {
            format!("checkout {} is no git checkout ({e}); put the checkout's root in `checkouts` of the private stack.toml", c.display())
        })?;
        if ledger::key(Path::new(top.trim())) != ledger::key(&c) {
            return Err(format!(
                "checkout {} is no checkout's root: git's top level there is {}; put that root in `checkouts` of the private stack.toml",
                c.display(),
                top.trim()
            ));
        }
        seen.push(norm(&c));
        out.push(c.clone());
        if !repo.worktrees {
            continue;
        }
        let list = git::run(&c, &["worktree", "list", "--porcelain"])?;
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
    Ok((out, skipped))
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
    let mut excludes: Vec<(PathBuf, Vec<String>)> = Vec::new();
    // A git that fails here is a conflict, never a repo without an exclude file (Sol review r5, decision 1).
    let mut add_exclude = |p: &Path, what: &str| -> Result<(), String> {
        if place.workspace {
            return Ok(());
        }
        let common = git::run(p, &["rev-parse", "--path-format=absolute", "--git-common-dir"])?;
        let ex = PathBuf::from(common.trim()).join("info").join("exclude");
        match excludes.iter_mut().find(|(e, _)| *e == ex) {
            Some((_, files)) if !files.iter().any(|f| f == what) => files.push(what.to_string()),
            Some(_) => {}
            None => excludes.push((ex, vec![what.to_string()])),
        }
        Ok(())
    };
    for adapter in chosen.iter().copied() {
        let file = adapter.repo_file();
        let scope = place_scope(place, Some(adapter));
        for p in &place.paths {
            let target = format!("{} ({file})", place.label);
            let path = Slot::AdapterFile(adapter).path(p, "").expect("a slot without a name");
            if !p.is_dir() {
                pl.plan.items.push(conflict(&target, &path, "checkout not found"));
                continue;
            }
            let tracked = if place.workspace {
                Ok(false)
            } else {
                git::status(p, &["ls-files", "--error-unmatch", "--", file])
            };
            match tracked {
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
            if let Err(e) = add_exclude(p, &format!("/{file}")) {
                pl.plan.items.push(conflict(&target, &path, e));
                continue;
            }
            let (text, composed) = place_text(pl.stacks, place.layers(false), adapter, p);
            let mut item = pl.tool_file(&target, &scope, &path, text.clone(), &composed);
            size_note(&mut item, adapter, &text);
            pl.plan.items.push(item);
        }
    }
    // Development skills, copied into each checkout for harnesses that read a skills dir there.
    let skills = place_skills(pl.stacks, place.layers(false));
    let skill_dirs: Vec<(&'static dyn Adapter, &str)> =
        chosen.iter().copied().filter_map(|a| a.repo_skills_dir().map(|d| (a, d))).collect();
    for (adapter, dir) in skill_dirs {
        let scope = place_scope(place, Some(adapter));
        for p in place.paths.iter().filter(|p| p.is_dir()) {
            for e in &skills {
                let name = e.id().trim_start_matches("skill.").to_string();
                let rel = format!("{dir}/{name}");
                let target = format!("{} (skill {name})", place.label);
                let Some(dest) = Slot::AdapterSkill(adapter).path(p, &name) else {
                    pl.plan.items.push(conflict(
                        &target,
                        &p.join(&rel),
                        format!("skill {}: no skill name sync writes", e.id()),
                    ));
                    continue;
                };
                let tracked = if place.workspace {
                    Ok(String::new())
                } else {
                    git::run(p, &["ls-files", "--", &rel])
                };
                match tracked {
                    Ok(o) if o.trim().is_empty() => {}
                    Ok(_) => {
                        pl.plan.items.push(conflict(
                            &target,
                            &dest,
                            format!("the repo tracks a skill at {rel}; rename one of them"),
                        ));
                        continue;
                    }
                    Err(e) => {
                        pl.plan.items.push(conflict(&target, &dest, e));
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
                        &dest,
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
    for p in place.paths.iter().filter(|p| p.is_dir()) {
        let item = pl.tool_dir(
            &format!("{} (role files)", place.label),
            &scope,
            &Slot::RoleDir.path(p, "").expect("a slot without a name"),
            tree.clone(),
        );
        pl.plan.items.push(item);
    }
    for (ex, files) in excludes {
        let current = match fsguard::read_text(&ex) {
            Ok(c) => c.unwrap_or_default(),
            Err(e) => {
                pl.plan.items.push(conflict(&format!("{} (git exclude)", place.label), &ex, e));
                continue;
            }
        };
        let proposed = excludes_text(&current, &files);
        pl.plan.items.push(owner_file_item(
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
        };
        let composed = compose::compose(pl.stacks, &ctx);
        let label = format!("{} (tracked AGENTS.md)", place.label);
        let mut item = match compose::place_block(&current, &composed.text, false) {
            Ok(proposed) => owner_file_item(&label, &tracked, proposed, &composed),
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
    let (mut paths, skipped) = match checkouts(repo) {
        Ok(c) => c,
        Err(e) => {
            // The place refuses whole (Sol review r5, R5-01): nothing is planned in any of its checkouts.
            let first = repo.checkouts.first().map(|c| expand_home(c)).unwrap_or_default();
            pl.plan.items.push(conflict(&format!("repo {name}"), &first, e));
            return;
        }
    };
    if let Some(one) = only_path {
        paths.retain(|p| same_place(p, one));
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
    let place = Place {
        label: format!("workspace {name}"),
        name: name.to_string(),
        workspace: true,
        project: w.project.clone(),
        adapters: &w.adapters,
        paths: vec![expand_home(&w.path)],
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
            match t.kind.as_str() {
                "instructions" => items.extend(instructions_items(&mut pl, t, adapter)),
                "skills" => items.extend(skills_items(&mut pl, t, adapter)),
                "settings" => items.push(settings_item(t, adapter)),
                k => return Err(format!("target {}: unknown kind {k}", t.name)),
            }
        }
        // Profiles sharing one `via` file plan it once; different texts for one file can't both be written.
        let mut kept: Vec<Item> = Vec::new();
        for item in items {
            match kept.iter_mut().find(|k| same_place(&k.path, &item.path)) {
                Some(k) if k.proposed == item.proposed => {}
                Some(k) => {
                    k.action = Action::Conflict;
                    k.notes.push(format!("{} wants different text in the same file", item.target));
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
            repo_items(&mut pl, name, repo, opts.adapter.as_deref(), opts.place.as_deref());
        }
        for (name, w) in &private.manifest.workspaces {
            if opts.repo.as_deref().is_some_and(|r| r != name) {
                continue;
            }
            if opts.place.is_none() {
                workspace_items(&mut pl, name, w, opts.adapter.as_deref());
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
    // Uninstall: nothing is wanted, so every tool-owned path in scope goes (owner files keep their blocks; `restore`
    // takes those back).
    if opts.uninstall {
        pl.plan.items.clear();
        pl.wanted.clear();
    }
    pl.unwanted();
    Ok(pl.plan)
}

// ---------------------------------------------------------------- apply

/// One writer of the ledger at a time on this machine (a user's sync, a post-checkout hook's, a hook
/// installation): a lock file in the tool's home, waited for up to `secs` seconds.
pub(crate) struct ApplyLock(PathBuf);

impl ApplyLock {
    pub(crate) fn take(home: &Path, secs: u64) -> Result<ApplyLock, String> {
        fsguard::create_dir_all(home)?;
        let lock = home.join("apply.lock");
        let until = std::time::Instant::now() + std::time::Duration::from_secs(secs);
        loop {
            match fsguard::create_file(&lock, format!("pid {}\n", crate::process::id()).as_bytes())
            {
                Ok(()) => return Ok(ApplyLock(lock)),
                Err(_) if lock.exists() && std::time::Instant::now() < until => {
                    std::thread::sleep(std::time::Duration::from_millis(200));
                }
                Err(_) if lock.exists() => {
                    return Err(format!(
                        "another apply holds {} ({}); if none is running, delete that file",
                        lock.display(),
                        read_lossy(&lock).unwrap_or_default().trim()
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
/// change (written before the change) with the hash sync wrote, which `restore` checks.
struct Run {
    home: PathBuf,
    dir: Option<PathBuf>,
    n: usize,
}

impl Run {
    fn new(home: &Path) -> Run {
        Run { home: home.to_path_buf(), dir: None, n: 0 }
    }

    /// The run's backup dir, made on first use: a new dir no earlier run used.
    fn dir(&mut self) -> Result<PathBuf, String> {
        if let Some(d) = &self.dir {
            return Ok(d.clone());
        }
        let root = self.home.join("backups");
        fsguard::create_dir_all(&root)?;
        for n in 0.. {
            let d = root.join(format!("{}-{}-{n}", crate::ops::today(), crate::process::id()));
            if fsguard::kind(&d)? == fsguard::Kind::Missing {
                fsguard::create_dir_all(&d)?;
                fsguard::create_file(&d.join("manifest.tsv"), b"")?;
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
        fsguard::append_line(&dir.join("manifest.tsv"), &fields.join("\t"))
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
        fsguard::create_file(&dir.join(&name), bytes)?;
        self.line(&["edit", &path.to_string_lossy(), &name, written, &attrs.text()])
    }

    fn created(&mut self, path: &Path, written: &str) -> Result<(), String> {
        self.line(&["created", &path.to_string_lossy(), written])
    }

    /// Notes a path moved here by `set-aside`, under its number.
    fn aside(&mut self, path: &Path) -> Result<PathBuf, String> {
        let dir = self.dir()?;
        self.n += 1;
        let name = format!("{:04}", self.n);
        self.line(&["aside", &path.to_string_lossy(), &name])?;
        Ok(dir.join(name))
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

/// Puts an owner file's new text in. Its bytes are compared with what was planned and kept in the run first, so a
/// failed backup changes nothing (Sol review r3, R3-02); then it moves aside, what moved is compared again (an edit
/// made since, even mid-apply, stops it), and the new file goes in with the old one's attributes (R3-06). Any error
/// after the move puts the old file back.
fn owner_update(item: &Item, run: &mut Run) -> Result<(), String> {
    let path = &item.path;
    let new = item.proposed.as_deref().unwrap_or("").as_bytes();
    let want = item.want.clone().unwrap_or_default();
    if item.action == Action::Create {
        if fsguard::kind(path)? != fsguard::Kind::Missing {
            return Err(stale(item, "something appeared"));
        }
        run.created(path, &want)?;
        return fsguard::put_file(path, new);
    }
    let planned = item.current.as_deref().map(str::as_bytes);
    let now = fsguard::read(path)?;
    if now.as_deref() != planned {
        return Err(stale(item, "it changed"));
    }
    let now = now.unwrap_or_default();
    let attrs = fsguard::attrs(path)?;
    run.edit(path, &now, &attrs, &want)?;
    let aside = fsguard::move_aside(path)?;
    let put = fsguard::read(&aside).and_then(|moved| {
        if moved.as_deref() != Some(now.as_slice()) {
            return Err(stale(item, "it changed"));
        }
        fsguard::put_file_as(path, new, Some(&attrs))
    });
    if let Err(e) = put {
        return Err(match fsguard::move_back(&aside, path) {
            Ok(()) => e,
            Err(back) => format!(
                "{e}; and the old file couldn't go back ({back}): it is at {}",
                aside.display()
            ),
        });
    }
    fsguard::delete_file(&aside)
}

/// Replaces or removes a tool-owned path: what is there moves aside and must still match its ledger record
/// (otherwise it goes back and apply stops); the new version goes in; the old one is deleted, following nothing.
fn tool_change(item: &Item, ledger: &mut Ledger) -> Result<(), String> {
    let path = &item.path;
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
            let aside = fsguard::move_aside(path)?;
            if Some(have(&aside)?) != recorded {
                fsguard::move_back(&aside, path)?;
                return Err(stale(item, "it changed"));
            }
            Some(aside)
        }
        Action::Forget => {
            ledger.forget(path);
            return Ok(());
        }
        _ => return Ok(()),
    };
    let put = match (item.action.clone(), &item.tree) {
        (Action::Remove, _) => Ok(None),
        (_, Some(tree)) => fsguard::put_tree(path, tree).map(Some),
        (_, None) => {
            let bytes = item.proposed.as_deref().unwrap_or("").as_bytes();
            fsguard::put_file(path, bytes).map(|()| Some(fsguard::sha(bytes)))
        }
    };
    let put = match put {
        Ok(p) => p,
        Err(e) => {
            if let Some(aside) = &old {
                fsguard::move_back(aside, path)?;
            }
            return Err(e);
        }
    };
    match put {
        Some(sha) => {
            let kind = if item.tree.is_some() { "dir" } else { "file" };
            ledger.record(path, kind, &sha, &item.scope);
        }
        None => ledger.forget(path),
    }
    if let Some(aside) = old {
        match fsguard::kind(&aside)? {
            fsguard::Kind::Dir => fsguard::delete_tree(&aside)?,
            _ => fsguard::delete_file(&aside)?,
        }
    }
    Ok(())
}

/// Carries out a plan: every change goes through the filesystem guard; tool-owned paths change only while they
/// match their ledger record, owner files only while they hold what was planned. Nothing to change touches
/// nothing, not even a lock (Sol review r2, R2-21).
pub fn apply(plan: &Plan) -> Result<Applied, String> {
    if plan.items.iter().any(|i| i.action == Action::Conflict) {
        return Err("the plan has conflicts; resolve them first (nothing was changed)".into());
    }
    let changing: Vec<&Item> =
        plan.items.iter().filter(|i| i.changes() && i.action != Action::RetireEntry).collect();
    if changing.is_empty() {
        return Ok(Applied::default());
    }
    let _lock = ApplyLock::take(&plan.home, 60)?;
    let mut ledger = Ledger::load(&plan.home)?;
    if ledger.revision() != plan.ledger_revision {
        return Err(
            "the ledger changed since the plan (another sync?); plan again (nothing was changed)"
                .into(),
        );
    }
    let mut run = Run::new(&plan.home);
    let mut applied = Applied::default();
    for item in changing {
        let result = match item.class {
            Class::Owner => owner_update(item, &mut run),
            Class::Tool => tool_change(item, &mut ledger).and_then(|()| ledger.save(&plan.home)),
        };
        applied.backups = run.dir.clone();
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

/// Undoes one apply's owner-file edits from its backup dir, last first: a file gets its old bytes back, and a file
/// apply created goes, each only while it still holds exactly what sync wrote (Sol review r2, R2-03). A path moved
/// there by `set-aside` comes back where nothing is. Tool-owned paths aren't restored: sync regenerates them, or
/// `lords uninstall` removes them. Returns one line per step.
pub fn restore(home: &Path, backup_dir: &Path) -> Result<Vec<String>, String> {
    let manifest = fsguard::read_text(&backup_dir.join("manifest.tsv"))?
        .ok_or_else(|| format!("{}: no manifest", backup_dir.display()))?;
    let _lock = ApplyLock::take(home, 60)?;
    let mut done = Vec::new();
    for line in manifest.lines().rev() {
        let f: Vec<&str> = line.split('\t').collect();
        let path = PathBuf::from(f.get(1).copied().unwrap_or(""));
        let holds = |sha: &str| -> Result<bool, String> {
            Ok(fsguard::read(&path)?.is_some_and(|b| fsguard::sha(&b) == sha))
        };
        let step = match f.as_slice() {
            ["edit", _, name, written, ..] if holds(written)? => {
                // A numbered backup that isn't there stops the restore, never puts an empty file back (r5).
                let old = fsguard::read(&backup_dir.join(name))?.ok_or_else(|| {
                    format!("{}: the backup {name} is missing", backup_dir.display())
                })?;
                // The owner file's attributes as apply found them (Sol review r4, R4-03).
                let attrs = f.get(4).and_then(|a| fsguard::Attrs::parse(a));
                let aside = fsguard::move_aside(&path)?;
                if fsguard::read(&aside)?.is_some_and(|b| fsguard::sha(&b) == *written) {
                    if let Err(e) = fsguard::put_file_as(&path, &old, attrs.as_ref()) {
                        fsguard::move_back(&aside, &path)?;
                        return Err(e);
                    }
                    fsguard::delete_file(&aside)?;
                    format!("restored {}", path.display())
                } else {
                    fsguard::move_back(&aside, &path)?;
                    format!("kept {} (changed while restoring)", path.display())
                }
            }
            // A file the owner has since marked (read-only, hidden, system) is theirs to remove (r5, R5-05).
            ["created", _, written] if holds(written)? && fsguard::attrs(&path)?.unusual() => {
                format!(
                    "kept {} (marked {} since apply)",
                    path.display(),
                    fsguard::attrs(&path)?.describe()
                )
            }
            ["created", _, written] if holds(written)? => {
                let aside = fsguard::move_aside(&path)?;
                fsguard::delete_file(&aside)?;
                format!("removed {}", path.display())
            }
            ["aside", _, name] => match fsguard::kind(&path)? {
                fsguard::Kind::Missing => {
                    fsguard::move_to(&backup_dir.join(name), &path)?;
                    format!("moved back {}", path.display())
                }
                _ => format!("kept {} (something is there now)", path.display()),
            },
            [kind, ..] if ["edit", "created"].contains(kind) => {
                format!("kept {} (changed since apply)", path.display())
            }
            _ => format!("skipped a manifest line: {line}"),
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
        let (kind, now) = match fsguard::kind(&item.path)? {
            fsguard::Kind::Dir => ("dir", fsguard::tree_sha(&fsguard::read_tree(&item.path)?)),
            fsguard::Kind::File => {
                ("file", fsguard::sha(&fsguard::read(&item.path)?.unwrap_or_default()))
            }
            fsguard::Kind::Missing => return Err(format!("{}: gone", p.display())),
        };
        if Some(&now) != item.want.as_ref() {
            return Err(format!("{}: changed since the plan", p.display()));
        }
        ledger.record(&item.path, kind, &now, &item.scope);
        done.push(format!("adopted {}", item.path.display()));
    }
    ledger.save(&plan.home)?;
    Ok(done)
}

/// Moves paths that aren't the tool's into a new backup dir, whole (a human's step for leftovers from before the
/// ledger, design: Ownership boundary). Refused for a ledger path, for anything holding a link, and across volumes
/// (it moves, never copies and deletes). `restore` moves them back.
pub fn set_aside(home: &Path, paths: &[PathBuf]) -> Result<Applied, String> {
    let _lock = ApplyLock::take(home, 60)?;
    let ledger = Ledger::load(home)?;
    let mut run = Run::new(home);
    let mut applied = Applied::default();
    for p in paths {
        if ledger.get(p).is_some() {
            return Err(format!(
                "{} is the tool's (in the ledger): `lords uninstall` removes it",
                p.display()
            ));
        }
        match fsguard::kind(p)? {
            fsguard::Kind::Missing => return Err(format!("{}: nothing there", p.display())),
            fsguard::Kind::Dir => {
                fsguard::read_tree(p)?;
            }
            fsguard::Kind::File => {}
        }
        let to = run.aside(p)?;
        fsguard::move_to(p, &to)?;
        applied.done.push(format!("set aside {} (to {})", p.display(), to.display()));
        applied.backups = run.dir.clone();
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
    {
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
            Ok((paths, _)) => dirs.extend(paths),
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
        let Ok((paths, skipped)) = checkouts(repo) else { continue };
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
        .collect();
    out.extend(copies);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

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
        fsguard::put_file_as(&path, b"OLD\n", Some(&marked)).unwrap();
        let before = fsguard::attrs(&path).unwrap();
        assert_eq!(before, marked);
        let mut item = Item::new("t", path.clone(), Action::Update);
        item.current = Some("OLD\n".into());
        item.proposed = Some("NEW\n".into());
        item.want = Some(fsguard::sha(b"NEW\n"));
        fsguard::fail_links(true);
        let result = owner_update(&item, &mut Run::new(&home));
        fsguard::fail_links(false);
        assert!(result.is_err(), "{result:?}");
        assert_eq!(fsguard::read(&path).unwrap().as_deref(), Some(&b"OLD\n"[..]));
        // The attributes an owner sets (read-only, hidden, system); Windows itself sets "archive" on a rename.
        assert_eq!(fsguard::attrs(&path).unwrap().describe(), before.describe());
        assert_eq!(fsguard::list_dir(path.parent().unwrap()).map(|v| v.len()).ok(), Some(1));
    }
}
