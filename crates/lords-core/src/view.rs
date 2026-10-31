//! The views any UI shows, as plain serialisable data. The window renders these, whether it reads them here or
//! through `lords panel serve` (panel.rs), so they are the panel's wire format too: each one reads back from its JSON
//! as it was. One function per view; a UI holds no logic of its own.

use crate::compose::Composed;
use crate::finding::{Finding, Severity};
use crate::model::{Audience, Basis, Entry, Evidence, Kind, Origin, Retired, Visibility};
use crate::stack::Stacks;
use crate::sync::{Action, Plan};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::PathBuf;

/// How many findings of each severity.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Counts {
    pub errors: usize,
    pub warnings: usize,
    pub candidates: usize,
    pub info: usize,
}

fn counts<'a>(severities: impl Iterator<Item = &'a Severity>) -> Counts {
    let mut c = Counts::default();
    for s in severities {
        match s {
            Severity::Error => c.errors += 1,
            Severity::Warning => c.warnings += 1,
            Severity::Candidate => c.candidates += 1,
            Severity::Info => c.info += 1,
        }
    }
    c
}

// ---------------------------------------------------------------- entries

/// One entry, as its list line and its detail need it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct EntryView {
    pub id: String,
    pub kind: Kind,
    pub title: String,
    pub scope: String,
    pub stack: Visibility,
    pub active: bool,
    pub audience: Vec<Audience>,
    pub roles: Vec<String>,
    pub topics: Vec<String>,
    pub origin: Origin,
    /// Where an adapted or inherited entry came from (a URL when the source is on GitHub).
    pub source: Option<String>,
    pub retired: Option<Retired>,
    pub overrides: Vec<String>,
    pub extends: Vec<String>,
    pub superseded_by: Option<String>,
    pub links: Vec<String>,
    pub evidence: Vec<Evidence>,
    pub basis: Vec<Basis>,
    /// The metadata file.
    pub file: PathBuf,
    pub body_file: PathBuf,
    pub body: String,
    /// Whether its text may change here: an inherited entry stays verbatim, a memory is the agent's own.
    pub editable: bool,
    /// The version of its files as they were read (`Entry::version`): an edit or retirement made from this view
    /// carries it.
    pub version: String,
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

pub fn entry_view(e: &Entry, findings: &[Finding]) -> EntryView {
    let mine: Vec<Severity> = findings
        .iter()
        .filter(|f| f.entry.as_deref() == Some(e.id()))
        .map(|f| f.severity)
        .collect();
    EntryView {
        id: e.id().to_string(),
        kind: e.meta.kind,
        title: e.meta.title.clone(),
        scope: e.scope.to_string(),
        stack: e.stack,
        active: e.active(),
        audience: e.meta.audience.clone(),
        roles: e.meta.roles.clone(),
        topics: e.meta.topics.clone(),
        origin: e.meta.origin.clone(),
        source: source_of(&e.meta.origin),
        retired: e.meta.retired.clone(),
        overrides: e.meta.overrides.clone(),
        extends: e.meta.extends.clone(),
        superseded_by: e.meta.superseded_by.clone(),
        links: e.meta.links.clone(),
        evidence: e.meta.evidence.clone(),
        basis: e.meta.basis.clone(),
        file: e.meta_file.clone(),
        body_file: e.body_file.clone(),
        body: e.body.clone(),
        editable: !matches!(e.meta.origin, Origin::Inherited(_)) && e.meta.kind != Kind::Memory,
        version: e.version.clone(),
        worst: mine.iter().copied().filter(|s| *s != Severity::Info).min(),
        findings: counts(mine.iter()),
    }
}

/// Every entry of both stacks, ordered by layer, kind and id (the list a UI shows).
pub fn entries(stacks: &Stacks, findings: &[Finding]) -> Vec<EntryView> {
    let mut all: Vec<&Entry> = stacks.entries().collect();
    all.sort_by(|a, b| (a.rank(), a.meta.kind, a.id()).cmp(&(b.rank(), b.meta.kind, b.id())));
    all.into_iter().map(|e| entry_view(e, findings)).collect()
}

/// What a stack declares, by stack: an edit may set only these.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Declared {
    pub public: Vec<String>,
    pub private: Vec<String>,
}

impl Declared {
    /// What an entry of `stack` may name: the public stack's, and the private stack's for a private entry.
    pub fn for_stack(&self, stack: Visibility) -> Vec<String> {
        let mut all = self.public.clone();
        if stack == Visibility::Private {
            all.extend(self.private.iter().cloned());
        }
        all
    }
}

