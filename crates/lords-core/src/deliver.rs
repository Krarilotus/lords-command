//! `lords deliver` (F8, design note Harness/notes/2026-11-07-lords-f8-delivery.md): a commit to a store, or a new
//! worktree, reaches its places with nobody running a command. One safe-only plan (D13), made and applied under the
//! apply lock, place by place (`sync::places`); whatever it held, or why it failed, goes into one typed state file that
//! `lords paths` reads (`status`). Every wait has the stack's bound (`[delivery]`): the lock wait, the attempts and
//! their pause, and the deadline no attempt or plan starts past. Nothing kills a delivery from inside, and its hooks
//! start it without a deadline of their own (design note D-2).

use crate::fsguard::{self, Contained, Kind, Part, Root};
use crate::ledger::{self, Ledger};
use crate::stack::{Delivery, Stacks, Store};
use crate::sync::{self, ApplyLock, HeldItem, HeldPlace, Hold, PlaceId, Refused, Step};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// Set in a delivery's own environment, so the commits it makes (its store work) start no delivery: the post-commit
/// hook exits at once when it is set (C7). `lords deliver` sets it for itself.
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
    /// Store work held: a memory import or a retirement whose commit failed.
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
    /// D13's `Hold` (`changed-copy`, `new-place`, …, `not-ready`, `write-refused`), or `delivery-failed`.
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

/// The state file's place below the tool home (`[delivery] state`, plain parts).
fn state_place(home: &Path, settings: &Delivery) -> Result<Contained, String> {
    Ok(Root::home(home)?.join(&settings.state_parts()?))
}

/// The state file's path.
pub fn state_file(home: &Path, settings: &Delivery) -> Result<PathBuf, String> {
    Ok(state_place(home, settings)?.path().to_path_buf())
}

/// The state, or `None` where there is none.
pub fn read(home: &Path, settings: &Delivery) -> Result<Option<State>, String> {
    let p = state_place(home, settings)?;
    match fsguard::read_text(p.path())? {
        None => Ok(None),
        Some(t) => serde_json::from_str(&t)
            .map(Some)
            .map_err(|e| format!("{} can't be read as a delivery state: {e}", p.path().display())),
    }
}

/// Changes the state under its own short lock (the state file's name plus `.lock`), so a writer that doesn't hold the
/// apply lock (a delivery that found it busy) never loses another's change: read, changed by `change`, written whole
/// by one rename, so it is never absent (F8-R1-03). A state that can't be read starts again empty.
fn update(
    home: &Path,
    settings: &Delivery,
    change: impl FnOnce(State) -> State,
) -> Result<State, String> {
    let p = state_place(home, settings)?;
    let mut parts = settings.state_parts()?;
    let name = parts.pop().map(|n| n.as_str().to_string()).unwrap_or_default();
    fsguard::create_dir_all(&Root::home(home)?.join(&parts))?;
    parts.push(Part::new(&format!("{name}.lock"))?);
    let lock_at = Root::home(home)?.join(&parts);
    // `[delivery] state_lock_secs` (review r1's non-blocker: no fixed limit).
    let limit = Duration::from_secs(settings.state_lock_secs);
    let until = Instant::now() + limit;
    let _lock = loop {
        match fsguard::try_lock(&lock_at, &format!("pid {}", crate::process::id()))? {
            fsguard::TryLock::Held(l) => break l,
            fsguard::TryLock::Busy(_) if Instant::now() < until => {
                std::thread::sleep(Duration::from_millis(100))
            }
            fsguard::TryLock::Busy(h) => {
                return Err(format!(
                    "{} is held past [delivery] state_lock_secs = {} ({h})",
                    lock_at.path().display(),
                    settings.state_lock_secs
                ))
            }
        }
    };
    let state = change(read(home, settings).ok().flatten().unwrap_or_default());
    let state = State { format: 1, ..state };
    let text = serde_json::to_string_pretty(&state).map_err(|e| e.to_string())?;
    let staged = fsguard::sibling(&p, "new")?;
    fsguard::create_file(&staged, text.as_bytes())?;
    if let Err(e) = fsguard::rename_over(&staged, &p, Instant::now() + limit) {
        let _ = fsguard::delete_file(&staged);
        return Err(e);
    }
    Ok(state)
}

