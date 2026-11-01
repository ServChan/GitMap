//! Схема: камера на пружинах, ввод и рисование. Геометрия считается в экранных
//! координатах, поэтому переходы между дорожками остаются под 45° при любом масштабе.
//!
//! При открытии проекта основная линия прочерчивается слева направо, а ветки прорастают
//! от своих развилок в тот момент, когда до них дошла линия. Времена считаются один раз,
//! всё остальное вычисляется из них: никакого списка анимаций хранить не нужно.

use crate::anim::{self, Spring, clamp01, ease_back, ease_out, lerp, smoothstep};
use crate::app::{Selection, Session};
use crate::fmt;
use crate::git::Idx;
use crate::layout::{LANE, Line as MapLine, LineKind, Map, TickLevel};
use crate::text::{self, Face};
use crate::theme::{fade, mix};
use crate::ui::{Cursor, Key, Ui, id, polyline};
use std::collections::{HashMap, HashSet};
use std::f64::consts::FRAC_PI_4;
use vello::kurbo::{Affine, BezPath, Cap, Circle, Join, Line, Point, Rect, RoundedRect, Stroke, Vec2};
use vello::peniko::{Color, Fill};

/// За сколько основная линия проходит видимую часть схемы.
const SWEEP: f32 = 0.95;
/// Сколько растёт одна ветка.
const GROW: f32 = 0.55;
const TICK_GAP: f64 = 6.0;
const TIMELINE: f64 = 56.0;
const TOP_BAND: f64 = 46.0;

pub struct Camera {
    x: Spring,
    y: Spring,
    /// Логарифм масштаба: изменение выглядит равномерным на глаз.
    zoom: Spring,
}

impl Camera {
    pub fn new() -> Self {
        Self { x: Spring::new(0.0), y: Spring::new(0.0), zoom: Spring::new(0.0) }
    }

    fn zoom_target(&self) -> f64 {
        (self.zoom.target as f64).exp()
    }

    fn jump(&mut self, shot: Shot) {
        self.x.set(shot.x as f32);
        self.y.set(shot.y as f32);
        self.zoom.set(shot.zoom.ln() as f32);
    }

    pub fn aim(&mut self, shot: Shot) {
        self.x.target = shot.x as f32;
        self.y.target = shot.y as f32;
        self.zoom.target = shot.zoom.ln() as f32;
    }

    fn step(&mut self, dt: f32) -> bool {
        let moving = !(self.x.settled() && self.y.settled() && self.zoom.settled());
        if moving {
            self.x.step(dt, anim::CAMERA);
            self.y.step(dt, anim::CAMERA);
            self.zoom.step(dt, anim::CAMERA);
        }
        moving
    }
}

#[derive(Clone, Copy)]
pub struct Shot {
    pub x: f64,
    pub y: f64,
    pub zoom: f64,
}

/// Всё, что схема помнит между кадрами.
pub struct Motion {
    pub camera: Camera,
    pub opened: Option<f64>,
    /// Когда начинает расти каждая линия, от момента открытия.
    starts: Vec<f32>,
    sweep: (f64, f64),
    /// Насколько линия в фокусе: 1 обычная, меньше приглушена.
    lit: Vec<f32>,
    /// Указатель с запаздыванием: лупа на линиях движется мягко.
    lens: Option<Point>,
    lens_power: f32,
    press: Option<Point>,
    velocity: Vec2,
    last_pointer: Option<Point>,
    ripples: Vec<(Idx, f64)>,
    /// Что подсвечено наведением на схеме или в списке.
    pub peek: Option<u32>,
    framed_for: (f64, f64),
    pub auto_frame: bool,
    tooltip: Option<Hit>,
    tooltip_amount: f32,
    /// Линии, дорастающие до новых коммитов: откуда и с какого момента.
    extend: HashMap<usize, (f64, f64)>,
    follow_tip: bool,
}

impl Motion {
    pub fn new(map: &Map) -> Self {
        Self {
            camera: Camera::new(),
            opened: None,
            starts: Vec::new(),
            sweep: (0.0, 1.0),
            lit: vec![1.0; map.lines.len()],
            lens: None,
            lens_power: 0.0,
            press: None,
            velocity: Vec2::ZERO,
            last_pointer: None,
            ripples: Vec::new(),
            peek: None,
            framed_for: (0.0, 0.0),
            auto_frame: true,
            tooltip: None,
            tooltip_amount: 0.0,
            extend: HashMap::new(),
            follow_tip: false,
        }
    }

    /// Репозиторий перечитан: всё остаётся на месте, новое подъезжает на линию.
    pub fn absorb(&mut self, map: &Map, fresh: &HashSet<Idx>, now: f64) {
        self.lit = vec![1.0; map.lines.len()];
        self.peek = None;
        self.tooltip = None;
        self.press = None;
        let t = (now - self.opened.unwrap_or(now)) as f32;
        self.starts = map.lines.iter().map(|l| if l.commits.iter().all(|c| fresh.contains(c)) { t } else { -10.0 }).collect();
        self.extend.clear();
        for (id, line) in map.lines.iter().enumerate() {
            let last = line.commits[line.commits.len() - 1];
            if fresh.contains(&last)
                && let Some(known) = line.commits.iter().rev().find(|c| !fresh.contains(c))
            {
                self.extend.insert(id, (map.x[*known as usize] as f64, now));
            }
        }
        for &commit in fresh.iter().take(6) {
            self.ripples.push((commit, now + 0.35));
        }
        self.follow_tip = self.auto_frame && !fresh.is_empty();
    }

    /// Волна от станции: выбор коммита виден на схеме, откуда бы он ни пришёл.
    pub fn ring(&mut self, commit: Idx, now: f64) {
        self.ripples.push((commit, now));
    }

    pub fn replay(&mut self, map: &Map) {
        self.opened = None;
        self.lit = vec![1.0; map.lines.len()];
        self.peek = None;
        self.tooltip = None;
        self.auto_frame = true;
    }
}

#[derive(Clone, Copy)]
struct View {
    center: Point,
    x: f64,
    y: f64,
    zoom: f64,
    zy: f64,
    free: Rect,
}

fn zoom_y(zoom: f64) -> f64 {
    zoom.sqrt().clamp(0.22, 1.5)
}

