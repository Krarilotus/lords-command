//! A safe-only plan's apply, place by place (F8, design note Harness/notes/2026-11-07-lords-f8-delivery.md §4 D3; F8
//! §15 M1, M2): each place's items are checked against the ledger and the disk, staged beside their paths, then swapped
//! in one by one with the ledger saved after each; a check or write that fails puts the place's earlier items back, so
//! a place is fully current or untouched. A file's swap is one rename over it, so a reader finds the old file or the new
//! one, never none; a dir's is two renames, absent between them.
//!
//! What a stopped run leaves beside a path (no journal: nothing kills a delivery from inside, design note D-2) is
//! named by its kind: `new` (a staged version: only new bytes) and `done` (an old version a swap checked, Lords' own
//! bytes) go at the place's next transaction; `old` (a backup a swap hadn't checked yet, which may hold an owner's edit)
//! holds the place until someone looks.

use super::{Action, Applied, Dest, HeldItem, HeldPlace, Hold, Item, PlaceId, Plan, Step};
use crate::fsguard::{self, Contained, Kind, Tree};
use crate::ledger::{self, Ledger, Owned};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// What one item does to its path.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Op {
    ReplaceFile,
    ReplaceDir,
    CreateFile,
    CreateDir,
    Forget,
}

/// One item of a place, as it goes through the transaction.
struct Change<'a> {
    item: &'a Item,
    op: Op,
    /// Its path, below the root the plan verified; `None` only for a forget of a record with no place.
    at: Option<Contained>,
    /// The new version, staged beside it.
    staged: Option<Contained>,
    /// The old version beside it, from its link or move there (`old`), renamed `done` once checked, until the place is
    /// through.
    backup: Option<Contained>,
    /// The hash of the new version, as staged.
    new: String,
    /// Its ledger record before.
    old: Option<Owned>,
    /// The new version is at the path (or the record is forgotten).
    swapped: bool,
    /// The old version beside it was checked to be the record's.
    checked: bool,
}

/// Why a place wasn't written.
enum Stop {
    /// Put back whole (or never touched): the place is held, with why.
    Held(String),
    /// Couldn't be put back whole: the apply stops, with why and what is left where.
    Broken(String),
}

impl From<String> for Stop {
    fn from(e: String) -> Stop {
        Stop::Held(e)
    }
}

/// The hash of what is at `p` (a dir's tree hash), `None` where nothing is.
fn hash(p: &Path) -> Result<Option<String>, String> {
    Ok(match fsguard::kind(p)? {
        Kind::Missing => None,
        Kind::Dir => Some(fsguard::tree_sha(&fsguard::read_tree(p)?)),
        Kind::File => Some(fsguard::sha(&fsguard::read(p)?.unwrap_or_default())),
    })
}

/// Removes a file or dir the transaction made.
fn remove(p: &Contained) -> Result<(), String> {
    match fsguard::kind(p.path())? {
        Kind::Dir => fsguard::delete_tree(p),
        Kind::File => fsguard::delete_file(p),
        Kind::Missing => Ok(()),
    }
}

/// The places a safe-only plan's changes fall into: items that share a place are one group (a shared `via` file joins
/// its targets); each group's label is its first place. In the plan's order, within a group and between groups.
fn groups(plan: &Plan) -> Vec<(PlaceId, Vec<&Item>)> {
    let changing: Vec<&Item> =
        plan.items.iter().filter(|i| i.changes() && i.action != Action::RetireEntry).collect();
    // Union of places: each item joins the group of any place it has.
    let mut of: BTreeMap<String, usize> = BTreeMap::new();
    let mut groups: Vec<(PlaceId, Vec<&Item>)> = Vec::new();
    for i in changing {
        let keys: Vec<String> = i.places.iter().map(PlaceId::key).collect();
        let found: Vec<usize> = {
            let mut f: Vec<usize> = keys.iter().filter_map(|k| of.get(k).copied()).collect();
            f.sort();
            f.dedup();
            f
        };
        let g = match found.first() {
            Some(&g) => {
                // Merge every other group this item touches into the first.
                for &other in found.iter().skip(1).rev() {
                    let (_, items) = std::mem::replace(
                        &mut groups[other],
                        (PlaceId::Target(String::new()), Vec::new()),
                    );
                    groups[g].1.extend(items);
                    for v in of.values_mut().filter(|v| **v == other) {
                        *v = g;
                    }
                }
                g
            }
            None => {
                let label = i.places.first().cloned().unwrap_or(PlaceId::Target(String::new()));
                groups.push((label, Vec::new()));
                groups.len() - 1
            }
        };
        groups[g].1.push(i);
        for k in keys {
            of.insert(k, g);
        }
    }
    groups.into_iter().filter(|(_, items)| !items.is_empty()).collect()
}

