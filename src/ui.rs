//! Свой набор элементов поверх Vello. Каждый кадр строится заново, а всё, что должно
//! двигаться плавно, живёт в пружинах по идентификатору элемента.

use crate::anim::{self, Spring, Tune};
use crate::text::{self, Face, Fonts};
use crate::theme::{Palette, fade, mix};
use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use vello::Scene;
use vello::kurbo::{Affine, BezPath, Cap, Circle, Line, Point, Rect, RoundedRect, Shape, Stroke};
use vello::peniko::{Color, Fill, Mix};

pub type Id = u64;

pub fn id(value: impl Hash) -> Id {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    value.hash(&mut hasher);
    hasher.finish()
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Key {
    Left,
    Right,
    Up,
    Down,
    Home,
    End,
    Enter,
    Space,
    Escape,
    Tab,
    Backspace,
    Delete,
    Plus,
    Minus,
    F5,
    Find,
    SelectAll,
    Copy,
    Cut,
    Paste,
}

#[derive(Default)]
pub struct Input {
    pub pointer: Option<Point>,
    pub down: bool,
    pub pressed: bool,
    pub released: bool,
    pub double_click: bool,
    pub wheel: f64,
    pub wheel_x: f64,
    pub shift: bool,
    pub keys: Vec<Key>,
    pub text: String,
    pub time: f64,
    pub dt: f32,
}

impl Input {
    pub fn key(&self, key: Key) -> bool {
        self.keys.contains(&key)
    }

    /// Одноразовые события кадра убираются, текущее состояние остаётся.
    pub fn end_frame(&mut self) {
        self.pressed = false;
        self.released = false;
        self.double_click = false;
        self.wheel = 0.0;
        self.wheel_x = 0.0;
        self.keys.clear();
        self.text.clear();
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Default)]
pub enum Cursor {
    #[default]
    Default,
    Pointer,
    Text,
    Grab,
    Grabbing,
}

#[derive(Default)]
pub struct State {
    springs: HashMap<Id, Spring>,
    active: Option<Id>,
    pub focus: Option<Id>,
    focusable: Vec<Id>,
    /// Позиция каретки ввода и начало выделения, в символах.
    pub caret: usize,
    pub anchor: usize,
    pub caret_moved: f64,
    measured: HashMap<Id, f64>,
}

pub struct Ui<'a> {
    pub scene: &'a mut Scene,
    pub fonts: &'a mut Fonts,
    pub input: &'a Input,
    pub state: &'a mut State,
    pub pal: &'static Palette,
    pub size: (f64, f64),
    pub animating: bool,
    /// Идёт постоянное фоновое движение: кадры нужны, но реже.
    pub ambient: bool,
    pub cursor: Cursor,
    /// Анимации выключены в настройках Windows.
    pub calm: bool,
    clips: Vec<Rect>,
    /// Области поверх: то, что под ними, не получает указатель.
    pub covers: Vec<Rect>,
}

#[derive(Clone, Copy, Default)]
pub struct Response {
    pub hovered: bool,
    pub pressed: bool,
    pub clicked: bool,
    pub focused: bool,
}

#[derive(Clone, Copy, PartialEq)]
pub enum Tone {
    Primary,
    Plain,
    Quiet,
    Danger,
}

impl<'a> Ui<'a> {
    pub fn new(
        scene: &'a mut Scene,
        fonts: &'a mut Fonts,
        input: &'a Input,
        state: &'a mut State,
        pal: &'static Palette,
        size: (f64, f64),
        calm: bool,
    ) -> Self {
        state.focusable.clear();
        Self {
            scene,
            fonts,
            input,
            state,
            pal,
            size,
            animating: false,
            ambient: false,
            cursor: Cursor::Default,
            calm,
            clips: Vec::new(),
            covers: Vec::new(),
        }
    }


    pub fn now(&self) -> f64 {
        self.input.time
    }

    /// Пружина к цели. Первый вызов сразу ставит значение `start`.
    pub fn spring(&mut self, key: Id, start: f32, target: f32, tune: Tune) -> f32 {
        let calm = self.calm;
        let dt = self.input.dt;
        let spring = self.state.springs.entry(key).or_insert_with(|| Spring::new(if calm { target } else { start }));
        spring.target = target;
        if calm {
            spring.set(target);
        } else if !spring.settled() || spring.value != target {
            spring.step(dt, tune);
            self.animating = true;
        }
        spring.value
    }

