//! A stack loaded from the tree its store committed is the stack its checkout holds (gap 11, design note §0b.3 step 5,
//! §0c.4 and §0e: the captured-source refactor). Same entries, paths, texts, versions and problems, and the same
//! composed text; nothing is read from disk once captured; and where the two can't agree, the contract is stated: git
//! keeps no empty dir, names match exactly, and a link is never followed.

// Tests are outside the filesystem boundary (fsguard.rs): their fixtures write their own temp dirs.
#![allow(clippy::disallowed_methods, clippy::disallowed_types)]

mod common;
use common::*;
use lords_core::capture::Captured;
use lords_core::compose::{compose, Context, Layers};
use lords_core::model::Audience;
use lords_core::Stacks;
use std::path::Path;
use std::process::Command;

/// The tree committed at `HEAD` in the repo at `root` (a stack dir that is its repo's root), as an independent reader
/// gets it: `git ls-tree` and `git cat-file`, a file per blob and a link per symlink or submodule.
fn captured(root: &Path) -> Captured {
    isolate_git();
    let run = |args: &[&str]| {
        let out = Command::new("git").arg("-C").arg(root).args(args).output().unwrap();
        assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
        out.stdout
    };
    let mut c = Captured::new(root);
    let list = run(&["ls-tree", "-r", "-z", "HEAD"]);
    for item in list.split(|b| *b == 0).filter(|i| !i.is_empty()) {
        let item = String::from_utf8(item.to_vec()).unwrap();
        let (head, path) = item.split_once('\t').unwrap();
        let fields: Vec<&str> = head.split(' ').collect();
        match fields[0] {
            "120000" | "160000" => c.add_link(path).unwrap(),
            _ => c.add_file(path, run(&["cat-file", "blob", fields[2]])).unwrap(),
        }
    }
    c
}

fn commit(root: &Path) {
    git(root, &["init", "-q"]);
    git(root, &["add", "-A"]);
    git(root, &["commit", "-q", "-m", "fixture"]);
}

/// Everything a load gives, as text: each stack's root, its entries with their paths, texts and versions, and its
/// problems.
fn loaded(s: &Stacks) -> String {
    s.all().map(|st| format!("{:?}\n{:#?}\n{:#?}", st.root, st.entries, st.problems)).collect()
}

fn context(layers: Layers) -> Context {
    Context {
        audience: vec![Audience::Opus, Audience::Sol],
        layers,
        public_only: false,
        skip_stores: vec![],
        roles: None,
        by_reference: false,
        role_dir: None,
        relative_to: None,
        default_role: None,
        default_role_inline: false,
        opener: None,
        child: None,
        default_role_lines: true,
        memory_file: None,
    }
}

