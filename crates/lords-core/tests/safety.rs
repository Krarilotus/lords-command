//! D13, typed safety (design note Harness/notes/2026-11-07-lords-d13-typed-safety.md): the planner types each item of
//! a plan `safe`, `safe-forget` or `needs-owner` with a reason, and `Stage::SafeOnly` keeps only what is safe, holding
//! every place where anything else is wanted. The cases are the ones `sync-all-v2.sh` and `forget_check.py` guarded by
//! hand, and the attacks of the note's §6.

// Tests are outside the filesystem boundary (fsguard.rs): their fixtures write their own temp dirs.
#![allow(clippy::disallowed_methods, clippy::disallowed_types)]

mod common;

use common::{entry, git, Fixture};
use lords_core::ledger::{self, Ledger};
use lords_core::sync::{apply, plan, Action, Hold, Options, Plan, Safety, Stage};
use std::fs;
use std::path::{Path, PathBuf};

const ROLES: &str = "roles = [\"coder\", \"reviewer\"]\n";
const ENDED: &str = "temporary:\n  until: 2000-01-01\n  review_by: 2999-12-31\n  reason: a trial\n";

/// The public rule `rule.r`, for both roles, so every place file carries it.
fn rule(f: &Fixture, body: &str) {
    let roles = "roles: [coder, reviewer]\n";
    f.write("pub/global/rules/r.md", &entry("rule.r", "global", "public", roles, body));
}

/// The private stack: repo `beta-core` with these checkouts (no discovered worktrees), and `more` after it.
fn manifest(f: &Fixture, checkouts: &[&Path], more: &str) {
    let list: Vec<String> = checkouts.iter().map(|c| format!("'{}'", c.display())).collect();
    f.write(
        "priv/stack.toml",
        &format!(
            "name = \"priv\"\nvisibility = \"private\"\n{ROLES}[projects.beta]\n[repos.beta-core]\nproject = \"beta\"\ndefault_role = \"coder\"\ncheckouts = [{}]\nworktrees = false\n{more}",
            list.join(", ")
        ),
    );
}

/// A git checkout `name` in the fixture, with one file.
fn checkout(f: &Fixture, name: &str) -> PathBuf {
    f.write(&format!("{name}/README.md"), "a place\n");
    f.git_init(name);
    f.path(name)
}

/// Two checkouts `a` and `b` of repo `beta-core`, with a public rule, synced once by hand.
fn synced(f: &Fixture) -> (PathBuf, PathBuf) {
    rule(f, "Rule one.\n");
    let (a, b) = (checkout(f, "a"), checkout(f, "b"));
    f.write("pub/stack.toml", &format!("name = \"pub\"\nvisibility = \"public\"\n{ROLES}"));
    manifest(f, &[&a, &b], "");
    sync_by_hand(f);
    for file in ["CLAUDE.local.md", "AGENTS.override.md"] {
        assert!(read(&a.join(file)).contains("Rule one."), "the fixture's place file: {file}");
    }
    (a, b)
}

/// A full plan, applied: what `lords sync --apply` writes when nothing is imported or retired.
fn sync_by_hand(f: &Fixture) {
    apply(&plan(&f.stacks(), &f.opts()).unwrap()).unwrap();
}

/// The repo's own skill `demo`, which sync copies into each checkout (Claude's skills dir); its dir in the stack.
fn skill(f: &Fixture) -> &'static str {
    let dir = "priv/repo/beta-core/skills/demo";
    f.write(
        &format!("{dir}/SKILL.md"),
        "---\nname: demo\ndescription: the demo skill\n---\nSteps.\n",
    );
    f.write(
        &format!("{dir}/lords.yaml"),
        "id: skill.demo\nkind: skill\ntitle: demo\naudience: [all]\nscope: repo:beta-core\nstatus: active\nvisibility: private\norigin:\n  type: own\n",
    );
    dir
}

/// A live memory store whose index is missing: a conflict of no place.
fn dead_index(f: &Fixture) -> String {
    fs::create_dir_all(f.path("live-memory")).unwrap();
    format!(
        "[[memory_stores]]\nname = \"live\"\npath = '{}'\nscope = \"global\"\nlive = true\n",
        f.path("live-memory").display()
    )
}

fn full(f: &Fixture) -> Plan {
    plan(&f.stacks(), &f.opts()).unwrap()
}