/// Whether delivery is set up with this tool home: the state file is there, or the ledger records a store's
/// post-commit hook (§15 M4: a missing state then reads stale, never "not set up").
pub fn configured(home: &Path, settings: &Delivery) -> bool {
    let there = state_place(home, settings)
        .and_then(|p| fsguard::kind(p.path()))
        .is_ok_and(|k| k != Kind::Missing);
    let hook = |o: &ledger::Owned| {
        o.scope.starts_with("hooks:") && o.parts.last().is_some_and(|p| p == "post-commit")
    };
    there || Ledger::load(home).is_ok_and(|l| l.owned.values().any(hook))
}

/// Delivery as `lords paths` shows it (C6).
pub enum Status {
    /// Never set up with this home: nothing is judged.
    NotConfigured,
    /// The state, and why every place is stale, if it is.
    Read(State, Option<String>),
}

/// Where delivery stands (C6, K5): the state, and why every place is stale where it is: the last whole delivery failed,
/// none ran yet, the state is missing or can't be read, or a store's committed stack moved past it. Read only.
pub fn status(stacks: &Stacks, home: &Path) -> Status {
    let settings = stacks.delivery();
    if !configured(home, &settings) {
        return Status::NotConfigured;
    }
    let state = match read(home, &settings) {
        Ok(Some(s)) => s,
        Ok(None) => {
            return Status::Read(
                State::default(),
                Some("no delivery state yet: run `lords deliver`".into()),
            )
        }
        Err(e) => return Status::Read(State::default(), Some(format!("{e}: run `lords deliver`"))),
    };
    let why = if let Some(f) = &state.failed {
        Some(format!("the last delivery failed: {}; run `{}`", f.why, f.command))
    } else if state.trees.is_empty() {
        Some("no whole delivery yet: run `lords deliver`".into())
    } else {
        let dirs = std::iter::once(&stacks.public).chain(&stacks.private).map(|s| s.root.as_path());
        let limit = Duration::from_secs(settings.store_git_secs);
        match dirs.map(|d| tree(d, limit)).collect::<Result<Vec<_>, _>>() {
            Ok(now) if now == state.trees => None,
            Ok(_) => Some(
                "a store's committed stack moved past the last delivery (a store's post-commit log is in its git dir): run `lords deliver`"
                    .into(),
            ),
            Err(e) => Some(format!("{e}: run `lords deliver`")),
        }
    };
    Status::Read(state, why)
}