/// A place as `Step` callbacks name it: its dir, or a target's name.
fn place_path(p: &PlaceId) -> PathBuf {
    match p {
        PlaceId::Dir(d) => d.clone(),
        PlaceId::Target(n) => PathBuf::from(n),
    }
}

/// Writes a safe-only plan place by place (module doc), under the lock the caller holds, with `ledger` loaded under it.
/// A place whose check or write failed is put back whole and held (`Applied::held`); one that can't be put back whole
/// stops the apply with an error that says what is where.
pub(super) fn apply(
    plan: &Plan,
    ledger: &mut Ledger,
    between: &mut dyn FnMut(Step, &Path),
) -> Result<Applied, String> {
    let mut applied = Applied::default();
    let held = |place: &PlaceId, items: &[&Item], why: String| {
        let first = items[0];
        let item = HeldItem {
            target: first.target.clone(),
            path: first.path.clone(),
            reason: Hold::WriteRefused,
            note: why,
        };
        let sync = super::safety::sync_command(place, items);
        HeldPlace { place: place.clone(), items: vec![item], sync }
    };
    // What stopped runs left beside the plan's paths, every item's, in sync or not, before any place is written: cleared,
    // or the place held (module doc). Each dir is listed once.
    let mut listed: BTreeMap<PathBuf, Vec<String>> = BTreeMap::new();
    let mut left: BTreeMap<String, (PlaceId, &Item, String)> = BTreeMap::new();
    for i in &plan.items {
        let Dest::Tool(at) = &i.dest else { continue };
        if let Err(Stop::Held(why) | Stop::Broken(why)) = sweep(at, &mut listed) {
            for p in &i.places {
                left.entry(p.key()).or_insert_with(|| (p.clone(), i, why.clone()));
            }
        }
    }
    for (place, items) in groups(plan) {
        let found = items.iter().flat_map(|i| &i.places).find_map(|p| left.remove(&p.key()));
        if let Some((_, _, why)) = found {
            applied.held.push(held(&place, &items, why));
            continue;
        }
        let at = place_path(&place);
        match one_place(&plan.home, ledger, &items, &at, plan.lock_wait.0, between) {
            Ok(done) => applied.done.extend(done),
            Err(Stop::Held(why)) => applied.held.push(held(&place, &items, why)),
            Err(Stop::Broken(why)) => {
                return Err(format!(
                "{why}. Apply stopped there; {} change(s) were made before it, each place whole",
                applied.done.len()
            ))
            }
        }
    }
    // A place with nothing to change but a backup no one has checked beside one of its paths: held, so it is named.
    for (_, (place, item, why)) in left {
        applied.held.push(held(&place, &[item], why));
    }
    Ok(applied)
}

