//! Temporary entries (owner, 25 Oct: "we don't poison the context with them forever as being temporary in the
//! past"). An entry ends on its `until` date, when the status it watches reaches the end state, or when its review
//! date passes unrenewed. An ended entry is never composed; `check` reports it and `sync --apply` retires it.

use crate::model::{Entry, Kind};
use crate::stack::Stacks;

/// Why a temporary entry has ended by `today` (YYYY-MM-DD), or `None` while it holds.
pub fn ended(e: &Entry, stacks: &Stacks, today: &str) -> Option<String> {
    let t = e.meta.temporary.as_ref()?;
    if let Some(until) = &t.until {
        if today > until.as_str() {
            return Some(format!("until {until} passed"));
        }
    }
    if let Some(w) = &t.when {
        let reached = stacks
            .find(&w.status)
            .filter(|s| s.meta.kind == Kind::Status)
            .and_then(|s| s.meta.state.as_deref())
            .is_some_and(|state| state == w.state);
        if reached {
            return Some(format!("{} is {}", w.status, w.state));
        }
    }
    if today > t.review_by.as_str() {
        return Some(format!("review date {} passed unrenewed", t.review_by));
    }
    None
}

/// The one short clause composed text carries for a live temporary entry.
pub fn label(e: &Entry, stacks: &Stacks) -> Option<String> {
    let t = e.meta.temporary.as_ref()?;
    let end = match (&t.until, &t.when) {
        (Some(until), _) => format!("until {until}"),
        (None, Some(w)) => {
            let what = stacks
                .find(&w.status)
                .map(|s| s.meta.title.clone())
                .unwrap_or_else(|| w.status.clone());
            format!("until {what}: {}", w.state)
        }
        (None, None) => format!("review by {}", t.review_by),
    };
    Some(format!("temporary, {end}"))
}

/// Every active entry that has ended by `today`, with why.
pub fn expired<'a>(stacks: &'a Stacks, today: &str) -> Vec<(&'a Entry, String)> {
    stacks
        .entries()
        .filter(|e| e.active())
        .filter_map(|e| ended(e, stacks, today).map(|why| (e, why)))
        .collect()
}
