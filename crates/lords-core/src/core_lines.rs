//! Core lines (design: Entry metadata, `start` and `core`): the one or two sentences of an entry an agent starts
//! with, each traced by the entry's id to its whole text, which stays authoritative and is opened by id when a task
//! touches it (the Lords owner, 31 Oct, plan L8).

use crate::finding::Finding;
use crate::model::{Core, Entry, Kind, Origin, Start, Visibility};
use crate::stack::Stacks;

/// The longest core line, in UTF-8 bytes: the drafted lines' longest is 433 (31 Oct, after check r1).
pub const LINE_MAX_BYTES: usize = 450;

/// An entry that takes no class: memory notes are an index already, and a repo's own skill has no metadata of ours
/// to carry one. Intent takes only `detail`: it stays a title in the index (plan L8).
pub fn takes_none(e: &Entry) -> bool {
    e.meta.kind == Kind::Memory || matches!(e.meta.origin, Origin::InRepo { .. })
}

/// Whether an entry has a class it can be composed by: a line, or `detail`. Without one (no `start`, or `core` or
/// `role` with no line yet) its whole text is composed, as before core lines.
pub fn classified(e: &Entry) -> bool {
    !takes_none(e)
        && match e.meta.start {
            Some(Start::Detail) => true,
            Some(_) => e.meta.kind != Kind::Intent && e.meta.core.is_some(),
            None => false,
        }
}

/// An entry's lines, each with the roles it names (empty: every role the entry binds).
pub fn lines(e: &Entry) -> Vec<(&str, &[String])> {
    match &e.meta.core {
        None => Vec::new(),
        Some(Core::One(l)) => vec![(l.as_str(), &[][..])],
        Some(Core::Lines(ls)) => ls.iter().map(|l| (l.line.as_str(), l.roles.as_slice())).collect(),
    }
}

/// The roles an entry binds: its own, or every declared role when it names none.
pub fn bound(e: &Entry, declared: &[String]) -> Vec<String> {
    if e.meta.roles.is_empty() {
        declared.to_vec()
    } else {
        e.meta.roles.clone()
    }
}

/// The lines `role` starts with; none when the entry has no line for it (then its index entry, or its whole text
/// while it has no class).
pub fn lines_for<'e>(e: &'e Entry, role: &str) -> Vec<&'e str> {
    lines(e)
        .into_iter()
        .filter(|(_, roles)| roles.is_empty() || roles.iter().any(|r| r == role))
        .map(|(l, _)| l)
        .collect()
}

/// `check`'s core-line findings (design: Check): each line one line within the byte limit, its roles among the
/// entry's, each class holding what it says, and, where a stack sets `require_core`, a line or `detail` on each entry.
pub fn check(stacks: &Stacks, out: &mut Vec<Finding>) {
    let declared = crate::sync::declared_roles(stacks);
    for e in stacks.entries().filter(|e| e.active()) {
        let mut found = |code: &'static str, message: String| {
            out.push(Finding::error(code, message).at(&e.meta_file, None).entry(e.id()));
        };
        let lines = lines(e);
        if takes_none(e) {
            if e.meta.start.is_some() || !lines.is_empty() {
                found(
                    "core-class",
                    format!("a {} entry takes no `start` or `core`", e.meta.kind.name()),
                );
            }
            continue;
        }
        if e.meta.kind == Kind::Intent
            && (!lines.is_empty() || e.meta.start.is_some_and(|s| s != Start::Detail))
        {
            found("core-class", "intent takes only `start: detail`: its title in the index".into());
        }
        let bound = bound(e, &declared);
        for (i, (line, roles)) in lines.iter().enumerate() {
            let n = i + 1;
            if line.len() > LINE_MAX_BYTES {
                found(
                    "core-line-long",
                    format!(
                        "core line {n}: {} bytes, past the limit of {LINE_MAX_BYTES}",
                        line.len()
                    ),
                );
            }
            if line.contains(['\n', '\r']) {
                found("core-line-break", format!("core line {n} holds a line break"));
            }
            for r in roles.iter().filter(|r| !bound.contains(r)) {
                found(
                    "core-roles",
                    format!("core line {n} names `{r}`, a role the entry doesn't bind"),
                );
            }
        }
        let uncovered: Vec<&String> = bound.iter().filter(|r| lines_for(e, r).is_empty()).collect();
        let class = match (e.meta.start, lines.is_empty()) {
            (None, false) => Some("a core line needs its class, `start: core` or `start: role`".to_string()),
            (Some(Start::Detail), false) => Some("`start: detail` has no core line".to_string()),
            (Some(Start::Core), false) if !uncovered.is_empty() => Some(format!(
                "`start: core` reaches every role it binds, but no line is for {}; name them, or use `start: role`",
                names(&uncovered)
            )),
            (Some(Start::Role), false) if lines.iter().any(|(_, roles)| roles.is_empty()) => {
                Some("`start: role` names its roles on every line".to_string())
            }
            _ => None,
        };
        if let Some(why) = class {
            found("core-class", why);
        }
        let required = match e.stack {
            Visibility::Public => stacks.public.manifest.require_core,
            Visibility::Private => stacks.private.as_ref().is_some_and(|p| p.manifest.require_core),
        };
        let pending =
            e.meta.start.is_none() || (e.meta.start != Some(Start::Detail) && lines.is_empty());
        if required && pending {
            found(
                "core-missing",
                "its stack requires core lines: give it `start` and its `core` line, or `start: detail`"
                    .into(),
            );
        }
    }
}

/// `lords show <id>`: the entry's whole text, the body without frontmatter (a skill's `SKILL.md` without its own),
/// LF line endings and no outer blank lines. Refused, with why, for an id no entry holds, two entries hold (`check`
/// reports it), or a retired or ended entry holds: those bind nobody, and the successor is named.
pub fn show(stacks: &Stacks, id: &str) -> Result<String, String> {
    let found: Vec<&Entry> = stacks.entries().filter(|e| e.id() == id).collect();
    let e = match found.as_slice() {
        [] => {
            let unloaded: usize = stacks.all().map(|s| s.problems.len()).sum();
            let more = if unloaded > 0 {
                format!(" ({unloaded} problems loading the stacks; `lords check` names them)")
            } else {
                String::new()
            };
            return Err(format!("no entry `{id}` in the stacks{more}"));
        }
        [e] => *e,
        es => {
            let files: Vec<String> = es.iter().map(|e| e.meta_file.display().to_string()).collect();
            return Err(format!(
                "`{id}` is held by {} entries, so neither is shown: {}",
                es.len(),
                files.join(", ")
            ));
        }
    };
    if !e.active() {
        let why = e
            .meta
            .retired
            .as_ref()
            .map(|r| format!(" on {}: {}", r.date, r.reason))
            .unwrap_or_default();
        let next = e
            .meta
            .superseded_by
            .as_ref()
            .map(|s| format!("; its successor is `{s}`"))
            .unwrap_or_default();
        return Err(format!("`{id}` is retired{why}{next}"));
    }
    if let Some(why) = crate::temporary::ended(e, stacks, &crate::ops::today()) {
        return Err(format!("`{id}` has ended ({why}); sync retires it"));
    }
    let body = if e.meta.kind == Kind::Skill {
        crate::frontmatter::split(&e.body).body.to_string()
    } else {
        e.body.clone()
    };
    let body = body.replace("\r\n", "\n");
    Ok(format!("{}\n", body.trim_matches('\n')))
}

fn names(roles: &[&String]) -> String {
    roles.iter().map(|r| r.as_str()).collect::<Vec<_>>().join(", ")
}
