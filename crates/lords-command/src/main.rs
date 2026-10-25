//! Lords Command's window: browse the stacks' entries, see what `check` finds, edit and retire, see what each
//! harness reads in each place and where every line comes from, and sync behind a confirmation.
//!
//! Renderer-agnostic by rule (owner, 25 Oct: one native app keeps the egui UI code; its renderer is chosen by
//! measurement, not assumption): draw only through egui's painter and widgets. No glow, wgpu or GPU calls, no paint
//! callbacks, no custom shaders or textures; the renderer is picked in Cargo.toml alone.
#![windows_subsystem = "windows"]

mod ui;
mod work;

use eframe::egui::{self, Button, ComboBox, FontId, RichText, ScrollArea, TextEdit};
use lords_core::adapters;
use lords_core::check;
use lords_core::config::Config;
use lords_core::finding::{Finding, Severity};
use lords_core::git::{self, Commit};
use lords_core::model::{Audience, Entry, Kind, Origin, Visibility};
use lords_core::ops::{self, Edit, Retire};
use lords_core::privacy::Rules;
use lords_core::sync::{self, Action, Plan};
use lords_core::Stacks;
use std::path::{Path, PathBuf};
use ui::{dim, mono, Line, AMBER, BG, BLUE, GREEN, RED, TEXT};
use work::Task;

const NAME: &str = env!("CARGO_PKG_NAME");
const AUDIENCES: [(Audience, &str, &str); 4] = [
    (Audience::All, "∀", "all agents"),
    (Audience::Opus, "O", "Opus"),
    (Audience::Sol, "S", "Sol"),
    (Audience::Astra, "A", "Astra"),
];

#[derive(Clone, Copy, PartialEq, Eq)]
enum View {
    Entries,
    Findings,
    Sync,
    Compose,
}

/// The picked entry's editable fields, as loaded and as edited.
#[derive(Clone, PartialEq)]
struct Fields {
    title: String,
    audience: Vec<Audience>,
    topics: Vec<String>,
    roles: Vec<String>,
    body: String,
}

struct Draft {
    id: String,
    loaded: Fields,
    now: Fields,
}

/// What a click in the detail asks for; carried out after drawing, when the stacks aren't borrowed.
enum Act {
    Pick(String),
    Save(String, Edit),
    Retire(String, Option<String>),
}

struct Paths {
    public: PathBuf,
    private: Option<PathBuf>,
}

struct App {
    paths: Result<Paths, String>,
    stacks: Option<Stacks>,
    load_error: Option<String>,
    findings: Vec<Finding>,
    checking: Option<Task<Vec<Finding>>>,
    view: View,
    kinds: [bool; 8],
    audience: [bool; 4],
    layer: String,
    search: String,
    retired: bool,
    picked: Option<String>,
    draft: Option<Draft>,
    successor: String,
    history: Option<(String, Result<Vec<Commit>, String>)>,
    patch: Option<(String, String)>,
    notice: Option<Result<String, String>>,
    plan_repo: String,
    plan: Option<Result<Plan, String>>,
    planning: Option<Task<Result<Plan, String>>>,
    plan_pick: Option<usize>,
    confirm_apply: bool,
    applying: Option<Task<Result<Vec<String>, String>>>,
    compose_adapter: &'static str,
    compose_repo: String,
}

