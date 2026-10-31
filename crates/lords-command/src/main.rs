//! Lords Command's window: browse the stacks' entries, see what `check` finds, edit and retire, see what each
//! harness reads in each place and where every line comes from, and sync behind a confirmation.
//!
//! It renders `lords_core::view`'s structs and nothing else, read through `panel::Source`: from this PC's stacks, or,
//! with `api` (and `tunnel`) in config.txt, from `lords panel serve` on the PC that holds them. One UI for both; every
//! read and write runs on a thread (`work::Task`), so a slow bridge never holds a frame.
//!
//! Renderer-agnostic by rule (owner, 25 Oct: one native app keeps the egui UI code; its renderer is chosen by
//! measurement, not assumption): draw only through egui's painter and widgets. No glow, wgpu or GPU calls, no paint
//! callbacks, no custom shaders or textures; the renderer is picked in Cargo.toml alone.
#![windows_subsystem = "windows"]

#[cfg(test)]
mod tests;
mod ui;
mod work;

use eframe::egui::{self, Button, ComboBox, FontId, RichText, ScrollArea, TextEdit};
use lords_core::config::Config;
use lords_core::finding::Severity;
use lords_core::git::Commit;
use lords_core::model::{Audience, Kind, Origin, Scope, Visibility};
use lords_core::ops::Edit;
use lords_core::panel::{EditRequest, Failure, Paths, RetireRequest, Source};
use lords_core::process::Tunnel;
use lords_core::sync::{self, Action};
use lords_core::view::{ComposedView, EntryView, FindingsView, PlanView, StacksView, StatusView};
use std::collections::BTreeMap;
use std::path::Path;
use std::time::{Duration, Instant};
use ui::{dim, mono, Line, AMBER, BG, BLUE, GREEN, RED, TEXT};
use work::Task;

const NAME: &str = env!("CARGO_PKG_NAME");
const AUDIENCES: [(Audience, &str, &str); 4] = [
    (Audience::All, "∀", "all agents"),
    (Audience::Opus, "O", "Opus"),
    (Audience::Sol, "S", "Sol"),
    (Audience::Astra, "A", "Astra"),
];
/// How often the top bar's state (the bridge, unpushed commits) is read again; after a failed load, how soon.
const POLL: Duration = Duration::from_secs(10);
const RETRY: Duration = Duration::from_secs(3);

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum View {
    Entries,
    Findings,
    Sync,
    Compose,
}

/// The picked entry's editable fields, as loaded and as edited.
#[derive(Clone, PartialEq, Debug)]
struct Fields {
    title: String,
    audience: Vec<Audience>,
    topics: Vec<String>,
    roles: Vec<String>,
    body: String,
}

#[derive(Debug)]
struct Draft {
    id: String,
    /// The version `loaded` came from: a save carries it.
    version: String,
    loaded: Fields,
    now: Fields,
}

impl Draft {
    fn of(e: &EntryView) -> Draft {
        Draft { id: e.id.clone(), version: e.version.clone(), loaded: fields(e), now: fields(e) }
    }

    /// Moves the draft onto the entry as it is now: each field the owner hasn't touched takes what is there now, each
    /// one the owner changed keeps the owner's words. Nothing typed is lost, and a save is made from the new version.
    fn rebase(&mut self, e: &EntryView) {
        if e.version == self.version {
            return;
        }
        let new = fields(e);
        fn take<T: Clone + PartialEq>(now: &mut T, loaded: &T, new: &T) {
            if now == loaded {
                *now = new.clone();
            }
        }
        take(&mut self.now.title, &self.loaded.title, &new.title);
        take(&mut self.now.audience, &self.loaded.audience, &new.audience);
        take(&mut self.now.topics, &self.loaded.topics, &new.topics);
        take(&mut self.now.roles, &self.loaded.roles, &new.roles);
        take(&mut self.now.body, &self.loaded.body, &new.body);
        (self.loaded, self.version) = (new, e.version.clone());
    }

    fn edit(&self) -> Edit {
        let (d, l) = (&self.now, &self.loaded);
        Edit {
            title: (d.title != l.title).then(|| d.title.clone()),
            audience: (d.audience != l.audience)
                .then(|| d.audience.iter().map(|a| audience_name(*a)).collect()),
            topics: (d.topics != l.topics).then(|| d.topics.clone()),
            roles: (d.roles != l.roles).then(|| d.roles.clone()),
            body: (d.body != l.body).then(|| d.body.clone()),
            base: Some(self.version.clone()),
        }
    }
}

/// What a click in the detail asks for; carried out after drawing.
enum Act {
    Pick(String),
    Save(String, Edit),
    Retire(String, String, Option<String>),
}

type Got<T> = Result<T, Failure>;
/// Composed text is asked for one adapter in one place (none: the user level).
type ComposeKey = (String, String);

struct App {
    source: Result<Source, String>,
    tunnel: Option<Tunnel>,
    stacks: Option<StacksView>,
    loading: Option<Task<Got<StacksView>>>,
    load_error: Option<Failure>,
    load_failed: Option<Instant>,
    findings: FindingsView,
    worst: BTreeMap<String, Severity>,
    checking: Option<Task<Got<FindingsView>>>,
    status: Option<Got<StatusView>>,
    status_task: Option<Task<Got<StatusView>>>,
    status_at: Option<Instant>,
    /// Remote only: whether the last call reached the panel's server, and why not.
    bridge: Option<Result<(), String>>,
    view: View,
    kinds: [bool; 8],
    audience: [bool; 4],
    layer: String,
    search: String,
    retired: bool,
    picked: Option<String>,
    draft: Option<Draft>,
    successor: String,
    history: Option<(String, Option<Got<Vec<Commit>>>)>,
    history_task: Option<Task<Got<Vec<Commit>>>>,
    patch: Option<(String, Option<Got<String>>)>,
    patch_task: Option<Task<Got<String>>>,
    notice: Option<Result<String, String>>,
    writing: Option<Task<Got<String>>>,
    confirm_push: bool,
    plan_repo: String,
    plan: Option<Got<PlanView>>,
    planning: Option<Task<Got<PlanView>>>,
    plan_pick: Option<usize>,
    confirm_apply: bool,
    applying: Option<Task<Result<Vec<String>, String>>>,
    compose_adapter: String,
    compose_repo: String,
    composed: Option<(ComposeKey, Got<ComposedView>)>,
    composing: Option<(ComposeKey, Task<Got<ComposedView>>)>,
}

