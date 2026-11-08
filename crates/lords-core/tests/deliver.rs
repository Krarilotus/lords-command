//! F8, a stack commit reaches every declared place on D13's type (design note
//! Harness/notes/2026-11-07-lords-f8-delivery.md): one safe-only plan, made and applied under the apply lock, place by
//! place, each fully current or untouched; what isn't current is in the state file `lords paths` reads; D22's retries,
//! D23's lock scope, D24's load check and the `[delivery]` settings.

// Tests are outside the filesystem boundary (fsguard.rs): their fixtures write their own temp dirs.
#![allow(clippy::disallowed_methods, clippy::disallowed_types)]

mod common;

use common::{entry, git, Fixture};
use lords_core::deliver::{self, Options, Report, State, StatePlace};
use lords_core::ledger::{key, Ledger, Owned};
use lords_core::stack::{Stacks, Store};
use lords_core::sync::{import_and_apply, import_and_apply_between, ApplyLock, Step};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

const ROLES: &str = "roles = [\"coder\", \"reviewer\"]\n";

/// The public rule `rule.r`, for both roles, so every place file carries it.
fn rule(f: &Fixture, body: &str) {
    let roles = "roles: [coder, reviewer]\n";
    f.write("pub/global/rules/r.md", &entry("rule.r", "global", "public", roles, body));
}

fn commit(f: &Fixture, store: &str, message: &str) {
    git(&f.path(store), &["add", "-A"]);
    git(&f.path(store), &["commit", "-q", "-m", message]);
}

fn read(p: &Path) -> String {
    fs::read_to_string(p).unwrap_or_default()
}

/// A place's two files.
fn files(p: &Path) -> [String; 2] {
    ["CLAUDE.local.md", "AGENTS.override.md"].map(|n| read(&p.join(n)))
}

/// Lords' staged and backup names left in a place (`.<name>.lords-…`).
fn leftovers(p: &Path) -> Vec<String> {
    let names = fs::read_dir(p).unwrap().map(|e| e.unwrap().file_name());
    names.map(|n| n.to_string_lossy().into_owned()).filter(|n| n.contains(".lords-")).collect()
}

/// The private stack: repo `beta-core`, its checkouts `a` and `b` (no worktrees), then `more`.
fn declare(f: &Fixture, a: &Path, b: &Path, more: &str) {
    f.write(
        "priv/stack.toml",
        &format!(
            "name = \"priv\"\nvisibility = \"private\"\n{ROLES}[projects.beta]\n[repos.beta-core]\nproject = \"beta\"\ndefault_role = \"coder\"\ncheckouts = ['{}', '{}']\nworktrees = false\n{more}",
            a.display(),
            b.display()
        ),
    );
}

/// Two places of repo `beta-core`, `a` and `b`, with the public rule; both stores committed, synced by hand once and
/// delivered once, so the state records the stores' trees. `more` goes at the end of the private stack.toml.
fn delivered_once(f: &Fixture, more: &str) -> (PathBuf, PathBuf) {
    rule(f, "Rule one.\n");
    let (a, b) = (f.path("a"), f.path("b"));
    for c in ["a", "b"] {
        f.write(&format!("{c}/README.md"), "a place\n");
        f.git_init(c);
    }
    f.write("pub/stack.toml", &format!("name = \"pub\"\nvisibility = \"public\"\n{ROLES}"));
    declare(f, &a, &b, more);
    f.git_init("pub");
    f.git_init("priv");
    import_and_apply(&f.pub_root(), Some(&f.priv_root()), &f.opts()).unwrap();
    for file in ["CLAUDE.local.md", "AGENTS.override.md"] {
        assert!(read(&a.join(file)).contains("Rule one."), "the fixture's place file: {file}");
    }
    let r = run(f).unwrap();
    assert!(!r.holds(), "{:?}", r.state);
    (a, b)
}

fn run(f: &Fixture) -> Result<Report, String> {
    deliver::run(&f.pub_root(), Some(&f.priv_root()), &Options::whole(&f.home()))
}

/// A whole delivery that runs `act` at the `nth` time it reaches `step` at `place` (any place, if none).
fn run_at(
    f: &Fixture,
    step: Step,
    nth: usize,
    place: Option<&Path>,
    act: impl FnMut(),
) -> Result<Report, String> {
    run_opts_at(f, &Options::whole(&f.home()), step, nth, place, act)
}

fn run_opts_at(
    f: &Fixture,
    opts: &Options,
    step: Step,
    nth: usize,
    place: Option<&Path>,
    mut act: impl FnMut(),
) -> Result<Report, String> {
    let mut seen = 0;
    deliver::run_between(&f.pub_root(), Some(&f.priv_root()), opts, &mut |s, p| {
        if s == step && place.is_none_or(|w| key(w) == key(p)) {
            seen += 1;
            if seen == nth {
                act();
            }
        }
    })
}

/// The state file, as `lords paths` reads it.
fn state(f: &Fixture) -> State {
    deliver::read(&f.home(), &f.stacks().delivery()).unwrap().expect("a state file")
}

/// The state's held place at `p`.
fn held_at<'a>(s: &'a State, p: &Path) -> Option<&'a StatePlace> {
    s.places.iter().find(|h| key(Path::new(&h.place)) == key(p))
}

fn rows(f: &Fixture) -> Vec<lords_core::paths::Row> {
    let ask =
        lords_core::paths::Ask { home: f.home(), codex: None, deadline: Duration::from_secs(1) };
    lords_core::paths::paths(&f.stacks(), &ask)
}

fn shown(rows: &[lords_core::paths::Row]) -> Vec<(String, String, String)> {
    rows.iter().map(|r| (r.state.to_string(), r.path.clone(), r.why.clone())).collect()
}

// Spec (brief item 1; note C1, C2, C6; F8 K5, "a corpus change reaches the next session of every path without anyone
// running a command"): once a store commit is delivered, every place's Claude and Codex files carry the committed text;
// the state records the stores' trees and holds nothing; `lords paths` reads stale between the commit and the delivery,
// and shows no stale row after it.
#[test]
fn a_committed_change_reaches_every_place() {
    let f = Fixture::new();
    let (a, b) = delivered_once(&f, "");
    rule(&f, "Rule two.\n");
    commit(&f, "pub", "rule two");
    let before = rows(&f);
    assert!(before.iter().any(|r| r.state == "stale"), "moved past: {:?}", shown(&before));

    let r = run(&f).unwrap();
    for p in [&a, &b] {
        for t in files(p) {
            assert!(t.contains("Rule two.") && !t.contains("Rule one."), "{t}");
        }
    }
    assert!(!r.holds(), "{:?}", r.state);
    let s = state(&f);
    assert!(s.failed.is_none() && s.places.is_empty(), "{s:?}");
    assert_eq!(s.trees.len(), 2, "both stores' trees: {s:?}");
    let after = rows(&f);
    assert!(after.iter().all(|r| r.state != "stale"), "{:?}", shown(&after));
}

