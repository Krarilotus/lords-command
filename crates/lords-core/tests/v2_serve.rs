//! v2 slice 1a (design §5): the agent API over one core and the project store a working dir selects. Every fixture
//! lives in its own temp dir with its own registry home; nothing here reads a real home dir.

// Tests are outside the filesystem boundary (fsguard.rs): their fixtures write their own temp dirs.
#![allow(clippy::disallowed_methods, clippy::disallowed_types)]

mod common;

use common::{entry, git, isolate_git};
use lords_core::identity;
use lords_core::serve::{self, Note, Start};
use lords_core::{check, migrate, privacy::Rules, Stacks};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use tempfile::TempDir;

const ROLES: &str = "roles = [\"orchestrator\", \"coder\", \"helper\", \"reviewer\"]\n";

struct World {
    dir: TempDir,
}

impl World {
    fn path(&self, rel: &str) -> PathBuf {
        self.dir.path().join(rel)
    }

    fn write(&self, rel: &str, text: &str) {
        let p = self.path(rel);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, text).unwrap();
    }

    fn home(&self) -> PathBuf {
        self.path("registry")
    }

    /// The core (a public stack and a private overlay whose non-global layers are broken: v2 never opens them),
    /// two project stores `alpha` and `beta` with a checkout each, both registered.
    fn new() -> World {
        isolate_git();
        let w = World { dir: TempDir::new().unwrap() };
        w.write("core/pub/stack.toml", "name = \"pub\"\nvisibility = \"public\"\n");
        w.write(
            "core/pub/global/rules/core-rule.md",
            &entry("rule.core-rule", "global", "public", "", "Core rule text.\n"),
        );
        w.write(
            // No roles here: a project store's entries are checked against their own store's lists.
            "core/priv/stack.toml",
            "name = \"priv\"\nvisibility = \"private\"\n",
        );
        w.write(
            "core/priv/global/facts/core-fact.md",
            &entry("fact.core-fact", "global", "private", "", "Core fact text.\n"),
        );
        // The old mixed overlay's other layers: unreadable as entries. Reading them would be a problem.
        w.write("core/priv/project/beta/rules/broken.md", "no frontmatter at all\n");
        for p in ["alpha", "beta"] {
            w.write(&format!("{p}-app/README.md"), "app\n");
            git(&w.path(&format!("{p}-app")), &["init", "-q"]);
            git(&w.path(&format!("{p}-app")), &["add", "-A"]);
            git(&w.path(&format!("{p}-app")), &["commit", "-q", "-m", "init"]);
            let checkout = w.path(&format!("{p}-app")).display().to_string().replace('\\', "/");
            w.write(
                &format!("stores/{p}/stack.toml"),
                &format!(
                    "name = \"{p}-lords\"\nvisibility = \"private\"\nproject = \"{p}\"\n{ROLES}topics = [\"t\"]\n\
                     [projects.{p}]\n[repos.{p}-app]\nproject = \"{p}\"\ncheckouts = ['{checkout}']\n"
                ),
            );
            w.write(
                &format!("stores/{p}/project/{p}/rules/{p}-coder.md"),
                &entry(
                    &format!("rule.{p}-coder"),
                    &format!("project:{p}"),
                    "private",
                    "roles: [coder]\n",
                    &format!("{p} coder duty.\n"),
                ),
            );
            w.write(
                &format!("stores/{p}/project/{p}/facts/{p}-fact.md"),
                &entry(
                    &format!("fact.{p}-fact"),
                    &format!("project:{p}"),
                    "private",
                    "topics: [t]\n",
                    &format!("{p} fact text.\n"),
                ),
            );
        }
        identity::set_core(&w.home(), &w.path("core/pub"), Some(&w.path("core/priv"))).unwrap();
        identity::register_store(&w.home(), &w.path("stores/alpha")).unwrap();
        identity::register_store(&w.home(), &w.path("stores/beta")).unwrap();
        w
    }

    fn start(&self, cwd: &Path, role: &str) -> Start {
        Start {
            cwd: cwd.to_path_buf(),
            role: role.into(),
            harness: "claude".into(),
            agent: "root".into(),
            ..Start::default()
        }
    }

    fn brief(&self, place: &str, role: &str) -> String {
        serve::brief(&self.home(), &self.start(&self.path(place), role)).unwrap().delivery_id
    }
}

