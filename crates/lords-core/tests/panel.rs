//! The panel (design note 2026-10-31-lords-panel-remote): `lords panel serve`'s answers and the window's `Source` that
//! reads them, against the note's inputs to attack (D2). The invariant: a remote edit is exactly an edit made on the
//! PC that holds the stacks (checked, hooked, under that PC's git identity, never over a newer version), and nothing
//! leaves it beyond what the window shows.

// Tests are outside the filesystem boundary (fsguard.rs): their fixtures write their own temp dirs.
#![allow(clippy::disallowed_methods, clippy::disallowed_types)]

mod common;
use common::*;
use lords_core::http::{self, Client, Reply};
use lords_core::ops::Edit;
use lords_core::panel::{self, EditRequest, Failure, Paths, RetireRequest, Source};
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, Once};
use std::time::{Duration, Instant};

/// The panel's writes run one at a time per process, so the tests that write take turns.
static WRITES: Mutex<()> = Mutex::new(());

fn turn() -> MutexGuard<'static, ()> {
    WRITES.lock().unwrap_or_else(|p| p.into_inner())
}

/// The tool's home (sync's ledger, check's probe files) in this run's own dir, never the real one.
fn isolate_home() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        let home = Path::new(env!("CARGO_TARGET_TMPDIR")).join("lords-panel-home");
        std::fs::create_dir_all(&home).unwrap();
        std::env::set_var("USERPROFILE", &home);
        std::env::set_var("HOME", &home);
    });
}

/// Both stacks in git, one entry each, and `lords panel serve`'s answers on a port of their own.
struct Served {
    f: Fixture,
    port: u16,
    local: Source,
    remote: Source,
}

fn served() -> Served {
    isolate_home();
    let f = Fixture::new();
    f.write("pub/global/rules/p.md", &entry("rule.p", "global", "public", "", "Public.\n"));
    f.write("priv/global/rules/v.md", &entry("rule.v", "global", "private", "", "Read.\n"));
    f.git_init("pub");
    f.git_init("priv");
    let paths = Paths { public: f.pub_root(), private: Some(f.priv_root()) };
    let listener = http::bind(0).unwrap();
    let port = listener.local_addr().unwrap().port();
    let served = paths.clone();
    std::thread::spawn(move || http::serve(listener, move |r| panel::handle(&served, r)));
    Served { f, port, local: Source::Local(paths), remote: Source::Remote(Client::new(port)) }
}

fn version(s: &Source, id: &str) -> String {
    s.stacks().unwrap().entries.into_iter().find(|e| e.id == id).unwrap().version
}

fn body_edit(id: &str, body: &str, base: Option<String>) -> EditRequest {
    EditRequest {
        id: id.into(),
        edit: Edit { body: Some(body.into()), base, ..Default::default() },
    }
}

fn hook(f: &Fixture, repo: &str, name: &str, script: &str) {
    f.write(&format!("{repo}/.git/hooks/{name}"), &format!("#!/bin/sh\n{script}\n"));
}

/// Sends `bytes` as they are and reads the reply: its status and text.
fn raw(port: u16, bytes: &[u8]) -> (u16, String) {
    let mut s = TcpStream::connect(("127.0.0.1", port)).unwrap();
    s.set_read_timeout(Some(Duration::from_secs(30))).unwrap();
    let _ = s.write_all(bytes);
    let mut out = Vec::new();
    let _ = s.read_to_end(&mut out);
    let text = String::from_utf8_lossy(&out).into_owned();
    (text.split(' ').nth(1).and_then(|c| c.parse().ok()).unwrap_or(0), text)
}

fn wait_until(what: &str, done: impl Fn() -> bool) {
    let start = Instant::now();
    while !done() {
        assert!(start.elapsed() < Duration::from_secs(60), "{what} within 60 s");
        std::thread::sleep(Duration::from_millis(50));
    }
}

