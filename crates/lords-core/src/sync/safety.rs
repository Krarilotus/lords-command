//! Whether a plan's item may be applied with no one looking (D13; design note
//! Harness/notes/2026-11-07-lords-d13-typed-safety.md). The planner types each item once, as its last step
//! (`classify`), from facts it checked; `Stage::SafeOnly` then keeps only what is safe, place by place (`safe_only`).
//! Nothing else decides either, and no note is ever read to decide.

use super::{Action, Change, Class, ConflictKind, Dest, Item, Plan};
use crate::fsguard::Root;
use crate::ledger;
use crate::stack::SafeAlso;
use serde::{Serialize, Serializer};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

/// Whether an item may be applied with no one looking.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "class", content = "reason", rename_all = "kebab-case")]
pub enum Safety {
    /// Writes nothing; or writes only a tool-owned path that held what its ledger record says when planned; or creates
    /// a tool-owned path where nothing is, in a place the ledger already records a path in.
    Safe,
    /// Drops the ledger record of a path that is gone, in a place that is gone too, planned from a complete view.
    SafeForget,
    /// Anything else: only a sync by hand applies it.
    NeedsOwner(Hold),
}

/// Why an item needs its owner.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Hold {
    /// An owner file's Lords part: settings, an instruction file's block, a git exclude.
    OwnerFile,
    /// A path to create in a place where the ledger records nothing yet.
    NewPlace,
    /// A tool-owned path or block the stacks no longer want.
    Removal,
    /// A copy its owner changed: kept as it is, or no longer wanted and left with its record dropped.
    ChangedCopy,
    /// A tool-owned path the planner couldn't read or check.
    Unchecked,
    /// A place's file its harness would cut.
    OverLimit,
    /// Any other conflict.
    Conflict,
    /// The record of a gone path whose place is still there.
    PlaceStillThere,
    /// A forget planned without a complete view.
    IncompleteView,
    /// An ended temporary entry, retired with a commit in its stack: store work, which holds no place.
    Retirement,
    /// Not typed by the planner (`Item::new`'s default): held, never applied.
    Unclassified,
}

impl Hold {
    /// Store work: a commit to a stack, which only a sync by hand makes. It changes no place's files.
    pub fn is_store_work(self) -> bool {
        self == Hold::Retirement
    }
}

impl std::fmt::Display for Hold {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Hold::OwnerFile => "owner-file",
            Hold::NewPlace => "new-place",
            Hold::Removal => "removal",
            Hold::ChangedCopy => "changed-copy",
            Hold::Unchecked => "unchecked",
            Hold::OverLimit => "over-limit",
            Hold::Conflict => "conflict",
            Hold::PlaceStillThere => "place-still-there",
            Hold::IncompleteView => "incomplete-view",
            Hold::Retirement => "retirement",
            Hold::Unclassified => "unclassified",
        })
    }
}

/// A place a plan writes in (F8, design note K3), by what names it: a user-level target, or a checkout, worktree or
/// workspace by its root. What `Stage::SafeOnly` holds whole.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PlaceId {
    Target(String),
    Dir(PathBuf),
}

impl PlaceId {
    /// One form for every spelling of the place.
    pub fn key(&self) -> String {
        match self {
            PlaceId::Target(n) => format!("target:{n}"),
            PlaceId::Dir(p) => ledger::key(p),
        }
    }
}

impl std::fmt::Display for PlaceId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PlaceId::Target(n) => write!(f, "target {n}"),
            PlaceId::Dir(p) => write!(f, "{}", p.display()),
        }
    }
}

impl Serialize for PlaceId {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.collect_str(self)
    }
}

/// A memory store with changes since its last import: store work, which `Stage::SafeOnly` never does.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct PendingImport {
    pub store: String,
    pub added: usize,
    pub changed: usize,
}

/// An item `Stage::SafeOnly` left out, and why.
#[derive(Clone, Debug, Serialize)]
pub struct HeldItem {
    pub target: String,
    pub path: PathBuf,
    pub reason: Hold,
    /// The planner's first note on it, for people.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub note: String,
}

/// A place held whole, with the items that hold it.
#[derive(Clone, Debug, Serialize)]
pub struct HeldPlace {
    pub place: PlaceId,
    pub items: Vec<HeldItem>,
}

/// What `Stage::SafeOnly` left out (`Plan::held`).
#[derive(Clone, Debug, Default, Serialize)]
pub struct Held {
    /// Each place held whole.
    pub places: Vec<HeldPlace>,
    /// Items of no place that need the owner (a dead memory index): each holds every place.
    pub everything: Vec<HeldItem>,
    /// Store work, never done here: ended entries to retire. Pending memory imports are `Plan::imports`.
    pub retirements: Vec<HeldItem>,
}

impl Held {
    /// Whether any place is held (the CLI's exit 1). Store work holds none.
    pub fn holds(&self) -> bool {
        !self.places.is_empty() || !self.everything.is_empty()
    }
}

/// Every root the ledger records a path in: a place Lords already manages (design note D4).
pub(crate) fn managed(ledger: &ledger::Ledger) -> BTreeSet<String> {
    ledger.owned.values().map(|o| ledger::key(Path::new(&o.root))).collect()
}

