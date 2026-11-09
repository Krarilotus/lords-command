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
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

mod place_uninstall;
mod places;
mod safety;
mod target_uninstall;

pub use safety::{Held, HeldItem, HeldPlace, Hold, PendingImport, PlaceId, Safety};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
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
    /// A tool-owned instruction file the owner has added text to, whose managed block still matches its ledger record
    /// (`uninstall --place`): the block goes, every byte around it stays, and the record goes.
    RemoveBlock,
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
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Class {
    /// Created by sync, recorded in the ledger, replaced whole and never edited.
    Tool,
    /// The owner's file: sync changes only its managed block or one key, with a backup.
    Owner,
}

#[derive(Clone, Debug, Serialize)]
#[serde(into = "ItemOut")]
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
    /// The file's text is the owner's and may hold credentials (a settings file: hook commands, env): its current and
    /// proposed text and a text diff are never shown, only what Lords changes there (rm-guard review r1, R6). Apply
    /// still uses them.
    #[serde(skip)]
    pub(crate) redacted: bool,
    /// A settings item whose hooks will name the guard copy: apply checks it is there and answers before it writes.
    #[serde(skip)]
    pub(crate) names_guard: Option<Contained>,
    /// A forget planned because nothing was at the record's path: apply forgets it only while nothing is (sol-loop r2,
    /// S4). A forget of a copy the owner changed (`Planner::unwanted`) acts whatever is there.
    #[serde(skip)]
    pub(crate) gone: bool,
    /// The guards those hooks run: apply checks the copy still lists each (D20).
    #[serde(skip)]
    pub(crate) names_guards: Vec<String>,
    /// Among them the child guard: apply checks the copy speaks this build's child protocol too (fix check r2, FC4).
    #[serde(skip)]
    pub(crate) names_child_guard: bool,
    /// And the role guard, for this adapter: apply checks the copy speaks this build's role protocol (F10, design
    /// note D6).
    #[serde(skip)]
    pub(crate) names_role_guard: Option<&'static str>,
    /// And the rm guard, with its hook's arguments: apply checks the copy speaks this build's rm protocol (D16-R1-01).
    #[serde(skip)]
    pub(crate) names_rm_guard: Option<&'static [&'static str]>,
    /// Codex's hooks file: Lords' hooks as rendered for it, whose trust apply settles once it wrote the file
    /// (`codex_trust::settle`).
    #[serde(skip)]
    pub(crate) codex_lords: Option<Vec<crate::codex_trust::Lords>>,
    /// Whether it may be applied with no one looking (D13): set once, by the planner's last step (`safety::classify`).
    pub safety: Safety,
    /// The places it belongs to, as the planner made it there (D13): what `Stage::SafeOnly` holds whole. None for an
    /// item of no place (an ended entry, a dead memory index).
    pub places: Vec<PlaceId>,
    /// A forget whose record's root, its checkout, worktree, workspace or target dir, is gone too (D13).
    #[serde(skip)]
    pub(crate) place_gone: bool,
}

/// An item as plans show it (`--json`, the app): a redacted item without its texts.
#[derive(Serialize)]
struct ItemOut {
    target: String,
    path: PathBuf,
    action: Action,
    class: Class,
    #[serde(skip_serializing_if = "String::is_empty")]
    scope: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    current: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    proposed: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    have: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    want: Option<String>,
    adoptable: bool,
    diff: String,
    notes: Vec<String>,
    safety: Safety,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    places: Vec<PlaceId>,
}

impl From<Item> for ItemOut {
    fn from(i: Item) -> ItemOut {
        let shown = !i.redacted;
        ItemOut {
            target: i.target,
            path: i.path,
            action: i.action,
            class: i.class,
            scope: i.scope,
            current: i.current.filter(|_| shown),
            proposed: i.proposed.filter(|_| shown),
            have: i.have,
            want: i.want,
            adoptable: i.adoptable,
            diff: i.diff,
            notes: i.notes,
            safety: i.safety,
            places: i.places,
        }
    }
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
    /// The one place a plan was asked for (`Options::place`) isn't a checkout or worktree of its repo yet: a new
    /// worktree still being made (F8, D22).
    NotReady,
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
    /// Only what may be applied with no one looking (D13, `Safety`): `Full`'s plan, with what the stacks no longer want
    /// listed even without a complete view, so a place wanting a removal is held, never half updated; then only the
    /// safe items of places that hold no other. Nothing is imported or retired. `Plan::held` says what was left out.
    SafeOnly,
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
            redacted: false,
            names_guard: None,
            gone: false,
            names_guards: Vec::new(),
            names_child_guard: false,
            names_role_guard: None,
            names_rm_guard: None,
            codex_lords: None,
            safety: Safety::NeedsOwner(Hold::Unclassified),
            places: Vec::new(),
            place_gone: false,
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
    /// The Codex apply asks about trust after it writes Codex's hooks (`Options::codex`).
    #[serde(skip)]
    pub codex: crate::codex_trust::Codex,
    /// Memory stores with changes since their last import: store work only a sync by hand does (D13).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub imports: Vec<PendingImport>,
    /// What `Stage::SafeOnly` left out; `None` for any other stage.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub held: Option<Held>,
    /// How long an apply of this plan waits for the apply lock, and for a reader that blocks one of its swaps
    /// (`[delivery] lock_wait_secs`).
    #[serde(skip)]
    pub lock_wait: LockWait,
    /// Each declared repo's post-checkout hook, as this build writes it (F8, C3): what `sync --apply` installs.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub hooks: Vec<crate::hooks::RepoHook>,
    /// The declared repos and workspaces this plan's scope doesn't cover (`repo app`, `workspace notes`): what
    /// `--user` or `--repo` leaves out, so "in sync" is never read as every place (gap 4b, S6).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub out_of_scope: Vec<String>,
}

/// `Plan::lock_wait`: the stacks' `[delivery] lock_wait_secs`, or its default.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LockWait(pub std::time::Duration);

impl Default for LockWait {
    fn default() -> LockWait {
        LockWait(stack::Delivery::default().lock_wait())
    }
}

/// What applying a plan releases besides its installs and updates: paths it removes, and ledger records it forgets
/// (a forget drops the record only; a copy the owner changed stays where it is).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Releases {
    pub removes: usize,
    pub forgets: usize,
}

impl Plan {
    /// Whether `apply` takes this plan, by its own refusal rules, and what it releases; or why not.
    pub fn applies(&self) -> Result<Releases, String> {
        let count = |a: Action| self.items.iter().filter(|i| i.action == a).count();
        let r = Releases {
            removes: count(Action::Remove) + count(Action::RemoveBlock),
            forgets: count(Action::Forget),
        };
        let conflicts: Vec<&Item> =
            self.items.iter().filter(|i| i.action == Action::Conflict).collect();
        if !conflicts.is_empty() {
            let mut places: BTreeSet<String> =
                conflicts.iter().flat_map(|i| i.units.iter().map(|u| u.to_string())).collect();
            if conflicts.iter().any(|i| i.units.is_empty()) {
                places.insert("no place".into());
            }
            return Err(format!(
                "{} conflict(s) in {}, and apply takes no plan with conflicts; resolve them first. It also lists {} removal(s) and {} forget(s), held until then",
                conflicts.len(),
                places.into_iter().collect::<Vec<_>>().join(", "),
                r.removes,
                r.forgets
            ));
        }
        if self.complete.is_none() && r.removes + r.forgets > 0 {
            return Err("the plan removes or forgets a path but has no proof the stacks loaded whole; plan again".into());
        }
        Ok(r)
    }
}

