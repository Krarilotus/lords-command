//! One dropped target's uninstall (`lords uninstall --target NAME`; D21, design note
//! `2026-11-07-lords-delivery-integrity.md` §3): its own planner, since the stack no longer says where the target was and
//! the ledger alone decides. Apply carries its plan out like any other (`sync::apply`).

use super::{display, Action, Class, Complete, Dest, Item, Options, Plan};
use crate::fsguard::{self, Part, Root};
use crate::ledger::{self, Ledger};
use crate::stack::{Stack, Stacks};
use std::collections::BTreeSet;
use std::path::Path;

/// One item per ledger record of scope `target:NAME`, for a target NAME the private stack no longer declares, the stacks
/// loaded with no problem (rule B). A record is placed only where a target's slot can be, as the stack can't say where
/// the target was (Sol review r3, R3-03's class): its root reached through no link, the one dir every record of the
/// target names, and its one part a skill's name for a dir or a plain name for a file. A placed copy holding what its
/// record says goes (Remove); a record whose path is gone goes (Forget); every other record is kept with its path, and
/// says why (Report). No other record is read for removal. Read only.
pub(super) fn plan(stacks: &Stacks, private: &Stack, opts: &Options) -> Result<Plan, String> {
    let name = opts.target.as_deref().unwrap_or("");
    if name.is_empty() {
        return Err("--target takes the name of a target the stack no longer declares".into());
    }
    if opts.repo.is_some() || opts.user_only || opts.place.is_some() || opts.adapter.is_some() {
        return Err(
            "one dropped target's uninstall takes its name alone: no --user, --repo, --place or --adapter".into()
        );
    }
    if private.manifest.targets.iter().any(|t| t.name == name) {
        return Err(format!(
            "the private stack.toml declares target `{name}`, which sync would write again: drop it there first; nothing was planned"
        ));
    }
    let problems = stacks.all().map(|s| s.problems.len()).sum::<usize>();
    if problems > 0 {
        return Err(format!(
            "uninstall needs a complete view, and the stacks loaded with {problems} problem(s) (`lords check` lists them); nothing was planned"
        ));
    }
    let ledger = Ledger::load(&opts.home)?;
    let scope = format!("target:{name}");
    let records: Vec<&ledger::Owned> = ledger.owned.values().filter(|o| o.scope == scope).collect();
    // A target has one dir: records naming two are no target's, so none of them is placed (a false record, R3-03).
    let roots: BTreeSet<String> = records.iter().map(|o| ledger::key(Path::new(&o.root))).collect();
    if roots.len() > 1 {
        return Err(format!(
            "the ledger ({}) records target `{name}` in {} dirs ({}), and a target has one; nothing was planned until the ledger is put right",
            ledger::file(&opts.home).display(),
            roots.len(),
            roots.into_iter().collect::<Vec<_>>().join(", ")
        ));
    }
    let root = records.first().map(|o| Root::recorded(Path::new(&o.root)));
    let mut plan =
        Plan { home: opts.home.clone(), ledger_revision: ledger.revision(), ..Plan::default() };
    let kept = |item: &mut Item, why: &str| {
        item.action = Action::Report;
        item.notes.push(format!("{why}: kept, with its ledger record"));
    };
    for o in records {
        let mut item = Item::new(&format!("target {name} (dropped)"), o.path(), Action::Report);
        item.class = Class::Tool;
        item.scope = o.scope.clone();
        item.have = Some(o.sha.clone());
        // Its place: the target's dir, through no link, and one name a target writes there.
        let at = match (&root, o.parts.as_slice()) {
            (Some(Err(e)), _) => Err(e.clone()),
            (Some(Ok(r)), [part]) => {
                let part = if o.kind == "dir" { Part::skill(part) } else { Part::new(part) };
                part.map(|p| r.join(&[p]))
            }
            _ => Err("no path a target writes: not one name below its dir".into()),
        };
        let at = match at {
            Ok(at) => at,
            Err(e) => {
                kept(&mut item, &format!("can't place it ({e})"));
                plan.items.push(item);
                continue;
            }
        };
        let remove = |item: &mut Item| {
            item.action = Action::Remove;
            item.root = at.root().path().to_path_buf();
            item.dest = Dest::Tool(at.clone());
            item.notes.push("Lords' copy, unchanged: removed with its record".into());
        };
        match fsguard::kind(at.path()) {
            Err(e) => kept(&mut item, &format!("can't check it ({e})")),
            Ok(fsguard::Kind::Missing) => {
                item.action = Action::Forget;
                item.notes.push("gone already; its ledger record goes".into());
            }
            Ok(fsguard::Kind::Dir) if o.kind == "dir" => match fsguard::read_tree(at.path()) {
                Err(e) => kept(&mut item, &format!("can't read it ({e})")),
                Ok(t) if fsguard::tree_sha(&t) == o.sha => remove(&mut item),
                Ok(_) => {
                    kept(&mut item, "changed since sync wrote it (a file edited, added or removed)")
                }
            },
            Ok(fsguard::Kind::File) if o.kind == "file" => match fsguard::read(at.path()) {
                Err(e) => kept(&mut item, &format!("can't read it ({e})")),
                Ok(None) => kept(&mut item, "gone while it was read; plan again"),
                Ok(Some(b)) if fsguard::sha(&b) == o.sha => remove(&mut item),
                Ok(Some(_)) => kept(&mut item, "changed since sync wrote it"),
            },
            Ok(k) => kept(
                &mut item,
                &format!("a {k:?} where its record says a {}", o.kind).to_lowercase(),
            ),
        }
        plan.items.push(item);
    }
    if let Some(Ok(r)) = root {
        plan.notes.push(format!("target {name} was at {}", display(r.path())));
        plan.roots.push(r);
    }
    plan.items.sort_by_key(|i| ledger::key(&i.path));
    // Every record of the target was looked at, and the stacks loaded with no problem (rule B).
    plan.complete = Some(Complete(()));
    Ok(plan)
}