/// Every file under `dir` with its bytes (the whole store, as a hash witness).
fn snapshot(dir: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
    let mut out = BTreeMap::new();
    fn walk(d: &Path, out: &mut BTreeMap<PathBuf, Vec<u8>>) {
        for e in fs::read_dir(d).unwrap().flatten() {
            let p = e.path();
            if p.is_dir() {
                walk(&p, out);
            } else {
                out.insert(p.clone(), fs::read(&p).unwrap());
            }
        }
    }
    walk(dir, &mut out);
    out
}

fn ids(items: &[serve::Item]) -> Vec<String> {
    items.iter().map(|i| i.id.clone()).collect()
}

// Spec: design §5 1a, reproduce the F1 import witness: the other project's store keeps its hashes and is never read; §2.1 "Check/search/
// import never open unrelated stores". Another project's store is broken after registration (its manifest is no
// TOML): every command of the bound project still works, sees none of the other project, and changes no byte there.
#[test]
fn a_bound_project_never_opens_another_projects_store() {
    let w = World::new();
    let beta = w.path("stores/beta");
    fs::write(beta.join("stack.toml"), "%%% not toml").unwrap();
    let before = snapshot(&beta);
    let id = w.brief("alpha-app", "coder");
    let c = serve::context(&w.home(), &id, "coder", None, "2026-10-28").unwrap();
    assert_eq!(c.project.as_deref(), Some("alpha"));
    assert!(c.problems.is_empty(), "no other layer or store was read: {:?}", c.problems);
    assert_eq!(ids(&c.duties), ["rule.alpha-coder"]);
    let all: Vec<String> = ids(&c.duties).into_iter().chain(ids(&c.index)).collect();
    for want in ["rule.core-rule", "fact.core-fact", "fact.alpha-fact"] {
        assert!(all.contains(&want.to_string()), "{want} in {all:?}");
    }
    assert!(all.iter().all(|i| !i.contains("beta")), "nothing of beta: {all:?}");
    let found = serve::search(&w.home(), &id, "fact text", &[], false, "2026-10-28").unwrap();
    assert_eq!(ids(&found), ["fact.core-fact", "fact.alpha-fact"]);
    let note = Note { text: "alpha learnt this".into(), ..Note::default() };
    serve::remember(&w.home(), &id, &note, "2026-10-28").unwrap();
    assert_eq!(snapshot(&beta), before, "beta's store is byte for byte unchanged");
}

// Spec: design §2.1 "Resolve names inside core + selected project; reject foreign-project relations": a link from
// alpha to an entry of beta's dangles in alpha's check.
#[test]
fn a_link_into_another_project_dangles() {
    let w = World::new();
    w.write(
        "stores/alpha/project/alpha/facts/linker.md",
        &entry("fact.linker", "project:alpha", "private", "links: [fact.beta-fact]\n", "x\n"),
    );
    let reg = identity::Registry::load(&w.home()).unwrap();
    let core = reg.core.unwrap();
    let stacks =
        Stacks::selected(&core.public, core.private.as_deref(), Some(&w.path("stores/alpha")))
            .unwrap();
    let findings = check::check_stack(&stacks, &Rules::public());
    let errors: Vec<_> =
        findings.iter().filter(|f| f.severity == lords_core::Severity::Error).collect();
    assert!(!errors.is_empty(), "{findings:#?}");
    assert!(
        errors.iter().all(|f| f.entry.as_deref() == Some("fact.linker") && f.code == "dangling"),
        "only the link dangles; the store's roles and topics are its own: {errors:#?}"
    );
}

// Spec: design §2.1, a project store holds its own project alone ("reject foreign-project relations"): an entry
// scoped to another project, or a global layer, is an error of the store.
#[test]
fn a_store_refuses_entries_of_another_scope() {
    let w = World::new();
    w.write(
        "stores/alpha/project/beta/facts/stray.md",
        &entry("fact.stray", "project:beta", "private", "", "x\n"),
    );
    w.write(
        "stores/alpha/global/facts/core-ish.md",
        &entry("fact.core-ish", "global", "private", "", "x\n"),
    );
    let s = lords_core::stack::Stack::load_reading(
        &w.path("stores/alpha"),
        lords_core::stack::Reads::ProjectStore,
    )
    .unwrap();
    let foreign: Vec<_> = s.problems.iter().filter(|p| p.code == "foreign-scope").collect();
    assert_eq!(foreign.len(), 2, "{:#?}", s.problems);
    assert!(s.find("fact.core-ish").is_none(), "a store's global layer is never loaded");
}