fn safe_only(f: &Fixture) -> Plan {
    plan(&f.stacks(), &Options { stage: Stage::SafeOnly, ..f.opts() }).unwrap()
}

/// Every item the planner made has a class it gave it (A9).
fn checked(p: &Plan) {
    let missed: Vec<_> = p
        .items
        .iter()
        .filter(|i| i.safety == Safety::NeedsOwner(Hold::Unclassified))
        .map(|i| (i.path.display().to_string(), i.action.clone()))
        .collect();
    assert!(missed.is_empty(), "items the planner left unclassified: {missed:?}");
}

fn item<'a>(p: &'a Plan, path: &Path) -> &'a lords_core::sync::Item {
    let all =
        || p.items.iter().map(|i| (i.path.display().to_string(), i.safety)).collect::<Vec<_>>();
    let i = p.items.iter().find(|i| ledger::key(&i.path) == ledger::key(path));
    i.unwrap_or_else(|| panic!("no item for {}: {:?}", path.display(), all()))
}

fn class(p: &Plan, path: &Path) -> Safety {
    item(p, path).safety
}

fn action(p: &Plan, path: &Path) -> Action {
    item(p, path).action.clone()
}

/// An item of the plan's JSON, by its path.
fn json_item(p: &Plan, path: &Path) -> serde_json::Value {
    let json = serde_json::to_value(p).unwrap();
    let same = |i: &&serde_json::Value| {
        ledger::key(Path::new(i["path"].as_str().unwrap_or_default())) == ledger::key(path)
    };
    json["items"].as_array().unwrap().iter().find(same).cloned().unwrap()
}

/// The keys of the places a safe-only plan held whole, sorted.
fn held(p: &Plan) -> Vec<String> {
    let h = p.held.as_ref().expect("a safe-only plan says what it held");
    let mut keys: Vec<String> = h.places.iter().map(|x| x.place.key()).collect();
    keys.sort();
    keys
}

/// The reason a safe-only plan gave for holding `path`, if it held it in a place.
fn held_for(p: &Plan, path: &Path) -> Option<Hold> {
    let h = p.held.as_ref()?;
    let mut items = h.places.iter().flat_map(|x| x.items.iter());
    items.find(|i| ledger::key(&i.path) == ledger::key(path)).map(|i| i.reason)
}

/// Whether a safe-only plan still holds an item below `dir`.
fn keeps_below(p: &Plan, dir: &Path) -> bool {
    let d = format!("{}/", ledger::key(dir));
    p.items.iter().any(|i| ledger::key(&i.path).starts_with(&d))
}

fn recorded_below(f: &Fixture, dir: &Path) -> usize {
    let d = format!("{}/", ledger::key(dir));
    Ledger::load(&f.home()).unwrap().owned.keys().filter(|k| k.starts_with(&d)).count()
}

fn read(p: &Path) -> String {
    fs::read_to_string(p).unwrap_or_default()
}

fn needs(h: Hold) -> Safety {
    Safety::NeedsOwner(h)
}

// ---------------------------------------------------------------- the class of each kind of item (note §4's table)

// Spec (note C2): a tool-owned file or dir whose disk content still matches its ledger record, updated or refreshed,
// is safe; `sync --json` shows the same class on each item, and its place.
#[test]
fn an_update_of_an_unchanged_copy_is_safe_and_the_json_says_so() {
    let f = Fixture::new();
    let (a, _) = synced(&f);
    rule(&f, "Rule two.\n");
    let p = full(&f);
    assert_eq!(action(&p, &a.join("CLAUDE.local.md")), Action::Update);
    assert_eq!(class(&p, &a.join("CLAUDE.local.md")), Safety::Safe);
    assert_eq!(action(&p, &a.join(".lords")), Action::Refresh);
    assert_eq!(class(&p, &a.join(".lords")), Safety::Safe);
    let i = json_item(&p, &a.join("CLAUDE.local.md"));
    assert_eq!(i["safety"], serde_json::json!({"class": "safe"}));
    let places = i["places"].as_array().unwrap();
    assert_eq!(places.len(), 1, "{i}");
    assert_eq!(ledger::key(Path::new(places[0].as_str().unwrap())), ledger::key(&a));
    checked(&p);
}

