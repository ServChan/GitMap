//! Сама схема: камера, ввод и рисование. Геометрия переходов считается в экранных
//! координатах, поэтому диагонали остаются под 45° при любом масштабе.

use crate::app::{Selection, Session};
use crate::fmt;
use crate::git::Idx;
use crate::layout::{LANE, Line, LineKind, Map, TickLevel};
use crate::theme::{self, Palette, semibold};
use eframe::egui::{
    self, Color32, CornerRadius, FontId, Painter, Pos2, Rect, Sense, Shape, Stroke, StrokeKind,
    epaint::TextShape, pos2, vec2,
};
use std::f32::consts::FRAC_PI_4;
use std::sync::Arc;

const RULER: f32 = 30.0;
const OVERVIEW: f32 = 58.0;
/// Мельче этого шага штрихи обычных коммитов сливаются и не рисуются.
const TICK_GAP: f32 = 6.0;
const CAMERA_SECONDS: f64 = 0.22;

#[derive(Clone, Copy, PartialEq)]
pub struct Cam {
    pub x: f32,
    pub y: f32,
    pub zoom: f32,
}

impl Cam {
    /// По вертикали масштаб меняется медленнее: на общем плане дорожки не слипаются.
    fn zoom_y(&self) -> f32 {
        self.zoom.sqrt().clamp(0.2, 1.35)
    }

    /// От 0 на общем плане до 1 вблизи. На общем плане слитые ветки уходят в фон,
    /// иначе у большого репозитория видна только сплошная штриховка.
    fn detail(&self) -> f32 {
        (self.zoom / 0.5).clamp(0.0, 1.0)
    }
}

#[derive(Clone, Copy)]
pub struct Tween {
    pub from: Cam,
    pub to: Cam,
    pub start: f64,
}

#[derive(Clone, Copy)]
struct View {
    rect: Rect,
    cam: Cam,
    zy: f32,
}

impl View {
    fn new(rect: Rect, cam: Cam) -> Self {
        Self { rect, cam, zy: cam.zoom_y() }
    }
    fn sx(&self, x: f32) -> f32 {
        self.rect.center().x + (x - self.cam.x) * self.cam.zoom
    }
    fn sy(&self, y: f32) -> f32 {
        self.rect.center().y + (y - self.cam.y) * self.zy
    }
    fn world_x(&self, sx: f32) -> f32 {
        self.cam.x + (sx - self.rect.center().x) / self.cam.zoom
    }
    fn world_y(&self, sy: f32) -> f32 {
        self.cam.y + (sy - self.rect.center().y) / self.zy
    }
    fn at(&self, map: &Map, commit: Idx) -> Pos2 {
        pos2(self.sx(map.x[commit as usize]), self.sy(map.y(commit)))
    }
}

enum Hit {
    Commit(Idx),
    Line(u32),
}

fn clamp_cam(cam: &mut Cam, map: &Map, canvas: Rect) {
    let room = (canvas.height() / 2.0 - RULER - 40.0) / cam.zoom_y();
    let reach = |lane: i32| ((lane.abs() as f32 + 1.0) * LANE - room).max(0.0);
    cam.y = cam.y.clamp(-reach(map.min_lane), reach(map.max_lane));
    cam.x = cam.x.clamp(0.0, map.width.max(1.0));
    cam.zoom = cam.zoom.clamp(min_zoom(map, canvas), 3.0);
}

fn fit_zoom(map: &Map, canvas: Rect) -> f32 {
    (canvas.width() - 320.0).max(120.0) / map.width.max(1.0)
}

fn min_zoom(map: &Map, canvas: Rect) -> f32 {
    (fit_zoom(map, canvas) * 0.8).min(1.0)
}

/// Вся схема в окне, основная ветка посередине.
pub fn whole(map: &Map, canvas: Rect) -> Cam {
    Cam { x: map.width / 2.0, y: 0.0, zoom: fit_zoom(map, canvas).min(1.7) }
}

/// Первый вид: последние события в читаемом масштабе.
pub fn latest(map: &Map, canvas: Rect) -> Cam {
    let fit = fit_zoom(map, canvas);
    if fit >= 0.8 {
        return whole(map, canvas);
    }
    let zoom = (canvas.height() / 860.0).clamp(0.9, 1.5);
    let main_tip = map.x[map.lines[0].commits[map.lines[0].commits.len() - 1] as usize];
    let ahead = canvas.width() * 0.2 / zoom;
    Cam { x: (map.width - ahead).min(main_tip + ahead), y: 0.0, zoom }
}

pub fn focus_line(map: &Map, line: &Line, canvas: Rect) -> Cam {
    let (first, last) = (line.commits[0], *line.commits.last().unwrap_or(&line.commits[0]));
    let (a, b) = (map.x[first as usize], map.x[last as usize]);
    let zoom = ((canvas.width() - 420.0).max(160.0) / (b - a).max(1.0)).clamp(min_zoom(map, canvas), 1.2);
    Cam { x: (a + b) / 2.0, y: map.lane_y(line.lane) * 0.6, zoom }
}

pub fn focus_commit(map: &Map, commit: Idx, zoom: f32) -> Cam {
    Cam { x: map.x[commit as usize], y: map.y(commit) * 0.6, zoom: zoom.max(0.8) }
}