// Spec: design §4 worked example: until 2026-10-30, review_by 2026-10-28, reason given. On 29 Oct it is returned
// "review due"; from 00:00 UTC on 30 Oct it is excluded (search shows it only on request, labelled).
#[test]
fn a_remembered_note_falls_due_then_ends_on_its_dates() {
    let w = World::new();
    let id = w.brief("alpha-app", "coder");
    let note = Note {
        text: "pilot note body".into(),
        title: Some("pilot note".into()),
        until: Some("2026-10-30".into()),
        review_by: Some("2026-10-28".into()),
        reason: Some("delivery pilot".into()),
        ..Note::default()
    };
    let r = serve::remember(&w.home(), &id, &note, "2026-10-27").unwrap();
    assert_eq!(r.id, "memory.pilot-note");
    let on = |day: &str| {
        let c = serve::context(&w.home(), &id, "coder", None, day).unwrap();
        c.index.into_iter().chain(c.duties).find(|i| i.id == r.id)
    };
    let label = |day: &str| on(day).unwrap().label.unwrap_or_default();
    assert_eq!(label("2026-10-28"), "temporary, until 2026-10-30");
    assert_eq!(label("2026-10-29"), "temporary, until 2026-10-30; review due since 2026-10-28");
    assert!(on("2026-10-30").is_none(), "excluded from 30 Oct");
    let c = serve::context(&w.home(), &id, "coder", None, "2026-10-29").unwrap();
    assert_eq!(c.valid_until.as_deref(), Some("2026-10-30"));
    assert!(serve::search(&w.home(), &id, "pilot", &[], false, "2026-10-30").unwrap().is_empty());
    let late = serve::search(&w.home(), &id, "pilot", &[], true, "2026-10-30").unwrap();
    assert_eq!(ids(&late), std::slice::from_ref(&r.id));
    assert!(late[0].label.as_deref().unwrap_or("").starts_with("expired"), "{:?}", late[0].label);
}

// Spec: design §4 remember: "Any temporary request needs a nonblank reason and at least one valid calendar date …
// reject malformed/impossible dates and already-expired writes"; nothing is written when it is refused.
#[test]
fn a_temporary_note_without_reason_or_valid_date_is_refused_before_writing() {
    let w = World::new();
    let id = w.brief("alpha-app", "coder");
    let store = w.path("stores/alpha");
    let before = snapshot(&store);
    let t = |until: Option<&str>, review: Option<&str>, reason: Option<&str>| Note {
        text: "x".into(),
        until: until.map(Into::into),
        review_by: review.map(Into::into),
        reason: reason.map(Into::into),
        ..Note::default()
    };
    for (case, note) in [
        ("no reason", t(Some("2026-11-30"), None, None)),
        ("blank reason", t(Some("2026-11-30"), None, Some("  "))),
        ("no date", t(None, None, Some("why"))),
        ("impossible date", t(Some("2026-02-30"), None, Some("why"))),
        ("malformed date", t(Some("30.11.2026"), None, Some("why"))),
        ("already ended", t(Some("2026-10-28"), None, Some("why"))),
    ] {
        assert!(serve::remember(&w.home(), &id, &note, "2026-10-28").is_err(), "{case} is refused");
    }
    assert_eq!(snapshot(&store), before, "nothing was written");
}