/// One git read of a store, stopped past `limit` (`[delivery] store_git_secs`).
fn git(dir: &Path, args: &[&str], limit: Duration) -> Result<String, String> {
    let out = crate::process::git_bounded(dir, args, limit)?;
    if !out.status.success() {
        return Err(format!(
            "{}: git {} failed: {}",
            dir.display(),
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// The git tree of the stack committed in `dir` (a store's stack dir, its repo's root or below it), keyed by the dir.
fn tree(dir: &Path, limit: Duration) -> Result<StoreTree, String> {
    Ok(StoreTree { stack: ledger::key(dir), tree: git(dir, &["rev-parse", "HEAD:./"], limit)? })
}

/// Each store's committed stack, refusing a store whose stack dir holds anything not committed: edited, untracked, or
/// ignored, which the stack loads all the same (F8 K4; sol-loop r1 S1). Only committed entries, which passed the
/// stores' commit hooks, reach agents.
fn committed(dirs: &[&Path], limit: Duration) -> Result<Vec<StoreTree>, String> {
    let mut trees = Vec::new();
    for d in dirs {
        let status = "--no-optional-locks status --porcelain --untracked-files=all --ignored -- .";
        if !git(d, &status.split(' ').collect::<Vec<_>>(), limit)?.is_empty() {
            return Err(format!(
                "{} has uncommitted (edited, untracked or ignored) files in its stack, and a delivery takes committed entries only; nothing was written: the next commit delivers",
                d.display()
            ));
        }
        trees.push(tree(d, limit)?);
    }
    Ok(trees)
}

/// Whether a file git tracks in a store's stack dir was written at or after `since` (sol-loop r2 S4): a write put back
/// before the next status leaves no other trace.
fn written_since(
    dirs: &[&Path],
    since: std::time::SystemTime,
    limit: Duration,
) -> Result<bool, String> {
    for d in dirs {
        for f in git(d, &["ls-files", "-z"], limit)?.split('\0').filter(|f| !f.is_empty()) {
            match fsguard::modified(d.join(f)) {
                Ok(t) if t >= since => return Ok(true),
                Ok(_) => {}
                Err(e) => return Err(format!("{}: {e}", d.join(f).display())),
            }
        }
    }
    Ok(false)
}

/// Whether a commit in `dir` starts a delivery: `dir` is the checkout of a store `[delivery] on_commit` lists.
fn triggers(public: &Path, private: Option<&Path>, dir: &Path, settings: &Delivery) -> bool {
    let at = |root: &Path| {
        crate::git::toplevel(root).is_some_and(|t| ledger::key(&t) == ledger::key(dir))
    };
    let stores = [(Store::Public, Some(public)), (Store::Private, private)];
    stores.iter().any(|(s, root)| settings.on_commit.contains(s) && root.is_some_and(at))
}

/// The command that delivers one new worktree again.
fn worktree_command(repo: &str, place: &Path) -> String {
    let p = crate::adapters::slashes(place).replace('\'', "'\\''");
    format!("lords deliver --repo {repo} --place '{p}' --new-worktree")
}

/// A place as the state names it, and its key for comparing (`PlaceId::key`).
pub(crate) fn place_key(place: &str) -> String {
    match place.strip_prefix("target ") {
        Some(n) => format!("target:{n}"),
        None => ledger::key(Path::new(place)),
    }
}

fn state_item(i: &HeldItem) -> StateItem {
    StateItem {
        path: i.path.display().to_string(),
        reason: i.reason.to_string(),
        note: i.note.clone(),
    }
}

fn state_place_of(h: &HeldPlace) -> StatePlace {
    StatePlace {
        place: h.place.to_string(),
        items: h.items.iter().map(state_item).collect(),
        command: h.sync.clone(),
    }
}

/// A repo's post-checkout hook as the state and sync's lines name it: its repo and the checkout it is installed from.
pub(crate) fn hook_what(h: &crate::hooks::RepoHook) -> String {
    format!("post-checkout of repo {} ({})", h.repo, crate::adapters::slashes(&h.checkout))
}

/// The repos whose post-checkout hook a plan found other than as this build writes it (C3), with what resolves each.
fn hook_notes(plan: &sync::Plan) -> Vec<StateNote> {
    use crate::hooks::HookState;
    let mut notes = Vec::new();
    for h in &plan.hooks {
        let fix = format!("lords sync --repo {} --apply", h.repo);
        let (why, command) = match &h.state {
            HookState::Current => continue,
            HookState::Missing => (
                "missing: new worktrees of this repo get no files until it is installed"
                    .to_string(),
                fix,
            ),
            HookState::Outdated => ("in an earlier form than this build writes".into(), fix),
            HookState::Changed(why) => (
                format!("{why}: Lords leaves it"),
                format!("put Lords' hook back or remove it, then {fix}"),
            ),
        };
        notes.push(StateNote { what: hook_what(h), why, command });
    }
    notes
}

/// The places a plan saw: of its items, and of what it held.
fn seen(plan: &sync::Plan) -> Vec<String> {
    let held = plan.held.iter().flat_map(|h| h.places.iter().map(|p| p.place.key()));
    plan.items.iter().flat_map(|i| i.places.iter().map(PlaceId::key)).chain(held).collect()
}

/// What a scoped safe-only apply leaves in the state (a new worktree's delivery, `sync --safe-only --apply`): the
/// entries of the places its plan saw are replaced by what it held; every other entry stays.
fn scoped(mut s: State, plan: &sync::Plan, applied: &sync::Applied) -> State {
    let seen = seen(plan);
    s.places.retain(|p| !seen.contains(&place_key(&p.place)));
    let held = plan.held.iter().flat_map(|h| h.places.iter()).chain(&applied.held);
    s.places.extend(held.map(state_place_of));
    s
}

/// `sync --safe-only --apply` by hand (F8, D5): the state's entries for the places its plan saw become what it held.
/// Only a state that is there is changed: a sync never sets delivery up.
pub fn settle_safe_only(
    home: &Path,
    settings: &Delivery,
    plan: &sync::Plan,
    applied: &sync::Applied,
) -> Result<(), String> {
    if !state_place(home, settings)
        .and_then(|p| fsguard::kind(p.path()))
        .is_ok_and(|k| k != Kind::Missing)
    {
        return Ok(());
    }
    update(home, settings, |s| scoped(s, plan, applied)).map(|_| ())
}

/// A sync by hand of the full stage (F8, D5): it wrote every item of its plan, so the places it saw leave the state
/// (with a complete view only: without one, a wanted removal may still wait); the repo hooks it installed leave it,
/// and the ones it couldn't go in, with why; where it did store work (`store_work`: `sync --apply` of the whole
/// machine or a repo, not `--place`), held store work goes. The scoped sync a held place names resolves it here too
/// (sol-loop r1 S7). Only a state that is there is changed: a sync never sets delivery up.
pub fn settle_by_hand(
    home: &Path,
    settings: &Delivery,
    plan: &sync::Plan,
    hooks: &[(crate::hooks::RepoHook, Result<String, String>)],
    store_work: bool,
) -> Result<(), String> {
    if !state_place(home, settings)
        .and_then(|p| fsguard::kind(p.path()))
        .is_ok_and(|k| k != Kind::Missing)
    {
        return Ok(());
    }
    update(home, settings, |mut s| {
        if plan.complete.is_some() {
            let seen = seen(plan);
            s.places.retain(|p| !seen.contains(&place_key(&p.place)));
        }
        let mut notes = hook_notes(plan);
        for (h, r) in hooks {
            let what = hook_what(h);
            notes.retain(|n| n.what != what);
            if let Err(why) = r {
                let command = format!("lords sync --repo {} --apply", h.repo);
                notes.push(StateNote { what, why: why.clone(), command });
            }
        }
        let planned: Vec<String> = plan.hooks.iter().map(hook_what).collect();
        s.hooks.retain(|n| !planned.contains(&n.what));
        s.hooks.extend(notes);
        if store_work {
            s.store_work.clear();
        }
        s
    })
    .map(|_| ())
}

/// Imports memory and retires ended entries, each as its own commit (C7), with `MARKER` in this process's environment
/// so no commit starts a delivery: what it did, and what it couldn't, as held store work.
fn store_work(public: &Path, private: Option<&Path>) -> (Vec<String>, Vec<StateNote>) {
    let (mut done, mut held) = (Vec::new(), Vec::new());
    let stacks = match Stacks::load(public, private) {
        Ok(s) => s,
        Err(e) => {
            let command = "lords sync --apply".to_string();
            held.push(StateNote { what: "store work".into(), why: e, command });
            return (done, held);
        }
    };
    if let Some(p) = stacks.private.as_ref().filter(|p| !p.manifest.memory_stores.is_empty()) {
        let pending: Vec<String> = match crate::ops::import_preview(p, None) {
            Ok(rs) => rs
                .into_iter()
                .filter(|r| !r.added.is_empty() || !r.changed.is_empty())
                .map(|r| r.store)
                .collect(),
            // A store that can't be read is held store work too, never left out (sol-loop r1 S8).
            Err(why) => {
                let command = "lords import".to_string();
                held.push(StateNote { what: "memory import".into(), why, command });
                Vec::new()
            }
        };
        if !pending.is_empty() {
            match crate::ops::import(p, None, true) {
                Ok(rs) => done.extend(rs.iter().filter_map(|r| {
                    r.commit
                        .as_ref()
                        .map(|c| format!("imported memory from {}: commit {c}", r.store))
                })),
                Err(why) => held.push(StateNote {
                    what: format!("memory import from {}", pending.join(", ")),
                    why,
                    command: "lords import".into(),
                }),
            }
        }
    }
    match Stacks::load(public, private)
        .and_then(|s| crate::ops::retire_expired(&s, &crate::ops::today(), true))
    {
        Ok(v) => done.extend(v.into_iter().map(|(id, c)| {
            format!(
                "retired expired {id}{}",
                c.map(|c| format!(": commit {c}")).unwrap_or_default()
            )
        })),
        Err(why) => held.push(StateNote {
            what: "retirement of ended entries".into(),
            why,
            command: "lords sync --apply".into(),
        }),
    }
    (done, held)
}

/// One delivery (module doc).
pub fn run(public: &Path, private: Option<&Path>, opts: &Options) -> Result<Report, String> {
    run_between(public, private, opts, &mut |_, _| {})
}

/// What an attempt asks for next.
enum Next {
    Done,
    /// Plan again: after the pause (`pause`), and why, for the state if no attempt is left.
    Again {
        pause: bool,
        why: String,
    },
}

/// `run`, calling `between` at each step with the place it concerns (the tool home for the plan).
#[doc(hidden)]
pub fn run_between(
    public: &Path,
    private: Option<&Path>,
    opts: &Options,
    between: &mut dyn FnMut(Step, &Path),
) -> Result<Report, String> {
    let start = Instant::now();
    let loaded = Stacks::load(public, private);
    let settings = loaded.as_ref().map(Stacks::delivery).unwrap_or_default();
    let mut report = Report { file: state_file(&opts.home, &settings)?, ..Report::default() };
    if let Err(e) = &loaded {
        return fail(opts, &settings, &mut report, e.clone());
    }
    if let Some(dir) = &opts.on_commit {
        if !triggers(public, private, dir, &settings) {
            report.not_a_trigger = true;
            return Ok(report);
        }
    }
    let deadline = start + Duration::from_secs(settings.deadline_secs);
    loop {
        report.attempts += 1;
        let left = deadline.saturating_duration_since(Instant::now());
        let lock = match ApplyLock::wait(&opts.home, settings.lock_wait().min(left)) {
            Ok(Ok(l)) => l,
            Ok(Err(busy)) | Err(busy) => return fail(opts, &settings, &mut report, busy),
        };
        let next = attempt(public, private, opts, &settings, &lock, deadline, between, &mut report);
        drop(lock);
        match next {
            Ok(Next::Done) => return Ok(report),
            Ok(Next::Again { pause, why }) => {
                let out = report.attempts >= settings.attempts.max(1);
                if out || Instant::now() >= deadline {
                    return give_up(opts, &settings, &mut report, pause, why);
                }
                if pause {
                    let p = Duration::from_secs(settings.retry_pause_secs);
                    std::thread::sleep(p.min(deadline.saturating_duration_since(Instant::now())));
                }
            }
            Err(e) => return fail(opts, &settings, &mut report, e),
        }
    }
}

/// One plan and apply under `lock` (D1).
#[allow(clippy::too_many_arguments)]
fn attempt(
    public: &Path,
    private: Option<&Path>,
    opts: &Options,
    settings: &Delivery,
    lock: &ApplyLock,
    deadline: Instant,
    between: &mut dyn FnMut(Step, &Path),
    report: &mut Report,
) -> Result<Next, String> {
    let home = &opts.home;
    let dirs: Vec<&Path> = std::iter::once(public).chain(private).collect();
    let (mut trees, mut held_work) = (Vec::new(), Vec::new());
    // No attempt, store work or plan starts past the deadline (sol-loop r1 S9).
    let past = |what: &str| {
        format!(
            "{what} past the delivery's deadline ([delivery] deadline_secs = {}); no place was written",
            settings.deadline_secs
        )
    };
    if Instant::now() > deadline {
        return Err(past("the apply lock came"));
    }
    let git_limit = Duration::from_secs(settings.store_git_secs);
    if opts.worktree.is_none() {
        trees = committed(&dirs, git_limit)?;
        if opts.on_commit.is_some() {
            if let Some(s) = read(home, settings).ok().flatten() {
                if s.trees == trees && s.failed.is_none() {
                    report.unchanged = true;
                    report.state = s;
                    return Ok(Next::Done);
                }
            }
        }
        if Instant::now() > deadline {
            return Err(past("the stores' check ended"));
        }
        if settings.store_work {
            let (done, held) = store_work(public, private);
            // Its commits moved a store's tree; one it couldn't make may have left a store unclean (a file someone
            // edited meanwhile): then the delivery fails with why, the store work's own reasons named.
            if !done.is_empty() || !held.is_empty() {
                trees = committed(&dirs, git_limit).map_err(|e| {
                    let why: Vec<String> =
                        held.iter().map(|n| format!("{}: {}", n.what, n.why)).collect();
                    match why.is_empty() {
                        true => e,
                        false => format!("{e} (after the store work: {})", why.join("; ")),
                    }
                })?;
            }
            report.done.extend(done);
            held_work = held;
        }
    }
    if Instant::now() > deadline {
        return Err(past("the store work ended"));
    }
    between(Step::Captured, home);
    // Every store file written from here on was perhaps read half way (sol-loop r2 S4).
    let loading = std::time::SystemTime::now();
    let stacks = Stacks::load(public, private)?;
    if Instant::now() > deadline {
        return Err(past("the stacks' load ended"));
    }
    let (repo, place) = match &opts.worktree {
        Some((r, p)) => (Some(r.clone()), Some(p.clone())),
        None => (None, None),
    };
    let sync_opts = sync::Options {
        home: home.clone(),
        stage: sync::Stage::SafeOnly,
        new_worktree: place.is_some(),
        repo,
        place,
        ..sync::Options::default()
    };
    let plan = sync::plan(&stacks, &sync_opts)?;
    if Instant::now() > deadline {
        return Err(past("planning ended"));
    }
    between(Step::Planned, home);
    // What was planned is what the stores committed: a store edited or committed while it loaded, or a file written
    // since the load began, even put back (a stack edit whose commit was refused), is planned again, so no uncommitted
    // text is delivered under a committed tree (K4; sol-loop r1 S4, r2 S4).
    if opts.worktree.is_none()
        && (committed(&dirs, git_limit)? != trees || written_since(&dirs, loading, git_limit)?)
    {
        let why = "a store's stack changed while the delivery planned";
        return Ok(Next::Again { pause: false, why: why.into() });
    }
    if Instant::now() > deadline {
        return Err(past("the stores' check after planning ended"));
    }
    let not_ready = plan.held.iter().flat_map(|h| &h.places).flat_map(|p| &p.items);
    if let Some(i) = not_ready.clone().find(|i| i.reason == Hold::NotReady) {
        return Ok(Next::Again { pause: true, why: i.note.clone() });
    }
    let applied = match sync::apply_held_between(&plan, lock, between) {
        Ok(a) => a,
        Err(Refused::LedgerChanged) => {
            let why = "the ledger changed between the plan and its apply, under each attempt (a writer outside the apply lock?)";
            return Ok(Next::Again { pause: false, why: why.into() });
        }
        Err(Refused::Other(e)) => return Err(e),
    };
    report.done.extend(applied.done.iter().cloned());
    let whole = opts.worktree.is_none();
    report.state = update(home, settings, |old| match whole {
        true => {
            let held = plan.held.clone().unwrap_or_default();
            State {
                format: 1,
                trees,
                failed: None,
                places: held.places.iter().chain(&applied.held).map(state_place_of).collect(),
                everything: held.everything.iter().map(state_item).collect(),
                hooks: hook_notes(&plan),
                store_work: held_work,
            }
        }
        false => scoped(old, &plan, &applied),
    })?;
    Ok(Next::Done)
}

/// A delivery that couldn't run or finish (C5, C6): recorded in the state, never only in a log. A whole delivery's is
/// its `failed`; a new worktree's is that place, held, with the command that delivers it again.
fn fail(
    opts: &Options,
    settings: &Delivery,
    report: &mut Report,
    why: String,
) -> Result<Report, String> {
    let recorded = update(&opts.home, settings, |mut s| {
        match &opts.worktree {
            None => s.failed = Some(Failed { why: why.clone(), command: "lords deliver".into() }),
            Some((repo, place)) => {
                let item = StateItem {
                    path: place.display().to_string(),
                    reason: "delivery-failed".into(),
                    note: why.clone(),
                };
                let entry = StatePlace {
                    place: place.display().to_string(),
                    items: vec![item],
                    command: worktree_command(repo, place),
                };
                s.places.retain(|p| place_key(&p.place) != ledger::key(place));
                s.places.push(entry);
            }
        }
        s
    });
    match recorded {
        Ok(s) => {
            report.state = s;
            Err(why)
        }
        Err(e) => Err(format!("{why}; and it couldn't be recorded in the delivery state: {e}")),
    }
}

/// The last attempt still asked for another (C5): a new worktree not ready is held, with the command that delivers
/// it; a ledger that kept changing fails the delivery.
fn give_up(
    opts: &Options,
    settings: &Delivery,
    report: &mut Report,
    not_ready: bool,
    why: String,
) -> Result<Report, String> {
    let (true, Some((repo, place))) = (not_ready, &opts.worktree) else {
        return fail(opts, settings, report, why);
    };
    let n = report.attempts;
    report.state = update(&opts.home, settings, |mut s| {
        s.places.retain(|p| place_key(&p.place) != ledger::key(place));
        s.places.push(StatePlace {
            place: place.display().to_string(),
            items: vec![StateItem {
                path: place.display().to_string(),
                reason: Hold::NotReady.to_string(),
                note: format!("{why} ({n} attempt(s))"),
            }],
            command: worktree_command(repo, place),
        });
        s
    })?;
    Ok(report.clone())
}