fn fields(e: &Entry) -> Fields {
    Fields {
        title: e.meta.title.clone(),
        audience: e.meta.audience.clone(),
        topics: e.meta.topics.clone(),
        roles: e.meta.roles.clone(),
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

fn source_url(o: &Origin) -> Option<String> {
    let s = o.source()?;
    let repo = if s.repo.contains("://") {
        s.repo.clone()
    } else {
        format!("https://github.com/{}", s.repo)
    };
    Some(if repo.contains("github.com") {
        format!("{repo}/blob/{}/{}", s.commit, s.path)
    } else {
        repo
    })
}

fn hover_of(e: &Entry) -> String {
    let mut h =
        format!("{}\n{}\n{} · {} · {}", e.id(), e.meta.title, e.stack, e.scope, e.meta.kind.name());
    if !e.meta.roles.is_empty() {
        h.push_str(&format!("\nfor: {}", e.meta.roles.join(", ")));
    }
    match &e.meta.origin {
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
    if !e.active() {
        h.push_str("\nretired");
        if let Some(r) = &e.meta.retired {
            h.push_str(&format!(" {}: {}", r.date, r.reason));
        }
    }
    h.push_str(&format!("\n{}", e.meta_file.display()));
    h
}

fn layer_key(e: &Entry) -> String {
    match e.scope {
        lords_core::model::Scope::Global => format!("global · {}", e.stack),
        _ => e.scope.to_string(),
    }
}

impl App {
    fn new(ctx: &egui::Context) -> App {
        let cfg = Config::discover();
        let paths = match cfg.public {
            Some(public) => Ok(Paths { public, private: cfg.private }),
            None => Err(
                "No stacks yet. Put a config.txt beside lords-command.exe or in ~/.lords-command/ with\n  public = <the public stack dir>\n  private = <your private stack dir>"
                    .to_string(),
            ),
        };
        let mut app = App {
            paths,
            stacks: None,
            load_error: None,
            findings: Vec::new(),
            checking: None,
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
            patch: None,
            notice: None,
            plan_repo: String::new(),
            plan: None,
            planning: None,
            plan_pick: None,
            confirm_apply: false,
            applying: None,
            compose_adapter: "claude",
            compose_repo: String::new(),
        };
        // `--view entries|findings|sync|compose`, `--pick <id>`, `--adapter <id>`, `--repo <name>`: open the
        // window at a place.
        let args: Vec<String> = std::env::args().skip(1).collect();
        for pair in args.windows(2) {
            match (pair[0].as_str(), pair[1].as_str()) {
                ("--view", "findings") => app.view = View::Findings,
                ("--view", "sync") => app.view = View::Sync,
                ("--view", "compose") => app.view = View::Compose,
                ("--pick", id) => app.picked = Some(id.to_string()),
                ("--adapter", id) => {
                    if let Some(a) = adapters::get(id) {
                        app.compose_adapter = a.id();
                    }
                }
                ("--repo", r) => (app.compose_repo, app.plan_repo) = (r.to_string(), r.to_string()),
                _ => {}
            }
        }
        app.reload(ctx);
        app
    }

    /// Loads the stacks now, and runs `check` (with drift) on a thread.
    fn reload(&mut self, ctx: &egui::Context) {
        let Ok(p) = &self.paths else { return };
        match Stacks::load(&p.public, p.private.as_deref()) {
            Ok(s) => {
                self.stacks = Some(s);
                self.load_error = None;
            }
            Err(e) => {
                self.stacks = None;
                self.load_error = Some(e);
            }
        }
        let (public, private) = (p.public.clone(), p.private.clone());
        self.checking = Some(Task::start(ctx, move || {
            let Ok(stacks) = Stacks::load(&public, private.as_deref()) else { return Vec::new() };
            let rules = match Rules::public()
                .with_allow_from(&public)
                .with_private(stacks.private.as_ref())
            {
                Ok(rules) => rules,
                Err(e) => return vec![lords_core::Finding::error("privacy-scope", e)],
            };
            let mut found = check::check(&stacks, &rules);
            found.extend(sync::drift_findings(&stacks, &sync::default_home().unwrap_or_default()));
            found
        }));
        // Keep the draft only if its entry still has the text it was opened with.
        if let (Some(d), Some(s)) = (&self.draft, &self.stacks) {
            if s.find(&d.id).map(fields) != Some(d.loaded.clone()) {
                self.draft = None;
            }
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

    fn count(&self, s: Severity) -> usize {
        self.findings.iter().filter(|f| f.severity == s).count()
    }

    fn top(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        ui.horizontal(|ui| {
            ui.label(RichText::new(NAME).font(FontId::monospace(12.0)).color(GREEN).strong());
            if self.checking.is_some() {
                ui.label(mono("…", dim(TEXT))).on_hover_text("checking");
            }
            for (s, words) in [
                (Severity::Error, "errors"),
                (Severity::Warning, "warnings"),
                (Severity::Candidate, "for you to decide"),
            ] {
                let n = self.count(s);
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

    fn visible(&self, e: &Entry) -> bool {
        let kind_on =
            Kind::ALL.iter().position(|k| *k == e.meta.kind).is_some_and(|i| self.kinds[i]);
        let audience_on = e.meta.audience.iter().any(|a| {
            AUDIENCES.iter().position(|(x, _, _)| x == a).is_some_and(|i| self.audience[i])
        });
        let needle = self.search.to_lowercase();
        let found = needle.is_empty()
            || e.id().to_lowercase().contains(&needle)
            || e.meta.title.to_lowercase().contains(&needle)
            || e.body.to_lowercase().contains(&needle);
        kind_on
            && audience_on
            && found
            && (self.retired || e.active())
            && (self.layer.is_empty() || layer_key(e) == self.layer)
    }

    fn list(&mut self, ui: &mut egui::Ui) {
        let Some(stacks) = &self.stacks else { return };
        let mut layers: Vec<String> = stacks.entries().map(layer_key).collect();
        layers.sort();
        layers.dedup();
        let mut entries: Vec<&Entry> = stacks.entries().filter(|e| self.visible(e)).collect();
        entries
            .sort_by(|a, b| (a.rank(), a.meta.kind, a.id()).cmp(&(b.rank(), b.meta.kind, b.id())));
        let rows: Vec<(String, Line, ui::Frame, String)> = entries
            .iter()
            .map(|e| {
                let (tag, hue) = ui::layer(&e.scope);
                let (g, c) = ui::kind(e.meta.kind);
                let mut line = Line::default();
                line.tag(&tag, hue)
                    .add(format!("{g} "), c)
                    .add(e.id(), TEXT)
                    .add(format!("  {}", e.meta.title), dim(TEXT));
                let worst = self
                    .findings
                    .iter()
                    .filter(|f| f.entry.as_deref() == Some(e.id()))
                    .map(|f| f.severity)
                    .min();
                if let Some(s) = worst.filter(|s| *s != Severity::Info) {
                    let (sg, sc) = ui::severity(s);
                    line.right(sg, sc);
                }
                let (og, oc) = ui::origin(&e.meta.origin);
                line.right(og, oc);
                (e.id().to_string(), line, ui::Frame::of(e), hover_of(e))
            })
            .collect();
        let total = stacks.entries().count();
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

    fn detail(&mut self, ui: &mut egui::Ui) -> Option<Act> {
        let stacks = self.stacks.as_ref()?;
        let Some(e) = self.picked.as_deref().and_then(|id| stacks.find(id)).cloned() else {
            ui.label(mono("☰ pick an entry", dim(TEXT)));
            return None;
        };
        let topics: Vec<String> = {
            let mut t = stacks.public.manifest.topics.clone();
            if e.stack == Visibility::Private {
                t.extend(stacks.private.iter().flat_map(|p| p.manifest.topics.clone()));
            }
            t
        };
        let roles: Vec<String> = {
            let mut r = stacks.public.manifest.roles.clone();
            if e.stack == Visibility::Private {
                r.extend(stacks.private.iter().flat_map(|p| p.manifest.roles.clone()));
            }
            r
        };
        let others: Vec<String> = stacks
            .entries()
            .filter(|x| x.active() && x.id() != e.id())
            .map(|x| x.id().to_string())
            .collect();
        let root = stacks.stack(e.stack).map(|s| s.root.clone()).unwrap_or_default();
        if self.draft.as_ref().is_none_or(|d| d.id != e.id()) {
            self.draft =
                Some(Draft { id: e.id().to_string(), loaded: fields(&e), now: fields(&e) });
        }
        let found: Vec<Finding> =
            self.findings.iter().filter(|f| f.entry.as_deref() == Some(e.id())).cloned().collect();

        // Header: what it is, where it lives, where it came from.
        ui.horizontal(|ui| {
            let (tag, hue) = ui::layer(&e.scope);
            let (g, c) = ui::kind(e.meta.kind);
            ui.label(
                RichText::new(format!(" {tag} "))
                    .font(FontId::monospace(ui::SIZE))
                    .color(BG)
                    .background_color(hue),
            )
            .on_hover_text(format!("{} {}", e.stack, e.scope));
            ui.label(mono(g, c)).on_hover_text(e.meta.kind.name());
            ui.label(RichText::new(e.id()).font(FontId::monospace(13.0)).color(TEXT).strong());
            let (og, oc) = ui::origin(&e.meta.origin);
            let r = ui.add(Button::new(mono(og, oc)).frame(false)).on_hover_text(hover_of(&e));
            if r.clicked() {
                if let Some(url) = source_url(&e.meta.origin) {
                    open(&url);
                }
            }
            let vis =
                if e.stack == Visibility::Public { ("▢", "public") } else { ("□", "private") };
            ui.label(mono(vis.0, TEXT)).on_hover_text(vis.1);
            if !e.active() {
                ui.label(mono("┄", AMBER)).on_hover_text("retired");
            }
            if ui
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
        let editable_body =
            !matches!(e.meta.origin, Origin::Inherited(_)) && e.meta.kind != Kind::Memory;
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
                .meta
                .overrides
                .iter()
                .map(|x| ("overrides", x))
                .chain(e.meta.extends.iter().map(|x| ("extends", x)))
                .chain(e.meta.superseded_by.iter().map(|x| ("superseded by", x)))
                .chain(e.meta.links.iter().map(|x| ("links", x)))
                .collect();
            for (rel, id) in named {
                ui.label(mono(rel, dim(TEXT)));
                if ui.add(Button::new(mono(id, BLUE)).frame(false)).clicked() && check::is_id(id) {
                    relation_click = Some(id.clone());
                }
                ui.end_row();
            }
            for ev in &e.meta.evidence {
                ui.label(mono("evidence", dim(TEXT)));
                ui.vertical(|ui| {
                    ui.label(mono(format!("{} ({})", ev.summary, ev.date), TEXT));
                    for l in &ev.links {
                        if ui.add(Button::new(mono(l, BLUE)).frame(false)).clicked() {
                            if check::is_id(l) {
                                relation_click = Some(l.clone());
                            } else {
                                open(l);
                            }
                        }
                    }
                });
                ui.end_row();
            }
            for b in &e.meta.basis {
                ui.label(mono("basis", dim(TEXT)));
                ui.label(mono(
                    format!("“{}” ({}, {})", b.quote, b.by.as_deref().unwrap_or("owner"), b.date),
                    TEXT,
                ));
                ui.end_row();
            }
        });
        let changed = draft.now != draft.loaded;
        let mut act = None;
        ui.horizontal(|ui| {
            let save = ui
                .add_enabled(changed, Button::new(mono("✓", GREEN)))
                .on_hover_text("save and commit");
            let undo = ui
                .add_enabled(changed, Button::new(mono("↶", TEXT)))
                .on_hover_text("drop the changes");
            if undo.clicked() {
                draft.now = draft.loaded.clone();
            }
            if save.clicked() {
                let d = &*draft;
                let edit = Edit {
                    title: (d.now.title != d.loaded.title).then(|| d.now.title.clone()),
                    audience: (d.now.audience != d.loaded.audience)
                        .then(|| d.now.audience.iter().map(|a| audience_name(*a)).collect()),
                    topics: (d.now.topics != d.loaded.topics).then(|| d.now.topics.clone()),
                    roles: (d.now.roles != d.loaded.roles).then(|| d.now.roles.clone()),
                    body: (d.now.body != d.loaded.body).then(|| d.now.body.clone()),
                };
                act = Some(Act::Save(e.id().to_string(), edit));
            }
            ui.add_space(14.0);
            if e.active() {
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
                    .add(Button::new(mono("⌫", AMBER)))
                    .on_hover_text("retire it (kept in the stack, committed)")
                    .clicked()
                {
                    act = Some(Act::Retire(
                        e.id().to_string(),
                        (!self.successor.is_empty()).then(|| self.successor.clone()),
                    ));
                }
            }
            ui.add_space(14.0);
            let shown = self.history.as_ref().is_some_and(|(id, _)| id == e.id());
            if ui
                .add(Button::new(mono("⟲", if shown { GREEN } else { TEXT })))
                .on_hover_text("history of this entry")
                .clicked()
            {
                self.history = if shown {
                    None
                } else {
                    let rel = e.meta_file.strip_prefix(&root).unwrap_or(&e.meta_file).to_path_buf();
                    Some((e.id().to_string(), git::log(&root, &rel)))
                };
                self.patch = None;
            }
        });
        if act.is_some() {
            return act;
        }
        if let Some(id) = relation_click {
            return Some(Act::Pick(id));
        }
        if let Some((_, log)) = &self.history {
            let mut show = None;
            match log {
                Err(err) => {
                    ui.label(mono(err, RED));
                }
                Ok(commits) => {
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
                let rel = e.meta_file.strip_prefix(&root).unwrap_or(&e.meta_file).to_path_buf();
                let text = git::show(&root, &hash, &rel).unwrap_or_else(|err| err);
                self.patch = Some((hash, text));
            }
        }
        if let Some((_, text)) = &self.patch {
            ScrollArea::vertical()
                .id_salt("patch")
                .max_height(220.0)
                .show(ui, |ui| ui::diff(ui, text));
        }
        ui.separator();
        let draft = self.draft.as_mut().expect("set above");
        ScrollArea::vertical().id_salt("body").auto_shrink([false, false]).show(ui, |ui| {
            let edit = TextEdit::multiline(&mut draft.now.body)
                .font(FontId::monospace(ui::SIZE))
                .desired_width(f32::INFINITY)
                .desired_rows(16)
                .interactive(editable_body);
            let r = ui.add(edit);
            if !editable_body {
                r.on_hover_text(if e.meta.kind == Kind::Memory {
                    "the agent's own note: promote it to change it"
                } else {
                    "inherited: stays verbatim"
                });
            }
        });
        None
    }

    /// Carries out what the detail asked for, then reloads.
    fn act(&mut self, act: Act, ctx: &egui::Context) {
        let Some(stacks) = &self.stacks else { return };
        match act {
            Act::Pick(id) => {
                self.pick(&id);
                return;
            }
            Act::Save(id, edit) => {
                let result = ops::edit(stacks, &id, &edit, true);
                self.notice = Some(result.map(|c| {
                    format!("saved {id}{}", c.map(|c| format!(", commit {c}")).unwrap_or_default())
                }));
                self.draft = None;
            }
            Act::Retire(id, successor) => {
                let r = Retire {
                    id: &id,
                    reason: "retired by the owner in Lords Command",
                    superseded_by: successor.as_deref(),
                    date: ops::today(),
                    commit: true,
                };
                self.notice = Some(ops::retire(stacks, &r).map(|r| {
                    format!(
                        "retired {id}{}",
                        r.commit.map(|c| format!(", commit {c}")).unwrap_or_default()
                    )
                }));
            }
        }
        self.reload(ctx);
    }

    // ------------------------------------------------------------ findings

    fn findings_view(&mut self, ui: &mut egui::Ui) {
        if self.findings.is_empty() {
            let text = if self.checking.is_some() { "… checking" } else { "✓ nothing found" };
            ui.label(mono(text, GREEN));
            return;
        }
        let mut sorted: Vec<&Finding> = self.findings.iter().collect();
        sorted.sort_by_key(|f| f.severity);
        let mut go = None;
        ScrollArea::vertical().id_salt("findings").auto_shrink([false, false]).show(ui, |ui| {
            ui.spacing_mut().item_spacing.y = 0.0;
            for f in sorted {
                let (g, c) = ui::severity(f.severity);
                let mut line = Line::default();
                line.add(format!("{g} "), c).add(format!("{:<18}", f.code), dim(TEXT));
                if let Some(id) = &f.entry {
                    line.add(format!("{id}  "), TEXT);
                }
                line.add(&f.message, if f.severity == Severity::Info { dim(TEXT) } else { TEXT });
                let mut hover = f.to_string();
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

    /// A dry run of sync for the chosen scope, on a thread. Planning only reads.
    fn start_plan(&mut self, ctx: &egui::Context) {
        let Ok(paths) = &self.paths else { return };
        let opts = sync::Options {
            repo: (!self.plan_repo.is_empty()).then(|| self.plan_repo.clone()),
            user_only: self.plan_repo.is_empty(),
            adapter: None,
            place: None,
            home: sync::default_home().unwrap_or_default(),
            uninstall: false,
        };
        let (pu, pr) = (paths.public.clone(), paths.private.clone());
        self.planning = Some(Task::start(ctx, move || {
            let stacks = Stacks::load(&pu, pr.as_deref())?;
            sync::plan(&stacks, &opts)
        }));
        (self.plan_pick, self.confirm_apply) = (None, false);
    }

    fn sync_view(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        // Opening the view plans at once: a dry run writes nothing.
        if self.plan.is_none() && self.planning.is_none() && self.applying.is_none() {
            self.start_plan(ctx);
        }
        let mut start_plan = false;
        let Ok(paths) = &self.paths else { return };
        let repos: Vec<String> = self
            .stacks
            .as_ref()
            .and_then(|s| s.private.as_ref())
            .map(|p| p.manifest.repos.keys().chain(p.manifest.workspaces.keys()).cloned().collect())
            .unwrap_or_default();
        let (public, private) = (paths.public.clone(), paths.private.clone());
        ui.horizontal(|ui| {
            ComboBox::from_id_salt("plan-scope")
                .selected_text(mono(if self.plan_repo.is_empty() { "user level" } else { &self.plan_repo }, TEXT))
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut self.plan_repo, String::new(), "user level");
                    for r in &repos {
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
            let changes = matches!(&self.plan, Some(Ok(p)) if p.items.iter().any(|i| i.changes()));
            let conflicts = matches!(&self.plan, Some(Ok(p)) if p.items.iter().any(|i| i.action == Action::Conflict));
            if changes && !busy {
                ui.add_space(20.0);
                let label = if self.confirm_apply { "⇄ apply ✓" } else { "⇄ apply" };
                let hover = if conflicts {
                    "resolve the conflicts first"
                } else if self.confirm_apply {
                    "click again: import memory, back up the owner files it edits, write"
                } else {
                    "write this plan (asks once more)"
                };
                if ui.add_enabled(!conflicts, Button::new(mono(label, AMBER))).on_hover_text(hover).clicked() {
                    if self.confirm_apply {
                        let opts = sync::Options {
                            repo: (!self.plan_repo.is_empty()).then(|| self.plan_repo.clone()),
                            user_only: self.plan_repo.is_empty(),
                            adapter: None,
                            place: None,
                            home: sync::default_home().unwrap_or_default(),
                            uninstall: false,
                        };
                        let (pu, pr) = (public.clone(), private.clone());
                        self.applying = Some(Task::start(ctx, move || {
                            let applied = sync::import_and_apply(&pu, pr.as_deref(), &opts)?;
                            let mut done = applied.done;
                            if let Some(b) = applied.backups {
                                done.push(format!("backups of the owner files changed: {}", b.display()));
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
            ui.label(mono("… planning (a dry run: nothing is written)", dim(TEXT)));
            return;
        };
        let plan = match plan {
            Ok(p) => p,
            Err(e) => {
                ui.label(mono(e, RED));
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
                        ui.label(mono("no text change", dim(TEXT)));
                    } else {
                        ui::diff(ui, &item.diff);
                    }
                },
            );
        }
    }

    // ------------------------------------------------------------ compose

    fn compose_view(&mut self, ui: &mut egui::Ui) {
        let Some(stacks) = &self.stacks else { return };
        let repos: Vec<String> = stacks
            .private
            .iter()
            .flat_map(|p| p.manifest.repos.keys().chain(p.manifest.workspaces.keys()).cloned())
            .collect();
        ui.horizontal(|ui| {
            ComboBox::from_id_salt("adapter")
                .selected_text(mono(self.compose_adapter, TEXT))
                .show_ui(ui, |ui| {
                    for id in adapters::ids() {
                        ui.selectable_value(&mut self.compose_adapter, id, id);
                    }
                });
            ComboBox::from_id_salt("compose-repo")
                .selected_text(mono(
                    if self.compose_repo.is_empty() { "user level" } else { &self.compose_repo },
                    TEXT,
                ))
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut self.compose_repo, String::new(), "user level");
                    for r in &repos {
                        ui.selectable_value(&mut self.compose_repo, r.clone(), r);
                    }
                });
        });
        let Some(adapter) = adapters::get(self.compose_adapter) else { return };
        let composed = if self.compose_repo.is_empty() {
            sync::user_composed(stacks, adapter)
        } else {
            sync::repo_composed(stacks, &self.compose_repo, adapter)
        };
        if composed.text.is_empty() {
            ui.label(mono("nothing applies here", dim(TEXT)));
            return;
        }
        if !composed.overridden.is_empty() {
            ui.label(mono(
                format!("· overridden here: {}", composed.overridden.join(", ")),
                dim(TEXT),
            ));
        }
        // In a repo, the file starts with what keeps the checkout's tracked instructions loading.
        if !self.compose_repo.is_empty() {
            let first = stacks
                .private
                .as_ref()
                .and_then(|p| p.manifest.repos.get(&self.compose_repo))
                .and_then(|r| r.checkouts.first())
                .map(|c| lords_core::config::expand_home(c));
            if let Some(pre) = first.and_then(|c| adapter.repo_preamble(&c)) {
                let lines = pre.lines().count();
                let head = pre.lines().next().unwrap_or_default().to_string();
                let more = if lines > 1 {
                    format!("  (+{} lines: the tracked AGENTS.md, inline)", lines - 1)
                } else {
                    String::new()
                };
                ui.label(mono(format!("{:<8}{head}{more}", ""), BLUE)).on_hover_text(
                    "written first in this repo's file, so the checkout's tracked instructions still load",
                );
            }
        }
        let mut go = None;
        ScrollArea::vertical().id_salt("composed").auto_shrink([false, false]).show(ui, |ui| {
            ui.spacing_mut().item_spacing.y = 1.0;
            for (i, text) in composed.text.lines().enumerate() {
                // An empty line gets no gutter: it belongs to no entry's words.
                let src = composed
                    .sources
                    .iter()
                    .find(|s| s.line == i + 1)
                    .filter(|_| !text.trim().is_empty());
                ui.horizontal_top(|ui| {
                    // The gutter: which layer and entry this line comes from; a fixed width keeps the text aligned.
                    let gutter_width = 56.0;
                    ui.allocate_ui_with_layout(
                        egui::vec2(gutter_width, ui::ROW),
                        egui::Layout::left_to_right(egui::Align::Center),
                        |ui| {
                            ui.set_min_width(gutter_width);
                            if let Some((s, e)) =
                                src.and_then(|s| stacks.find(&s.id).map(|e| (s, e)))
                            {
                                let (tag, hue) = ui::layer(&e.scope);
                                let (g, c) = ui::kind(e.meta.kind);
                                let gutter = RichText::new(format!(" {tag} "))
                                    .font(FontId::monospace(ui::SIZE))
                                    .color(BG)
                                    .background_color(hue);
                                let hover = format!(
                                    "{}\n{} {}\n{}:{}",
                                    s.id,
                                    e.stack,
                                    e.scope,
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
                    let color = if src.is_some() { TEXT } else { dim(TEXT) };
                    ui.add(egui::Label::new(mono(text, color)).wrap());
                });
            }
        });
        if let Some(id) = go {
            self.pick(&id);
            self.view = View::Entries;
        }
    }
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        if let Some(found) = self.checking.as_ref().and_then(|t| t.take()) {
            self.findings = found;
            self.checking = None;
        }
        if let Some(plan) = self.planning.as_ref().and_then(|t| t.take()) {
            self.plan_pick =
                plan.as_ref().ok().and_then(|p| p.items.iter().position(|i| i.changes()));
            self.plan = Some(plan);
            self.planning = None;
        }
        if let Some(done) = self.applying.as_ref().and_then(|t| t.take()) {
            self.applying = None;
            self.notice = Some(done.map(|lines| format!("applied: {}", lines.join("; "))));
            self.plan = None;
            self.reload(ctx);
        }
        let panel = egui::Frame::default().fill(BG).inner_margin(8.0);
        egui::TopBottomPanel::top("top")
            .frame(panel)
            .show_separator_line(false)
            .show(ctx, |ui| self.top(ui, ctx));
        if let Err(e) = &self.paths {
            egui::CentralPanel::default().frame(panel).show(ctx, |ui| {
                ui.label(mono(e, AMBER));
            });
            return;
        }
        if let Some(e) = &self.load_error {
            let e = e.clone();
            egui::CentralPanel::default().frame(panel).show(ctx, |ui| {
                ui.label(mono(e, RED));
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
                    .show(ctx, |ui| self.detail(ui))
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
                egui::CentralPanel::default().frame(panel).show(ctx, |ui| self.compose_view(ui));
            }
        }
    }
}

fn style(ctx: &egui::Context) {
    // egui's fonts lack some glyphs (✦ ♟ ⚒ ◷); Windows' symbol font, if present, fills in.
    let symbols = std::env::var_os("WINDIR")
        .map(|dir| Path::new(&dir).join("Fonts").join("seguisym.ttf"))
        .and_then(|path| lords_core::fsguard::read_bytes(path).ok());
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