pub fn canvas_rect(area: Rect) -> Rect {
    Rect::from_min_max(area.min, pos2(area.max.x, area.max.y - OVERVIEW))
}

pub fn move_camera(session: &mut Session, to: Cam, now: f64) {
    session.auto_frame = false;
    session.tween = (!theme::reduced_motion()).then_some(Tween { from: session.cam, to, start: now });
    if session.tween.is_none() {
        session.cam = to;
    }
}

fn advance_camera(session: &mut Session, ctx: &egui::Context) {
    let Some(tween) = session.tween else { return };
    let t = ((ctx.input(|i| i.time) - tween.start) / CAMERA_SECONDS).clamp(0.0, 1.0) as f32;
    let ease = 1.0 - (1.0 - t).powi(3);
    let mix = |a: f32, b: f32| a + (b - a) * ease;
    session.cam = Cam {
        x: mix(tween.from.x, tween.to.x),
        y: mix(tween.from.y, tween.to.y),
        // Масштаб меняем по степени: равномерно на глаз.
        zoom: tween.from.zoom * (tween.to.zoom / tween.from.zoom).powf(ease),
    };
    if t >= 1.0 {
        session.tween = None;
    } else {
        ctx.request_repaint();
    }
}

pub fn show(ui: &mut egui::Ui, s: &mut Session, pal: &Palette) {
    let area = ui.available_rect_before_wrap();
    let canvas = canvas_rect(area);
    let strip = Rect::from_min_max(pos2(area.min.x, canvas.max.y), area.max);
    let response = ui.allocate_rect(canvas, Sense::click_and_drag());
    response.widget_info(|| {
        egui::WidgetInfo::labeled(
            egui::Role::Image,
            true,
            "Схема веток. Перемещение стрелками, масштаб клавишами плюс и минус.",
        )
    });
    let overview = ui.allocate_rect(strip, Sense::click_and_drag());
    // Пока схему не трогали, кадр следует за размером окна.
    if s.needs_framing || (s.auto_frame && s.framed_size != canvas.size()) {
        s.cam = latest(&s.map, canvas);
        s.framed_size = canvas.size();
        s.needs_framing = false;
    }
    advance_camera(s, ui.ctx());
    handle_input(ui, s, &response, canvas);
    clamp_cam(&mut s.cam, &s.map, canvas);

    let view = View::new(canvas, s.cam);
    let cartouche = cartouche_rect(ui, s, canvas);
    let pointer = response.hover_pos().filter(|p| !cartouche.contains(*p) && p.y > canvas.min.y + RULER);
    let hit = pointer.and_then(|p| hit_test(s, &view, p));
    if hit.is_some() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    if response.clicked() {
        let next = match hit {
            Some(Hit::Commit(commit)) => Selection::Commit(commit),
            Some(Hit::Line(line)) => Selection::Line(line),
            None => Selection::None,
        };
        s.select(next);
    }

    let painter = ui.painter_at(canvas);
    painter.rect_filled(canvas, CornerRadius::ZERO, pal.paper);
    paint_graticule(&painter, s, &view, pal);
    paint_links(&painter, s, &view, pal);
    paint_lines(&painter, s, &view, pal);
    paint_stations(&painter, s, &view, pal);
    paint_labels(ui, &painter, s, &view, pal);
    paint_marks(&painter, s, &view, pal, &hit);
    paint_ruler(&painter, s, &view, pal);
    paint_cartouche(ui, &painter, s, canvas, pal);
    if let (Some(hit), Some(pos)) = (&hit, pointer) {
        paint_tooltip(ui, &painter, s, hit, pos, canvas, pal);
    }

    paint_overview(ui, strip, s, &view, pal);
    if (overview.dragged() || overview.clicked())
        && let Some(pos) = overview.interact_pointer_pos()
    {
        let scale = overview_scale(&s.map, strip);
        s.tween = None;
        s.auto_frame = false;
        s.cam.x = (pos.x - strip.min.x - OVERVIEW_PAD) / scale;
        ui.ctx().request_repaint();
    }
    if response.has_focus() {
        painter.rect_stroke(canvas.shrink(1.0), CornerRadius::ZERO, Stroke::new(2.0, pal.ink), StrokeKind::Inside);
    }
}

fn handle_input(ui: &egui::Ui, s: &mut Session, response: &egui::Response, canvas: Rect) {
    let zy = s.cam.zoom_y();
    if response.dragged() {
        s.tween = None;
        s.auto_frame = false;
        let delta = response.drag_delta();
        s.cam.x -= delta.x / s.cam.zoom;
        s.cam.y -= delta.y / zy;
        ui.ctx().set_cursor_icon(egui::CursorIcon::Grabbing);
    }
    if !response.hovered() {
        return;
    }
    let (scroll, pinch, pointer) =
        ui.input(|i| (i.smooth_scroll_delta, i.zoom_delta(), i.pointer.hover_pos()));
    if scroll.x != 0.0 {
        s.tween = None;
        s.auto_frame = false;
        s.cam.x -= scroll.x / s.cam.zoom;
    }
    let factor = pinch * (scroll.y * 0.0028).exp();
    if (factor - 1.0).abs() > 1e-4 {
        s.tween = None;
        s.auto_frame = false;
        let view = View::new(canvas, s.cam);
        let pivot = pointer.unwrap_or(canvas.center());
        let world = (view.world_x(pivot.x), view.world_y(pivot.y));
        s.cam.zoom = (s.cam.zoom * factor).clamp(min_zoom(&s.map, canvas), 3.0);
        s.cam.x = world.0 - (pivot.x - canvas.center().x) / s.cam.zoom;
        s.cam.y = world.1 - (pivot.y - canvas.center().y) / s.cam.zoom_y();
    }
}