/// The place of an item a repo or workspace made: the deepest of its roots the item's path lies below (a worktree
/// inside another checkout's dir is its own place), else the path itself (a conflict about the repo as a whole). Never
/// the item's `root`: a conflict is made with its own path as its root (F8, H6).
pub(crate) fn place_in(roots: &[Root], path: &Path) -> PlaceId {
    let key = ledger::key(path);
    let below = |r: &&Root| key.starts_with(&format!("{}/", ledger::key(r.path())));
    let deepest = roots.iter().filter(below).max_by_key(|r| ledger::key(r.path()).len());
    PlaceId::Dir(deepest.map_or(path, |r| r.path()).to_path_buf())
}

/// A ledger record's place: its target, or the root the ledger records it in.
pub(crate) fn record_place(o: &ledger::Owned) -> PlaceId {
    match o.scope.strip_prefix("target:") {
        Some(name) => PlaceId::Target(name.to_string()),
        None => PlaceId::Dir(PathBuf::from(&o.root)),
    }
}

/// The planner's last step: each item's class, from the facts the planner checked and set on it (its action, class,
/// conflict kind, destination, `gone` and `place_gone`), whether the plan has its complete view, and the roots the
/// ledger records paths in; and what the stack lets `--safe-only` apply besides (`[safe_only] applies`, by default
/// nothing: the owner, 7 Nov, "make thigns more configurable"). Notes are never read.
pub(crate) fn classify(plan: &mut Plan, managed: &BTreeSet<String>, also: &[SafeAlso]) {
    let complete = plan.complete.is_some();
    for i in &mut plan.items {
        i.safety = class_of(i, managed, also, complete);
    }
}

fn class_of(i: &Item, managed: &BTreeSet<String>, also: &[SafeAlso], complete: bool) -> Safety {
    let need = Safety::NeedsOwner;
    // A tool-owned path below a root the plan verified, where apply acts (rule A).
    let tool = i.class == Class::Tool && i.change() == Change::ToolOwned;
    let owner_file = matches!(i.change(), Change::OwnerEdit | Change::ExcludeEdit);
    let exclude = i.change() == Change::ExcludeEdit && also.contains(&SafeAlso::Exclude);
    match i.action {
        // Nothing written: a tracked file shown, or a path already as wanted.
        Action::InSync => Safety::Safe,
        Action::Report if i.class == Class::Owner => Safety::Safe,
        // A tool-owned path reported: one the planner couldn't read or check (F8 sol-loop r3 S4).
        Action::Report => need(Hold::Unchecked),
        // The planner makes these only where the ledger's record matched the disk (`Planner::tool_file`, `tool_dir`).
        Action::Update | Action::Refresh if tool => Safety::Safe,
        // And these only where nothing is.
        Action::Create | Action::Install if tool => match &i.dest {
            Dest::Tool(at) if managed.contains(&ledger::key(at.root().path())) => Safety::Safe,
            _ if also.contains(&SafeAlso::NewPlace) => Safety::Safe,
            _ => need(Hold::NewPlace),
        },
        // Sync's own lines in a checkout's git exclude, where the stack names them.
        Action::Create | Action::Update if exclude => Safety::Safe,
        Action::Create | Action::Update if owner_file => need(Hold::OwnerFile),
        Action::Remove | Action::RemoveBlock => need(Hold::Removal),
        Action::Conflict => need(match i.conflict {
            ConflictKind::ModifiedCopy => Hold::ChangedCopy,
            ConflictKind::OverLimit => Hold::OverLimit,
            ConflictKind::Other => Hold::Conflict,
        }),
        // A copy its owner changed that the stacks no longer want: it stays, and its record would go.
        Action::Forget if !i.gone => need(Hold::ChangedCopy),
        Action::Forget if !i.place_gone => need(Hold::PlaceStillThere),
        Action::Forget if !complete => need(Hold::IncompleteView),
        Action::Forget => Safety::SafeForget,
        Action::RetireEntry => need(Hold::Retirement),
        _ => need(Hold::Unclassified),
    }
}

/// `Stage::SafeOnly`: keeps only the safe items of places that hold no other, and says what it left out
/// (`Plan::held`). Only `safety` and `places` decide: an item that needs the owner holds each of its places; one of no
/// place holds every place, unless it is store work, which holds only itself (it changes no place's files, and no
/// place's text waits on it).
pub(crate) fn safe_only(plan: &mut Plan) {
    let mut held = Held::default();
    let mut places: BTreeMap<String, HeldPlace> = BTreeMap::new();
    for i in &plan.items {
        let Safety::NeedsOwner(reason) = i.safety else { continue };
        let note = i.notes.first().cloned().unwrap_or_default();
        let it = HeldItem { target: i.target.clone(), path: i.path.clone(), reason, note };
        if reason.is_store_work() {
            held.retirements.push(it);
        } else if i.places.is_empty() {
            held.everything.push(it);
        } else {
            for p in &i.places {
                let place = HeldPlace { place: p.clone(), items: Vec::new() };
                places.entry(p.key()).or_insert(place).items.push(it.clone());
            }
        }
    }
    let all = !held.everything.is_empty();
    plan.items.retain(|i| {
        matches!(i.safety, Safety::Safe | Safety::SafeForget)
            && !all
            && !i.places.iter().any(|p| places.contains_key(&p.key()))
    });
    held.places = places.into_values().collect();
    plan.held = Some(held);
}
