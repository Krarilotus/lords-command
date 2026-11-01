//! `lords uninstall --repo NAME --place DIR` (a project's worktree retire, PI-13, as the Lords owner accepted it;
//! design note `2026-11-01-lords-uninstall-place.md` Q2/Q3): the place's recorded copies whose identity and bytes
//! still match the ledger go, each with its record; an instruction file holding the owner's text too loses Lords'
//! block only; a changed copy stays with its record; a record already gone is dropped; nothing of another place,
//! nothing tracked; a dir that is no declared place of the repo is refused.

// Tests are outside the filesystem boundary (fsguard.rs): their fixtures write their own temp dirs.
#![allow(clippy::disallowed_methods, clippy::disallowed_types)]

mod common;
use common::*;
use lords_core::ledger::{self, Ledger};
use lords_core::sync::{apply, plan, Action, Item, Options, Plan, Stage};
use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

/// A repo with a main checkout, two worktrees under `wt-root` and one nested inside the checkout (`checkout/.wt/n`),
/// all synced: each holds `CLAUDE.local.md`, `.lords` and the development skill `demo`.
fn synced(f: &Fixture) -> [PathBuf; 4] {
    f.write(
        "priv/repo/beta-core/facts/h.md",
        &entry("fact.h", "repo:beta-core", "private", "", "Hosts.\n"),
    );
    f.write("pub/global/skills/demo/SKILL.md", "---\nname: demo\ndescription: demo\n---\nSteps.\n");
    f.write(
        "pub/global/skills/demo/lords.yaml",
        "id: skill.demo\nkind: skill\ntitle: demo\naudience: [all]\nroles: [coder]\nscope: global\nstatus: active\nvisibility: public\norigin:\n  type: own\n",
    );
    let main = f.path("checkout");
    f.write("checkout/README.md", "a repo\n");
    f.git_init("checkout");
    git(&main, &["worktree", "add", "-q", "../wt-root/a", "-b", "a"]);
    git(&main, &["worktree", "add", "-q", "../wt-root/b", "-b", "b"]);
    git(&main, &["worktree", "add", "-q", ".wt/n", "-b", "n"]);
    f.write(
        "priv/stack.toml",
        &format!(
            "name = 'priv'\nvisibility = 'private'\nroles = ['coder']\n[projects.beta]\n[repos.beta-core]\nproject = 'beta'\nadapters = ['claude']\ncheckouts = ['{}']\nworktree_roots = ['{}', '{}']\n",
            main.display(),
            f.path("wt-root").display(),
            f.path("checkout/.wt").display()
        ),
    );
    apply(&plan(&f.stacks(), &Options { repo: Some("beta-core".into()), ..f.opts() }).unwrap())
        .unwrap();
    let places = [main.clone(), f.path("wt-root/a"), f.path("wt-root/b"), f.path("checkout/.wt/n")];
    for p in &places {
        for c in copies(p) {
            assert!(c.exists() && recorded(f, &c), "synced: {}", c.display());
        }
    }
    places
}

/// Lords' copies in a place.
fn copies(place: &Path) -> [PathBuf; 3] {
    [place.join("CLAUDE.local.md"), place.join(".lords"), place.join(".claude/skills/demo")]
}

fn uninstall(f: &Fixture, place: &Path) -> Result<Plan, String> {
    plan(
        &f.stacks(),
        &Options {
            repo: Some("beta-core".into()),
            place: Some(place.to_path_buf()),
            uninstall: true,
            ..f.opts()
        },
    )
}

fn recorded(f: &Fixture, path: &Path) -> bool {
    let key = ledger::key(path);
    Ledger::load(&f.home()).unwrap().owned.keys().any(|k| *k == key)
}

fn revision(f: &Fixture) -> String {
    Ledger::load(&f.home()).unwrap().revision()
}

/// Paths as the ledger compares them (separators, case on Windows), so a test doesn't depend on the spelling git
/// gave a worktree's root.
fn keys<'a>(paths: impl IntoIterator<Item = &'a Path>) -> BTreeSet<String> {
    paths.into_iter().map(ledger::key).collect()
}

fn item<'a>(p: &'a Plan, path: &Path) -> &'a Item {
    p.items
        .iter()
        .find(|i| ledger::key(&i.path) == ledger::key(path))
        .unwrap_or_else(|| panic!("no item for {}: {:?}", path.display(), p.items))
}

