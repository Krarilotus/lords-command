//! The delivery journal under interruption and concurrency (Sol review r1, F4; design §2.3 "append-only local journal
//! with locked writes"). Each test has its own registry home.

// Tests are outside the filesystem boundary (fsguard.rs): their fixtures write their own temp dirs.
#![allow(clippy::disallowed_methods, clippy::disallowed_types)]

use lords_core::journal;
use lords_core::serve::{self, Start};
use std::fs;
use std::io::Write;
use std::path::Path;
use tempfile::TempDir;

fn write(root: &Path, rel: &str, text: &str) {
    let p = root.join(rel);
    fs::create_dir_all(p.parent().unwrap()).unwrap();
    fs::write(p, text).unwrap();
}

/// A core-only registry home and a dir to brief in.
fn world() -> TempDir {
    let d = TempDir::new().unwrap();
    write(d.path(), "core/stack.toml", "name = \"pub\"\nvisibility = \"public\"\n");
    fs::create_dir_all(d.path().join("cwd")).unwrap();
    lords_core::identity::set_core(&d.path().join("registry"), &d.path().join("core"), None)
        .unwrap();
    d
}

fn brief(d: &TempDir) -> String {
    let start = Start { cwd: d.path().join("cwd"), role: "coder".into(), ..Start::default() };
    serve::brief(&d.path().join("registry"), &start).unwrap().delivery_id
}

/// What a crash in the middle of an append leaves: the start of a line, no newline.
fn cut(d: &TempDir) {
    let mut f =
        fs::OpenOptions::new().append(true).open(d.path().join("registry/journal.jsonl")).unwrap();
    f.write_all(b"{\"v\":").unwrap();
}

// Spec: Sol review r1 F4 (witness `cut-journal`): "define bounded recovery for an incomplete final record, preserving
// completed records and visibly reporting the gap". A cut last line leaves earlier deliveries and new ones working,
// and the gap is recorded, not hidden.
#[test]
fn a_cut_last_line_is_recovered_visibly() {
    let d = world();
    let home = d.path().join("registry");
    let old = brief(&d);
    cut(&d);
    serve::context(&home, &old, "coder", None, "2026-10-28").unwrap();
    let new = brief(&d);
    serve::context(&home, &new, "coder", None, "2026-10-28").unwrap();
    let events = journal::read(&home).unwrap();
    assert!(events.iter().any(|e| e["event"] == "journal_gap"), "the gap is recorded: {events:#?}");
    let text = fs::read_to_string(home.join("journal.jsonl")).unwrap();
    assert!(text.contains("{\"v\":\n"), "the cut fragment itself is kept as evidence");
}

// Spec: F4 "Keep interior corruption strict … never silently accept corrupt audit evidence or report a successful
// usable brief when its journal cannot be read": a bad line with complete lines after it is an error for reads and
// for a new brief.
#[test]
fn interior_corruption_stays_an_error() {
    let d = world();
    let home = d.path().join("registry");
    brief(&d);
    let mut f = fs::OpenOptions::new().append(true).open(home.join("journal.jsonl")).unwrap();
    f.write_all(b"not an event\n").unwrap();
    drop(f);
    assert!(journal::read(&home).is_err());
    let start = Start { cwd: d.path().join("cwd"), role: "coder".into(), ..Start::default() };
    assert!(serve::brief(&home, &start).is_err(), "no usable brief is claimed");
}
