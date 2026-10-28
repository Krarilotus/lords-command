//! Entry metadata as `schema/entry.schema.json` defines it, and where an entry sits.

use serde::{Deserialize, Serialize};
use std::fmt;
use std::path::PathBuf;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Intent,
    Role,
    Rule,
    Fact,
    Status,
    Template,
    Skill,
    Memory,
}

impl Kind {
    pub const ALL: [Kind; 8] = [
        Kind::Intent,
        Kind::Role,
        Kind::Rule,
        Kind::Fact,
        Kind::Status,
        Kind::Template,
        Kind::Skill,
        Kind::Memory,
    ];

    /// The directory this kind lives in, inside a layer.
    pub fn dir(self) -> &'static str {
        match self {
            Kind::Intent => "intent",
            Kind::Role => "roles",
            Kind::Rule => "rules",
            Kind::Fact => "facts",
            Kind::Status => "status",
            Kind::Template => "templates",
            Kind::Skill => "skills",
            Kind::Memory => "memory",
        }
    }

    pub fn from_dir(dir: &str) -> Option<Kind> {
        Kind::ALL.into_iter().find(|k| k.dir() == dir)
    }

    pub fn name(self) -> &'static str {
        match self {
            Kind::Intent => "intent",
            Kind::Role => "role",
            Kind::Rule => "rule",
            Kind::Fact => "fact",
            Kind::Status => "status",
            Kind::Template => "template",
            Kind::Skill => "skill",
            Kind::Memory => "memory",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Audience {
    Opus,
    Sol,
    Astra,
    All,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Visibility {
    Public,
    Private,
}

impl fmt::Display for Visibility {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Visibility::Public => "public",
            Visibility::Private => "private",
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    Active,
    Retired,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Retired {
    pub date: String,
    pub reason: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Basis {
    pub quote: String,
    pub date: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub by: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Source {
    pub repo: String,
    pub commit: String,
    pub path: String,
    pub licence: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub copyright: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub changes: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sha256: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum Origin {
    Own,
    Adapted(Source),
    Inherited(Source),
    Imported {
        store: String,
        file: String,
    },
    /// A skill tracked in a repo, indexed where it is (never linked, copied or moved): the repo, its path in the
    /// checkout, and the hash of its files now.
    InRepo {
        repo: String,
        path: String,
        sha256: String,
    },
}

impl Origin {
    pub fn label(&self) -> &'static str {
        match self {
            Origin::Own => "own",
            Origin::Adapted(_) => "adapted",
            Origin::Inherited(_) => "inherited",
            Origin::Imported { .. } => "imported",
            Origin::InRepo { .. } => "in repo",
        }
    }

    pub fn source(&self) -> Option<&Source> {
        match self {
            Origin::Adapted(s) | Origin::Inherited(s) => Some(s),
            _ => None,
        }
    }
}

/// The status an end condition watches: when entry `status` has `state`, the temporary entry has ended.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EndsWhen {
    pub status: String,
    pub state: String,
}

/// A temporary entry's end (owner, 25 Oct: temporary rules must not "poison the context … forever"): a reason and
/// at least one date. `until` ends it; `when` ends it once a status reaches a state; `review_by` only makes it due
/// for review (design v2 §4). `Entry::lifecycle` is the one place that reads these fields' meaning.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Temporary {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub until: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub when: Option<EndsWhen>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub review_by: Option<String>,
    pub reason: String,
}

/// Where an entry stands on a day (design v2 §4). Everything that composes, checks, retires or audits entries reads
/// this, never the temporary fields' dates.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Lifecycle {
    Active,
    /// Past its review date (`since` is that date) and not ended: still composed, labelled.
    ReviewDue {
        since: String,
    },
    /// Its `until` day has come or its `when` status has its state: never composed; sync retires it.
    Expired {
        why: String,
    },
    Retired,
    /// An end that can't be evaluated (a blank reason, no date, an impossible date, a `when` watching no status):
    /// never composed, and a check error.
    Invalid {
        why: String,
    },
}

/// A real calendar date written YYYY-MM-DD (`2026-02-30` is not).
pub fn valid_date(s: &str) -> bool {
    parse_date(s).is_some()
}

fn parse_date(s: &str) -> Option<(u32, u32, u32)> {
    let b = s.as_bytes();
    if b.len() != 10 || b[4] != b'-' || b[7] != b'-' {
        return None;
    }
    let num = |r: std::ops::Range<usize>| {
        b[r.clone()].iter().all(u8::is_ascii_digit).then(|| s[r].parse::<u32>().ok()).flatten()
    };
    let (y, m, d) = (num(0..4)?, num(5..7)?, num(8..10)?);
    ((1..=12).contains(&m) && (1..=days_in_month(y, m)).contains(&d)).then_some((y, m, d))
}

fn days_in_month(y: u32, m: u32) -> u32 {
    match m {
        2 if y.is_multiple_of(4) && (!y.is_multiple_of(100) || y.is_multiple_of(400)) => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    }
}

/// The day after a valid date.
fn next_day(s: &str) -> Option<String> {
    let (y, m, d) = parse_date(s)?;
    let (y, m, d) = if d < days_in_month(y, m) {
        (y, m, d + 1)
    } else if m < 12 {
        (y, m + 1, 1)
    } else {
        (y + 1, 1, 1)
    };
    Some(format!("{y:04}-{m:02}-{d:02}"))
}

impl Temporary {
    /// What makes this block unevaluable on its own, without looking at other entries.
    fn invalid(&self) -> Option<String> {
        if self.reason.trim().is_empty() {
            return Some("its reason is blank".into());
        }
        if self.until.is_none() && self.review_by.is_none() {
            return Some("it has neither an until nor a review_by date".into());
        }
        for (field, date) in [("until", &self.until), ("review_by", &self.review_by)] {
            if let Some(d) = date.as_deref().filter(|d| !valid_date(d)) {
                return Some(format!("its {field} `{d}` is no calendar date"));
            }
        }
        None
    }
}

/// A case behind an entry: what happened, when, and where it is recorded. Kept beside the rule, never in the
/// composed text (the rule stays a short principle).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Evidence {
    pub summary: String,
    pub date: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub links: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Meta {
    pub id: String,
    pub kind: Kind,
    pub title: String,
    pub audience: Vec<Audience>,
    /// The roles it is for (stack.toml `roles`); empty: every role.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub roles: Vec<String>,
    pub scope: String,
    pub status: Status,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub superseded_by: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retired: Option<Retired>,
    pub visibility: Visibility,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub basis: Vec<Basis>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub evidence: Vec<Evidence>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub temporary: Option<Temporary>,
    /// A status entry's current state, which a temporary entry's `when` can watch.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state: Option<String>,
    pub origin: Origin,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub topics: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub overrides: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub extends: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub order: Option<i64>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub links: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<serde_json::Value>,
}

/// How narrow a scope is. Narrower layers may change broader ones (design: Layers).
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Scope {
    Global,
    Project(String),
    Repo(String),
    /// A working directory that is no git repo (e.g. a harness dir), declared in stack.toml `workspaces`.
    Workspace(String),
    Path {
        repo: String,
        sub: String,
    },
}

impl Scope {
    pub fn parse(s: &str) -> Option<Scope> {
        if s == "global" {
            return Some(Scope::Global);
        }
        if let Some(p) = s.strip_prefix("project:") {
            return (!p.is_empty()).then(|| Scope::Project(p.to_string()));
        }
        if let Some(r) = s.strip_prefix("repo:") {
            return (!r.is_empty()).then(|| Scope::Repo(r.to_string()));
        }
        if let Some(w) = s.strip_prefix("workspace:") {
            return (!w.is_empty()).then(|| Scope::Workspace(w.to_string()));
        }
        let rest = s.strip_prefix("path:")?;
        let (repo, sub) = rest.split_once(':')?;
        (!repo.is_empty() && !sub.is_empty())
            .then(|| Scope::Path { repo: repo.to_string(), sub: sub.trim_matches('/').to_string() })
    }

    /// Rank 0 public global … 4 path. Private global sits above public global.
    pub fn rank(&self, visibility: Visibility) -> u8 {
        match self {
            Scope::Global if visibility == Visibility::Public => 0,
            Scope::Global => 1,
            Scope::Project(_) => 2,
            Scope::Repo(_) | Scope::Workspace(_) => 3,
            Scope::Path { .. } => 4,
        }
    }

    pub fn repo(&self) -> Option<&str> {
        match self {
            Scope::Repo(r) | Scope::Path { repo: r, .. } => Some(r),
            _ => None,
        }
    }
}

impl fmt::Display for Scope {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Scope::Global => f.write_str("global"),
            Scope::Project(p) => write!(f, "project:{p}"),
            Scope::Repo(r) => write!(f, "repo:{r}"),
            Scope::Workspace(w) => write!(f, "workspace:{w}"),
            Scope::Path { repo, sub } => write!(f, "path:{repo}:{sub}"),
        }
    }
}

