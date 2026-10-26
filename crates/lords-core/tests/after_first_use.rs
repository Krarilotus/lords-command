//! The follow-ups from first use (orchestrator, 26 Oct): exclude edits planned per common dir across all its
//! worktrees, and a failed git call that names its exit status. Each spec line quotes the rule it holds.

// Tests are outside the filesystem boundary (fsguard.rs): their fixtures write their own temp dirs.
#![allow(clippy::disallowed_methods, clippy::disallowed_types)]

mod common;
use common::*;
use lords_core::sync::{self, Action, Stage};
use std::fs;
use std::path::PathBuf;

/// A repo `proj` with one worktree `wts/a`, declared with its worktrees; the common exclude file holds an earlier
/// version's block, which hid `.lords/` in every place.
fn repo_with_worktree(f: &Fixture) -> (PathBuf, PathBuf, PathBuf) {
    f.write("pub/global/rules/r.md", &entry("rule.r", "global", "public", "", "RULE\n"));
    f.write("proj/README.md", "a repo\n");
    f.git_init("proj");
    let (repo, wt) = (f.path("proj"), f.path("wts/a"));
    fs::create_dir_all(f.path("wts")).unwrap();
    git(&repo, &["worktree", "add", "-q", "-b", "a", wt.to_str().unwrap()]);
    f.write(
        "priv/stack.toml",
        &format!(
            "name = 'priv'\nvisibility = 'private'\nroles = ['coder']\n[repos.proj]\ncheckouts = ['{}']\nworktree_roots = ['{}']\nadapters = ['claude']\n",
            repo.display(),
            f.path("wts").display()
        ),
    );
    let exclude = repo.join(".git/info/exclude");
    fs::write(
        &exclude,
        "# git's own\n# Lords Command: agent files written by sync\nCLAUDE.local.md\n.lords/\n",
    )
    .unwrap();
    (repo, wt, exclude)
}

fn exclude_item(p: &sync::Plan) -> &sync::Item {
    p.items.iter().find(|i| i.path.ends_with("exclude")).expect("an exclude item")
}

// Spec (orchestrator, 26 Oct: "exclude edits planned per common dir across all its worktrees, so a place's dry run
// sees what its edit does to the other places"): a place's plan whose exclude edit would make an untracked path
// show in another worktree's git status is a conflict naming that path, and the install stage changes nothing.
#[test]
fn an_exclude_edit_that_would_show_another_worktrees_files_is_a_conflict() {
    let f = Fixture::new();
    let (repo, wt, exclude) = repo_with_worktree(&f);
    f.write("wts/a/.lords/role-coder.md", "from before the ledger\n");
    let before = fs::read(&exclude).unwrap();
    let status = git(&wt, &["status", "--porcelain"]);
    assert_eq!(status, "", "hidden by the earlier block");
    let dry = sync::Options { repo: Some("proj".into()), place: Some(repo.clone()), ..f.opts() };
    let p = sync::plan(&f.stacks(), &dry).unwrap();
    let ex = exclude_item(&p);
    assert_eq!(ex.action, Action::Conflict, "{:?}", ex.notes);
    assert!(ex.notes.iter().any(|n| n.contains("wts/a/.lords")), "{:?}", ex.notes);
    let install = sync::Options { stage: Stage::Install, ..dry };
    let p = sync::plan(&f.stacks(), &install).unwrap();
    assert!(p.notes.iter().any(|n| n.contains("skipped whole")), "{:?}", p.notes);
    sync::apply(&p).unwrap();
    assert_eq!(fs::read(&exclude).unwrap(), before, "the exclude file is unchanged");
    assert_eq!(git(&wt, &["status", "--porcelain"]), status, "the worktree's status is unchanged");
}

// Spec (the same rule): with nothing in any worktree that the edit would show, the exclude edit goes ahead.
#[test]
fn an_exclude_edit_that_shows_nothing_new_goes_ahead() {
    let f = Fixture::new();
    let (repo, wt, exclude) = repo_with_worktree(&f);
    let opts = sync::Options {
        repo: Some("proj".into()),
        place: Some(repo.clone()),
        stage: Stage::Install,
        ..f.opts()
    };
    let p = sync::plan(&f.stacks(), &opts).unwrap();
    assert_eq!(exclude_item(&p).action, Action::Update, "{:?}", exclude_item(&p).notes);
    sync::apply(&p).unwrap();
    assert!(fs::read_to_string(&exclude).unwrap().contains("/CLAUDE.local.md"));
    assert_eq!(git(&wt, &["status", "--porcelain"]), "");
    assert_eq!(git(&repo, &["status", "--porcelain"]), "");
}

// Spec (orchestrator, 26 Oct: "`git::run` names the exit status or signal in every error"): a git call that fails
// says how it ended, so an empty "failed:" can't hide whether git ran at all.
#[test]
fn a_failed_git_call_names_its_exit_status() {
    let f = Fixture::new();
    fs::create_dir_all(f.path("plain")).unwrap();
    let err =
        lords_core::git::run(&f.path("plain"), &["rev-parse", "--show-toplevel"]).unwrap_err();
    assert!(err.contains("exit code") || err.contains("exit status"), "{err}");
    let err = lords_core::git::bytes(&f.path("plain"), &["show", "HEAD"]).unwrap_err();
    assert!(err.contains("exit code") || err.contains("exit status"), "{err}");
    let err = lords_core::git::status(&f.path("plain"), &["diff", "--quiet"]).unwrap_err();
    assert!(err.contains("exit code") || err.contains("exit status"), "{err}");
}

// Spec (orchestrator, 26 Oct: "an edit that only adds lines can't reveal anything, so it needs no untracked listing
// at all"): an exclude edit that only adds lines lists no worktree: one where any listing fails doesn't stop it.
#[test]
fn an_exclude_edit_that_only_adds_lines_lists_no_worktree() {
    let f = Fixture::new();
    let (repo, _wt, exclude) = repo_with_worktree(&f);
    fs::write(&exclude, "# git's own\n").unwrap();
    // A second worktree git still lists, where every git call fails: its `.git` points nowhere.
    let broken = f.path("wts/b");
    git(&repo, &["worktree", "add", "-q", "-b", "b", broken.to_str().unwrap()]);
    fs::write(broken.join(".git"), format!("gitdir: {}\n", f.path("nowhere").display())).unwrap();
    let opts = sync::Options { repo: Some("proj".into()), place: Some(repo.clone()), ..f.opts() };
    let p = sync::plan(&f.stacks(), &opts).unwrap();
    let ex = exclude_item(&p);
    assert_eq!(ex.action, Action::Update, "{:?}", ex.notes);
}
