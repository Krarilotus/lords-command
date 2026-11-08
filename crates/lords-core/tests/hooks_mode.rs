//! D28: a stack repo's hooks check by its stack's `visibility`, not by a flag (witness: the Lords owner, 8 Nov 14:12,
//! reinstalled the private store's hooks without `--secrets-only`, and its commits ran the public policy for four
//! minutes). A private stack's hooks check secrets only whatever the flag; `--secrets-only` on a public stack is
//! refused and changes nothing; a repo with no stack keeps the flag.

// Tests are outside the filesystem boundary (fsguard.rs): their fixtures write their own temp dirs.
#![allow(clippy::disallowed_methods, clippy::disallowed_types)]

mod common;

use common::Fixture;
use std::fs;
use std::path::Path;

const EXE: &str = "C:/lords/lords.exe";

/// The privacy line of each of the repo's hooks that has one: whether it checks secrets only.
fn secrets_only(repo: &Path) -> Vec<(String, bool)> {
    ["pre-commit", "commit-msg", "pre-push"]
        .iter()
        .filter_map(|h| {
            let text = fs::read_to_string(repo.join(".git/hooks").join(h)).ok()?;
            let line = text.lines().find(|l| l.contains(" privacy "))?.to_string();
            Some((h.to_string(), line.contains("--secrets-only")))
        })
        .collect()
}

// Spec (D28, the master thread 8 Nov 14:32: "`hooks install` takes the mode from the stack's `visibility`"): a private
// stack repo's pre-commit, commit-msg and pre-push check secrets only, though the install named no flag.
#[test]
fn a_private_stack_repos_hooks_check_secrets_only_without_the_flag() {
    let f = Fixture::new();
    f.git_init("priv");
    lords_core::hooks::install(&f.priv_root(), Path::new(EXE), false, false, &f.home()).unwrap();
    let modes = secrets_only(&f.priv_root());
    assert_eq!(modes.len(), 3, "{modes:?}");
    assert!(modes.iter().all(|(_, s)| *s), "{modes:?}");
}

// Spec (D28: "a flag that contradicts it is refused"): `--secrets-only` on a public stack repo is refused, naming the
// stack's visibility, and no hook is written; without it the hooks check the whole policy.
#[test]
fn secrets_only_on_a_public_stack_repo_is_refused_and_changes_nothing() {
    let f = Fixture::new();
    f.git_init("pub");
    let r = lords_core::hooks::install(&f.pub_root(), Path::new(EXE), true, false, &f.home());
    let e = r.err().expect("refused");
    assert!(e.contains("public"), "{e}");
    assert!(
        secrets_only(&f.pub_root()).is_empty(),
        "no hook written: {:?}",
        secrets_only(&f.pub_root())
    );
    lords_core::hooks::install(&f.pub_root(), Path::new(EXE), false, false, &f.home()).unwrap();
    let modes = secrets_only(&f.pub_root());
    assert_eq!(modes.len(), 3, "{modes:?}");
    assert!(modes.iter().all(|(_, s)| !*s), "{modes:?}");
}

// Spec (D28: the mode comes from a stack where there is one): a repo with no stack keeps what the flag says.
#[test]
fn a_repo_without_a_stack_keeps_the_flag() {
    let f = Fixture::new();
    f.write("plain/README.md", "a repo\n");
    f.git_init("plain");
    let repo = f.path("plain");
    lords_core::hooks::install(&repo, Path::new(EXE), true, false, &f.home()).unwrap();
    let modes = secrets_only(&repo);
    assert!(!modes.is_empty() && modes.iter().all(|(_, s)| *s), "{modes:?}");
}