// Spec (Q2 "Removed", Q3 "What must hold" 2 and 4; PI-13: "removes the place's recorded copies whose identity and
// bytes still match the ledger … Each removal drops its ledger record"): one worktree's three unchanged copies are
// planned as removals and nothing else is; applied, they go with their records, and the main checkout's, the sibling
// worktree's and the nested worktree's stay with theirs. Planned again, nothing of the place remains (exit 0's
// condition).
#[test]
fn only_the_places_own_unchanged_copies_go() {
    let f = Fixture::new();
    let [main, a, b, nested] = synced(&f);
    let p = uninstall(&f, &a).unwrap();
    assert_eq!(
        keys(p.items.iter().map(|i| i.path.as_path())),
        keys(copies(&a).iter().map(|c| c.as_path()))
    );
    assert!(p.items.iter().all(|i| i.action == Action::Remove), "{:?}", p.items);
    apply(&p).unwrap();
    for c in copies(&a) {
        assert!(!c.exists() && !recorded(&f, &c), "removed: {}", c.display());
    }
    for place in [&main, &b, &nested] {
        for c in copies(place) {
            assert!(c.exists() && recorded(&f, &c), "untouched: {}", c.display());
        }
    }
    assert!(uninstall(&f, &a).unwrap().items.is_empty(), "nothing of the place remains");
}

// Spec (Q3 T4: "a worktree nested inside <tree>: its records have another root: untouched"): the main checkout's
// uninstall leaves the worktree inside it, copies and records.
#[test]
fn a_worktree_nested_in_the_place_is_another_place() {
    let f = Fixture::new();
    let [main, _, _, nested] = synced(&f);
    apply(&uninstall(&f, &main).unwrap()).unwrap();
    assert!(copies(&main).iter().all(|c| !c.exists()));
    for c in copies(&nested) {
        assert!(c.exists() && recorded(&f, &c), "untouched: {}", c.display());
    }
}

// Spec (Q2 "Kept, named, exit 1: a changed copy, with its record"; PI-13: "a changed copy, which it names"): `.lords`
// with an edited role file, and the instruction file with an edit inside Lords' block, are each kept with a note
// saying it changed, with their records; the unchanged skill goes. Planned again, exactly those two remain, kept.
#[test]
fn a_changed_copy_is_kept_and_named_with_its_record() {
    let f = Fixture::new();
    let [_, a, _, _] = synced(&f);
    let role = a.join(".lords/role-coder.md");
    fs::write(&role, "OWNER EDIT\n").unwrap();
    let file = a.join("CLAUDE.local.md");
    let text = fs::read_to_string(&file).unwrap();
    // A line inserted after the begin marker: inside the block, so no block-only removal either.
    fs::write(&file, text.replacen('\n', "\nmine, inside the block\n", 1)).unwrap();
    let p = uninstall(&f, &a).unwrap();
    for c in [a.join(".lords"), file.clone()] {
        let i = item(&p, &c);
        assert_eq!(i.action, Action::Report, "{i:?}");
        assert!(i.notes.iter().any(|n| n.contains("changed")), "{:?}", i.notes);
    }
    assert_eq!(item(&p, &a.join(".claude/skills/demo")).action, Action::Remove);
    apply(&p).unwrap();
    assert_eq!(fs::read_to_string(&role).unwrap(), "OWNER EDIT\n");
    assert!(fs::read_to_string(&file).unwrap().contains("mine, inside the block"));
    assert!(recorded(&f, &a.join(".lords")) && recorded(&f, &file), "their records stay");
    assert!(
        !a.join(".claude/skills/demo").exists() && !recorded(&f, &a.join(".claude/skills/demo"))
    );
    let left = uninstall(&f, &a).unwrap();
    assert_eq!(
        keys(left.items.iter().map(|i| i.path.as_path())),
        keys([a.join(".lords").as_path(), file.as_path()])
    );
    assert!(left.items.iter().all(|i| i.action == Action::Report), "{:?}", left.items);
}