/// Both stacks, each with what the loader reads and what it reports: every layer, a whole-body rule and one with a
/// core line, intent, a skill with nested files, a memory store with defaults and an index, and the problems a
/// malformed entry, a skill without its SKILL.md, a file that isn't UTF-8, a stray file and an unknown kind dir give.
fn fixture() -> Fixture {
    let f = Fixture::new();
    f.write("pub/global/rules/a.md", &entry("rule.a", "global", "public", "", "\nWhole text.\n"));
    f.write(
        "pub/global/rules/b.md",
        &entry("rule.b", "global", "public", "start: core\ncore: One writer per file.\n", "B.\n"),
    );
    f.write("pub/global/intent/i.md", &entry("intent.i", "global", "public", "", "Intent.\n"));
    f.write(
        "pub/global/skills/review/lords.yaml",
        "id: skill.review\nkind: skill\ntitle: review\naudience: [all]\nscope: global\nstatus: active\nvisibility: public\norigin:\n  type: own\n",
    );
    f.write(
        "pub/global/skills/review/SKILL.md",
        "---\nname: review\ndescription: d\n---\nSteps.\n",
    );
    f.write("pub/global/skills/review/scripts/run.sh", "echo\n");
    f.write("pub/global/skills/review/refs/deep/note.md", "note\n");
    f.write("pub/global/skills/broken/lords.yaml", "id: skill.broken\nkind: skill\n");
    f.write("pub/global/rules/bad.md", "---\nid: [unclosed\n---\nx\n");
    f.write("pub/global/rules/notes.txt", "not an entry\n");
    f.write("pub/global/stray.md", "a file directly in a layer dir\n");
    f.write("pub/global/oddities/x.md", "an unknown kind dir\n");
    std::fs::write(f.path("pub/global/rules/latin1.md"), b"---\nid: rule.l\n---\n\xe9\n").unwrap();
    f.write(
        "priv/project/beta/rules/p.md",
        &entry("rule.p", "project:beta", "private", "roles: [coder]\n", "Project.\n"),
    );
    f.write(
        "priv/repo/beta-core/rules/r.md",
        &entry("rule.r", "repo:beta-core", "private", "", "Repo.\n"),
    );
    f.write(
        "priv/global/memory/agent/lords.defaults.yaml",
        "kind: memory\nscope: global\naudience: [opus]\nstatus: active\nvisibility: private\norigin:\n  type: imported\n  store: agent\n",
    );
    f.write("priv/global/memory/agent/MEMORY.md", "- [x](x.md)\n");
    f.write("priv/global/memory/agent/x.md", "---\nname: n\ndescription: d\n---\nbody\n");
    commit(&f.pub_root());
    commit(&f.priv_root());
    f
}

fn load_captured(f: &Fixture) -> Stacks {
    Stacks::load_captured(&captured(&f.pub_root()), Some(&captured(&f.priv_root())), true).unwrap()
}

// Spec (design note §0b.3 step 5: "every recorded path is the logical one by construction"; §0c.4: the loader's
// diagnostics, absence and unreadable cases "each give the same result from both sources"): the stacks loaded from
// their committed trees equal the stacks loaded from their checkouts, field by field and problem by problem, and
// compose to the same text in a global and a repo file.
#[test]
fn a_captured_stack_loads_as_its_checkout() {
    let f = fixture();
    let (disk, kept) = (f.stacks(), load_captured(&f));
    assert!(loaded(&disk).contains("unknown kind dir"), "the fixture's problems load");
    assert_eq!(loaded(&kept), loaded(&disk));
    for layers in [
        Layers::Global,
        Layers::Repo {
            repo: "beta-core".into(),
            project: Some("beta".into()),
            sub: None,
            global: true,
        },
    ] {
        let ctx = context(layers);
        assert_eq!(compose(&kept, &ctx).text, compose(&disk, &ctx).text);
    }
}

// Spec (§0b.3 step 5: "every read stays inside the captured blobs by construction"; r3 review: "poison/change logical
// source content after capture and assert captured output remains unchanged"): once captured, editing, adding and
// deleting the checkout's files changes nothing a captured load gives.
#[test]
fn nothing_is_read_from_the_checkout_once_captured() {
    let f = fixture();
    let before = loaded(&f.stacks());
    let (public, private) = (captured(&f.pub_root()), captured(&f.priv_root()));
    f.write("pub/global/rules/a.md", &entry("rule.a", "global", "public", "", "Edited.\n"));
    f.write("pub/global/rules/new.md", &entry("rule.new", "global", "public", "", "New.\n"));
    std::fs::remove_file(f.path("priv/repo/beta-core/rules/r.md")).unwrap();
    std::fs::remove_file(f.path("pub/global/skills/review/SKILL.md")).unwrap();
    assert_ne!(loaded(&f.stacks()), before, "the checkout did change");
    let kept = Stacks::load_captured(&public, Some(&private), true).unwrap();
    assert_eq!(loaded(&kept), before);
}

