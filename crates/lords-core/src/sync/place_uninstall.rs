//! One place's uninstall (`lords uninstall --repo NAME --place DIR`; PI-13, design note
//! `2026-11-01-lords-uninstall-place.md` Q2–Q4): its own planner, since nothing is wanted there and the ledger alone
//! decides. Apply carries its plan out like any other (`sync::apply`).

use super::{
    checkouts, display, fold, tracked_at, Action, Class, Complete, Dest, Item, Options, Plan, Slot,
    Stage, Unit,
};
use crate::adapters;
use crate::compose;
use crate::config::expand_home;
use crate::fsguard::{self, Contained, Part, Root};
use crate::git;
use crate::ledger::{self, Ledger};
use crate::stack::{Stack, Stacks};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// The slot of a repo scope that gives exactly a record's parts and kind below `root`: the role dir for `repo:NAME`,
/// an adapter's file or skill copy for `repo:NAME/<adapter>` (any adapter this build knows, whether the repo declares
/// it now or not: a copy of one it dropped is still the place's).
fn repo_slot(root: &Root, o: &ledger::Owned) -> Option<(Contained, Slot)> {
    let slots = match o.scope.split_once('/') {
        None => vec![Slot::RoleDir],
        Some((_, id)) => {
            let a = adapters::get(id)?;
            let mut v = vec![Slot::AdapterFile(a)];
            if a.repo_skills_dir().is_some() {
                v.push(Slot::AdapterSkill(a));
            }
            v
        }
    };
    let name = o.parts.last().cloned().unwrap_or_default();
    slots.into_iter().filter(|s| s.kind() == o.kind).find_map(|s| {
        s.place(root, &name)
            .filter(|c| c.parts().iter().map(Part::as_str).eq(o.parts.iter().map(String::as_str)))
            .map(|c| (c, s))
    })
}

/// A file's text without Lords' managed block, when the block alone (begin line to end line, that line's ending
/// included) is what the record's hash says and the file holds more than it: what an owner added around a place's
/// instruction file. Every byte outside the block stays. `None` otherwise.
fn without_block(bytes: &[u8], sha: &str) -> Option<String> {
    let text = std::str::from_utf8(bytes).ok()?;
    let (b, e) = compose::block_bounds(text)?;
    (fsguard::sha(&bytes[b..e]) == sha && e - b < text.len())
        .then(|| format!("{}{}", &text[..b], &text[e..]))
}

/// Whether a record's root is a place, by the filesystem's identity of both: a root that is gone is none.
enum Here {
    Yes,
    No,
    Unsure(String),
}

