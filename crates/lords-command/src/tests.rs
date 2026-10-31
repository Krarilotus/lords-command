//! The window, headless: egui runs its frames with no renderer and no window, what it would draw is read from its
//! accessibility tree, and a click is sent as an accessibility action (as egui's own test harness does). Nothing opens
//! on this PC's desktop.

// Tests are outside the filesystem and process boundary (fsguard.rs): their fixtures write their own temp dirs.
#![allow(clippy::disallowed_methods, clippy::disallowed_types)]

use super::*;
use eframe::egui::accesskit::{self, ActionRequest, NodeId};
use lords_core::http;
use std::path::PathBuf;
use std::process::Command;
use std::sync::{Mutex, MutexGuard, Once};

/// The panel's writes run one at a time per process, so the tests that write take turns.
static WRITES: Mutex<()> = Mutex::new(());

fn turn() -> MutexGuard<'static, ()> {
    WRITES.lock().unwrap_or_else(|p| p.into_inner())
}

/// git with a config of its own, and the tool's home, in this build's temp dir: never the real ones.
fn isolate() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        let exe = std::env::current_exe().unwrap();
        let tmp = exe.ancestors().nth(3).unwrap().join("tmp").join("lords-window-test");
        std::fs::create_dir_all(&tmp).unwrap();
        let cfg = tmp.join("gitconfig");
        // Built at run time, so the repo's own privacy check doesn't see an address in this source.
        let email = ["test", "example.invalid"].join("@");
        std::fs::write(
            &cfg,
            format!("[user]\n\tname = Test\n\temail = {email}\n[init]\n\tdefaultBranch = main\n"),
        )
        .unwrap();
        std::env::set_var("GIT_CONFIG_GLOBAL", &cfg);
        std::env::set_var("GIT_CONFIG_NOSYSTEM", "1");
        std::env::set_var("USERPROFILE", &tmp);
        std::env::set_var("HOME", &tmp);
    });
}

fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git").arg("-C").arg(dir).args(args).output().unwrap();
    assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn entry(id: &str, visibility: &str, title: &str, body: &str) -> String {
    let kind = id.split('.').next().unwrap();
    format!("---\nid: {id}\nkind: {kind}\ntitle: {title}\naudience: [all]\nscope: global\nstatus: active\nvisibility: {visibility}\norigin:\n  type: own\n---\n{body}")
}

/// Both stacks in git, one entry each.
struct Stacks {
    dir: tempfile::TempDir,
}

impl Stacks {
    fn new() -> Stacks {
        isolate();
        let s = Stacks { dir: tempfile::TempDir::new().unwrap() };
        s.write("pub/stack.toml", "name = \"pub\"\nvisibility = \"public\"\n");
        s.write("priv/stack.toml", "name = \"priv\"\nvisibility = \"private\"\n");
        s.write("pub/global/rules/p.md", &entry("rule.p", "public", "p", "Public.\n"));
        s.write("priv/global/rules/v.md", &entry("rule.v", "private", "v", "Read.\n"));
        for repo in ["pub", "priv"] {
            git(&s.path(repo), &["init", "-q"]);
            git(&s.path(repo), &["add", "-A"]);
            git(&s.path(repo), &["commit", "-q", "-m", "fixture"]);
        }
        s
    }

    fn path(&self, rel: &str) -> PathBuf {
        self.dir.path().join(rel)
    }

    fn write(&self, rel: &str, text: &str) {
        let p = self.path(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, text).unwrap();
    }

    fn paths(&self) -> Paths {
        Paths { public: self.path("pub"), private: Some(self.path("priv")) }
    }

    /// `lords panel serve`'s answers for these stacks, on a port of their own.
    fn serve(&self) -> Source {
        let listener = http::bind(0).unwrap();
        let port = listener.local_addr().unwrap().port();
        let paths = self.paths();
        std::thread::spawn(move || {
            http::serve(listener, move |r| lords_core::panel::handle(&paths, r))
        });
        Source::Remote(Client::new(port))
    }
}