// Spec (note C4, owner-file; brief: "owner files (settings, the user CLAUDE.md, ~/.codex/AGENTS.md, git excludes)"): a
// wanted edit of a checkout's git exclude or of a user-level instruction file's block needs the owner.
#[test]
fn an_owner_file_edit_needs_the_owner() {
    let f = Fixture::new();
    let (a, b) = synced(&f);
    fs::write(a.join(".git/info/exclude"), "").unwrap();
    let mine = f.write("home/.claude/CLAUDE.md", "My own line.\n");
    // A rule for every role, which a user-level file composes (it takes no rule for only some roles), so the file wants
    // its block.
    f.write(
        "pub/global/rules/u.md",
        &entry("rule.u", "global", "public", "", "Rule for everyone.\n"),
    );
    let target = format!(
        "[[targets]]\nname = \"user\"\nadapter = \"claude\"\nkind = \"instructions\"\npath = '{}'\n",
        mine.display()
    );
    manifest(&f, &[&a, &b], &target);
    let p = full(&f);
    assert_eq!(class(&p, &a.join(".git/info/exclude")), needs(Hold::OwnerFile));
    assert_eq!(action(&p, &mine), Action::Update);
    assert_eq!(class(&p, &mine), needs(Hold::OwnerFile));
    let i = json_item(&p, &mine);
    assert_eq!(i["safety"], serde_json::json!({"class": "needs-owner", "reason": "owner-file"}));
    assert_eq!(i["places"], serde_json::json!(["target user"]));
    checked(&p);
}

// Spec (note C2, D4: "a create of a tool-owned file in a place Lords already manages"): a new skill's copy in a checkout
// where the ledger records Lords' paths is safe; the files of a declared checkout where it records none need the owner,
// and a file there that isn't the tool's is a conflict.
#[test]
fn a_create_is_safe_only_where_lords_already_manages_the_place() {
    let f = Fixture::new();
    let (a, b) = synced(&f);
    skill(&f);
    let c = checkout(&f, "c");
    f.write("c/AGENTS.override.md", "Someone's own file.\n");
    manifest(&f, &[&a, &b, &c], "");
    let p = full(&f);
    assert_eq!(action(&p, &a.join(".claude/skills/demo")), Action::Install);
    assert_eq!(class(&p, &a.join(".claude/skills/demo")), Safety::Safe);
    assert_eq!(action(&p, &c.join("CLAUDE.local.md")), Action::Create);
    assert_eq!(class(&p, &c.join("CLAUDE.local.md")), needs(Hold::NewPlace));
    assert_eq!(class(&p, &c.join(".lords")), needs(Hold::NewPlace));
    assert_eq!(action(&p, &c.join("AGENTS.override.md")), Action::Conflict);
    assert_eq!(class(&p, &c.join("AGENTS.override.md")), needs(Hold::Conflict));
    checked(&p);
}

// Spec (note C3, C4; `forget_check.py`: a forget is fine only where its place no longer exists): the records of a
// checkout that is gone, dropped from the stack, are safe forgets; a copy deleted by hand in a place that is still
// there is held; a copy its owner changed, and an unchanged one, of a skill the stack dropped, need the owner.
#[test]
fn a_forget_is_safe_only_where_its_place_is_gone() {
    let f = Fixture::new();
    let (a, b) = synced(&f);
    let (c, d) = (checkout(&f, "c"), checkout(&f, "d"));
    let dir = skill(&f);
    manifest(&f, &[&a, &b, &c, &d], "");
    sync_by_hand(&f);
    assert!(c.join(".claude/skills/demo").is_dir(), "the fixture's copies");

    fs::remove_dir_all(f.path(dir)).unwrap();
    fs::remove_dir_all(a.join(".claude/skills/demo")).unwrap();
    fs::write(b.join(".claude/skills/demo/SKILL.md"), "My own steps.\n").unwrap();
    manifest(&f, &[&a, &b, &d], "");
    fs::remove_dir_all(&c).unwrap();
    let p = full(&f);
    assert_eq!(class(&p, &c.join("CLAUDE.local.md")), Safety::SafeForget);
    assert_eq!(class(&p, &c.join(".lords")), Safety::SafeForget);
    assert_eq!(class(&p, &a.join(".claude/skills/demo")), needs(Hold::PlaceStillThere));
    assert_eq!(action(&p, &b.join(".claude/skills/demo")), Action::Forget);
    assert_eq!(class(&p, &b.join(".claude/skills/demo")), needs(Hold::ChangedCopy));
    assert_eq!(action(&p, &d.join(".claude/skills/demo")), Action::Remove);
    assert_eq!(class(&p, &d.join(".claude/skills/demo")), needs(Hold::Removal));
    checked(&p);
}