/// One item per ledger record of the place, that is, of the repo's scopes with a root that is the same filesystem
/// object as DIR (never compared by spelling: D4, uninstall-place review r2, UP-R2-01). A copy holding what its
/// record says goes (Remove); an instruction file holding the owner's text around an unchanged block loses the block
/// (RemoveBlock); a record whose path is gone goes (Forget); anything else is kept with its record, and says why
/// (Report). Read only. DIR must be one of the repo's verified checkouts, or a worktree under its `worktree_roots`
/// sharing a configured checkout's git common dir (UP-R1-04); anything else refuses, as do stacks with a problem
/// (rule B, as uninstall refuses).
pub(super) fn plan(stacks: &Stacks, private: &Stack, opts: &Options) -> Result<Plan, String> {
    let dir = opts.place.as_deref().unwrap_or(Path::new(""));
    let Some(name) = opts.repo.as_deref() else {
        return Err(
            "a place (--place) is one checkout or worktree of a repo: it needs --repo".into()
        );
    };
    if opts.user_only {
        return Err("a place is a repo's, not the user level's: --place takes no --user".into());
    }
    if opts.adapter.is_some() {
        return Err(
            "one place's uninstall takes every adapter's records: it takes no --adapter".into()
        );
    }
    if opts.stage == Stage::Install {
        return Err("the install stage removes nothing, so it uninstalls no place".into());
    }
    let repo = private.manifest.repos.get(name).ok_or_else(|| {
        format!("no repo `{name}` in the private stack.toml (a place is one of a repo's checkouts or worktrees)")
    })?;
    let problems = stacks.all().map(|s| s.problems.len()).sum::<usize>();
    if problems > 0 {
        return Err(format!(
            "uninstall needs a complete view, and the stacks loaded with {problems} problem(s) (`lords check` lists them); nothing was planned"
        ));
    }
    let refused = |why: String| format!("{}: {why}; nothing was planned", display(dir));
    match fsguard::kind(dir) {
        Ok(fsguard::Kind::Dir) => {}
        Ok(_) => return Err(refused("no dir there".into())),
        Err(e) => return Err(refused(e)),
    }
    let (roots, _, _) = checkouts(repo).map_err(|e| format!("repo {name}: {e}"))?;
    let (mut matched, mut unsure): (Vec<Root>, Vec<String>) = (Vec::new(), Vec::new());
    for r in roots {
        match fsguard::same_file(r.path(), dir) {
            Ok(true) => matched.push(r),
            Ok(false) => {}
            Err(e) => unsure.push(e),
        }
    }
    let Some(place) = matched.first().cloned() else {
        let mut why = format!(
            "none of repo {name}'s checkouts, nor a worktree of it under its worktree_roots"
        );
        if !unsure.is_empty() {
            why.push_str(&format!(
                " ({} of them can't be compared: {})",
                unsure.len(),
                unsure.iter().take(3).cloned().collect::<Vec<_>>().join("; ")
            ));
        }
        return Err(refused(why));
    };
    // A discovered worktree is this repo's only if it shares a configured checkout's git common dir: a checkout of
    // another repo at a path git still lists for this one is not (UP-R1-04).
    let configured: Vec<PathBuf> = repo.checkouts.iter().map(|c| expand_home(c)).collect();
    if !configured.iter().any(|c| fsguard::same_file(c, dir).unwrap_or(false)) {
        let common = |p: &Path| {
            git::run(p, &["rev-parse", "--path-format=absolute", "--git-common-dir"])
                .map(|s| PathBuf::from(s.trim()))
        };
        let ours = common(place.path()).map_err(refused)?;
        let shared = configured.iter().any(|c| {
            common(c).is_ok_and(|theirs| fsguard::same_file(&theirs, &ours).unwrap_or(false))
        });
        if !shared {
            return Err(refused(format!(
                "a checkout of another repository (its git dir is {}), though repo {name}'s git still lists it (`git worktree prune` drops that)",
                ours.display()
            )));
        }
    }
    let ledger = Ledger::load(&opts.home)?;
    let mut plan = Plan {
        home: opts.home.clone(),
        ledger_revision: ledger.revision(),
        roots: matched.clone(),
        ..Plan::default()
    };
    let base = format!("repo:{name}");
    let mut here: BTreeMap<String, Here> = BTreeMap::new();
    let mut placed: Vec<(Item, ledger::Owned, Contained, Slot)> = Vec::new();
    let kept = |item: &mut Item, why: &str| {
        item.action = Action::Report;
        item.notes.push(format!("{why}: kept, with its ledger record"));
    };
    for o in ledger.owned.values() {
        if !(o.scope == base || o.scope.strip_prefix(&base).is_some_and(|r| r.starts_with('/'))) {
            continue;
        }
        let at = here.entry(o.root.clone()).or_insert_with(|| {
            let root = Path::new(&o.root);
            match fsguard::kind(root) {
                Ok(fsguard::Kind::Missing) => Here::No,
                Err(e) => Here::Unsure(e),
                Ok(_) => match fsguard::same_file(root, place.path()) {
                    Ok(true) => Here::Yes,
                    Ok(false) => Here::No,
                    Err(e) => Here::Unsure(e),
                },
            }
        });
        let mut item = Item::new(&format!("repo {name} (one place)"), o.path(), Action::Report);
        item.class = Class::Tool;
        item.scope = o.scope.clone();
        item.have = Some(o.sha.clone());
        item.units = vec![Unit::Repo(name.to_string())];
        match at {
            Here::No => continue,
            Here::Unsure(e) => {
                let why = format!("can't tell whether its root {} is this place ({e})", o.root);
                kept(&mut item, &why);
                plan.items.push(item);
                continue;
            }
            Here::Yes => {}
        }
        // The matched root the record names by key, since the ledger finds records by key; and a slot of its scope.
        let Some(root) =
            matched.iter().find(|r| ledger::key(r.path()) == ledger::key(Path::new(&o.root)))
        else {
            kept(&mut item, &format!("recorded under {}, another spelling of this place", o.root));
            plan.items.push(item);
            continue;
        };
        match repo_slot(root, o) {
            Some((c, slot)) => placed.push((item, o.clone(), c, slot)),
            None => {
                kept(&mut item, "no path sync writes in a place of its scope");
                plan.items.push(item);
            }
        }
    }
    // What the repo tracks at those paths, one git call per root (case-folded on Windows, as sync asks).
    let rels: Vec<String> = placed.iter().map(|(_, o, _, _)| o.parts.join("/")).collect();
    let mut tracked: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for (_, _, c, _) in &placed {
        if let std::collections::btree_map::Entry::Vacant(v) =
            tracked.entry(ledger::key(c.root().path()))
        {
            let asked: Vec<&str> = rels.iter().map(String::as_str).collect();
            v.insert(
                tracked_at(c.root().path(), &asked)
                    .map_err(|e| refused(format!("can't tell what the repo tracks there ({e})")))?,
            );
        }
    }
    // The instruction files kept, and the `.lords` dirs planned to go (by their items' index), for the hold below.
    let (mut kept_files, mut role_dirs): (Vec<PathBuf>, Vec<usize>) = (Vec::new(), Vec::new());
    for ((mut item, o, at, slot), rel) in placed.into_iter().zip(rels) {
        let rel = fold(&rel);
        let files = tracked.get(&ledger::key(at.root().path())).map(Vec::as_slice).unwrap_or(&[]);
        if files
            .iter()
            .any(|f| *f == rel || f.strip_prefix(&rel).is_some_and(|r| r.starts_with('/')))
        {
            kept(&mut item, "the repo tracks it (or a file below it) now, and uninstall never touches a tracked file");
            // A kept instruction file all the same: it holds what it names (sol-loop repair r1, U1).
            if matches!(slot, Slot::AdapterFile(_)) {
                kept_files.push(at.path().to_path_buf());
            }
            plan.items.push(item);
            continue;
        }
        let path = at.path().to_path_buf();
        let remove = |item: &mut Item, action: Action, note: &str| {
            item.action = action;
            item.path = path.clone();
            item.root = at.root().path().to_path_buf();
            item.dest = Dest::Tool(at.clone());
            item.notes.push(note.into());
        };
        match fsguard::kind(&path) {
            Err(e) => kept(&mut item, &format!("can't check it ({e})")),
            Ok(fsguard::Kind::Missing) => {
                item.action = Action::Forget;
                item.notes.push("gone already; its ledger record goes".into());
            }
            Ok(fsguard::Kind::Dir) if o.kind == "dir" => match fsguard::read_tree(&path) {
                Err(e) => kept(&mut item, &format!("can't read it ({e})")),
                Ok(t) if fsguard::tree_sha(&t) == o.sha => remove(
                    &mut item,
                    Action::Remove,
                    "Lords' copy, unchanged: removed with its record",
                ),
                Ok(_) => {
                    kept(&mut item, "changed since sync wrote it (a file edited, added or removed)")
                }
            },
            Ok(fsguard::Kind::File) if o.kind == "file" => match fsguard::read(&path) {
                Err(e) => kept(&mut item, &format!("can't read it ({e})")),
                Ok(None) => kept(&mut item, "gone while it was read; plan again"),
                Ok(Some(b)) if fsguard::sha(&b) == o.sha => remove(
                    &mut item,
                    Action::Remove,
                    "Lords' copy, unchanged: removed with its record",
                ),
                Ok(Some(b)) => match without_block(&b, &o.sha)
                    .filter(|_| matches!(slot, Slot::AdapterFile(_)))
                {
                    Some(rest) => {
                        remove(
                            &mut item,
                            Action::RemoveBlock,
                            "Lords' block unchanged, the owner's text around it: the block goes, the rest stays, the record goes",
                        );
                        item.current = Some(String::from_utf8_lossy(&b).into_owned());
                        item.proposed = Some(rest);
                        item.redacted = true;
                    }
                    None => kept(&mut item, "changed since sync wrote it"),
                },
            },
            Ok(k) => kept(
                &mut item,
                &format!("a {k:?} where its record says a {}", o.kind).to_lowercase(),
            ),
        }
        match (slot, &item.action) {
            (Slot::AdapterFile(_), Action::Report) => kept_files.push(path),
            (Slot::RoleDir, Action::Remove) => role_dirs.push(plan.items.len()),
            _ => {}
        }
        plan.items.push(item);
    }
    // A kept instruction file holds the `.lords` files it names (the role-files line, the memory pointer): `.lords`, one
    // record, stays with it (the Lords owner, 2 Nov, review r1 R1-01). A reading that fails holds it (design note §6b),
    // and so does text that isn't UTF-8, whose names can't be read (fix check r1, FC1-01: UTF-16).
    for i in role_dirs {
        let names: Option<Vec<String>> = fsguard::read_tree(&plan.items[i].path)
            .ok()
            .map(|t| t.into_keys().filter(|n| n != ".gitignore").collect());
        let holder = kept_files.iter().find(|f| match (fsguard::read(f), &names) {
            (Ok(None), _) => false,
            (Ok(Some(b)), Some(names)) => match std::str::from_utf8(&b) {
                Ok(text) => names.iter().any(|n| text.contains(n.as_str())),
                Err(_) => true,
            },
            _ => true,
        });
        if let Some(f) = holder {
            let item = &mut plan.items[i];
            item.action = Action::Report;
            item.notes =
                vec![format!("named by {}, which stays: kept, with its ledger record", display(f))];
        }
    }
    plan.items.sort_by_key(|i| ledger::key(&i.path));
    // Every record of the place was looked at, and the stacks loaded with no problem (rule B).
    plan.complete = Some(Complete(()));
    Ok(plan)
}
