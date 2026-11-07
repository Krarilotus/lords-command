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

// Spec (F8 §15.2 A3, A4; note A3, A4): when a reader holds a place's second file open so it can't be replaced, the swap
// is tried again until `lock_wait_secs`, then the place's first file is put back, so the place stays whole at its old
// text and the state holds it (`write-refused`); once the reader lets go, the next delivery brings it current, with
// nothing left beside its files.
#[cfg(windows)]
#[test]
fn a_file_a_reader_holds_open_leaves_its_place_whole() {
    use std::os::windows::fs::OpenOptionsExt;
    let f = Fixture::new();
    let (a, b) = delivered_once(&f, "[delivery]\nlock_wait_secs = 1\n");
    rule(&f, "Rule two.\n");
    commit(&f, "pub", "rule two");
    let codex = a.join("AGENTS.override.md");
    let mut open = None;
    let start = Instant::now();
    let r = run_at(&f, Step::Swapped, 1, Some(&a), || {
        // Read sharing only: the file can't be replaced or deleted while this is open.
        open = Some(fs::OpenOptions::new().read(true).share_mode(1).open(&codex).unwrap());
    });
    assert!(files(&b).iter().all(|t| t.contains("Rule two.")), "{r:?}");
    assert!(files(&a).iter().all(|t| t.contains("Rule one.")), "whole at its old text: {r:?}");
    assert!(start.elapsed() < Duration::from_secs(60), "{:?}", start.elapsed());
    let s = state(&f);
    let held = held_at(&s, &a).unwrap_or_else(|| panic!("held: {s:?}"));
    assert!(held.items.iter().any(|i| i.reason == "write-refused"), "{held:?}");
    drop(open);
    run(&f).unwrap();
    assert!(files(&a).iter().all(|t| t.contains("Rule two.")));
    assert!(leftovers(&a).is_empty(), "{:?}", leftovers(&a));
    assert!(held_at(&state(&f), &a).is_none());
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