// Spec (note C4, unchecked; F8 sol-loop r3 S4): a tool-owned copy the stacks no longer want that the planner can't
// check (a link inside it) is reported, not changed, and needs the owner.
#[test]
fn a_copy_the_planner_cant_check_needs_the_owner() {
    let f = Fixture::new();
    let (a, _) = synced(&f);
    let dir = skill(&f);
    sync_by_hand(&f);
    common::dir_link(&a.join(".claude/skills/demo/linked"), &f.path("priv"));
    fs::remove_dir_all(f.path(dir)).unwrap();
    let p = full(&f);
    assert_eq!(action(&p, &a.join(".claude/skills/demo")), Action::Report);
    assert_eq!(class(&p, &a.join(".claude/skills/demo")), needs(Hold::Unchecked));
    checked(&p);
}

// Spec (note C4: changed copies, conflicts; A9, the class is set after every mutation): a copy its owner changed needs
// the owner; a place file that grows past its harness's limit, which the planner made as an update and then turned into
// a conflict, is held as over the limit, not as the update it was.
#[test]
fn conflicts_need_the_owner_by_their_kind() {
    let f = Fixture::new();
    let (a, b) = synced(&f);
    fs::write(a.join("CLAUDE.local.md"), "My own text.\n").unwrap();
    let big = format!("{}\n", "x".repeat(40_000));
    let roles = "roles: [coder, reviewer]\n";
    let e = entry("rule.big", "repo:beta-core", "private", roles, &big);
    f.write("priv/repo/beta-core/rules/big.md", &e);
    let p = full(&f);
    assert_eq!(class(&p, &a.join("CLAUDE.local.md")), needs(Hold::ChangedCopy));
    assert_eq!(action(&p, &b.join("CLAUDE.local.md")), Action::Update, "Claude reads it whole");
    assert_eq!(action(&p, &b.join("AGENTS.override.md")), Action::Conflict);
    assert_eq!(class(&p, &b.join("AGENTS.override.md")), needs(Hold::OverLimit));
    checked(&p);
}

// Spec (note C4: retirements; §4, a conflict of no place): an ended temporary entry is store work that needs the owner,
// and a live memory store whose index is missing is a conflict that belongs to no place.
#[test]
fn store_work_and_a_dead_memory_index_need_the_owner() {
    let f = Fixture::new();
    let (a, b) = synced(&f);
    let old = f.write(
        "priv/global/rules/old.md",
        &entry("rule.old", "global", "private", ENDED, "Old.\n"),
    );
    manifest(&f, &[&a, &b], &dead_index(&f));
    let p = full(&f);
    assert_eq!(action(&p, &old), Action::RetireEntry);
    assert_eq!(class(&p, &old), needs(Hold::Retirement));
    let dead = f.path("live-memory/MEMORY.md");
    assert_eq!(class(&p, &dead), needs(Hold::Conflict));
    assert!(item(&p, &dead).places.is_empty(), "{:?}", item(&p, &dead).places);
    checked(&p);
}

// ---------------------------------------------------------------- Stage::SafeOnly, place by place (note §6)

