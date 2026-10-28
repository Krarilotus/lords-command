//! Temporary entries (owner, 25 Oct: "we don't poison the context with them forever as being temporary in the
//! past"). What a temporary entry's fields mean is `Entry::lifecycle`'s alone (design v2 §4); this module reads it
//! against the loaded stacks. An expired or invalid entry is never composed; `check` reports it, and `sync --apply`
//! retires only an expired one.

use crate::model::{Entry, Lifecycle};
use crate::stack::Stacks;

/// Where `e` stands on `as_of` (YYYY-MM-DD), its `when` read against `stacks`.
pub fn lifecycle(e: &Entry, stacks: &Stacks, as_of: &str) -> Lifecycle {
    e.lifecycle(as_of, &|id| stacks.find(id))
}

/// The one short clause composed text carries for a live temporary entry.
pub fn label(e: &Entry, stacks: &Stacks, as_of: &str) -> Option<String> {
    let t = e.meta.temporary.as_ref()?;
    let end = match (&t.until, &t.when) {
        (Some(until), _) => Some(format!("until {until}")),
        (None, Some(w)) => {
            let what = stacks
                .find(&w.status)
                .map(|s| s.meta.title.clone())
                .unwrap_or_else(|| w.status.clone());
            Some(format!("until {what}: {}", w.state))
        }
        (None, None) => None,
    };
    let review = match lifecycle(e, stacks, as_of) {
        Lifecycle::ReviewDue { since } => Some(format!("review due since {since}")),
        _ if end.is_none() => t.review_by.as_ref().map(|r| format!("review by {r}")),
        _ => None,
    };
    let parts: Vec<String> = end.into_iter().chain(review).collect();
    Some(format!("temporary, {}", parts.join("; ")))
}

/// Every active entry that has expired by `as_of`, with why.
pub fn expired<'a>(stacks: &'a Stacks, as_of: &str) -> Vec<(&'a Entry, String)> {
    stacks
        .entries()
        .filter_map(|e| match lifecycle(e, stacks, as_of) {
            Lifecycle::Expired { why } => Some((e, why)),
            _ => None,
        })
        .collect()
}
