//! The third Sol review's R3-05 (Harness/notes/2026-10-25-sol-review-lords-command-r3.md): the guard's link check
//! and the action are separate steps, so apply checks each path again after acting on it. That check is a new call,
//! so this file doesn't build on 3546e9e (its red run); round3.rs has the others.

mod common;
use common::*;
use lords_core::fsguard;
use std::path::Path;
use std::process::Command;

fn dir_link(link: &Path, to: &Path) {
    std::fs::create_dir_all(link.parent().unwrap()).unwrap();
    #[cfg(windows)]
    {
        let win = |p: &Path| p.to_string_lossy().replace('/', "\\");
        let out = Command::new("cmd")
            .args(["/C", "mklink", "/J"])
            .arg(win(link))
            .arg(win(to))
            .output()
            .unwrap();
        assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    }
    #[cfg(not(windows))]
    std::os::unix::fs::symlink(to, link).unwrap();
}

// Spec (R3-05: "The guard's link check is separate from the filesystem action"; property: after each action the
// path it acted on is still inside its target with no link on the way, or apply stops): the check after an action
// refuses a path whose dir became a link, and a path outside the target's root; a plain path inside passes.
#[test]
fn r3_05_the_check_after_an_action_refuses_a_link_or_a_path_outside() {
    let f = Fixture::new();
    f.write("target/real/file", "X");
    f.write("elsewhere/file", "Y");
    let root = f.path("target");
    assert!(fsguard::verify_inside(&f.path("target/real/file"), &root).is_ok());
    // The dir moved away and a junction put in its place, as a concurrent process could between check and act.
    std::fs::rename(f.path("target/real"), f.path("target/moved")).unwrap();
    dir_link(&f.path("target/real"), &f.path("elsewhere"));
    let err = fsguard::verify_inside(&f.path("target/real/file"), &root).unwrap_err();
    assert!(err.contains("link"), "{err}");
    assert!(fsguard::verify_inside(&f.path("elsewhere/file"), &root).is_err());
    let _ = std::fs::remove_dir(f.path("target/real"));
}