/// Клавиши работают, пока ввод текста не занят.
pub fn handle_keys(ctx: &egui::Context, s: &mut Session, canvas: Rect) {
    if ctx.egui_wants_keyboard_input() {
        return;
    }
    let now = ctx.input(|i| i.time);
    let step = 160.0 / s.cam.zoom;
    let mut target = s.cam;
    let mut moved = false;
    ctx.input(|i| {
        let mut press = |key: egui::Key, apply: &mut dyn FnMut(&mut Cam)| {
            if i.key_pressed(key) {
                apply(&mut target);
                moved = true;
            }
        };
        press(egui::Key::ArrowLeft, &mut |c| c.x -= step);
        press(egui::Key::ArrowRight, &mut |c| c.x += step);
        press(egui::Key::ArrowUp, &mut |c| c.y -= LANE * 2.0);
        press(egui::Key::ArrowDown, &mut |c| c.y += LANE * 2.0);
        press(egui::Key::Plus, &mut |c| c.zoom *= 1.3);
        press(egui::Key::Equals, &mut |c| c.zoom *= 1.3);
        press(egui::Key::Minus, &mut |c| c.zoom /= 1.3);
    });
    if ctx.input(|i| i.key_pressed(egui::Key::Home)) {
        target = whole(&s.map, canvas);
        moved = true;
    }
    if ctx.input(|i| i.key_pressed(egui::Key::End)) {
        target = latest(&s.map, canvas);
        moved = true;
    }
    if moved {
        clamp_cam(&mut target, &s.map, canvas);
        move_camera(s, target, now);
        ctx.request_repaint();
    }
}

fn hit_test(s: &Session, view: &View, pos: Pos2) -> Option<Hit> {
    let map = &s.map;
    let lane = (view.world_y(pos.y) / LANE).round() as i32;
    if (view.sy(map.lane_y(lane)) - pos.y).abs() > 13.0 {
        return None;
    }
    let world = view.world_x(pos.x);
    for (id, line) in map.lines.iter().enumerate() {
        if !line.visible || line.lane != lane {
            continue;
        }
        let at = line.commits.partition_point(|&c| map.x[c as usize] < world);
        let near = [at.checked_sub(1), Some(at).filter(|&a| a < line.commits.len())];
        let nearest = near
            .into_iter()
            .flatten()
            .map(|i| line.commits[i])
            .min_by(|&a, &b| {
                let d = |c: Idx| (map.x[c as usize] - world).abs();
                d(a).total_cmp(&d(b))
            })?;
        if (view.sx(map.x[nearest as usize]) - pos.x).abs() <= 9.0 {
            return Some(Hit::Commit(nearest));
        }
        let (first, last) = (line.commits[0], line.commits[line.commits.len() - 1]);
        if world >= map.x[first as usize] && world <= map.x[last as usize] {
            return Some(Hit::Line(id as u32));
        }
    }
    None
}

fn mix(a: Color32, b: Color32, t: f32) -> Color32 {
    let m = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * t).round() as u8;
    Color32::from_rgb(m(a.r(), b.r()), m(a.g(), b.g()), m(a.b(), b.b()))
}

/// Какая линия выделена сейчас: остальные уходят в бумагу.
fn focused_line(s: &Session) -> Option<u32> {
    match s.selection {
        Selection::Line(line) => Some(line),
        Selection::Commit(commit) => Some(s.map.line_of[commit as usize]),
        Selection::None => None,
    }
}

fn line_color(s: &Session, id: u32, pal: &Palette) -> Color32 {
    let line = &s.map.lines[id as usize];
    let base = match line.kind {
        LineKind::Main => pal.ink,
        LineKind::Active => pal.lines[line.color],
        LineKind::Merged => mix(pal.lines[line.color], pal.paper, 0.74 - 0.42 * s.cam.detail()),
    };
    match focused_line(s) {
        Some(focus) if focus != id && line.kind != LineKind::Main => mix(base, pal.paper, 0.78),
        _ => base,
    }
}

fn line_width(kind: LineKind, zy: f32) -> f32 {
    let k = zy.clamp(0.55, 1.2);
    match kind {
        LineKind::Main => 7.0 * k,
        LineKind::Active => 4.5 * k,
        LineKind::Merged => (2.25 * zy.min(1.2)).max(1.0),
    }
}