// Spec (C2, C6, D13 A2; review F8-R1-02): a place one of whose files its owner changed is held whole while the other
// place gets the change; the state names the place, the changed file's reason (`changed-copy`) and the sync by hand that
// resolves it; `lords paths` gives no role a complete verdict over it, the place's default role nor another; once the
// owner's edit is gone and a sync by hand has run, the state no longer holds the place (A16).
#[test]
fn an_owner_changed_copy_holds_its_place_whole_and_the_state_names_it() {
    let f = Fixture::new();
    let (a, b) = delivered_once(&f, "");
    let mine = format!("{}My own line.\n", read(&a.join("CLAUDE.local.md")));
    fs::write(a.join("CLAUDE.local.md"), &mine).unwrap();
    let codex_before = read(&a.join("AGENTS.override.md"));
    rule(&f, "Rule two.\n");
    let only = entry("rule.rv", "global", "public", "roles: [reviewer]\n", "Review rule.\n");
    f.write("pub/global/rules/rv.md", &only);
    commit(&f, "pub", "rule two, a reviewer rule");

    let r = run(&f).unwrap();
    assert!(files(&b).iter().all(|t| t.contains("Rule two.")), "the other place is delivered");
    assert_eq!(read(&a.join("CLAUDE.local.md")), mine, "the owner's copy is never overwritten");
    assert_eq!(read(&a.join("AGENTS.override.md")), codex_before, "its place is held whole");
    assert!(r.holds());
    let s = state(&f);
    let held = held_at(&s, &a).unwrap_or_else(|| panic!("a is held: {s:?}"));
    assert!(held.items.iter().any(|i| i.reason == "changed-copy"), "{held:?}");
    assert!(held.command.starts_with("lords sync --repo beta-core --place "), "{held:?}");
    assert!(held_at(&s, &b).is_none(), "{s:?}");

    let rows = rows(&f);
    for role in ["coder", "reviewer"] {
        let mine = rows.iter().filter(|r| r.path.starts_with(&format!("T3-delegated as {role}")));
        assert!(mine.clone().all(|r| r.state != "complete"), "{role}: {:?}", shown(&rows));
        assert!(mine.clone().any(|r| r.state == "stale"), "{role}: {:?}", shown(&rows));
    }

    fs::remove_file(a.join("CLAUDE.local.md")).unwrap();
    import_and_apply(&f.pub_root(), Some(&f.priv_root()), &f.opts()).unwrap();
    assert!(files(&a).iter().all(|t| t.contains("Rule two.")));
    assert!(held_at(&state(&f), &a).is_none(), "{:?}", state(&f));
}

// Spec (F8 K4, sol-loop r1 S1; note A14): text a store holds but hasn't committed, edited, untracked or ignored, is never
// delivered: the delivery writes nothing, and the state says why, with the command that delivers once it is committed.
#[test]
fn uncommitted_store_text_is_never_delivered() {
    let f = Fixture::new();
    let (a, _) = delivered_once(&f, "");
    let failed = |f: &Fixture| state(f).failed.map(|x| (x.why, x.command));
    rule(&f, "Rule draft.\n");
    let r = run(&f);
    assert!(!read(&a.join("CLAUDE.local.md")).contains("Rule draft."));
    let why = failed(&f).unwrap_or_else(|| panic!("edited: {r:?}"));
    assert!(why.0.contains("uncommitted") && why.1 == "lords deliver", "{why:?}");
    assert!(r.is_err(), "{r:?}");

    rule(&f, "Rule one.\n");
    f.write(
        "priv/global/rules/new.md",
        &entry("rule.new", "global", "private", "", "Rule untracked.\n"),
    );
    let r = run(&f);
    assert!(!read(&a.join("CLAUDE.local.md")).contains("Rule untracked."));
    assert!(failed(&f).is_some_and(|w| w.0.contains("uncommitted")), "untracked: {r:?}");

    fs::remove_file(f.path("priv/global/rules/new.md")).unwrap();
    f.write("pub/.git/info/exclude", "global/rules/draft.md\n");
    let draft = entry("rule.draft", "global", "public", "roles: [coder]\n", "Rule ignored.\n");
    f.write("pub/global/rules/draft.md", &draft);
    let r = run(&f);
    assert!(!read(&a.join("CLAUDE.local.md")).contains("Rule ignored."));
    assert!(failed(&f).is_some_and(|w| w.0.contains("uncommitted")), "ignored: {r:?}");
}

// Spec (D13 A4; F8 sol-loop r1 S2, r2 S3, r3 S4): a place the stacks want a skill copy removed from is held whole, never
// half updated, even when the plan's view isn't complete (a stray file in a stack): for an unchanged copy (0), one its
// owner changed (1), and one holding a link the planner can't check (2).
#[test]
fn a_place_wanting_a_removal_is_held_even_without_a_complete_view() {
    for edit in 0..3 {
        let f = Fixture::new();
        let (a, _) = delivered_once(&f, "");
        let skill = "priv/repo/beta-core/skills/demo";
        f.write(
            &format!("{skill}/SKILL.md"),
            "---\nname: demo\ndescription: the demo skill\n---\nSteps.\n",
        );
        f.write(
            &format!("{skill}/lords.yaml"),
            "id: skill.demo\nkind: skill\ntitle: demo\naudience: [all]\nscope: repo:beta-core\nstatus: active\nvisibility: private\norigin:\n  type: own\n",
        );
        commit(&f, "priv", "a skill");
        import_and_apply(&f.pub_root(), Some(&f.priv_root()), &f.opts()).unwrap();
        assert!(a.join(".claude/skills/demo").is_dir(), "the fixture's skill copy");
        match edit {
            1 => fs::write(a.join(".claude/skills/demo/SKILL.md"), "My own steps.\n").unwrap(),
            2 => common::dir_link(&a.join(".claude/skills/demo/linked"), &f.path("priv")),
            _ => {}
        }
        fs::remove_dir_all(f.path(skill)).unwrap();
        commit(&f, "priv", "the skill gone");
        f.write("pub/global/rules/notes.txt", "not an entry\n");
        rule(&f, "Rule two.\n");
        commit(&f, "pub", "rule two, and a stray file");
        let _ = run(&f);
        assert!(read(&a.join("CLAUDE.local.md")).contains("Rule one."), "edit {edit}: held whole");
        assert!(a.join(".claude/skills/demo").is_dir(), "edit {edit}");
        assert!(held_at(&state(&f), &a).is_some(), "edit {edit}: {:?}", state(&f));
    }
}

// Spec (brief item 3, D23; note C5, A9; settings `lock_wait_secs`): while another writer holds the apply lock, a
// delivery waits at most `lock_wait_secs`, then writes no place and records why, naming the holder, with the command that
// runs it again; once the lock is free, a delivery brings the places current and the failure goes.
#[test]
fn a_delivery_waits_for_a_running_writer_then_records_that_it_could_not() {
    let f = Fixture::new();
    let (a, _) = delivered_once(&f, "[delivery]\nlock_wait_secs = 1\n");
    rule(&f, "Rule two.\n");
    commit(&f, "pub", "rule two");
    let held = ApplyLock::wait(&f.home(), Duration::ZERO).unwrap().expect("the lock is free");
    let start = Instant::now();
    let r = run(&f);
    assert!(start.elapsed() < Duration::from_secs(20), "{:?}", start.elapsed());
    assert!(!read(&a.join("CLAUDE.local.md")).contains("Rule two."), "{r:?}");
    let failed = state(&f).failed.unwrap_or_else(|| panic!("recorded: {r:?}"));
    assert!(failed.why.contains("another apply holds"), "{failed:?}");
    assert_eq!(failed.command, "lords deliver");
    drop(held);

    run(&f).unwrap();
    assert!(files(&a).iter().all(|t| t.contains("Rule two.")));
    assert!(state(&f).failed.is_none(), "{:?}", state(&f));
}

// Spec (D23; note C5, D2: "`import_and_apply` takes the lock before its first plan and holds it to the end"): a sync by
// hand holds the apply lock from its first plan through its apply, so no other writer gets it in between, and its own
// apply still goes through.
#[test]
fn a_sync_by_hand_holds_the_lock_from_its_first_plan_to_its_apply() {
    let f = Fixture::new();
    let (a, _) = delivered_once(&f, "");
    rule(&f, "Rule two.\n");
    commit(&f, "pub", "rule two");
    let mut free = None;
    import_and_apply_between(&f.pub_root(), Some(&f.priv_root()), &f.opts(), || {
        free = Some(ApplyLock::wait(&f.home(), Duration::ZERO).unwrap().is_ok());
    })
    .unwrap();
    assert_eq!(free, Some(false), "another writer got the lock after the first plan");
    assert!(read(&a.join("CLAUDE.local.md")).contains("Rule two."));
}

