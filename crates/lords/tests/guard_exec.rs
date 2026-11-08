//! The guard copy runs though something held it open for writing a moment before (Linux, ETXTBSY: job
//! lords-test-linux-84c9449e-1794157526-435833, where `install_guard`'s self-test of the copy it had just written failed
//! with "Text file busy"). Linux refuses to run a file that any process holds open for writing; in a process with many
//! threads, a child another thread starts holds every file that was open then until its own exec, so a copy written
//! and closed a moment ago can still be held. Off Windows only: Windows has no such refusal.
//!
//! The copy is held from just before the self-test starts it (`process::guard_self_test`), so the hold covers the
//! start whatever the machine's speed: a hold timed around a whole `install_guard` ended during its reads of the
//! binary, before the start (red attempt lords-test-linux-602d1313-1794159411-456898 passed without the fix).

#![cfg(unix)]
// Tests are outside the filesystem boundary (fsguard.rs): their fixtures write their own temp dirs.
#![allow(clippy::disallowed_methods, clippy::disallowed_types)]

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// The guard copy installed in a fresh tool home under `d`.
fn installed(d: &Path) -> PathBuf {
    let home = d.join(".lords-command");
    fs::create_dir_all(&home).unwrap();
    let exe = Path::new(env!("CARGO_BIN_EXE_lords"));
    assert_eq!(
        lords_core::sync::install_guard(&home, exe),
        Ok(true),
        "the install writes the copy"
    );
    home.join("bin").join("lords-guard.exe")
}

/// The copy's self-test, started while the copy is held open for writing, which ends `hold` later; and how long the
/// self-test took.
fn self_test_while_held(copy: &Path, hold: Duration) -> (Result<(), String>, Duration) {
    let held = fs::OpenOptions::new().write(true).open(copy).unwrap();
    let release = std::thread::spawn(move || {
        std::thread::sleep(hold);
        drop(held);
    });
    let started = Instant::now();
    let r = lords_core::process::guard_self_test(copy);
    let took = started.elapsed();
    release.join().unwrap();
    (r, took)
}

// Spec (the job above; `lords guard install`: "a staged copy that must pass `guard --self-test`"): a guard copy that
// something holds open for writing for a moment (200 ms) as its self-test starts still answers it.
#[test]
fn a_guard_copy_held_open_for_writing_a_moment_still_answers() {
    let d = tempfile::TempDir::new().unwrap();
    let copy = installed(d.path());
    let (r, _) = self_test_while_held(&copy, Duration::from_millis(200));
    assert_eq!(r, Ok(()), "it answers once the hold ends");
}

// Spec (rule.remote-calls-have-deadlines' class: a wait has its bound, and a missing answer fails closed): a copy held
// open for writing for longer (3 s) is refused with the cause named, well before the hold ends. (The bound, not a
// fails-without test: without the fix the refusal is immediate.)
#[test]
fn a_guard_copy_held_open_for_writing_longer_is_refused_with_why() {
    let d = tempfile::TempDir::new().unwrap();
    let copy = installed(d.path());
    let (r, took) = self_test_while_held(&copy, Duration::from_secs(3));
    let e = r.expect_err("refused");
    assert!(e.contains("busy"), "{e}");
    assert!(took < Duration::from_millis(2500), "bounded: {took:?}");
}

/// A stand-in guard copy that never answers: it hangs until it is killed.
fn hanging(d: &Path) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let p = d.join("hanging-guard");
    fs::write(&p, "#!/bin/sh\nexec sleep 30\n").unwrap();
    fs::set_permissions(&p, fs::Permissions::from_mode(0o755)).unwrap();
    p
}

/// `guard_self_test_within(copy, budget)`, started while the copy is held open for writing, which ends `hold` later;
/// and how long it took.
fn within_while_held(
    copy: &Path,
    hold: Duration,
    budget: Duration,
) -> (Result<(), String>, Duration) {
    let held = fs::OpenOptions::new().write(true).open(copy).unwrap();
    let release = std::thread::spawn(move || {
        std::thread::sleep(hold);
        drop(held);
    });
    let started = Instant::now();
    let r = lords_core::process::guard_self_test_within(copy, budget);
    let took = started.elapsed();
    release.join().unwrap();
    (r, took)
}

// Spec (package review r1, R1-01: "establish the overall deadline before the first spawn and retain it through
// child/output waiting"; the master: a spawn that succeeds near the window's end does not get a fresh budget): a copy
// held busy for 800 ms that then hangs is stopped at its budget (1.5 s) counted from the call, not from its start
// (about 2.3 s).
#[test]
fn a_copy_that_starts_late_gets_only_what_is_left_of_its_budget() {
    let d = tempfile::TempDir::new().unwrap();
    let copy = hanging(d.path());
    let (r, took) =
        within_while_held(&copy, Duration::from_millis(800), Duration::from_millis(1500));
    let e = r.expect_err("past its budget");
    assert!(e.contains("took over"), "the budget stops it: {e}");
    assert!(took < Duration::from_millis(2000), "one budget from the call: {took:?}");
}

// Spec (package review r1, R1-01: the busy wait inside the overall budget): a copy held busy for longer than a short
// budget (300 ms) is refused as busy when the budget ends, not after the whole busy window (1 s).
#[test]
fn the_busy_wait_ends_with_the_budget() {
    let d = tempfile::TempDir::new().unwrap();
    let copy = hanging(d.path());
    let (r, took) = within_while_held(&copy, Duration::from_secs(3), Duration::from_millis(300));
    let e = r.expect_err("refused");
    assert!(e.contains("busy"), "the last busy refusal: {e}");
    assert!(took < Duration::from_millis(700), "within the budget: {took:?}");
}