    pub fn tween(&mut self, key: impl Hash, target: bool, tune: Tune) -> f32 {
        let target = if target { 1.0 } else { 0.0 };
        self.spring(id(key), target, target, tune)
    }

    pub fn measured(&self, key: Id) -> f64 {
        self.state.measured.get(&key).copied().unwrap_or(0.0)
    }

    pub fn measure(&mut self, key: Id, value: f64) {
        self.state.measured.insert(key, value);
    }

    pub fn pointer_in(&self, rect: Rect) -> bool {
        let Some(p) = self.input.pointer else { return false };
        rect.contains(p) && self.clips.iter().all(|c| c.contains(p))
    }

    pub fn covered(&self, p: Point) -> bool {
        self.covers.iter().any(|r| r.contains(p))
    }

    /// Обработка щелчка, наведения и клавиатурного фокуса для прямоугольника.
    pub fn interact(&mut self, key: Id, rect: Rect) -> Response {
        self.state.focusable.push(key);
        let hovered = self.pointer_in(rect) && self.state.active.is_none_or(|a| a == key);
        let mut response = Response { hovered, ..Response::default() };
        if hovered && self.input.pressed {
            self.state.active = Some(key);
            self.state.focus = None;
        }
        if self.state.active == Some(key) {
            response.pressed = self.input.down && hovered;
            if self.input.released {
                self.state.active = None;
                response.clicked = hovered;
            }
        }
        response.focused = self.state.focus == Some(key);
        if response.focused && (self.input.key(Key::Enter) || self.input.key(Key::Space)) {
            response.clicked = true;
        }
        if hovered {
            self.cursor = Cursor::Pointer;
        }
        response
    }

    /// Захват указателя для перетаскивания: держится до отпускания кнопки.
    pub fn drag(&mut self, key: Id, rect: Rect) -> bool {
        if self.pointer_in(rect) && self.input.pressed && self.state.active.is_none() {
            self.state.active = Some(key);
            self.state.focus = None;
        }
        if self.state.active == Some(key) {
            if !self.input.down {
                self.state.active = None;
                return false;
            }
            return true;
        }
        false
    }

    pub fn any_active(&self) -> bool {
        self.state.active.is_some()
    }

    /// В конце кадра: Tab переводит фокус по элементам в порядке их появления.
    pub fn finish(&mut self) {
        if self.input.key(Key::Tab) && !self.state.focusable.is_empty() {
            let list = &self.state.focusable;
            let at = self.state.focus.and_then(|f| list.iter().position(|i| *i == f));
            let next = match (at, self.input.shift) {
                (Some(i), false) => (i + 1) % list.len(),
                (Some(0), true) | (None, true) => list.len() - 1,
                (Some(i), true) => i - 1,
                (None, false) => 0,
            };
            self.state.focus = Some(list[next]);
        }
    }

    // ---------- Рисование ----------

    pub fn fill(&mut self, shape: &impl Shape, color: Color) {
        self.scene.fill(Fill::NonZero, Affine::IDENTITY, color, None, shape);
    }

    pub fn stroke(&mut self, shape: &impl Shape, width: f64, color: Color) {
        self.scene.stroke(&Stroke::new(width).with_caps(Cap::Round), Affine::IDENTITY, color, None, shape);
    }

    pub fn hline(&mut self, x0: f64, x1: f64, y: f64, width: f64, color: Color) {
        self.scene.stroke(&Stroke::new(width), Affine::IDENTITY, color, None, &Line::new((x0, y), (x1, y)));
    }

    pub fn clip(&mut self, rect: Rect, radius: f64) {
        self.scene.push_clip_layer(Fill::NonZero, Affine::IDENTITY, &RoundedRect::from_rect(rect, radius));
        self.clips.push(rect);
    }

    pub fn unclip(&mut self) {
        self.scene.pop_layer();
        self.clips.pop();
    }

    /// Слой с прозрачностью: всё внутри гаснет как единое целое.
    pub fn alpha(&mut self, alpha: f32, rect: Rect) {
        self.scene.push_layer(Fill::NonZero, Mix::Normal, alpha, Affine::IDENTITY, &rect);
    }

    pub fn end_alpha(&mut self) {
        self.scene.pop_layer();
    }