/// Скругление углов ломаной, как у линий на схеме метро.
fn rounded(points: &[Pos2], radius: f32) -> Vec<Pos2> {
    let mut out = Vec::with_capacity(points.len() * 6);
    out.push(points[0]);
    for w in points.windows(3) {
        let (a, p, b) = (w[0], w[1], w[2]);
        let (to_a, to_b) = (a - p, b - p);
        let t = radius.min(to_a.length() / 2.0).min(to_b.length() / 2.0);
        let (start, end) = (p + to_a.normalized() * t, p + to_b.normalized() * t);
        for i in 0..=6 {
            let u = i as f32 / 6.0;
            let q = start.to_vec2() * (1.0 - u) * (1.0 - u) + p.to_vec2() * 2.0 * u * (1.0 - u) + end.to_vec2() * u * u;
            out.push(q.to_pos2());
        }
    }
    out.push(points[points.len() - 1]);
    out
}

/// Путь соединения: ответвление сразу уходит на свою дорожку, слияние идёт
/// по своей дорожке и поворачивает только перед целью.
pub fn route(a: Pos2, b: Pos2, fork: bool, radius: f32) -> Vec<Pos2> {
    let dy = (b.y - a.y).abs();
    let dx = b.x - a.x;
    if dy < 0.5 || dx <= 0.5 {
        return vec![a, b];
    }
    let run = dy.min(dx);
    let corner = if fork { pos2(a.x + run, b.y) } else { pos2(b.x - run, a.y) };
    rounded(&[a, corner, b], radius)
}

fn paint_graticule(painter: &Painter, s: &Session, view: &View, pal: &Palette) {
    let rect = view.rect;
    for tick in s.map.ticks.iter().filter(|t| t.level == TickLevel::Year) {
        let x = view.sx(tick.x);
        if x > rect.min.x && x < rect.max.x {
            painter.line_segment([pos2(x, rect.min.y), pos2(x, rect.max.y)], Stroke::new(1.0, pal.rule));
        }
    }
}

fn paint_links(painter: &Painter, s: &Session, view: &View, pal: &Palette) {
    let map = &s.map;
    let clip = view.rect.expand(40.0);
    for link in &map.links {
        let (a, b) = (view.at(map, link.from), view.at(map, link.to));
        if a.x > clip.max.x || b.x < clip.min.x || a.y.max(b.y) < clip.min.y || a.y.min(b.y) > clip.max.y {
            continue;
        }
        // Длинный хвост за краем окна нет смысла считать целиком.
        let (a, b) = if link.fork {
            (a, pos2(b.x.min(clip.max.x + (b.y - a.y).abs()), b.y))
        } else {
            (pos2(a.x.max(clip.min.x - (b.y - a.y).abs()), a.y), b)
        };
        let owner = link.owner;
        let line = &map.lines[owner as usize];
        let width = line_width(line.kind, view.zy);
        let points = route(a, b, link.fork, 10.0 * view.zy);
        painter.add(Shape::line(points, Stroke::new(width, line_color(s, owner, pal))));
    }
}

fn paint_lines(painter: &Painter, s: &Session, view: &View, pal: &Palette) {
    let map = &s.map;
    let clip = view.rect.expand(16.0);
    // Ветки могут уйти дальше основной. Центр схемы остаётся виден и там.
    let main_tip = view.sx(map.x[map.lines[0].commits[map.lines[0].commits.len() - 1] as usize]);
    let (from, to) = (main_tip.max(clip.min.x), view.sx(map.width).min(clip.max.x));
    if from < to {
        let y = view.sy(0.0);
        let dashes = Shape::dashed_line(&[pos2(from, y), pos2(to, y)], Stroke::new(1.5, pal.rule_strong), 6.0, 6.0);
        painter.extend(dashes);
    }
    for kind in [LineKind::Merged, LineKind::Active, LineKind::Main] {
        for (id, line) in map.lines.iter().enumerate() {
            if !line.visible || line.kind != kind {
                continue;
            }
            let y = view.sy(map.lane_y(line.lane));
            if y < clip.min.y || y > clip.max.y {
                continue;
            }
            let (first, last) = (line.commits[0], line.commits[line.commits.len() - 1]);
            let (a, b) = (view.sx(map.x[first as usize]), view.sx(map.x[last as usize]));
            if b < clip.min.x || a > clip.max.x {
                continue;
            }
            let stroke = Stroke::new(line_width(kind, view.zy), line_color(s, id as u32, pal));
            painter.line_segment([pos2(a.max(clip.min.x), y), pos2(b.min(clip.max.x), y)], stroke);
        }
    }
}

