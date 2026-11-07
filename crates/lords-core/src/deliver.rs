//! `lords deliver` (F8, design note Harness/notes/2026-11-07-lords-f8-delivery.md): a commit to a store, or a new
//! worktree, reaches its places with nobody running a command. One safe-only plan (D13), made and applied under the
//! apply lock, place by place; whatever it held, or why it failed, goes into one typed state file that `lords paths`
//! reads.

use crate::stack::Delivery;
use crate::sync;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// Set in a delivery's own environment, so the commits it makes (its store work) start no delivery: the post-commit
/// hook exits at once when it is set (C7).
pub const MARKER: &str = "LORDS_DELIVERING";

/// What `lords deliver --protocol` answers: sync names a copy in a git hook only while it answers this one.
pub const PROTOCOL: u32 = 1;

/// One delivery's scope and trigger.
#[derive(Clone, Debug)]
pub struct Options {
    /// The tool home: its ledger, its apply lock and the state file.
    pub home: PathBuf,
    /// One new worktree (its post-checkout hook): its repo and its dir. `None`: the whole machine.
    pub worktree: Option<(String, PathBuf)>,
    /// The store a commit was made in (its post-commit hook): only a store `[delivery] on_commit` lists starts a
    /// delivery, and only when the stores' committed stacks moved since the last one.
    pub on_commit: Option<PathBuf>,
}

impl Options {
    pub fn whole(home: &Path) -> Options {
        Options { home: home.to_path_buf(), worktree: None, on_commit: None }
    }

    pub fn worktree(home: &Path, repo: &str, place: &Path) -> Options {
        Options {
            home: home.to_path_buf(),
            worktree: Some((repo.to_string(), place.to_path_buf())),
            on_commit: None,
        }
    }
}

/// What a delivery did and left.
#[derive(Clone, Debug, Default)]
pub struct Report {
    /// What changed, one line each.
    pub done: Vec<String>,
    /// The state as this delivery left it.
    pub state: State,
    /// Where the state file is.
    pub file: PathBuf,
    /// `on_commit`, and the stores' committed stacks hadn't moved: nothing planned.
    pub unchanged: bool,
    /// `on_commit` names a store `[delivery] on_commit` doesn't list: nothing done.
    pub not_a_trigger: bool,
    /// Plans made.
    pub attempts: u32,
}

impl Report {
    /// Whether something is held: a place, everything, or a repo's hook (exit 1). Store work alone isn't.
    pub fn holds(&self) -> bool {
        !self.state.places.is_empty()
            || !self.state.everything.is_empty()
            || !self.state.hooks.is_empty()
    }
}

/// The state file (`[delivery] state`), format 1: what the last deliveries held or failed, with what resolves each.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct State {
    pub format: u32,
    /// Each store's committed stack, as the last whole delivery composed from it.
    #[serde(default)]
    pub trees: Vec<StoreTree>,
    /// Why the last whole delivery failed, if it did.
    #[serde(default)]
    pub failed: Option<Failed>,
    /// Each place held whole.
    #[serde(default)]
    pub places: Vec<StatePlace>,
    /// Items of no place that hold every place (a dead memory index).
    #[serde(default)]
    pub everything: Vec<StateItem>,
    /// Repos whose post-checkout hook isn't as this build writes it.
    #[serde(default)]
    pub hooks: Vec<StateNote>,
    /// Store work held: a memory import or a retirement whose commit was refused.
    #[serde(default)]
    pub store_work: Vec<StateNote>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoreTree {
    pub stack: String,
    pub tree: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Failed {
    pub why: String,
    pub command: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StatePlace {
    pub place: String,
    pub items: Vec<StateItem>,
    /// What resolves it.
    pub command: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StateItem {
    pub path: String,
    /// D13's `Hold` (`changed-copy`, `new-place`, …), `not-ready`, or `write-refused` (a check or write that failed
    /// at apply).
    pub reason: String,
    #[serde(default)]
    pub note: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StateNote {
    pub what: String,
    pub why: String,
    pub command: String,
}

/// The state file's place below the tool home (`[delivery] state`).
pub fn state_file(home: &Path, settings: &Delivery) -> Result<PathBuf, String> {
    let _ = settings;
    Ok(home.join("state").join("delivery.json"))
}

/// The state, or `None` where there is none.
pub fn read(home: &Path, settings: &Delivery) -> Result<Option<State>, String> {
    let _ = (home, settings);
    Ok(None)
}

/// Whether delivery is set up with this tool home: the state file is there, or the ledger records a store's
/// post-commit hook.
pub fn configured(home: &Path, settings: &Delivery) -> bool {
    let _ = (home, settings);
    false
}

/// One delivery (module doc).
pub fn run(public: &Path, private: Option<&Path>, opts: &Options) -> Result<Report, String> {
    run_between(public, private, opts, &mut |_, _| {})
}

/// `run`, calling `between` at each step with the place it concerns (the tool home for the plan).
#[doc(hidden)]
pub fn run_between(
    public: &Path,
    private: Option<&Path>,
    opts: &Options,
    between: &mut dyn FnMut(sync::Step, &Path),
) -> Result<Report, String> {
    // The surface only (tests first): as before this package, nothing delivers.
    let _ = (public, private, opts, between);
    Ok(Report::default())
}