// Spec (Q2: "An instruction file that holds text outside Lords' block, with the block's bytes hashing to the record,
// loses the block only; every other byte stays. Each removal drops its record"): owner text before the block (CRLF
// lines) and after it (no final newline); the block goes, the file then holds exactly the owner's bytes, and its
// record is gone. Planned again, nothing of the place remains.
#[test]
fn a_block_in_a_file_with_the_owners_text_goes_alone() {
    let f = Fixture::new();
    let [_, a, _, _] = synced(&f);
    let file = a.join("CLAUDE.local.md");
    let lords = fs::read_to_string(&file).unwrap();
    let (before, after) = ("# Mine\r\nkept as it is\r\n", "\nmore of mine, no newline at the end");
    fs::write(&file, format!("{before}{lords}{after}")).unwrap();
    let p = uninstall(&f, &a).unwrap();
    assert_eq!(item(&p, &file).action, Action::RemoveBlock, "{:?}", item(&p, &file));
    apply(&p).unwrap();
    assert_eq!(fs::read(&file).unwrap(), format!("{before}{after}").into_bytes());
    assert!(!recorded(&f, &file), "its record goes");
    assert!(uninstall(&f, &a).unwrap().items.is_empty(), "nothing of the place remains");
}

// Spec (Q5 4, the trusted boundary called directly; Q3 T7: "what the file holds at the swap … the item goes back"):
// a block removal whose file the owner changes after the plan was made changes nothing in that file, and its record
// stays.
#[test]
fn a_block_removal_whose_file_changed_since_the_plan_changes_nothing() {
    let f = Fixture::new();
    let [_, a, _, _] = synced(&f);
    let file = a.join("CLAUDE.local.md");
    let lords = fs::read_to_string(&file).unwrap();
    fs::write(&file, format!("# Mine\n{lords}")).unwrap();
    let p = uninstall(&f, &a).unwrap();
    assert_eq!(item(&p, &file).action, Action::RemoveBlock);
    let edited = format!("# Mine, edited after the plan\n{lords}");
    fs::write(&file, &edited).unwrap();
    assert!(apply(&p).is_err());
    assert_eq!(fs::read_to_string(&file).unwrap(), edited);
    assert!(recorded(&f, &file), "its record stays");
}

// Spec (Q2 "Already gone: a record whose path is gone is dropped"; "Exit 0 … no record of the place remains"): with
// `.lords` deleted by hand, its record is planned as a forget, nothing is kept, and after apply the record is gone
// and nothing of the place remains.
#[test]
fn a_record_whose_copy_is_gone_is_dropped() {
    let f = Fixture::new();
    let [_, a, _, _] = synced(&f);
    fs::remove_dir_all(a.join(".lords")).unwrap();
    let p = uninstall(&f, &a).unwrap();
    assert_eq!(item(&p, &a.join(".lords")).action, Action::Forget);
    assert!(p.items.iter().all(|i| i.action != Action::Report), "{:?}", p.items);
    apply(&p).unwrap();
    assert!(!recorded(&f, &a.join(".lords")));
    assert!(uninstall(&f, &a).unwrap().items.is_empty(), "nothing of the place remains");
}

// Spec (Q2 "Safety: <tree> must be a configured checkout of NAME or a worktree of it under worktree_roots"; Q3 T3):
// a subdir of a worktree, a plain dir under `worktree_roots`, another repo's checkout there, the worktree root dir
// itself and a missing path are each refused, and nothing changes: the ledger's revision and every copy stay.
#[test]
fn a_tree_that_is_no_place_of_the_repo_is_refused() {
    let f = Fixture::new();
    let [main, a, _, _] = synced(&f);
    fs::create_dir_all(a.join("sub")).unwrap();
    fs::create_dir_all(f.path("wt-root/plain")).unwrap();
    f.write("wt-root/other/README.md", "another repo\n");
    f.git_init("wt-root/other");
    let before = revision(&f);
    for dir in [
        a.join("sub"),
        f.path("wt-root/plain"),
        f.path("wt-root/other"),
        f.path("wt-root"),
        f.path("wt-root/missing"),
    ] {
        let r = uninstall(&f, &dir);
        assert!(r.is_err(), "{}: {:?}", dir.display(), r.map(|p| p.items));
    }
    assert_eq!(revision(&f), before);
    for c in copies(&a).iter().chain(copies(&main).iter()) {
        assert!(c.exists(), "{}", c.display());
    }
}

