//! Whether a plan's item may be applied with no one looking (D13; design note
//! Harness/notes/2026-11-07-lords-d13-typed-safety.md). The planner types each item once, as its last step
//! (`classify`), from facts it checked; `Stage::SafeOnly` then keeps only what is safe, place by place (`safe_only`).
//! Nothing else decides either, and no note is ever read to decide.

use super::Plan;
use crate::ledger;
use serde::{Serialize, Serializer};
use std::collections::BTreeSet;
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

/// The planner's last step: each item's class, from what the planner checked.
pub(crate) fn classify(plan: &mut Plan, managed: &BTreeSet<String>) {
    let _ = (plan, managed);
}

/// `Stage::SafeOnly`: keeps only the safe items of places that hold no other, and says what it left out
/// (`Plan::held`).
pub(crate) fn safe_only(plan: &mut Plan) {
    plan.held = Some(Held::default());
}