/// One place's transaction (module doc): its changes as lines, or why it was held or stopped.
fn one_place(
    home: &Path,
    ledger: &mut Ledger,
    items: &[&Item],
    at: &Path,
    wait: Duration,
    between: &mut dyn FnMut(Step, &Path),
) -> Result<Vec<String>, Stop> {
    let mut changes = Vec::new();
    for i in items {
        changes.push(prepare(i, ledger)?);
    }
    for k in 0..changes.len() {
        if let Err(why) = stage(&mut changes[k]) {
            clear_staged(&mut changes);
            return Err(Stop::Held(why));
        }
    }
    between(Step::Staged, at);
    for k in 0..changes.len() {
        let deadline = Instant::now() + wait;
        if let Err(why) = swap(&mut changes[k], ledger, home, deadline) {
            return Err(match undo(&mut changes, ledger, home, wait) {
                Ok(()) => Stop::Held(why),
                Err(left) => {
                    Stop::Broken(format!("{why}; and the place couldn't be put back whole: {left}"))
                }
            });
        }
        between(Step::Swapped, at);
    }
    let mut left = Vec::new();
    for c in &mut changes {
        if let Some(b) = c.backup.take() {
            if let Err(e) = remove(&b) {
                left.push(e);
            }
        }
    }
    // The place is through; a backup that couldn't go is Lords' own old bytes, and the next transaction removes it.
    let _ = left;
    Ok(changes.iter().map(|c| format!("{:?} {}", c.item.action, c.item.path.display())).collect())
}

/// An item's change, checked against the ledger and the disk now, under the lock (check 0), and its path cleared of
/// what a stopped run left there (module doc).
fn prepare<'a>(item: &'a Item, ledger: &Ledger) -> Result<Change<'a>, Stop> {
    let stale = |what: &str| {
        Stop::Held(format!("{}: {what} since the plan; the place is held", item.path.display()))
    };
    let op = match (&item.action, item.tree.is_some()) {
        (Action::Update | Action::Refresh, false) => Op::ReplaceFile,
        (Action::Update | Action::Refresh, true) => Op::ReplaceDir,
        (Action::Create | Action::Install, false) => Op::CreateFile,
        (Action::Create | Action::Install, true) => Op::CreateDir,
        (Action::Forget, _) => Op::Forget,
        (a, _) => {
            return Err(Stop::Held(format!(
                "{}: {a:?} isn't what a safe-only apply writes",
                item.path.display()
            )))
        }
    };
    let at = match &item.dest {
        Dest::Tool(c) => Some(c.clone()),
        Dest::None if op == Op::Forget => None,
        _ => {
            return Err(Stop::Held(format!(
                "{}: no place below a verified root",
                item.path.display()
            )))
        }
    };
    let old = ledger.get(&item.path).cloned();
    let now = hash(&item.path)?;
    match op {
        Op::ReplaceFile | Op::ReplaceDir => {
            if old.as_ref().map(|o| &o.sha) != item.have.as_ref() {
                return Err(stale("its ledger record changed"));
            }
            if now != item.have {
                return Err(stale("it changed"));
            }
        }
        Op::CreateFile | Op::CreateDir if now.is_some() => return Err(stale("something appeared")),
        Op::Forget if item.gone && now.is_some() => {
            return Err(stale("something is at its path again"))
        }
        _ => {}
    }
    let new = String::new();
    Ok(Change {
        item,
        op,
        at,
        staged: None,
        backup: None,
        new,
        old,
        swapped: false,
        checked: false,
    })
}

/// What a stopped run left beside `p` (module doc): staged versions and checked old ones go; an unchecked backup that
/// holds what `p` holds is the same old version and goes too; any other holds the place. `listed`: each dir's names,
/// read once, so a path with nothing beside it costs no more.
fn sweep(p: &Contained, listed: &mut BTreeMap<PathBuf, Vec<String>>) -> Result<(), Stop> {
    let (Some(dir), Some(name)) = (p.path().parent(), p.path().file_name()) else { return Ok(()) };
    if !listed.contains_key(dir) {
        let names = match fsguard::kind(dir)? {
            Kind::Dir => fsguard::list_dir(dir)?
                .iter()
                .filter_map(|e| e.file_name().map(|n| n.to_string_lossy().into_owned()))
                .collect(),
            _ => Vec::new(),
        };
        listed.insert(dir.to_path_buf(), names);
    }
    let prefix = format!(".{}.lords-", name.to_string_lossy());
    if !listed[dir].iter().any(|n| n.starts_with(&prefix)) {
        return Ok(());
    }
    for left in fsguard::siblings(p, "new")?.iter().chain(&fsguard::siblings(p, "done")?) {
        remove(left)?;
    }
    for b in fsguard::siblings(p, "old")? {
        if hash(b.path())? == hash(p.path())? && fsguard::kind(b.path())? == Kind::File {
            fsguard::delete_file(&b)?;
        } else {
            return Err(Stop::Held(format!(
                "a stopped change left {} beside it, which may hold an owner's edit: compare it with {}, keep what you need, and delete it",
                b.path().display(),
                p.path().display()
            )));
        }
    }
    Ok(())
}