// Spec (Q2 "Safety: <tree> and each record's root are compared by identity, not spelling (D4)"; Q5 2): the same
// worktree named with a trailing separator, through `.` and `..`, and (Windows) in upper case plans the same three
// removals as its plain spelling.
#[test]
fn a_place_is_found_whatever_its_spelling() {
    let f = Fixture::new();
    let [_, a, _, _] = synced(&f);
    let want = keys(copies(&a).iter().map(|c| c.as_path()));
    let mut spellings = vec![
        PathBuf::from(format!("{}/", a.display())),
        f.path("wt-root/b/../a"),
        f.path("wt-root/./a"),
    ];
    if cfg!(windows) {
        spellings.push(PathBuf::from(a.to_string_lossy().to_uppercase()));
    }
    for s in spellings {
        let p = uninstall(&f, &s).unwrap_or_else(|e| panic!("{}: {e}", s.display()));
        assert_eq!(keys(p.items.iter().map(|i| i.path.as_path())), want, "{}", s.display());
    }
}

// Spec (Q3 T2, review r2 UP-R2-01's witness: two configured checkouts `WT` and `wt` in a case-sensitive dir, records
// only in `WT`; uninstalling `wt` removed `WT`'s copies through the case-folded key): `wt` holds no record, so its
// plan has nothing, and `WT`'s copies and records stay.
#[cfg(windows)]
#[test]
fn case_distinct_dirs_are_two_places() {
    let f = Fixture::new();
    fs::create_dir_all(f.path("cs")).unwrap();
    let out = std::process::Command::new("fsutil")
        .args(["file", "setCaseSensitiveInfo"])
        .arg(f.path("cs"))
        .arg("enable")
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stdout));
    f.write(
        "priv/repo/beta-core/facts/h.md",
        &entry("fact.h", "repo:beta-core", "private", "", "Hosts.\n"),
    );
    let (upper, lower) = (f.path("cs/WT"), f.path("cs/wt"));
    f.write("cs/WT/README.md", "a repo\n");
    f.git_init("cs/WT");
    f.write("cs/wt/README.md", "another\n");
    f.git_init("cs/wt");
    let stack = |checkouts: &[&Path]| {
        let list: Vec<String> = checkouts.iter().map(|c| format!("'{}'", c.display())).collect();
        format!(
            "name = 'priv'\nvisibility = 'private'\nroles = ['coder']\n[projects.beta]\n[repos.beta-core]\nproject = 'beta'\nadapters = ['claude']\ncheckouts = [{}]\nworktrees = false\n",
            list.join(", ")
        )
    };
    f.write("priv/stack.toml", &stack(&[&upper]));
    apply(&plan(&f.stacks(), &Options { repo: Some("beta-core".into()), ..f.opts() }).unwrap())
        .unwrap();
    let theirs = [upper.join("CLAUDE.local.md"), upper.join(".lords")];
    assert!(theirs.iter().all(|c| c.exists()));
    // Now `wt` is declared too, and never synced.
    f.write("priv/stack.toml", &stack(&[&upper, &lower]));
    let p = uninstall(&f, &lower).unwrap();
    let planned: Vec<String> =
        p.items.iter().map(|i| format!("{:?} {}", i.action, i.path.display())).collect();
    let _ = apply(&p);
    for c in &theirs {
        assert!(c.exists() && recorded(&f, c), "WT's copy stays: {} ({planned:?})", c.display());
    }
    assert!(planned.is_empty(), "wt holds no record: {planned:?}");
}

// Spec (Q2 "tracked files are never touched"; Q3 T10): a skill copy whose file the repo now tracks (`git add -f`) is
// kept, its note saying the repo tracks it, with its record; the place's other copies go.
#[test]
fn a_copy_the_repo_tracks_is_never_touched() {
    let f = Fixture::new();
    let [_, a, _, _] = synced(&f);
    git(&a, &["add", "-f", ".claude/skills/demo/SKILL.md"]);
    let skill = a.join(".claude/skills/demo");
    let p = uninstall(&f, &a).unwrap();
    let i = item(&p, &skill);
    assert_eq!(i.action, Action::Report, "{i:?}");
    assert!(i.notes.iter().any(|n| n.contains("tracks")), "{:?}", i.notes);
    apply(&p).unwrap();
    assert!(skill.join("SKILL.md").is_file() && recorded(&f, &skill));
    assert!(!a.join(".lords").exists());
}