// Spec (brief: "The window renders the view structs whether it reads them locally or over HTTP"): every read the
// window makes gives the same view through the server as here.
#[test]
fn the_window_reads_the_same_views_here_and_through_the_server() {
    let s = served();
    assert_eq!(s.remote.stacks().unwrap(), s.local.stacks().unwrap());
    assert_eq!(s.remote.findings().unwrap(), s.local.findings().unwrap());
    assert_eq!(s.remote.compose("claude", None).unwrap(), s.local.compose("claude", None).unwrap());
    assert_eq!(s.remote.plan(None).unwrap(), s.local.plan(None).unwrap());
    let history = s.local.history("rule.v").unwrap();
    assert_eq!(s.remote.history("rule.v").unwrap(), history);
    let first = &history[0].hash;
    assert_eq!(s.remote.patch("rule.v", first).unwrap(), s.local.patch("rule.v", first).unwrap());
    assert_eq!(s.remote.status().unwrap(), s.local.status().unwrap());
}

// Spec (D2.1; brief: "Each edit carries the hash of the file as it was read; on a mismatch it's refused", "committed
// through the stack repo's hooks under the PC's git identity"): through the server, an edit made from a version an
// agent's commit has replaced is refused and changes nothing, an edit carrying no version is refused, and one made
// from the version there now is committed by the serving PC's own git identity (the request names none).
#[test]
fn a_stale_edit_is_refused_and_a_fresh_one_is_committed_under_this_pcs_identity() {
    let _t = turn();
    let s = served();
    let read = version(&s.remote, "rule.v");
    let agents = entry("rule.v", "global", "private", "", "The agent's.\n");
    s.f.write("priv/global/rules/v.md", &agents);
    git(&s.f.priv_root(), &["commit", "-q", "-am", "an agent's change"]);
    let stale = s.remote.edit(&body_edit("rule.v", "Mine.\n", Some(read)));
    assert!(
        matches!(&stale, Err(Failure::Refused(m)) if m.contains("changed since it was read")),
        "{stale:?}"
    );
    let unversioned = s.remote.edit(&body_edit("rule.v", "Mine.\n", None));
    assert!(matches!(&unversioned, Err(Failure::Refused(_))), "{unversioned:?}");
    assert_eq!(s.f.read("priv/global/rules/v.md"), agents);

    let now = version(&s.remote, "rule.v");
    let saved = s.remote.edit(&body_edit("rule.v", "Mine.\n", Some(now))).unwrap();
    assert!(saved.commit.is_some());
    assert!(s.f.read("priv/global/rules/v.md").ends_with("---\nMine.\n"));
    let last = git(&s.f.priv_root(), &["log", "-1", "--format=%an|%s"]);
    assert_eq!(last.trim(), "Test|Edit rule.v");
}

// Spec (D2.1, "checked, hooked"): a write whose commit the stack repo's hooks refuse leaves nothing behind, neither in
// the files nor staged in the index, and its refusal carries the hook's words; an edit and a retirement alike.
#[test]
fn a_write_the_hooks_refuse_leaves_nothing_behind() {
    let _t = turn();
    let s = served();
    hook(
        &s.f,
        "priv",
        "pre-commit",
        "if [ -f \"$(git rev-parse --git-dir)/refuse\" ]; then echo 'refused by the hook' >&2; exit 1; fi",
    );
    s.f.write("priv/.git/refuse", "");
    let before = s.f.read("priv/global/rules/v.md");
    let base = version(&s.remote, "rule.v");
    let edited = s.remote.edit(&body_edit("rule.v", "Mine.\n", Some(base.clone())));
    let retired =
        s.remote.retire(&RetireRequest { id: "rule.v".into(), base, superseded_by: None });
    for r in [edited.map(|_| ()), retired.map(|_| ())] {
        assert!(
            matches!(&r, Err(Failure::Refused(m)) if m.contains("refused by the hook") && m.contains("nothing changed")),
            "{r:?}"
        );
    }
    assert_eq!(s.f.read("priv/global/rules/v.md"), before);
    assert_eq!(git(&s.f.priv_root(), &["status", "--porcelain"]), "");
    assert_eq!(git(&s.f.priv_root(), &["log", "-1", "--format=%s"]).trim(), "fixture");
}

