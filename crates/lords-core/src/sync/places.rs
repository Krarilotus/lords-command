//! A safe-only plan's apply, place by place (F8, design note Harness/notes/2026-11-07-lords-f8-delivery.md §4 D3; F8
//! §15 M1, M2): each place's items are checked against the ledger and the disk, staged beside their paths, then swapped
//! in one by one with the ledger saved after each; a check or write that fails puts the place's earlier items back, so
//! a place is fully current or untouched. A file's swap is one rename over it, so a reader finds the old file or the new
//! one, never none; a dir's is two renames, absent between them.
//!
//! A transaction removes only what it recorded making (review r1, R1-01; design note §14.1): before it makes anything
//! beside a path, every name it will make there (`new`, a staged version; `old`, the current version's second name;
//! `done`, the old version once checked; a dir's `new` aside in an undo) goes into the record in the tool home, with
//! the hash it will hold. A later apply, whatever its plan, removes each recorded path that still holds its hash; any
//! other `.<name>.lords-*` beside a plan's path is kept as it is and holds its place (`left-beside`).
//!
//! On Windows every file a place publishes or replaces is held open from before its first swap to the end (R1-02,
//! §14.2), so a reader that doesn't share delete is found while nothing is published, and none can come between a
//! swap and its undo. A place's dirs go before its files: a dir's move is the one step such a reader can still refuse,
//! and it then comes while no file is published (the stated limit of §14.2).

use super::{Action, Applied, Dest, HeldItem, HeldPlace, Hold, Item, PlaceId, Plan, Step};
use crate::fsguard::{self, Contained, Holding, Kind, Part, Root, Tree};
use crate::ledger::{self, Ledger, Owned};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// The record's file, in the tool home beside the ledger.
pub const RECORD: &str = "place-transaction.json";

/// What place transactions made beside the plans' paths and haven't removed yet, each written here before it is made.
#[derive(Debug, Default, Serialize, Deserialize)]
struct Record {
    made: Vec<Made>,
}

/// One path a transaction made beside `beside` (a plan item's path, in the ledger's form): its name, whether it is a
/// dir, and the hash it holds as made.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct Made {
    beside: String,
    name: String,
    dir: bool,
    sha: String,
}

fn record_file(home: &Path) -> Result<Contained, String> {
    Ok(Root::home(home)?.join(&[Part::new(RECORD)?]))
}

/// The record; empty without a file. One that can't be read is an error, so nothing is removed by a guess (A18).
fn read_record(home: &Path) -> Result<Record, String> {
    let at = record_file(home)?;
    match fsguard::read(at.path())? {
        None => Ok(Record::default()),
        Some(bytes) => serde_json::from_slice(&bytes).map_err(|e| {
            format!(
                "{}: can't be read ({e}); nothing beside a place's files was removed: look at it, keep what you need beside the place files, and delete it",
                at.path().display()
            )
        }),
    }
}

/// The record written whole by one rename, or its file removed when it lists nothing.
fn write_record(home: &Path, r: &Record, wait: Duration) -> Result<(), String> {
    let at = record_file(home)?;
    if r.made.is_empty() {
        return match fsguard::kind(at.path())? {
            Kind::Missing => Ok(()),
            _ => fsguard::delete_file(&at),
        };
    }
    let text = serde_json::to_vec_pretty(r).map_err(|e| e.to_string())?;
    let staged = fsguard::sibling(&at, "new")?;
    fsguard::create_file(&staged, &text)?;
    if let Err(e) = fsguard::rename_over(&staged, &at, Instant::now() + wait) {
        let _ = fsguard::delete_file(&staged);
        return Err(e);
    }
    Ok(())
}

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
    /// The names this change makes beside its path, given before anything is made (`name`): the staged version, the
    /// old version's, the checked old version's, a dir's aside for an undo; each with its record line.
    names: Names,
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