fn paint_stations(painter: &Painter, s: &Session, view: &View, pal: &Palette) {
    let map = &s.map;
    let rect = view.rect;
    let detail = view.cam.detail();
    let ring_gap: f32 = if detail < 0.6 { 12.0 } else { 0.0 };
    for (id, line) in map.lines.iter().enumerate() {
        if !line.visible || (line.kind == LineKind::Merged && detail < 0.6) {
            continue;
        }
        let y = view.sy(map.lane_y(line.lane));
        if y < rect.min.y - 12.0 || y > rect.max.y + 12.0 {
            continue;
        }
        let color = line_color(s, id as u32, pal);
        let width = line_width(line.kind, view.zy);
        let radius = width / 2.0 + if line.kind == LineKind::Merged { 2.0 } else { 2.6 };
        let tick_end = y + (width / 2.0 + 4.0) * if line.lane > 0 { 1.0 } else { -1.0 };
        let left = view.world_x(rect.min.x - 12.0);
        let right = view.world_x(rect.max.x + 12.0);
        let from = line.commits.partition_point(|&c| map.x[c as usize] < left);
        let mut last_tick = f32::MIN;
        let mut last_ring = f32::MIN;
        let terminal = line.commits[line.commits.len() - 1];
        for &commit in &line.commits[from..] {
            let wx = map.x[commit as usize];
            if wx > right {
                break;
            }
            let x = view.sx(wx);
            let root = line.kind == LineKind::Main && commit == line.commits[0];
            if root || (commit == terminal && line.kind != LineKind::Merged) {
                continue;
            }
            if map.event[commit as usize] {
                if x - last_ring >= ring_gap.max(radius * 1.6) || (detail >= 0.6 && s.labels.contains_key(&commit)) {
                    last_ring = x;
                    let ring = if line.kind == LineKind::Merged { color } else { pal.ink };
                    painter.circle(pos2(x, y), radius, pal.paper, Stroke::new((width * 0.42).max(1.5), ring));
                }
            } else if detail >= 0.6 && x - last_tick >= TICK_GAP {
                last_tick = x;
                painter.line_segment([pos2(x, y), pos2(x, tick_end)], Stroke::new(1.5, color));
            }
        }
    }
    // Концы линий: корень основной ветки и вершины веток в работе.
    let bar = |x: f32, y: f32, half: f32, stroke: Stroke| {
        painter.line_segment([pos2(x, y - half), pos2(x, y + half)], stroke);
    };
    for (id, line) in map.lines.iter().enumerate() {
        if !line.visible || line.kind == LineKind::Merged {
            continue;
        }
        let y = view.sy(map.lane_y(line.lane));
        let width = line_width(line.kind, view.zy);
        let stroke = Stroke::new(width, line_color(s, id as u32, pal));
        if line.kind == LineKind::Main {
            bar(view.sx(map.x[line.commits[0] as usize]), y, width * 1.6, stroke);
        }
        bar(view.sx(map.x[line.commits[line.commits.len() - 1] as usize]), y, width * 1.6, stroke);
    }
}

/// Занятые места, чтобы подписи не ложились друг на друга.
#[derive(Default)]
struct Placed(Vec<Rect>);

impl Placed {
    fn claim(&mut self, rect: Rect) -> bool {
        if self.0.iter().any(|r| r.intersects(rect)) {
            return false;
        }
        self.0.push(rect);
        true
    }
}

/// Подпись с ореолом цвета бумаги: так делают на картах, чтобы текст читался поверх линий.
fn halo_text(painter: &Painter, pos: Pos2, galley: &Arc<egui::Galley>, angle: f32, color: Color32, halo: Color32) {
    for offset in [vec2(-1.2, 0.0), vec2(1.2, 0.0), vec2(0.0, -1.2), vec2(0.0, 1.2)] {
        let shape = TextShape::new(pos + offset, galley.clone(), halo).with_override_text_color(halo).with_angle(angle);
        painter.add(shape);
    }
    painter.add(TextShape::new(pos, galley.clone(), color).with_angle(angle));
}