    /// Лист поверх схемы: мягкая тень в два слоя и тонкая кромка.
    pub fn sheet(&mut self, rect: Rect, radius: f64, alpha: f32) {
        let pal = self.pal;
        let lift = |r: Rect, dy: f64| r + (vello::kurbo::Vec2::new(0.0, dy));
        self.scene.draw_blurred_rounded_rect(Affine::IDENTITY, lift(rect, 10.0), fade(pal.shadow, 0.55 * alpha), radius, 26.0);
        self.scene.draw_blurred_rounded_rect(Affine::IDENTITY, lift(rect, 1.0), fade(pal.shadow, 0.6 * alpha), radius, 3.0);
        let shape = RoundedRect::from_rect(rect, radius);
        self.fill(&shape, fade(pal.sheet, alpha));
        self.stroke(&RoundedRect::from_rect(rect.inset(-0.5), radius), 1.0, fade(pal.rule, alpha));
    }


    /// Строка в заданную ширину, по вертикали по центру `y`.
    #[allow(clippy::too_many_arguments)]
    pub fn line(&mut self, content: &str, face: Face, size: f32, x: f64, y: f64, width: f64, color: Color) {
        let layout = self.fonts.fit(content, face, size, width as f32);
        let top = y - layout.height() as f64 / 2.0;
        text::draw(self.scene, &layout, Affine::translate((x, top)), color);
    }

    pub fn line_right(&mut self, content: &str, face: Face, size: f32, right: f64, y: f64, color: Color) -> f64 {
        let layout = self.fonts.layout(content, face, size, None);
        let width = layout.width() as f64;
        let top = y - layout.height() as f64 / 2.0;
        text::draw(self.scene, &layout, Affine::translate((right - width, top)), color);
        width
    }

    /// Абзац с переносами. Возвращает высоту.
    pub fn paragraph(&mut self, content: &str, face: Face, size: f32, at: Point, width: f64, color: Color) -> f64 {
        let layout = self.fonts.layout(content, face, size, Some(width as f32));
        text::draw(self.scene, &layout, Affine::translate(at.to_vec2()), color);
        layout.height() as f64
    }

    pub fn focus_ring(&mut self, rect: Rect, radius: f64) {
        let color = self.pal.ink;
        self.stroke(&RoundedRect::from_rect(rect.inset(2.0), radius + 2.0), 2.0, color);
    }

    // ---------- Элементы ----------

    pub fn button(&mut self, key: impl Hash, rect: Rect, label: &str, tone: Tone) -> bool {
        let key = id(("button", key));
        let response = self.interact(key, rect);
        let pal = self.pal;
        let hover = self.tween((key, "hover"), response.hovered, anim::SNAP);
        let press = self.tween((key, "press"), response.pressed, anim::SNAP);
        let squeeze = 1.0 - 0.03 * press as f64;
        let center = rect.center();
        let shown = Rect::from_center_size(center, (rect.width() * squeeze, rect.height() * squeeze));
        let shape = RoundedRect::from_rect(shown, 8.0);
        let (fill, text) = match tone {
            Tone::Primary => (mix(pal.ink, pal.ink_weak, 0.25 * hover), pal.sheet),
            Tone::Danger => (mix(pal.danger, pal.ink, 0.18 * hover), pal.on_danger),
            Tone::Plain => (fade(pal.sheet_hover, 0.45 + 0.55 * hover), pal.ink),
            Tone::Quiet => (fade(pal.sheet_hover, hover), pal.ink),
        };
        self.fill(&shape, fill);
        if tone == Tone::Plain {
            self.stroke(&shape, 1.0, mix(pal.rule_strong, pal.ink, hover));
        }
        let face = if matches!(tone, Tone::Primary | Tone::Danger) { Face::Semi } else { Face::Body };
        let width = self.fonts.width(label, face, 14.0) as f64;
        self.line(label, face, 14.0, center.x - width / 2.0, center.y, width + 2.0, text);
        if response.focused {
            self.focus_ring(rect, 8.0);
        }
        response.clicked
    }