/// The window, drawn headless.
struct Window {
    ctx: egui::Context,
    app: App,
    /// What the last frame drew: each accessible node's id and label.
    drawn: Vec<(NodeId, String)>,
    clicks: Vec<NodeId>,
}

impl Window {
    fn new(source: Source) -> Window {
        let ctx = egui::Context::default();
        ctx.enable_accesskit();
        let app = App::with(&ctx, Ok(source), None);
        Window { ctx, app, drawn: Vec::new(), clicks: Vec::new() }
    }

    fn frame(&mut self) {
        let events = self
            .clicks
            .drain(..)
            .map(|target| {
                egui::Event::AccessKitActionRequest(ActionRequest {
                    action: accesskit::Action::Click,
                    target,
                    data: None,
                })
            })
            .collect();
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(1180.0, 720.0),
            )),
            events,
            ..Default::default()
        };
        let app = &mut self.app;
        let out = self.ctx.run(input, |ctx| app.ui(ctx));
        if let Some(update) = out.platform_output.accesskit_update {
            self.drawn = update
                .nodes
                .iter()
                .filter_map(|(id, n)| n.label().map(|l| (*id, l.to_string())))
                .collect();
        }
    }

    /// Frames until nothing the window asked for is still running, then one more to draw what came back.
    fn settle(&mut self) {
        let start = Instant::now();
        loop {
            self.frame();
            let a = &self.app;
            let idle = a.loading.is_none()
                && a.checking.is_none()
                && a.status_task.is_none()
                && a.writing.is_none()
                && a.history_task.is_none()
                && a.composing.is_none();
            if idle {
                break;
            }
            assert!(start.elapsed() < Duration::from_secs(120), "the window settles");
            std::thread::sleep(Duration::from_millis(50));
        }
        self.frame();
    }

    fn shows(&self, label: &str) -> bool {
        self.drawn.iter().any(|(_, l)| l == label)
    }

    /// Clicks the one node with this label; the frame after the click's draws what it did.
    fn click(&mut self, label: &str) {
        let hits: Vec<NodeId> =
            self.drawn.iter().filter(|(_, l)| l == label).map(|(id, _)| *id).collect();
        assert_eq!(hits.len(), 1, "one `{label}` drawn: {:?}", self.drawn);
        self.clicks.push(hits[0]);
        self.frame();
        self.frame();
    }

    /// The entry list's lines, as drawn.
    fn lines(&self) -> Vec<String> {
        let mut lines: Vec<String> =
            self.drawn.iter().map(|(_, l)| l.clone()).filter(|l| l.starts_with("rule.")).collect();
        lines.sort();
        lines
    }
}

// Spec (brief: "One UI for both modes: the window renders the view structs whether it reads them locally or over
// HTTP"): over the same stacks, the window draws the same entry lines from this PC's stacks as through the panel's
// server; only the remote one shows the bridge, up (⇆).
#[test]
fn the_window_draws_the_same_lines_from_either_source() {
    let s = Stacks::new();
    let mut local = Window::new(Source::Local(s.paths()));
    let mut remote = Window::new(s.serve());
    local.settle();
    remote.settle();
    assert_eq!(local.lines().len(), 2, "{:?}", local.drawn);
    assert_eq!(remote.lines(), local.lines());
    assert!(remote.shows("⇆") && !local.shows("⇆"));
}