fn paint_labels(ui: &egui::Ui, painter: &Painter, s: &Session, view: &View, pal: &Palette) {
    let map = &s.map;
    let rect = view.rect.shrink2(vec2(10.0, 0.0));
    let body = FontId::new(13.0, semibold());
    let small = FontId::proportional(12.5);
    let mut placed = Placed::default();

    // Таблички линий. Вершина может быть за краем окна, тогда табличка держится у края,
    // и понятно, что это за линия.
    let order = (0..map.lines.len()).filter(|&i| map.lines[i].visible && map.lines[i].kind != LineKind::Merged);
    for id in order {
        let line = &map.lines[id];
        let y = view.sy(map.lane_y(line.lane));
        let (first, last) = (line.commits[0], line.commits[line.commits.len() - 1]);
        let (a, b) = (view.sx(map.x[first as usize]), view.sx(map.x[last as usize]));
        if y < rect.min.y + RULER || y > rect.max.y || b < rect.min.x || a > rect.max.x {
            continue;
        }
        let dim = matches!(focused_line(s), Some(f) if f != id as u32 && line.kind != LineKind::Main);
        let fill = if line.kind == LineKind::Main { pal.ink } else { pal.lines[line.color] };
        let text = match line.kind {
            LineKind::Main => pal.paper,
            _ if dim => pal.ink_weak,
            _ => pal.on_line,
        };
        let galley = painter.layout_no_wrap(line.name.clone(), body.clone(), text);
        let size = galley.size() + vec2(14.0, 6.0);
        let width = line_width(line.kind, view.zy);
        let x = (b + width + 10.0).min(rect.max.x - size.x).max(rect.min.x);
        let plate = Rect::from_min_size(pos2(x, y - size.y / 2.0), size);
        if placed.claim(plate.expand(2.0)) {
            if dim {
                painter.rect(plate, CornerRadius::ZERO, pal.paper, Stroke::new(1.0, pal.rule_strong), StrokeKind::Inside);
            } else {
                painter.rect_filled(plate, CornerRadius::ZERO, fill);
            }
            painter.galley(plate.min + vec2(7.0, 3.0), galley, text);
        }
    }

    // Имена слитых веток ложатся над линией, если помещаются на её видимый участок.
    if view.zy > 0.5 {
        for (id, line) in map.lines.iter().enumerate() {
            if !line.visible || line.kind != LineKind::Merged || line.name.is_empty() {
                continue;
            }
            let y = view.sy(map.lane_y(line.lane));
            if y < rect.min.y + RULER + 10.0 || y > rect.max.y {
                continue;
            }
            let (first, last) = (line.commits[0], line.commits[line.commits.len() - 1]);
            let (a, b) = (view.sx(map.x[first as usize]).max(rect.min.x), view.sx(map.x[last as usize]).min(rect.max.x));
            if b - a < 40.0 {
                continue;
            }
            let color = if focused_line(s) == Some(id as u32) { pal.ink } else { pal.lines[line.color] };
            let galley = painter.layout_no_wrap(line.name.clone(), small.clone(), color);
            if galley.size().x > b - a {
                continue;
            }
            let pos = pos2(a.max(b - galley.size().x), y - galley.size().y - 5.0);
            if placed.claim(Rect::from_min_size(pos, galley.size())) {
                halo_text(painter, pos, &galley, 0.0, color, pal.paper);
            }
        }
    }

    // Теги и указатели на станциях: под 45°, как названия станций на схеме.
    if view.cam.zoom > 0.3 {
        let mut last: Vec<(i32, f32)> = Vec::new();
        let left = view.world_x(rect.min.x);
        let from = s.label_order.partition_point(|&c| map.x[c as usize] < left);
        for &commit in &s.label_order[from..] {
            if !map.is_visible(commit) {
                continue;
            }
            let p = view.at(map, commit);
            if p.x > rect.max.x {
                break;
            }
            if p.y < rect.min.y + RULER + 34.0 || p.y > rect.max.y {
                continue;
            }
            let lane = map.lines[map.line_of[commit as usize] as usize].lane;
            if last.iter().any(|&(l, x)| l == lane && (x - p.x).abs() < 17.0) {
                continue;
            }
            last.push((lane, p.x));
            let galley = ui.painter().layout_no_wrap(s.labels[&commit].clone(), small.clone(), pal.ink);
            let radius = 8.0;
            let pos = p + vec2(radius * 0.5, -radius - 4.0);
            halo_text(painter, pos, &galley, -FRAC_PI_4, pal.ink, pal.paper);
        }
    }

    if let Some(head) = s.graph.head.filter(|&h| map.is_visible(h)) {
        let p = view.at(map, head);
        if rect.contains(p) {
            let galley = painter.layout_no_wrap("вы здесь".to_owned(), small.clone(), pal.ink);
            let pos = p + vec2(4.0 - galley.size().x, 12.0);
            if placed.claim(Rect::from_min_size(pos, galley.size())) {
                halo_text(painter, pos, &galley, 0.0, pal.ink, pal.paper);
            }
        }
    }
}

/// Наведение, выделение, найденное.
fn paint_marks(painter: &Painter, s: &Session, view: &View, pal: &Palette, hit: &Option<Hit>) {
    let map = &s.map;
    let ring = |commit: Idx, radius: f32, width: f32| {
        painter.circle(view.at(map, commit), radius, pal.paper, Stroke::new(width, pal.ink));
    };
    if let Some(head) = s.graph.head.filter(|&h| map.is_visible(h)) {
        // Двойное кольцо: то, что сейчас открыто в рабочей папке.
        let p = view.at(map, head);
        painter.circle(p, 7.5, pal.paper, Stroke::new(2.0, pal.ink));
        painter.circle_filled(p, 3.5, pal.ink);
    }
    let found = s.search_hits.iter().filter(|&&c| map.is_visible(c));
    for &commit in found.take(4000) {
        let p = view.at(map, commit);
        if view.rect.contains(p) {
            painter.circle(p, 6.0, pal.ink, Stroke::new(2.0, pal.paper));
        }
    }
    if let Selection::Commit(commit) = s.selection {
        ring(commit, 9.0, 3.0);
        painter.circle_filled(view.at(map, commit), 3.5, pal.ink);
    }
    if let Some(Hit::Commit(commit)) = hit {
        ring(*commit, 7.5, 2.5);
    }
}