// Spec (Q3 T5: "a link inside the place: the choke point refuses it: the copy is kept … nothing is followed"): with
// the place's `.claude` a link to a dir outside it, the skill copy behind it is kept and never removed; the place's
// other copies go.
#[test]
fn a_link_in_the_place_is_not_followed() {
    let f = Fixture::new();
    let [_, a, _, _] = synced(&f);
    let outside = f.path("outside/claude");
    fs::create_dir_all(outside.parent().unwrap()).unwrap();
    fs::rename(a.join(".claude"), &outside).unwrap();
    dir_link(&a.join(".claude"), &outside);
    let p = uninstall(&f, &a).unwrap();
    assert_eq!(item(&p, &a.join(".claude/skills/demo")).action, Action::Report);
    apply(&p).unwrap();
    assert!(outside.join("skills/demo/SKILL.md").is_file());
    assert!(!a.join(".lords").exists());
}

// Spec (Q3 T3, review r1 UP-R1-04's witness: repo A's worktree moved away without `git worktree repair`, repo B's
// checkout at its path, which A's `git worktree list` still names): that dir is no place of A; refused, B's copy
// stays.
#[test]
fn a_reused_worktree_path_of_another_repo_is_refused() {
    let f = Fixture::new();
    f.write(
        "priv/repo/beta-core/facts/h.md",
        &entry("fact.h", "repo:beta-core", "private", "", "Hosts.\n"),
    );
    let (alpha, x) = (f.path("alpha"), f.path("wt-root/x"));
    f.write("alpha/README.md", "alpha\n");
    f.git_init("alpha");
    git(&alpha, &["worktree", "add", "-q", "../wt-root/x", "-b", "x"]);
    fs::rename(&x, f.path("moved-x")).unwrap();
    f.write("wt-root/x/README.md", "beta\n");
    f.git_init("wt-root/x");
    f.write(
        "priv/stack.toml",
        &format!(
            "name = 'priv'\nvisibility = 'private'\nroles = ['coder']\n[projects.beta]\n[repos.alpha]\nproject = 'beta'\nadapters = ['claude']\ncheckouts = ['{}']\nworktree_roots = ['{}']\n[repos.beta-core]\nproject = 'beta'\nadapters = ['claude']\ncheckouts = ['{}']\nworktrees = false\n",
            alpha.display(),
            f.path("wt-root").display(),
            x.display()
        ),
    );
    apply(&plan(&f.stacks(), &Options { repo: Some("beta-core".into()), ..f.opts() }).unwrap())
        .unwrap();
    assert!(x.join("CLAUDE.local.md").is_file());
    let r = plan(
        &f.stacks(),
        &Options {
            repo: Some("alpha".into()),
            place: Some(x.clone()),
            uninstall: true,
            ..f.opts()
        },
    );
    assert!(r.is_err(), "{:?}", r.map(|p| p.items));
    assert!(x.join("CLAUDE.local.md").is_file(), "B's copy stays");
}

// Spec (Q5 4, the trusted boundary called directly; Q4 "Refused before anything is read: no repo, user_only, an
// adapter, the install stage"): `plan` with a place to uninstall refuses each of these, and no copy changes.
#[test]
fn a_place_to_uninstall_needs_its_repo_and_nothing_else() {
    let f = Fixture::new();
    let [_, a, _, _] = synced(&f);
    let base = Options { place: Some(a.clone()), uninstall: true, ..f.opts() };
    let repo = Some("beta-core".to_string());
    let cases = [
        ("no repo", base.clone()),
        ("the user level", Options { repo: repo.clone(), user_only: true, ..base.clone() }),
        (
            "one adapter",
            Options { repo: repo.clone(), adapter: Some("claude".into()), ..base.clone() },
        ),
        (
            "the install stage",
            Options { repo: repo.clone(), stage: Stage::Install, ..base.clone() },
        ),
    ];
    for (what, o) in cases {
        let r = plan(&f.stacks(), &o);
        assert!(r.is_err(), "{what}: {:?}", r.map(|p| p.items));
    }
    assert!(copies(&a).iter().all(|c| c.exists()));
}