// Spec: design §2.1 "remember writes only to the bound project and within role authority … Unknown locations
// refuse both writes. Helpers/reviewers report lessons instead"; §4 "Only permitted project memory/fact kinds".
#[test]
fn remember_is_refused_outside_a_project_for_non_writers_and_for_other_kinds() {
    let w = World::new();
    fs::create_dir_all(w.path("elsewhere")).unwrap();
    let note = Note { text: "x".into(), ..Note::default() };
    for role in ["helper", "reviewer"] {
        let id = w.brief("alpha-app", role);
        assert!(
            serve::remember(&w.home(), &id, &note, "2026-10-28").is_err(),
            "{role} doesn't write"
        );
    }
    let nowhere = w.brief("elsewhere", "coder");
    assert!(
        serve::remember(&w.home(), &nowhere, &note, "2026-10-28").is_err(),
        "no project, no write"
    );
    let id = w.brief("alpha-app", "coder");
    let rule = Note { text: "x".into(), kind: Some("rule".into()), ..Note::default() };
    assert!(serve::remember(&w.home(), &id, &rule, "2026-10-28").is_err(), "no route to a rule");
    let fact = Note { text: "a fact".into(), kind: Some("fact".into()), ..Note::default() };
    assert_eq!(serve::remember(&w.home(), &id, &fact, "2026-10-28").unwrap().id, "fact.a-fact");
}

// Spec: design §5 1a "concurrent remember … witnesses"; §4 atomic write: lock the selected store, never accept a
// path from text. Eight writers with one title: eight entries, eight ids, and the store loads without a problem.
#[test]
fn concurrent_writers_each_get_their_own_entry() {
    let w = World::new();
    let id = w.brief("alpha-app", "coder");
    let home = w.home();
    let made: Vec<String> = std::thread::scope(|s| {
        let hs: Vec<_> = (0..8)
            .map(|n| {
                let (home, id) = (home.clone(), id.clone());
                s.spawn(move || {
                    let note = Note {
                        text: format!("body {n}"),
                        title: Some("../same title".into()),
                        ..Note::default()
                    };
                    serve::remember(&home, &id, &note, "2026-10-28").unwrap().id
                })
            })
            .collect();
        hs.into_iter().map(|h| h.join().unwrap()).collect()
    });
    let mut unique = made.clone();
    unique.sort();
    unique.dedup();
    assert_eq!(unique.len(), 8, "{made:?}");
    let s = lords_core::stack::Stack::load_reading(
        &w.path("stores/alpha"),
        lords_core::stack::Reads::ProjectStore,
    )
    .unwrap();
    assert!(s.problems.is_empty(), "{:#?}", s.problems);
    for i in &made {
        let e = s.find(i).unwrap();
        assert!(
            e.meta_file.starts_with(w.path("stores/alpha/project/alpha/memory/remembered")),
            "{}",
            e.meta_file.display()
        );
    }
}

// Spec: design §2.3 "Invalid, stale, wrong-agent or wrong-role receipts never satisfy another key"; §2.1 the
// delivery id binds the session, and a request can't select another project or role by changing a field.
#[test]
fn a_delivery_serves_only_its_own_role_and_known_ids() {
    let w = World::new();
    let id = w.brief("alpha-app", "helper");
    assert!(
        serve::context(&w.home(), &id, "orchestrator", None, "2026-10-28").is_err(),
        "wrong role"
    );
    assert!(
        serve::context(&w.home(), "d-0000", "helper", None, "2026-10-28").is_err(),
        "unknown id"
    );
    let c = serve::context(&w.home(), &id, "helper", None, "2026-10-28").unwrap();
    assert!(c.duties.is_empty(), "the coder's duty is not the helper's: {:?}", ids(&c.duties));
    let beta = w.brief("beta-app", "helper");
    let b = serve::context(&w.home(), &beta, "helper", None, "2026-10-28").unwrap();
    assert_eq!(b.project.as_deref(), Some("beta"));
}

// Spec: design §2.1 "Unknown or ambiguous locations receive core only and cannot write".
#[test]
fn an_unknown_dir_gets_the_core_alone() {
    let w = World::new();
    fs::create_dir_all(w.path("elsewhere")).unwrap();
    let b = serve::brief(&w.home(), &w.start(&w.path("elsewhere"), "coder")).unwrap();
    assert_eq!(b.project, None);
    let c = serve::context(&w.home(), &b.delivery_id, "coder", None, "2026-10-28").unwrap();
    let mut all: Vec<String> = ids(&c.duties).into_iter().chain(ids(&c.index)).collect();
    all.sort();
    assert_eq!(all, ["fact.core-fact", "rule.core-rule"]);
}

