//! The stores' commit check of the starts one hook gives (gap 11, design note `2026-11-08-lords-gap11-headroom.md`
//! §0b–§0e; `check --stack`, a store's pre-commit and pre-merge-commit). A commit to a store is refused when it makes a
//! place's start grow while past what one hook gives (`sync::CHILD_START_CHARS`), or takes one past it (I1). Both
//! sides are counted from git's objects, never a checkout: the commit as its index holds it, its store's `HEAD` as the
//! baseline, and the other store's committed tree, read only once its status is clean and no `index.lock` is there,
//! and used for both (I2). Every hard line stays where it is: a start past the limit is refused, never shortened.

use crate::capture::{self, Captured, Env, Read};
use crate::stack::{Stacks, Store};
use crate::sync::{self, CHILD_START_CHARS};
use std::collections::BTreeMap;
use std::path::Path;
use std::time::Duration;

/// What the check reads, and where.
pub struct Inputs<'a> {
    /// The checkout the commit is made in (its hook's), and its stack dir there, relative to it: `stack`, or `.`
    /// where the store's repo is its stack.
    pub checkout: &'a Path,
    pub stack: &'a str,
    /// The store the commit is to.
    pub store: Store,
    /// Each store's stack dir as configured: where composition says its files are, and where the other store is
    /// read.
    pub public: &'a Path,
    pub private: &'a Path,
    /// The tool home a start names its opener in.
    pub home: &'a Path,
    /// The date both sides are composed on.
    pub today: &'a str,
    /// Each git read's limit (`[delivery] store_git_secs`).
    pub limit: Duration,
}

/// One start's size on each side of the commit.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Count {
    /// `repo <name>` or `workspace <name>`.
    pub place: String,
    /// `child.md` or `start-<role>.md`.
    pub file: String,
    /// Characters as the hook gives it, with the commit.
    pub new: usize,
    /// Without it, as its store's `HEAD` has it with the same other store: 0 for a start that isn't there yet, or one
    /// a `HEAD` that doesn't load can't tell. `None` where the baseline wasn't read: every start fits, or the store
    /// has no `HEAD` yet; either way it counts as 0.
    pub old: Option<usize>,
}

impl Count {
    /// I1's refusal: past the limit, and grown.
    pub fn refused(&self) -> bool {
        refuses(self.new, self.old.unwrap_or(0))
    }
}

/// The predicate (design check r3, §0b.3 step 6): a start is refused when it is past the limit and longer than before.
/// So one that fits may reach the limit, and one past it may stay as long or get shorter, while nothing crosses it.
pub fn refuses(new: usize, old: usize) -> bool {
    new > CHILD_START_CHARS && new > old
}

/// What the check found.
#[derive(Debug, PartialEq, Eq)]
pub enum Outcome {
    /// The commit leaves its store's stack as `HEAD` has it: no start can change.
    Unchanged,
    /// No place is declared: there is no start.
    NoPlaces,
    /// Every start counted, none refused.
    Admitted(Vec<Count>),
    /// The starts counted, and some refused (`Count::refused`).
    Refused(Vec<Count>),
    /// The other store can't be read as committed: it has work not committed, or a git operation under way.
    Unclean(String),
}

/// Where the check is, for a test to act there (`admit_between`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AdmitStep {
    /// Just before a git read of the other store.
    Read(Read),
    /// Decided, before the check returns.
    Decided,
}

/// The check (module doc).
pub fn admit(i: &Inputs) -> Result<Outcome, String> {
    admit_between(i, &mut |_| {})
}