// Spec (brief item 3, D22: "on a 'ledger changed' outcome it re-plans and retries, bounded"; note A6): a ledger changed
// between a delivery's plan and its apply (by a writer outside the lock) makes it plan again; the second attempt
// delivers, and the change the other writer made stands (here a record of a gone worktree, which the second plan forgets).
#[test]
fn a_ledger_changed_under_the_plan_is_planned_again() {
    let f = Fixture::new();
    let (a, b) = delivered_once(&f, "");
    rule(&f, "Rule two.\n");
    commit(&f, "pub", "rule two");
    let gone = f.path("gone-worktree");
    let r = run_at(&f, Step::Planned, 1, None, || {
        let mut l = Ledger::load(&f.home()).unwrap();
        let o = Owned {
            root: gone.to_string_lossy().into_owned(),
            parts: vec!["CLAUDE.local.md".into()],
            kind: "file".into(),
            sha: "0".repeat(64),
            scope: "repo:beta-core/claude".into(),
        };
        l.owned.insert(key(&gone.join("CLAUDE.local.md")), o);
        l.save(&f.home()).unwrap();
    })
    .unwrap();
    for p in [&a, &b] {
        assert!(files(p).iter().all(|t| t.contains("Rule two.")), "{r:?}");
    }
    assert_eq!(r.attempts, 2, "{r:?}");
    assert!(Ledger::load(&f.home()).unwrap().get(&gone.join("CLAUDE.local.md")).is_none());
}

// Spec (F8 §15.2 A2; review F8-R1-01; note A2): an owner's edit to a place's second file, made after the plan, after
// the place is staged, or after its first file is in, holds that place whole: its first file keeps its old text, the
// owner's edit stays, nothing is left beside them, and the state names the place; the other place is delivered.
#[test]
fn an_owner_edit_during_a_delivery_holds_the_whole_place() {
    for (step, nth) in [(Step::Planned, 1), (Step::Staged, 1), (Step::Swapped, 1)] {
        let f = Fixture::new();
        let (a, b) = delivered_once(&f, "");
        rule(&f, "Rule two.\n");
        commit(&f, "pub", "rule two");
        let claude = read(&a.join("CLAUDE.local.md"));
        let codex = a.join("AGENTS.override.md");
        let mine = format!("{}My own line.\n", read(&codex));
        let place = (step != Step::Planned).then_some(a.as_path());
        let r = run_at(&f, step, nth, place, || fs::write(&codex, &mine).unwrap()).unwrap();
        assert!(files(&b).iter().all(|t| t.contains("Rule two.")), "{step:?}: b is delivered");
        assert_eq!(read(&a.join("CLAUDE.local.md")), claude, "{step:?}: held whole");
        assert_eq!(read(&codex), mine, "{step:?}: the owner's edit stays");
        assert!(leftovers(&a).is_empty(), "{step:?}: {:?}", leftovers(&a));
        assert!(held_at(&r.state, &a).is_some(), "{step:?}: {:?}", r.state);
    }
}

// Spec (sol-loop r2 S5; C4: a place is fully current or untouched): an owner's edit to a file of a place that the
// delivery leaves as it is (an in-sync skill copy), made after the plan, while an earlier place is written, holds that
// place whole when its turn comes: its place files keep their old text, and the edit stays.
#[test]
fn an_edit_to_an_in_sync_file_before_its_place_is_written_holds_the_place() {
    let f = Fixture::new();
    let (a, b) = delivered_once(&f, "");
    let skill = "priv/repo/beta-core/skills/demo";
    f.write(
        &format!("{skill}/SKILL.md"),
        "---\nname: demo\ndescription: the demo skill\n---\nSteps.\n",
    );
    f.write(
        &format!("{skill}/lords.yaml"),
        "id: skill.demo\nkind: skill\ntitle: demo\naudience: [all]\nscope: repo:beta-core\nstatus: active\nvisibility: private\norigin:\n  type: own\n",
    );
    commit(&f, "priv", "a skill");
    import_and_apply(&f.pub_root(), Some(&f.priv_root()), &f.opts()).unwrap();
    let skill_b = b.join(".claude/skills/demo/SKILL.md");
    assert!(skill_b.is_file(), "the fixture's skill copy");
    rule(&f, "Rule two.\n");
    commit(&f, "pub", "rule two");
    let r =
        run_at(&f, Step::Staged, 1, Some(&a), || fs::write(&skill_b, "My own steps.\n").unwrap())
            .unwrap();
    assert!(files(&a).iter().all(|t| t.contains("Rule two.")), "a is delivered: {r:?}");
    assert!(files(&b).iter().all(|t| t.contains("Rule one.")), "b is held whole: {r:?}");
    assert_eq!(read(&skill_b), "My own steps.\n");
    assert!(held_at(&r.state, &b).is_some(), "{:?}", r.state);
}

// Spec (sol-loop r2 S4; K4: only committed text reaches agents): a store file written while the delivery planned, even
// put back as it was (a stack edit whose commit was refused), may have been read uncommitted: the delivery plans again,
// from what the store holds then, and delivers that.
#[test]
fn a_store_file_written_while_the_delivery_planned_is_planned_again() {
    let f = Fixture::new();
    let (a, b) = delivered_once(&f, "");
    rule(&f, "Rule two.\n");
    commit(&f, "pub", "rule two");
    let file = f.path("pub/global/rules/r.md");
    let committed_text = read(&file);
    let r = run_at(&f, Step::Planned, 1, None, || {
        fs::write(&file, "a draft").unwrap();
        fs::write(&file, &committed_text).unwrap();
    })
    .unwrap();
    assert_eq!(r.attempts, 2, "{r:?}");
    for p in [&a, &b] {
        assert!(files(p).iter().all(|t| t.contains("Rule two.")), "{r:?}");
    }
}

/// A whole delivery that runs `act` once, at the first swap at `place` after which exactly `n` of its two place files
/// hold "Rule two.": with 1, one is published and the other still to be replaced (a place's dirs go first, note
/// §14.2); with 2, both are in and the place's backups not yet removed.
fn run_when_new(
    f: &Fixture,
    place: &Path,
    n: usize,
    mut act: impl FnMut(),
) -> Result<Report, String> {
    let mut done = false;
    let opts = Options::whole(&f.home());
    deliver::run_between(&f.pub_root(), Some(&f.priv_root()), &opts, &mut |s, p| {
        let new = || files(place).iter().filter(|t| t.contains("Rule two.")).count();
        if !done && s == Step::Swapped && key(p) == key(place) && new() == n {
            done = true;
            act();
        }
    })
}

/// Opens `p` for reading with read sharing only (`FileShare.Read`): Windows then refuses to rename over it or delete it.
#[cfg(windows)]
fn read_sharing_only(p: &Path) -> std::io::Result<fs::File> {
    use std::os::windows::fs::OpenOptionsExt;
    fs::OpenOptions::new().read(true).share_mode(1).open(p)
}

// Spec (review r1 F8-R1-02, witness P3; note §14.2, T2a, A16): once a place's transaction has published its first file,
// a reader without delete sharing can open neither the published file nor the one still to be replaced, so it can't
// stop the swaps or their undo; the place ends fully current and isn't held.
#[cfg(windows)]
#[test]
fn a_reader_without_delete_sharing_cant_open_a_place_file_during_its_transaction() {
    let f = Fixture::new();
    let (a, b) = delivered_once(&f, "[delivery]\nlock_wait_secs = 0\n");
    rule(&f, "Rule two.\n");
    commit(&f, "pub", "rule two");
    let mut opened = Vec::new();
    let r = run_when_new(&f, &a, 1, || {
        for n in ["CLAUDE.local.md", "AGENTS.override.md"] {
            opened.push((n, read_sharing_only(&a.join(n))));
        }
    })
    .unwrap();
    assert_eq!(opened.len(), 2, "the delivery reached a half-published place");
    for (n, o) in &opened {
        assert!(o.is_err(), "{n}: a reader without delete sharing got in during the transaction");
    }
    drop(opened);
    for p in [&a, &b] {
        assert!(files(p).iter().all(|t| t.contains("Rule two.")), "fully current: {r:?}");
        assert!(leftovers(p).is_empty(), "{:?}", leftovers(p));
    }
    assert!(held_at(&r.state, &a).is_none(), "{:?}", r.state);
}