/// One loaded entry.
#[derive(Clone, Debug)]
pub struct Entry {
    pub meta: Meta,
    pub scope: Scope,
    /// The stack it was loaded from.
    pub stack: Visibility,
    /// The file holding the metadata (Markdown entry, memory file, or a skill's `lords.yaml`).
    pub meta_file: PathBuf,
    /// The file holding the text (the Markdown file itself, or a skill's `SKILL.md`).
    pub body_file: PathBuf,
    pub body: String,
    /// 1-based line of `body_file` where `body` starts.
    pub body_line: usize,
    /// For a skill: every file in its directory, sorted. Otherwise the entry's one file.
    pub files: Vec<PathBuf>,
}

impl Entry {
    pub fn id(&self) -> &str {
        &self.meta.id
    }

    pub fn active(&self) -> bool {
        self.meta.status == Status::Active
    }

    pub fn rank(&self) -> u8 {
        self.scope.rank(self.stack)
    }

    /// Where this entry stands on `as_of` (UTC, YYYY-MM-DD, as `ops::today()`); `status` finds the entry a `when`
    /// watches. `until` is exclusive (`as_of >= until` has ended it); a passed `review_by` only makes it due.
    pub fn lifecycle<'s>(
        &self,
        as_of: &str,
        status: &dyn Fn(&str) -> Option<&'s Entry>,
    ) -> Lifecycle {
        if !self.active() {
            return Lifecycle::Retired;
        }
        let Some(t) = &self.meta.temporary else { return Lifecycle::Active };
        if let Some(why) = t.invalid() {
            return Lifecycle::Invalid { why };
        }
        let mut reached = false;
        if let Some(w) = &t.when {
            let Some(s) = status(&w.status).filter(|s| s.meta.kind == Kind::Status) else {
                return Lifecycle::Invalid {
                    why: format!("its when watches `{}`, which is no status entry", w.status),
                };
            };
            let Some(state) = s.meta.state.as_deref() else {
                return Lifecycle::Invalid {
                    why: format!("its when watches `{}`, which has no state", w.status),
                };
            };
            reached = state == w.state;
        }
        if let Some(until) = t.until.as_deref().filter(|u| as_of >= *u) {
            return Lifecycle::Expired { why: format!("until {until} reached") };
        }
        if let (true, Some(w)) = (reached, &t.when) {
            return Lifecycle::Expired { why: format!("{} is {}", w.status, w.state) };
        }
        match t.review_by.as_deref().filter(|r| as_of > *r) {
            Some(r) => Lifecycle::ReviewDue { since: r.to_string() },
            None => Lifecycle::Active,
        }
    }

    /// The next day after `as_of` on which this entry's lifecycle changes by date alone, so a context built on
    /// `as_of` is valid before it. `None`: no date changes it any more (a status a `when` watches still can).
    pub fn next_boundary(&self, as_of: &str) -> Option<String> {
        let t = self.meta.temporary.as_ref().filter(|_| self.active())?;
        if t.invalid().is_some() || t.until.as_deref().is_some_and(|u| as_of >= u) {
            return None;
        }
        let due = t.review_by.as_deref().filter(|r| as_of <= *r).and_then(next_day);
        [t.until.clone(), due].into_iter().flatten().min()
    }
}
