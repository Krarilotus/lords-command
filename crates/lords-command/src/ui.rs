//! overseer's visual language for entries: one line each, symbols and colours, a frame that says what it is,
//! words in the hover. Solid frame: active; dashed: retired. Square corners: private; rounded: public.

use eframe::egui::{
    self, text::LayoutJob, Color32, FontId, Pos2, Rect, RichText, Sense, Shape, Stroke, TextFormat,
    Vec2,
};
use lords_core::finding::Severity;
use lords_core::model::{Entry, Kind, Origin, Scope, Visibility};

pub const BG: Color32 = Color32::from_rgb(4, 9, 6);
pub const GREEN: Color32 = Color32::from_rgb(57, 255, 136);
pub const AMBER: Color32 = Color32::from_rgb(255, 164, 58);
pub const BLUE: Color32 = Color32::from_rgb(117, 209, 255);
pub const RED: Color32 = Color32::from_rgb(255, 64, 80);
pub const TEXT: Color32 = Color32::from_rgb(170, 200, 180);
pub const EDGE: Color32 = Color32::from_rgb(35, 105, 62);
pub const PICKED: Color32 = Color32::from_rgb(14, 40, 24);
pub const ROW: f32 = 20.0;
pub const SIZE: f32 = 12.0;

/// Tag hues, apart from the four signal colours so a tag never reads as a state (overseer's set).
const HUES: [Color32; 8] = [
    Color32::from_rgb(190, 140, 255),
    Color32::from_rgb(255, 120, 200),
    Color32::from_rgb(0, 210, 190),
    Color32::from_rgb(200, 230, 90),
    Color32::from_rgb(230, 200, 140),
    Color32::from_rgb(150, 170, 255),
    Color32::from_rgb(255, 150, 110),
    Color32::from_rgb(180, 180, 180),
];

pub fn dim(color: Color32) -> Color32 {
    color.gamma_multiply(0.4)
}

pub fn mono(text: impl Into<String>, color: Color32) -> RichText {
    RichText::new(text.into()).font(FontId::monospace(SIZE)).color(color)
}

/// Each kind's glyph and hue.
pub fn kind(k: Kind) -> (&'static str, Color32) {
    match k {
        Kind::Intent => ("✦", HUES[0]),
        Kind::Role => ("♟", HUES[1]),
        Kind::Rule => ("§", HUES[2]),
        Kind::Fact => ("ⓘ", HUES[3]),
        Kind::Status => ("◷", HUES[4]),
        Kind::Template => ("▤", HUES[5]),
        Kind::Skill => ("⚒", HUES[6]),
        Kind::Memory => ("✎", HUES[7]),
    }
}

/// Where an entry came from: ● own, ◐ adapted, ○ inherited, ↧ imported.
pub fn origin(o: &Origin) -> (&'static str, Color32) {
    match o {
        Origin::Own => ("●", GREEN),
        Origin::Adapted(_) => ("◐", AMBER),
        Origin::Inherited(_) => ("○", BLUE),
        Origin::Imported { .. } => ("↧", TEXT),
        Origin::InRepo { .. } => ("⌂", BLUE),
    }
}

pub fn severity(s: Severity) -> (&'static str, Color32) {
    match s {
        Severity::Error => ("✕", RED),
        Severity::Warning => ("⚠", AMBER),
        Severity::Candidate => ("?", BLUE),
        Severity::Info => ("·", TEXT),
    }
}

fn hue_of(name: &str) -> Color32 {
    let h = name.bytes().fold(7u32, |h, b| h.wrapping_mul(31).wrapping_add(b as u32));
    HUES[h as usize % HUES.len()]
}