// Spec (the Lords owner, 8 Nov 16:48, condition 4: "an empty kind dir: git doesn't track it"; §0e: "it's absent from
// the capture; the loader must give the same result as for an empty dir"): a checkout with empty known kind dirs,
// in a layer that has none committed too, loads as its capture does. An empty unknown dir is a problem only on
// disk, as no commit holds it: the contract, stated.
#[test]
fn an_empty_dir_is_absent_from_a_capture() {
    let f = fixture();
    std::fs::create_dir_all(f.path("pub/global/facts")).unwrap();
    std::fs::create_dir_all(f.path("pub/project/alpha/rules")).unwrap();
    let kept = loaded(&load_captured(&f));
    assert_eq!(loaded(&f.stacks()), kept);
    std::fs::create_dir_all(f.path("pub/global/empty-unknown")).unwrap();
    let disk = loaded(&f.stacks());
    assert_eq!(disk.matches("unknown kind dir").count(), 2, "on disk it is reported: {disk}");
    assert_eq!(kept.matches("unknown kind dir").count(), 1, "a capture has no empty dir");
}

// Spec (condition 4: "a name that differs only in case from one the loader looks up … the test should state which
// result is the contract"; §0e: "case-exact is the contract"): a skill whose file is `skill.md` has no SKILL.md in a
// capture, on every system. On disk it depends on the filesystem; on Windows's, which ignores case, it loads.
#[test]
fn names_in_a_capture_match_exactly() {
    let f = fixture();
    let dir = "pub/global/skills/lower";
    f.write(
        &format!("{dir}/lords.yaml"),
        "id: skill.lower\nkind: skill\ntitle: lower\naudience: [all]\nscope: global\nstatus: active\nvisibility: public\norigin:\n  type: own\n",
    );
    f.write(&format!("{dir}/skill.md"), "---\nname: lower\ndescription: d\n---\nSteps.\n");
    git(&f.pub_root(), &["add", "-A"]);
    git(&f.pub_root(), &["commit", "-q", "-m", "lower"]);
    let kept = load_captured(&f);
    assert!(kept.find("skill.lower").is_none());
    assert!(loaded(&kept).contains("a skill needs a SKILL.md"));
    #[cfg(windows)]
    assert!(
        f.stacks().find("skill.lower").is_some(),
        "Windows's filesystem finds SKILL.md as skill.md"
    );
}

// Spec (the Lords owner, 16:48: "the captured kind/link_at report links, so the existing checks refuse exactly where
// they touch one"; §0e): a link a skill dir holds is left out of its files, as the checkout loader leaves one out; a
// layer's kind dir that is a link stops the load where the loader would list it, as a capture never follows one.
#[test]
fn a_link_in_a_capture_is_refused_where_the_loader_touches_it() {
    let f = fixture();
    let mut c = captured(&f.pub_root());
    c.add_link("global/skills/review/linked").unwrap();
    let kept = Stacks::load_captured(&c, None, false).unwrap();
    let review = kept.find("skill.review").unwrap();
    assert!(!review.files.iter().any(|p| p.ends_with("linked")), "{:?}", review.files);
    assert_eq!(review.files.len(), 4, "{:?}", review.files);
    let mut c = captured(&f.pub_root());
    c.add_link("global/templates").unwrap();
    let err = Stacks::load_captured(&c, None, false).err().unwrap();
    assert!(err.contains("a link in the captured stack"), "{err}");
}

// Spec (Captured's contract: "a path in the tree with `/` between its parts"): a part a tree can't hold, or a path
// that meets another, is refused when captured, never made into a different path.
#[test]
fn a_capture_refuses_paths_a_tree_cannot_hold() {
    let mut c = Captured::new(Path::new("stack"));
    c.add_file("global/rules/a.md", b"x".to_vec()).unwrap();
    for bad in
        ["", "global//b.md", "../b.md", "global/./b.md", "global/rules/a.md", "global/rules/a.md/c"]
    {
        assert!(c.add_file(bad, b"y".to_vec()).is_err(), "{bad}");
    }
    assert!(c.add_link("global/rules").is_err());
}
