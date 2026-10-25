//! The real stacks, where this machine has them: the files sync would write stay under the size warning line
//! (orchestrator, 25 Oct: a spec test that composes the real private stack's largest place "and asserts it's under
//! the line, so it can't creep back up"). Read-only: it composes and measures, and writes nothing.

use lords_core::config::Config;
use lords_core::sync::{sizes, SIZE_WARN_PERCENT};
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

// Spec (above): every place the private stack declares a checkout or dir for is measured, and each place's file
// for an adapter with a limit, read with that adapter's user-level file, stays under SIZE_WARN_PERCENT of the
// limit, measured with absolute paths as sync writes them.
#[test]
fn every_place_on_this_machine_stays_under_the_size_line() {
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
            all.iter().any(|s| s.place == place && s.limit.is_some() && s.reads > s.bytes),
            "{place} is measured with a limited adapter"
        );
    }
    let over: Vec<String> = all
        .iter()
        .filter(|s| s.over_line())
        .map(|s| {
            format!(
                "{} {}: {} of {:?}, by part {:?}",
                s.place, s.file, s.reads, s.limit, s.sections
            )
        })
        .collect();
    assert!(over.is_empty(), "past {SIZE_WARN_PERCENT}% of the limit: {over:#?}");
}