// Spec (brief: "One write at a time"): while one write runs (its commit's hook takes a while), a second is refused at
// once rather than queued, and goes through once the first is done.
#[test]
fn a_second_write_is_refused_while_one_runs() {
    let _t = turn();
    let s = served();
    hook(&s.f, "priv", "pre-commit", "sleep 3");
    let (first, second) = (version(&s.remote, "rule.v"), version(&s.remote, "rule.p"));
    let remote = s.remote.clone();
    let running =
        std::thread::spawn(move || remote.edit(&body_edit("rule.v", "First.\n", Some(first))));
    wait_until("the first write's file change", || {
        s.f.read("priv/global/rules/v.md").ends_with("First.\n")
    });
    let refused = s.remote.edit(&body_edit("rule.p", "Second.\n", Some(second.clone())));
    assert!(
        matches!(&refused, Err(Failure::Refused(m)) if m.contains("another write is running")),
        "{refused:?}"
    );
    assert!(running.join().unwrap().is_ok());
    assert!(s.remote.edit(&body_edit("rule.p", "Second.\n", Some(second))).is_ok());
}

// Spec (D2.2, "a body at the size limits"): an edit whose request is exactly `MAX_BODY` bytes lands whole, its text
// byte for byte; one byte more is refused before anything is sent, and nothing changes. The server itself refuses a
// head past `MAX_HEAD` and a declared body past `MAX_BODY` before reading either.
#[test]
fn the_size_limits_hold_to_the_byte() {
    let _t = turn();
    let s = served();
    let base = version(&s.remote, "rule.v");
    let size = |n: usize| {
        serde_json::to_vec(&body_edit("rule.v", &"x".repeat(n), Some(base.clone()))).unwrap().len()
    };
    let n = http::MAX_BODY - size(0);
    assert_eq!(size(n), http::MAX_BODY);
    let before = s.f.read("priv/global/rules/v.md");
    let over = s.remote.edit(&body_edit("rule.v", &"x".repeat(n + 1), Some(base.clone())));
    assert!(
        matches!(&over, Err(Failure::Refused(m)) if m.contains("nothing was sent")),
        "{over:?}"
    );
    assert_eq!(s.f.read("priv/global/rules/v.md"), before);
    s.remote.edit(&body_edit("rule.v", &"x".repeat(n), Some(base))).unwrap();
    let entries = s.local.stacks().unwrap().entries;
    let body = entries.into_iter().find(|e| e.id == "rule.v").unwrap().body;
    assert_eq!(body, "x".repeat(n));

    let head = format!(
        "GET /stacks HTTP/1.1\r\nHost: 127.0.0.1\r\nX: {}\r\n\r\n",
        "y".repeat(http::MAX_HEAD)
    );
    assert_eq!(raw(s.port, head.as_bytes()).0, 431);
    let declared = format!(
        "POST /edit HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n",
        http::MAX_BODY + 1
    );
    assert_eq!(raw(s.port, declared.as_bytes()).0, 413);
}