/// A change's names beside its path (`Change::names`).
#[derive(Default)]
struct Names {
    new: Option<Contained>,
    old: Option<Contained>,
    done: Option<Contained>,
    aside: Option<Contained>,
    made: Vec<(Made, Contained)>,
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
    let held = |place: &PlaceId, items: &[&Item], reason: Hold, why: String| {
        let first = items[0];
        let item =
            HeldItem { target: first.target.clone(), path: first.path.clone(), reason, note: why };
        let sync = super::safety::sync_command(place, items);
        HeldPlace { place: place.clone(), items: vec![item], sync }
    };
    let wait = plan.lock_wait.0;
    // What stopped runs left beside the plan's paths, every item's, in sync or not, before any place is written: what
    // the record lists and still holds what it recorded goes; anything else holds its place (module doc). A record
    // that can't be read stops the apply before anything is removed.
    let mut left = sweep(plan, wait)?;
    for (place, items) in groups(plan) {
        let found = items.iter().flat_map(|i| &i.places).find_map(|p| left.remove(&p.key()));
        if let Some((_, _, why)) = found {
            applied.held.push(held(&place, &items, Hold::LeftBeside, why));
            continue;
        }
        if let Some(why) = in_sync_changed(plan, &items) {
            applied.held.push(held(&place, &items, Hold::WriteRefused, why));
            continue;
        }
        let at = place_path(&place);
        match one_place(&plan.home, ledger, &items, &at, wait, between) {
            Ok(done) => applied.done.extend(done),
            Err(Stop::Held(why)) => {
                applied.held.push(held(&place, &items, Hold::WriteRefused, why))
            }
            Err(Stop::Broken(why)) => {
                return Err(format!(
                "{why}. Apply stopped there; {} change(s) were made before it, each place whole",
                applied.done.len()
            ))
            }
        }
    }
    // A place with nothing to change but something left beside one of its paths: held, so it is named.
    for (_, (place, item, why)) in left {
        applied.held.push(held(&place, &[item], Hold::LeftBeside, why));
    }
    Ok(applied)
}

/// What stopped transactions left beside the plan's paths, each path once (module doc): a recorded path that still
/// holds its recorded hash goes, with its line; one that's gone loses its line; what's left beside a path after that
/// holds its places, by place key, with why. Record lines beside paths this plan doesn't see stay for a later one.
fn sweep(
    plan: &Plan,
    wait: Duration,
) -> Result<BTreeMap<String, (PlaceId, &Item, String)>, String> {
    let mut record = read_record(&plan.home)?;
    let mut seen: BTreeSet<String> = BTreeSet::new();
    let mut listed: BTreeMap<PathBuf, Vec<String>> = BTreeMap::new();
    let mut left: BTreeMap<String, (PlaceId, &Item, String)> = BTreeMap::new();
    for i in &plan.items {
        let Dest::Tool(at) = &i.dest else { continue };
        let here = ledger::key(at.path());
        if !seen.insert(here.clone()) {
            continue;
        }
        let why = match clear_beside(at, &here, &record, &mut listed)
            .and_then(|()| kept_beside(at, &mut listed))
        {
            Ok(None) => continue,
            Ok(Some(why)) | Err(why) => why,
        };
        for p in &i.places {
            left.entry(p.key()).or_insert_with(|| (p.clone(), i, why.clone()));
        }
    }
    record.made.retain(|m| !seen.contains(&m.beside));
    write_record(&plan.home, &record, wait)?;
    Ok(left)
}

/// Removes each path the record lists beside `at` that still holds what it recorded, a file as a file and a dir as a
/// dir; any other stays as it is. A dir it removed from is listed again.
fn clear_beside(
    at: &Contained,
    here: &str,
    record: &Record,
    listed: &mut BTreeMap<PathBuf, Vec<String>>,
) -> Result<(), String> {
    for m in record.made.iter().filter(|m| m.beside == here) {
        let s = fsguard::sibling_named(at, &m.name)?;
        let same = match (fsguard::kind(s.path())?, m.dir) {
            (Kind::Dir, true) | (Kind::File, false) => {
                hash(s.path())?.as_deref() == Some(&m.sha[..])
            }
            _ => false,
        };
        if same {
            remove(&s)?;
            if let Some(dir) = at.path().parent() {
                listed.remove(dir);
            }
        }
    }
    Ok(())
}

/// Why the place of `at` is held for what is still beside it with Lords' sibling name (`.<name>.lords-…`), if
/// anything is: no record lists it holding what it recorded, so it may be its owner's, and stays as it is. Each dir is
/// listed once.
fn kept_beside(
    at: &Contained,
    listed: &mut BTreeMap<PathBuf, Vec<String>>,
) -> Result<Option<String>, String> {
    let (Some(dir), Some(name)) = (at.path().parent(), at.path().file_name()) else {
        return Ok(None);
    };
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
    let left: Vec<&String> = listed[dir].iter().filter(|n| n.starts_with(&prefix)).collect();
    if left.is_empty() {
        return Ok(None);
    }
    let names: Vec<String> = left.iter().map(|n| dir.join(n).display().to_string()).collect();
    Ok(Some(format!(
        "{} beside {}: no transaction recorded making it as it is, so it is kept; compare it with {}, keep what you need, and delete it",
        names.join(", "),
        at.path().display(),
        at.path().display()
    )))
}

