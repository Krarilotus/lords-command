//! The in-place index of a repo's tracked skills (backbone requirement BB-8) after first use (orchestrator, 26 Oct:
//! "the in-place index lists only what git tracks under the path, never sync's own copies").

// Tests are outside the filesystem boundary (fsguard.rs): their fixtures write their own temp dirs.
#![allow(clippy::disallowed_methods, clippy::disallowed_types)]

mod common;
use common::*;
use lords_core::sync::{self, Action};

/// A public role skill sync copies into each checkout, and a repo that tracks its own skill in the same dir sync
/// copies into (`.claude/skills`), indexed in place.
fn repo_with_tracked_skill(f: &Fixture) -> std::path::PathBuf {
    f.write("pub/global/skills/demo/SKILL.md", "---\nname: demo\ndescription: demo\n---\nSteps.\n");
    f.write(
        "pub/global/skills/demo/lords.yaml",
        "id: skill.demo\nkind: skill\ntitle: demo\naudience: [all]\nroles: [coder]\nscope: global\nstatus: active\nvisibility: public\norigin:\n  type: own\n",
    );
    f.write(
        "repo/.claude/skills/verify/SKILL.md",
        "---\nname: verify\ndescription: the repo's own\n---\nRun it.\n",
    );
    f.git_init("repo");
    f.write(
        "priv/stack.toml",
        &format!(
            "name = 'priv'\nvisibility = 'private'\nroles = ['coder']\n[repos.r]\ncheckouts = ['{}']\nworktrees = false\nadapters = ['claude']\nskills = ['.claude/skills']\n",
            f.path("repo").display()
        ),
    );
    f.path("repo")
}

// Spec (orchestrator, 26 Oct, and guarantee 6: a plan right after its apply changes nothing): sync's own copy in a
// repo's indexed skills dir isn't indexed as the repo's skill, so the next plan plans nothing.
#[test]
fn an_installed_copy_in_an_indexed_skills_dir_changes_nothing_next() {
    let f = Fixture::new();
    let repo = repo_with_tracked_skill(&f);
    sync::apply(&sync::plan(&f.stacks(), &f.opts()).unwrap()).unwrap();
    assert!(repo.join(".claude/skills/demo/SKILL.md").is_file(), "the copy is installed");
    let again = sync::plan(&f.stacks(), &f.opts()).unwrap();
    let changing: Vec<_> = again
        .items
        .iter()
        .filter(|i| i.action != Action::InSync && i.action != Action::Report)
        .map(|i| (&i.action, &i.path))
        .collect();
    assert!(changing.is_empty(), "{changing:?}");
}

// Spec (the same): the index holds the tracked skill and nothing git doesn't track there.
#[test]
fn the_index_lists_only_tracked_skills() {
    let f = Fixture::new();
    let repo = repo_with_tracked_skill(&f);
    f.write(
        "repo/.claude/skills/scratch/SKILL.md",
        "---\nname: scratch\ndescription: untracked\n---\nNot the repo's.\n",
    );
    assert!(repo.join(".claude/skills/scratch/SKILL.md").is_file());
    let stacks = f.stacks();
    let ids: Vec<&str> =
        stacks.entries().filter(|e| e.id().starts_with("skill.r.")).map(|e| e.id()).collect();
    assert_eq!(ids, vec!["skill.r.verify"]);
}