fn fields(e: &EntryView) -> Fields {
    Fields {
        title: e.title.clone(),
        audience: e.audience.clone(),
        topics: e.topics.clone(),
        roles: e.roles.clone(),
        body: e.body.clone(),
    }
}

fn audience_name(a: Audience) -> String {
    serde_json::to_value(a).ok().and_then(|v| v.as_str().map(str::to_string)).unwrap_or_default()
}

/// Opens a source in the browser (or a path in Explorer).
fn open(target: &str) {
    lords_core::process::open_in_file_manager(target);
}

fn hover_of(e: &EntryView) -> String {
    let mut h = format!("{}\n{}\n{} · {} · {}", e.id, e.title, e.stack, e.scope, e.kind.name());
    if !e.roles.is_empty() {
        h.push_str(&format!("\nfor: {}", e.roles.join(", ")));
    }
    match &e.origin {
        Origin::Own => h.push_str("\nown"),
        Origin::Adapted(s) => h.push_str(&format!(
            "\nadapted from {} @ {} ({}), {}",
            s.repo, s.commit, s.path, s.licence
        )),
        Origin::Inherited(s) => h.push_str(&format!(
            "\ninherited from {} @ {} ({}), {}",
            s.repo, s.commit, s.path, s.licence
        )),
        Origin::InRepo { repo, path, sha256 } => h.push_str(&format!(
            "\ntracked in repo {repo} at {path} (sha256 {})",
            &sha256[..12.min(sha256.len())]
        )),
        Origin::Imported { store, file } => {
            h.push_str(&format!("\nimported from memory store {store}: {file}"))
        }
    }
    if !e.active {
        h.push_str("\nretired");
        if let Some(r) = &e.retired {
            h.push_str(&format!(" {}: {}", r.date, r.reason));
        }
    }
    h.push_str(&format!("\n{}", e.file.display()));
    h
}

fn scope_of(e: &EntryView) -> Scope {
    Scope::parse(&e.scope).unwrap_or(Scope::Global)
}

fn layer_key(e: &EntryView) -> String {
    match scope_of(e) {
        Scope::Global => format!("global · {}", e.stack),
        _ => e.scope.clone(),
    }
}

impl App {
    fn new(ctx: &egui::Context) -> App {
        let (source, tunnel) = match Config::load() {
            Err(e) => (Err(e), None),
            Ok(cfg) => match (&cfg.api, cfg.public) {
                (Some(api), _) => {
                    // The panel's token is in the file beside config.txt (review r1 R2).
                    let dir = cfg.source.as_deref().and_then(Path::parent).unwrap_or(Path::new("."));
                    (lords_core::panel::remote(api, dir), cfg.tunnel.as_deref().map(Tunnel::new))
                }
                (None, Some(public)) => (Ok(Source::Local(Paths { public, private: cfg.private })), None),
                (None, None) => (
                    Err("No stacks yet. Put a config.txt beside lords-command.exe or in ~/.lords-command/ with\n  public = <the public stack dir>\n  private = <your private stack dir>\nor, on another PC,\n  api = http://127.0.0.1:<port>\n  tunnel = <ssh alias>"
                        .to_string()),
                    None,
                ),
            },
        };
        let mut app = App::with(ctx, source, tunnel);
        // `--view entries|findings|sync|compose`, `--pick <id>`, `--adapter <id>`, `--repo <name>`: open the
        // window at a place.
        let args: Vec<String> = std::env::args().skip(1).collect();
        for pair in args.windows(2) {
            match (pair[0].as_str(), pair[1].as_str()) {
                ("--view", "findings") => app.view = View::Findings,
                ("--view", "sync") => app.view = View::Sync,
                ("--view", "compose") => app.view = View::Compose,
                ("--pick", id) => app.picked = Some(id.to_string()),
                ("--adapter", id) => app.compose_adapter = id.to_string(),
                ("--repo", r) => (app.compose_repo, app.plan_repo) = (r.to_string(), r.to_string()),
                _ => {}
            }
        }
        app
    }

    fn with(ctx: &egui::Context, source: Result<Source, String>, tunnel: Option<Tunnel>) -> App {
        let mut app = App {
            source,
            tunnel,
            stacks: None,
            loading: None,
            load_error: None,
            load_failed: None,
            findings: FindingsView::default(),
            worst: BTreeMap::new(),
            checking: None,
            status: None,
            status_task: None,
            status_at: None,
            bridge: None,
            view: View::Entries,
            kinds: [true; 8],
            audience: [true; 4],
            layer: String::new(),
            search: String::new(),
            retired: false,
            picked: None,
            draft: None,
            successor: String::new(),
            history: None,
            history_task: None,
            patch: None,
            patch_task: None,
            notice: None,
            writing: None,
            confirm_push: false,
            plan_repo: String::new(),
            plan: None,
            planning: None,
            plan_pick: None,
            confirm_apply: false,
            applying: None,
            compose_adapter: "claude".into(),
            compose_repo: String::new(),
            composed: None,
            composing: None,
        };
        if let Some(t) = &mut app.tunnel {
            t.ensure();
        }
        app.reload(ctx);
        app
    }

    fn remote(&self) -> bool {
        self.source.as_ref().is_ok_and(Source::is_remote)
    }

    /// Notes whether a remote call reached the panel's server.
    fn track<T>(&mut self, got: &Got<T>) {
        if self.remote() {
            self.bridge = Some(match got {
                Err(Failure::Unreachable(m)) | Err(Failure::Lost(m)) => Err(m.clone()),
                _ => Ok(()),
            });
        }
    }

