//! The real stacks, where this machine has them: `sizes` measures every place sync would write. How big those files
//! are today is the stacks' state, not the code's: `lords check` warns past the warning line and sync refuses a file
//! past its harness's limit (orchestrator, 27 Oct, moving the 25 Oct size assertion there). Read-only: it composes
//! and measures, and writes nothing.

// Tests are outside the filesystem boundary (fsguard.rs): their fixtures write their own temp dirs.
#![allow(clippy::disallowed_methods, clippy::disallowed_types)]

use lords_core::config::Config;
use lords_core::sync::sizes;
use lords_core::Stacks;
use std::path::PathBuf;

/// The public stack of this repo and the private stack: `LORDS_PRIVATE_STACK`, else config.txt, else
/// `lords-command-private` beside this repo.
fn real_stacks() -> Option<Stacks> {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("..").join("..");
    let root = std::path::absolute(&root).ok()?;
    let private = std::env::var_os("LORDS_PRIVATE_STACK")
        .map(PathBuf::from)
        .or(Config::discover().private)
        .or_else(|| Some(root.join("..").join("lords-command-private")))
        .filter(|p| p.join("stack.toml").is_file())?;
    let private = std::path::absolute(&private).ok()?;
    Stacks::load(&root.join("stack"), Some(&private)).ok()
}

// Spec (above): every place the private stack declares a checkout or dir for is measured for an adapter with a
// limit, so the check's warning and sync's refusal see each one.
#[test]
fn every_place_on_this_machine_is_measured_against_a_limit() {
    let Some(stacks) = real_stacks() else {
        eprintln!("no private stack on this machine: the real places aren't measured");
        return;
    };
    let all = sizes(&stacks);
    let manifest = &stacks.private.as_ref().unwrap().manifest;
    // A place sync writes to: one with a checkout or dir.
    let written = |name: &String| lords_core::sync::first_path(&stacks, name).is_some();
    let declared = manifest.repos.keys().filter(|r| written(r)).map(|r| format!("repo {r}"));
    let workspaces = manifest.workspaces.keys().filter(|w| written(w));
    let declared = declared.chain(workspaces.map(|w| format!("workspace {w}")));
    for place in declared {
        assert!(
            all.iter().any(|s| s.place == place && s.limit.is_some()),
            "{place} is measured with a limited adapter"
        );
    }
}
