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

/// Every id a context returns, duties and index together.
fn all_ids(c: &serve::Context) -> Vec<String> {
    ids(&c.duties).into_iter().chain(ids(&c.index)).collect()
}

// Spec: design §2.1 "load the two core layers and the selected project's declared layers": a path entry of the bound
// repo reaches a session working under that path, and only there (sol-loop r1, S1).
#[test]
fn a_path_entry_reaches_sessions_under_its_path() {
    let w = World::new();
    fs::create_dir_all(w.path("alpha-app/src/deep")).unwrap();
    w.write(
        "stores/alpha/repo/alpha-app/rules/src-rule.md",
        &entry(
            "rule.src-rule",
            "path:alpha-app:src",
            "private",
            "roles: [coder]\n",
            "Under src.\n",
        ),
    );
    let deep = w.brief("alpha-app/src/deep", "coder");
    let c = serve::context(&w.home(), &deep, "coder", None, "2026-10-28").unwrap();
    assert!(ids(&c.duties).contains(&"rule.src-rule".to_string()), "{:?}", all_ids(&c));
    let found = serve::search(&w.home(), &deep, "", &["rule.src-rule".into()], false, "2026-10-28");
    assert_eq!(ids(&found.unwrap()), ["rule.src-rule"]);
    let top = w.brief("alpha-app", "coder");
    let c = serve::context(&w.home(), &top, "coder", None, "2026-10-28").unwrap();
    assert!(!all_ids(&c).contains(&"rule.src-rule".to_string()), "not above its path");
}

// Spec: design §2.1 "reuse entry ids, provenance, roles, scopes, overrides and extensions": an entry another applicable
// entry overrides is left out, as the composer leaves it out (sol-loop r1, S2).
#[test]
fn an_overridden_entry_is_left_out() {
    let w = World::new();
    w.write(
        "stores/alpha/project/alpha/rules/replaces.md",
        &entry(
            "rule.alpha-replaces",
            "project:alpha",
            "private",
            "overrides: [rule.core-rule]\n",
            "The project's own version.\n",
        ),
    );
    let id = w.brief("alpha-app", "coder");
    let c = serve::context(&w.home(), &id, "coder", None, "2026-10-28").unwrap();
    let all = all_ids(&c);
    assert!(
        all.contains(&"rule.alpha-replaces".to_string())
            && !all.contains(&"rule.core-rule".to_string()),
        "{all:?}"
    );
    assert!(serve::search(&w.home(), &id, "core rule text", &[], false, "2026-10-28")
        .unwrap()
        .is_empty());
    // Elsewhere the core rule still holds.
    fs::create_dir_all(w.path("elsewhere")).unwrap();
    let out = w.brief("elsewhere", "coder");
    let c = serve::context(&w.home(), &out, "coder", None, "2026-10-28").unwrap();
    assert!(all_ids(&c).contains(&"rule.core-rule".to_string()));
}

// Spec: design §2.1 "Missing selected store is a visible error with core-only fallback" (sol-loop r1, S3): the brief
// still binds the session to the core, says why, and nothing can be written.
#[test]
fn a_missing_store_falls_back_to_the_core_visibly() {
    let w = World::new();
    fs::rename(w.path("stores/alpha"), w.path("stores/alpha-moved")).unwrap();
    let b = serve::brief(&w.home(), &w.start(&w.path("alpha-app"), "coder")).unwrap();
    assert_eq!(b.project, None);
    assert!(b.text.contains("alpha") && b.text.contains("core only"), "{}", b.text);
    let c = serve::context(&w.home(), &b.delivery_id, "coder", None, "2026-10-28").unwrap();
    let mut all = all_ids(&c);
    all.sort();
    assert_eq!(all, ["fact.core-fact", "rule.core-rule"]);
    let note = Note { text: "x".into(), ..Note::default() };
    assert!(serve::remember(&w.home(), &b.delivery_id, &note, "2026-10-28").is_err());
}

// Spec: design §2.1, search is "scoped to the same binding" as context, role included (sol-loop r2, S4): an override
// meant for another role hides nothing from this delivery, so search returns what its context lists.
#[test]
fn search_and_context_agree_for_the_deliverys_role() {
    let w = World::new();
    w.write(
        "stores/alpha/project/alpha/rules/reviewer-version.md",
        &entry(
            "rule.reviewer-version",
            "project:alpha",
            "private",
            "roles: [reviewer]\noverrides: [rule.core-rule]\n",
            "For reviewers instead.\n",
        ),
    );
    let id = w.brief("alpha-app", "coder");
    let c = serve::context(&w.home(), &id, "coder", None, "2026-10-28").unwrap();
    assert!(all_ids(&c).contains(&"rule.core-rule".to_string()), "{:?}", all_ids(&c));
    let found =
        serve::search(&w.home(), &id, "", &["rule.core-rule".into()], false, "2026-10-28").unwrap();
    assert_eq!(ids(&found), ["rule.core-rule"]);
    let reviewer =
        serve::search(&w.home(), &id, "", &["rule.reviewer-version".into()], false, "2026-10-28");
    assert!(reviewer.unwrap().is_empty(), "another role's entry is not this delivery's");
}