/// `admit`, calling `between` just before each of its git reads of the other store, and once decided.
#[doc(hidden)]
pub fn admit_between(i: &Inputs, between: &mut dyn FnMut(AdmitStep)) -> Result<Outcome, String> {
    let head = head(i)?;
    if head.is_some() && unchanged(i)? {
        return Ok(Outcome::Unchanged);
    }
    let (own, other) = match i.store {
        Store::Public => (i.public, i.private),
        Store::Private => (i.private, i.public),
    };
    // The other store: no git operation under way, then a clean status, then its tree, in that order (I2).
    let mut read = |r: Read| between(AdmitStep::Read(r));
    if let Some(why) = lock_at(other, i.limit, &mut read)? {
        return Ok(Outcome::Unclean(why));
    }
    read(Read::Status);
    let listed = capture::status(other, i.limit)?;
    if !listed.is_empty() {
        let first: Vec<&str> = listed.lines().take(5).collect();
        return Ok(Outcome::Unclean(format!(
            "{} has work not committed in its stack ({}{}), so its committed tree may not be what it delivers next: commit it, or set it aside, then commit here again",
            other.display(),
            first.join("; "),
            if listed.lines().count() > 5 { "; …" } else { "" }
        )));
    }
    read(Read::Head);
    let tree = capture::tree(other, i.limit)?;
    let pair = Pair {
        other: capture::capture_tree(Env::Isolated, other, other, &tree.tree, i.limit, &mut read)?,
        store: i.store,
    };
    let candidate = pair.stacks(capture::capture_index(i.checkout, i.stack, own, i.limit)?)?;
    let new = counts(&candidate, i);
    if new.is_empty() {
        between(AdmitStep::Decided);
        return Ok(Outcome::NoPlaces);
    }
    // The baseline, only where a start is past the limit: its store's HEAD with the same other store.
    let old = match (head, new.values().any(|n| *n > CHILD_START_CHARS)) {
        (Some(_), true) => {
            let at = match i.stack {
                "." => "HEAD^{tree}".to_string(),
                s => format!("HEAD:{}", s.trim_end_matches('/')),
            };
            let h =
                capture::capture_tree(Env::Inherited, i.checkout, own, &at, i.limit, &mut |_| {})?;
            Some(pair.stacks(h).map(|s| counts(&s, i)).unwrap_or_default())
        }
        _ => None,
    };
    let counted: Vec<Count> = new
        .into_iter()
        .map(|((place, file), n)| {
            let was =
                old.as_ref().map(|o| o.get(&(place.clone(), file.clone())).copied().unwrap_or(0));
            Count { place, file, new: n, old: was }
        })
        .collect();
    between(AdmitStep::Decided);
    Ok(match counted.iter().any(Count::refused) {
        true => Outcome::Refused(counted),
        false => Outcome::Admitted(counted),
    })
}

/// The other store's committed tree, held once for both sides of the commit (I2: the candidate and the baseline can't
/// use two different trees of it).
struct Pair {
    other: Captured,
    store: Store,
}

impl Pair {
    /// Both stores, with `own` as the committing store's side. Repos' own skills take no part in these starts.
    fn stacks(&self, own: Captured) -> Result<Stacks, String> {
        let other = self.other.clone();
        match self.store {
            Store::Public => Stacks::load_captured(own, Some(other), false),
            Store::Private => Stacks::load_captured(other, Some(own), false),
        }
    }
}

/// Each declared place's hook starts, in characters, keyed by (place, file).
fn counts(stacks: &Stacks, i: &Inputs) -> BTreeMap<(String, String), usize> {
    let mut out = BTreeMap::new();
    for (place, layers) in sync::declared_places(stacks) {
        for s in sync::hook_starts(stacks, layers, i.home, i.today) {
            out.insert((place.clone(), s.file.clone()), s.chars());
        }
    }
    out
}

/// The commit's store's `HEAD`, or none before its first commit.
fn head(i: &Inputs) -> Result<Option<String>, String> {
    let args = ["rev-parse", "--verify", "-q", "HEAD"];
    let out = crate::process::git_inherited_bounded(i.checkout, &args, None, i.limit)?;
    match out.status.code() {
        Some(0) => Ok(Some(String::from_utf8_lossy(&out.stdout).trim().to_string())),
        Some(1) => Ok(None),
        _ => Err(format!(
            "{}: git {} failed: {}",
            i.checkout.display(),
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        )),
    }
}

/// Whether the commit leaves the stack dir as `HEAD` has it: then, whatever the other store holds, each start is what
/// it was (§0b.1). Any other answer than yes or no is an error.
fn unchanged(i: &Inputs) -> Result<bool, String> {
    let args = ["diff-index", "--cached", "--quiet", "HEAD", "--", i.stack];
    let out = crate::process::git_inherited_bounded(i.checkout, &args, None, i.limit)?;
    match out.status.code() {
        Some(0) => Ok(true),
        Some(1) => Ok(false),
        _ => Err(format!(
            "{}: git {} failed: {}",
            i.checkout.display(),
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        )),
    }
}

/// Why the store at `dir` may be in the middle of a git operation: its `index.lock` is there (`commit -a` and `commit
/// <paths>` hold it until their commit is in, while the checkout can look clean: §0b.2, D2). Never removed here.
fn lock_at(
    dir: &Path,
    limit: Duration,
    read: &mut dyn FnMut(Read),
) -> Result<Option<String>, String> {
    read(Read::Lock);
    let args = ["rev-parse", "--path-format=absolute", "--git-path", "index.lock"];
    let at = capture::git(dir, &args, limit)?;
    let lock = dir.join(at);
    match crate::fsguard::kind_here(&lock) {
        Ok(crate::fsguard::Kind::Missing) => Ok(None),
        Ok(_) => Ok(Some(format!(
            "{}: its index.lock exists ({}), so a git operation there may be under way, and this check can't tell whether it is publishing a commit. If no git process is running in {}, remove that one file, then commit here again",
            dir.display(),
            lock.display(),
            dir.display()
        ))),
        Err(e) => Err(format!("{}: whether its index.lock exists can't be told: {e}", dir.display())),
    }
}