/// The new version, staged beside the path.
fn stage(c: &mut Change) -> Result<(), String> {
    let Some(at) = &c.at else { return Ok(()) };
    match c.op {
        Op::ReplaceFile | Op::CreateFile => {
            let staged = fsguard::sibling(at, "new")?;
            let bytes = c.item.proposed.as_deref().unwrap_or("").as_bytes();
            fsguard::create_file(&staged, bytes)?;
            c.new = fsguard::sha(bytes);
            c.staged = Some(staged);
        }
        Op::ReplaceDir | Op::CreateDir => {
            let staged = fsguard::sibling(at, "new")?;
            let tree: &Tree = c.item.tree.as_deref().ok_or("a dir planned without its contents")?;
            c.staged = Some(staged.clone());
            c.new = fsguard::put_tree(&staged, tree)?;
        }
        Op::Forget => {}
    }
    Ok(())
}

fn clear_staged(changes: &mut [Change]) {
    for c in changes {
        if let Some(s) = c.staged.take() {
            let _ = remove(&s);
        }
    }
}

/// One item in (module doc): checked again as it goes, its ledger record saved after. Each step records what `undo`
/// needs before the next one that can fail (sol-loop r1 S2): the old version's name beside the path, and whether the
/// new one is in, so an error at any point is put back with the place.
fn swap(c: &mut Change, ledger: &mut Ledger, home: &Path, deadline: Instant) -> Result<(), String> {
    let path = c.item.path.display().to_string();
    let changed = || {
        format!("{path}: changed while it was replaced; the owner's version stays, and the place is held")
    };
    let have = c.item.have.clone();
    let kind = if matches!(c.op, Op::ReplaceDir | Op::CreateDir) { "dir" } else { "file" };
    match (c.op, &c.at, c.staged.clone()) {
        (Op::Forget, _, _) => {
            if c.item.gone && fsguard::kind(&c.item.path)? != Kind::Missing {
                return Err(format!("{path}: something is at its path again"));
            }
            ledger.forget(&c.item.path);
            c.swapped = true;
        }
        (Op::ReplaceFile, Some(at), Some(staged)) => {
            let backup = fsguard::sibling(at, "old")?;
            fsguard::link_as(at, &backup)?;
            c.backup = Some(backup.clone());
            // Check 1: the file is still the record's.
            if hash(backup.path())? != have {
                return Err(changed());
            }
            fsguard::rename_over(&staged, at, deadline)?;
            (c.staged, c.swapped) = (None, true);
            // Check 2: no edit in place landed between check 1 and the rename; if one did, `undo` puts its object back.
            if hash(backup.path())? != have {
                return Err(changed());
            }
            let done = fsguard::sibling(at, "done")?;
            fsguard::move_to(&backup, &done)?;
            (c.backup, c.checked) = (Some(done), true);
        }
        (Op::ReplaceDir, Some(at), Some(staged)) => {
            let backup = fsguard::sibling(at, "old")?;
            fsguard::move_to(at, &backup)?;
            // The path is empty until the new tree is in; `undo` moves the old one back.
            c.backup = Some(backup.clone());
            if hash(backup.path())? != have {
                return Err(changed());
            }
            fsguard::move_to(&staged, at)?;
            (c.staged, c.swapped) = (None, true);
            let done = fsguard::sibling(at, "done")?;
            fsguard::move_to(&backup, &done)?;
            (c.backup, c.checked) = (Some(done), true);
        }
        (Op::CreateFile, Some(at), Some(staged)) => {
            // A link where nothing may be: refused if anything appeared since.
            fsguard::link_as(&staged, at)?;
            c.swapped = true;
            fsguard::delete_file(&staged)?;
            c.staged = None;
        }
        (Op::CreateDir, Some(at), Some(staged)) => {
            fsguard::move_to(&staged, at)?;
            (c.staged, c.swapped) = (None, true);
        }
        _ => return Err(format!("{path}: not staged")),
    }
    if let Some(at) = &c.at {
        if c.op != Op::Forget {
            ledger.record(at, kind, &c.new, &c.item.scope);
            fsguard::verify_inside(&c.item.path, &c.item.root)?;
        }
    }
    ledger.save(home)
}