/// A junction at `link` to `target` (cmd reads a `/` as a switch, so both go with `\`).
fn junction(link: &Path, target: &Path) {
    let bs = |p: &Path| p.to_string_lossy().replace('/', "\\");
    let out = std::process::Command::new("cmd")
        .args(["/c", "mklink", "/J"])
        .arg(bs(link))
        .arg(bs(target))
        .output()
        .unwrap();
    assert!(out.status.success(), "mklink: {}", String::from_utf8_lossy(&out.stderr));
}

/// What a session at `place` gets now: its brief's project and every id of its context.
fn served(w: &World, place: &str) -> (Option<String>, Vec<String>, String) {
    let b = serve::brief(&w.home(), &w.start(&w.path(place), "coder")).unwrap();
    let c = serve::context(&w.home(), &b.delivery_id, "coder", None, "2026-10-28").unwrap();
    let note = Note { text: "x".into(), ..Note::default() };
    assert!(
        serve::remember(&w.home(), &b.delivery_id, &note, "2026-10-28").is_err(),
        "no write: {}",
        b.text
    );
    (b.project, all_ids(&c), b.text)
}

// Spec: design §2.1 "Selection precedes reads … one selected project"; Sol review r1 F1 (witness `linked-store`): a
// registered store path that is now a junction to another project's store is not that project: the session falls
// back to the core, visibly, and gets nothing of the other store.
#[test]
#[cfg(windows)]
fn a_store_path_replaced_by_a_link_is_not_served() {
    let w = World::new();
    let before = w.brief("alpha-app", "coder");
    fs::rename(w.path("stores/alpha"), w.path("stores/alpha-real")).unwrap();
    junction(&w.path("stores/alpha"), &w.path("stores/beta"));
    let (project, ids, text) = served(&w, "alpha-app");
    assert_eq!(project, None, "{text}");
    assert!(text.contains("core only") && text.contains("alpha"), "{text}");
    assert!(ids.iter().all(|i| !i.contains("beta")), "{ids:?}");
    assert!(
        serve::context(&w.home(), &before, "coder", None, "2026-10-28").is_err(),
        "the old delivery rebriefs"
    );
}

// Spec: as above, F1 "store-root and descendant links": a link inside the selected store reaches no other store.
#[test]
#[cfg(windows)]
fn a_link_inside_the_store_is_not_followed() {
    let w = World::new();
    junction(
        &w.path("stores/alpha/project/alpha/rules/linked"),
        &w.path("stores/beta/project/beta/rules"),
    );
    let (project, ids, text) = served(&w, "alpha-app");
    assert_eq!(project, None, "{text}");
    assert!(ids.iter().all(|i| !i.contains("beta")), "{ids:?}");
}

// Spec: F1 "validate the registered store identity and expected project": a store whose manifest now names another
// project is not served as the bound one.
#[test]
fn a_store_whose_manifest_names_another_project_is_not_served() {
    let w = World::new();
    let beta = fs::read_to_string(w.path("stores/beta/stack.toml")).unwrap();
    fs::write(w.path("stores/alpha/stack.toml"), beta).unwrap();
    let (project, _, text) = served(&w, "alpha-app");
    assert_eq!(project, None, "{text}");
    assert!(text.contains("beta"), "the reason names what it found: {text}");
}

// Spec: F1 (witness `foreign-layer`) "select only that project's declared layers … invalid placement/foreign scope
// must never become deliverable": an entry in another project's layer dir, scoped to one of the store's own places,
// is never served; the store falls back to the core with the reason.
#[test]
fn an_entry_in_a_foreign_layer_is_never_served() {
    let w = World::new();
    w.write(
        "stores/alpha/project/beta/rules/foreign.md",
        &entry("rule.foreign", "repo:alpha-app", "private", "roles: [coder]\n", "FOREIGN_BODY\n"),
    );
    let (project, ids, text) = served(&w, "alpha-app");
    assert!(!ids.contains(&"rule.foreign".to_string()), "{ids:?}");
    assert_eq!(project, None, "{text}");
}

