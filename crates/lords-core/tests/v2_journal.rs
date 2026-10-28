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
    let gap = events.iter().find(|e| e["event"] == "journal_gap").expect("the gap is recorded");
    // The cut fragment itself is kept as evidence: `{"v":` is 7b 22 76 22 3a.
    assert_eq!(gap["cut_hex"], "7b2276223a", "{gap}");
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

// Spec: Sol fix check 1, F4b (witness `review_invalid_utf8_completed_middle_line`): "preserve strict decoding of
// completed records; any tolerance for a cut final record must be confined to that record".
#[test]
fn invalid_utf8_in_a_completed_record_is_an_error() {
    let d = world();
    let home = d.path().join("registry");
    fs::write(
        home.join("journal.jsonl"),
        b"{\"event\":\"old\"}\n{\"event\":\"\xff\"}\n{\"event\":\"new\"}\n",
    )
    .unwrap();
    assert!(journal::read(&home).is_err());
}

// Spec: F4a "make recovery itself recoverable/idempotent at every interrupted publication prefix while preserving
// completed events": an append cut after any byte of its line, followed by two appends, leaves every completed event
// and the two new ones readable, the cut recorded once.
#[test]
fn an_append_cut_at_any_byte_is_recovered() {
    let line =
        b"{\"v\":1,\"at\":\"2026-10-28T00:00:00Z\",\"event\":\"cut\",\"pad\":\"\xc3\xa9\"}\n";
    for cut in 1..line.len() {
        let d = world();
        let home = d.path().join("registry");
        brief(&d);
        let mut f = fs::OpenOptions::new().append(true).open(home.join("journal.jsonl")).unwrap();
        f.write_all(&line[..cut]).unwrap();
        drop(f);
        journal::append(&home, "first", serde_json::json!({})).unwrap();
        journal::append(&home, "second", serde_json::json!({})).unwrap();
        let events = journal::read(&home).unwrap_or_else(|e| panic!("cut {cut}: {e}"));
        let kinds: Vec<&str> = events.iter().map(|e| e["event"].as_str().unwrap()).collect();
        assert_eq!(kinds, ["brief", "journal_gap", "first", "second"], "cut {cut}");
    }
}

// Spec: F4 "serialize reads/recovery with appends" (the review's controlled witness `review_journal_reader_lock_witness`):
// a reader that starts while a writer holds the journal's lock waits for it and sees the completed write.
#[test]
fn a_reader_waits_for_a_writer_holding_the_lock() {
    use lords_core::fsguard::{self, Part, Root};
    use std::sync::mpsc;
    use std::time::Duration;
    let d = world();
    let home = d.path().join("registry");
    brief(&d);
    let done = fs::read(home.join("journal.jsonl")).unwrap();
    let root = Root::home(&home).unwrap();
    let held = fsguard::lock(&root.join(&[Part::new("journal.lock").unwrap()])).unwrap();
    let mut partial = done.clone();
    partial.extend_from_slice(b"{\"v\":1,\"event\":\"ne");
    fs::write(home.join("journal.jsonl"), &partial).unwrap();
    std::thread::scope(|s| {
        let (tx, rx) = mpsc::channel();
        let h = &home;
        s.spawn(move || tx.send(journal::read(h)).unwrap());
        let early = rx.recv_timeout(Duration::from_millis(300));
        let mut whole = done.clone();
        whole.extend_from_slice(b"{\"v\":1,\"event\":\"new\"}\n");
        fs::write(home.join("journal.jsonl"), &whole).unwrap();
        drop(held);
        assert!(early.is_err(), "the reader returned while the writer held the lock: {early:?}");
        let events = rx.recv_timeout(Duration::from_secs(10)).unwrap().unwrap();
        assert_eq!(events.last().unwrap()["event"], "new");
    });
}

// Spec: Sol fix check 1, F4a "make recovery itself recoverable/idempotent at every interrupted publication prefix":
// recovery publishes by a staged copy renamed over the journal, so its interrupted states are the journal as it was
// (cut tail and all) with a staged copy cut after any byte. From each, two appends leave every complete event, one gap
// and the two new events readable, and no staged copy behind.
#[test]
fn a_recovery_interrupted_at_any_byte_is_recovered() {
    let start = b"{\"v\":1,\"event\":\"brief\"}\n{\"v\":".to_vec();
    let probe = TempDir::new().unwrap();
    fs::write(probe.path().join("journal.jsonl"), &start).unwrap();
    journal::append(probe.path(), "probe", serde_json::json!({})).unwrap();
    let after = fs::read(probe.path().join("journal.jsonl")).unwrap();
    // What recovery published: everything up to the probe's own line.
    let probe_line = after[..after.len() - 1].iter().rposition(|b| *b == b'\n').unwrap() + 1;
    let recovered = &after[..probe_line];
    for cut in 0..=recovered.len() {
        let d = TempDir::new().unwrap();
        let home = d.path();
        fs::write(home.join("journal.jsonl"), &start).unwrap();
        fs::write(home.join(format!(".journal.jsonl.lords-new-1-{cut}-0")), &recovered[..cut])
            .unwrap();
        journal::append(home, "first", serde_json::json!({})).unwrap();
        journal::append(home, "second", serde_json::json!({})).unwrap();
        let events = journal::read(home).unwrap_or_else(|e| panic!("cut {cut}: {e}"));
        let kinds: Vec<&str> = events.iter().map(|e| e["event"].as_str().unwrap()).collect();
        assert_eq!(kinds, ["brief", "journal_gap", "first", "second"], "cut {cut}");
        let left: Vec<_> = fs::read_dir(home)
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().contains("lords-new"))
            .collect();
        assert!(left.is_empty(), "cut {cut}: staged copies left: {left:?}");
    }
}