/// Why a place's file the plan leaves as it is (in sync) no longer holds what the plan read, if one doesn't: an edit
/// since the plan, before any of the place's files is written, holds the place whole like an edit of a file it writes
/// (sol-loop r2 S5).
fn in_sync_changed(plan: &Plan, items: &[&Item]) -> Option<String> {
    let keys: Vec<String> = items.iter().flat_map(|i| &i.places).map(PlaceId::key).collect();
    let in_place = |i: &&Item| i.places.iter().any(|p| keys.contains(&p.key()));
    let kept = plan.items.iter().filter(|i| i.action == Action::InSync).filter(in_place);
    kept.filter(|i| matches!(i.dest, Dest::Tool(_))).find_map(|i| match hash(&i.path) {
        Ok(h) if h == i.have => None,
        Ok(_) => Some(format!("{}: changed since the plan; the place is held", i.path.display())),
        Err(e) => Some(e),
    })
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
    // Dirs before files (module doc; §14.2, decided (a)): a dir's move is refused, if at all, while no file is in.
    changes.sort_by_key(|c| !matches!(c.op, Op::ReplaceDir | Op::CreateDir));
    // Every name beside a path, and its record line, before anything is made there (§14.1).
    for c in &mut changes {
        name(c)?;
    }
    let mut record = read_record(home)?;
    record.made.extend(changes.iter().flat_map(|c| c.names.made.iter().map(|(m, _)| m.clone())));
    write_record(home, &record, wait)?;
    let result = transact(home, ledger, &mut changes, at, wait, between);
    // What it made and removed leaves the record; what is still beside a path stays recorded, for the next apply. A
    // line left by a record that couldn't be rewritten names a path that is gone, which the next apply drops.
    if let Ok(mut record) = read_record(home) {
        let gone: Vec<&Made> = changes
            .iter()
            .flat_map(|c| &c.names.made)
            .filter(|(_, s)| matches!(fsguard::kind(s.path()), Ok(Kind::Missing)))
            .map(|(m, _)| m)
            .collect();
        record.made.retain(|m| !gone.contains(&m));
        let _ = write_record(home, &record, wait);
    }
    result
}

/// The transaction proper (module doc): staged; old versions linked beside their paths and every file it publishes or
/// replaces held open; swapped one by one; the holdings dropped and the checked old versions removed. A failure before
/// the first swap leaves the place untouched; one after it puts the place back (`undo`).
fn transact(
    home: &Path,
    ledger: &mut Ledger,
    changes: &mut [Change],
    at: &Path,
    wait: Duration,
    between: &mut dyn FnMut(Step, &Path),
) -> Result<Vec<String>, Stop> {
    for k in 0..changes.len() {
        if let Err(why) = stage(&mut changes[k]) {
            clear_staged(changes);
            return Err(Stop::Held(why));
        }
    }
    let deadline = Instant::now() + wait;
    let mut holding: Vec<Holding> = Vec::new();
    for k in 0..changes.len() {
        if let Err(why) = ready(&mut changes[k], &mut holding, deadline) {
            drop(holding);
            unready(changes);
            return Err(Stop::Held(why));
        }
    }
    between(Step::Staged, at);
    for k in 0..changes.len() {
        let deadline = Instant::now() + wait;
        if let Err(why) = swap(&mut changes[k], ledger, home, deadline) {
            let undone = undo(changes, ledger, home, wait);
            drop(holding);
            return Err(match undone {
                Ok(()) => Stop::Held(why),
                Err(left) => {
                    Stop::Broken(format!("{why}; and the place couldn't be put back whole: {left}"))
                }
            });
        }
        between(Step::Swapped, at);
    }
    drop(holding);
    // The place is through; a backup that couldn't go stays recorded, and the next apply removes it.
    for c in changes.iter_mut() {
        if let Some(b) = c.backup.take() {
            let _ = remove(&b);
        }
    }
    Ok(changes.iter().map(|c| format!("{:?} {}", c.item.action, c.item.path.display())).collect())
}