fn paint_ruler(painter: &Painter, s: &Session, view: &View, pal: &Palette) {
    let rect = view.rect;
    let band = Rect::from_min_size(rect.min, vec2(rect.width(), RULER));
    painter.rect_filled(band, CornerRadius::ZERO, pal.paper);
    painter.hline(band.x_range(), band.max.y, Stroke::new(1.0, pal.ink));
    let font = FontId::proportional(12.5);
    let strong = FontId::new(12.5, semibold());
    let left = band.min.x + 14.0;

    // Текущий период всегда виден у левого края.
    let cursor = view.world_x(left);
    let at = s.map.ticks.partition_point(|t| t.x <= cursor);
    let current = s.map.ticks.get(at.saturating_sub(1)).map(|t| fmt::month_year(t.time));
    let sticky = current.map(|text| painter.layout_no_wrap(text, strong.clone(), pal.ink));
    let sticky_end = sticky.as_ref().map_or(left, |g| left + g.size().x);

    // Крупные деления подписываются первыми, мелкие занимают то, что осталось.
    let mut used: Vec<(f32, f32)> = vec![(band.min.x, sticky_end + 10.0)];
    let (from, to) = (view.world_x(band.min.x), view.world_x(band.max.x));
    for level in [TickLevel::Year, TickLevel::Month, TickLevel::Day] {
        for tick in s.map.ticks.iter().filter(|t| t.x >= from && t.x <= to && t.level == level) {
            let x = view.sx(tick.x);
            painter.vline(x, band.max.y - if level == TickLevel::Day { 3.0 } else { 6.0 }..=band.max.y, Stroke::new(1.0, pal.ink));
            let text = match level {
                TickLevel::Year => fmt::year(tick.time),
                TickLevel::Month => fmt::month(tick.time),
                TickLevel::Day => fmt::day_of_month(tick.time),
            };
            let (font, color) = if level == TickLevel::Year { (strong.clone(), pal.ink) } else { (font.clone(), pal.ink_weak) };
            let galley = painter.layout_no_wrap(text, font, color);
            let span = (x, x + galley.size().x + 14.0);
            if span.1 < band.max.x && used.iter().all(|u| span.1 < u.0 || span.0 > u.1) {
                used.push(span);
                painter.galley(pos2(x + 4.0, band.min.y + 6.0), galley, color);
            }
        }
    }
    if let Some(galley) = sticky {
        let cover = Rect::from_min_size(band.min, vec2(sticky_end - band.min.x + 8.0, RULER - 1.0));
        painter.rect_filled(cover, CornerRadius::ZERO, pal.paper);
        painter.galley(pos2(left, band.min.y + 6.0), galley, pal.ink);
    }
}

fn cartouche_lines(s: &Session) -> (String, String) {
    (s.title.clone(), s.caption.clone())
}

fn cartouche_rect(ui: &egui::Ui, s: &Session, canvas: Rect) -> Rect {
    let (title, caption) = cartouche_lines(s);
    let top = ui.painter().layout_no_wrap(title, FontId::new(38.0, theme::display()), Color32::WHITE);
    let sub = ui.painter().layout_no_wrap(caption, FontId::proportional(13.5), Color32::WHITE);
    let size = vec2(top.size().x.max(sub.size().x) + 32.0, top.size().y + sub.size().y + 24.0);
    Rect::from_min_size(canvas.min + vec2(0.0, RULER + 1.0), size)
}

/// Заглавие схемы в углу, как на печатной карте.
fn paint_cartouche(ui: &egui::Ui, painter: &Painter, s: &Session, canvas: Rect, pal: &Palette) {
    let rect = cartouche_rect(ui, s, canvas);
    if rect.width() > canvas.width() * 0.8 {
        return;
    }
    painter.rect_filled(rect, CornerRadius::ZERO, pal.paper);
    painter.hline(rect.x_range(), rect.max.y, Stroke::new(1.0, pal.ink));
    painter.vline(rect.max.x, rect.y_range(), Stroke::new(1.0, pal.ink));
    let (title, caption) = cartouche_lines(s);
    let top = painter.layout_no_wrap(title, FontId::new(38.0, theme::display()), pal.ink);
    let height = top.size().y;
    painter.galley(rect.min + vec2(14.0, 8.0), top, pal.ink);
    let sub = painter.layout_no_wrap(caption, FontId::proportional(13.5), pal.ink_weak);
    painter.galley(rect.min + vec2(14.0, 10.0 + height), sub, pal.ink_weak);
}

fn paint_tooltip(ui: &egui::Ui, painter: &Painter, s: &Session, hit: &Hit, pos: Pos2, canvas: Rect, pal: &Palette) {
    let (title, meta) = match *hit {
        Hit::Commit(commit) => {
            let c = &s.graph.commits[commit as usize];
            let hash = c.id.to_hex_with_len(8).to_string();
            (c.subject.clone(), format!("{hash}   {}   {}", c.author, fmt::day(c.time)))
        }
        Hit::Line(id) => {
            let line = &s.map.lines[id as usize];
            let name = if line.name.is_empty() { "Ветка без имени".to_owned() } else { line.name.clone() };
            (name, fmt::count(line.commits.len(), "коммит", "коммита", "коммитов"))
        }
    };
    let wrap = 360.0;
    let head = ui.painter().layout(title, FontId::new(14.0, semibold()), pal.paper, wrap);
    let foot = ui.painter().layout(meta, FontId::proportional(12.5), pal.paper, wrap);
    let size = vec2(head.size().x.max(foot.size().x) + 20.0, head.size().y + foot.size().y + 18.0);
    let mut min = pos + vec2(16.0, 16.0);
    if min.x + size.x > canvas.max.x - 8.0 {
        min.x = pos.x - size.x - 12.0;
    }
    if min.y + size.y > canvas.max.y - 8.0 {
        min.y = pos.y - size.y - 12.0;
    }
    let rect = Rect::from_min_size(min, size);
    painter.rect_filled(rect, CornerRadius::ZERO, pal.ink);
    let height = head.size().y;
    painter.galley(rect.min + vec2(10.0, 7.0), head, pal.paper);
    painter.galley(rect.min + vec2(10.0, 10.0 + height), foot, pal.paper);
}

const OVERVIEW_PAD: f32 = 14.0;