    /// Переключатель: кольцо едет по короткому участку линии.
    pub fn toggle(&mut self, key: impl Hash, rect: Rect, label: &str, value: &mut bool) -> bool {
        let key = id(("toggle", key));
        let response = self.interact(key, rect);
        let pal = self.pal;
        if response.clicked {
            *value = !*value;
        }
        let on = self.tween((key, "on"), *value, anim::BOUNCE);
        let hover = self.tween((key, "hover"), response.hovered, anim::SNAP);
        let y = rect.center().y;
        let (x0, x1) = (rect.x0 + 7.0, rect.x0 + 29.0);
        let dot = x0 + (x1 - x0) * on as f64;
        self.scene.stroke(
            &Stroke::new(4.0).with_caps(Cap::Round),
            Affine::IDENTITY,
            pal.rule_strong,
            None,
            &Line::new((x0, y), (x1, y)),
        );
        if on > 0.01 {
            self.scene.stroke(
                &Stroke::new(4.0).with_caps(Cap::Round),
                Affine::IDENTITY,
                pal.ink,
                None,
                &Line::new((x0, y), (dot.max(x0 + 0.1), y)),
            );
        }
        let radius = 6.0 + 1.0 * hover as f64;
        self.fill(&Circle::new((dot, y), radius), pal.sheet);
        self.stroke(&Circle::new((dot, y), radius), 2.5, pal.ink);
        self.line(label, Face::Body, 14.0, rect.x0 + 42.0, y, rect.width() - 42.0, pal.ink);
        if response.focused {
            self.focus_ring(rect, 6.0);
        }
        response.clicked
    }

    /// Выбор из нескольких: подложка едет к выбранному пункту.
    pub fn segments(&mut self, key: impl Hash, rect: Rect, labels: &[&str], selected: &mut usize) -> bool {
        let key = id(("segments", key));
        let pal = self.pal;
        let count = labels.len() as f64;
        let width = rect.width() / count;
        self.fill(&RoundedRect::from_rect(rect, 8.0), fade(pal.sheet_hover, 0.8));
        let target = *selected as f32;
        let slide = self.spring(key, target, target, anim::SOFT) as f64;
        let thumb = Rect::new(rect.x0 + slide * width + 2.0, rect.y0 + 2.0, rect.x0 + (slide + 1.0) * width - 2.0, rect.y1 - 2.0);
        self.fill(&RoundedRect::from_rect(thumb, 6.0), pal.ink);
        let mut changed = false;
        for (i, label) in labels.iter().enumerate() {
            let cell = Rect::new(rect.x0 + i as f64 * width, rect.y0, rect.x0 + (i as f64 + 1.0) * width, rect.y1);
            let response = self.interact(id((key, i)), cell);
            if response.clicked && *selected != i {
                *selected = i;
                changed = true;
            }
            let lit = 1.0 - ((slide - i as f64).abs() as f32).min(1.0);
            let color = mix(pal.ink, pal.sheet, lit);
            let text_width = self.fonts.width(label, Face::Body, 13.0) as f64;
            self.line(label, Face::Body, 13.0, cell.center().x - text_width / 2.0, cell.center().y, text_width + 2.0, color);
            if response.focused {
                self.focus_ring(cell, 6.0);
            }
        }
        changed
    }