    fn start<T: Send + 'static>(
        &self,
        ctx: &egui::Context,
        call: impl FnOnce(&Source) -> T + Send + 'static,
    ) -> Option<Task<T>> {
        let source = self.source.as_ref().ok()?.clone();
        Some(Task::start(ctx, move || call(&source)))
    }

    /// Reads the stacks again, and runs `check` (with drift) and the push state beside it. Composed text is made from
    /// the stacks, so ◫ asks for it again (sol-loop r1 S3).
    fn reload(&mut self, ctx: &egui::Context) {
        (self.composed, self.composing) = (None, None);
        self.loading = self.start(ctx, |s| s.stacks());
        self.checking = self.start(ctx, |s| s.findings());
        self.start_status(ctx);
    }

    fn start_status(&mut self, ctx: &egui::Context) {
        if self.status_task.is_none() {
            self.status_task = self.start(ctx, |s| s.status());
            self.status_at = Some(Instant::now());
        }
    }

    fn pick(&mut self, id: &str) {
        if self.picked.as_deref() != Some(id) {
            self.picked = Some(id.to_string());
            self.draft = None;
            self.history = None;
            self.patch = None;
            self.successor.clear();
            self.notice = None;
        }
    }

    /// Takes in whatever finished since the last frame.
    fn poll(&mut self, ctx: &egui::Context) {
        if let Some(got) = self.loading.as_ref().and_then(Task::take) {
            self.loading = None;
            self.track(&got);
            match got {
                Ok(v) => {
                    if let Some(d) = &mut self.draft {
                        match v.entries.iter().find(|e| e.id == d.id) {
                            Some(e) => d.rebase(e),
                            None => self.draft = None,
                        }
                    }
                    (self.stacks, self.load_error, self.load_failed) = (Some(v), None, None);
                }
                Err(f) => (self.load_error, self.load_failed) = (Some(f), Some(Instant::now())),
            }
        }
        if let Some(got) = self.checking.as_ref().and_then(Task::take) {
            self.checking = None;
            self.track(&got);
            self.findings = got.unwrap_or_else(|f| {
                let missing = lords_core::Finding::error("panel", f.to_string());
                lords_core::view::findings(&[missing])
            });
            self.worst = self.findings.worst();
        }
        if let Some(got) = self.status_task.as_ref().and_then(Task::take) {
            self.status_task = None;
            self.track(&got);
            self.status = Some(got);
        }
        if let Some(got) = self.history_task.as_ref().and_then(Task::take) {
            self.history_task = None;
            self.track(&got);
            if let Some((_, slot)) = &mut self.history {
                *slot = Some(got);
            }
        }
        if let Some(got) = self.patch_task.as_ref().and_then(Task::take) {
            self.patch_task = None;
            self.track(&got);
            if let Some((_, slot)) = &mut self.patch {
                *slot = Some(got);
            }
        }
        if let Some(got) = self.planning.as_ref().and_then(Task::take) {
            self.planning = None;
            self.track(&got);
            self.plan_pick = got.as_ref().ok().and_then(|p| p.items.iter().position(|i| i.changes));
            self.plan = Some(got);
        }
        if let Some(got) = self.composing.as_ref().and_then(|(_, t)| t.take()) {
            let (key, _) = self.composing.take().expect("checked above");
            self.track(&got);
            self.composed = Some((key, got));
        }
        if let Some(got) = self.writing.as_ref().and_then(Task::take) {
            self.writing = None;
            self.track(&got);
            self.notice = Some(got.map_err(|f| f.to_string()));
            // A refusal reloads too: a stale draft moves onto the new version, keeping what was typed.
            self.reload(ctx);
        }
        if let Some(done) = self.applying.as_ref().and_then(Task::take) {
            self.applying = None;
            self.notice = Some(done.map(|lines| format!("applied: {}", lines.join("; "))));
            self.plan = None;
            self.reload(ctx);
        }
        // The bridge and the push state, every `POLL`; a load that failed, again after `RETRY`.
        if self.status_at.is_none_or(|t| t.elapsed() >= POLL) {
            if let Some(why) = self.tunnel.as_mut().and_then(Tunnel::ensure) {
                self.bridge = Some(Err(why));
            }
            self.start_status(ctx);
        }
        if self.loading.is_none() && self.load_failed.is_some_and(|t| t.elapsed() >= RETRY) {
            self.load_failed = None;
            self.reload(ctx);
        }
        ctx.request_repaint_after(RETRY.min(POLL));
    }

    fn top(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        ui.horizontal(|ui| {
            ui.label(RichText::new(NAME).font(FontId::monospace(12.0)).color(GREEN).strong());
            if self.checking.is_some() || self.loading.is_some() || self.writing.is_some() {
                ui.label(mono("…", dim(TEXT))).on_hover_text("reading or writing");
            }
            let counts = &self.findings.counts;
            for (n, s, words) in [
                (counts.errors, Severity::Error, "errors"),
                (counts.warnings, Severity::Warning, "warnings"),
                (counts.candidates, Severity::Candidate, "for you to decide"),
            ] {
                if n > 0 {
                    let (g, c) = ui::severity(s);
                    if ui
                        .add(Button::new(mono(format!("{g}{n}"), c)).frame(false))
                        .on_hover_text(format!("{n} {words}"))
                        .clicked()
                    {
                        self.view = View::Findings;
                    }
                }
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.label(mono(" ? ", TEXT)).on_hover_ui(ui::legend);
                if ui
                    .add(Button::new(mono("↻", TEXT)).frame(false))
                    .on_hover_text("reload the stacks and check again")
                    .clicked()
                {
                    self.reload(ctx);
                }
                self.push_glyph(ui, ctx);
                self.bridge_glyph(ui, ctx);
                ui.add_space(12.0);
                for (v, g, words) in [
                    (View::Compose, "◫", "what each harness reads, line by line"),
                    (View::Sync, "⇄", "sync: plan, diff, apply"),
                    (View::Findings, "⚑", "findings"),
                    (View::Entries, "☰", "entries"),
                ] {
                    let color = if self.view == v { GREEN } else { dim(TEXT) };
                    if ui
                        .add(Button::new(mono(g, color)).frame(false))
                        .on_hover_text(words)
                        .clicked()
                    {
                        self.view = v;
                    }
                }
            });
        });
        if let Some(n) = &self.notice {
            let (text, color) = match n {
                Ok(t) => (t.as_str(), GREEN),
                Err(e) => (e.as_str(), RED),
            };
            ui.label(mono(text, color));
        }
    }

    /// Remote only: ⇆ while the panel's server answers, ✕ with why not; a click tries again now.
    fn bridge_glyph(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        let Some(bridge) = &self.bridge else { return };
        let Ok(Source::Remote(c)) = &self.source else { return };
        let route = match &self.tunnel {
            Some(t) => format!("ssh -N {} → 127.0.0.1:{}", t.alias(), c.port()),
            None => format!("127.0.0.1:{}", c.port()),
        };
        let (g, color, hover) = match bridge {
            Ok(()) => ("⇆", GREEN, format!("bridge up\n{route}")),
            Err(why) => ("✕", RED, format!("bridge down\n{route}\n{why}\nclick: try again now")),
        };
        if ui.add(Button::new(mono(g, color)).frame(false)).on_hover_text(hover).clicked() {
            if let Some(why) = self.tunnel.as_mut().and_then(Tunnel::ensure) {
                self.bridge = Some(Err(why));
            }
            self.reload(ctx);
        }
    }

    /// ⇡n: commits the stack repos' upstreams lack; two clicks push them through each repo's pre-push check.
    fn push_glyph(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        let Some(status) = &self.status else { return };
        let (text, hover) = match status {
            Err(f) => ("⇡?".to_string(), format!("the unpushed commits can't be read\n{f}")),
            Ok(s) if s.unpushed() == 0 && s.repos.iter().all(|r| r.error.is_none()) => return,
            Ok(s) => {
                let mut hover = String::new();
                for r in &s.repos {
                    let up = r.upstream.as_deref().unwrap_or("no upstream");
                    hover.push_str(&format!(
                        "{} {} → {up}: {}\n",
                        r.stack,
                        r.branch,
                        r.commits.len()
                    ));
                    if let Some(e) = &r.error {
                        hover.push_str(&format!("  {e}\n"));
                    }
                    for c in &r.commits {
                        hover.push_str(&format!("  {} {} {}\n", c.hash, c.date, c.subject));
                    }
                }
                hover.push_str(if self.confirm_push {
                    "click again: push through the pre-push check"
                } else {
                    "push (asks once more)"
                });
                let mark = if self.confirm_push { " ✓" } else { "" };
                (format!("⇡{}{mark}", s.unpushed()), hover)
            }
        };
        let busy = self.writing.is_some();
        let r =
            ui.add_enabled(!busy, Button::new(mono(text, AMBER)).frame(false)).on_hover_text(hover);
        if r.clicked() && matches!(status, Ok(s) if s.unpushed() > 0) {
            if self.confirm_push {
                self.confirm_push = false;
                self.writing =
                    self.start(ctx, |s| s.push().map(|p| format!("⇡ {}", p.lines.join("; "))));
            } else {
                self.confirm_push = true;
            }
        }
    }

    // ------------------------------------------------------------ entries

    fn filters(&mut self, ui: &mut egui::Ui, layers: &[String]) {
        ui.horizontal_wrapped(|ui| {
            for (i, k) in Kind::ALL.iter().enumerate() {
                let (g, c) = ui::kind(*k);
                ui::toggle(ui, &mut self.kinds[i], g, c, k.name());
            }
            ui.add_space(8.0);
            for (i, (_, g, words)) in AUDIENCES.iter().enumerate() {
                ui::toggle(ui, &mut self.audience[i], g, BLUE, words);
            }
            ui.add_space(8.0);
            ui::toggle(ui, &mut self.retired, "┄", TEXT, "show retired entries");
            ComboBox::from_id_salt("layer")
                .selected_text(mono(
                    if self.layer.is_empty() { "◇ every layer" } else { &self.layer },
                    TEXT,
                ))
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut self.layer, String::new(), "every layer");
                    for l in layers {
                        ui.selectable_value(&mut self.layer, l.clone(), l);
                    }
                });
            ui.add(TextEdit::singleline(&mut self.search).hint_text("⌕").desired_width(120.0));
        });
    }

    fn visible(&self, e: &EntryView) -> bool {
        let kind_on = Kind::ALL.iter().position(|k| *k == e.kind).is_some_and(|i| self.kinds[i]);
        let audience_on = e.audience.iter().any(|a| {
            AUDIENCES.iter().position(|(x, _, _)| x == a).is_some_and(|i| self.audience[i])
        });
        let needle = self.search.to_lowercase();
        let found = needle.is_empty()
            || e.id.to_lowercase().contains(&needle)
            || e.title.to_lowercase().contains(&needle)
            || e.body.to_lowercase().contains(&needle);
        kind_on
            && audience_on
            && found
            && (self.retired || e.active)
            && (self.layer.is_empty() || layer_key(e) == self.layer)
    }

    fn list(&mut self, ui: &mut egui::Ui) {
        let Some(stacks) = &self.stacks else { return };
        let mut layers: Vec<String> = stacks.entries.iter().map(layer_key).collect();
        layers.sort();
        layers.dedup();
        // The view is ordered by layer, kind and id already.
        let rows: Vec<(String, Line, ui::Frame, String)> = stacks
            .entries
            .iter()
            .filter(|e| self.visible(e))
            .map(|e| {
                let (tag, hue) = ui::layer(&scope_of(e));
                let (g, c) = ui::kind(e.kind);
                let mut line = Line::default();
                line.tag(&tag, hue)
                    .add(format!("{g} "), c)
                    .add(&e.id, TEXT)
                    .add(format!("  {}", e.title), dim(TEXT));
                if let Some(s) = self.worst.get(&e.id) {
                    let (sg, sc) = ui::severity(*s);
                    line.right(sg, sc);
                }
                let (og, oc) = ui::origin(&e.origin);
                line.right(og, oc);
                (e.id.clone(), line, ui::Frame::of(e), hover_of(e))
            })
            .collect();
        let total = stacks.entries.len();
        self.filters(ui, &layers);
        ui.label(mono(format!("{} / {total}", rows.len()), dim(TEXT)));
        let mut clicked = None;
        ScrollArea::vertical().id_salt("entries").auto_shrink([false, false]).show(ui, |ui| {
            ui.spacing_mut().item_spacing.y = 0.0;
            for (id, line, frame, hover) in &rows {
                let picked = self.picked.as_deref() == Some(id.as_str());
                if ui::draw(ui, line, Some(*frame), picked, hover).clicked() {
                    clicked = Some(id.clone());
                }
            }
        });
        if let Some(id) = clicked {
            self.pick(&id);
        }
    }

    fn detail(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) -> Option<Act> {
        let stacks = self.stacks.as_ref()?;
        let Some(e) =
            self.picked.as_deref().and_then(|id| stacks.entries.iter().find(|e| e.id == id))
        else {
            ui.label(mono("☰", dim(TEXT))).on_hover_text("pick an entry");
            return None;
        };
        let e = e.clone();
        let topics = stacks.topics.for_stack(e.stack);
        let roles = stacks.roles.for_stack(e.stack);
        let others: Vec<String> = stacks
            .entries
            .iter()
            .filter(|x| x.active && x.id != e.id)
            .map(|x| x.id.clone())
            .collect();
        if self.draft.as_ref().is_none_or(|d| d.id != e.id) {
            self.draft = Some(Draft::of(&e));
        }
        let found: Vec<_> = self
            .findings
            .items
            .iter()
            .filter(|f| f.entry.as_deref() == Some(&e.id))
            .cloned()
            .collect();
        let remote = self.remote();

        // Header: what it is, where it lives, where it came from.
        ui.horizontal(|ui| {
            let (tag, hue) = ui::layer(&scope_of(&e));
            let (g, c) = ui::kind(e.kind);
            ui.label(
                RichText::new(format!(" {tag} "))
                    .font(FontId::monospace(ui::SIZE))
                    .color(BG)
                    .background_color(hue),
            )
            .on_hover_text(format!("{} {}", e.stack, e.scope));
            ui.label(mono(g, c)).on_hover_text(e.kind.name());
            ui.label(RichText::new(&e.id).font(FontId::monospace(13.0)).color(TEXT).strong());
            let (og, oc) = ui::origin(&e.origin);
            let r = ui.add(Button::new(mono(og, oc)).frame(false)).on_hover_text(hover_of(&e));
            if r.clicked() {
                if let Some(url) = e.source.as_deref().filter(|u| u.contains("://")) {
                    open(url);
                }
            }
            let vis =
                if e.stack == Visibility::Public { ("▢", "public") } else { ("□", "private") };
            ui.label(mono(vis.0, TEXT)).on_hover_text(vis.1);
            if !e.active {
                ui.label(mono("┄", AMBER)).on_hover_text("retired");
            }
            // The file is on the stacks' PC: opened from there only.
            if !remote
                && ui
                    .add(Button::new(mono("⧉", dim(TEXT))).frame(false))
                    .on_hover_text(format!("open the file\n{}", e.body_file.display()))
                    .clicked()
            {
                open(&e.body_file.display().to_string());
            }
        });
        for f in &found {
            let (g, c) = ui::severity(f.severity);
            ui.label(mono(format!("{g} {}: {}", f.code, f.message), c));
        }
        let mut relation_click = None;
        let draft = self.draft.as_mut().expect("set above");
        egui::Grid::new("fields").num_columns(2).spacing([10.0, 4.0]).show(ui, |ui| {
            ui.label(mono("title", dim(TEXT)));
            ui.add(TextEdit::singleline(&mut draft.now.title).desired_width(f32::INFINITY));
            ui.end_row();
            ui.label(mono("audience", dim(TEXT)));
            ui.horizontal(|ui| {
                for (a, g, words) in AUDIENCES {
                    let mut on = draft.now.audience.contains(&a);
                    if ui::toggle(ui, &mut on, g, BLUE, words).clicked() {
                        draft.now.audience.retain(|x| *x != a);
                        if on {
                            draft.now.audience.push(a);
                        }
                    }
                }
            });
            ui.end_row();
            ui.label(mono("topics", dim(TEXT)));
            ui.horizontal_wrapped(|ui| {
                if topics.is_empty() {
                    ui.label(mono("—", dim(TEXT)))
                        .on_hover_text("the stack.toml declares no topics");
                }
                for t in &topics {
                    let mut on = draft.now.topics.contains(t);
                    if ui::toggle(ui, &mut on, t, ui::BLUE, "topic").clicked() {
                        draft.now.topics.retain(|x| x != t);
                        if on {
                            draft.now.topics.push(t.clone());
                        }
                    }
                }
            });
            ui.end_row();
            ui.label(mono("roles", dim(TEXT)));
            ui.horizontal_wrapped(|ui| {
                if roles.is_empty() {
                    ui.label(mono("—", dim(TEXT)))
                        .on_hover_text("the stack.toml declares no roles");
                } else if draft.now.roles.is_empty() {
                    ui.label(mono("∀", BLUE)).on_hover_text("every role");
                }
                for r in &roles {
                    let mut on = draft.now.roles.contains(r);
                    if ui::toggle(ui, &mut on, r, AMBER, "only for this role").clicked() {
                        draft.now.roles.retain(|x| x != r);
                        if on {
                            draft.now.roles.push(r.clone());
                        }
                    }
                }
            });
            ui.end_row();
            let named: Vec<(&str, &String)> = e
                .overrides
                .iter()
                .map(|x| ("overrides", x))
                .chain(e.extends.iter().map(|x| ("extends", x)))
                .chain(e.superseded_by.iter().map(|x| ("superseded by", x)))
                .chain(e.links.iter().map(|x| ("links", x)))
                .collect();
            for (rel, id) in named {
                ui.label(mono(rel, dim(TEXT)));
                if ui.add(Button::new(mono(id, BLUE)).frame(false)).clicked()
                    && lords_core::check::is_id(id)
                {
                    relation_click = Some(id.clone());
                }
                ui.end_row();
            }
            for ev in &e.evidence {
                ui.label(mono("evidence", dim(TEXT)));
                ui.vertical(|ui| {
                    ui.label(mono(format!("{} ({})", ev.summary, ev.date), TEXT));
                    for l in &ev.links {
                        if ui.add(Button::new(mono(l, BLUE)).frame(false)).clicked() {
                            if lords_core::check::is_id(l) {
                                relation_click = Some(l.clone());
                            } else {
                                open(l);
                            }
                        }
                    }
                });
                ui.end_row();
            }
            for b in &e.basis {
                ui.label(mono("basis", dim(TEXT)));
                ui.label(mono(
                    format!("“{}” ({}, {})", b.quote, b.by.as_deref().unwrap_or("owner"), b.date),
                    TEXT,
                ));
                ui.end_row();
            }
        });
        let changed = draft.now != draft.loaded;
        let busy = self.writing.is_some();
        let mut act = None;
        let mut toggle_history = false;
        ui.horizontal(|ui| {
            let save = ui
                .add_enabled(changed && !busy, Button::new(mono("✓", GREEN)))
                .on_hover_text("save and commit");
            let undo = ui
                .add_enabled(changed, Button::new(mono("↶", TEXT)))
                .on_hover_text("drop the changes");
            if undo.clicked() {
                draft.now = draft.loaded.clone();
            }
            if save.clicked() {
                act = Some(Act::Save(e.id.clone(), draft.edit()));
            }
            ui.add_space(14.0);
            if e.active {
                ComboBox::from_id_salt("successor")
                    .selected_text(mono(
                        if self.successor.is_empty() {
                            "→ no successor"
                        } else {
                            &self.successor
                        },
                        dim(TEXT),
                    ))
                    .show_ui(ui, |ui| {
                        ui.selectable_value(&mut self.successor, String::new(), "no successor");
                        for id in &others {
                            ui.selectable_value(&mut self.successor, id.clone(), id);
                        }
                    })
                    .response
                    .on_hover_text("the entry that replaces it, if any");
                if ui
                    .add_enabled(!busy, Button::new(mono("⌫", AMBER)))
                    .on_hover_text("retire it (kept in the stack, committed)")
                    .clicked()
                {
                    act = Some(Act::Retire(
                        e.id.clone(),
                        draft.version.clone(),
                        (!self.successor.is_empty()).then(|| self.successor.clone()),
                    ));
                }
            }
            ui.add_space(14.0);
            let shown = self.history.as_ref().is_some_and(|(id, _)| *id == e.id);
            toggle_history = ui
                .add(Button::new(mono("⟲", if shown { GREEN } else { TEXT })))
                .on_hover_text("history of this entry")
                .clicked();
        });
        if toggle_history {
            self.patch = None;
            if self.history.as_ref().is_some_and(|(id, _)| *id == e.id) {
                self.history = None;
            } else {
                self.history = Some((e.id.clone(), None));
                let id = e.id.clone();
                self.history_task = self.start(ctx, move |s| s.history(&id));
            }
        }
        if act.is_some() {
            return act;
        }
        if let Some(id) = relation_click {
            return Some(Act::Pick(id));
        }
        if let Some((_, log)) = &self.history {
            let mut show = None;
            match log {
                None => {
                    ui.label(mono("…", dim(TEXT)));
                }
                Some(Err(err)) => {
                    ui.label(mono(err.to_string(), RED));
                }
                Some(Ok(commits)) => {
                    for c in commits {
                        let picked = self.patch.as_ref().is_some_and(|(h, _)| *h == c.hash);
                        let text = mono(
                            format!("{} {} {}", c.date, c.hash, c.subject),
                            if picked { GREEN } else { TEXT },
                        );
                        if ui.add(Button::new(text).frame(false)).clicked() {
                            show = Some(c.hash.clone());
                        }
                    }
                }
            }
            if let Some(hash) = show {
                self.patch = Some((hash.clone(), None));
                let id = e.id.clone();
                self.patch_task = self.start(ctx, move |s| s.patch(&id, &hash));
            }
        }
        if let Some((_, text)) = &self.patch {
            ScrollArea::vertical().id_salt("patch").max_height(220.0).show(ui, |ui| match text {
                None => {
                    ui.label(mono("…", dim(TEXT)));
                }
                Some(Ok(t)) => ui::diff(ui, t),
                Some(Err(f)) => {
                    ui.label(mono(f.to_string(), RED));
                }
            });
        }
        ui.separator();
        let draft = self.draft.as_mut().expect("set above");
        ScrollArea::vertical().id_salt("body").auto_shrink([false, false]).show(ui, |ui| {
            let edit = TextEdit::multiline(&mut draft.now.body)
                .font(FontId::monospace(ui::SIZE))
                .desired_width(f32::INFINITY)
                .desired_rows(16)
                .interactive(e.editable);
            let r = ui.add(edit);
            if !e.editable {
                r.on_hover_text(if e.kind == Kind::Memory {
                    "the agent's own note: promote it to change it"
                } else {
                    "inherited: stays verbatim"
                });
            }
        });
        None
    }

    /// Starts what the detail asked for; its result reloads.
    fn act(&mut self, act: Act, ctx: &egui::Context) {
        match act {
            Act::Pick(id) => self.pick(&id),
            Act::Save(id, edit) => {
                self.writing = self.start(ctx, move |s| {
                    let saved = s.edit(&EditRequest { id: id.clone(), edit })?;
                    Ok(format!(
                        "✓ {id}{}",
                        saved.commit.map(|c| format!(" {c}")).unwrap_or_default()
                    ))
                });
            }
            Act::Retire(id, base, superseded_by) => {
                self.writing = self.start(ctx, move |s| {
                    let r = s.retire(&RetireRequest { id: id.clone(), base, superseded_by })?;
                    Ok(format!("⌫ {id}{}", r.commit.map(|c| format!(" {c}")).unwrap_or_default()))
                });
            }
        }
    }

    // ------------------------------------------------------------ findings

    fn findings_view(&mut self, ui: &mut egui::Ui) {
        if self.findings.items.is_empty() {
            let (g, hover) = if self.checking.is_some() {
                ("…", "checking")
            } else {
                ("✓", "nothing found")
            };
            ui.label(mono(g, GREEN)).on_hover_text(hover);
            return;
        }
        let mut go = None;
        ScrollArea::vertical().id_salt("findings").auto_shrink([false, false]).show(ui, |ui| {
            ui.spacing_mut().item_spacing.y = 0.0;
            for f in &self.findings.items {
                let (g, c) = ui::severity(f.severity);
                let mut line = Line::default();
                line.add(format!("{g} "), c).add(format!("{:<18}", f.code), dim(TEXT));
                if let Some(id) = &f.entry {
                    line.add(format!("{id}  "), TEXT);
                }
                line.add(&f.message, if f.severity == Severity::Info { dim(TEXT) } else { TEXT });
                let mut hover = f.text.clone();
                if !f.related.is_empty() {
                    hover.push_str(&format!("\nwith: {}", f.related.join(", ")));
                }
                if ui::draw(ui, &line, None, false, &hover).clicked() {
                    go = Some(f.entry.clone());
                }
            }
        });
        match go {
            Some(Some(id)) => {
                self.pick(&id);
                self.view = View::Entries;
            }
            // A finding about a target, not an entry: its diff is in the sync view.
            Some(None) => self.view = View::Sync,
            None => {}
        }
    }

    // ------------------------------------------------------------ sync

    fn sync_options(&self) -> sync::Options {
        sync::Options {
            repo: (!self.plan_repo.is_empty()).then(|| self.plan_repo.clone()),
            user_only: self.plan_repo.is_empty(),
            home: sync::default_home().unwrap_or_default(),
            ..sync::Options::default()
        }
    }

    /// A dry run of sync for the chosen scope, on a thread. Planning only reads.
    fn start_plan(&mut self, ctx: &egui::Context) {
        let place = (!self.plan_repo.is_empty()).then(|| self.plan_repo.clone());
        self.planning = self.start(ctx, move |s| s.plan(place.as_deref()));
        (self.plan_pick, self.confirm_apply) = (None, false);
    }

    fn sync_view(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        // Opening the view plans at once: a dry run writes nothing.
        if self.plan.is_none() && self.planning.is_none() && self.applying.is_none() {
            self.start_plan(ctx);
        }
        let mut start_plan = false;
        let places = self.stacks.as_ref().map(|s| s.places.clone()).unwrap_or_default();
        // Apply writes this PC's agent files: only where the stacks are.
        let local = match &self.source {
            Ok(Source::Local(p)) => Some(p.clone()),
            _ => None,
        };
        ui.horizontal(|ui| {
            ComboBox::from_id_salt("plan-scope")
                .selected_text(mono(
                    if self.plan_repo.is_empty() { "user level" } else { &self.plan_repo },
                    TEXT,
                ))
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut self.plan_repo, String::new(), "user level");
                    for r in &places {
                        ui.selectable_value(&mut self.plan_repo, r.clone(), r);
                    }
                });
            let busy = self.planning.is_some() || self.applying.is_some();
            if ui
                .add_enabled(!busy, Button::new(mono("⟳ plan", TEXT)))
                .on_hover_text("a dry run: nothing is written")
                .clicked()
            {
                start_plan = true;
            }
            if busy {
                ui.label(mono("…", dim(TEXT)));
            }
            let changes = matches!(&self.plan, Some(Ok(p)) if p.items.iter().any(|i| i.changes));
            let conflicts = matches!(&self.plan, Some(Ok(p)) if p.conflicts > 0);
            if let (true, false, Some(paths)) = (changes, busy, &local) {
                ui.add_space(20.0);
                let label = if self.confirm_apply { "⇄ apply ✓" } else { "⇄ apply" };
                let hover = if conflicts {
                    "resolve the conflicts first"
                } else if self.confirm_apply {
                    "click again: import memory, back up the owner files it edits, write"
                } else {
                    "write this plan (asks once more)"
                };
                if ui
                    .add_enabled(!conflicts, Button::new(mono(label, AMBER)))
                    .on_hover_text(hover)
                    .clicked()
                {
                    if self.confirm_apply {
                        let opts = self.sync_options();
                        let (pu, pr) = (paths.public.clone(), paths.private.clone());
                        self.applying = Some(Task::start(ctx, move || {
                            let applied = sync::import_and_apply(&pu, pr.as_deref(), &opts)?;
                            let mut done = applied.done;
                            if let Some(b) = applied.backups {
                                done.push(format!(
                                    "backups of the owner files changed: {}",
                                    b.display()
                                ));
                            }
                            Ok(done)
                        }));
                        self.confirm_apply = false;
                    } else {
                        self.confirm_apply = true;
                    }
                }
            }
        });
        if start_plan {
            self.start_plan(ctx);
        }
        let Some(plan) = &self.plan else {
            ui.label(mono("…", dim(TEXT))).on_hover_text("planning: a dry run, nothing is written");
            return;
        };
        let plan = match plan {
            Ok(p) => p,
            Err(e) => {
                ui.label(mono(e.to_string(), RED));
                return;
            }
        };
        for n in &plan.notes {
            ui.label(mono(format!("· {n}"), dim(TEXT)));
        }
        let height = (ui.available_height() * 0.45).max(120.0);
        let mut picked = self.plan_pick;
        ScrollArea::vertical().id_salt("plan").max_height(height).auto_shrink([false, false]).show(
            ui,
            |ui| {
                ui.spacing_mut().item_spacing.y = 0.0;
                for (i, item) in plan.items.iter().enumerate() {
                    let (g, c, words) = match item.action {
                        Action::Create => ("+", GREEN, "create"),
                        Action::Update => ("~", AMBER, "update"),
                        Action::InSync => ("=", dim(TEXT), "in sync"),
                        Action::Install => ("↓", GREEN, "install the tool's dir"),
                        Action::Refresh => {
                            ("↻", AMBER, "replace the tool's dir with its new version")
                        }
                        Action::Remove => ("⌫", AMBER, "remove the tool's path (no longer wanted)"),
                        Action::Forget => ("·", dim(TEXT), "drop a ledger record"),
                        Action::RetireEntry => {
                            ("⌛", AMBER, "an expired temporary entry: retired in its stack")
                        }
                        Action::Report => ("◌", BLUE, "tracked: shown, never written"),
                        Action::Conflict => ("✕", RED, "conflict"),
                    };
                    let mut line = Line::default();
                    line.add(format!("{g} "), c)
                        .add(format!("{}  ", item.target), TEXT)
                        .add(item.path.display().to_string(), dim(TEXT));
                    let mut hover = format!("{words}\n{}", item.path.display());
                    for n in &item.notes {
                        hover.push_str(&format!("\n{n}"));
                    }
                    if ui::draw(ui, &line, None, picked == Some(i), &hover).clicked() {
                        picked = if picked == Some(i) { None } else { Some(i) };
                    }
                }
            },
        );
        self.plan_pick = picked;
        if let Some(item) = picked.and_then(|i| plan.items.get(i)) {
            ui.separator();
            for n in &item.notes {
                ui.label(mono(format!("· {n}"), AMBER));
            }
            let whose = match item.class {
                sync::Class::Tool => "the tool's (ledger), replaced whole",
                sync::Class::Owner => "the owner's: one managed edit, backed up",
            };
            ui.label(mono(whose, BLUE));
            ScrollArea::vertical().id_salt("plan-diff").auto_shrink([false, false]).show(
                ui,
                |ui| {
                    if item.diff.is_empty() {
                        ui.label(mono("=", dim(TEXT))).on_hover_text("no text change");
                    } else {
                        ui::diff(ui, &item.diff);
                    }
                },
            );
        }
    }

    // ------------------------------------------------------------ compose

    fn compose_view(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        let (places, adapters) = match &self.stacks {
            Some(s) => (s.places.clone(), s.adapters.clone()),
            None => return,
        };
        ui.horizontal(|ui| {
            ComboBox::from_id_salt("adapter")
                .selected_text(mono(&self.compose_adapter, TEXT))
                .show_ui(ui, |ui| {
                    for id in &adapters {
                        ui.selectable_value(&mut self.compose_adapter, id.clone(), id);
                    }
                });
            ComboBox::from_id_salt("compose-repo")
                .selected_text(mono(
                    if self.compose_repo.is_empty() { "user level" } else { &self.compose_repo },
                    TEXT,
                ))
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut self.compose_repo, String::new(), "user level");
                    for r in &places {
                        ui.selectable_value(&mut self.compose_repo, r.clone(), r);
                    }
                });
        });
        let key = (self.compose_adapter.clone(), self.compose_repo.clone());
        let have = self.composed.as_ref().is_some_and(|(k, _)| *k == key);
        let asked = self.composing.as_ref().is_some_and(|(k, _)| *k == key);
        if !have && !asked {
            let (adapter, place) = key.clone();
            let task = self.start(ctx, move |s| {
                s.compose(&adapter, (!place.is_empty()).then_some(place.as_str()))
            });
            self.composing = task.map(|t| (key.clone(), t));
        }
        let composed = match self.composed.as_ref().filter(|(k, _)| *k == key) {
            None => {
                ui.label(mono("…", dim(TEXT)));
                return;
            }
            Some((_, Err(f))) => {
                ui.label(mono(f.to_string(), RED));
                return;
            }
            Some((_, Ok(c))) => c.clone(),
        };
        if composed.lines.is_empty() {
            ui.label(mono("∅", dim(TEXT))).on_hover_text("nothing applies here");
            return;
        }
        if !composed.overridden.is_empty() {
            ui.label(mono(
                format!("· overridden here: {}", composed.overridden.join(", ")),
                dim(TEXT),
            ));
        }
        // In a repo, the file starts with what keeps the checkout's tracked instructions loading.
        if let Some(pre) = &composed.preamble {
            let head = pre.lines().next().unwrap_or_default().to_string();
            ui.label(mono(format!("{:<8}{head}", ""), BLUE)).on_hover_text(format!(
                "written first in this repo's file, so the checkout's tracked instructions still load\n{pre}"
            ));
        }
        let mut go = None;
        ScrollArea::vertical().id_salt("composed").auto_shrink([false, false]).show(ui, |ui| {
            ui.spacing_mut().item_spacing.y = 1.0;
            for l in &composed.lines {
                ui.horizontal_top(|ui| {
                    // The gutter: which layer and entry this line comes from; a fixed width keeps the text aligned.
                    let gutter_width = 56.0;
                    ui.allocate_ui_with_layout(
                        egui::vec2(gutter_width, ui::ROW),
                        egui::Layout::left_to_right(egui::Align::Center),
                        |ui| {
                            ui.set_min_width(gutter_width);
                            let known = l
                                .source
                                .as_ref()
                                .and_then(|s| Some((s, Scope::parse(&s.scope)?, s.kind?)));
                            if let Some((s, scope, kind)) = known {
                                let (tag, hue) = ui::layer(&scope);
                                let (g, c) = ui::kind(kind);
                                let gutter = RichText::new(format!(" {tag} "))
                                    .font(FontId::monospace(ui::SIZE))
                                    .color(BG)
                                    .background_color(hue);
                                let hover = format!(
                                    "{}\n{} {}\n{}:{}",
                                    s.id,
                                    s.stack,
                                    s.scope,
                                    s.file.display(),
                                    s.file_line
                                );
                                if ui
                                    .add(Button::new(gutter).frame(false))
                                    .on_hover_text(hover)
                                    .clicked()
                                {
                                    go = Some(s.id.clone());
                                }
                                ui.label(mono(g, c));
                            }
                        },
                    );
                    let color = if l.source.is_some() { TEXT } else { dim(TEXT) };
                    ui.add(egui::Label::new(mono(&l.text, color)).wrap());
                });
            }
        });
        if let Some(id) = go {
            self.pick(&id);
            self.view = View::Entries;
        }
    }

    /// One frame of the whole window.
    fn ui(&mut self, ctx: &egui::Context) {
        self.poll(ctx);
        let panel = egui::Frame::default().fill(BG).inner_margin(8.0);
        egui::TopBottomPanel::top("top")
            .frame(panel)
            .show_separator_line(false)
            .show(ctx, |ui| self.top(ui, ctx));
        if let Err(e) = &self.source {
            let e = e.clone();
            egui::CentralPanel::default().frame(panel).show(ctx, |ui| {
                ui.label(mono(e, AMBER));
            });
            return;
        }
        if self.stacks.is_none() {
            let shown = self.load_error.as_ref().map(|f| (f.to_string(), RED));
            egui::CentralPanel::default().frame(panel).show(ctx, |ui| match shown {
                Some((e, c)) => {
                    ui.label(mono(e, c));
                }
                None => {
                    ui.label(mono("…", dim(TEXT))).on_hover_text("reading the stacks");
                }
            });
            return;
        }
        match self.view {
            View::Entries => {
                egui::SidePanel::left("list")
                    .frame(panel)
                    .resizable(true)
                    .default_width(ctx.screen_rect().width() * 0.46)
                    .min_width(300.0)
                    .show(ctx, |ui| self.list(ui));
                let act = egui::CentralPanel::default()
                    .frame(panel)
                    .show(ctx, |ui| self.detail(ui, ctx))
                    .inner;
                if let Some(act) = act {
                    self.act(act, ctx);
                }
            }
            View::Findings => {
                egui::CentralPanel::default().frame(panel).show(ctx, |ui| self.findings_view(ui));
            }
            View::Sync => {
                egui::CentralPanel::default().frame(panel).show(ctx, |ui| self.sync_view(ui, ctx));
            }
            View::Compose => {
                egui::CentralPanel::default()
                    .frame(panel)
                    .show(ctx, |ui| self.compose_view(ui, ctx));
            }
        }
    }
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.ui(ctx);
    }
}