/// The guard copy's bytes, read through the path check (a link anywhere on the way is refused); `None` when there is
/// none.
fn guard_bytes(place: &Contained) -> Result<Option<Vec<u8>>, String> {
    fsguard::check(place.path())?;
    match fsguard::read_bytes(place.path()) {
        Ok(b) => Ok(Some(b)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(format!("{}: {e}", place.path().display())),
    }
}

/// Where the guard copy lives in the tool's home: `<tool home>/bin/lords-guard.exe`, the stable copy of `lords` that
/// harness hooks run (orchestrator, 30 Oct), never a build output a `cargo clean` or a locked rebuild could take away.
pub fn guard_place(home: &Path) -> Result<Contained, String> {
    Ok(Root::home(home)?.join(&[Part::new("bin")?, Part::new("lords-guard.exe")?]))
}

/// Whether the guard copy is there and answers its self-test: what a settings plan needs before it names it in a hook,
/// checked when planning and again when applying. Read only.
fn guard_ready(place: &Contained) -> Result<(), String> {
    // One that can't be read, or checked, names the fix too (sol-loop repair r1, R2-02).
    let read = guard_bytes(place)
        .map_err(|e| format!("guard can't be read ({e}): run `lords guard install`"))?;
    match read {
        None => Err("guard not installed: run `lords guard install`".into()),
        Some(_) => crate::process::guard_self_test(place.path())
            .map_err(|e| format!("guard not ready ({e}): run `lords guard install`")),
    }
}

/// Whether the guard copy runs every guard of `guards`, as it lists them (`process::guard_list`; D20, design note
/// 2026-11-07-lords-delivery-integrity §2): an earlier copy answers a guard it doesn't know with a usage note and lets
/// everything through, so sync never names one in a hook the copy can't run. Checked when planning and again when
/// applying. Read only.
fn guard_runs(place: &Contained, guards: &[String]) -> Result<(), String> {
    let fix = "run `lords guard install`, then sync again; the hooks stay as they are";
    let listed = crate::process::guard_list(place.path())
        .map_err(|e| format!("the guards the guard copy runs can't be told ({e}): {fix}"))?
        .ok_or_else(|| {
            format!(
                "the guard copy is from an earlier build and can't say which guards it runs: {fix}"
            )
        })?;
    let missing: Vec<String> =
        guards.iter().filter(|g| !listed.contains(g)).map(|g| format!("`{g}`")).collect();
    if missing.is_empty() {
        return Ok(());
    }
    Err(format!(
        "the guard copy runs no {} guard (it runs: {}): {fix}",
        missing.join(", "),
        listed.join(", ")
    ))
}

/// Whether the guard copy speaks this build's child protocol (fix check r2, FC4): asked with the very command line the
/// child hooks run (`process::guard_protocol`), so sync never names a copy in a hook whose arguments it refuses
/// or whose answer is another protocol's. Checked when planning and again when applying. Read only.
fn guard_speaks_child(place: &Contained, home: &Path) -> Result<(), String> {
    guard_speaks(place, Some(home), "child", &[], crate::guard::CHILD_PROTOCOL)
}

/// Whether the guard copy speaks this build's rm protocol, asked as `guard_speaks_child` asks, with the rm hook's own
/// arguments for its harness (`Adapter::rm_args`; review r1, D16-R1-01).
fn guard_speaks_rm(place: &Contained, args: &[&str]) -> Result<(), String> {
    guard_speaks(place, None, "rm", args, crate::guard::RM_PROTOCOL)
}

/// Whether the guard copy speaks this build's role protocol, asked as `guard_speaks_child` asks, with the role hook's
/// harness as `adapter`'s hook names it (design note D6; F10-R1-03).
pub(crate) fn guard_speaks_role(
    place: &Contained,
    home: &Path,
    adapter: &str,
) -> Result<(), String> {
    guard_speaks(place, Some(home), "role", &["--adapter", adapter], crate::guard::ROLE_PROTOCOL)
}

/// Whether the guard copy answers protocol `want` of guard `which` (`process::guard_protocol`).
fn guard_speaks(
    place: &Contained,
    home: Option<&Path>,
    which: &str,
    extra: &[&str],
    want: u32,
) -> Result<(), String> {
    let have = crate::process::guard_protocol(place.path(), which, home, extra)
        .map_err(|e| format!("the guard copy's {which} protocol can't be told ({e})"));
    match have {
        Ok(Some(n)) if n == want => Ok(()),
        Ok(Some(n)) => Err(format!(
            "the guard copy speaks {which} protocol {n}, this build's hooks need {want}: run `lords guard install`, then sync again; the hooks stay as they are"
        )),
        Ok(None) => Err(format!(
            "the guard copy is from an earlier build and refuses the {which} hooks this build writes (protocol {want}): run `lords guard install`, then sync again; the hooks stay as they are"
        )),
        Err(e) => Err(format!(
            "{e}: run `lords guard install`, then sync again; the hooks stay as they are"
        )),
    }
}

/// `lords guard install` (orchestrator, 30 Oct, after the rm-guard fix check r4: sync never touches the guard copy):
/// installs or updates the guard copy from `from` under the apply lock: a staged copy that must pass `guard
/// --self-test`, renamed into place; a failure leaves the old copy as it was. `true` when it wrote one, `false` when
/// the copy was the same already (and still answers).
pub fn install_guard(home: &Path, from: &Path) -> Result<bool, String> {
    let bytes = fsguard::read_bytes(from).map_err(|e| format!("{}: {e}", from.display()))?;
    let _lock = ApplyLock::take(home, 60)?;
    let place = guard_place(home)?;
    fsguard::create_dir_all(&Root::home(home)?.join(&[Part::new("bin")?]))?;
    match guard_bytes(&place)? {
        Some(have) if have == bytes => {
            crate::process::guard_self_test(place.path()).map(|()| false)
        }
        _ => fsguard::install_exe(&place, &bytes, crate::process::guard_self_test).map(|()| true),
    }
}

/// The install's sweep of the guard copies it set aside while a hook ran them (`fsguard::sweep_busy`, design note
/// 2026-11-07-lords-delivery-integrity §4): each still holding the bytes its name records goes; every other file of
/// that name comes back, with why it stays (review r1, R1-01). Under the apply lock, so never one another install set
/// aside and may still move back (sol-loop r1, S3).
pub fn guard_aside(home: &Path) -> Result<Vec<String>, String> {
    let _lock = ApplyLock::take(home, 60)?;
    fsguard::sweep_busy(&guard_place(home)?)
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
    /// With `uninstall`: only the records of this one target the stack no longer declares (`lords uninstall --target`;
    /// D21).
    pub target: Option<String>,
    /// What the apply may do: everything, or the install stage only.
    pub stage: Stage,
    /// The Codex an apply that writes Codex's hooks asks to trust Lords' own and reads trust back from (design note
    /// 2026-11-02-lords-codex-trust); none by default, and then every hook of that file is named as not known trusted.
    pub codex: crate::codex_trust::Codex,
    /// With `place`: it is a worktree `git worktree add` just made (its post-checkout hook), so a safe-only plan counts
    /// it as a place Lords manages, and its creates are safe (F8, D-4).
    pub new_worktree: bool,
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
    fsguard::read_tree(src).map(skill_files)
}

/// What a skill's dir holds as its copy has it: without Lords' sidecar, and ignoring itself.
fn skill_files(mut tree: Tree) -> Tree {
    tree.remove(stack::SIDECAR);
    tree.insert(".gitignore".into(), b"*\n".to_vec());
    tree
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
    /// A place whose child start its hooks can't deliver whole: the plan fails whatever else its role files' item
    /// has against it (repair sol-loop r2, R2).
    undeliverable: bool,
    /// The date the plan composes the starts a hook gives on, read once (`plan_on`).
    today: String,
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
            if o.scope.starts_with("hooks:") || o.scope.starts_with(crate::hooks::config::SCOPE) {
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
        // A stack loaded from its committed tree gives its skills' files from that tree too (gap 11, §0e condition 2).
        let kept = self.stacks.captured_for(e);
        self.trees
            .entry(e.id().to_string())
            .or_insert_with(|| {
                let tree = match kept {
                    Some(c) => c.tree(&src),
                    None => fsguard::read_tree(&src),
                };
                tree.map(skill_files).map(std::sync::Arc::new)
            })
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
            // The record's own place, as the ledger records it (D13): what its forget, removal or report holds.
            item.places = vec![safety::record_place(&o)];
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
                None => {
                    item.gone = true;
                    // Its place gone too, its checkout or worktree removed: a forget no one need look at (D13). A root
                    // that can't be checked isn't gone.
                    item.place_gone = fsguard::kind(Path::new(&o.root)) == Ok(fsguard::Kind::Missing);
                    item.notes.push("gone already; its ledger record goes".into())
                }
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
    home: &Path,
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
        opener: Some(opener(home)),
        child: None,
        default_role_lines: true,
        memory_file: None,
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
        largest.sort_by_key(|a| std::cmp::Reverse(a.1));
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
    let mut ctx = context(
        pl.stacks,
        adapter,
        audience_of(&t.audience, adapter),
        Layers::Global,
        &pl.plan.home,
    );
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
/// `exclude`, and its tool-call hooks hold one Lords guard per name in `guards` (guard.rs). Only those two keys change;
/// the rest of the file keeps its bytes. Hooks that aren't Lords' keep their content and order; the `hooks` value
/// is written whole, so their spacing may change (sol-loop r1, S3, declined as a nit: no behaviour changes). An
/// adapter with no exclude setting (Codex's `hooks.json`) takes a target with no `exclude`: its hooks only.
fn settings_item(pl: &mut Planner, t: &Target, adapter: &dyn Adapter) -> Item {
    let owner = OwnerFile::target(t);
    pl.plan.owner_files.push(owner.clone());
    let path = owner.path().to_path_buf();
    let key = adapter.excludes_key();
    if key.is_none() && !t.exclude.is_empty() {
        return conflict(
            &t.name,
            &path,
            format!("adapter {} has no exclude setting", adapter.id()),
        );
    }
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
    let mut added = false;
    let mut added_excludes = Vec::new();
    let mut list = serde_json::Value::Null;
    if let Some(key) = key {
        let current =
            value.as_object_mut().expect("an object").entry(key).or_insert(serde_json::json!([]));
        let Some(current) = current.as_array_mut() else {
            return conflict(&t.name, &path, format!("`{key}` isn't a list; left alone"));
        };
        // One path, however its separators and case are spelt (Sol review r2, R2-20).
        let same = |a: &str, b: &str| ledger::key(Path::new(a)) == ledger::key(Path::new(b));
        for e in &t.exclude {
            let want = adapters::slashes(&expand_home(e));
            if !current.iter().any(|v| v.as_str().is_some_and(|s| same(s, &want))) {
                added_excludes.push(want.clone());
                current.push(serde_json::Value::String(want));
                added = true;
            }
        }
        list = serde_json::Value::Array(current.clone());
    }
    let guard = match guard_place(&pl.plan.home) {
        Ok(g) => g,
        Err(e) => return conflict(&t.name, &path, e),
    };
    // Sync never writes the guard copy (`lords guard install` does): a target naming guards is planned only from a copy
    // that is there and answers. Any other is a conflict naming the fix, its settings as they are, never planned as if
    // it named none, which took Lords' hooks out for good, even after an install put the copy back (review r1, R1-02).
    // Every settings item apply writes for such a target names the copy, so apply checks it again under the lock.
    let guards: &[String] = &t.guards;
    if !guards.is_empty() {
        if let Err(why) = guard_ready(&guard) {
            return conflict(&t.name, &path, why);
        }
    }
    // A copy that answers but can't run a guard the target names, or refuses this build's child hooks, is refused, with
    // the hooks it has kept (D20; fix check r2, FC4): taking them out would leave the harness without the guards that
    // still work.
    if !guards.is_empty() {
        if let Err(e) = guard_runs(&guard, guards) {
            return conflict(&t.name, &path, e);
        }
    }
    let child = guards.iter().any(|g| g == "child");
    if child {
        if let Err(e) = guard_speaks_child(&guard, &pl.plan.home) {
            return conflict(&t.name, &path, e);
        }
    }
    let role = guards.iter().any(|g| g == "role").then(|| adapter.id());
    if let Some(id) = role {
        if let Err(e) = guard_speaks_role(&guard, &pl.plan.home, id) {
            return conflict(&t.name, &path, e);
        }
    }
    let rm = guards.iter().any(|g| g == "rm").then(|| adapter.rm_args());
    if let Some(args) = rm {
        if let Err(e) = guard_speaks_rm(&guard, args) {
            return conflict(&t.name, &path, e);
        }
    }
    let hooks = match guard_hooks(guards, adapter, &value, guard.path(), &pl.plan.home) {
        Ok(h) => h,
        Err(e) => return conflict(&t.name, &path, e),
    };
    let before_hooks = value.get("hooks").cloned().unwrap_or(serde_json::Value::Null);
    if let Some(h) = &hooks {
        value.as_object_mut().expect("an object").insert("hooks".into(), h.clone());
    }
    let proposed = match &current {
        None => serde_json::to_string_pretty(&value).expect("JSON serialises") + "\n",
        Some(c) => {
            let mut text = c.clone();
            if let (true, Some(key)) = (added, key) {
                text = match json_set_key(&text, key, &list) {
                    Ok(t) => t,
                    Err(e) => return conflict(&t.name, &path, e),
                };
            }
            if let Some(h) = &hooks {
                text = match json_set_key(&text, "hooks", h) {
                    Ok(t) => t,
                    Err(e) => return conflict(&t.name, &path, e),
                };
            }
            text
        }
    };
    let mut item =
        owner_file_item(&t.name, &path, Dest::Owner(owner), proposed, &Composed::default());
    item.redacted = true;
    item.names_guard = (!guards.is_empty()).then(|| guard.clone());
    item.names_guards = guards.to_vec();
    item.names_child_guard = child;
    item.names_role_guard = role;
    item.names_rm_guard = rm;
    // Codex keys trust by position (design note 2026-11-02-lords-codex-trust): Lords' hooks as rendered here are the
    // only ones apply trusts once it wrote the file, and the plan says what the write does to trust (§2.5).
    if adapter.id() == "codex" {
        let lords = lords_hooks(adapter, guards, guard.path(), &pl.plan.home);
        if let Some(h) = &hooks {
            let was = serde_json::json!({ "hooks": before_hooks });
            let will = serde_json::json!({ "hooks": h });
            item.notes.extend(crate::codex_trust::foresee(&was, &will, &lords));
        }
        item.codex_lords = Some(lords);
    }
    item.notes.extend(not_lords(&before_hooks, adapter, guard.path(), &pl.plan.home));
    let names = |h: &serde_json::Value| guard_names(h, adapter, guard.path(), &pl.plan.home);
    let changed = hooks.as_ref().map(|h| (names(&before_hooks), names(h)));
    if let (Some((before, after)), Some(why)) = (&changed, adapter.guard_note()) {
        if after.iter().any(|g| !before.contains(g)) {
            item.notes.push(why.to_string());
        }
    }
    item.diff = settings_summary(&added_excludes, changed);
    item
}

/// The hook event each guard runs on, and its matcher: `rm` before a shell call (Bash or PowerShell, an exact list in
/// both harnesses; design note 2026-11-02-lords-git-help-guard §3), `queue` when a turn would end, `session`
/// when a session starts, `turn` when a prompt does (never a native subagent's `SubagentStop`), `child` when a
/// native subagent starts, `time` before an editor write or a T3 message, and `helper` before a native helper starts.
fn guard_event(name: &str) -> Option<(&'static str, Option<&'static str>)> {
    match name {
        "rm" => Some(("PreToolUse", Some("Bash|PowerShell"))),
        "queue" => Some(("Stop", None)),
        "session" => Some(("SessionStart", None)),
        "turn" => Some(("UserPromptSubmit", None)),
        // Every agent type: a native child of any kind starts as the stack's `child_role`.
        "child" => Some(("SubagentStart", None)),
        // A session whose launch names its role (F10).
        "role" => Some(("UserPromptSubmit", None)),
        // An editor write or a T3 message (design note 2026-11-02-lords-time-guard, §2).
        "time" => Some(("PreToolUse", Some(crate::time_guard::MATCHER))),
        // Claude Code's native helper tool (design note 2026-11-02-lords-native-helper-guard, §2).
        "helper" => Some(("PreToolUse", Some(crate::helper_guard::MATCHER))),
        _ => None,
    }
}

/// Lords' hooks for `guards` as `guard_hooks` writes them for `adapter`, each with the event Codex lists it under: the
/// only hooks sync trusts in Codex (design note 2026-11-02-lords-codex-trust, I2).
pub(crate) fn lords_hooks(
    adapter: &dyn Adapter,
    guards: &[String],
    guard: &Path,
    home: &Path,
) -> Vec<crate::codex_trust::Lords> {
    let (exe, home) = (adapters::slashes(guard), adapters::slashes(home));
    let hook = |g: &String| {
        let (event, _) = guard_event(g)?;
        let hook = adapter.guard_hooks(&exe, g, &home).into_iter().next()?;
        let (guard, event) = (g.clone(), crate::codex_trust::event_name(event));
        Some(crate::codex_trust::Lords { guard, event, command: hook["command"].as_str()?.into() })
    };
    guards.iter().filter_map(hook).collect()
}

/// The hook events Lords' guards are written to.
const GUARD_EVENTS: [&str; 5] =
    ["PreToolUse", "Stop", "SessionStart", "UserPromptSubmit", "SubagentStart"];

/// The names of Lords' guard hooks (`Adapter::guard_of`, for copy `guard` and tool home `home`) a settings `hooks` value
/// holds, each once (settings an earlier build of the child guard wrote hold it as three hooks, one per part).
pub(crate) fn guard_names(
    hooks: &serde_json::Value,
    adapter: &dyn Adapter,
    guard: &Path,
    home: &Path,
) -> Vec<String> {
    let (exe, home) = (adapters::slashes(guard), adapters::slashes(home));
    let mut names: Vec<String> = GUARD_EVENTS
        .iter()
        .filter_map(|ev| hooks[*ev].as_array())
        .flatten()
        .filter_map(|e| e["hooks"].as_array())
        .flatten()
        .filter_map(|h| adapter.guard_of(h, &exe, &home))
        .collect();
    names.dedup();
    names
}

/// Where a settings `hooks` value holds a hook that runs the guard copy but isn't Lords' (`Adapter::runs_copy`, not
/// `Adapter::guard_of`), by event, entry and hook, never by its text (it may hold a credential; rm-guard review r1, R6):
/// sync leaves each as it is and says so (D18).
fn not_lords(
    hooks: &serde_json::Value,
    adapter: &dyn Adapter,
    guard: &Path,
    home: &Path,
) -> Vec<String> {
    let (exe, home) = (adapters::slashes(guard), adapters::slashes(home));
    let mut found = Vec::new();
    for ev in GUARD_EVENTS {
        for (i, e) in hooks[ev].as_array().into_iter().flatten().enumerate() {
            for (j, h) in e["hooks"].as_array().into_iter().flatten().enumerate() {
                if adapter.runs_copy(h, &exe) && adapter.guard_of(h, &exe, &home).is_none() {
                    found.push(format!(
                        "`hooks.{ev}` entry {}, hook {}: runs Lords' guard copy but isn't a hook Lords writes, so sync leaves it as it is",
                        i + 1,
                        j + 1
                    ));
                }
            }
        }
    }
    found
}

/// What a settings plan shows: what Lords changes there, never the settings' text (rm-guard review r1, R6).
fn settings_summary(
    added_excludes: &[String],
    guards: Option<(Vec<String>, Vec<String>)>,
) -> String {
    let mut out = String::new();
    for e in added_excludes {
        out.push_str(&format!("Lords exclude added: {e}\n"));
    }
    let Some((before, after)) = guards else { return out };
    let added: Vec<&String> = after.iter().filter(|g| !before.contains(g)).collect();
    let removed: Vec<&String> = before.iter().filter(|g| !after.contains(g)).collect();
    for g in &added {
        let at = match guard_event(g) {
            Some((event, Some(matcher))) => format!(" ({event}, {matcher})"),
            Some((event, None)) => format!(" ({event})"),
            None => String::new(),
        };
        out.push_str(&format!("Lords hook added: the {g} guard{at}\n"));
    }
    for g in &removed {
        out.push_str(&format!("Lords hook removed: the {g} guard\n"));
    }
    if added.is_empty() && removed.is_empty() {
        for g in &after {
            out.push_str(&format!("Lords hook updated: the {g} guard\n"));
        }
    }
    out
}

/// The settings' `hooks` with one Lords guard per name in the target's `guards`, each on its event (`guard_event`), or
/// `None` when they already hold exactly those. A guard runs the `lords` that syncs, in the adapter's form
/// (`Adapter::guard_hook`: Claude Code's exec form, `command` and `args`, no shell, as `lords hooks install` does for
/// git hooks; Codex's command line). A hook is Lords' when its command is one a Lords build rendered for this copy and
/// home (`Adapter::guard_of`; D18); any other hook stays as it is, and so does every event Lords writes no guard to.
fn guard_hooks(
    guards: &[String],
    adapter: &dyn Adapter,
    settings: &serde_json::Value,
    guard: &Path,
    home: &Path,
) -> Result<Option<serde_json::Value>, String> {
    let current = settings.get("hooks").cloned().unwrap_or(serde_json::Value::Null);
    // Lords' hook: one whose command is byte for byte one a Lords build rendered for this copy and home (D18; sol-loop
    // r1, S1). Any other hook, whatever it runs, is never Lords'.
    let exe = adapters::slashes(guard);
    let home = adapters::slashes(home);
    let ours = |h: &serde_json::Value| adapter.guard_of(h, &exe, &home).is_some();
    let lords_entry =
        |e: &serde_json::Value| e["hooks"].as_array().is_some_and(|hs| hs.iter().any(&ours));
    let has_lords = GUARD_EVENTS
        .iter()
        .any(|ev| current[*ev].as_array().is_some_and(|a| a.iter().any(lords_entry)));
    if guards.is_empty() && !has_lords {
        return Ok(None);
    }
    let mut wanted = Vec::new();
    for g in guards {
        let (event, matcher) = guard_event(g).ok_or_else(|| {
            format!(
                "no guard `{g}`; there are: rm, queue, session, turn, child, role, time, helper"
            )
        })?;
        if !adapter.guards().contains(&g.as_str()) {
            return Err(format!(
                "adapter {} runs no `{g}` guard (it runs: {})",
                adapter.id(),
                adapter.guards().join(", ")
            ));
        }
        wanted.push((g, event, matcher));
    }
    let mut hooks = match current {
        serde_json::Value::Null => serde_json::json!({}),
        v @ serde_json::Value::Object(_) => v,
        _ => return Err("`hooks` isn't an object; left alone".into()),
    };
    let before = hooks.clone();
    let events = hooks.as_object_mut().expect("an object");
    let entry = |g: &str, matcher: Option<&str>| {
        let hooks = adapter.guard_hooks(&exe, g, &home);
        match matcher {
            Some(m) => serde_json::json!({ "matcher": m, "hooks": hooks }),
            None => serde_json::json!({ "hooks": hooks }),
        }
    };
    // Every hook keeps its event, group and handler index, which Codex keys trust by (design note
    // 2026-11-02-lords-codex-trust, I3; D17): a wanted guard's hook is rewritten where Lords wrote it, in its own entry
    // on its event, and a new one is appended. Any other hook of Lords' goes, and its entry with it once it held
    // nothing else (sol-loop r1, S2): the one change that moves later hooks.
    let mut placed = BTreeSet::new();
    for ev in GUARD_EVENTS {
        let Some(list) = events.get_mut(ev) else { continue };
        let Some(list) = list.as_array_mut() else {
            if wanted.iter().any(|(_, e, _)| *e == ev) {
                return Err(format!("`hooks.{ev}` isn't a list; left alone"));
            }
            continue;
        };
        let mut emptied = Vec::new();
        for (i, e) in list.iter_mut().enumerate() {
            let alone = match e["hooks"].as_array().map(Vec::as_slice) {
                Some([h]) => adapter.guard_of(h, &exe, &home),
                _ => None,
            };
            let here = wanted.iter().find(|(g, event, _)| {
                alone.as_deref() == Some(g.as_str()) && *event == ev && !placed.contains(g)
            });
            if let Some((g, _, matcher)) = here {
                placed.insert(*g);
                *e = entry(g, *matcher);
                emptied.push(false);
                continue;
            }
            // An entry Lords' hook shares with others: the hook is rewritten in its slot while the entry's matcher is its
            // guard's (sol-loop r1, S1); under another matcher, Lords' can't go on without moving the others' hooks, so
            // the target is a conflict, before any write (review r1, R1-01).
            let shared = e["matcher"].as_str().map(str::to_string);
            let Some(hs) = e.get_mut("hooks").and_then(|h| h.as_array_mut()) else {
                emptied.push(false);
                continue;
            };
            let held = hs.len();
            let mut kept = Vec::with_capacity(held);
            for h in hs.drain(..) {
                let Some(name) = adapter.guard_of(&h, &exe, &home) else {
                    kept.push(h);
                    continue;
                };
                let slot = wanted
                    .iter()
                    .find(|(g, event, _)| **g == name && *event == ev && !placed.contains(g));
                match slot {
                    Some((g, _, m)) if *m == shared.as_deref() => {
                        placed.insert(*g);
                        kept.extend(adapter.guard_hooks(&exe, g, &home));
                    }
                    Some((g, _, m)) => {
                        let (has, wants) = (shared.as_deref(), m.unwrap_or("none"));
                        return Err(format!(
                            "`hooks.{ev}` entry {} holds Lords' {g} hook beside others' hooks under matcher {}, not Lords' {wants}; sync moves no one else's hook: give Lords' hook an entry of its own, or that entry Lords' matcher",
                            i + 1,
                            has.unwrap_or("none")
                        ));
                    }
                    None => {}
                }
            }
            emptied.push(held > 0 && kept.is_empty());
            *hs = kept;
        }
        let mut emptied = emptied.into_iter();
        list.retain(|_| !emptied.next().unwrap_or(false));
    }
    for (g, event, matcher) in wanted.iter().filter(|(g, ..)| !placed.contains(g)) {
        let list = events.entry(*event).or_insert(serde_json::json!([]));
        list.as_array_mut().expect("a list, checked above").push(entry(g, *matcher));
    }
    if hooks == before {
        return Ok(None);
    }
    Ok(Some(hooks))
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
    // A role no stack declares selects nothing, which would make what the target installed look unwanted: a conflict,
    // before the target's scope is covered, so nothing of it is removed (Codex switch-on review r1, R3).
    let declared = declared_roles(pl.stacks);
    if let Some(r) = t.roles.iter().find(|r| !declared.contains(r)) {
        return vec![conflict(&t.name, &dir, format!("role `{r}` is in no stack's roles list"))];
    }
    let root = match Root::target(t) {
        Ok(r) => r,
        Err(e) => return vec![conflict(&t.name, &dir, e)],
    };
    pl.verified(&root);
    let mut ctx = context(
        pl.stacks,
        adapter,
        audience_of(&t.audience, adapter),
        Layers::Global,
        &pl.plan.home,
    );
    // A target that names roles also takes the global skills for those roles, as a file that must stand alone takes
    // every global entry (orchestrator, 30 Oct, item 2: Codex's user-level skills); one that names none keeps only
    // skills without roles (25 Oct).
    if !t.roles.is_empty() {
        ctx.roles = Some(t.roles.clone());
        ctx.layers = Layers::Workspace { name: String::new(), project: None, global: true };
    }
    let wanted: Vec<&Entry> = compose::applicable(pl.stacks, &ctx)
        .into_iter()
        .filter(|e| e.meta.kind == Kind::Skill && matches!(e.scope, crate::model::Scope::Global))
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
    // On Windows a name that differs only in case is the same file, so git is asked case-insensitively and the answer
    // folded (sol-loop sync-perf r1, S2): a tracked `claude.local.md` is where `CLAUDE.local.md` would be written.
    let specs: Vec<String> = paths
        .iter()
        .map(|p| if cfg!(windows) { format!(":(icase){p}") } else { p.to_string() })
        .collect();
    let mut args = vec!["ls-files", "-z", "--"];
    args.extend(specs.iter().map(String::as_str));
    Ok(git::run(dir, &args)?.split('\0').filter(|s| !s.is_empty()).map(fold).collect())
}

/// A repo-relative name as the tracked-file check compares it: on Windows its ASCII letters case-folded, as git's
/// `:(icase)` matches them; other letters as they are, so names only Unicode case rules merge (`straße`, `strasse`;
/// `fıx`, `fix`) stay apart, as NTFS keeps them (sol-loop sync-perf r2, S3). Elsewhere, as is.
fn fold(name: &str) -> String {
    if cfg!(windows) {
        name.to_ascii_uppercase()
    } else {
        name.to_string()
    }
}

/// Every checkout and (if asked) worktree of a repo, as roots verified by `Root::checkout`, the worktrees skipped
/// for lying outside `worktree_roots`, and one note per worktree git lists that is no checkout (stale).
/// Configured and discovered roots get the same check (design-simplification-r6, rule A; Sol review r6, R6-02). A
/// configured checkout that is none refuses the whole place (R5-01): it is the config that is wrong. A discovered
/// one is git's stale registration: skipped, and said. A git call that fails refuses the place.
/// A repo's verified roots, the worktrees skipped for lying outside `worktree_roots`, and the stale ones noted.
type Checkouts = (Vec<Root>, Vec<PathBuf>, Vec<String>);

pub(crate) fn checkouts(repo: &stack::Repo) -> Result<Checkouts, String> {
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
/// Whether `dir` lies under one of the repo's `worktree_roots` (every dir does where it names none), as `checkouts`
/// decides it for a worktree git lists.
fn under_roots(repo: &stack::Repo, dir: &Path) -> bool {
    let d = norm(dir);
    repo.worktree_roots.is_empty()
        || repo
            .worktree_roots
            .iter()
            .map(|r| norm(&expand_home(r)))
            .any(|r| d == r || d.starts_with(&format!("{}/", r.trim_end_matches('/'))))
}

/// How far a `--repo` selector reaches (gap 4b, design note 2026-11-08-lords-gap4b-selectors S5): `uninstall` takes
/// a key only, in any case, since a path there could name one worktree yet remove the whole repo's copies.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reach {
    Key,
    Place,
}

/// What a `--repo` selector names: a declared key, and, when it named one checkout or worktree of a repo by a path,
/// that dir, which scopes the command as `--place` does (review 4b r1, F1: a path never widens to the whole repo). A
/// key names the whole repo; a workspace is one dir, so it has no `place`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Selected {
    pub key: String,
    pub place: Option<PathBuf>,
}

/// What a `--repo` selector names (gap 4b, S1–S3; `Selected`). The first rule that matches decides: the exact key;
/// the key in any case; with `Reach::Place`, a path (relative to `cwd`) that is a declared place's dir or lies in one,
/// the deepest. A dir's name is no selector (fix check r1: one name can be a checkout's and a worktree's). A path
/// matching two places is refused naming each, and no match is refused naming every key: never a guess.
pub fn select(
    stacks: &Stacks,
    selector: &str,
    cwd: &Path,
    reach: Reach,
) -> Result<Selected, String> {
    let manifest = &stacks
        .private
        .as_ref()
        .ok_or(
            "repos and workspaces are declared in the private stack.toml; no private stack loaded",
        )?
        .manifest;
    let keys: Vec<&String> = manifest.repos.keys().chain(manifest.workspaces.keys()).collect();
    let whole = |key: &str| Selected { key: key.to_string(), place: None };
    if keys.iter().any(|k| *k == selector) {
        return Ok(whole(selector));
    }
    // Keys are lowercase (the stack schema refuses others at load), so a key in another case names one at most.
    let lower = selector.to_lowercase();
    if keys.iter().any(|k| **k == lower) {
        return Ok(whole(&lower));
    }
    if reach == Reach::Place {
        // Each declared place's dirs: a repo's checkouts and the worktrees git lists (a repo whose worktrees can't be
        // listed keeps its checkouts, A8), each its own place; a workspace's path.
        let mut dirs: Vec<Found> = Vec::new();
        for (key, repo) in &manifest.repos {
            let declared: Vec<PathBuf> = repo.checkouts.iter().map(|c| expand_home(c)).collect();
            let worktrees = checkouts(repo).map(|(roots, _, _)| roots).unwrap_or_default();
            let place = |dir: PathBuf| Found { key: key.clone(), dir, repo: true };
            dirs.extend(declared.into_iter().map(place));
            dirs.extend(worktrees.iter().map(|r| place(r.path().to_path_buf())));
        }
        for (key, w) in &manifest.workspaces {
            dirs.push(Found { key: key.clone(), dir: expand_home(&w.path), repo: false });
        }
        // Compared resolved (a junction or link, A4), in the ledger's form (any case on Windows); a declared dir that
        // isn't there never matches.
        let resolved = |p: &Path| fsguard::canonical(p).ok().map(|c| ledger::key(&c));
        let typed = Path::new(selector);
        let typed = if typed.is_absolute() { typed.to_path_buf() } else { cwd.join(typed) };
        if let Some(at) = resolved(&typed) {
            let within: Vec<(&Found, usize)> = dirs
                .iter()
                .filter_map(|f| {
                    let d = resolved(&f.dir)?;
                    (at == d || at.starts_with(&format!("{}/", d.trim_end_matches('/'))))
                        .then_some((f, d.len()))
                })
                .collect();
            if let Some(deepest) = within.iter().map(|w| w.1).max() {
                let found = within.into_iter().filter(|w| w.1 == deepest).map(|w| w.0).collect();
                if let Some(answer) = pick(selector, found, "a path") {
                    return answer;
                }
            }
        }
    }
    let names: Vec<&str> = keys.iter().map(|k| k.as_str()).collect();
    let hint = match reach {
        Reach::Place => " or a path inside one",
        Reach::Key => {
            "; uninstall takes a key (one checkout or worktree: `--repo KEY --place DIR`)"
        }
    };
    Err(format!(
        "no repo or workspace `{selector}` in the private stack.toml; its names: {}{hint} (no --repo: every place)",
        names.join(", ")
    ))
}

/// One dir of a declared place, as `select` matches it: a repo's checkout or worktree, or a workspace.
struct Found {
    key: String,
    dir: PathBuf,
    repo: bool,
}

/// One place from a rule's matches, or a refusal naming each key matched; `None` when the rule matched nothing. Two
/// matches of one key are one place only when they are one dir (a checkout git also lists as a worktree).
fn pick(selector: &str, found: Vec<&Found>, how: &str) -> Option<Result<Selected, String>> {
    let mut places: Vec<(String, String, Option<PathBuf>)> = found
        .iter()
        .map(|f| {
            let place = f.repo.then(|| f.dir.clone());
            let dir = fsguard::canonical(&f.dir).unwrap_or_else(|_| f.dir.clone());
            (f.key.clone(), ledger::key(&dir), place)
        })
        .collect();
    places.sort();
    places.dedup_by(|a, b| a.0 == b.0 && a.1 == b.1);
    match &places[..] {
        [] => None,
        [(key, _, place)] => Some(Ok(Selected { key: key.clone(), place: place.clone() })),
        many => Some(Err(format!(
            "--repo `{selector}` names more than one place by {how}: {}; give the key, and --place DIR for one checkout",
            many.iter().map(|(k, d, _)| format!("`{k}` ({d})")).collect::<Vec<_>>().join(", ")
        ))),
    }
}

/// The declared repos and workspaces a plan's scope leaves out (gap 4b, S6): every one with `--user`, every other
/// one with `--repo`, none without a scope.
fn left_out(manifest: &stack::Manifest, opts: &Options) -> Vec<String> {
    if opts.uninstall {
        return Vec::new();
    }
    let all = manifest
        .repos
        .keys()
        .map(|k| (k, "repo"))
        .chain(manifest.workspaces.keys().map(|k| (k, "workspace")));
    match (&opts.repo, opts.user_only) {
        (None, false) => Vec::new(),
        (repo, _) => all
            .filter(|(k, _)| repo.as_deref() != Some(k.as_str()))
            .map(|(k, kind)| format!("{kind} {k}"))
            .collect(),
    }
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

/// A native child's start files in a place's role dir (`child_composed`): for one that also loads the place's file,
/// and for one that loads none.
pub const CHILD_FILE: &str = "child.md";
pub const CHILD_WHOLE_FILE: &str = "child-whole.md";

/// The start a session gets whose launch names `role` (`ChildStart::Launched`; the `role` guard gives it).
pub fn start_file(role: &str) -> String {
    format!("start-{role}.md")
}

/// The file in a place's role dir that holds an adapter's Memory section, where its place files name it instead
/// (`place_files.<adapter>.memory = "pointer"`): one per adapter, as each composes its own section.
pub fn memory_file(adapter: &str) -> String {
    format!("memory-{adapter}.md")
}

/// Everything someone in `role` follows in a place, standing alone: every global entry for the role, the project,
/// the place, and the memory index; for every agent family (lines for only some are labelled).
pub fn role_composed(stacks: &Stacks, layers: Layers, role: &str, home: &Path) -> Composed {
    compose::compose(stacks, &role_context(layers, role, home))
}

fn role_context(layers: Layers, role: &str, home: &Path) -> Context {
    Context {
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
        opener: Some(opener(home)),
        child: None,
        default_role_lines: true,
        memory_file: None,
    }
}

/// The `lords` a composed file names to open an entry by id: the installed guard copy (`lords guard install`), a
/// whole `lords`, at one path per tool home.
pub fn opener(home: &Path) -> PathBuf {
    home.join("bin").join("lords-guard.exe")
}

/// The role a native subagent starts as (stack.toml `child_role`; the private stack's first).
pub fn child_role(stacks: &Stacks) -> Option<&str> {
    stacks.private.iter().chain([&stacks.public]).find_map(|s| s.manifest.child_role.as_deref())
}

/// A native child's start file in a place (the `child` guard gives it at `SubagentStart`): `.lords/child.md`, what of
/// `role` the place's file doesn't carry, or `.lords/child-whole.md`, the role's whole start for a child that loads no
/// place file (compose.rs, `ChildStart`).
pub fn child_composed(
    stacks: &Stacks,
    layers: Layers,
    role: &str,
    home: &Path,
    start: compose::ChildStart,
) -> Composed {
    child_composed_on(stacks, layers, role, home, start, &crate::ops::today())
}

fn child_composed_on(
    stacks: &Stacks,
    layers: Layers,
    role: &str,
    home: &Path,
    start: compose::ChildStart,
    today: &str,
) -> Composed {
    let mut ctx = role_context(layers.clone(), role, home);
    ctx.child = Some(start);
    if start != compose::ChildStart::Whole {
        // What the place's files show beside the start, in every harness the place has (design note
        // 2026-11-08-lords-delivery-once, D2a): its default role's lines, where each file shows them, and its default
        // role's whole entries, where each carries them inline. The start leaves those out.
        let names = place_adapter_names(stacks, &layers);
        let adapters = place_adapters(&names);
        let all = |f: &dyn Fn(&dyn Adapter) -> bool| {
            !adapters.is_empty() && adapters.iter().all(|a| f(*a))
        };
        ctx.default_role = place_default_role(stacks, &layers);
        ctx.default_role_lines =
            all(&|a| stacks.place_files(a.id()).default_role_lines.unwrap_or(true));
        ctx.default_role_inline = all(&|a| a.default_role_inline());
    }
    compose::compose_on(stacks, &ctx, today)
}

/// The harnesses a place's files are written for (stack.toml `adapters`), by name.
fn place_adapter_names(stacks: &Stacks, layers: &Layers) -> Vec<String> {
    let Some(private) = stacks.private.as_ref() else { return Vec::new() };
    let manifest = &private.manifest;
    match layers {
        Layers::Repo { repo, .. } => manifest.repos.get(repo).map(|r| r.adapters.clone()),
        Layers::Workspace { name, .. } => manifest.workspaces.get(name).map(|w| w.adapters.clone()),
        Layers::Global => None,
    }
    .unwrap_or_default()
}

/// A start one hook gives a session in a place, as install writes it in the place's role dir and the stores' commit
/// check counts it (gap 11, design note §0b.3 step 6: one calculation for both).
pub struct HookStart {
    /// Its file in the role dir: `child.md`, or `start-<role>.md`.
    pub file: String,
    pub role: String,
    /// `Beyond` for `child.md`, `Launched` for a role's start.
    pub kind: compose::ChildStart,
    pub text: String,
}

impl HookStart {
    /// Its size as the hook gives it: characters, without Lords' marker lines (`guard::child_context`).
    pub fn chars(&self) -> usize {
        crate::guard::child_context(&self.text).chars().count()
    }
}

/// The starts a place in `layers` gives through a hook, on `today`: `child.md` for the child role (plan L6), then a
/// `start-<role>.md` for every declared role but the product's (F10). `child-whole.md` isn't one of them: past what a
/// hook gives, it is given nowhere (the Lords owner's option 1, 1 Nov).
pub fn hook_starts(stacks: &Stacks, layers: Layers, home: &Path, today: &str) -> Vec<HookStart> {
    let child = child_role(stacks)
        .map(|r| (CHILD_FILE.to_string(), r.to_string(), compose::ChildStart::Beyond));
    let roles = declared_roles(stacks).into_iter().filter(|r| r != compose::PRODUCT_ROLE);
    let launched = roles.map(|r| (start_file(&r), r, compose::ChildStart::Launched));
    child
        .into_iter()
        .chain(launched)
        .map(|(file, role, kind)| {
            let text = child_composed_on(stacks, layers.clone(), &role, home, kind, today).text;
            HookStart { file, role, kind, text }
        })
        .collect()
}

/// Every place the private stack declares, as `(label, layers)`: each repo, then each workspace, as `plan` takes them,
/// whether or not it has a checkout here (a place's starts don't depend on one: `role_context`).
pub fn declared_places(stacks: &Stacks) -> Vec<(String, Layers)> {
    let Some(private) = stacks.private.as_ref() else { return Vec::new() };
    let repos = private.manifest.repos.keys().map(|name| {
        let layers = Layers::Repo {
            repo: name.clone(),
            project: stacks.repo_project(name),
            sub: None,
            global: true,
        };
        (format!("repo {name}"), layers)
    });
    let workspaces = private.manifest.workspaces.iter().map(|(name, w)| {
        let layers =
            Layers::Workspace { name: name.clone(), project: w.project.clone(), global: true };
        (format!("workspace {name}"), layers)
    });
    repos.chain(workspaces).collect()
}

/// The most a `SubagentStart` hook's context may hold in characters before Claude Code saves it to a file and gives
/// the child only a preview, which it isn't asked to read (code.claude.com/docs/en/hooks: 10,000, no setting).
pub const CHILD_CONTEXT_MAX_CHARS: usize = 10_000;

/// The most a child's start holds: what one hook gives in one read, under the cap with room to spare (one read per
/// start for every agent type: fix check r2, FC1, the Lords owner's option 1 of 1 Nov). A `child.md` past it is
/// refused; a `child-whole.md` past it is written and given nowhere, with a note (`guard::child_hook`), until the
/// corpus fits.
pub const CHILD_START_CHARS: usize = 9_500;

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
    let every = chosen.clone();
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
    // Each with a checkout it serves, from which git lists every worktree that shares it, and every checkout that
    // reads it: its places (D13), since an edit there shows or hides paths in each of them.
    let mut excludes: Vec<(OwnerFile, PathBuf, Vec<String>, Vec<PlaceId>)> = Vec::new();
    // A git that fails here is a conflict, never a repo without an exclude file (Sol review r5, decision 1). The
    // exclude file is the one git names for this verified checkout (rule A).
    let mut add_exclude = |root: &Root, what: &str| -> Result<(), String> {
        if place.workspace {
            return Ok(());
        }
        let ex = OwnerFile::exclude(root)?;
        let at = PlaceId::Dir(root.path().to_path_buf());
        match excludes.iter_mut().find(|(e, ..)| ledger::key(e.path()) == ledger::key(ex.path())) {
            Some((_, _, files, places)) => {
                if !files.iter().any(|f| f == what) {
                    files.push(what.to_string());
                }
                if !places.contains(&at) {
                    places.push(at);
                }
            }
            None => {
                excludes.push((ex, root.path().to_path_buf(), vec![what.to_string()], vec![at]))
            }
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
            Some(Ok(files)) => {
                let rel = fold(rel);
                Ok(files.iter().any(|f| {
                    *f == rel || f.strip_prefix(&rel).is_some_and(|rest| rest.starts_with('/'))
                }))
            }
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
            let (text, composed) =
                place_text(pl.stacks, place.layers(false), adapter, p, &pl.plan.home);
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
            let c = role_composed(pl.stacks, place.layers(true), &role, &pl.plan.home);
            (role, c)
        })
        .collect();
    let mut tree = Tree::new();
    for (role, c) in &roles {
        if !c.text.is_empty() {
            tree.insert(role_file(role), c.text.clone().into_bytes());
        }
    }
    // A native child's start files, for the `child` guard (plan L6, an orchestrator's condition 3; review r1, F1). A
    // `child.md` one hook can't give in one read is refused like a file past its harness's limit (repair sol-loop r1,
    // R2; fix check r2, FC1). A `child-whole.md` past it is written and given nowhere, with the guard's note, until the
    // corpus fits (the Lords owner's option 1, 1 Nov): today every place's is.
    // A start per role for a session whose launch names it (F10; the `role` guard gives it). One past what one hook
    // gives is written all the same: the guard then points the session at its role file, and the plan says so.
    let (mut undeliverable, mut long_starts) = (Vec::new(), Vec::new());
    for s in hook_starts(pl.stacks, place.layers(true), &pl.plan.home, &pl.today) {
        let (file, n) = (&s.file, s.chars());
        if n > CHILD_START_CHARS && s.kind == compose::ChildStart::Beyond {
            undeliverable.push(format!(
                "{file} is {n} characters, past the {CHILD_START_CHARS} one hook gives a child in one read, one part: not written, the old files stay"
            ));
        } else if n > CHILD_START_CHARS {
            long_starts.push(format!(
                "{file} is {n} characters, past the {CHILD_START_CHARS} one hook gives: a session named {} at launch is told to read role-{}.md instead",
                s.role, s.role
            ));
        }
        tree.insert(s.file, s.text.into_bytes());
    }
    // A `child-whole.md` past what one hook gives is written and given nowhere, with the guard's note, until the
    // corpus fits (the Lords owner's option 1, 1 Nov): today every place's is.
    if let Some(role) = child_role(pl.stacks) {
        let whole = compose::ChildStart::Whole;
        let text = child_composed(pl.stacks, place.layers(true), role, &pl.plan.home, whole).text;
        tree.insert(CHILD_WHOLE_FILE.into(), text.into_bytes());
    }
    // The Memory section of each adapter whose place files name it instead, whatever adapter this sync takes: those
    // files name it in every checkout (design note 2026-11-02-lords-memory-pointer, I2, A5). It is the same in each.
    let pointer =
        |a: &dyn Adapter| pl.stacks.place_files(a.id()).memory == stack::MemoryForm::Pointer;
    if let Some(root) = place.paths.iter().find(|r| r.path().is_dir()) {
        for a in every.into_iter().filter(|a| pointer(*a)) {
            let home = &pl.plan.home;
            let mut ctx = place_context(pl.stacks, a, place.layers(false), Some(root.path()), home);
            // Its paths are whole, as it sits below the checkout, whatever the adapter shows (A11).
            ctx.relative_to = None;
            if let Some(text) = compose::compose(pl.stacks, &ctx).memory {
                tree.insert(memory_file(a.id()), text.into_bytes());
            }
        }
    }
    tree.insert(".gitignore".into(), b"*\n".to_vec());
    pl.undeliverable |= !undeliverable.is_empty();
    let tree = std::sync::Arc::new(tree);
    let scope = place_scope(place, None);
    for root in place.paths.iter().filter(|r| r.path().is_dir()) {
        let mut item = pl.tool_dir(
            &format!("{} (role files)", place.label),
            &scope,
            &Slot::RoleDir.place(root, "").expect("a slot without a name"),
            tree.clone(),
        );
        if !undeliverable.is_empty() {
            // A conflict it has already keeps its kind (what may be removed depends on it).
            if item.action != Action::Conflict {
                item.action = Action::Conflict;
                item.conflict = ConflictKind::OverLimit;
            }
            item.notes.splice(0..0, undeliverable.iter().cloned());
        }
        item.notes.extend(long_starts.iter().cloned());
        pl.plan.items.push(item);
    }
    for (ex, checkout, files, places) in excludes {
        pl.plan.owner_files.push(ex.clone());
        let mut item =
            exclude_item(&format!("{} (git exclude)", place.label), &ex, &checkout, &files);
        item.places = places;
        pl.plan.items.push(item);
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
            opener: None,
            child: None,
            default_role_lines: true,
            memory_file: None,
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

/// A repo's git exclude file holding `files` among its lines: the owner file's one edit, or a conflict.
fn exclude_item(label: &str, ex: &OwnerFile, checkout: &Path, files: &[String]) -> Item {
    let current = match fsguard::read_text(ex.path()) {
        Ok(c) => c.unwrap_or_default(),
        Err(e) => return conflict(label, ex.path(), e),
    };
    let proposed = excludes_text(&current, files);
    // The file is shared by every worktree of the repo: an edit that would show a path in any of their `git
    // status` is planned as a conflict, whichever place asked for it (orchestrator, 26 Oct).
    if proposed != current && !only_adds(&current, &proposed) {
        match newly_shown(checkout, &current, &proposed) {
            Ok(shown) if shown.is_empty() => {}
            Ok(shown) => {
                return conflict(
                    label,
                    ex.path(),
                    format!(
                        "the edit would show {} untracked path(s) in git status where this file is shared, e.g. {}; set them aside first (`lords set-aside`)",
                        shown.len(),
                        shown.iter().take(3).cloned().collect::<Vec<_>>().join(", ")
                    ),
                )
            }
            Err(e) => return conflict(label, ex.path(), e),
        }
    }
    owner_file_item(label, ex.path(), Dest::Owner(ex.clone()), proposed, &Composed::default())
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
            // Skipped on purpose: a worktree git lists outside the repo's worktree roots, or a dir outside them.
            if skipped.iter().any(|s| same_place(s, one)) || !under_roots(repo, one) {
                pl.plan.notes.push(format!(
                    "repo {name}: {} is none of its checkouts or worktrees under worktree_roots",
                    display(one)
                ));
                return;
            }
            // Under its roots, but no checkout or worktree git lists yet: a new worktree still being made (F8, D22).
            let mut item = conflict(
                &format!("repo {name}"),
                one,
                "not a checkout or worktree of this repo yet (a new worktree still being made?)",
            );
            item.conflict = ConflictKind::NotReady;
            item.places = vec![PlaceId::Dir(one.to_path_buf())];
            pl.plan.items.push(item);
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
    plan_on(stacks, opts, &crate::ops::today())
}

/// `plan` on the date `today` (`YYYY-MM-DD`), read once for the whole plan: every place's starts a hook gives are
/// composed on it, and the ended entries it names are those ended by then (gap 11, §0d.5; sol-loop r1, S5).
#[doc(hidden)]
pub fn plan_on(stacks: &Stacks, opts: &Options, today: &str) -> Result<Plan, String> {
    let private: &Stack = stacks
        .private
        .as_ref()
        .ok_or("targets are declared in the private stack.toml; no private stack loaded")?;
    if opts.home.as_os_str().is_empty() {
        return Err("no tool home given (Options.home): sync reads its ledger there".into());
    }
    // A place is held whole only where the plan sees all of it (D13, design note D3; sol-loop r1 S1).
    if opts.stage == Stage::SafeOnly && opts.adapter.is_some() {
        return Err(
            "a safe-only plan holds a place whole, and an adapter filter plans part of it: drop --adapter".into()
        );
    }
    // One place's uninstall has its own planner: nothing is wanted there, and the ledger alone decides (PI-13).
    // Their items are typed too, as the last step (D13); an uninstall creates nothing, so no place is new to it.
    let lock_wait = LockWait(stacks.delivery().lock_wait());
    if opts.uninstall && opts.place.is_some() {
        let mut p = place_uninstall::plan(stacks, private, opts)?;
        safety::classify(&mut p, &BTreeSet::new(), &[]);
        p.lock_wait = lock_wait;
        return Ok(p);
    }
    // One dropped target's uninstall too: the stack no longer says where it was (D21).
    if opts.uninstall && opts.target.is_some() {
        let mut p = target_uninstall::plan(stacks, private, opts)?;
        safety::classify(&mut p, &BTreeSet::new(), &[]);
        p.lock_wait = lock_wait;
        return Ok(p);
    }
    let ledger = Ledger::load(&opts.home)?;
    let mut pl = Planner {
        undeliverable: false,
        stacks,
        plan: Plan {
            home: opts.home.clone(),
            ledger_revision: ledger.revision(),
            codex: opts.codex.clone(),
            lock_wait,
            out_of_scope: left_out(&private.manifest, opts),
            ..Plan::default()
        },
        ledger,
        wanted: BTreeSet::new(),
        covered: Vec::new(),
        trees: BTreeMap::new(),
        places: BTreeMap::new(),
        checkouts: BTreeMap::new(),
        today: today.to_string(),
    };
    pl.check_ledger()?;
    // The memory index points at the stack's copies: say what an import would bring first.
    if !private.manifest.memory_stores.is_empty() {
        match crate::ops::import_preview(private, None) {
            Ok(reports) => {
                for r in reports.iter().filter(|r| !r.added.is_empty() || !r.changed.is_empty()) {
                    let (added, changed) = (r.added.len(), r.changed.len());
                    pl.plan.imports.push(PendingImport { store: r.store.clone(), added, changed });
                    // A safe-only plan imports nothing: it lists the import as store work instead (D13).
                    if opts.stage != Stage::SafeOnly {
                        pl.plan.notes.push(format!(
                            "memory store {}: {added} new, {changed} changed since the last import; --apply imports them (one commit) before writing",
                            r.store
                        ));
                    }
                }
            }
            Err(e) => pl.plan.notes.push(format!("memory stores: {e}")),
        }
    }
    // Ended temporary entries: never composed; listed here, retired by import_and_apply.
    for (e, why) in crate::temporary::expired(stacks, today) {
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
                i.places = vec![PlaceId::Target(t.name.clone())];
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
                    k.places.extend(item.places);
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
            let roots = match pl.checkouts.get(name) {
                Some(Ok((roots, _, _))) => roots.clone(),
                _ => Vec::new(),
            };
            for i in &mut pl.plan.items[start..] {
                i.units = vec![Unit::Repo(name.clone())];
                if i.places.is_empty() {
                    i.places = vec![safety::place_in(&roots, &i.path)];
                }
            }
        }
        for (name, w) in &private.manifest.workspaces {
            if opts.repo.as_deref().is_some_and(|r| r != name) {
                continue;
            }
            if opts.place.is_none() {
                let start = pl.plan.items.len();
                workspace_items(&mut pl, name, w, opts.adapter.as_deref());
                let roots: Vec<Root> = Root::workspace(w).into_iter().collect();
                for i in &mut pl.plan.items[start..] {
                    i.units = vec![Unit::Workspace(name.clone())];
                    i.places = vec![safety::place_in(&roots, &i.path)];
                }
            }
        }
        if let Some(r) = &opts.repo {
            if !private.manifest.repos.contains_key(r)
                && !private.manifest.workspaces.contains_key(r)
            {
                // The declared names, so the caller can run the command again without opening the stack (the master
                // thread, 8 Nov: `--repo Harness` for the workspace `harness`).
                let names: Vec<&str> = (private.manifest.repos.keys())
                    .chain(private.manifest.workspaces.keys())
                    .map(String::as_str)
                    .collect();
                return Err(format!(
                    "no repo or workspace `{r}` in the private stack.toml; its names: {} (no --repo: every place)",
                    names.join(", ")
                ));
            }
        }
    }
    // A live memory store whose index isn't a readable file: a conflict, so no file loses its memory index to a dead
    // pointer or a stale copy, and nothing applies until it is there (Codex switch-on review r1, R2).
    for m in compose::live_stores(stacks) {
        if let Some(why) = compose::live_index_problem(m) {
            pl.plan.items.push(conflict(
                &format!("memory store {}", m.name),
                &compose::live_index(m),
                why,
            ));
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
    pl.plan.over_limit =
        pl.undeliverable || pl.plan.items.iter().any(|i| i.conflict == ConflictKind::OverLimit);
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
        // A safe-only plan lists what isn't wanted even without a complete view, only so its place is held whole, never
        // half updated (F8 sol-loop r1 S2): such a forget is classed `incomplete-view`, and a removal is never safe.
        (Stage::Full, Some(_)) | (Stage::SafeOnly, _) => pl.unwanted(),
        (Stage::Full, None) => pl.plan.notes.push(format!(
            "removal held: {why}, so nothing is removed or forgotten until both are none"
        )),
    }
    pl.plan.complete = complete;
    // Each declared repo's post-checkout hook, as this build writes it (F8, C3): `sync --apply` installs what isn't.
    if !opts.uninstall && !opts.user_only && opts.place.is_none() {
        pl.plan.hooks = repo_hooks(stacks, private, opts, &pl.ledger);
    }
    // A new worktree, from its post-checkout hook, is a place Lords manages for its own plan (F8, D-4).
    let mut managed = safety::managed(&pl.ledger);
    if let (true, Some(one), Some(repo)) = (opts.new_worktree, &opts.place, &opts.repo) {
        if let Some(Ok((roots, _, _))) = pl.checkouts.get(repo) {
            managed.extend(
                roots.iter().filter(|r| same_place(r.path(), one)).map(|r| ledger::key(r.path())),
            );
        }
    }
    // The planner's last step, after every change to an item's action (D13): nothing types it again.
    safety::classify(&mut pl.plan, &managed, &stacks.safe_only().applies);
    if opts.stage == Stage::SafeOnly {
        safety::safe_only(&mut pl.plan);
    }
    Ok(pl.plan)
}

/// The post-checkout hook of each declared repo in scope that has worktrees and a checkout (F8, C3), as
/// `hooks::post_checkout_state` finds it against what sync writes there: the guard copy (D-3) running `deliver` with
/// these stacks into this home. Read only. A checkout git can't read is left out: the plan's own conflict for it says
/// why.
fn repo_hooks(
    stacks: &Stacks,
    private: &Stack,
    opts: &Options,
    ledger: &Ledger,
) -> Vec<crate::hooks::RepoHook> {
    let Ok(exe) = guard_place(&opts.home) else { return Vec::new() };
    let mut hooks = Vec::new();
    for (name, repo) in &private.manifest.repos {
        if opts.repo.as_deref().is_some_and(|r| r != name) || !repo.worktrees {
            continue;
        }
        // One hook per git common dir its declared checkouts have: every independent clone's worktrees get it (sol-loop
        // r1 S6).
        let mut seen = BTreeSet::new();
        for checkout in repo.checkouts.iter().map(|c| expand_home(c)) {
            let Ok(common) = crate::hooks::common_dir(&checkout) else { continue };
            if !seen.insert(ledger::key(&common)) {
                continue;
            }
            let args = crate::hooks::sync_args(stacks, name);
            let Ok(state) =
                crate::hooks::post_checkout_state(&checkout, ledger, exe.path(), &args, &opts.home)
            else {
                continue;
            };
            hooks.push(crate::hooks::RepoHook { repo: name.clone(), checkout, state });
        }
    }
    hooks
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
            && !matches!(
                i.action,
                Action::RetireEntry | Action::Remove | Action::RemoveBlock | Action::Forget
            )
            && matches!(i.change(), Change::ToolOwned | Change::ExcludeEdit)
    });
}

// ---------------------------------------------------------------- apply

/// One writer of the ledger at a time on this machine (a user's sync, a post-checkout hook's, a hook
/// installation), and the readers that must not see one mid-swap (the child guard): `apply.lock` in the tool's home,
/// held by an OS lock that ends with its holder however it ends (`fsguard::Lock`; fix check r2, FC5), waited for up to
/// `secs` seconds, the deadline checked before each attempt. Released when dropped.
pub struct ApplyLock(#[allow(dead_code)] fsguard::Lock);

impl ApplyLock {
    pub(crate) fn take(home: &Path, secs: u64) -> Result<ApplyLock, String> {
        match ApplyLock::wait(home, std::time::Duration::from_secs(secs))? {
            Ok(lock) => Ok(lock),
            Err(busy) => Err(busy),
        }
    }

    /// The lock, waiting at most `wait` for another holder (F8, C5); `Ok(Err(why))` when that holder keeps it past
    /// then, naming it; `Err` when the lock can't be tried at all.
    pub fn wait(
        home: &Path,
        wait: std::time::Duration,
    ) -> Result<Result<ApplyLock, String>, String> {
        let root = Root::home(home)?;
        root.create()?;
        let lock = root.join(&[Part::new("apply.lock")?]);
        let until = std::time::Instant::now() + wait;
        let step = std::time::Duration::from_millis(200);
        loop {
            match fsguard::try_lock(&lock, &format!("pid {}", crate::process::id()))? {
                fsguard::TryLock::Held(l) => return Ok(Ok(ApplyLock(l))),
                fsguard::TryLock::Busy(_) if std::time::Instant::now() + step < until => {
                    std::thread::sleep(step);
                }
                fsguard::TryLock::Busy(holder) => {
                    return Ok(Err(format!(
                        "another apply holds {} ({holder}); this build's lock ends with its holder, so one in an earlier build's form (\"pid N\") is that build's: if none is running, delete that file",
                        lock.path().display(),
                    )))
                }
            }
        }
    }
}

/// Where a safe-only apply is (F8): for tests that must reach a point between two of its changes without timing, as
/// an owner's edit, a reader or a kill would.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Step {
    /// A delivery has the stores' committed trees it delivers, under the lock it applies with; it hasn't loaded them
    /// yet (gap 11, §0e: what is written to a store's checkout from here on can't reach this delivery).
    Captured,
    /// A delivery's plan is made, under the lock it applies with; nothing is written yet.
    Planned,
    /// Every item of one place is checked and its new version staged beside it; nothing at its paths has changed.
    Staged,
    /// One item of the place is in, and its ledger record saved.
    Swapped,
    /// A path a stopped transaction recorded was found to hold its recorded hash, and is about to be removed; the path
    /// given is that path (F8 sol-loop r1, S1).
    Compared,
    /// A staged version is about to be made at the path given, its fresh name beside a place's path (sol-loop r1, S2).
    Creating,
}

/// Why `apply_held` changed nothing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Refused {
    /// The ledger changed since the plan (a writer outside the lock): plan again (F8, D22).
    LedgerChanged,
    Other(String),
}

impl std::fmt::Display for Refused {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Refused::LedgerChanged => f.write_str(
                "the ledger changed since the plan (another sync?); plan again (nothing was changed)",
            ),
            Refused::Other(e) => f.write_str(e),
        }
    }
}