// Spec (A2, A10; F8 K3): a checkout whose CLAUDE.local.md its owner changed is held whole: its AGENTS.override.md and
// `.lords` stay as they are, though alone they'd be safe; the other checkout gets the change. The conflict is made with
// its own path as its root, yet it holds its checkout.
#[test]
fn an_owner_changed_copy_holds_its_place_whole() {
    let f = Fixture::new();
    let (a, b) = synced(&f);
    let mine = format!("{}My own line.\n", read(&a.join("CLAUDE.local.md")));
    fs::write(a.join("CLAUDE.local.md"), &mine).unwrap();
    let codex = read(&a.join("AGENTS.override.md"));
    let role = read(&a.join(".lords/role-coder.md"));
    rule(&f, "Rule two.\n");
    let p = safe_only(&f);
    assert_eq!(held(&p), [ledger::key(&a)]);
    let h = &p.held.as_ref().unwrap().places[0];
    assert_eq!(h.items.len(), 1, "{:?}", h.items);
    assert_eq!(h.items[0].reason, Hold::ChangedCopy);
    assert!(!keeps_below(&p, &a), "nothing of the held place is kept");
    assert!(p.held.as_ref().unwrap().holds());

    apply(&p).unwrap();
    assert_eq!(read(&a.join("CLAUDE.local.md")), mine, "the owner's copy is never overwritten");
    assert_eq!(read(&a.join("AGENTS.override.md")), codex, "its place is held whole");
    assert_eq!(read(&a.join(".lords/role-coder.md")), role);
    for file in ["CLAUDE.local.md", "AGENTS.override.md"] {
        assert!(read(&b.join(file)).contains("Rule two."), "the other place: {file}");
    }
}

// Spec (A4; F8 sol-loop r1 S2, r2 S3, r3 S4): with a stray file in a stack, the view isn't complete and a full plan
// lists no removal; a safe-only plan holds every place that wants one all the same, whether its copy is unchanged (0),
// changed by its owner (1) or can't be checked (2). Its place file isn't updated.
#[test]
fn a_place_wanting_a_removal_is_held_even_without_a_complete_view() {
    for edit in 0..3 {
        let f = Fixture::new();
        let (a, _) = synced(&f);
        let dir = skill(&f);
        sync_by_hand(&f);
        match edit {
            1 => fs::write(a.join(".claude/skills/demo/SKILL.md"), "My own steps.\n").unwrap(),
            2 => common::dir_link(&a.join(".claude/skills/demo/linked"), &f.path("priv")),
            _ => {}
        }
        fs::remove_dir_all(f.path(dir)).unwrap();
        f.write("pub/global/rules/notes.txt", "not an entry\n");
        rule(&f, "Rule two.\n");
        assert!(full(&f).complete.is_none(), "edit {edit}: the fixture's view isn't complete");
        let p = safe_only(&f);
        assert!(held(&p).contains(&ledger::key(&a)), "edit {edit}: {:?}", held(&p));
        assert!(!keeps_below(&p, &a), "edit {edit}");
        apply(&p).unwrap();
        assert!(read(&a.join("CLAUDE.local.md")).contains("Rule one."), "edit {edit}");
        assert!(a.join(".claude/skills/demo").is_dir(), "edit {edit}");
    }
}

// Spec (A5; `forget_check.py`, `sync-all-v2.sh`): the records of a checkout that is gone are forgotten and nothing is
// held; a record whose path is gone in a place that is still there holds its place and stays; a forget planned without
// a complete view is held too, and nothing is forgotten.
#[test]
fn a_forget_applies_only_where_its_place_is_gone() {
    let f = Fixture::new();
    let (a, b) = synced(&f);
    let dir = skill(&f);
    sync_by_hand(&f);
    manifest(&f, &[&a], "");
    fs::remove_dir_all(&b).unwrap();
    let p = safe_only(&f);
    assert!(held(&p).is_empty(), "{:?}", held(&p));
    assert!(!p.held.as_ref().unwrap().holds());
    assert!(recorded_below(&f, &b) > 0);
    apply(&p).unwrap();
    assert_eq!(recorded_below(&f, &b), 0, "the gone place's records are forgotten");

    fs::remove_dir_all(f.path(dir)).unwrap();
    fs::remove_dir_all(a.join(".claude/skills/demo")).unwrap();
    let p = safe_only(&f);
    assert_eq!(held(&p), [ledger::key(&a)]);
    assert_eq!(held_for(&p, &a.join(".claude/skills/demo")), Some(Hold::PlaceStillThere));
    let before = recorded_below(&f, &a);
    apply(&p).unwrap();
    assert_eq!(recorded_below(&f, &a), before, "the record stays while its place is there");

    let f = Fixture::new();
    let (a, b) = synced(&f);
    manifest(&f, &[&a], "");
    fs::remove_dir_all(&b).unwrap();
    f.write("pub/global/rules/notes.txt", "not an entry\n");
    let p = safe_only(&f);
    assert_eq!(held_for(&p, &b.join("CLAUDE.local.md")), Some(Hold::IncompleteView));
    apply(&p).unwrap();
    assert!(recorded_below(&f, &b) > 0, "nothing forgotten without a complete view");
}