// Spec (note §14.2, T2b, A16; C4): a reader without delete sharing that holds a place file before the transaction is
// found before anything is written: the place stays untouched at its old text, with nothing beside its files, and is
// held (`write-refused`); once the reader lets go, the next delivery brings it current.
#[cfg(windows)]
#[test]
fn a_reader_there_before_the_transaction_leaves_its_place_untouched() {
    let f = Fixture::new();
    let (a, b) = delivered_once(&f, "[delivery]\nlock_wait_secs = 1\n");
    rule(&f, "Rule two.\n");
    commit(&f, "pub", "rule two");
    let reader = read_sharing_only(&a.join("AGENTS.override.md")).unwrap();
    let start = Instant::now();
    let r = run(&f).unwrap();
    assert!(start.elapsed() < Duration::from_secs(60), "{:?}", start.elapsed());
    assert!(files(&b).iter().all(|t| t.contains("Rule two.")), "{r:?}");
    assert!(files(&a).iter().all(|t| t.contains("Rule one.")), "untouched: {r:?}");
    assert!(leftovers(&a).is_empty(), "{:?}", leftovers(&a));
    let held = held_at(&r.state, &a).unwrap_or_else(|| panic!("held: {:?}", r.state));
    assert!(held.items.iter().any(|i| i.reason == "write-refused"), "{held:?}");
    drop(reader);
    run(&f).unwrap();
    assert!(files(&a).iter().all(|t| t.contains("Rule two.")));
    assert!(leftovers(&a).is_empty(), "{:?}", leftovers(&a));
    assert!(held_at(&state(&f), &a).is_none());
}

// Spec (note §14.2, T2c; C4): a place whose second file's owner edit is found after its first file is published is put
// back whole even while a reader with delete sharing (as the harnesses read) holds the published file open.
#[cfg(windows)]
#[test]
fn a_place_is_put_back_whole_under_a_reader_that_shares_delete() {
    use std::os::windows::fs::OpenOptionsExt;
    let f = Fixture::new();
    let (a, _) = delivered_once(&f, "[delivery]\nlock_wait_secs = 0\n");
    rule(&f, "Rule two.\n");
    commit(&f, "pub", "rule two");
    let before = files(&a);
    let mut reader = Vec::new();
    let mut edited = None;
    let r = run_when_new(&f, &a, 1, || {
        for n in ["CLAUDE.local.md", "AGENTS.override.md"] {
            let p = a.join(n);
            if read(&p).contains("Rule two.") {
                reader.push(fs::OpenOptions::new().read(true).share_mode(7).open(&p).unwrap());
            } else {
                let mine = format!("{}My own line.\n", read(&p));
                fs::write(&p, &mine).unwrap();
                edited = Some((p, mine));
            }
        }
    })
    .unwrap();
    let (p, mine) = edited.expect("one file was still old");
    assert_eq!(read(&p), mine, "the owner's edit stays");
    let other = files(&a).into_iter().find(|t| !t.contains("My own line.")).unwrap();
    assert!(before.contains(&other), "the published file is put back: {other:?}");
    assert!(held_at(&r.state, &a).is_some(), "{:?}", r.state);
    drop(reader);
}

// Spec (F8 §15.2 A1; note A1, D-2: a stop from outside at each step): a delivery stopped after a place is staged, or
// after one or two of its items are in, leaves every place file there, holding the old text or the new; the next
// delivery brings every place current and leaves nothing beside its files.
#[test]
fn a_delivery_stopped_at_any_step_is_finished_by_the_next() {
    for (step, nth) in
        [(Step::Staged, 1), (Step::Swapped, 1), (Step::Swapped, 2), (Step::Staged, 2)]
    {
        let f = Fixture::new();
        let (a, b) = delivered_once(&f, "");
        rule(&f, "Rule two.\n");
        commit(&f, "pub", "rule two");
        let killed = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            run_at(&f, step, nth, None, || panic!("stopped"))
        }));
        for t in files(&a).iter().chain(&files(&b)) {
            assert!(t.contains("Rule one.") || t.contains("Rule two."), "{step:?} {nth}: {t:?}");
        }
        let r = run(&f);
        for p in [&a, &b] {
            assert!(files(p).iter().all(|t| t.contains("Rule two.")), "{step:?} {nth}: {r:?}");
            assert!(leftovers(p).is_empty(), "{step:?} {nth}: {:?}", leftovers(p));
        }
        assert!(killed.is_err(), "{step:?} {nth}: never reached");
    }
}

/// Every swap a whole delivery makes in a fresh fixture set up by `setup`, counted: the same setup makes the same plan.
fn swaps(setup: impl Fn(&Fixture)) -> usize {
    let f = Fixture::new();
    setup(&f);
    let mut n = 0;
    let opts = Options::whole(&f.home());
    deliver::run_between(&f.pub_root(), Some(&f.priv_root()), &opts, &mut |s, _| {
        n += usize::from(s == Step::Swapped);
    })
    .unwrap();
    n
}

/// Delivered once, then the public rule two committed: a whole delivery has every place to change.
fn rule_two(f: &Fixture) -> (PathBuf, PathBuf) {
    let ab = delivered_once(f, "");
    rule(f, "Rule two.\n");
    commit(f, "pub", "rule two");
    ab
}

/// The held reason of `p`'s place in the state, if it is held.
fn reasons(s: &State, p: &Path) -> Vec<String> {
    held_at(s, p).map(|h| h.items.iter().map(|i| i.reason.clone()).collect()).unwrap_or_default()
}

// Spec (review r1 F8-R1-01, witness P1; note §14.1, T1a, T1b, A14; rule.delete-only-what-you-created): a file and a dir
// beside a place file with Lords' names that no transaction recorded are kept byte for byte by a whole delivery, and
// hold their place (`left-beside`), named in the state and in `lords paths`; the other place is delivered.
#[test]
fn an_unrecorded_sibling_with_lords_name_is_kept_and_holds_its_place() {
    let f = Fixture::new();
    let (a, b) = rule_two(&f);
    let file = a.join(".CLAUDE.local.md.lords-done-123-456-1");
    fs::write(&file, "Owner's unique saved text.\n").unwrap();
    let dir = a.join(".AGENTS.override.md.lords-new-123-456-2");
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("keep.txt"), "Owner's dir text.\n").unwrap();

    let r = run(&f).unwrap();
    assert_eq!(read(&file), "Owner's unique saved text.\n", "{r:?}");
    assert_eq!(read(&dir.join("keep.txt")), "Owner's dir text.\n", "{r:?}");
    assert!(files(&b).iter().all(|t| t.contains("Rule two.")), "{r:?}");
    assert!(reasons(&state(&f), &a).contains(&"left-beside".to_string()), "{:?}", state(&f));
    let rows = rows(&f);
    assert!(
        rows.iter().any(|r| r.state == "stale" && r.why.contains("lords-")),
        "{:?}",
        shown(&rows)
    );
}