impl From<String> for Refused {
    fn from(e: String) -> Refused {
        Refused::Other(e)
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

    /// Keeps what a file sync never writes held before a program sync asks writes it (Codex's config.toml, before its
    /// writer trusts Lords' hooks; design note 2026-11-02-lords-codex-trust, §2.2). `restore` names it, never writes it.
    fn copy(&mut self, path: &Path) -> Result<(), String> {
        let Some(bytes) = fsguard::read(path)? else { return Ok(()) };
        let dir = self.dir()?;
        self.n += 1;
        let name = format!("{:04}", self.n);
        fsguard::create_file(&dir.child(Part::new(&name)?), &bytes)?;
        self.line(&["copy", &path.to_string_lossy(), &name])
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

/// What an apply did: one line per change, and the backup dir, if it changed an owner file; and one line per hook of
/// a Codex hooks file it wrote that doesn't run (`codex_trust::settle`).
#[derive(Clone, Debug, Default)]
pub struct Applied {
    pub done: Vec<String>,
    pub backups: Option<PathBuf>,
    pub codex: Vec<String>,
    /// A safe-only apply's places whose check or write failed: each put back whole and held, with why (F8, C4).
    pub held: Vec<HeldPlace>,
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
fn owner_update(
    item: &Item,
    run: &mut Run,
    installed: &mut dyn FnMut(&Path),
) -> Result<(), String> {
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
        fsguard::put_file(owner, new)?;
        installed(path);
        return Ok(());
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
    installed(path);
    aside.delete()
}

/// Replaces or removes a tool-owned path: what is there moves aside and must still match its ledger record
/// (otherwise it goes back and apply stops); the new version goes in; the old one is deleted, following nothing.
/// Whether a tool item's destination is still as the plan saw it, read only: nothing where it creates, and its ledger
/// record unchanged where it updates or removes. Apply runs it for every tool item before anything changes (rm-guard
/// fix check r3, R7: validate everything first, then act); `tool_change` runs it again and checks the content when it
/// acts, for what changes later.
fn tool_current(item: &Item, ledger: &Ledger) -> Result<(), String> {
    // A record planned to go because nothing was at its path goes only while nothing is (sol-loop r2, S4).
    if item.action == Action::Forget
        && item.gone
        && fsguard::kind(&item.path)? != fsguard::Kind::Missing
    {
        return Err(stale(item, "something is at its path again"));
    }
    let Dest::Tool(place) = &item.dest else { return Ok(()) };
    match item.action {
        Action::Create | Action::Install
            if fsguard::kind(place.path())? != fsguard::Kind::Missing =>
        {
            Err(stale(item, "something appeared"))
        }
        Action::Update | Action::Refresh | Action::Remove | Action::RemoveBlock => {
            let recorded = ledger.get(place.path()).map(|o| o.sha.clone());
            if recorded.is_none() || recorded != item.have {
                return Err(stale(item, "its ledger record changed"));
            }
            Ok(())
        }
        _ => Ok(()),
    }
}

fn tool_change(item: &Item, ledger: &mut Ledger) -> Result<(), String> {
    if item.action == Action::Forget {
        tool_current(item, ledger)?;
        ledger.forget(&item.path);
        return Ok(());
    }
    let Dest::Tool(place) = &item.dest else {
        return Err(format!("{}: no place below a verified root", item.path.display()));
    };
    let path = place.path();
    // Lords' block out of a file the owner added text to (one place's uninstall): the file is swapped only while it
    // holds exactly what was planned, its attributes kept, and put back on any error; then the record goes. No backup:
    // the bytes that go are the record's, and the owner's stay in the file.
    if item.action == Action::RemoveBlock {
        tool_current(item, ledger)?;
        let (Some(was), Some(rest)) = (&item.current, &item.proposed) else {
            return Err(format!("{}: a block removal planned without its texts", path.display()));
        };
        fsguard::replace_file_if(place, was.as_bytes(), rest.as_bytes())?;
        ledger.forget(path);
        return Ok(());
    }
    let have = |p: &Path| -> Result<String, String> {
        Ok(match fsguard::kind(p)? {
            fsguard::Kind::Dir => fsguard::tree_sha(&fsguard::read_tree(p)?),
            fsguard::Kind::File => fsguard::sha(&fsguard::read(p)?.unwrap_or_default()),
            fsguard::Kind::Missing => String::new(),
        })
    };
    tool_current(item, ledger)?;
    let record = ledger.get(path).cloned();
    let recorded = record.as_ref().map(|o| o.sha.clone());
    let old = match item.action {
        Action::Create | Action::Install => None,
        Action::Update | Action::Refresh | Action::Remove => {
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
            // From here any failure puts the copy back at its path before returning (uninstall-place review r1,
            // UP2-R1-01): a copy not shown to hold its record's bytes, an unreadable one included, never leaves it.
            match have(aside.path()) {
                Ok(h) if Some(&h) == recorded.as_ref() => {}
                Ok(_) => return Err(aside.back_after(stale(item, "it changed"))),
                Err(e) => return Err(aside.back_after(e)),
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
            return Err(match &old {
                Some(aside) => aside.back_after(e),
                None => e,
            })
        }
    };
    match put {
        Some(sha) => {
            let kind = if item.tree.is_some() { "dir" } else { "file" };
            ledger.record(place, kind, &sha, &item.scope);
            // The new version holds the path now; the old one, shown to be the record's, only goes.
            if let Some(aside) = old {
                aside.delete()?;
            }
        }
        None => {
            // A removal's record goes once its copy is gone; a delete that stops part way puts back what is left,
            // and the record stays.
            if let Some(aside) = old {
                aside.delete_or_back()?;
            }
            ledger.forget(path);
        }
    }
    Ok(())
}

/// Carries out a plan: every change goes through the filesystem guard, at the place the plan built for it;
/// tool-owned paths change only while they match their ledger record, owner files only while they hold what was
/// planned, and nothing is removed or forgotten without the plan's proof of a complete view (rule B). Nothing to
/// change touches nothing, not even a lock (Sol review r2, R2-21).
pub fn apply(plan: &Plan) -> Result<Applied, String> {
    apply_between(plan, |_| {})
}

/// `apply`, with `installed` run as each owner file's new version is in and before its old one goes: the point where
/// another process can hold that old copy open, for tests that must reach it without timing (review r1, R1-02).
#[doc(hidden)]
pub fn apply_between(plan: &Plan, mut installed: impl FnMut(&Path)) -> Result<Applied, String> {
    // The refusal rules a dry run reports too (`Plan::applies`).
    plan.applies().map_err(|e| format!("{e} (nothing was changed)"))?;
    if !plan.items.iter().any(|i| i.changes() && i.action != Action::RetireEntry) {
        return Ok(Applied::default());
    }
    // One lock over everything this apply changes, waited for as the stacks set (F8, C5); every check first, then the
    // changes (rm-guard review r1, R7). Apply never writes the guard copy (`lords guard install` does).
    let lock = ApplyLock::wait(&plan.home, plan.lock_wait.0)??;
    applied_under(plan, &lock, &mut installed, &mut |_, _| {}).map_err(|e| e.to_string())
}

/// `apply` under a lock the caller holds since before it planned (F8, C5): so no other writer can have changed what the
/// plan read. A safe-only plan is written place by place, each whole or not at all (`places`).
pub fn apply_held(plan: &Plan, lock: &ApplyLock) -> Result<Applied, Refused> {
    apply_held_between(plan, lock, &mut |_, _| {})
}

/// `apply_held`, calling `between` at each `Step` with the place it concerns.
#[doc(hidden)]
pub fn apply_held_between(
    plan: &Plan,
    lock: &ApplyLock,
    between: &mut dyn FnMut(Step, &Path),
) -> Result<Applied, Refused> {
    applied_under(plan, lock, &mut |_| {}, between)
}

/// `apply`'s checks and changes, under the apply lock `_lock`.
fn applied_under(
    plan: &Plan,
    _lock: &ApplyLock,
    installed: &mut dyn FnMut(&Path),
    between: &mut dyn FnMut(Step, &Path),
) -> Result<Applied, Refused> {
    plan.applies().map_err(|e| format!("{e} (nothing was changed)"))?;
    let changing: Vec<&Item> =
        plan.items.iter().filter(|i| i.changes() && i.action != Action::RetireEntry).collect();
    // A safe-only plan goes on with nothing to change too: its apply clears and reports what stopped runs left beside
    // its paths (F8 review r1, R1-03).
    if changing.is_empty() && plan.held.is_none() {
        return Ok(Applied::default());
    }
    // The place an item acts on is the one the plan built for its path: an item altered since doesn't apply.
    for i in &changing {
        let at = match &i.dest {
            Dest::Tool(c) => Some(c.path()),
            Dest::Owner(o) => Some(o.path()),
            Dest::None => None,
        };
        if at.is_some_and(|a| ledger::key(a) != ledger::key(&i.path)) {
            return Err(Refused::Other(format!(
                "{}: not the place the plan built for it; plan again (nothing was changed)",
                i.path.display()
            )));
        }
    }
    let mut ledger = Ledger::load(&plan.home)?;
    if ledger.revision() != plan.ledger_revision {
        return Err(Refused::LedgerChanged);
    }
    // A safe-only plan (D13) is written place by place, each place checked and put back whole on its own (F8, C4).
    if plan.held.is_some() {
        return places::apply(plan, &mut ledger, between).map_err(Refused::Other);
    }
    // The ledger covers only tool-owned paths; a plan is current only if its tool destinations and the owner files it
    // changes are as it saw them too (rm-guard fix checks r2, r3, R7). Checked here, under the lock, before anything
    // changes; and settings that will name the guard copy only while it is there and answers.
    for i in changing.iter().filter(|i| i.class == Class::Tool) {
        tool_current(i, &ledger)?;
    }
    for i in changing.iter().filter(|i| i.class == Class::Owner) {
        let now = fsguard::read_text(&i.path)?.map(|t| fsguard::sha(t.as_bytes()));
        if now != i.have {
            return Err(Refused::Other(format!(
                "{} changed since the plan (another sync?); plan again (nothing was changed)",
                i.path.display()
            )));
        }
    }
    for i in &changing {
        let Some(g) = &i.names_guard else { continue };
        guard_ready(g).map_err(|e| format!("{e} (nothing was changed)"))?;
        guard_runs(g, &i.names_guards).map_err(|e| format!("{e} (nothing was changed)"))?;
        if i.names_child_guard {
            guard_speaks_child(g, &plan.home).map_err(|e| format!("{e} (nothing was changed)"))?;
        }
        if let Some(id) = i.names_role_guard {
            guard_speaks_role(g, &plan.home, id)
                .map_err(|e| format!("{e} (nothing was changed)"))?;
        }
        if let Some(args) = i.names_rm_guard {
            guard_speaks_rm(g, args).map_err(|e| format!("{e} (nothing was changed)"))?;
        }
    }
    let mut run = Run::new(&plan.home)?;
    let mut applied = Applied::default();
    let mut wrote = Vec::new();
    let result =
        apply_items(&changing, plan, &mut run, &mut ledger, &mut applied, &mut wrote, installed);
    // Codex's trust for each of its hooks files written, whatever came after it (design note
    // 2026-11-02-lords-codex-trust, I1).
    for item in wrote {
        let (text, lords) = (item.proposed.as_deref().unwrap_or(""), item.codex_lords.as_deref());
        let mut backup = |p: &Path| run.copy(p);
        let lines = crate::codex_trust::settle(
            &plan.codex,
            &item.path,
            text,
            lords.unwrap_or_default(),
            &mut backup,
        );
        applied.codex.extend(lines);
    }
    applied.backups = run.dir.as_ref().map(|d| d.path().to_path_buf());
    let applied = match result {
        Err(e) if !applied.codex.is_empty() => Err(format!(
            "{e}; and {} Codex hook(s) not trusted: {}",
            applied.codex.len(),
            applied.codex.join("; ")
        )),
        r => r.map(|()| applied),
    };
    applied.map_err(Refused::Other)
}

/// `apply`'s changes, item by item; `wrote` gets each Codex hooks file written.
fn apply_items<'a>(
    changing: &[&'a Item],
    plan: &Plan,
    run: &mut Run,
    ledger: &mut Ledger,
    applied: &mut Applied,
    wrote: &mut Vec<&'a Item>,
    installed: &mut dyn FnMut(&Path),
) -> Result<(), String> {
    for item in changing.iter().copied() {
        let mut put = false;
        let result = match item.class {
            Class::Owner => owner_update(item, run, &mut |p: &Path| {
                put = true;
                installed(p)
            }),
            Class::Tool => tool_change(item, ledger).and_then(|()| ledger.save(&plan.home)),
        };
        // A Codex hooks file in place is settled, whatever failed after it went in (review r1, R1-02).
        if put && item.codex_lords.is_some() {
            wrote.push(item);
        }
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
    Ok(())
}

/// One manifest line, re-derived under the current config.
enum Undo {
    Edit { owner: OwnerFile, name: String, written: String, attrs: fsguard::Attrs },
    Created { owner: OwnerFile, written: String },
    Aside { to: Contained, from: Contained },
}

/// One manifest line, parsed whole (r7 fix check, FC-02): each kind's every field, as `Run` writes it. A path is
/// absolute, a backup name a `Part`, a hash 64 hex digits, attributes `attrs:<hex>`; anything else is no record. A
/// `copy` is another program's file, kept before it wrote (`Run::copy`): named, never written back.
enum Record<'a> {
    Edit { path: &'a Path, name: Part, written: &'a str, attrs: fsguard::Attrs },
    Created { path: &'a Path, written: &'a str },
    Aside { path: &'a Path, name: Part },
    Copy { path: &'a Path, name: Part },
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
            ["copy", p, name] => Record::Copy { path: path(p)?, name: Part::new(name).ok()? },
            _ => return None,
        })
    }
}

/// Undoes one apply's owner-file edits from its backup dir, last first: a file gets its old bytes back, and a file
/// apply created goes, each only while it still holds exactly what sync wrote (Sol review r2, R2-03). It deletes
/// nothing: what it takes from a path (a created file, an edited file's sync version) moves into the backup dir's
/// `restored/`, where a writer that held it open still writes (R6-01), and its step line names where. A path moved
/// there by `set-aside` comes back where nothing is. Every line must re-derive under `plan`, the current config
/// (design review, point 3): an edit's file one the config names now, a set-aside path within one of the plan's
/// verified roots, and an edited file one whose attributes as they are now sync may replace (rule C, as apply). If
/// any doesn't, or a line doesn't parse, nothing changes, and each such line is listed with its path and where its
/// bytes are in the backup; the tool never guesses or skips a line (Sol review r7, R7-06/R7-07). Tool-owned paths aren't restored: sync regenerates them, or
/// `lords uninstall` removes them. Returns one line per step.
pub fn restore(plan: &Plan, backup_dir: &Path) -> Result<Vec<String>, String> {
    restore_between(plan, backup_dir, &mut |_| {})
}

/// `restore`, calling `between` with each path it is about to put back or remove, once that path has been checked:
/// for tests that must reach the moment between the check and the change, as a writer would.
#[doc(hidden)]
pub fn restore_between(
    plan: &Plan,
    backup_dir: &Path,
    between: &mut dyn FnMut(&Path),
) -> Result<Vec<String>, String> {
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
    let mut left = Vec::new();
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
            Record::Copy { path, name } => left.push(format!(
                "left {} as it is (not sync's to write); what it held before that apply is in {}",
                path.display(),
                kept(name.as_str())
            )),
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
    // Restore deletes nothing (the master's BLOCK R6-01, 9 Nov: "restore never destroys bytes written after sync"):
    // a file it takes from its path goes into the backup dir's `restored/`, the file object itself, so what a writer
    // holding it open writes later is kept there too (design note 2026-11-09-lords-restore-keeps-bytes-design.md).
    let restored = bdir.child(Part::new("restored")?);
    for (n, (path, step)) in undo.into_iter().enumerate() {
        let holds = |sha: &str| -> Result<bool, String> {
            Ok(fsguard::read(&path)?.is_some_and(|b| fsguard::sha(&b) == sha))
        };
        let keep_at = || -> Result<Contained, String> {
            let name =
                path.file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
            Ok(restored.child(Part::new(&format!("{n}-{name}"))?))
        };
        let step = match step {
            Undo::Edit { owner, name, written, attrs } if holds(&written)? => {
                // A numbered backup that isn't there stops the restore, never puts an empty file back (r5).
                let old = fsguard::read(&backup_dir.join(&name))?.ok_or_else(|| {
                    format!("{}: the backup {name} is missing", backup_dir.display())
                })?;
                // Checked again as it is replaced, like apply (rule C; R7-06).
                fsguard::attrs(&path)?.keep().map_err(|e| format!("{}: {e}", path.display()))?;
                between(&path);
                // A move that fails moves nothing: the file stays at its path, reported.
                let aside = match fsguard::move_aside_into(&owner, &keep_at()?) {
                    Ok(a) => a,
                    Err(e) => {
                        done.push(format!(
                            "kept {} (it couldn't go into the backup: {e})",
                            path.display()
                        ));
                        continue;
                    }
                };
                // What is aside goes back on any error before it is replaced (sol-loop r1, B1).
                let read = fsguard::read(aside.path()).map_err(|e| aside.back_after(e))?;
                if read.is_some_and(|b| fsguard::sha(&b) == written) {
                    // The owner file's attributes as apply found them (Sol review r4, R4-03).
                    if let Err(e) = fsguard::put_file_as(&owner, &old, Some(&attrs)) {
                        return Err(aside.back_after(e));
                    }
                    format!(
                        "restored {}; what sync had written is kept at {}",
                        path.display(),
                        aside.path().display()
                    )
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
                between(&path);
                // Checked again once it is aside, like an edit's undo: a write since the check comes back (the Lords
                // whole-project review, 9 Nov). A move that fails moves nothing: the file stays, reported.
                let aside = match fsguard::move_aside_into(&owner, &keep_at()?) {
                    Ok(a) => a,
                    Err(e) => {
                        done.push(format!(
                            "kept {} (it couldn't go into the backup: {e})",
                            path.display()
                        ));
                        continue;
                    }
                };
                // What is aside goes back on any error (sol-loop r1, B1).
                let read = fsguard::read(aside.path()).map_err(|e| aside.back_after(e))?;
                if read.is_some_and(|b| fsguard::sha(&b) == written) {
                    format!(
                        "removed {}; what it held is kept at {}",
                        path.display(),
                        aside.path().display()
                    )
                } else {
                    aside.back()?;
                    format!("kept {} (changed while restoring)", path.display())
                }
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
    done.extend(left);
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
        opener: None,
        child: None,
        default_role_lines: true,
        memory_file: None,
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
    let home = default_home().unwrap_or_default();
    compose::compose(stacks, &context(stacks, adapter, adapter.audience(), Layers::Global, &home))
}

/// What an adapter's untracked file in a repo holds, before the tracked-instructions preamble, as written in
/// the place's first checkout (or dir).
pub fn repo_composed(stacks: &Stacks, place: &str, adapter: &dyn Adapter) -> Composed {
    let ctx = place_context(
        stacks,
        adapter,
        place_layers(stacks, place, false),
        first_path(stacks, place).as_deref(),
        &default_home().unwrap_or_default(),
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
    home: &Path,
) -> Context {
    let default_role = place_default_role(stacks, &layers);
    let mut ctx = context(stacks, adapter, adapter.audience(), layers, home);
    ctx.by_reference = true;
    ctx.role_dir = checkout.map(|c| c.join(ROLE_DIR));
    ctx.default_role = default_role;
    ctx.default_role_inline = adapter.default_role_inline();
    let settings = stacks.place_files(adapter.id());
    ctx.default_role_lines = settings.default_role_lines.unwrap_or(true);
    if settings.memory == stack::MemoryForm::Pointer {
        ctx.memory_file = ctx.role_dir.as_ref().map(|d| d.join(memory_file(adapter.id())));
    }
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
    home: &Path,
) -> (String, Composed) {
    let composed =
        compose::compose(stacks, &place_context(stacks, adapter, layers, Some(checkout), home));
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
            // A live memory store's index must be there (Codex switch-on review r1, R2).
            for m in compose::live_stores(stacks) {
                if let Some(why) = compose::live_index_problem(m) {
                    out.push(Finding::error("memory-live", why).at(compose::live_index(m), None));
                }
            }
            // `check` runs in the hooks: its findings name what failed, never a note's or the plan error's text, which
            // can name a private repo, a ledger's path or an import's failure (design note privacy-own-allow, sol-loop
            // r5, S11). `lords sync`, interactive, shows them.
            if !plan.notes.is_empty() {
                let n = plan.notes.len();
                out.push(Finding::info(
                    "sync-note",
                    format!("the sync plan has {n} note(s); `lords sync` shows them"),
                ));
            }
            out
        }
        Err(_) => vec![Finding::warning(
            "drift",
            "no sync plan: the ledger, a target or the stacks don't load or fit; `lords sync` says why",
        )],
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
    import_and_apply_between(public, private, opts, || {})
}

/// `import_and_apply`, with `between` run after the first plan and before the rest: the point where another process
/// can change what a plan read (the lock wait), for tests that must reach it without timing.
#[doc(hidden)]
pub fn import_and_apply_between(
    public: &Path,
    private: Option<&Path>,
    opts: &Options,
    between: impl FnOnce(),
) -> Result<Applied, String> {
    let stacks = Stacks::load(public, private)?;
    let settings = stacks.delivery();
    // One writer from the first plan to the last change (F8, C5; D23): a plan that will be applied is never made beside
    // another writer's half-done apply, and a delivery its own store commits start waits for this one.
    let lock = ApplyLock::wait(&opts.home, settings.lock_wait())??;
    let first = plan(&stacks, opts)?;
    if let Some(c) = first.items.iter().find(|i| i.action == Action::Conflict) {
        return Err(format!(
            "the plan has conflicts, e.g. {}: {}; resolve them first (nothing was changed)",
            c.path.display(),
            c.notes.join("; ")
        ));
    }
    between();
    let mut done = Vec::new();
    // The install stage imports and retires nothing.
    if opts.stage == Stage::Full {
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
    // A fresh plan, always, under the same lock: what a plan read reaches beyond the stacks (a tracked `AGENTS.md`, the
    // git index), and any of it can change after the first plan; apply checks only each item's destination and the
    // ledger (sync perf review r1, R2).
    let stacks = Stacks::load(public, private)?;
    let last = plan(&stacks, opts)?;
    let mut applied = apply_held(&last, &lock).map_err(|e| e.to_string())?;
    done.append(&mut applied.done);
    // Each declared repo's post-checkout hook, from the guard copy (F8, C3, D-3); then what this resolved leaves the
    // delivery's state (D5). Neither changes what was applied.
    let hooks = match opts.stage {
        Stage::Full => repo_hooks_in(&last, &stacks, &opts.home, &lock),
        _ => Vec::new(),
    };
    for (h, r) in &hooks {
        let what = crate::deliver::hook_what(h);
        done.push(match r {
            Ok(line) => format!("{what}: {line}"),
            Err(why) => format!("{what}: not installed: {why}"),
        });
    }
    if opts.stage == Stage::Full {
        if let Err(e) = crate::deliver::settle_by_hand(&opts.home, &settings, &last, &hooks, true) {
            done.push(format!("the delivery state couldn't be updated: {e}"));
        }
    }
    applied.done = done;
    Ok(applied)
}

/// `sync --apply`'s repo hooks (F8, C3): each the plan found missing or in an earlier form is installed from the guard
/// copy, while the copy answers `deliver`'s protocol, under the caller's lock: one line each, or why not.
fn repo_hooks_in(
    plan: &Plan,
    stacks: &Stacks,
    home: &Path,
    lock: &ApplyLock,
) -> Vec<(crate::hooks::RepoHook, Result<String, String>)> {
    use crate::hooks::HookState;
    let todo: Vec<&crate::hooks::RepoHook> = plan
        .hooks
        .iter()
        .filter(|h| matches!(h.state, HookState::Missing | HookState::Outdated))
        .collect();
    if todo.is_empty() {
        return Vec::new();
    }
    let copy = guard_place(home).and_then(|c| {
        match crate::process::deliver_protocol(c.path()) {
            Ok(Some(n)) if n == crate::deliver::PROTOCOL => Ok(c),
            Ok(_) => Err(format!(
                "the guard copy doesn't run `deliver` (protocol {}): run `lords guard install`, then sync again",
                crate::deliver::PROTOCOL
            )),
            Err(e) => Err(format!("{e}: run `lords guard install`, then sync again")),
        }
    });
    todo.into_iter()
        .map(|h| {
            let r = copy.clone().and_then(|c| {
                let args = crate::hooks::sync_args(stacks, &h.repo);
                crate::hooks::install_post_checkout_held(&h.checkout, c.path(), &args, home, lock)
                    .map(|i| format!("installed in {}", i.hook.display()))
            });
            (h.clone(), r)
        })
        .collect()
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
    /// The checkout or worktree measured, where the place has more than one: the largest file of its adapter there
    /// (Codex switch-on review r1, R1).
    pub at: Option<String>,
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
    /// The stack's warning line for this adapter's place files (`place_files.<adapter>.warn_bytes`), in place of
    /// `SIZE_WARN_PERCENT` of the limit.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub warn: Option<usize>,
}

impl Size {
    /// Past the warning line: the stack's (`warn`), else `SIZE_WARN_PERCENT` of the limit.
    pub fn over_line(&self) -> bool {
        match self.warn {
            Some(w) => self.reads > w,
            None => self.limit.is_some_and(|l| self.reads * 100 > l * SIZE_WARN_PERCENT),
        }
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
        let against = match self.warn {
            Some(w) => format!("{against}, past the stack's budget of {w}"),
            None => against,
        };
        let parts: Vec<String> = self.sections.iter().map(|(s, n)| format!("{s} {n}")).collect();
        let place = match &self.at {
            Some(at) => format!("{}, {at}", self.place),
            None => self.place.clone(),
        };
        format!("{}, {who} ({}): {against}; by section: {}", place, self.file, parts.join(", "))
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
    let home = default_home().unwrap_or_default();
    let mut out = Vec::new();
    for adapter in adapters::ALL.iter().copied() {
        let text = adapter.render(&user_composed(stacks, adapter));
        // The limit is on a place's project files; the user-level file is read whole (adapters.rs, `byte_limit`).
        out.push(Size {
            place: "user level".into(),
            at: None,
            adapter: adapter.id().to_string(),
            file: adapter.id().to_string(),
            bytes: text.len(),
            reads: text.len(),
            limit: None,
            sections: sections(&text, &text),
            enabled: true,
            warn: None,
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
        // Every checkout and worktree sync writes for, each with its own tracked preamble (Codex switch-on review r1,
        // R1: measuring only the first let a later one past the limit pass check).
        let paths: Vec<PathBuf> = match private.manifest.repos.get(&name) {
            Some(repo) => checkouts(repo)
                .map(|(roots, _, _)| roots.iter().map(|r| r.path().to_path_buf()).collect())
                .unwrap_or_else(|_| vec![dir.clone()]),
            None => vec![dir.clone()],
        };
        // Every adapter is measured, also one the place doesn't get yet: the line holds before it is switched on.
        for adapter in adapters::ALL.iter().copied() {
            let enabled = ids.is_empty() || ids.iter().any(|a| a == adapter.id());
            // The largest of the place's files decides; past one checkout, the label names it.
            let (at, text, composed) = paths
                .iter()
                .map(|p| {
                    let (text, composed) =
                        place_text(stacks, place_layers(stacks, &name, false), adapter, p, &home);
                    (p, text, composed)
                })
                .max_by_key(|(_, text, _)| text.len())
                .unwrap_or_else(|| {
                    let (text, composed) = place_text(
                        stacks,
                        place_layers(stacks, &name, false),
                        adapter,
                        &dir,
                        &home,
                    );
                    (&dir, text, composed)
                });
            let rendered = adapter.render(&composed);
            out.push(Size {
                place: label.clone(),
                at: (paths.len() > 1).then(|| at.display().to_string()),
                adapter: adapter.id().to_string(),
                file: adapter.repo_file().to_string(),
                bytes: text.len(),
                reads: text.len(),
                limit: adapter.byte_limit(),
                sections: sections(&text, &rendered),
                enabled,
                warn: stacks.place_files(adapter.id()).warn_bytes,
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
                at: None,
                adapter: String::new(),
                file: "skill copies".into(),
                bytes: each,
                reads: 0,
                limit: None,
                sections: parts,
                enabled: true,
                warn: None,
            });
        }
        for role in declared_roles(stacks) {
            let text = role_composed(stacks, place_layers(stacks, &name, true), &role, &home).text;
            out.push(Size {
                place: label.clone(),
                at: None,
                adapter: String::new(),
                file: format!("{ROLE_DIR}/{}", role_file(&role)),
                bytes: text.len(),
                reads: 0,
                limit: None,
                sections: sections(&text, &text),
                enabled: true,
                warn: None,
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
    // An adapter the place doesn't write for yet: measured, so the line holds before it is switched on, but only
    // info (orchestrator, 30 Oct, study P6). One it writes for: past the line a warning, past the hard limit an error,
    // since the harness would cut the file without a word (orchestrator, 30 Oct, item 2).
    // Past the warning line or the limit: a stack's budget above the limit never hides the limit (repair sol-loop
    // r1, R5).
    let mut out: Vec<crate::Finding> = all
        .into_iter()
        .filter(|s| s.over_line() || s.limit.is_some_and(|l| s.reads > l))
        .map(|s| match (s.enabled, s.limit.is_some_and(|l| s.reads > l)) {
            (true, true) => crate::Finding::error(
                "size",
                format!(
                    "{} (past the limit: the harness would cut it, so sync doesn't write it)",
                    s.describe()
                ),
            ),
            (true, false) => crate::Finding::warning("size", s.describe()),
            (false, _) => crate::Finding::info(
                "size",
                format!("{} (not switched on here; it matters once it is)", s.describe()),
            ),
        })
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
        let result = owner_update(&item, &mut Run::new(&home).unwrap(), &mut |_| {});
        fsguard::fail_links(false);
        assert!(result.is_err(), "{result:?}");
        assert_eq!(fsguard::read(&path).unwrap().as_deref(), Some(&b"OLD\n"[..]));
        assert_eq!(marks(fsguard::attrs(&path).unwrap()), marks(before));
        assert_eq!(fsguard::list_dir(path.parent().unwrap()).map(|v| v.len()).ok(), Some(1));
    }
}