// Spec (A6): a new skill reaches every checkout Lords manages; a declared checkout with no record is held, and nothing
// is created there.
#[test]
fn a_create_reaches_managed_places_and_a_new_place_is_held() {
    let f = Fixture::new();
    let (a, b) = synced(&f);
    skill(&f);
    let c = checkout(&f, "c");
    manifest(&f, &[&a, &b, &c], "");
    let p = safe_only(&f);
    assert_eq!(held(&p), [ledger::key(&c)]);
    apply(&p).unwrap();
    assert!(a.join(".claude/skills/demo/SKILL.md").is_file());
    assert!(b.join(".claude/skills/demo/SKILL.md").is_file());
    assert!(!c.join("CLAUDE.local.md").exists() && !c.join(".lords").exists());
}

// Spec (A7; F8 K3: "A conflict of no place holds everything"): a live memory store whose index is missing holds every
// place: nothing is kept, and nothing is written.
#[test]
fn a_conflict_of_no_place_holds_everything() {
    let f = Fixture::new();
    let (a, b) = synced(&f);
    manifest(&f, &[&a, &b], &dead_index(&f));
    rule(&f, "Rule two.\n");
    let p = safe_only(&f);
    let h = p.held.as_ref().unwrap();
    assert_eq!(h.everything.len(), 1, "{:?}", h.everything);
    assert!(h.holds());
    assert!(p.items.is_empty(), "{:?}", p.items.iter().map(|i| &i.path).collect::<Vec<_>>());
    apply(&p).unwrap();
    assert!(read(&a.join("CLAUDE.local.md")).contains("Rule one."));
}

// Spec (A8; the Lords owner, 7 Nov: "Memory imports and retirements are listed separately as pending store work, and
// `--safe-only` never does them"): an ended entry and a memory store with a new note are listed, hold no place, and
// stay undone: no commit lands in the store, while the places get their change.
#[test]
fn store_work_is_listed_and_never_done() {
    let f = Fixture::new();
    let (a, b) = synced(&f);
    let old = f.write(
        "priv/global/rules/old.md",
        &entry("rule.old", "global", "private", ENDED, "Old.\n"),
    );
    let mem = f.path("agent-memory");
    f.write("agent-memory/note.md", "---\nname: note\ndescription: a note\n---\nThe note.\n");
    let store = format!(
        "[[memory_stores]]\nname = \"agent\"\npath = '{}'\nscope = \"global\"\n",
        mem.display()
    );
    manifest(&f, &[&a, &b], &store);
    f.git_init("priv");
    let head = git(&f.priv_root(), &["rev-parse", "HEAD"]);
    rule(&f, "Rule two.\n");
    let p = safe_only(&f);
    let h = p.held.as_ref().unwrap();
    assert!(!h.holds(), "store work holds no place: {:?}", h.places);
    assert_eq!(h.retirements.len(), 1, "{:?}", h.retirements);
    assert_eq!(ledger::key(&h.retirements[0].path), ledger::key(&old));
    assert_eq!(p.imports.len(), 1, "{:?}", p.imports);
    assert_eq!((p.imports[0].store.as_str(), p.imports[0].added), ("agent", 1));
    assert!(!p.items.iter().any(|i| i.action == Action::RetireEntry));
    apply(&p).unwrap();
    assert!(read(&a.join("CLAUDE.local.md")).contains("Rule two."));
    assert_eq!(git(&f.priv_root(), &["rev-parse", "HEAD"]), head, "no commit in the store");
    assert_eq!(git(&f.priv_root(), &["status", "--porcelain"]), "", "nothing written in the store");
}

/// Every `[safe_only]` setting a stack has accepted, so a test can say what holds under each one a stack still accepts.
const SETTINGS: [&str; 3] =
    ["", "[safe_only]\napplies = [\"new-place\"]\n", "[safe_only]\napplies = [\"exclude\"]\n"];