/// Puts a place's items back, last first, from whatever step each reached (module doc; sol-loop r1 S2): a staged
/// version goes; an old version beside its path goes back where the path is empty or holds what this transaction put
/// there, and is dropped where it is only a second name of the file still at the path; a created path that holds what
/// this put there goes; each put back gets its ledger record as it was. A path holding anything else is its owner's
/// edit since, and stays, with the new record, for the next plan to hold; an old version a swap checked goes beside it,
/// one it hadn't stays, which holds the place at the next transaction. Every item is tried, a rename blocked by a
/// reader until `wait`; the error lists what couldn't be put back.
fn undo(
    changes: &mut [Change],
    ledger: &mut Ledger,
    home: &Path,
    wait: Duration,
) -> Result<(), String> {
    let mut left = Vec::new();
    let deadline = Instant::now() + wait;
    for c in changes.iter_mut().rev() {
        let mut one = || -> Result<(), String> {
            if let Some(s) = c.staged.take() {
                remove(&s)?;
            }
            let (key, path, old) = (ledger::key(&c.item.path), c.item.path.clone(), c.old.clone());
            let restore = |l: &mut Ledger| match &old {
                Some(o) => {
                    l.owned.insert(key.clone(), o.clone());
                }
                None => l.forget(&path),
            };
            let Some(at) = c.at.clone() else {
                if c.swapped {
                    restore(ledger);
                }
                c.swapped = false;
                return Ok(());
            };
            let now = hash(at.path())?;
            let ours = c.swapped && now.as_deref() == Some(c.new.as_str());
            let back = match (c.op, c.backup.take()) {
                (Op::Forget, _) => c.swapped,
                (Op::ReplaceFile | Op::ReplaceDir, Some(b)) if now.is_none() => {
                    fsguard::move_to(&b, &at)?;
                    true
                }
                (Op::ReplaceFile, Some(b)) if ours => {
                    fsguard::rename_over(&b, &at, deadline)?;
                    true
                }
                (Op::ReplaceDir, Some(b)) if ours => {
                    let aside = fsguard::sibling(&at, "new")?;
                    fsguard::move_to(&at, &aside)?;
                    fsguard::move_to(&b, &at)?;
                    remove(&aside)?;
                    true
                }
                (Op::ReplaceFile, Some(b)) if !c.swapped => {
                    fsguard::delete_file(&b)?;
                    false
                }
                (Op::CreateFile | Op::CreateDir, _) if ours => {
                    remove(&at)?;
                    true
                }
                (_, Some(b)) if c.checked => {
                    remove(&b)?;
                    false
                }
                _ => false,
            };
            if back {
                restore(ledger);
            }
            c.swapped = false;
            Ok(())
        };
        if let Err(e) = one() {
            left.push(e);
        }
    }
    if let Err(e) = ledger.save(home) {
        left.push(e);
    }
    match left.is_empty() {
        true => Ok(()),
        false => Err(left.join("; ")),
    }
}