// Spec: Sol review r1 F2 (witness `unreadable-file`); design §2.1 "Missing selected store is a visible error with
// core-only fallback": a required entry that can't be read (another process holds it, OS error 32) leaves no partial
// project active or writable.
#[test]
#[cfg(windows)]
fn an_unreadable_entry_leaves_no_partial_project() {
    use std::os::windows::fs::OpenOptionsExt;
    let w = World::new();
    let before = w.brief("alpha-app", "coder");
    let held = fs::OpenOptions::new()
        .read(true)
        .share_mode(0)
        .open(w.path("stores/alpha/project/alpha/rules/alpha-coder.md"))
        .unwrap();
    let (project, ids, text) = served(&w, "alpha-app");
    assert_eq!(project, None, "{text}");
    assert!(!ids.contains(&"fact.alpha-fact".to_string()), "{ids:?}");
    let err = serve::context(&w.home(), &before, "coder", None, "2026-10-28").unwrap_err();
    assert!(err.contains("lords brief"), "an actionable rebrief: {err}");
    drop(held);
}

// Spec: Sol review r1 F3 (witness `registration.log`): every acknowledged registration is kept when several run at
// once, and a reader meanwhile never sees a registry with less than it held.
#[test]
fn concurrent_registrations_all_survive() {
    isolate_git();
    let dir = TempDir::new().unwrap();
    let home = dir.path().join("registry");
    let stores: Vec<PathBuf> = (0..12)
        .map(|n| {
            let s = dir.path().join(format!("s{n}"));
            fs::create_dir_all(&s).unwrap();
            fs::write(s.join("stack.toml"), format!("name = \"s{n}\"\nvisibility = \"private\"\nproject = \"p{n}\"\n[projects.p{n}]\n")).unwrap();
            s
        })
        .collect();
    identity::set_core(&home, dir.path(), None).unwrap();
    std::thread::scope(|sc| {
        let reader = sc.spawn(|| {
            for _ in 0..200 {
                let reg = identity::Registry::load(&home).unwrap();
                assert!(reg.core.is_some(), "a reader never sees the registry empty");
            }
        });
        let writers: Vec<_> =
            stores.iter().map(|s| sc.spawn(|| identity::register_store(&home, s))).collect();
        for h in writers {
            h.join().unwrap().unwrap();
        }
        reader.join().unwrap();
    });
    let reg = identity::Registry::load(&home).unwrap();
    let mut projects: Vec<String> = reg.stores.iter().map(|s| s.project.clone()).collect();
    projects.sort();
    let mut want: Vec<String> = (0..12).map(|n| format!("p{n}")).collect();
    want.sort();
    assert_eq!(projects, want);
}

// Spec: Sol fix check 1, F3 (witness `review_registry_interrupted_save`): "registry publication/recovery must preserve
// the last acknowledged state across interruption, or refuse with an actionable recovery path instead of silently
// treating it as first use". A save interrupted after moving the registry aside is never read as an empty registry,
// and no later change builds on that emptiness.
#[test]
fn an_interrupted_registry_save_is_never_read_as_empty() {
    use lords_core::fsguard::{self, Part, Root};
    let w = World::new();
    let root = Root::home(&w.home()).unwrap();
    let held = fsguard::lock(&root.join(&[Part::new("stores.lock").unwrap()])).unwrap();
    let aside = fsguard::move_aside(&root.join(&[Part::new("stores.toml").unwrap()])).unwrap();
    drop(held);
    let loaded = identity::Registry::load(&w.home());
    assert!(
        loaded.as_ref().is_ok_and(|r| r.stores.len() == 2 && r.core.is_some()) || loaded.is_err(),
        "an interrupted save read as {loaded:?}"
    );
    if let Err(e) = &loaded {
        assert!(
            e.contains(&aside.path().display().to_string()),
            "the error names what to restore: {e}"
        );
        assert!(
            identity::set_core(&w.home(), &w.path("core/pub"), None).is_err(),
            "nothing builds on it"
        );
    }
}

// Spec: as above, every interruption of a save that publishes by a staged copy: a partial staged copy (each prefix
// of the new registry) beside an intact registry leaves the last acknowledged registry readable, and the next save
// succeeds over it and keeps every registration.
#[test]
fn a_save_interrupted_at_any_byte_keeps_the_last_registry() {
    let w = World::new();
    let home = w.home();
    let before = fs::read(home.join("stores.toml")).unwrap();
    let next = before
        .iter()
        .copied()
        .chain(b"# a save in progress\n".iter().copied())
        .collect::<Vec<u8>>();
    for cut in [0, 1, next.len() / 2, next.len() - 1, next.len()] {
        let staged = home.join(format!(".stores.toml.lords-new-1-{cut}-0"));
        fs::write(&staged, &next[..cut]).unwrap();
        let reg = identity::Registry::load(&home).unwrap();
        assert_eq!(reg.stores.len(), 2, "cut {cut}");
        identity::set_core(&home, &w.path("core/pub"), None).unwrap();
        assert_eq!(
            identity::Registry::load(&home).unwrap().stores.len(),
            2,
            "cut {cut}: the next save keeps both"
        );
    }
}