// Spec (D2.3; brief: "a request from outside loopback, and a forged Host header"): the server listens on 127.0.0.1
// alone, and no request a browser page could make reaches its handler: another Host (a rebound DNS name,
// `localhost`), none or two, an Origin, Referer or Sec-Fetch header, a POST that isn't JSON (a page's "simple"
// cross-site POST), an OPTIONS preflight, a chunked body. A plain request to 127.0.0.1 does.
#[test]
fn only_this_pcs_own_requests_are_answered_and_never_a_browsers() {
    let listener = http::bind(0).unwrap();
    let addr = listener.local_addr().unwrap();
    assert!(addr.ip().is_loopback());
    let calls = Arc::new(AtomicUsize::new(0));
    let counted = calls.clone();
    std::thread::spawn(move || {
        http::serve(listener, move |_| {
            counted.fetch_add(1, Ordering::SeqCst);
            Reply::json(&"ok")
        })
    });
    // From another address of this PC nothing listens (where it has one: the address a packet out would leave by).
    let outward = std::net::UdpSocket::bind("0.0.0.0:0")
        .and_then(|u| u.connect("192.0.2.1:9").and_then(|()| u.local_addr()))
        .ok()
        .map(|a| a.ip())
        .filter(|ip| !ip.is_loopback() && !ip.is_unspecified());
    if let Some(ip) = outward {
        let tried = TcpStream::connect_timeout(&(ip, addr.port()).into(), Duration::from_secs(2));
        assert!(tried.is_err(), "reached through {ip}");
    }
    let port = addr.port();
    let get = |headers: &str| format!("GET /stacks HTTP/1.1\r\n{headers}\r\n");
    let host = format!("Host: 127.0.0.1:{port}\r\n");
    let refused = [
        get(&format!("Host: evil.example:{port}\r\n")),
        get(&format!("Host: localhost:{port}\r\n")),
        get(""),
        get(&format!("{host}{host}")),
        get(&format!("{host}Origin: http://evil.example\r\n")),
        get(&format!("{host}Referer: http://evil.example/\r\n")),
        get(&format!("{host}Sec-Fetch-Site: cross-site\r\n")),
        format!(
            "POST /edit HTTP/1.1\r\n{host}Content-Type: text/plain\r\nContent-Length: 2\r\n\r\n{{}}"
        ),
        format!("OPTIONS /edit HTTP/1.1\r\n{host}\r\n"),
        format!("POST /edit HTTP/1.1\r\n{host}Content-Type: application/json\r\nTransfer-Encoding: chunked\r\n\r\n0\r\n\r\n"),
    ];
    for r in &refused {
        let (status, text) = raw(port, r.as_bytes());
        assert!((400..500).contains(&status), "{r:?} got {text}");
    }
    assert_eq!(calls.load(Ordering::SeqCst), 0, "no refused request reached the handler");
    assert_eq!(raw(port, get(&host).as_bytes()).0, 200);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

// Spec (D2.4, "the tunnel dropping mid-write"): a write whose connection drops once its request is sent lands whole
// (committed, its text exact, nothing left uncommitted). The window tells a write that may have landed from a server
// it never reached: a reply that doesn't come back is `Lost`; a port where nothing listens is `Unreachable`.
#[test]
fn a_write_whose_connection_drops_lands_whole_and_the_window_says_it_may_have() {
    let _t = turn();
    let s = served();
    let edit = body_edit("rule.v", "Dropped.\n", Some(version(&s.remote, "rule.v")));
    let body = serde_json::to_string(&edit).unwrap();
    let request = format!(
        "POST /edit HTTP/1.1\r\nHost: 127.0.0.1:{}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
        s.port,
        body.len()
    );
    let mut dropped = TcpStream::connect(("127.0.0.1", s.port)).unwrap();
    dropped.write_all(request.as_bytes()).unwrap();
    drop(dropped);
    wait_until("the dropped write's commit", || {
        git(&s.f.priv_root(), &["log", "-1", "--format=%s"]).trim() == "Edit rule.v"
    });
    assert!(s.f.read("priv/global/rules/v.md").ends_with("---\nDropped.\n"));
    assert_eq!(git(&s.f.priv_root(), &["status", "--porcelain"]), "");

    // A server that takes the whole request and closes without a reply.
    let silent = TcpListener::bind("127.0.0.1:0").unwrap();
    let silent_port = silent.local_addr().unwrap().port();
    std::thread::spawn(move || {
        for mut c in silent.incoming().flatten() {
            let _ = http::read_request(&mut c);
        }
    });
    let lost = Source::Remote(Client::new(silent_port)).edit(&body_edit(
        "rule.v",
        "x\n",
        Some("v".into()),
    ));
    assert!(matches!(lost, Err(Failure::Lost(_))), "{lost:?}");
    let gone = TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
    let unreachable = Source::Remote(Client::new(gone)).stacks();
    assert!(matches!(unreachable, Err(Failure::Unreachable(_))), "{unreachable:?}");
}

// Spec (D2.5; brief: "Push goes through the pre-push privacy check"): the status counts the commits each stack repo's
// upstream lacks; a push its pre-push hook refuses pushes nothing and says the hook's words, and the count stands;
// once the hook lets it through, the upstream has them and the count is 0. A repo with no upstream counts nothing.
#[test]
fn a_push_the_privacy_check_refuses_pushes_nothing() {
    use lords_core::model::Visibility;
    let _t = turn();
    let s = served();
    let remote = s.f.path("remote.git");
    std::fs::create_dir_all(&remote).unwrap();
    git(&remote, &["init", "-q", "--bare"]);
    git(&s.f.priv_root(), &["remote", "add", "origin", &remote.display().to_string()]);
    git(&s.f.priv_root(), &["push", "-q", "-u", "origin", "main"]);
    s.remote.edit(&body_edit("rule.v", "To push.\n", Some(version(&s.remote, "rule.v")))).unwrap();
    let ahead = |s: &Served| {
        let status = s.remote.status().unwrap();
        let of = |v: Visibility| status.repos.iter().find(|r| r.stack == v).unwrap();
        let public = of(Visibility::Public);
        assert_eq!((public.upstream.as_deref(), public.commits.len()), (None, 0));
        (of(Visibility::Private).commits.len(), status.unpushed())
    };
    assert_eq!(ahead(&s), (1, 1));
    let upstream = || git(&remote, &["rev-parse", "main"]);
    let before = upstream();
    hook(&s.f, "priv", "pre-push", "echo 'privacy: a private detail in the push' >&2; exit 1");
    let refused = s.remote.push();
    assert!(
        matches!(&refused, Err(Failure::Refused(m)) if m.contains("a private detail in the push")),
        "{refused:?}"
    );
    assert_eq!(upstream(), before);
    assert_eq!(ahead(&s), (1, 1));
    std::fs::remove_file(s.f.path("priv/.git/hooks/pre-push")).unwrap();
    s.remote.push().unwrap();
    assert_eq!(upstream(), git(&s.f.priv_root(), &["rev-parse", "HEAD"]));
    assert_eq!(ahead(&s), (0, 0));
}

// Spec (D2.8): what a request names reaches git only as what it must be: a commit only as a hash (never an option
// such as `--output=`), an entry only if it is loaded (its file comes from the entry), an adapter and a place only if
// they exist and are declared.
#[test]
fn request_fields_never_steer_git() {
    let s = served();
    let out = s.f.path("priv/written-by-git");
    let option = format!("--output={}", out.display());
    let r = s.remote.patch("rule.v", &option);
    assert!(matches!(&r, Err(Failure::Refused(m)) if m.contains("no commit hash")), "{r:?}");
    let target = http::target("/patch", &[("id", "rule.v"), ("commit", &option)]);
    let request = format!("GET {target} HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n");
    assert_eq!(raw(s.port, request.as_bytes()).0, 409);
    assert!(!out.exists());
    assert!(matches!(s.remote.history("rule.none"), Err(Failure::Refused(_))));
    assert!(matches!(s.remote.compose("nope", None), Err(Failure::Refused(_))));
    assert!(matches!(s.remote.compose("claude", Some("not-declared")), Err(Failure::Refused(_))));
}