// Spec (note §14.1, T1c, A15): a sibling a stopped transaction recorded, changed after the stop, is kept byte for byte
// by the next delivery, and holds its place (`left-beside`).
#[test]
fn a_recorded_sibling_changed_after_a_stop_is_kept_and_holds_its_place() {
    let f = Fixture::new();
    let (a, _) = rule_two(&f);
    let stop = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        run_when_new(&f, &a, 2, || panic!("stopped"))
    }));
    assert!(stop.is_err(), "the stop was reached");
    let done = leftovers(&a).into_iter().find(|n| n.starts_with(".CLAUDE.local.md.lords-done-"));
    let done =
        a.join(done.unwrap_or_else(|| panic!("a checked backup is left: {:?}", leftovers(&a))));
    fs::write(&done, "Owner's text, written over the backup.\n").unwrap();

    let r = run(&f).unwrap();
    assert_eq!(read(&done), "Owner's text, written over the backup.\n", "{r:?}");
    assert!(reasons(&state(&f), &a).contains(&"left-beside".to_string()), "{:?}", state(&f));
}

// Spec (review r1 F8-R1-03, witness P2; note §14.3, T3a): with every place in sync, an unchecked backup beside a place
// file holding other text is kept, and holds its place, named in the state and in `lords paths`.
#[test]
fn an_unchecked_backup_holds_its_place_when_nothing_else_changes() {
    let f = Fixture::new();
    let (a, _) = delivered_once(&f, "");
    let old = a.join(".CLAUDE.local.md.lords-old-123-456-1");
    fs::write(&old, "The owner's other text.\n").unwrap();
    let r = run(&f).unwrap();
    assert_eq!(read(&old), "The owner's other text.\n", "{r:?}");
    assert!(reasons(&state(&f), &a).contains(&"left-beside".to_string()), "{:?}", state(&f));
    let rows = rows(&f);
    assert!(rows.iter().any(|r| r.state == "stale"), "{:?}", shown(&rows));
}

// Spec (review r1 F8-R1-03, "a stop after the final successful swap/ledger save leaves checked `done` backups"; note
// §14.3, T3b, A17): a delivery stopped after its last swap leaves its places current with recorded siblings beside; the
// next delivery, with nothing to change, removes exactly those and holds nothing.
#[test]
fn a_stop_after_the_last_swap_is_cleared_by_a_delivery_with_nothing_to_change() {
    let f = Fixture::new();
    let (a, b) = stopped_after_last_swap(&f);
    for p in [&a, &b] {
        assert!(files(p).iter().all(|t| t.contains("Rule two.")), "current after its last swap");
    }
    assert!(!(leftovers(&a).is_empty() && leftovers(&b).is_empty()), "siblings are left");

    let r = run(&f).unwrap();
    for p in [&a, &b] {
        assert!(leftovers(p).is_empty(), "{:?}", leftovers(p));
    }
    assert!(!r.holds() && state(&f).places.is_empty(), "{:?}", state(&f));
}

// Spec (note §14.5, A18): a transaction record that can't be read removes nothing: the whole delivery fails, named in
// the state with the record's path, and the siblings beside the place files stay.
#[test]
fn a_transaction_record_that_cant_be_read_removes_nothing() {
    let f = Fixture::new();
    let (a, _) = rule_two(&f);
    let sibling = a.join(".CLAUDE.local.md.lords-new-123-456-1");
    fs::write(&sibling, "Some bytes.\n").unwrap();
    fs::write(f.home().join("place-transaction.json"), "not a record").unwrap();
    let r = run(&f);
    assert_eq!(read(&sibling), "Some bytes.\n", "{r:?}");
    let failed = state(&f).failed.unwrap_or_else(|| panic!("recorded: {r:?}"));
    assert!(failed.why.contains("place-transaction.json"), "{failed:?}");
}

/// Rule two delivered up to its last swap, then stopped: every place is current, with the transaction's recorded
/// backups (`done`) still beside the files of the last place.
fn stopped_after_last_swap(f: &Fixture) -> (PathBuf, PathBuf) {
    let last = swaps(|f| {
        rule_two(f);
    });
    let ab = rule_two(f);
    let stop = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        run_at(f, Step::Swapped, last, None, || panic!("stopped"))
    }));
    assert!(stop.is_err(), "the last swap ({last}) was reached");
    ab
}

/// The first of `places` with a `done` backup of its `CLAUDE.local.md` beside it, and that backup. Only the Windows
/// reader and writer cases ask (an unused function fails clippy elsewhere).
#[cfg(windows)]
fn a_done_backup(places: [&Path; 2]) -> (PathBuf, PathBuf) {
    for p in places {
        let found =
            leftovers(p).into_iter().find(|n| n.starts_with(".CLAUDE.local.md.lords-done-"));
        if let Some(n) = found {
            return (p.to_path_buf(), p.join(n));
        }
    }
    panic!("no done backup left: {:?} {:?}", leftovers(places[0]), leftovers(places[1]))
}

// Spec (sol-loop r1 S1: "an in-place write between the two is deleted, and for a directory `delete_tree_at` discards
// whatever it now holds"; note §14.7, S1a): an owner's write to a recorded file, and into a recorded dir, made after
// its hash was compared and before its removal, is kept, and its place is held.
#[test]
fn an_edit_between_the_comparison_and_the_removal_is_kept() {
    let f = Fixture::new();
    stopped_after_last_swap(&f);
    let (mut file, mut dir) = (None, None);
    let opts = Options::whole(&f.home());
    let r = deliver::run_between(&f.pub_root(), Some(&f.priv_root()), &opts, &mut |s, p| {
        if s != Step::Compared {
            return;
        }
        let is_dir = p.file_name().is_some_and(|n| n.to_string_lossy().starts_with("..lords."));
        if is_dir && dir.is_none() {
            fs::create_dir_all(p).unwrap();
            fs::write(p.join("owner.txt"), "Owner's file in the dir.\n").unwrap();
            dir = Some(p.to_path_buf());
        } else if !is_dir && file.is_none() {
            fs::write(p, "Owner's edit.\n").unwrap();
            file = Some(p.to_path_buf());
        }
    })
    .unwrap();
    let file = file.unwrap_or_else(|| panic!("a recorded file was compared: {r:?}"));
    let dir = dir.unwrap_or_else(|| panic!("a recorded dir was compared: {r:?}"));
    assert_eq!(read(&file), "Owner's edit.\n", "{r:?}");
    assert_eq!(read(&dir.join("owner.txt")), "Owner's file in the dir.\n", "{r:?}");
    assert!(r.holds(), "{:?}", r.state);
}

// Spec (sol-loop r1 S1: "On Windows that means a handle that shares no write access, then hash, then delete"; note
// §14.7, S1b): a recorded backup that a writer already holds open is kept with its bytes, and its place is held.
#[cfg(windows)]
#[test]
fn a_recorded_backup_a_writer_holds_is_kept() {
    let f = Fixture::new();
    let (a, b) = stopped_after_last_swap(&f);
    let (place, done) = a_done_backup([&a, &b]);
    let bytes = read(&done);
    let writer = fs::OpenOptions::new().write(true).open(&done).unwrap();
    let r = run(&f).unwrap();
    assert_eq!(read(&done), bytes, "kept as it was: {r:?}");
    assert!(reasons(&r.state, &place).contains(&"left-beside".to_string()), "{:?}", r.state);
    drop(writer);
}

// Spec (sol-loop r1 S2: "Recovery must not claim a failed creation"; note §14.7, S2): a dir planted at the name a
// staged dir is about to take, with other content, is kept with its content, and its place is held.
#[test]
fn a_dir_planted_at_a_staged_name_is_kept() {
    let f = Fixture::new();
    let (a, _) = rule_two(&f);
    let mut planted = None;
    let opts = Options::whole(&f.home());
    let r = deliver::run_between(&f.pub_root(), Some(&f.priv_root()), &opts, &mut |s, p| {
        let staged_dir =
            p.file_name().is_some_and(|n| n.to_string_lossy().starts_with("..lords.lords-new-"));
        if s == Step::Creating && planted.is_none() && staged_dir && key(p).starts_with(&key(&a)) {
            fs::create_dir_all(p).unwrap();
            fs::write(p.join("planted.txt"), "Planted, not Lords'.\n").unwrap();
            planted = Some(p.to_path_buf());
        }
    })
    .unwrap();
    let planted =
        planted.unwrap_or_else(|| panic!("a staged dir was about to be made in a: {r:?}"));
    assert_eq!(read(&planted.join("planted.txt")), "Planted, not Lords'.\n", "{r:?}");
    assert!(held_at(&r.state, &a).is_some(), "{:?}", r.state);
    let r = run(&f).unwrap();
    assert_eq!(
        read(&planted.join("planted.txt")),
        "Planted, not Lords'.\n",
        "never claimed: {r:?}"
    );
}