impl View {
    fn new(free: Rect, x: f64, y: f64, zoom: f64) -> Self {
        Self { center: free.center(), x, y, zoom, zy: zoom_y(zoom), free }
    }
    fn of(free: Rect, camera: &Camera) -> Self {
        Self::new(free, camera.x.value as f64, camera.y.value as f64, (camera.zoom.value as f64).exp())
    }
    fn sx(&self, x: f64) -> f64 {
        self.center.x + (x - self.x) * self.zoom
    }
    fn sy(&self, y: f64) -> f64 {
        self.center.y + (y - self.y) * self.zy
    }
    fn wx(&self, sx: f64) -> f64 {
        self.x + (sx - self.center.x) / self.zoom
    }
    fn wy(&self, sy: f64) -> f64 {
        self.y + (sy - self.center.y) / self.zy
    }
    fn at(&self, map: &Map, commit: Idx) -> Point {
        Point::new(self.sx(map.x[commit as usize] as f64), self.sy(map.y(commit) as f64))
    }
    /// От 0 на общем плане до 1 вблизи: на общем плане слитые ветки уходят в фон.
    fn detail(&self) -> f32 {
        ((self.zoom / 0.5) as f32).clamp(0.0, 1.0)
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Hit {
    Commit(Idx),
    Line(u32),
    Work,
}

/// Незакоммиченная работа: пунктирный отросток от HEAD. Он не лежит на дорожке,
/// потому что это ещё не история.
fn ghost(s: &Session, view: &View) -> Option<(Point, Point)> {
    if s.work.is_empty() {
        return None;
    }
    let head = s.graph.head.filter(|&h| s.map.is_visible(h))?;
    let lane = s.map.lines[s.map.line_of[head as usize] as usize].lane;
    let reach = 40.0 * view.zy.clamp(0.7, 1.2);
    let side = if lane < 0 { -1.0 } else { 1.0 };
    let p = view.at(&s.map, head);
    Some((p, p + Vec2::new(reach, reach * side)))
}

/// Докуда линия дошла сейчас: новые коммиты подъезжают, а не появляются скачком.
fn tip_x(s: &Session, id: usize, x1: f64, now: f64) -> f64 {
    match s.motion.extend.get(&id) {
        Some(&(from, start)) => from + (x1 - from) * ease_out(((now - start) / 0.7) as f32) as f64,
        None => x1,
    }
}

fn fit_zoom(map: &Map, free: Rect) -> f64 {
    (free.width() - 200.0).max(120.0) / (map.width as f64).max(1.0)
}

fn min_zoom(map: &Map, free: Rect) -> f64 {
    (fit_zoom(map, free) * 0.8).min(1.0)
}

/// Вся схема в окне, основная ветка посередине.
pub fn whole(map: &Map, free: Rect) -> Shot {
    Shot { x: map.width as f64 / 2.0, y: 0.0, zoom: fit_zoom(map, free).min(1.7) }
}

/// Первый вид: последние события в читаемом масштабе.
pub fn latest(map: &Map, free: Rect) -> Shot {
    if fit_zoom(map, free) >= 0.8 {
        return whole(map, free);
    }
    let zoom = (free.height() / 760.0).clamp(0.9, 1.5);
    let main_tip = map.x[map.lines[0].commits[map.lines[0].commits.len() - 1] as usize] as f64;
    let ahead = free.width() * 0.2 / zoom;
    Shot { x: (map.width as f64 - ahead).min(main_tip + ahead), y: 0.0, zoom }
}

pub fn focus_line(map: &Map, line: &MapLine, free: Rect) -> Shot {
    let (a, b) = line_ends(map, line);
    let zoom = ((free.width() - 260.0).max(160.0) / (b - a).max(1.0)).clamp(min_zoom(map, free), 1.3);
    Shot { x: (a + b) / 2.0, y: map.lane_y(line.lane) as f64 * 0.6, zoom }
}

pub fn focus_commit(map: &Map, commit: Idx, zoom: f64) -> Shot {
    Shot { x: map.x[commit as usize] as f64, y: map.y(commit) as f64 * 0.6, zoom: zoom.max(0.9) }
}

fn line_ends(map: &Map, line: &MapLine) -> (f64, f64) {
    let (first, last) = (line.commits[0], line.commits[line.commits.len() - 1]);
    (map.x[first as usize] as f64, map.x[last as usize] as f64)
}

pub fn current_zoom(s: &Session) -> f64 {
    s.motion.camera.zoom_target()
}

fn clamp_camera(camera: &mut Camera, map: &Map, free: Rect) {
    let zoom = camera.zoom_target().clamp(min_zoom(map, free), 3.0);
    camera.zoom.target = zoom.ln() as f32;
    let room = (free.height() / 2.0 - 70.0) / zoom_y(zoom);
    let reach = |lane: i32| ((lane.abs() as f64 + 1.0) * LANE as f64 - room).max(0.0);
    camera.y.target = (camera.y.target as f64).clamp(-reach(map.min_lane), reach(map.max_lane)) as f32;
    camera.x.target = (camera.x.target as f64).clamp(0.0, (map.width as f64).max(1.0)) as f32;
}

fn begin_reveal(s: &mut Session, view: &View, now: f64, calm: bool) {
    let map = &s.map;
    let motion = &mut s.motion;
    let (left, right) = (view.wx(view.free.x0 - 60.0), view.wx(view.free.x1 + 60.0));
    motion.sweep = (left, right);
    motion.opened = Some(if calm { now - 100.0 } else { now });
    // Обратная функция к ease_out: когда фронт основной линии дойдёт до точки.
    let time_at = |x: f64| {
        let u = clamp01(((x - left) / (right - left)) as f32);
        SWEEP * (1.0 - (1.0_f32 - u).cbrt())
    };
    motion.starts = map
        .lines
        .iter()
        .map(|line| {
            let fork = map
                .links
                .iter()
                .find(|l| l.fork && l.to == line.commits[0])
                .map_or(map.x[line.commits[0] as usize], |l| map.x[l.from as usize]);
            if (fork as f64) < left { -10.0 } else { time_at(fork as f64) + 0.05 * line.depth as f32 }
        })
        .collect();
}

/// Сколько линии уже видно, 0..1, и сколько её соединений со слиянием.
fn grown(s: &Session, id: usize, t: f32) -> (f32, f32) {
    let start = s.motion.starts.get(id).copied().unwrap_or(-10.0);
    (ease_out((t - start) / GROW), ease_out((t - start - GROW * 0.8) / 0.32))
}

fn front(s: &Session, t: f32) -> f64 {
    let (left, right) = s.motion.sweep;
    if t >= SWEEP { f64::MAX } else { left + (right - left) * ease_out(t / SWEEP) as f64 }
}

/// Скругление углов ломаной, как у линий на схеме метро.
fn rounded(points: &[Point], radius: f64, out: &mut Vec<Point>) {
    out.push(points[0]);
    for w in points.windows(3) {
        let (a, p, b) = (w[0], w[1], w[2]);
        let (to_a, to_b) = (a - p, b - p);
        // Угол совпал с концом: у ровно 45° прямого участка нет, скруглять нечего.
        if to_a.hypot() < 1e-6 || to_b.hypot() < 1e-6 {
            out.push(p);
            continue;
        }
        let t = radius.min(to_a.hypot() / 2.0).min(to_b.hypot() / 2.0);
        let (start, end) = (p + to_a.normalize() * t, p + to_b.normalize() * t);
        for i in 0..=8 {
            let u = i as f64 / 8.0;
            let q = start.to_vec2() * (1.0 - u) * (1.0 - u) + p.to_vec2() * 2.0 * u * (1.0 - u) + end.to_vec2() * u * u;
            out.push(q.to_point());
        }
    }
    out.push(points[points.len() - 1]);
}

/// Путь соединения: ответвление сразу уходит на свою дорожку, слияние идёт
/// по своей дорожке и поворачивает только перед целью.
pub fn route(a: Point, b: Point, fork: bool, radius: f64) -> Vec<Point> {
    let dy = (b.y - a.y).abs();
    let dx = b.x - a.x;
    if dy < 0.5 || dx <= 0.5 {
        return vec![a, b];
    }
    let run = dy.min(dx);
    let corner = if fork { Point::new(a.x + run, b.y) } else { Point::new(b.x - run, a.y) };
    let mut out = Vec::with_capacity(12);
    rounded(&[a, corner, b], radius, &mut out);
    out
}

fn length(points: &[Point]) -> f64 {
    points.windows(2).map(|w| (w[1] - w[0]).hypot()).sum()
}

/// Начало ломаной на долю `amount` от длины.
fn trim(points: &[Point], amount: f32) -> Vec<Point> {
    if amount >= 1.0 {
        return points.to_vec();
    }
    let mut left = length(points) * amount.max(0.0) as f64;
    let mut out = vec![points[0]];
    for w in points.windows(2) {
        let step = (w[1] - w[0]).hypot();
        if step >= left {
            out.push(w[0] + (w[1] - w[0]) * (left / step.max(1e-6)));
            break;
        }
        left -= step;
        out.push(w[1]);
    }
    out
}

/// Точка и направление на расстоянии `distance` от начала.
fn along(points: &[Point], mut distance: f64) -> (Point, f64) {
    for w in points.windows(2) {
        let delta = w[1] - w[0];
        let step = delta.hypot();
        if distance <= step || w[1] == points[points.len() - 1] {
            return (w[0] + delta * (distance / step.max(1e-6)).min(1.0), delta.y.atan2(delta.x));
        }
        distance -= step;
    }
    (points[0], 0.0)
}

fn line_width(kind: LineKind, zy: f64) -> f64 {
    let k = zy.clamp(0.5, 1.25);
    match kind {
        LineKind::Main => 8.0 * k,
        LineKind::Active => 5.0 * k,
        LineKind::Merged => (2.4 * zy.min(1.25)).max(1.0),
    }
}

fn line_color(ui: &Ui, s: &Session, id: usize, view: &View) -> Color {
    let line = &s.map.lines[id];
    let pal = ui.pal;
    let base = match line.kind {
        LineKind::Main => pal.ink,
        LineKind::Active => pal.lines[line.color],
        LineKind::Merged => mix(pal.lines[line.color], pal.ground, 0.74 - 0.44 * view.detail()),
    };
    if line.kind == LineKind::Main {
        return base;
    }
    let lit = s.motion.lit.get(id).copied().unwrap_or(1.0);
    mix(base, pal.ground, 0.84 * (1.0 - lit))
}

fn focused_line(s: &Session) -> Option<u32> {
    match s.selection {
        Selection::Line(line) => Some(line),
        Selection::Commit(commit) => Some(s.map.line_of[commit as usize]),
        Selection::None | Selection::Work => s.motion.peek,
    }
}

fn hit_test(s: &Session, view: &View, pos: Point) -> Option<Hit> {
    let map = &s.map;
    if ghost(s, view).is_some_and(|(_, q)| (q - pos).hypot() < 14.0) {
        return Some(Hit::Work);
    }
    let lane = (view.wy(pos.y) / LANE as f64).round() as i32;
    if (view.sy(map.lane_y(lane) as f64) - pos.y).abs() > 14.0 {
        return None;
    }
    let world = view.wx(pos.x) as f32;
    for (id, line) in map.lines.iter().enumerate() {
        if !line.visible || line.lane != lane {
            continue;
        }
        let at = line.commits.partition_point(|&c| map.x[c as usize] < world);
        let near = [at.checked_sub(1), Some(at).filter(|&a| a < line.commits.len())];
        let nearest = near.into_iter().flatten().map(|i| line.commits[i]).min_by(|&a, &b| {
            let d = |c: Idx| (map.x[c as usize] - world).abs();
            d(a).total_cmp(&d(b))
        })?;
        if (view.sx(map.x[nearest as usize] as f64) - pos.x).abs() <= 9.0 {
            return Some(Hit::Commit(nearest));
        }
        let (a, b) = line_ends(map, line);
        if (world as f64) >= a && (world as f64) <= b {
            return Some(Hit::Line(id as u32));
        }
    }
    None
}

fn handle_input(ui: &mut Ui, s: &mut Session, free: Rect, hit: Option<Hit>) {
    let (w, h) = ui.size;
    let window = Rect::new(0.0, TOP_BAND, w, h);
    let pointer = ui.input.pointer.filter(|p| !ui.covered(*p));
    let timeline = timeline_rect(free);
    let over_map = pointer.is_some_and(|p| window.contains(p) && !timeline.contains(p));
    let motion = &mut s.motion;
    let key = id("map");

    if over_map && ui.input.pressed {
        motion.press = ui.input.pointer;
        motion.velocity = Vec2::ZERO;
    }
    let dragging = ui.drag(key, if over_map { window } else { Rect::ZERO });
    if dragging {
        ui.cursor = Cursor::Grabbing;
        if let (Some(p), Some(last)) = (ui.input.pointer, motion.last_pointer) {
            let delta = p - last;
            let zoom = motion.camera.zoom_target();
            let zy = zoom_y(zoom);
            motion.camera.x.target -= (delta.x / zoom) as f32;
            motion.camera.y.target -= (delta.y / zy) as f32;
            // Схема идёт за рукой без запаздывания, пружина нужна только на отпускании.
            motion.camera.x.value = motion.camera.x.target;
            motion.camera.y.value = motion.camera.y.target;
            motion.camera.x.velocity = 0.0;
            motion.camera.y.velocity = 0.0;
            let dt = ui.input.dt.max(1.0 / 240.0) as f64;
            motion.velocity = motion.velocity * 0.7 + delta / dt * 0.3;
            motion.auto_frame = false;
        }
    }
    if ui.input.released {
        if let (Some(start), Some(p)) = (motion.press, ui.input.pointer) {
            let moved = (p - start).hypot();
            if moved < 4.0 {
                let now = ui.input.time;
                if let (Some(from), Some(Hit::Commit(commit))) = (s.compare_from, hit) {
                    s.compare_from = None;
                    s.motion.ripples.push((commit, now));
                    if from != commit {
                        s.compare(from, commit, now);
                    }
                } else {
                    let next = match hit {
                        Some(Hit::Commit(commit)) => Selection::Commit(commit),
                        Some(Hit::Line(line)) => Selection::Line(line),
                        Some(Hit::Work) => Selection::Work,
                        None => Selection::None,
                    };
                    if next != s.selection {
                        s.page_changed = now;
                    }
                    s.select(next);
                    if let Selection::Commit(commit) = next {
                        s.motion.ripples.push((commit, now));
                    }
                }
            } else {
                // Схема катится ещё немного: цель уходит вперёд по скорости руки.
                let zoom = s.motion.camera.zoom_target();
                let glide = s.motion.velocity * 0.22;
                s.motion.camera.x.target -= (glide.x / zoom) as f32;
                s.motion.camera.y.target -= (glide.y / zoom_y(zoom)) as f32;
            }
        }
        s.motion.press = None;
    }
    let motion = &mut s.motion;
    motion.last_pointer = ui.input.pointer;
    if !over_map {
        return;
    }
    if ui.input.wheel_x != 0.0 {
        motion.auto_frame = false;
        motion.camera.x.target -= (ui.input.wheel_x / motion.camera.zoom_target()) as f32;
    }
    if ui.input.wheel != 0.0 {
        motion.auto_frame = false;
        let pivot = ui.input.pointer.unwrap_or(free.center());
        let before = motion.camera.zoom_target();
        let after = (before * (ui.input.wheel * 0.0032).exp()).clamp(min_zoom(&s.map, free), 3.0);
        let center = free.center();
        let world_x = motion.camera.x.target as f64 + (pivot.x - center.x) / before;
        let world_y = motion.camera.y.target as f64 + (pivot.y - center.y) / zoom_y(before);
        motion.camera.zoom.target = after.ln() as f32;
        motion.camera.x.target = (world_x - (pivot.x - center.x) / after) as f32;
        motion.camera.y.target = (world_y - (pivot.y - center.y) / zoom_y(after)) as f32;
    }
}

/// Клавиши работают, пока фокус не в поле ввода или на элементе.
pub fn handle_keys(ui: &Ui, s: &mut Session, free: Rect) {
    if ui.state.focus.is_some() {
        return;
    }
    let camera = &mut s.motion.camera;
    let zoom = camera.zoom_target();
    let step = (180.0 / zoom) as f32;
    for key in &ui.input.keys {
        match key {
            Key::Left => camera.x.target -= step,
            Key::Right => camera.x.target += step,
            Key::Up => camera.y.target -= LANE * 2.0,
            Key::Down => camera.y.target += LANE * 2.0,
            Key::Plus => camera.zoom.target += 0.3,
            Key::Minus => camera.zoom.target -= 0.3,
            Key::Home => camera.aim(whole(&s.map, free)),
            Key::End => camera.aim(latest(&s.map, free)),
            _ => continue,
        }
        s.motion.auto_frame = false;
    }
}

fn timeline_rect(free: Rect) -> Rect {
    Rect::new(free.x0 + 12.0, free.y1 - TIMELINE - 12.0, free.x1 - 12.0, free.y1 - 12.0)
}

pub fn show(ui: &mut Ui, s: &mut Session, free: Rect, alpha: f32) {
    // Пока схему не трогали, кадр следует за размером окна.
    let size = (free.width().round(), free.height().round());
    if s.motion.opened.is_none() || (s.motion.auto_frame && s.motion.framed_for != size) {
        let shot = latest(&s.map, free);
        s.motion.camera.jump(shot);
        s.motion.framed_for = size;
    }
    if s.motion.follow_tip {
        s.motion.follow_tip = false;
        let shot = latest(&s.map, free);
        s.motion.camera.aim(shot);
    }
    let dt = ui.input.dt;
    let view = View::of(free, &s.motion.camera);
    if s.motion.opened.is_none() {
        begin_reveal(s, &view, ui.input.time, ui.calm);
    }
    let pointer = ui.input.pointer.filter(|p| !ui.covered(*p) && p.y > TOP_BAND && !timeline_rect(free).contains(*p));
    let hit = pointer.and_then(|p| hit_test(s, &view, p));
    let choosing = pointer.is_some() && s.compare_from.is_some();
    if (hit.is_some() || choosing) && !ui.any_active() {
        ui.cursor = Cursor::Pointer;
    } else if pointer.is_some() && !ui.any_active() {
        ui.cursor = Cursor::Grab;
    }
    handle_input(ui, s, free, hit);
    clamp_camera(&mut s.motion.camera, &s.map, free);
    if s.motion.camera.step(dt) {
        ui.animating = true;
    }
    let view = View::of(free, &s.motion.camera);

    let t = (ui.input.time - s.motion.opened.unwrap_or(0.0)) as f32;
    if t < SWEEP + GROW + 1.0 {
        ui.animating = true;
    }
    update_focus(ui, s, hit, pointer);

    if alpha < 1.0 {
        let (w, h) = ui.size;
        ui.alpha(alpha, Rect::new(0.0, 0.0, w, h));
    }
    paint_grid(ui, &view);
    paint_graticule(ui, s, &view);
    paint_lines(ui, s, &view, t);
    paint_stations(ui, s, &view, t);
    paint_trains(ui, s, &view, t);
    paint_labels(ui, s, &view, t);
    paint_marks(ui, s, &view, &hit, t);
    paint_work(ui, s, &view, t);
    paint_ruler(ui, s, &view);
    paint_title(ui, s, &view, t);
    paint_timeline(ui, s, free, &view, t);
    if alpha < 1.0 {
        ui.end_alpha();
    }
    paint_tooltip(ui, s, &view, hit, pointer);
}

fn update_focus(ui: &mut Ui, s: &mut Session, hit: Option<Hit>, pointer: Option<Point>) {
    let dt = ui.input.dt;
    let peek_from_map = match hit {
        Some(Hit::Line(line)) => Some(line),
        Some(Hit::Commit(commit)) => Some(s.map.line_of[commit as usize]),
        Some(Hit::Work) | None => None,
    };
    if pointer.is_some() {
        s.motion.peek = peek_from_map;
    }
    let focus = focused_line(s);
    let held = s.selection != Selection::None;
    let lit_low = if held { 0.0 } else { 0.42 };
    let rate = 1.0 - (-dt * 13.0).exp();
    let motion = &mut s.motion;
    for (i, lit) in motion.lit.iter_mut().enumerate() {
        let target = if focus.is_none_or(|f| f as usize == i) { 1.0 } else { lit_low };
        if (*lit - target).abs() > 0.004 {
            *lit += (target - *lit) * rate;
            ui.animating = true;
        } else {
            *lit = target;
        }
    }
    // Лупа догоняет указатель и плавно гаснет вне схемы.
    motion.lens_power += (if pointer.is_some() { 1.0 } else { 0.0 } - motion.lens_power) * rate;
    if let Some(p) = pointer {
        let lens = motion.lens.unwrap_or(p);
        let moved = lens + (p - lens) * (1.0 - (-dt as f64 * 22.0).exp());
        if (moved - p).hypot() > 0.3 {
            ui.animating = true;
        }
        motion.lens = Some(moved);
    }
    if motion.lens_power > 0.01 && motion.lens_power < 0.99 {
        ui.animating = true;
    }
    motion.ripples.retain(|(_, start)| ui.input.time - start < 0.7);
    if !motion.ripples.is_empty() {
        ui.animating = true;
    }
    motion.extend.retain(|_, (_, start)| ui.input.time - *start < 0.75);
    if !motion.extend.is_empty() {
        ui.animating = true;
    }
}

/// Перекрестья картографической сетки. Сдвигаются медленнее линий: схема получает глубину.
fn paint_grid(ui: &mut Ui, view: &View) {
    let (w, h) = ui.size;
    let step = 112.0;
    let dx = (-view.x * view.zoom * 0.12).rem_euclid(step);
    let dy = (-view.y * view.zy * 0.12).rem_euclid(step);
    let mut path = BezPath::new();
    let arm = 3.5;
    let mut y = dy - step;
    while y < h + step {
        let mut x = dx - step;
        while x < w + step {
            path.move_to((x - arm, y));
            path.line_to((x + arm, y));
            path.move_to((x, y - arm));
            path.line_to((x, y + arm));
            x += step;
        }
        y += step;
    }
    let color = ui.pal.rule;
    ui.scene.stroke(&Stroke::new(1.0), Affine::IDENTITY, color, None, &path);
}

fn paint_graticule(ui: &mut Ui, s: &Session, view: &View) {
    let (_, h) = ui.size;
    let color = fade(ui.pal.rule, 0.9);
    for tick in s.map.ticks.iter().filter(|t| t.level == TickLevel::Year) {
        let x = view.sx(tick.x as f64);
        if x > view.free.x0 && x < view.free.x1 {
            let stroke = Stroke::new(1.0).with_dashes(0.0, [2.0, 6.0]);
            ui.scene.stroke(&stroke, Affine::IDENTITY, color, None, &Line::new((x, TOP_BAND), (x, h)));
        }
    }
}

fn draw_line(ui: &mut Ui, points: &[Point], width: f64, color: Color, casing: bool) {
    let Some(path) = polyline(points) else { return };
    let round = Stroke::new(width).with_caps(Cap::Round).with_join(Join::Round);
    if casing {
        // Кант цвета фона: на пересечениях видно, какая линия идёт поверх.
        let case = Stroke::new(width + 4.0).with_caps(Cap::Butt).with_join(Join::Round);
        let ground = ui.pal.ground;
        ui.scene.stroke(&case, Affine::IDENTITY, ground, None, &path);
    }
    ui.scene.stroke(&round, Affine::IDENTITY, color, None, &path);
}

fn paint_lines(ui: &mut Ui, s: &Session, view: &View, t: f32) {
    let map = &s.map;
    let (w, h) = ui.size;
    let clip = Rect::new(-40.0, -40.0, w + 40.0, h + 40.0);
    let main_front = front(s, t);
    for kind in [LineKind::Merged, LineKind::Active, LineKind::Main] {
        // Сначала соединения этого класса, сверху дорожки: стыки не видны.
        for link in &map.links {
            let line = &map.lines[link.owner as usize];
            if line.kind != kind {
                continue;
            }
            let (a, b) = (view.at(map, link.from), view.at(map, link.to));
            if a.x > clip.x1 || b.x < clip.x0 || a.y.max(b.y) < clip.y0 || a.y.min(b.y) > clip.y1 {
                continue;
            }
            let (first_grow, tail) = grown(s, link.owner as usize, t);
            let is_start = link.fork && link.to == line.commits[0];
            let amount = if is_start { clamp01(first_grow / 0.34) } else { tail };
            if amount <= 0.0 {
                continue;
            }
            // Длинный хвост за краем окна считать незачем.
            let (a, b) = if link.fork {
                (a, Point::new(b.x.min(clip.x1 + (b.y - a.y).abs()), b.y))
            } else {
                (Point::new(a.x.max(clip.x0 - (b.y - a.y).abs()), a.y), b)
            };
            let width = line_width(kind, view.zy);
            let color = line_color(ui, s, link.owner as usize, view);
            let points = trim(&route(a, b, link.fork, 12.0 * view.zy), amount);
            draw_line(ui, &points, width, color, kind != LineKind::Merged);
        }
        for (id, line) in map.lines.iter().enumerate() {
            if !line.visible || line.kind != kind {
                continue;
            }
            let y = view.sy(map.lane_y(line.lane) as f64);
            if y < clip.y0 || y > clip.y1 {
                continue;
            }
            let (x0, x1) = line_ends(map, line);
            let x1 = tip_x(s, id, x1, ui.input.time);
            let (mut a, mut b) = (view.sx(x0), view.sx(x1));
            if kind == LineKind::Main {
                // Дальше конца основной линии центр схемы идёт пунктиром.
                let end = view.sx(map.width as f64).min(clip.x1);
                if b < end {
                    let dashes = Stroke::new(2.0).with_dashes((t * 26.0) as f64, [2.0, 9.0]).with_caps(Cap::Round);
                    let color = fade(ui.pal.rule_strong, smoothstep(0.7, 1.2, t));
                    ui.scene.stroke(&dashes, Affine::IDENTITY, color, None, &Line::new((b.max(clip.x0), y), (end, y)));
                }
                b = b.min(view.sx(main_front));
            } else {
                let (amount, _) = grown(s, id, t);
                let share = clamp01((amount - 0.34) / 0.66);
                if share <= 0.0 {
                    continue;
                }
                b = a + (b - a) * share as f64;
            }
            if b < clip.x0 || a > clip.x1 {
                continue;
            }
            a = a.max(clip.x0);
            b = b.min(clip.x1);
            let width = line_width(kind, view.zy);
            let color = line_color(ui, s, id, view);
            if kind != LineKind::Merged {
                // Лёгкая тень: линия лежит на листе, а не нарисована в нём.
                let shadow = fade(ui.pal.shadow, 0.5);
                let band = Rect::new(a, y + 1.0, b, y + width * 0.5 + 3.0);
                ui.scene.draw_blurred_rounded_rect(Affine::IDENTITY, band, shadow, width * 0.4, 2.6);
            }
            draw_line(ui, &[Point::new(a, y), Point::new(b, y)], width, color, kind != LineKind::Merged);
            // Выбранная ветка течёт: по ней идёт бегущий пунктир.
            if s.selection == Selection::Line(id as u32) {
                let flow = Stroke::new((width * 0.32).max(1.2))
                    .with_caps(Cap::Round)
                    .with_dashes(-(ui.input.time * 42.0), [1.0, 13.0]);
                let on = if kind == LineKind::Main { ui.pal.ground } else { ui.pal.sheet };
                ui.scene.stroke(&flow, Affine::IDENTITY, on, None, &Line::new((a, y), (b, y)));
                ui.ambient = true;
            }
        }
    }
}

/// Сила лупы в точке линии: штрихи и станции рядом с указателем подрастают.
fn lens_at(s: &Session, view: &View, x: f64, y: f64) -> f64 {
    let Some(lens) = s.motion.lens else { return 0.0 };
    let (dx, dy) = (x - lens.x, y - lens.y);
    let reach = 90.0;
    let near = (-(dx * dx) / (reach * reach)).exp() * (-(dy * dy) / (34.0 * 34.0)).exp();
    near * s.motion.lens_power as f64 * view.detail() as f64
}

fn paint_stations(ui: &mut Ui, s: &Session, view: &View, t: f32) {
    let map = &s.map;
    let (w, h) = ui.size;
    let detail = view.detail();
    let ring_gap: f64 = if detail < 0.6 { 12.0 } else { 0.0 };
    let main_front = front(s, t);
    let mut ticks: Vec<(BezPath, Color)> = Vec::new();
    for (id, line) in map.lines.iter().enumerate() {
        if !line.visible || (line.kind == LineKind::Merged && detail < 0.6) {
            continue;
        }
        let y = view.sy(map.lane_y(line.lane) as f64);
        if y < -20.0 || y > h + 20.0 {
            continue;
        }
        let color = line_color(ui, s, id, view);
        let width = line_width(line.kind, view.zy);
        let radius = width / 2.0 + if line.kind == LineKind::Merged { 2.0 } else { 2.8 };
        let side = if line.lane > 0 { 1.0 } else { -1.0 };
        let left = view.wx(-12.0) as f32;
        let right = view.wx(w + 12.0) as f32;
        let from = line.commits.partition_point(|&c| map.x[c as usize] < left);
        let mut last_tick = f64::MIN;
        let mut last_ring = f64::MIN;
        let terminal = line.commits[line.commits.len() - 1];
        let (x0, x1) = line_ends(map, line);
        let reached = tip_x(s, id, x1, ui.input.time);
        let (amount, _) = grown(s, id, t);
        let share = clamp01((amount - 0.34) / 0.66);
        let mut path = BezPath::new();
        for &commit in &line.commits[from..] {
            let wx = map.x[commit as usize];
            if wx > right || wx as f64 > reached + 0.5 {
                break;
            }
            let x = view.sx(wx as f64);
            let root = line.kind == LineKind::Main && commit == line.commits[0];
            if root || (commit == terminal && line.kind != LineKind::Merged) {
                continue;
            }
            // Станция выскакивает, когда до неё дошла линия.
            let pop = if line.kind == LineKind::Main {
                let (a, b) = s.motion.sweep;
                let span = ((b - a) * 0.05).max(1.0);
                clamp01(((main_front - wx as f64) / span) as f32)
            } else if share <= 0.0 {
                0.0
            } else {
                let place = if x1 > x0 { ((wx as f64 - x0) / (x1 - x0)) as f32 } else { 1.0 };
                clamp01((share - place) / 0.2 + 0.1)
            };
            if pop <= 0.0 {
                continue;
            }
            let lens = lens_at(s, view, x, y);
            if map.event[commit as usize] {
                let labelled = detail >= 0.6 && s.labels.contains_key(&commit);
                if x - last_ring >= ring_gap.max(radius * 1.6) || labelled {
                    last_ring = x;
                    let scale = ease_back(pop) as f64 * (1.0 + 0.45 * lens);
                    let ring = if line.kind == LineKind::Merged { color } else { ui.pal.ink };
                    let circle = Circle::new((x, y), radius * scale);
                    let surface = ui.pal.sheet;
                    ui.fill(&circle, surface);
                    ui.scene.stroke(&Stroke::new((width * 0.42).max(1.6)), Affine::IDENTITY, ring, None, &circle);
                }
            } else if detail >= 0.6 && (x - last_tick >= TICK_GAP || lens > 0.2) {
                last_tick = x;
                let reach = width / 2.0 + (4.0 + 7.0 * lens) * ease_out(pop) as f64;
                path.move_to((x, y));
                path.line_to((x, y + side * reach));
            }
        }
        ticks.push((path, color));
    }
    for (path, color) in ticks {
        ui.scene.stroke(&Stroke::new(1.6).with_caps(Cap::Round), Affine::IDENTITY, color, None, &path);
    }
    // Концы линий: корень основной ветки и вершины веток в работе.
    for (id, line) in map.lines.iter().enumerate() {
        if !line.visible || line.kind == LineKind::Merged {
            continue;
        }
        let y = view.sy(map.lane_y(line.lane) as f64);
        let width = line_width(line.kind, view.zy);
        let color = line_color(ui, s, id, view);
        let bar = |ui: &mut Ui, x: f64, grow: f32| {
            let half = width * 1.5 * ease_back(grow) as f64;
            if half > 0.0 {
                let shape = Line::new((x, y - half), (x, y + half));
                ui.scene.stroke(&Stroke::new(width).with_caps(Cap::Round), Affine::IDENTITY, color, None, &shape);
            }
        };
        let (x0, x1) = line_ends(map, line);
        let x1 = tip_x(s, id, x1, ui.input.time);
        if line.kind == LineKind::Main {
            bar(ui, view.sx(x0), clamp01(t / 0.2));
            bar(ui, view.sx(x1), if main_front >= x1 { clamp01((t - SWEEP * 0.6) / 0.25) } else { 0.0 });
        } else {
            let (amount, _) = grown(s, id, t);
            bar(ui, view.sx(x1), clamp01((amount - 0.92) / 0.08));
        }
    }
}

/// Поезда: живые маршруты схемы. Идут только по веткам в работе и по центру.
fn paint_trains(ui: &mut Ui, s: &Session, view: &View, t: f32) {
    if ui.calm || view.zoom < 0.3 || t < SWEEP + GROW {
        return;
    }
    let map = &s.map;
    let (w, h) = ui.size;
    let screen = Rect::new(0.0, 0.0, w, h);
    let mut drawn = 0;
    for (id, line) in map.lines.iter().enumerate() {
        if drawn >= 14 {
            break;
        }
        if !line.visible || line.kind == LineKind::Merged || line.commits.len() < 2 {
            continue;
        }
        let (x0, x1) = line_ends(map, line);
        let y = view.sy(map.lane_y(line.lane) as f64);
        let mut points: Vec<Point> = Vec::new();
        if let Some(link) = map.links.iter().find(|l| l.fork && l.to == line.commits[0]) {
            points = route(view.at(map, link.from), view.at(map, link.to), true, 12.0 * view.zy);
        }
        if points.is_empty() {
            points.push(Point::new(view.sx(x0), y));
        }
        points.push(Point::new(view.sx(x1), y));
        let total = length(&points);
        if total < 150.0 {
            continue;
        }
        // Своя скорость и свой сдвиг у каждого маршрута: поезда не ходят строем.
        let seed = crate::ui::id((id, "train")) as f64 / u64::MAX as f64;
        let speed = 70.0 + 50.0 * seed;
        let pause = 0.35;
        let cycle = (ui.input.time * speed / total + seed).rem_euclid(1.0 + pause);
        if cycle > 1.0 {
            continue;
        }
        let (at, angle) = along(&points, cycle * total);
        if !screen.contains(at) {
            continue;
        }
        drawn += 1;
        let edge = smoothstep(0.0, 0.05, cycle as f32) * smoothstep(0.0, 0.05, (1.0 - cycle) as f32);
        let k = view.zy.clamp(0.7, 1.15);
        let color = line_color(ui, s, id, view);
        let place = Affine::translate(at.to_vec2()) * Affine::rotate(angle);
        let body = RoundedRect::new(-15.0 * k, -5.0 * k, 15.0 * k, 5.0 * k, 5.0 * k);
        let surface = fade(ui.pal.sheet, edge);
        ui.scene.fill(Fill::NonZero, place, surface, None, &body);
        ui.scene.stroke(&Stroke::new(2.2), place, fade(color, edge), None, &body);
        // Окна поезда и лобовая часть: сразу видно направление.
        for offset in [-8.5, -2.5, 3.5] {
            let window = RoundedRect::new(offset * k, -2.0 * k, (offset + 4.0) * k, 2.0 * k, 1.0);
            ui.scene.fill(Fill::NonZero, place, fade(color, edge), None, &window);
        }
        ui.ambient = true;
    }
}

fn plate(ui: &mut Ui, rect: Rect, fill: Color, alpha: f32) {
    let pal = ui.pal;
    let down = rect + Vec2::new(0.0, 2.0);
    ui.scene.draw_blurred_rounded_rect(Affine::IDENTITY, down, fade(pal.shadow, 0.8 * alpha), 6.0, 4.0);
    ui.fill(&RoundedRect::from_rect(rect, 6.0), fade(fill, alpha));
}

fn paint_labels(ui: &mut Ui, s: &Session, view: &View, t: f32) {
    let map = &s.map;
    let pal = ui.pal;
    let free = view.free.inset((-10.0, -TOP_BAND.min(view.free.height() / 2.0) + 40.0));
    let (_, h) = ui.size;
    let mut placed: Vec<Rect> = vec![timeline_rect(view.free)];
    let mut claim = |rect: Rect| {
        if placed.iter().any(|r| r.overlaps(rect)) {
            false
        } else {
            placed.push(rect);
            true
        }
    };
    let focus = focused_line(s);

    // Таблички линий. Вершина может быть за краем, тогда табличка держится у края.
    for (id, line) in map.lines.iter().enumerate() {
        if !line.visible || line.kind == LineKind::Merged {
            continue;
        }
        let y = view.sy(map.lane_y(line.lane) as f64);
        let (x0, x1) = line_ends(map, line);
        let (a, b) = (view.sx(x0), view.sx(tip_x(s, id, x1, ui.input.time)));
        if y < TOP_BAND + 20.0 || y > h || b < free.x0 || a > free.x1 {
            continue;
        }
        let appear = if line.kind == LineKind::Main {
            clamp01((t - SWEEP * 0.75) / 0.3)
        } else {
            clamp01((grown(s, id, t).0 - 0.8) / 0.2)
        };
        if appear <= 0.0 {
            continue;
        }
        let dim = line.kind != LineKind::Main && focus.is_some_and(|f| f as usize != id);
        let lit = s.motion.lit.get(id).copied().unwrap_or(1.0);
        let fill = if line.kind == LineKind::Main { pal.ink } else { pal.lines[line.color] };
        let text_color = if line.kind == LineKind::Main { pal.sheet } else { pal.on_line };
        let layout = ui.fonts.layout(&line.name, Face::Semi, 13.5, None);
        let (tw, th) = (layout.width() as f64, layout.height() as f64);
        let size = (tw + 18.0, th + 8.0);
        let width = line_width(line.kind, view.zy);
        let x = (b + width + 10.0).min(free.x1 - size.0).max(free.x0);
        let slide = (1.0 - ease_out(appear)) as f64 * -10.0;
        let rect = Rect::from_origin_size((x + slide, y - size.1 / 2.0), size);
        if claim(rect.inset(2.0)) {
            let alpha = appear * if dim { lerp(0.55, 1.0, lit) } else { 1.0 };
            plate(ui, rect, fill, alpha);
            text::draw(ui.scene, &layout, Affine::translate((rect.x0 + 9.0, rect.y0 + 4.0)), fade(text_color, alpha));
        }
    }

    // Имена слитых веток над линией, если умещаются на её видимый участок.
    if view.zy > 0.5 {
        for (id, line) in map.lines.iter().enumerate() {
            if !line.visible || line.kind != LineKind::Merged || line.name.is_empty() {
                continue;
            }
            let y = view.sy(map.lane_y(line.lane) as f64);
            if y < TOP_BAND + 20.0 || y > h {
                continue;
            }
            let (x0, x1) = line_ends(map, line);
            let (a, b) = (view.sx(x0).max(free.x0), view.sx(x1).min(free.x1));
            let appear = clamp01((grown(s, id, t).0 - 0.7) / 0.3);
            if b - a < 40.0 || appear <= 0.0 {
                continue;
            }
            let layout = ui.fonts.layout(&line.name, Face::Body, 12.5, None);
            let (tw, th) = (layout.width() as f64, layout.height() as f64);
            if tw > b - a {
                continue;
            }
            let at = Point::new(a.max(b - tw), y - th - 5.0);
            if claim(Rect::from_origin_size(at, (tw, th))) {
                let lit = s.motion.lit.get(id).copied().unwrap_or(1.0);
                let color = fade(mix(pal.lines[line.color], pal.ground, 0.7 * (1.0 - lit)), appear);
                text::halo(ui.scene, &layout, Affine::translate(at.to_vec2()), fade(pal.ground, appear), 3.0);
                text::draw(ui.scene, &layout, Affine::translate(at.to_vec2()), color);
            }
        }
    }

    // Теги и указатели на станциях: под 45°, как названия станций на схеме.
    if view.zoom > 0.3 {
        let mut last: Vec<(i32, f64)> = Vec::new();
        let left = view.wx(free.x0) as f32;
        let from = s.label_order.partition_point(|&c| map.x[c as usize] < left);
        let appear = clamp01((t - SWEEP) / 0.35);
        for &commit in &s.label_order[from..] {
            if !map.is_visible(commit) {
                continue;
            }
            let p = view.at(map, commit);
            if p.x > free.x1 {
                break;
            }
            if p.y < TOP_BAND + 60.0 || p.y > h {
                continue;
            }
            let lane = map.lines[map.line_of[commit as usize] as usize].lane;
            if last.iter().any(|&(l, x)| l == lane && (x - p.x).abs() < 18.0) {
                continue;
            }
            last.push((lane, p.x));
            let layout = ui.fonts.layout(&s.labels[&commit], Face::Body, 12.5, None);
            let place = Affine::translate((p.x + 5.0, p.y - 12.0)) * Affine::rotate(-FRAC_PI_4);
            text::halo(ui.scene, &layout, place, fade(pal.ground, appear), 3.0);
            text::draw(ui.scene, &layout, place, fade(pal.ink, appear));
        }
    }

    if let Some(head) = s.graph.head.filter(|&h| map.is_visible(h)) {
        let p = view.at(map, head);
        let appear = clamp01((t - SWEEP - 0.2) / 0.3);
        if free.contains(p) && appear > 0.0 {
            let layout = ui.fonts.layout("вы здесь", Face::Body, 12.5, None);
            let at = p + Vec2::new(4.0 - layout.width() as f64, 13.0);
            if claim(Rect::from_origin_size(at, (layout.width() as f64, layout.height() as f64))) {
                text::halo(ui.scene, &layout, Affine::translate(at.to_vec2()), fade(pal.ground, appear), 3.0);
                text::draw(ui.scene, &layout, Affine::translate(at.to_vec2()), fade(pal.ink, appear));
            }
        }
    }
}

/// Отросток незакоммиченной работы: ещё не история, поэтому пунктир и пустое кольцо.
fn paint_work(ui: &mut Ui, s: &Session, view: &View, t: f32) {
    let Some((p, q)) = ghost(s, view) else { return };
    let appear = clamp01((t - SWEEP - 0.3) / 0.4);
    if appear <= 0.0 || !view.free.contains(q) {
        return;
    }
    let pal = ui.pal;
    let flow = Stroke::new(2.5).with_caps(Cap::Round).with_dashes(-(ui.input.time * 18.0), [1.0, 7.0]);
    ui.scene.stroke(&flow, Affine::IDENTITY, fade(pal.ink, appear), None, &Line::new(p, q));
    if !ui.calm {
        ui.ambient = true;
    }
    let selected = s.selection == Selection::Work;
    let ring = Circle::new(q, 7.5 * ease_back(appear) as f64);
    ui.fill(&ring, if selected { pal.ink } else { pal.sheet });
    let dashes = Stroke::new(2.0).with_dashes(0.0, [3.0, 3.0]);
    ui.scene.stroke(&dashes, Affine::IDENTITY, fade(pal.ink, appear), None, &ring);
    let label = fmt::count(s.work.len(), "файл не в коммите", "файла не в коммите", "файлов не в коммите");
    let layout = ui.fonts.layout(&label, Face::Body, 12.5, None);
    let fits = q.x + 13.0 + layout.width() as f64 + 8.0 < view.free.x1;
    let at = if fits {
        q + Vec2::new(13.0, -layout.height() as f64 / 2.0)
    } else {
        q + Vec2::new(8.0 - layout.width() as f64, 12.0)
    };
    text::halo(ui.scene, &layout, Affine::translate(at.to_vec2()), fade(pal.ground, appear), 3.0);
    text::draw(ui.scene, &layout, Affine::translate(at.to_vec2()), fade(pal.ink, appear));
}

/// Выделение, найденное, положение HEAD и волна от щелчка.
fn paint_marks(ui: &mut Ui, s: &Session, view: &View, hit: &Option<Hit>, t: f32) {
    let map = &s.map;
    let pal = ui.pal;
    let now = ui.input.time;
    let head_line = s.graph.head.map(|h| map.line_of[h as usize] as usize);
    let head_ready = head_line.is_some_and(|l| l == 0 && t > SWEEP * 0.8 || l != 0 && grown(s, l, t).0 > 0.9);
    if let Some(head) = s.graph.head.filter(|&h| map.is_visible(h) && head_ready) {
        let p = view.at(map, head);
        // Как отметка «вы здесь» на картах: от неё расходится сигнал.
        if !ui.calm && view.free.contains(p) {
            let phase = (now / 2.6).fract() as f32;
            let ring = Circle::new(p, 9.0 + 22.0 * ease_out(phase) as f64);
            ui.stroke(&ring, 2.0, fade(pal.ink, 0.5 * (1.0 - phase)));
            ui.ambient = true;
        }
        ui.fill(&Circle::new(p, 8.5), pal.sheet);
        ui.stroke(&Circle::new(p, 8.5), 2.5, pal.ink);
        ui.fill(&Circle::new(p, 4.0), pal.ink);
    }
    for &commit in s.search_hits.iter().filter(|&&c| map.is_visible(c)).take(4000) {
        let p = view.at(map, commit);
        if view.free.contains(p) {
            ui.fill(&Circle::new(p, 6.5), pal.ink);
            ui.stroke(&Circle::new(p, 6.5), 2.0, pal.sheet);
        }
    }
    for &(commit, start) in &s.motion.ripples {
        let phase = ((now - start) / 0.7) as f32;
        if phase < 0.0 {
            continue;
        }
        let ring = Circle::new(view.at(map, commit), 10.0 + 38.0 * ease_out(phase) as f64);
        ui.stroke(&ring, 2.5, fade(pal.ink, 0.65 * (1.0 - phase)));
    }
    if let Selection::Commit(commit) = s.selection {
        let p = view.at(map, commit);
        ui.fill(&Circle::new(p, 10.0), pal.sheet);
        ui.stroke(&Circle::new(p, 10.0), 3.0, pal.ink);
        ui.fill(&Circle::new(p, 4.0), pal.ink);
    }
    if let Some(Hit::Commit(commit)) = hit {
        let p = view.at(map, *commit);
        let grow = ui.tween(("hover-ring", *commit), true, anim::BOUNCE);
        let radius = 6.0 + 2.5 * grow as f64;
        ui.fill(&Circle::new(p, radius), pal.sheet);
        ui.stroke(&Circle::new(p, radius), 2.5, pal.ink);
    }
}

fn paint_ruler(ui: &mut Ui, s: &Session, view: &View) {
    let pal = ui.pal;
    let (left, right) = (view.free.x0 + 16.0, view.free.x1 - 150.0);
    let y = 22.0;
    let (from, to) = (view.wx(view.free.x0) as f32, view.wx(view.free.x1) as f32);
    let cursor = view.wx(left) as f32;
    let at = s.map.ticks.partition_point(|t| t.x <= cursor);
    let current = s.map.ticks.get(at.saturating_sub(1)).map(|t| fmt::month_year(t.time)).unwrap_or_default();
    let sticky = ui.fonts.width(&current, Face::Semi, 13.0) as f64;
    ui.line(&current, Face::Semi, 13.0, left, y, sticky + 4.0, pal.ink);
    let mut used: Vec<(f64, f64)> = vec![(view.free.x0, left + sticky + 12.0)];
    for level in [TickLevel::Year, TickLevel::Month, TickLevel::Day] {
        let mut previous = f64::MIN;
        for tick in s.map.ticks.iter().filter(|t| t.x >= from && t.x <= to && t.level >= level) {
            let x = view.sx(tick.x as f64);
            let gap = x - previous;
            previous = x;
            if tick.level != level {
                continue;
            }
            let label = match level {
                TickLevel::Year => fmt::year(tick.time),
                TickLevel::Month => fmt::month(tick.time),
                TickLevel::Day => fmt::day_of_month(tick.time),
            };
            let face = if level == TickLevel::Year { Face::Semi } else { Face::Body };
            let width = ui.fonts.width(&label, face, 12.5) as f64;
            let span = (x - 2.0, x + width + 12.0);
            if span.1 > right || used.iter().any(|u| span.1 >= u.0 && span.0 <= u.1) {
                continue;
            }
            // Подпись проявляется по мере того, как ей становится просторно.
            let room = smoothstep(width as f32 * 0.7, width as f32 + 16.0, gap as f32);
            let alpha = if level == TickLevel::Year { 1.0 } else { room };
            if alpha < 0.02 {
                continue;
            }
            used.push(span);
            let color = if level == TickLevel::Year { pal.ink } else { pal.ink_weak };
            ui.line(&label, face, 12.5, x + 4.0, y, width + 2.0, fade(color, alpha));
            ui.hline_at(x, y - 7.0, y + 7.0, fade(pal.rule_strong, alpha));
        }
    }
}

/// Заглавие выезжает из-под маски снизу вверх: как надпись на табло.
fn paint_title(ui: &mut Ui, s: &Session, view: &View, t: f32) {
    let pal = ui.pal;
    let at = Point::new(view.free.x0 + 18.0, TOP_BAND - 4.0);
    let title = ui.fonts.layout(&s.title, Face::Display, 60.0, None);
    let (tw, th) = (title.width() as f64, title.height() as f64);
    if tw > view.free.width() * 0.8 {
        return;
    }
    let shift = (1.0 - ease_out(t / 0.5)) as f64 * th;
    ui.clip(Rect::new(at.x - 8.0, at.y, at.x + tw + 12.0, at.y + th + 2.0), 0.0);
    let place = Affine::translate((at.x, at.y + shift));
    text::halo(ui.scene, &title, place, pal.ground, 6.0);
    text::draw(ui.scene, &title, place, pal.ink);
    ui.unclip();
    let appear = clamp01((t - 0.18) / 0.35);
    let caption = ui.fonts.layout(&s.caption, Face::Body, 14.0, None);
    let place = Affine::translate((at.x + 1.0, at.y + th + 2.0 + (1.0 - ease_out(appear)) as f64 * 8.0));
    text::halo(ui.scene, &caption, place, fade(pal.ground, appear), 4.0);
    text::draw(ui.scene, &caption, place, fade(pal.ink_weak, appear));
}

fn paint_tooltip(ui: &mut Ui, s: &mut Session, view: &View, hit: Option<Hit>, pointer: Option<Point>) {
    let dt = ui.input.dt;
    let rate = 1.0 - (-dt * 20.0).exp();
    let motion = &mut s.motion;
    if hit.is_some() {
        motion.tooltip = hit;
    }
    let goal = if hit.is_some() { 1.0 } else { 0.0 };
    motion.tooltip_amount += (goal - motion.tooltip_amount) * rate;
    if (motion.tooltip_amount - goal).abs() > 0.01 {
        ui.animating = true;
    } else {
        motion.tooltip_amount = goal;
    }
    let amount = motion.tooltip_amount;
    let Some(shown) = motion.tooltip.filter(|_| amount > 0.01) else { return };
    let map = &s.map;
    let (title, meta, anchor) = match shown {
        Hit::Commit(commit) => {
            let c = &s.graph.commits[commit as usize];
            let hash = c.id.to_hex_with_len(8).to_string();
            (c.subject.clone(), format!("{hash}  ·  {}  ·  {}", c.author, fmt::day(c.time)), view.at(map, commit))
        }
        Hit::Line(id) => {
            let line = &map.lines[id as usize];
            let name = if line.name.is_empty() { "Ветка без имени".to_owned() } else { line.name.clone() };
            let anchor = pointer.unwrap_or(view.center);
            (name, fmt::count(line.commits.len(), "коммит", "коммита", "коммитов"), anchor)
        }
        Hit::Work => {
            let anchor = ghost(s, view).map_or(view.center, |(_, q)| q);
            let meta = fmt::count(s.work.len(), "файл изменён", "файла изменено", "файлов изменено");
            ("Незакоммиченная работа".to_owned(), meta, anchor)
        }
    };
    let head = ui.fonts.layout(&title, Face::Semi, 14.0, Some(380.0));
    let foot = ui.fonts.layout(&meta, Face::Body, 12.5, Some(380.0));
    let width = head.width().max(foot.width()) as f64 + 24.0;
    let height = head.height() as f64 + foot.height() as f64 + 22.0;
    let (w, _) = ui.size;
    let mut x = anchor.x - width / 2.0;
    x = x.clamp(view.free.x0 + 8.0, (view.free.x1 - width - 8.0).min(w));
    let above = anchor.y - height - 22.0 > TOP_BAND;
    let y = if above { anchor.y - height - 20.0 } else { anchor.y + 22.0 };
    let rise = (1.0 - amount) as f64 * if above { 8.0 } else { -8.0 };
    let scale = 0.94 + 0.06 * amount as f64;
    let rect = Rect::from_origin_size((x, y + rise), (width, height));
    let pivot = Point::new(anchor.x.clamp(rect.x0, rect.x1), if above { rect.y1 } else { rect.y0 });
    let place = Affine::translate(pivot.to_vec2()) * Affine::scale(scale) * Affine::translate(-pivot.to_vec2());
    let pal = ui.pal;
    let body = place.transform_rect_bbox(rect);
    ui.sheet(body, 10.0, amount);
    // Стрелка от листа к станции.
    let tip = if above { body.y1 } else { body.y0 };
    let dir = if above { 1.0 } else { -1.0 };
    let cx = anchor.x.clamp(body.x0 + 14.0, body.x1 - 14.0);
    let mut arrow = BezPath::new();
    arrow.move_to((cx - 7.0, tip));
    arrow.line_to((cx, tip + 7.0 * dir));
    arrow.line_to((cx + 7.0, tip));
    arrow.close_path();
    ui.fill(&arrow, fade(pal.sheet, amount));
    let inner = place * Affine::translate((rect.x0 + 12.0, rect.y0 + 9.0));
    text::draw(ui.scene, &head, inner, fade(pal.ink, amount));
    let lower = inner * Affine::translate((0.0, head.height() as f64 + 4.0));
    text::draw(ui.scene, &foot, lower, fade(pal.ink_weak, amount));
}

/// Лента внизу: сколько работы шло в каждый момент истории. Это и обзор, и навигация.
fn paint_timeline(ui: &mut Ui, s: &mut Session, free: Rect, view: &View, t: f32) {
    let rect = timeline_rect(free);
    let pal = ui.pal;
    ui.sheet(rect, 12.0, 1.0);
    let inner = rect.inset(-12.0);
    let width = (s.map.width as f64).max(1.0);
    let scale = inner.width() / width;
    let bars = &s.activity;
    let count = bars.len().max(1);
    let step = inner.width() / count as f64;
    let peak = bars.iter().copied().max().unwrap_or(1).max(1) as f32;
    let mut path = BezPath::new();
    for (i, &value) in bars.iter().enumerate() {
        if value == 0 {
            continue;
        }
        let grow = ease_back(clamp01((t - 0.15 - 0.6 * i as f32 / count as f32) / 0.35));
        let height = inner.height() * ((value as f32 / peak).sqrt() * grow) as f64;
        let x = inner.x0 + (i as f64 + 0.5) * step;
        path.move_to((x, inner.y1));
        path.line_to((x, inner.y1 - height.max(1.5)));
    }
    let bar = Stroke::new((step * 0.62).clamp(1.0, 5.0)).with_caps(Cap::Round);
    ui.scene.stroke(&bar, Affine::IDENTITY, fade(pal.ink, 0.62), None, &path);

    let a = (inner.x0 + view.wx(view.free.x0).max(0.0) * scale).min(inner.x1 - 8.0);
    let b = (inner.x0 + view.wx(view.free.x1).min(width) * scale).max(a + 8.0);
    let window = Rect::new(a - 3.0, rect.y0 + 5.0, b + 3.0, rect.y1 - 5.0);
    ui.fill(&RoundedRect::from_rect(window, 8.0), fade(pal.ink, 0.1));
    ui.stroke(&RoundedRect::from_rect(window, 8.0), 2.0, pal.ink);

    let dragging = ui.drag(id("timeline"), rect);
    if ui.pointer_in(rect) {
        ui.cursor = Cursor::Pointer;
    }
    if dragging && let Some(p) = ui.input.pointer {
        let camera = &mut s.motion.camera;
        camera.x.target = (((p.x - inner.x0) / scale).clamp(0.0, width)) as f32;
        s.motion.auto_frame = false;
    }
    ui.covers.push(rect);
}

pub fn activity(map: &Map) -> Vec<u32> {
    const BARS: usize = 150;
    let width = (map.width as f64).max(1.0);
    let mut bars = vec![0u32; BARS];
    for (commit, &x) in map.x.iter().enumerate() {
        if map.is_visible(commit as Idx) {
            let at = ((x as f64 / width) * (BARS - 1) as f64) as usize;
            bars[at.min(BARS - 1)] += 1;
        }
    }
    bars
}

/// Схема-образец для заставок: линия уже лежит по центру, сразу видно, чем это станет.
pub fn demo(ui: &mut Ui, area: Rect, time: f64) {
    let pal = ui.pal;
    let cy = area.center().y + 30.0;
    let (left, right) = (area.x0 + 48.0, area.x1 - 48.0);
    let px = |t: f64| left + (right - left) * t;
    let t = time as f32;
    let branches: [(f64, f64, f64, usize); 3] = [(0.16, 0.44, -52.0, 1), (0.30, 0.58, 52.0, 0), (0.56, 0.9, -104.0, 2)];
    let main = ease_out(t / 1.0);
    for (i, (from, to, rise, color)) in branches.iter().copied().enumerate() {
        let grow = ease_out((t - 0.35 - i as f32 * 0.16) / 0.8);
        if grow <= 0.0 {
            continue;
        }
        let y = cy + rise;
        let turn = px(to) - rise.abs();
        let mut points = route(Point::new(px(from), cy), Point::new(turn, y), true, 12.0);
        points.extend(route(Point::new(turn, y), Point::new(px(to), cy), false, 12.0));
        let trimmed = trim(&points, grow);
        draw_line(ui, &trimmed, 5.0, pal.lines[color], true);
        if !ui.calm && grow >= 1.0 {
            let total = length(&points);
            let phase = (time * 60.0 / total + i as f64 * 0.37).rem_euclid(1.0);
            let (at, angle) = along(&points, phase * total);
            let place = Affine::translate(at.to_vec2()) * Affine::rotate(angle);
            let body = RoundedRect::new(-15.0, -5.0, 15.0, 5.0, 5.0);
            ui.scene.fill(Fill::NonZero, place, pal.sheet, None, &body);
            ui.scene.stroke(&Stroke::new(2.2), place, pal.lines[color], None, &body);
            ui.ambient = true;
        }
    }
    let end = left + (right - left) * main as f64;
    draw_line(ui, &[Point::new(left, cy), Point::new(end, cy)], 8.0, pal.ink, true);
    for (i, (from, to, ..)) in branches.iter().copied().enumerate() {
        for x in [from, to] {
            let pop = ease_back((t - 0.2 - i as f32 * 0.12) / 0.4) as f64;
            if pop > 0.0 {
                ui.fill(&Circle::new((px(x), cy), 7.0 * pop), pal.sheet);
                ui.stroke(&Circle::new((px(x), cy), 7.0 * pop), 3.0, pal.ink);
            }
        }
    }

}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exact_45_degree_routes_stay_finite() {
        // Ровно 45°: угол совпадает с концом пути, и скругление не должно давать NaN.
        for fork in [true, false] {
            let points = route(Point::new(0.0, 0.0), Point::new(50.0, 50.0), fork, 12.0);
            assert!(points.iter().all(|p| p.x.is_finite() && p.y.is_finite()));
            let (at, angle) = along(&points, 30.0);
            assert!(at.x.is_finite() && at.y.is_finite() && angle.is_finite());
        }
    }

    #[test]
    fn trim_keeps_the_start_of_the_path() {
        let points = [Point::new(0.0, 0.0), Point::new(10.0, 0.0), Point::new(10.0, 10.0)];
        let half = trim(&points, 0.5);
        assert_eq!(half.last().copied(), Some(Point::new(10.0, 0.0)));
        assert_eq!(trim(&points, 1.0).len(), 3);
    }

    #[test]
    fn zero_length_steps_do_not_break_walking() {
        let points = [Point::new(5.0, 5.0), Point::new(5.0, 5.0), Point::new(15.0, 5.0)];
        let (at, angle) = along(&points, 4.0);
        assert!(at.x.is_finite() && angle.is_finite());
    }
}