// Spec: design §2.1 "On-demand context has a configurable budget/pagination; omitted duties must be named, not lost".
#[test]
fn duties_past_the_budget_are_named() {
    let w = World::new();
    let id = w.brief("alpha-app", "coder");
    let c = serve::context(&w.home(), &id, "coder", Some(5), "2026-10-28").unwrap();
    assert!(c.duties.is_empty());
    assert_eq!(c.omitted, ["rule.alpha-coder"]);
    assert!(ids(&c.index).contains(&"rule.alpha-coder".to_string()), "still in the index");
}

/// An old mixed private stack holding two projects, as v1 has it.
fn old_stack(w: &World) -> PathBuf {
    w.write(
        "old/stack.toml",
        &format!(
            "name = \"old\"\nvisibility = \"private\"\n{ROLES}topics = [\"t\"]\n[projects.alpha]\ntitle = \"A\"\n[projects.beta]\n\
             [repos.app]\nproject = \"alpha\"\ncheckouts = ['{}']\n[repos.other]\nproject = \"beta\"\n\
             [workspaces.ws]\nproject = \"alpha\"\npath = '{}'\n\
             [[memory_stores]]\nname = \"alpha-notes\"\npath = \"~/nowhere-a\"\nscope = \"project:alpha\"\n\
             [[memory_stores]]\nname = \"beta-notes\"\npath = \"~/nowhere-b\"\nscope = \"project:beta\"\n",
            w.path("alpha-app").display().to_string().replace('\\', "/"),
            w.path("ws").display().to_string().replace('\\', "/"),
        ),
    );
    w.write("old/global/facts/g.md", &entry("fact.g", "global", "private", "", "g\n"));
    w.write(
        "old/project/alpha/rules/a.md",
        &entry("rule.a", "project:alpha", "private", "", "a\n"),
    );
    w.write("old/repo/app/facts/b.md", &entry("fact.b", "repo:app", "private", "", "b\n"));
    w.write("old/workspace/ws/facts/c.md", &entry("fact.c", "workspace:ws", "private", "", "c\n"));
    w.write("old/project/beta/rules/z.md", &entry("rule.z", "project:beta", "private", "", "z\n"));
    w.write("old/repo/other/facts/y.md", &entry("fact.y", "repo:other", "private", "", "y\n"));
    w.path("old")
}

// Spec: design §5 1a the project's owner migrates its entries and declared memories with an id/hash manifest; §5 "the old mixed
// private stack is a rollback source": the copy holds exactly the project's ids and bytes, the source is unchanged,
// and the other project stays behind.
#[test]
fn migrate_copies_one_project_with_its_manifest_and_leaves_the_source() {
    let w = World::new();
    let old = old_stack(&w);
    let before = snapshot(&old);
    let to = w.path("new/alpha-lords");
    let m = migrate::migrate(&old, "alpha", &to).unwrap();
    assert!(m.problems.is_empty(), "{:?}", m.problems);
    assert_eq!(m.ids, ["fact.b", "fact.c", "rule.a"]);
    assert_eq!(snapshot(&old), before, "the source is unchanged");
    let store = lords_core::stack::Stack::load_reading(&to, lords_core::stack::Reads::ProjectStore)
        .unwrap();
    assert_eq!(store.manifest.project.as_deref(), Some("alpha"));
    assert!(
        store.manifest.repos.contains_key("app") && !store.manifest.repos.contains_key("other")
    );
    let stores: Vec<&str> = store.manifest.memory_stores.iter().map(|s| s.name.as_str()).collect();
    assert_eq!(stores, ["alpha-notes"]);
    for f in &m.files {
        assert_eq!(
            lords_core::fsguard::sha(&fs::read(to.join(&f.file)).unwrap()),
            f.sha256,
            "{}",
            f.file
        );
        assert_eq!(fs::read(old.join(&f.file)).unwrap(), fs::read(to.join(&f.file)).unwrap());
    }
    let record: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(to.join(migrate::RECORD)).unwrap()).unwrap();
    assert_eq!(record["ids"], serde_json::json!(["fact.b", "fact.c", "rule.a"]));
    assert!(migrate::migrate(&old, "alpha", &to).is_err(), "never over an existing store");
}