    /// Поле ввода. Возвращает true, если текст изменился.
    pub fn input_field(&mut self, key: impl Hash, rect: Rect, value: &mut String, hint: &str) -> bool {
        let key = id(("input", key));
        self.state.focusable.push(key);
        let pal = self.pal;
        let hovered = self.pointer_in(rect);
        let inner_x = rect.x0 + 12.0;
        if hovered {
            self.cursor = Cursor::Text;
            if self.input.pressed {
                self.state.focus = Some(key);
                let at = self.input.pointer.map_or(0.0, |p| p.x - inner_x);
                self.state.caret = self.caret_at(value, at);
                self.state.anchor = self.state.caret;
                self.state.caret_moved = self.now();
                self.state.active = Some(key);
            }
        }
        if self.state.active == Some(key) {
            if self.input.down {
                let at = self.input.pointer.map_or(0.0, |p| p.x - inner_x);
                self.state.caret = self.caret_at(value, at);
            } else {
                self.state.active = None;
            }
        }
        let focused = self.state.focus == Some(key);
        let changed = if focused { self.edit(value) } else { false };
        let glow = self.tween((key, "focus"), focused, anim::SNAP);
        let shape = RoundedRect::from_rect(rect, 9.0);
        self.fill(&shape, fade(pal.sheet_hover, 0.55 + 0.45 * glow));
        self.stroke(&shape, 1.0 + glow as f64, mix(pal.rule_strong, pal.ink, glow));

        let y = rect.center().y;
        self.clip(rect.inset((-10.0, 0.0)), 0.0);
        if value.is_empty() {
            self.line(hint, Face::Body, 14.0, inner_x, y, rect.width() - 24.0, pal.ink_weak);
        } else {
            if focused && self.state.caret != self.state.anchor {
                let (a, b) = (self.state.caret.min(self.state.anchor), self.state.caret.max(self.state.anchor));
                let (a, b) = (self.prefix_width(value, a), self.prefix_width(value, b));
                let selection = Rect::new(inner_x + a, y - 10.0, inner_x + b, y + 10.0);
                self.fill(&RoundedRect::from_rect(selection, 3.0), fade(pal.ink, 0.22));
            }
            let layout = self.fonts.layout(value, Face::Body, 14.0, None);
            let top = y - layout.height() as f64 / 2.0;
            text::draw(self.scene, &layout, Affine::translate((inner_x, top)), pal.ink);
        }
        if focused {
            let since = (self.now() - self.state.caret_moved) as f32;
            let blink = 0.5 + 0.5 * (since * 5.0 + 1.57).sin();
            let x = inner_x + self.prefix_width(value, self.state.caret);
            self.hline_vertical(x, y - 9.0, y + 9.0, fade(pal.ink, anim::smoothstep(0.2, 0.5, blink)));
            self.ambient = true;
        }
        self.unclip();
        if focused && self.input.pressed && !hovered {
            self.state.focus = None;
        }
        changed
    }

    pub fn hline_at(&mut self, x: f64, y0: f64, y1: f64, color: Color) {
        self.hline_vertical(x, y0, y1, color);
    }

    fn hline_vertical(&mut self, x: f64, y0: f64, y1: f64, color: Color) {
        self.scene.stroke(&Stroke::new(1.5), Affine::IDENTITY, color, None, &Line::new((x, y0), (x, y1)));
    }

    fn prefix_width(&mut self, value: &str, chars: usize) -> f64 {
        let prefix: String = value.chars().take(chars).collect();
        self.fonts.width(&prefix, Face::Body, 14.0) as f64
    }

    fn caret_at(&mut self, value: &str, x: f64) -> usize {
        let count = value.chars().count();
        (0..=count)
            .min_by(|&a, &b| {
                let distance = |ui: &mut Self, i| (ui.prefix_width(value, i) - x).abs();
                distance(self, a).total_cmp(&distance(self, b))
            })
            .unwrap_or(0)
    }

    fn edit(&mut self, value: &mut String) -> bool {
        let before = value.clone();
        let count = value.chars().count();
        let state = &mut *self.state;
        state.caret = state.caret.min(count);
        state.anchor = state.anchor.min(count);
        let byte = |text: &str, chars: usize| text.char_indices().nth(chars).map_or(text.len(), |(i, _)| i);
        let remove_selection = |value: &mut String, state: &mut State| {
            let (a, b) = (state.caret.min(state.anchor), state.caret.max(state.anchor));
            if a != b {
                let (x, y) = (byte(value, a), byte(value, b));
                value.replace_range(x..y, "");
                state.caret = a;
                state.anchor = a;
                return true;
            }
            false
        };
        let shift = self.input.shift;
        let mut moved = false;
        for key in &self.input.keys {
            moved = true;
            let count = value.chars().count();
            match key {
                Key::Left => state.caret = state.caret.saturating_sub(1),
                Key::Right => state.caret = (state.caret + 1).min(count),
                Key::Home => state.caret = 0,
                Key::End => state.caret = count,
                Key::SelectAll => {
                    state.anchor = 0;
                    state.caret = count;
                    continue;
                }
                Key::Backspace => {
                    if !remove_selection(value, state) && state.caret > 0 {
                        let (x, y) = (byte(value, state.caret - 1), byte(value, state.caret));
                        value.replace_range(x..y, "");
                        state.caret -= 1;
                        state.anchor = state.caret;
                    }
                    continue;
                }
                Key::Delete => {
                    if !remove_selection(value, state) && state.caret < count {
                        let (x, y) = (byte(value, state.caret), byte(value, state.caret + 1));
                        value.replace_range(x..y, "");
                    }
                    continue;
                }
                Key::Copy | Key::Cut => {
                    let (a, b) = (state.caret.min(state.anchor), state.caret.max(state.anchor));
                    if a != b {
                        copy_text(value[byte(value, a)..byte(value, b)].to_owned());
                        if *key == Key::Cut {
                            remove_selection(value, state);
                        }
                    }
                    continue;
                }
                Key::Paste => {
                    if let Some(pasted) = paste_text() {
                        remove_selection(value, state);
                        let pasted: String = pasted.chars().filter(|c| !c.is_control()).collect();
                        value.insert_str(byte(value, state.caret), &pasted);
                        state.caret += pasted.chars().count();
                        state.anchor = state.caret;
                    }
                    continue;
                }
                _ => {
                    moved = false;
                    continue;
                }
            }
            if !shift {
                state.anchor = state.caret;
            }
        }
        let typed: String = self.input.text.chars().filter(|c| !c.is_control()).collect();
        if !typed.is_empty() {
            remove_selection(value, state);
            value.insert_str(byte(value, state.caret), &typed);
            state.caret += typed.chars().count();
            state.anchor = state.caret;
            moved = true;
        }
        if moved {
            state.caret_moved = self.input.time;
        }
        *value != before
    }