fn style(ctx: &egui::Context) {
    // egui's fonts lack some glyphs (✦ ♟ ⚒ ◷); Windows' symbol font, if present, fills in.
    let symbols = std::env::var_os("WINDIR")
        .map(|dir| Path::new(&dir).join("Fonts").join("seguisym.ttf"))
        .and_then(lords_core::fsguard::read_bytes_lossy);
    if let Some(bytes) = symbols {
        let mut fonts = egui::FontDefinitions::default();
        fonts.font_data.insert("symbols".into(), egui::FontData::from_owned(bytes).into());
        for family in [egui::FontFamily::Monospace, egui::FontFamily::Proportional] {
            fonts.families.entry(family).or_default().push("symbols".into());
        }
        ctx.set_fonts(fonts);
    }
    let mut visuals = egui::Visuals::dark();
    visuals.panel_fill = BG;
    visuals.window_fill = BG;
    visuals.extreme_bg_color = egui::Color32::from_rgb(10, 22, 14);
    visuals.override_text_color = Some(TEXT);
    visuals.selection.bg_fill = ui::EDGE;
    ctx.set_visuals(visuals);
    ctx.style_mut(|style| {
        style.override_font_id = Some(FontId::monospace(11.5));
        style.spacing.item_spacing = egui::vec2(6.0, 4.0);
    });
}

fn main() -> eframe::Result {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title(NAME)
            .with_icon(
                eframe::icon_data::from_png_bytes(include_bytes!("../../../assets/icon.png"))
                    .expect("built-in app icon"),
            )
            .with_inner_size([1180.0, 720.0])
            .with_min_inner_size([640.0, 400.0]),
        ..Default::default()
    };
    eframe::run_native(
        NAME,
        options,
        Box::new(|cc| {
            style(&cc.egui_ctx);
            Ok(Box::new(App::new(&cc.egui_ctx)))
        }),
    )
}