// Spec (sol-loop r1 S3: "An unchanged backup that a reader held temporarily becomes unrecorded forever after the
// reader closes"; note §14.7): a recorded backup a reader holds open without delete sharing is kept while it does,
// and the next delivery after the reader lets go removes it and holds nothing.
#[cfg(windows)]
#[test]
fn a_backup_a_reader_held_is_removed_once_the_reader_lets_go() {
    let f = Fixture::new();
    let (a, b) = stopped_after_last_swap(&f);
    let (place, done) = a_done_backup([&a, &b]);
    let reader = read_sharing_only(&done).unwrap();
    let r = run(&f).unwrap();
    assert!(done.exists(), "kept while read: {r:?}");
    assert!(held_at(&r.state, &place).is_some(), "{:?}", r.state);
    drop(reader);
    let r = run(&f).unwrap();
    for p in [&a, &b] {
        assert!(leftovers(p).is_empty(), "{:?}", leftovers(p));
    }
    assert!(!r.holds(), "{:?}", r.state);
}

// Spec (sol-loop r2 S1: "The final recursive cleanup of `gone` deletes files that an owner's process added inside it
// after the hash"; note §14.8): a file an owner's process adds inside a recorded dir after its comparison, while the
// dir is moved aside, is kept with its bytes, and the place is held.
#[test]
fn a_file_added_inside_a_compared_dir_is_kept() {
    let f = Fixture::new();
    stopped_after_last_swap(&f);
    let mut late = None;
    let opts = Options::whole(&f.home());
    let r = deliver::run_between(&f.pub_root(), Some(&f.priv_root()), &opts, &mut |s, p| {
        let is_dir = p.file_name().is_some_and(|n| n.to_string_lossy().starts_with("..lords."));
        if s != Step::Compared || !is_dir || late.is_some() {
            return;
        }
        // The dir as it is between its comparison and its removal: at `p`, or moved aside beside it.
        let parent = p.parent().unwrap();
        let aside = fs::read_dir(parent)
            .unwrap()
            .map(|e| e.unwrap().path())
            .find(|q| {
                q.file_name()
                    .is_some_and(|n| n.to_string_lossy().starts_with("..lords.lords-gone-"))
            })
            .unwrap_or_else(|| p.to_path_buf());
        fs::write(aside.join("late.txt"), "Late owner file.\n").unwrap();
        late = Some(p.to_path_buf());
    });
    let p = late.unwrap_or_else(|| panic!("a recorded dir was compared: {r:?}"));
    let kept: Vec<PathBuf> = fs::read_dir(p.parent().unwrap())
        .unwrap()
        .map(|e| e.unwrap().path().join("late.txt"))
        .filter(|q| q.is_file())
        .collect();
    assert_eq!(kept.len(), 1, "the late file is kept once: {kept:?} {r:?}");
    assert_eq!(read(&kept[0]), "Late owner file.\n");
    if let Ok(r) = &r {
        assert!(r.holds(), "{r:?}");
    }
}

/// A whole delivery that publishes one of `a`'s two place files, then finds an owner's line in the other, so the place
/// is put back; `at` runs once, at the undo's comparison of the published file (`Step::Compared` with its path). The
/// published file's path, and the delivery's answer.
fn undo_compared(
    f: &Fixture,
    a: &Path,
    mut at: impl FnMut(&Path),
) -> (PathBuf, Result<Report, String>) {
    let (mut published, mut done) = (None::<PathBuf>, false);
    let opts = Options::whole(&f.home());
    let r = deliver::run_between(&f.pub_root(), Some(&f.priv_root()), &opts, &mut |s, p| {
        let names = ["CLAUDE.local.md", "AGENTS.override.md"].map(|n| a.join(n));
        if s == Step::Swapped && published.is_none() && key(p) == key(a) {
            let new: Vec<&PathBuf> =
                names.iter().filter(|n| read(n).contains("Rule two.")).collect();
            if let [one] = new[..] {
                // The other file, still old, gets its owner's line: its swap is refused and the place is put back.
                let old = names.iter().find(|n| *n != one).unwrap();
                fs::write(old, format!("{}My own line.\n", read(old))).unwrap();
                published = Some(one.clone());
            }
        } else if s == Step::Compared
            && !done
            && published.as_ref().is_some_and(|q| key(p) == key(q))
        {
            at(p);
            done = true;
        }
    });
    assert!(done, "the undo compared the published file: {r:?}");
    (published.expect("a place was half published"), r)
}

// Spec (sol-loop r2 S4, r3 S4 and S5: "an owner write that creates `p` between the two is lost"; "file rollback leaves
// `p` missing for a moment"; note §14.9): while a place is put back, its published file's path always names a file,
// and an owner's write to it after the undo compared it is refused or kept, never lost.
#[test]
fn an_owner_edit_between_the_undo_comparison_and_its_put_back_is_kept() {
    let f = Fixture::new();
    let (a, _) = rule_two(&f);
    let (mut there, mut refused) = (false, false);
    let (published, r) = undo_compared(&f, &a, |p| {
        there = p.is_file();
        refused = fs::write(p, "Owner's edit after the comparison.\n").is_err();
    });
    assert!(there, "the published file's path names a file throughout the put-back: {r:?}");
    let kept = fs::read_dir(a.as_path())
        .unwrap()
        .map(|e| e.unwrap().path())
        .any(|q| read(&q) == "Owner's edit after the comparison.\n");
    assert!(refused || kept, "the owner's edit is refused or kept: {:?} {r:?}", read(&published));
}

// Spec (sol-loop r3 S6: "every restore-rename error is treated as occupied. Other errors can leave `p` absent while the
// compared copy gets deleted"; note §14.9): if the backup named for the old version goes at the undo's comparison, the
// published file's path still names a file afterwards, and the place is either put back (its old text) or the delivery
// reports that it couldn't be.
#[test]
fn a_put_back_that_fails_leaves_the_path_named() {
    let f = Fixture::new();
    let (a, _) = rule_two(&f);
    let (published, r) = undo_compared(&f, &a, |p| {
        // The old version's backup, which the undo would put back, goes.
        let name = p.file_name().unwrap().to_string_lossy().into_owned();
        for e in fs::read_dir(p.parent().unwrap()).unwrap() {
            let q = e.unwrap().path();
            if q.file_name()
                .is_some_and(|n| n.to_string_lossy().starts_with(&format!(".{name}.lords-done-")))
            {
                fs::remove_file(&q).unwrap();
            }
        }
    });
    assert!(published.is_file(), "the path still names a file: {r:?}");
    let reported = match &r {
        Ok(rep) => rep.state.failed.is_some(),
        Err(_) => true,
    };
    let put_back = read(&published).contains("Rule one.");
    assert!(put_back || reported, "put back, or reported as not: {:?} {r:?}", read(&published));
}

// Spec (F8 §15, M4; review F8-R1-03; note A5): once a store's hooks are installed, delivery is configured, so a state
// file that is missing reads stale in `lords paths`, never as "not set up".
#[test]
fn a_configured_delivery_without_its_state_file_is_stale() {
    let f = Fixture::new();
    delivered_once(&f, "");
    let exe = Path::new("C:/lords/lords.exe");
    lords_core::hooks::install(&f.priv_root(), exe, true, false, &f.home()).unwrap();
    let file = deliver::state_file(&f.home(), &f.stacks().delivery()).unwrap();
    let _ = fs::remove_file(file);
    let rows = rows(&f);
    assert!(rows.iter().any(|r| r.state == "stale"), "{:?}", shown(&rows));
    assert!(rows.iter().all(|r| r.state != "complete"), "{:?}", shown(&rows));
}