/// A change's names beside its path, each fresh (`fsguard::sibling`), with the record line of each: the hash of the
/// new version for its staged one and a dir's aside, the record's hash for the old version and the checked one.
fn name(c: &mut Change) -> Result<(), String> {
    let Some(at) = c.at.clone() else { return Ok(()) };
    let dir = matches!(c.op, Op::ReplaceDir | Op::CreateDir);
    let new = match (&c.item.tree, dir) {
        (Some(t), true) => fsguard::tree_sha(t),
        _ => fsguard::sha(c.item.proposed.as_deref().unwrap_or("").as_bytes()),
    };
    let have = c.item.have.clone().unwrap_or_default();
    let beside = ledger::key(at.path());
    let mut made = Vec::new();
    let mut one = |what: &str, sha: &str| -> Result<Contained, String> {
        let s = fsguard::sibling(&at, what)?;
        let name =
            s.path().file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        made.push((Made { beside: beside.clone(), name, dir, sha: sha.to_string() }, s.clone()));
        Ok(s)
    };
    let mut names = Names::default();
    match c.op {
        Op::Forget => return Ok(()),
        Op::CreateFile | Op::CreateDir => names.new = Some(one("new", &new)?),
        Op::ReplaceFile => {
            names.new = Some(one("new", &new)?);
            names.old = Some(one("old", &have)?);
            names.done = Some(one("done", &have)?);
        }
        Op::ReplaceDir => {
            names.new = Some(one("new", &new)?);
            names.old = Some(one("old", &have)?);
            names.done = Some(one("done", &have)?);
            names.aside = Some(one("new", &new)?);
        }
    }
    names.made = made;
    c.names = names;
    Ok(())
}

/// Before the first swap (§14.2): a file's old version gets its second name beside it, and every file the place will
/// publish or replace is held open, each tried until `deadline`. A reader without delete sharing is found here.
fn ready(c: &mut Change, holding: &mut Vec<Holding>, deadline: Instant) -> Result<(), String> {
    match (c.op, &c.at) {
        (Op::ReplaceFile, Some(at)) => {
            let old = c.names.old.clone().ok_or("no name for the old version")?;
            fsguard::link_as(at, &old)?;
            c.backup = Some(old);
            holding.push(fsguard::hold_open(at, deadline)?);
        }
        (Op::CreateFile, Some(_)) => {}
        _ => return Ok(()),
    }
    if let Some(s) = &c.staged {
        holding.push(fsguard::hold_open(s, deadline)?);
    }
    Ok(())
}

/// Undoes `ready` and `stage` when the place won't be swapped: staged versions and the old versions' second names go.
fn unready(changes: &mut [Change]) {
    clear_staged(changes);
    for c in changes {
        if let Some(b) = c.backup.take() {
            let _ = fsguard::delete_file(&b);
        }
    }
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
        names: Names::default(),
        backup: None,
        new,
        old,
        swapped: false,
        checked: false,
    })
}

/// The new version, staged beside the path, at the name `name` recorded for it.
fn stage(c: &mut Change) -> Result<(), String> {
    if c.at.is_none() || c.op == Op::Forget {
        return Ok(());
    }
    let staged = c.names.new.clone().ok_or("no name for the staged version")?;
    match c.op {
        Op::ReplaceFile | Op::CreateFile => {
            let bytes = c.item.proposed.as_deref().unwrap_or("").as_bytes();
            fsguard::create_file(&staged, bytes)?;
            c.new = fsguard::sha(bytes);
            c.staged = Some(staged);
        }
        Op::ReplaceDir | Op::CreateDir => {
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
            // The old version's second name, given by `ready`.
            let backup =
                c.backup.clone().ok_or_else(|| format!("{path}: no old version beside it"))?;
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
            let done = c.names.done.clone().ok_or_else(|| format!("{path}: no name for done"))?;
            fsguard::move_to(&backup, &done)?;
            (c.backup, c.checked) = (Some(done), true);
        }
        (Op::ReplaceDir, Some(at), Some(staged)) => {
            let backup = c.names.old.clone().ok_or_else(|| format!("{path}: no name for old"))?;
            fsguard::move_to(at, &backup)?;
            // The path is empty until the new tree is in; `undo` moves the old one back.
            c.backup = Some(backup.clone());
            if hash(backup.path())? != have {
                return Err(changed());
            }
            fsguard::move_to(&staged, at)?;
            (c.staged, c.swapped) = (None, true);
            let done = c.names.done.clone().ok_or_else(|| format!("{path}: no name for done"))?;
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
                    let aside = c.names.aside.clone().ok_or("no name for the aside")?;
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