    /// Прокручиваемая область: `build` рисует содержимое со сдвигом и возвращает его высоту.
    pub fn scroll(&mut self, key: impl Hash, rect: Rect, build: impl FnOnce(&mut Self, f64)) {
        let key = id(("scroll", key));
        let content = self.measured(key);
        let limit = (content - rect.height()).max(0.0);
        let hovered = self.pointer_in(rect);
        let target_key = id((key, "target"));
        let mut target = self.measured(target_key);
        if hovered && self.input.wheel != 0.0 {
            target -= self.input.wheel;
        }
        target = target.clamp(0.0, limit);
        self.measure(target_key, target);
        let offset = self.spring(key, 0.0, target as f32, anim::SNAP) as f64;
        self.clip(rect, 0.0);
        build(self, offset);
        self.unclip();
        let content = self.measured(id((key, "content")));
        self.measure(key, content);
        if content > rect.height() + 1.0 {
            let pal = self.pal;
            let active = self.tween((key, "bar"), hovered, anim::SNAP);
            let height = (rect.height() * rect.height() / content).max(28.0);
            let y = rect.y0 + (rect.height() - height) * (offset / (content - rect.height()));
            let bar = Rect::new(rect.x1 - 7.0, y + 2.0, rect.x1 - 3.0, y + height - 2.0);
            self.fill(&RoundedRect::from_rect(bar, 2.0), fade(pal.rule_strong, 0.15 + 0.55 * active));
        }
    }

    /// Высота содержимого области `scroll` сообщается отсюда: она известна только в конце.
    pub fn scroll_content(&mut self, key: impl Hash, height: f64) {
        let key = id(("scroll", key));
        self.measure(id((key, "content")), height);
    }

    /// Прокрутить область так, чтобы участок [y0, y1] оказался виден.
    pub fn scroll_to(&mut self, key: impl Hash, view: f64, y0: f64, y1: f64) {
        let key = id(("scroll", key));
        let target_key = id((key, "target"));
        let target = self.measured(target_key);
        if y0 < target {
            self.measure(target_key, y0);
        } else if y1 > target + view {
            self.measure(target_key, y1 - view);
        }
    }

    pub fn path(&mut self, points: &[Point], width: f64, color: Color) {
        let Some(path) = polyline(points) else { return };
        let stroke = Stroke::new(width).with_caps(Cap::Round);
        self.scene.stroke(&stroke, Affine::IDENTITY, color, None, &path);
    }
}

pub fn polyline(points: &[Point]) -> Option<BezPath> {
    let (first, rest) = points.split_first()?;
    let mut path = BezPath::new();
    path.move_to(*first);
    for point in rest {
        path.line_to(*point);
    }
    Some(path)
}

pub fn copy_text(text: String) {
    if let Ok(mut clipboard) = arboard::Clipboard::new() {
        let _ = clipboard.set_text(text);
    }
}

fn paste_text() -> Option<String> {
    arboard::Clipboard::new().ok()?.get_text().ok()
}
