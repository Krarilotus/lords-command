//! The views any UI shows, as plain serialisable data (owner, 25 Oct: one local web UI; the eframe window is
//! interim). One function per view; a UI renders these and holds no logic of its own.

use crate::compose::Composed;
use crate::finding::{Finding, Severity};
use crate::model::{Entry, Origin};
use crate::stack::Stacks;
use crate::sync::{Action, Plan};
use serde::Serialize;
use std::path::PathBuf;

/// How many findings of each severity.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct Counts {
    pub errors: usize,
    pub warnings: usize,
    pub candidates: usize,
    pub info: usize,
}

fn counts<'a>(findings: impl Iterator<Item = &'a Finding>) -> Counts {
    let mut c = Counts::default();
    for f in findings {
        match f.severity {
            Severity::Error => c.errors += 1,
            Severity::Warning => c.warnings += 1,
            Severity::Candidate => c.candidates += 1,
            Severity::Info => c.info += 1,
        }
    }
    c
}

// ---------------------------------------------------------------- entries

/// One entry, as a list line and its detail need it.
#[derive(Clone, Debug, Serialize)]
pub struct EntryView {
    pub id: String,
    pub kind: &'static str,
    pub title: String,
    pub scope: String,
    pub stack: String,
    pub active: bool,
    pub audience: Vec<String>,
    pub roles: Vec<String>,
    pub topics: Vec<String>,
    /// own, adapted, inherited, imported, in repo.
    pub origin: &'static str,
    /// Where an adapted or inherited entry came from (a URL when the source is on GitHub).
    pub source: Option<String>,
    pub file: PathBuf,
    /// The worst finding about it, if any (not counting info).
    pub worst: Option<Severity>,
    pub findings: Counts,
}

fn source_of(o: &Origin) -> Option<String> {
    match o {
        Origin::Adapted(s) | Origin::Inherited(s) => {
            let repo = if s.repo.contains("://") {
                s.repo.clone()
            } else {
                format!("https://github.com/{}", s.repo)
            };
            Some(if repo.contains("github.com") {
                format!("{repo}/blob/{}/{}", s.commit, s.path)
            } else {
                repo
            })
        }
        Origin::InRepo { repo, path, .. } => Some(format!("{repo}:{path}")),
        _ => None,
    }
}

fn names<T: Serialize>(items: &[T]) -> Vec<String> {
    items
        .iter()
        .filter_map(|a| serde_json::to_value(a).ok().and_then(|v| v.as_str().map(str::to_string)))
        .collect()
}

pub fn entry_view(e: &Entry, findings: &[Finding]) -> EntryView {
    let mine: Vec<&Finding> =
        findings.iter().filter(|f| f.entry.as_deref() == Some(e.id())).collect();
    EntryView {
        id: e.id().to_string(),
        kind: e.meta.kind.name(),
        title: e.meta.title.clone(),
        scope: e.scope.to_string(),
        stack: e.stack.to_string(),
        active: e.active(),
        audience: names(&e.meta.audience),
        roles: e.meta.roles.clone(),
        topics: e.meta.topics.clone(),
        origin: e.meta.origin.label(),
        source: source_of(&e.meta.origin),
        file: e.meta_file.clone(),
        worst: mine.iter().map(|f| f.severity).filter(|s| *s != Severity::Info).min(),
        findings: counts(mine.into_iter()),
    }
}

/// Every entry of both stacks, ordered by layer, kind and id (the list a UI shows).
pub fn entries(stacks: &Stacks, findings: &[Finding]) -> Vec<EntryView> {
    let mut all: Vec<&Entry> = stacks.entries().collect();
    all.sort_by(|a, b| (a.rank(), a.meta.kind, a.id()).cmp(&(b.rank(), b.meta.kind, b.id())));
    all.into_iter().map(|e| entry_view(e, findings)).collect()
}

// ---------------------------------------------------------------- findings

#[derive(Clone, Debug, Serialize)]
pub struct FindingsView {
    pub counts: Counts,
    /// Worst first, then by file and line.
    pub items: Vec<Finding>,
}

pub fn findings(found: &[Finding]) -> FindingsView {
    let mut items = found.to_vec();
    items.sort_by(|a, b| {
        a.severity.cmp(&b.severity).then_with(|| a.file.cmp(&b.file)).then(a.line.cmp(&b.line))
    });
    FindingsView { counts: counts(found.iter()), items }
}

// ---------------------------------------------------------------- sync plan

#[derive(Clone, Debug, Serialize)]
pub struct PlanItemView {
    pub target: String,
    pub path: PathBuf,
    pub action: Action,
    pub changes: bool,
    pub copy_from: Option<PathBuf>,
    pub diff: String,
    pub notes: Vec<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct PlanView {
    pub items: Vec<PlanItemView>,
    pub notes: Vec<String>,
    pub to_change: usize,
    pub in_sync: usize,
    pub report_only: usize,
    pub conflicts: usize,
}

pub fn plan(p: &Plan) -> PlanView {
    let count = |f: &dyn Fn(&Action) -> bool| p.items.iter().filter(|i| f(&i.action)).count();
    PlanView {
        items: p
            .items
            .iter()
            .map(|i| PlanItemView {
                target: i.target.clone(),
                path: i.path.clone(),
                action: i.action.clone(),
                changes: i.changes(),
                copy_from: i.copy_from.clone(),
                diff: i.diff.clone(),
                notes: i.notes.clone(),
            })
            .collect(),
        notes: p.notes.clone(),
        to_change: count(&|a| !matches!(a, Action::InSync | Action::Report | Action::Conflict)),
        in_sync: count(&|a| *a == Action::InSync),
        report_only: count(&|a| *a == Action::Report),
        conflicts: count(&|a| *a == Action::Conflict),
    }
}

// ---------------------------------------------------------------- composed text with its sources

/// Where one composed line comes from.
#[derive(Clone, Debug, Serialize)]
pub struct SourceView {
    pub id: String,
    pub kind: &'static str,
    pub scope: String,
    pub stack: String,
    pub file: PathBuf,
    pub file_line: usize,
}

#[derive(Clone, Debug, Serialize)]
pub struct LineView {
    /// 1-based.
    pub n: usize,
    pub text: String,
    /// `None` for headings, markers and blank lines: they belong to no entry.
    pub source: Option<SourceView>,
}

#[derive(Clone, Debug, Serialize)]
pub struct ComposedView {
    pub lines: Vec<LineView>,
    pub overridden: Vec<String>,
}

pub fn composed(stacks: &Stacks, c: &Composed) -> ComposedView {
    let lines = c
        .text
        .lines()
        .enumerate()
        .map(|(i, text)| {
            let source =
                c.sources.iter().find(|s| s.line == i + 1).filter(|_| !text.trim().is_empty()).map(
                    |s| {
                        let e = stacks.find(&s.id);
                        SourceView {
                            id: s.id.clone(),
                            kind: e.map(|e| e.meta.kind.name()).unwrap_or(""),
                            scope: e.map(|e| e.scope.to_string()).unwrap_or_default(),
                            stack: s.stack.to_string(),
                            file: s.file.clone(),
                            file_line: s.file_line,
                        }
                    },
                );
            LineView { n: i + 1, text: text.to_string(), source }
        })
        .collect();
    ComposedView { lines, overridden: c.overridden.clone() }
}