/// A layer's two-letter tag and hue: GL for global; a project in capitals, a repo in small letters.
pub fn layer(scope: &Scope) -> (String, Color32) {
    let two = |s: &str| s.chars().filter(|c| c.is_alphanumeric()).take(2).collect::<String>();
    match scope {
        Scope::Global => ("GL".into(), HUES[4]),
        Scope::Project(p) => (two(p).to_uppercase(), hue_of(p)),
        Scope::Repo(r) => (two(r).to_lowercase(), hue_of(r)),
        Scope::Workspace(w) => (format!("~{}", two(w).chars().next().unwrap_or('?')), hue_of(w)),
        Scope::Path { repo, .. } => {
            (format!("{}/", two(repo).chars().next().unwrap_or('?')), hue_of(repo))
        }
    }
}

/// One line's coloured parts.
#[derive(Default)]
pub struct Line {
    parts: Vec<(String, Color32, Color32)>,
    right: Vec<(String, Color32)>,
}

impl Line {
    pub fn add(&mut self, text: impl Into<String>, color: Color32) -> &mut Self {
        self.parts.push((text.into(), color, Color32::TRANSPARENT));
        self
    }

    pub fn tag(&mut self, text: &str, hue: Color32) -> &mut Self {
        self.parts.push((format!(" {text} "), BG, hue));
        self.add(" ", TEXT)
    }

    /// Right-aligned, never cut (provenance, findings).
    pub fn right(&mut self, text: impl Into<String>, color: Color32) -> &mut Self {
        self.right.push((text.into(), color));
        self
    }

    fn job(parts: &[(String, Color32, Color32)], width: Option<f32>) -> LayoutJob {
        let mut job = LayoutJob::default();
        for (text, color, background) in parts {
            let format = TextFormat {
                font_id: FontId::monospace(SIZE),
                color: *color,
                background: *background,
                ..Default::default()
            };
            job.append(text, 0.0, format);
        }
        if let Some(width) = width {
            job.wrap.max_width = width;
            job.wrap.max_rows = 1;
            job.wrap.break_anywhere = true;
            job.wrap.overflow_character = Some('…');
        }
        job
    }
}

#[derive(Clone, Copy)]
pub struct Frame {
    pub dashed: bool,
    pub round: bool,
}

impl Frame {
    pub fn of(e: &Entry) -> Frame {
        Frame { dashed: !e.active(), round: e.stack == Visibility::Public }
    }
}

/// A rectangle's outline with corners rounded by `radius`, as one closed polyline.
fn outline(rect: Rect, radius: f32) -> Vec<Pos2> {
    if radius <= 0.0 {
        let (a, b, c, d) =
            (rect.left_top(), rect.right_top(), rect.right_bottom(), rect.left_bottom());
        return vec![a, b, c, d, a];
    }
    let corners = [
        (rect.right_bottom() - Vec2::splat(radius), 0.0),
        (rect.left_bottom() + Vec2::new(radius, -radius), 0.5),
        (rect.left_top() + Vec2::splat(radius), 1.0),
        (rect.right_top() + Vec2::new(-radius, radius), 1.5),
    ];
    let mut points = Vec::new();
    for (center, start) in corners {
        for step in 0..=4 {
            let angle = std::f32::consts::PI * (start + step as f32 / 8.0);
            points.push(center + radius * Vec2::angled(angle));
        }
    }
    points.push(points[0]);
    points
}