/// The private stack of `shared_checkouts`: checkout `m`, worktrees under `m/.wt`, and `more`.
fn shared_manifest(f: &Fixture, m: &Path, more: &str) {
    f.write(
        "priv/stack.toml",
        &format!(
            "name = \"priv\"\nvisibility = \"private\"\n{ROLES}[projects.beta]\n[repos.beta-core]\nproject = \"beta\"\ndefault_role = \"coder\"\ncheckouts = ['{}']\nworktree_roots = ['{}']\n{more}",
            m.display(),
            m.join(".wt").display()
        ),
    );
}

/// Checkout `m` of repo `beta-core` and its linked worktree `m/.wt/n`, which share one git exclude, with a public rule
/// and `more` in the private stack.toml, synced once by hand; `None` when the stack refuses `more`.
fn shared_checkouts(f: &Fixture, more: &str) -> Option<(PathBuf, PathBuf)> {
    rule(f, "Rule one.\n");
    f.write("pub/stack.toml", &format!("name = \"pub\"\nvisibility = \"public\"\n{ROLES}"));
    let m = checkout(f, "m");
    git(&m, &["worktree", "add", "-q", ".wt/n", "-b", "n"]);
    let n = m.join(".wt/n");
    shared_manifest(f, &m, more);
    lords_core::Stacks::load(&f.pub_root(), Some(&f.priv_root())).ok()?;
    sync_by_hand(f);
    assert!(read(&n.join("CLAUDE.local.md")).contains("Rule one."), "the fixture's worktree");
    Some((m, n))
}

// Spec (review r1, R1-01; the Lords owner, 7 Nov 17:33: "A shared owner-file write (the common exclude) is then always
// needs-owner, so it holds every place it lists"): under every setting a stack accepts, a safe-only plan never writes
// the git exclude a checkout shares with its linked worktree. While an edit of it is wanted and the main checkout's
// place file is its owner's, the worktree's files and the exclude stay as they were, and both places are held.
#[test]
fn a_shared_exclude_is_never_written_and_holds_every_reader() {
    for setting in SETTINGS {
        let f = Fixture::new();
        let Some((m, n)) = shared_checkouts(&f, setting) else {
            assert!(setting.contains("exclude"), "a setting refused: {setting:?}");
            continue;
        };
        let exclude = m.join(".git/info/exclude");
        fs::write(&exclude, "# Owner line\n").unwrap();
        fs::write(m.join("CLAUDE.local.md"), "My owner text.\n").unwrap();
        let files =
            ["CLAUDE.local.md", "AGENTS.override.md", ".lords/role-coder.md"].map(|x| n.join(x));
        let before = files.clone().map(|p| read(&p));
        rule(&f, "Rule two.\n");
        let p = safe_only(&f);
        apply(&p).unwrap();
        assert_eq!(files.clone().map(|p| read(&p)), before, "{setting:?}: the worktree's files");
        assert_eq!(read(&exclude), "# Owner line\n", "{setting:?}: the shared exclude");
        let mut both = vec![ledger::key(&m), ledger::key(&n)];
        both.sort();
        assert_eq!(held(&p), both, "{setting:?}");
    }
}

// Spec (A1, A11): the git exclude a linked worktree shares with its main checkout holds both when its edit is wanted;
// a worktree inside the main checkout's dir is a place of its own, so an owner-changed file in the main checkout
// doesn't hold it.
#[test]
fn a_shared_exclude_holds_every_checkout_reading_it_and_a_nested_worktree_is_its_own_place() {
    let f = Fixture::new();
    let (m, n) = shared_checkouts(&f, "").unwrap();

    let exclude = m.join(".git/info/exclude");
    let lines = read(&exclude);
    fs::write(&exclude, "").unwrap();
    rule(&f, "Rule two.\n");
    let p = safe_only(&f);
    let mut both = vec![ledger::key(&m), ledger::key(&n)];
    both.sort();
    assert_eq!(held(&p), both);
    let h = p.held.as_ref().unwrap();
    let reasons: Vec<Hold> =
        h.places.iter().flat_map(|x| x.items.iter().map(|i| i.reason)).collect();
    assert!(reasons.iter().all(|r| *r == Hold::OwnerFile), "{reasons:?}");

    fs::write(&exclude, lines).unwrap();
    fs::write(m.join("CLAUDE.local.md"), "My own text.\n").unwrap();
    let p = safe_only(&f);
    assert_eq!(held(&p), [ledger::key(&m)]);
    apply(&p).unwrap();
    assert!(
        read(&n.join("CLAUDE.local.md")).contains("Rule two."),
        "the nested worktree is delivered"
    );
    assert_eq!(read(&m.join("CLAUDE.local.md")), "My own text.\n");
}