/// Repo `beta-core` with its checkout `a` and worktrees under `wts`, the public rule; synced by hand once. `more` goes at
/// the end of the private stack.toml.
fn with_worktrees(f: &Fixture, more: &str) -> PathBuf {
    rule(f, "Rule one.\n");
    let a = f.path("a");
    f.write("a/README.md", "a place\n");
    f.git_init("a");
    fs::create_dir_all(f.path("wts")).unwrap();
    f.write("pub/stack.toml", &format!("name = \"pub\"\nvisibility = \"public\"\n{ROLES}"));
    f.write(
        "priv/stack.toml",
        &format!(
            "name = \"priv\"\nvisibility = \"private\"\n{ROLES}[projects.beta]\n[repos.beta-core]\nproject = \"beta\"\ndefault_role = \"coder\"\ncheckouts = ['{}']\nworktree_roots = ['{}']\n{more}",
            a.display(),
            f.path("wts").display()
        ),
    );
    f.git_init("pub");
    f.git_init("priv");
    import_and_apply(&f.pub_root(), Some(&f.priv_root()), &f.opts()).unwrap();
    assert!(read(&a.join("CLAUDE.local.md")).contains("Rule one."), "the fixture's place file");
    a
}

fn add_worktree(a: &Path, wt: &Path, branch: &str) {
    git(a, &["worktree", "add", "-q", "-b", branch, wt.to_str().unwrap()]);
}

// Spec (brief item 2; note C3, D-4): a new worktree's own delivery (its post-checkout hook's) writes its place files
// through the safe-only path, its creates safe because it is the new worktree; the whole-machine delivery keeps D13's D4
// and holds a worktree no hook delivered (`new-place`), writing nothing there, with the sync by hand that resolves it.
#[test]
fn a_new_worktree_is_delivered_by_its_own_safe_only_run() {
    let f = Fixture::new();
    let a = with_worktrees(&f, "");
    let one = f.path("wts/one");
    add_worktree(&a, &one, "one");
    let o = Options::worktree(&f.home(), "beta-core", &one);
    let r = deliver::run(&f.pub_root(), Some(&f.priv_root()), &o).unwrap();
    assert!(files(&one).iter().all(|t| t.contains("Rule one.")), "{r:?}");
    assert!(one.join(".lords/start-coder.md").is_file(), "{r:?}");
    assert!(held_at(&r.state, &one).is_none(), "{:?}", r.state);

    let two = f.path("wts/two");
    add_worktree(&a, &two, "two");
    let r = run(&f).unwrap();
    assert!(!two.join("CLAUDE.local.md").exists(), "{r:?}");
    let s = state(&f);
    let held = held_at(&s, &two).unwrap_or_else(|| panic!("two is held: {s:?}"));
    assert!(held.items.iter().any(|i| i.reason == "new-place"), "{held:?}");
    assert!(held.command.starts_with("lords sync --repo beta-core --place "), "{held:?}");
}

// Spec (D22, the post-checkout log of 7 Nov: a worktree git didn't list yet, or listed before it was a checkout; note
// C5, A7; settings `attempts`, `retry_pause_secs`): a new worktree's delivery that finds its place not yet a worktree of
// its repo tries again after the pause; once it is one, its files come. One that never becomes one is held after the
// last attempt (`not-ready`), with the command that delivers it, and nothing is written.
#[test]
fn a_new_worktree_not_ready_is_tried_again_then_held() {
    let f = Fixture::new();
    let a = with_worktrees(&f, "[delivery]\nattempts = 2\nretry_pause_secs = 0\n");
    let two = f.path("wts/two");
    let o = Options::worktree(&f.home(), "beta-core", &two);
    let r = run_opts_at(&f, &o, Step::Planned, 1, None, || add_worktree(&a, &two, "two")).unwrap();
    assert!(files(&two).iter().all(|t| t.contains("Rule one.")), "{r:?}");
    assert_eq!(r.attempts, 2, "{r:?}");
    assert!(held_at(&r.state, &two).is_none(), "{:?}", r.state);

    let three = f.path("wts/three");
    let o = Options::worktree(&f.home(), "beta-core", &three);
    let r = deliver::run(&f.pub_root(), Some(&f.priv_root()), &o).unwrap();
    assert_eq!(r.attempts, 2, "{r:?}");
    assert!(!three.exists(), "nothing written");
    let s = state(&f);
    let held = held_at(&s, &three).unwrap_or_else(|| panic!("three is held: {s:?}"));
    assert!(held.items.iter().any(|i| i.reason == "not-ready"), "{held:?}");
    assert!(
        held.command.starts_with("lords deliver --repo beta-core --place ")
            && held.command.ends_with(" --new-worktree"),
        "{held:?}"
    );
}

// Spec (note A8: "a worktree outside `worktree_roots`: nothing is written and nothing is held"): a worktree of the repo
// that lies outside its worktree roots is skipped on purpose, as a full sync skips it. A baseline beside the not-ready
// case, which must not take it for one.
#[test]
fn a_worktree_outside_its_roots_is_left_and_not_held() {
    let f = Fixture::new();
    let a = with_worktrees(&f, "");
    let out = f.path("elsewhere/x");
    add_worktree(&a, &out, "x");
    let o = Options::worktree(&f.home(), "beta-core", &out);
    let r = deliver::run(&f.pub_root(), Some(&f.priv_root()), &o).unwrap();
    assert!(!out.join("CLAUDE.local.md").exists(), "{r:?}");
    assert!(held_at(&r.state, &out).is_none(), "{:?}", r.state);
    assert!(r.attempts <= 1, "{r:?}");
}

// Spec (brief item 5; note C7, D-1, A13; D6's import half): a whole delivery imports a memory store's new note first,
// as its own commit in the private store, which is clean after it. When the store's commit hook refuses that commit, the
// import's files are put back, so the store is clean again; the delivery still delivers the committed stacks, and the
// state lists the import as held store work.
#[test]
fn store_work_is_its_own_commit_and_a_refused_one_is_put_back() {
    let f = Fixture::new();
    let mem = f.path("memory");
    fs::create_dir_all(&mem).unwrap();
    // A note before the first sync, so its import commits the store's defaults file with it: an import of no note writes
    // that file and commits nothing, which leaves the store uncommitted (older than F8).
    fs::write(mem.join("note-zero.md"), "---\nname: note zero\n---\nA first note.\n").unwrap();
    let store = format!(
        "[[memory_stores]]\nname = \"agent\"\npath = '{}'\nscope = \"global\"\n",
        mem.display()
    );
    let (a, _) = delivered_once(&f, &store);
    let clean =
        |f: &Fixture| git(&f.priv_root(), &["status", "--porcelain", "--untracked-files=all"]);

    fs::write(mem.join("note-one.md"), "---\nname: note one\n---\nA note.\n").unwrap();
    rule(&f, "Rule two.\n");
    commit(&f, "pub", "rule two");
    let r = run(&f).unwrap();
    let log = git(&f.priv_root(), &["log", "--format=%s"]);
    assert!(log.contains("Import memory from agent"), "{log}");
    assert_eq!(clean(&f), "", "{r:?}");
    assert!(read(&a.join("CLAUDE.local.md")).contains("Rule two."));
    assert!(r.state.store_work.is_empty(), "{:?}", r.state);

    f.write("priv/.git/hooks/pre-commit", "#!/bin/sh\nexit 1\n");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let refuse = f.path("priv/.git/hooks/pre-commit");
        fs::set_permissions(refuse, fs::Permissions::from_mode(0o755)).unwrap();
    }
    fs::write(mem.join("note-two.md"), "---\nname: note two\n---\nAnother note.\n").unwrap();
    rule(&f, "Rule three.\n");
    commit(&f, "pub", "rule three");
    let r = run(&f).unwrap();
    assert_eq!(clean(&f), "", "put back: {r:?}");
    assert!(read(&a.join("CLAUDE.local.md")).contains("Rule three."), "{r:?}");
    assert!(r.state.store_work.iter().any(|w| w.what.contains("agent")), "{:?}", r.state);
}