/// Both stacks as the window opens them: the entries (without findings, which `check` brings later), what each
/// stack declares, the places composed text and sync plans can be shown for, and the adapters.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct StacksView {
    pub entries: Vec<EntryView>,
    pub topics: Declared,
    pub roles: Declared,
    /// The repos and workspaces the private stack declares.
    pub places: Vec<String>,
    pub adapters: Vec<String>,
}

pub fn stacks(stacks: &Stacks) -> StacksView {
    let declared = |of: &dyn Fn(&crate::stack::Stack) -> Vec<String>| Declared {
        public: of(&stacks.public),
        private: stacks.private.as_ref().map(of).unwrap_or_default(),
    };
    StacksView {
        entries: entries(stacks, &[]),
        topics: declared(&|s| s.manifest.topics.clone()),
        roles: declared(&|s| s.manifest.roles.clone()),
        places: stacks
            .private
            .iter()
            .flat_map(|p| p.manifest.repos.keys().chain(p.manifest.workspaces.keys()).cloned())
            .collect(),
        adapters: crate::adapters::ids().into_iter().map(str::to_string).collect(),
    }
}

// ---------------------------------------------------------------- findings

/// One finding, with its one-line text (`Finding`'s own).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct FindingView {
    pub severity: Severity,
    pub code: String,
    pub message: String,
    pub file: Option<PathBuf>,
    pub line: Option<usize>,
    pub entry: Option<String>,
    pub related: Vec<String>,
    pub text: String,
}

impl From<&Finding> for FindingView {
    fn from(f: &Finding) -> FindingView {
        FindingView {
            severity: f.severity,
            code: f.code.to_string(),
            message: f.message.clone(),
            file: f.file.clone(),
            line: f.line,
            entry: f.entry.clone(),
            related: f.related.clone(),
            text: f.to_string(),
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct FindingsView {
    pub counts: Counts,
    /// Worst first, then by file and line.
    pub items: Vec<FindingView>,
}

impl FindingsView {
    /// Each entry's worst finding, info not counted.
    pub fn worst(&self) -> BTreeMap<String, Severity> {
        let mut worst: BTreeMap<String, Severity> = BTreeMap::new();
        for f in self.items.iter().filter(|f| f.severity != Severity::Info) {
            if let Some(id) = &f.entry {
                let w = worst.entry(id.clone()).or_insert(f.severity);
                *w = (*w).min(f.severity);
            }
        }
        worst
    }
}

pub fn findings(found: &[Finding]) -> FindingsView {
    let mut items: Vec<&Finding> = found.iter().collect();
    items.sort_by(|a, b| {
        a.severity.cmp(&b.severity).then_with(|| a.file.cmp(&b.file)).then(a.line.cmp(&b.line))
    });
    FindingsView {
        counts: counts(found.iter().map(|f| &f.severity)),
        items: items.into_iter().map(FindingView::from).collect(),
    }
}

// ---------------------------------------------------------------- sync plan

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PlanItemView {
    pub target: String,
    pub path: PathBuf,
    pub action: Action,
    pub changes: bool,
    pub class: crate::sync::Class,
    pub diff: String,
    pub notes: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
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
                class: i.class,
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
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SourceView {
    pub id: String,
    /// `None` when no loaded entry has the id.
    pub kind: Option<Kind>,
    pub scope: String,
    pub stack: Visibility,
    pub file: PathBuf,
    pub file_line: usize,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct LineView {
    /// 1-based.
    pub n: usize,
    pub text: String,
    /// `None` for headings, markers and blank lines: they belong to no entry.
    pub source: Option<SourceView>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ComposedView {
    pub lines: Vec<LineView>,
    pub overridden: Vec<String>,
    /// In a repo: what its file starts with so the checkout's tracked instructions still load (the adapter's
    /// preamble for its first checkout).
    #[serde(default)]
    pub preamble: Option<String>,
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
                            kind: e.map(|e| e.meta.kind),
                            scope: e.map(|e| e.scope.to_string()).unwrap_or_default(),
                            stack: s.stack,
                            file: s.file.clone(),
                            file_line: s.file_line,
                        }
                    },
                );
            LineView { n: i + 1, text: text.to_string(), source }
        })
        .collect();
    ComposedView { lines, overridden: c.overridden.clone(), preamble: None }
}

// ---------------------------------------------------------------- the stack repos' unpushed commits

/// One stack repo's commits that its upstream doesn't have yet.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AheadView {
    pub stack: Visibility,
    pub branch: String,
    /// `None`: the branch has no upstream, so nothing can be pushed.
    pub upstream: Option<String>,
    pub commits: Vec<crate::git::Commit>,
    /// Why it couldn't be read; never taken for "nothing to push".
    pub error: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct StatusView {
    pub repos: Vec<AheadView>,
}

impl StatusView {
    pub fn unpushed(&self) -> usize {
        self.repos.iter().map(|r| r.commits.len()).sum()
    }
}