/// Draws one framed line; the hover carries the words.
pub fn draw(
    ui: &mut egui::Ui,
    line: &Line,
    frame: Option<Frame>,
    picked: bool,
    hover: &str,
) -> egui::Response {
    let (rect, response) =
        ui.allocate_exact_size(Vec2::new(ui.available_width(), ROW), Sense::click());
    let painter = ui.painter_at(rect);
    let edge = rect.shrink2(Vec2::new(1.0, 2.0));
    let radius = if frame.is_some_and(|f| f.round) { 6.0 } else { 0.0 };
    if picked {
        painter.rect_filled(edge, radius, PICKED);
    } else if response.hovered() {
        painter.rect_filled(edge, radius, PICKED.gamma_multiply(0.5));
    }
    if let Some(frame) = frame {
        let points = outline(edge, radius);
        let stroke = Stroke::new(1.0, EDGE);
        if frame.dashed {
            painter.extend(Shape::dashed_line(&points, stroke, 4.0, 3.0));
        } else {
            painter.add(Shape::line(points, stroke));
        }
    }
    let mut right_x = edge.right() - 7.0;
    for (text, color) in line.right.iter().rev() {
        let g = ui.fonts(|f| f.layout_no_wrap(text.clone(), FontId::monospace(SIZE), *color));
        right_x -= g.size().x;
        painter.galley(Pos2::new(right_x, rect.center().y - g.size().y / 2.0), g, *color);
        right_x -= 4.0;
    }
    let width = (right_x - edge.left() - 12.0).max(10.0);
    let galley = ui.fonts(|f| f.layout_job(Line::job(&line.parts, Some(width))));
    let said = format!("{} {}", hover.lines().next().unwrap_or_default(), galley.text());
    response.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, &said));
    painter.galley(
        Pos2::new(edge.left() + 7.0, rect.center().y - galley.size().y / 2.0),
        galley,
        TEXT,
    );
    if hover.is_empty() {
        response
    } else {
        response.on_hover_text(hover)
    }
}

/// A toggle drawn as its symbol: bright when on, dim when off.
pub fn toggle(
    ui: &mut egui::Ui,
    on: &mut bool,
    symbol: &str,
    color: Color32,
    hover: &str,
) -> egui::Response {
    let text = mono(symbol, if *on { color } else { dim(color) });
    let r = ui.add(egui::Button::new(text).frame(false)).on_hover_text(hover);
    if r.clicked() {
        *on = !*on;
    }
    r
}

/// The `?` legend: each symbol and its words.
pub fn legend(ui: &mut egui::Ui) {
    let mut rows: Vec<(String, Color32, &str)> = Vec::new();
    for (k, words) in [
        (Kind::Intent, "intent"),
        (Kind::Role, "role"),
        (Kind::Rule, "rule"),
        (Kind::Fact, "fact"),
        (Kind::Status, "status"),
        (Kind::Template, "template"),
        (Kind::Skill, "skill"),
        (Kind::Memory, "memory"),
    ] {
        let (g, c) = kind(k);
        rows.push((g.into(), c, words));
    }
    for (g, c, w) in [
        ("●", GREEN, "own"),
        ("◐", AMBER, "adapted (source on hover, click opens it)"),
        ("○", BLUE, "inherited, verbatim"),
        ("↧", TEXT, "imported memory"),
        ("⌂", BLUE, "a skill tracked in its repo, indexed in place"),
        (" ~h ", HUES[4], "a workspace (a working dir with no git repo)"),
        (" GL ", HUES[4], "global layer; PR project; re repo"),
        ("─", TEXT, "active"),
        ("┄", TEXT, "retired"),
        ("▢", TEXT, "public (rounded)"),
        ("□", TEXT, "private (square)"),
        ("✕", RED, "error"),
        ("⚠", AMBER, "warning"),
        ("?", BLUE, "for the owner to decide"),
    ] {
        rows.push((g.into(), c, w));
    }
    egui::Grid::new("legend").spacing([8.0, 1.0]).show(ui, |ui| {
        for (symbol, color, words) in rows {
            ui.label(mono(symbol, color));
            ui.label(mono(words, TEXT));
            ui.end_row();
        }
    });
}

/// A patch or diff, its lines coloured: added green, removed red, hunk headers blue.
pub fn diff(ui: &mut egui::Ui, text: &str) {
    for l in text.lines() {
        let color = match l.chars().next() {
            Some('+') if !l.starts_with("+++") => GREEN,
            Some('-') if !l.starts_with("---") => RED,
            Some('@') => BLUE,
            _ => dim(TEXT),
        };
        ui.label(mono(l, color));
    }
}