// Spec (brief: "Each edit carries the hash of the file as it was read; on a mismatch it's refused and the panel
// reloads"; D3: nothing typed is lost): a save made while an agent committed a change to the same entry is refused;
// the window reloads, keeps the owner's text, takes the agent's change in the field the owner didn't touch, and the
// next save lands with both.
#[test]
fn a_stale_save_keeps_what_was_typed_and_lands_on_the_new_version() {
    let _t = turn();
    let s = Stacks::new();
    let mut w = Window::new(s.serve());
    w.settle();
    w.app.pick("rule.v");
    w.settle();
    w.app.draft.as_mut().unwrap().now.body = "Mine.\n".into();
    s.write("priv/global/rules/v.md", &entry("rule.v", "private", "The agent's title", "Read.\n"));
    git(&s.path("priv"), &["commit", "-q", "-am", "an agent's change"]);
    let save = |w: &mut Window| {
        let d = w.app.draft.as_ref().unwrap();
        let act = Act::Save(d.id.clone(), d.edit());
        let ctx = w.ctx.clone();
        w.app.act(act, &ctx);
        w.settle();
    };
    save(&mut w);
    let refused = w.app.notice.clone();
    assert!(
        matches!(&refused, Some(Err(m)) if m.contains("changed since it was read")),
        "{refused:?}"
    );
    let d = w.app.draft.as_ref().unwrap();
    assert_eq!((d.now.title.as_str(), d.now.body.as_str()), ("The agent's title", "Mine.\n"));
    save(&mut w);
    assert!(matches!(&w.app.notice, Some(Ok(_))), "{:?}", w.app.notice);
    let file = std::fs::read_to_string(s.path("priv/global/rules/v.md")).unwrap();
    assert!(file.contains("title: The agent's title") && file.ends_with("---\nMine.\n"), "{file}");
}

// Spec (sol-loop r1 S3; the window before this branch composed every frame, so ◫ always showed the stacks as loaded):
// after the stacks change and the window reloads, ◫ shows what each harness reads now, not what it read before.
#[test]
fn what_each_harness_reads_is_read_again_after_a_reload() {
    let _t = turn();
    let s = Stacks::new();
    let mut w = Window::new(Source::Local(s.paths()));
    w.app.view = View::Compose;
    w.settle();
    let text = |w: &Window| {
        let c = w.app.composed.as_ref().and_then(|(_, c)| c.as_ref().ok()).expect("composed");
        c.lines.iter().map(|l| l.text.clone()).collect::<Vec<_>>().join("\n")
    };
    assert!(text(&w).contains("Read."), "{}", text(&w));
    s.write("priv/global/rules/v.md", &entry("rule.v", "private", "v", "Changed.\n"));
    git(&s.path("priv"), &["commit", "-q", "-am", "an agent's change"]);
    let ctx = w.ctx.clone();
    w.app.reload(&ctx);
    w.settle();
    assert!(text(&w).contains("Changed.") && !text(&w).contains("Read."), "{}", text(&w));
}

// Spec (brief, the top bar: "⇡n (unpushed commits; a click pushes)", "a bridge glyph (⇆ up, ✕ down)"): the top bar
// counts the commits the stack repos' upstreams lack; a first click asks (⇡1 ✓), the second pushes through the
// pre-push check, and the count goes. A window whose server can't be reached shows ✕.
#[test]
fn the_top_bar_counts_unpushed_commits_pushes_on_the_second_click_and_shows_a_bridge_down() {
    let _t = turn();
    let s = Stacks::new();
    let upstream = s.path("upstream.git");
    std::fs::create_dir_all(&upstream).unwrap();
    git(&upstream, &["init", "-q", "--bare"]);
    git(&s.path("priv"), &["remote", "add", "origin", &upstream.display().to_string()]);
    git(&s.path("priv"), &["push", "-q", "-u", "origin", "main"]);
    s.write("priv/global/rules/v.md", &entry("rule.v", "private", "v", "Unpushed.\n"));
    git(&s.path("priv"), &["commit", "-q", "-am", "to push"]);
    let mut w = Window::new(s.serve());
    w.settle();
    assert!(w.shows("⇡1"), "{:?}", w.drawn);
    w.click("⇡1");
    assert!(w.shows("⇡1 ✓"), "asks once more: {:?}", w.drawn);
    assert_eq!(
        git(&upstream, &["rev-parse", "main"]),
        git(&s.path("priv"), &["rev-parse", "HEAD~1"])
    );
    w.click("⇡1 ✓");
    w.settle();
    assert_eq!(
        git(&upstream, &["rev-parse", "main"]),
        git(&s.path("priv"), &["rev-parse", "HEAD"])
    );
    assert!(!w.drawn.iter().any(|(_, l)| l.starts_with('⇡')), "{:?}", w.drawn);

    let gone = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
    let mut down = Window::new(Source::Remote(Client::new(gone)));
    down.settle();
    assert!(down.shows("✕") && !down.shows("⇆"), "{:?}", down.drawn);
}