// Spec (A13: an owner edit after planning is caught by apply's own checks; F8's C4, which closes this note's §8
// residual "an apply refused part way can leave a place half written"): a safe-only plan applied after the owner
// changed one of its files holds that file's place whole, put back as it was (`write-refused`), and the owner's text
// stays; the other place is written. Before F8 the whole apply refused here.
#[test]
fn an_edit_after_planning_holds_its_place_and_is_kept() {
    let f = Fixture::new();
    let (a, b) = synced(&f);
    rule(&f, "Rule two.\n");
    let p = safe_only(&f);
    assert!(keeps_below(&p, &b));
    fs::write(b.join("CLAUDE.local.md"), "Edited after the plan.\n").unwrap();
    let applied = apply(&p).unwrap();
    assert_eq!(read(&b.join("CLAUDE.local.md")), "Edited after the plan.\n");
    assert!(read(&b.join("AGENTS.override.md")).contains("Rule one."), "held whole");
    assert!(read(&a.join("CLAUDE.local.md")).contains("Rule two."), "the other place is written");
    let held = |h: &lords_core::sync::HeldPlace| {
        matches!(&h.place, lords_core::sync::PlaceId::Dir(d) if ledger::key(d) == ledger::key(&b))
            && h.items.iter().all(|i| i.reason == Hold::WriteRefused)
    };
    assert!(applied.held.iter().any(held), "{:?}", applied.held);
}

// Spec (the owner, 7 Nov 17:16, intent item 10: "make thigns more configurable, no harded coded shit that will bite
// later and we would need to adjust manually again"; note §11b; the Lords owner, 7 Nov 17:33, after review r1: "Only
// `new-place` remains, and `exclude` now fails the stack's load, with a message"): what `--safe-only` applies besides
// what is always safe is the stack's `[safe_only] applies`, by default nothing more, and "new-place" is the one value:
// a new worktree of a checkout Lords manages, whose shared exclude already holds sync's lines, gets its files with no
// one looking. "exclude", a destination other places share, is refused when the stack loads, naming the value and
// what may be named, and so is any other value.
#[test]
fn the_stack_names_what_safe_only_also_applies() {
    let f = Fixture::new();
    let (m, _) = shared_checkouts(&f, "[safe_only]\napplies = [\"new-place\"]\n").unwrap();
    git(&m, &["worktree", "add", "-q", ".wt/o", "-b", "o"]);
    let o = m.join(".wt/o");
    assert_eq!(class(&full(&f), &o.join("CLAUDE.local.md")), Safety::Safe);
    let p = safe_only(&f);
    apply(&p).unwrap();
    assert!(read(&o.join("CLAUDE.local.md")).contains("Rule one."));
    assert!(held(&p).is_empty(), "{:?}", held(&p));

    for value in ["exclude", "removal"] {
        shared_manifest(&f, &m, &format!("[safe_only]\napplies = [\"{value}\"]\n"));
        let s = lords_core::Stacks::load(&f.pub_root(), Some(&f.priv_root()));
        let e = s.err().unwrap_or_else(|| panic!("`{value}` is accepted"));
        assert!(e.contains(value) && e.contains("new-place"), "{e}");
    }
}

// Spec (note D3; sol-loop r1 S1): a place is held whole only where the plan sees all of it, so the planner itself
// refuses a safe-only plan of one adapter's files, whoever calls it: otherwise a checkout whose Codex file its owner
// changed would get Claude's file and `.lords` updated.
#[test]
fn a_safe_only_plan_of_part_of_a_place_is_refused() {
    let f = Fixture::new();
    let (a, _) = synced(&f);
    fs::write(a.join("AGENTS.override.md"), "My own text.\n").unwrap();
    rule(&f, "Rule two.\n");
    let opts = Options { stage: Stage::SafeOnly, adapter: Some("claude".into()), ..f.opts() };
    let p = plan(&f.stacks(), &opts);
    let kept = p.as_ref().map(|p| p.items.iter().map(|i| i.path.clone()).collect::<Vec<_>>());
    assert!(p.as_ref().is_err_and(|e| e.contains("adapter")), "{kept:?}");
}