// Spec (sol-loop r1 S1; C7; D6): a refused import puts back only what it wrote: a copy someone edited while the store's
// commit hook ran (here the hook itself, as an owner would) keeps the edit, and the delivery's state says it was left.
#[test]
fn a_refused_import_leaves_an_edit_made_during_its_commit() {
    let f = Fixture::new();
    let mem = f.path("memory");
    fs::create_dir_all(&mem).unwrap();
    fs::write(mem.join("note-zero.md"), "---\nname: note zero\n---\nA first note.\n").unwrap();
    let store = format!(
        "[[memory_stores]]\nname = \"agent\"\npath = '{}'\nscope = \"global\"\n",
        mem.display()
    );
    delivered_once(&f, &store);
    f.write(
        "priv/.git/hooks/pre-commit",
        "#!/bin/sh\necho 'Mine.' >> global/memory/agent/note-two.md\nexit 1\n",
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let refuse = f.path("priv/.git/hooks/pre-commit");
        fs::set_permissions(refuse, fs::Permissions::from_mode(0o755)).unwrap();
    }
    fs::write(mem.join("note-two.md"), "---\nname: note two\n---\nAnother note.\n").unwrap();
    rule(&f, "Rule two.\n");
    commit(&f, "pub", "rule two");
    let r = run(&f);
    let copy = f.path("priv/global/memory/agent/note-two.md");
    assert!(read(&copy).contains("Mine."), "the edit stays: {r:?}");
    let s = format!("{:?}", state(&f));
    assert!(s.contains("left as it is"), "{s}");
}

// Spec (sol-loop r1 S8; C7): a memory store the delivery can't read is held store work, with the command that imports
// it, never left out of the state.
#[test]
fn a_memory_store_that_cant_be_read_is_held_store_work() {
    let f = Fixture::new();
    let gone = f.path("no-such-memory");
    let store = format!(
        "[[memory_stores]]\nname = \"gone\"\npath = '{}'\nscope = \"global\"\n",
        gone.display()
    );
    delivered_once(&f, "");
    let toml = format!("{}{store}", fs::read_to_string(f.path("priv/stack.toml")).unwrap());
    f.write("priv/stack.toml", &toml);
    commit(&f, "priv", "a store that isn't there");
    let r = run(&f).unwrap();
    let held = r.state.store_work.iter().find(|w| w.what.contains("memory import"));
    let held = held.unwrap_or_else(|| panic!("held: {:?}", r.state));
    assert_eq!(held.command, "lords import", "{held:?}");
}

// Spec (sol-loop r1 S6; C3: "each declared repo … gets Lords' post-checkout hook"): a repo declared with two independent
// clones gets a hook in each: the plan lists one per git common dir, and a declared worktree of one of them adds none.
#[test]
fn every_clone_of_a_repo_gets_its_post_checkout() {
    let f = Fixture::new();
    rule(&f, "Rule one.\n");
    for c in ["a", "b"] {
        f.write(&format!("{c}/README.md"), "a place\n");
        f.git_init(c);
    }
    git(&f.path("a"), &["worktree", "add", "-q", "../a-wt", "-b", "wt"]);
    f.write("pub/stack.toml", &format!("name = \"pub\"\nvisibility = \"public\"\n{ROLES}"));
    let list: Vec<String> =
        ["a", "b", "a-wt"].iter().map(|c| format!("'{}'", f.path(c).display())).collect();
    f.write(
        "priv/stack.toml",
        &format!(
            "name = \"priv\"\nvisibility = \"private\"\n{ROLES}[projects.beta]\n[repos.beta-core]\nproject = \"beta\"\ncheckouts = [{}]\n",
            list.join(", ")
        ),
    );
    let p = lords_core::sync::plan(&f.stacks(), &f.opts()).unwrap();
    let at: Vec<String> = p.hooks.iter().map(|h| key(&h.checkout)).collect();
    assert_eq!(at, [key(&f.path("a")), key(&f.path("b"))], "{:?}", p.hooks);
}

// Spec (brief item 6, D24: "the stack load refuses two targets whose paths are the same or nested"): two targets at one
// path, or one inside the other, fail the load naming both; siblings load.
#[test]
fn two_targets_at_one_path_or_one_inside_another_dont_load() {
    for (second, kind, ok) in [
        ("x/skills", "skills", false),
        ("x/skills/inner", "skills", false),
        ("x", "skills", false),
        ("x/CLAUDE.md", "instructions", true),
    ] {
        let f = Fixture::new();
        let target = |name: &str, kind: &str, rel: &str| {
            format!(
                "[[targets]]\nname = \"{name}\"\nadapter = \"claude\"\nkind = \"{kind}\"\npath = '{}'\n",
                f.path(rel).display()
            )
        };
        f.write(
            "priv/stack.toml",
            &format!(
                "name = \"priv\"\nvisibility = \"private\"\n{}{}",
                target("first", "skills", "x/skills"),
                target("second", kind, second)
            ),
        );
        let loaded = Stacks::load(&f.pub_root(), Some(&f.priv_root()));
        match (ok, loaded) {
            (true, l) => assert!(l.is_ok(), "{second}: {:?}", l.err()),
            (false, Ok(_)) => panic!("{second}: loaded"),
            (false, Err(e)) => {
                assert!(e.contains("first") && e.contains("second"), "{second}: {e}")
            }
        }
    }
}

// Spec (brief item 7; note §5): `[delivery]` has a default for every key, a stack's value replaces it, and the state file
// stays inside the tool home: a path that leaves it, is absolute or isn't plain parts fails the load, as does a value
// the schema refuses.
#[test]
fn the_delivery_settings_have_defaults_and_keep_the_state_inside_the_home() {
    let f = Fixture::new();
    let d = f.stacks().delivery();
    let all = (d.lock_wait_secs, d.attempts, d.retry_pause_secs, d.deadline_secs, d.store_work);
    assert_eq!(all, (120, 3, 2, 600, true));
    // The limits review r1 found fixed (note §14.4): settings, with the values they had as defaults.
    assert_eq!((d.store_git_secs, d.state_lock_secs), (30, 10));
    assert_eq!(
        (d.state.as_str(), &d.on_commit[..]),
        ("state/delivery.json", &[Store::Public, Store::Private][..])
    );
    let manifest = |delivery: &str| {
        f.write(
            "priv/stack.toml",
            &format!("name = \"priv\"\nvisibility = \"private\"\n[delivery]\n{delivery}"),
        );
    };
    manifest("lock_wait_secs = 5\non_commit = [\"private\"]\nstate = \"state/mine.json\"\n");
    let d = f.stacks().delivery();
    assert_eq!((d.lock_wait_secs, d.attempts, &d.on_commit[..]), (5, 3, &[Store::Private][..]));
    assert!(deliver::state_file(&f.home(), &d).unwrap().ends_with("state/mine.json"));
    for bad in [
        "state = \"../outside.json\"",
        "state = \"state/../../x.json\"",
        "state = \"/abs.json\"",
        "state = 'state\\x.json'",
        "attempts = 0",
        "on_commit = [\"other\"]",
    ] {
        manifest(&format!("{bad}\n"));
        assert!(Stacks::load(&f.pub_root(), Some(&f.priv_root())).is_err(), "{bad}");
    }
}