fn overview_scale(map: &Map, strip: Rect) -> f32 {
    (strip.width() - OVERVIEW_PAD * 2.0) / map.width.max(1.0)
}

/// Вся схема полосой внизу: где мы и что вокруг.
fn paint_overview(ui: &egui::Ui, strip: Rect, s: &Session, view: &View, pal: &Palette) {
    let painter = ui.painter_at(strip);
    let map = &s.map;
    painter.rect_filled(strip, CornerRadius::ZERO, pal.paper);
    painter.hline(strip.x_range(), strip.min.y, Stroke::new(1.0, pal.ink));
    let scale = overview_scale(map, strip);
    let lanes = map.max_lane.abs().max(map.min_lane.abs()).max(1) as f32;
    let step = ((strip.height() / 2.0 - 8.0) / lanes).min(8.0);
    let cy = strip.center().y;
    let x = |wx: f32| strip.min.x + OVERVIEW_PAD + wx * scale;
    for line in map.lines.iter().rev().filter(|l| l.visible) {
        let (first, last) = (line.commits[0], line.commits[line.commits.len() - 1]);
        let y = cy + line.lane as f32 * step;
        let (a, b) = (x(map.x[first as usize]), x(map.x[last as usize]).max(x(map.x[first as usize]) + 1.5));
        let (color, width) = match line.kind {
            LineKind::Main => (pal.ink, 3.0),
            LineKind::Active => (pal.lines[line.color], 2.0),
            LineKind::Merged => (mix(pal.lines[line.color], pal.paper, 0.45), 1.0),
        };
        painter.line_segment([pos2(a, y), pos2(b, y)], Stroke::new(width, color));
    }
    let a = x(view.world_x(view.rect.min.x)).max(strip.min.x);
    let b = x(view.world_x(view.rect.max.x)).min(strip.max.x);
    let window = Rect::from_min_max(pos2(a, strip.min.y + 4.0), pos2(b.max(a + 6.0), strip.max.y - 4.0));
    painter.rect_stroke(window, CornerRadius::ZERO, Stroke::new(1.5, pal.ink), StrokeKind::Inside);
}

/// Заглушка, пока схемы нет: линия уже лежит по центру, сразу видно, чем это станет.
pub fn placeholder(ui: &mut egui::Ui, pal: &Palette, title: &str, body: &str, action: Option<&str>) -> bool {
    let rect = ui.available_rect_before_wrap();
    let painter = ui.painter_at(rect);
    let cy = rect.center().y;
    let k = (rect.width() / 1000.0).clamp(0.7, 1.4);
    let (left, right) = (rect.min.x + 48.0, rect.max.x - 48.0);
    let px = |t: f32| left + (right - left) * t;

    let branches: [(f32, f32, f32, f32, usize); 3] = [
        (0.16, 0.44, -1.0, 44.0, 1),
        (0.30, 0.58, 1.0, 44.0, 0),
        (0.56, 0.90, -1.0, 88.0, 2),
    ];
    for (from, to, side, height, color) in branches {
        let y = cy + side * height * k;
        let color = pal.lines[color];
        let stroke = Stroke::new(4.5, color);
        let turn = px(to) - height * k;
        painter.add(Shape::line(route(pos2(px(from), cy), pos2(turn, y), true, 10.0), stroke));
        painter.add(Shape::line(route(pos2(turn, y), pos2(px(to), cy), false, 10.0), stroke));
        for t in 1..5 {
            let x = turn - t as f32 * 34.0;
            painter.line_segment([pos2(x, y), pos2(x, y + side * 9.0)], Stroke::new(1.5, color));
        }
    }
    painter.line_segment([pos2(left, cy), pos2(right, cy)], Stroke::new(7.0, pal.ink));
    painter.line_segment([pos2(left, cy - 11.0), pos2(left, cy + 11.0)], Stroke::new(7.0, pal.ink));
    painter.line_segment([pos2(right, cy - 11.0), pos2(right, cy + 11.0)], Stroke::new(7.0, pal.ink));
    for (from, to, ..) in branches {
        for t in [from, to] {
            painter.circle(pos2(px(t), cy), 6.5, pal.paper, Stroke::new(3.0, pal.ink));
        }
    }

    let text_left = rect.min.x + 48.0;
    let top = cy - 88.0 * k - 150.0;
    let title_font = FontId::new(60.0, theme::display());
    let head = painter.layout(title.to_owned(), title_font, pal.ink, rect.width() - 96.0);
    let head_h = head.size().y;
    painter.galley(pos2(text_left, top.max(rect.min.y + 32.0)), head, pal.ink);
    let sub = painter.layout(body.to_owned(), FontId::proportional(16.0), pal.ink_weak, 520.0);
    let sub_y = top.max(rect.min.y + 32.0) + head_h + 6.0;
    let sub_h = sub.size().y;
    painter.galley(pos2(text_left, sub_y), sub, pal.ink_weak);

    let mut clicked = false;
    if let Some(label) = action {
        let at = Rect::from_min_size(pos2(text_left, sub_y + sub_h + 14.0), vec2(220.0, 34.0));
        let button = egui::Button::new(egui::RichText::new(label).color(pal.paper).family(semibold())).fill(pal.ink);
        clicked = ui.put(at, button).clicked();
    }
    clicked
}
