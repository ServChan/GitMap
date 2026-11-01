//! Просмотр изменений: большой лист поверх схемы. Слева файлы, справа строки.
//! Показывается только видимая часть: файлы бывают на десятки тысяч строк.

use crate::anim::{self, clamp01, ease_out};
use crate::app::Session;
use crate::fmt;
use crate::git::{FileDiff, FileKind, LineKind};
use crate::text::{self, Face};
use crate::theme::{fade, mix};
use crate::ui::{Key, Tone, Ui, id};
use gix::ObjectId;
use vello::kurbo::{Affine, Point, Rect, RoundedRect, Vec2};

const FILES_WIDTH: f64 = 270.0;
const ROW: f64 = 20.0;
const FILE_ROW: f64 = 34.0;

#[derive(Clone)]
pub enum Request {
    /// `from: None` значит сравнение с первым родителем.
    Commits { from: Option<ObjectId>, to: ObjectId },
    /// От общего предка с основной веткой: только то, что сделано в самой ветке.
    Branch { main: ObjectId, tip: ObjectId },
    Worktree,
}

pub struct DiffView {
    pub title: String,
    pub files: Option<Result<Vec<FileDiff>, String>>,
    pub selected: usize,
    pub opened: f64,
    pub closing: Option<f64>,
    /// Какой файл открыть, как только сравнение посчитается.
    pub prefer: Option<String>,
}

pub fn sheet_rect(free: Rect) -> Rect {
    Rect::new(free.x0 + 14.0, 44.0, free.x1 - 14.0, free.y1 - 14.0)
}

fn badge(kind: FileKind) -> &'static str {
    match kind {
        FileKind::Added => "A",
        FileKind::Modified => "M",
        FileKind::Deleted => "D",
        FileKind::Renamed => "R",
    }
}

pub fn show(ui: &mut Ui, s: &mut Session, free: Rect) {
    let Some(view) = &mut s.diff else { return };
    let now = ui.now();
    if ui.input.key(Key::Escape) && view.closing.is_none() {
        view.closing = Some(now);
    }
    let opening = ease_out(((now - view.opened) / 0.32) as f32);
    let closing = view.closing.map_or(0.0, |start| clamp01(((now - start) / 0.18) as f32));
    if closing >= 1.0 {
        s.diff = None;
        return;
    }
    if opening < 1.0 || closing > 0.0 {
        ui.animating = true;
    }
    let phase = opening * (1.0 - closing);
    let target = sheet_rect(free);
    let rect = target + Vec2::new(0.0, (1.0 - phase) as f64 * 26.0);
    let pal = ui.pal;
    ui.sheet(rect, 16.0, phase);
    ui.clip(rect, 16.0);
    ui.alpha(phase, rect);

    let title_y = rect.y0 + 26.0;
    let title = view.title.clone();
    ui.line(&title, Face::Semi, 17.0, rect.x0 + 20.0, title_y, rect.width() - 150.0, pal.ink);
    if ui.button("diff-close", Rect::new(rect.x1 - 110.0, rect.y0 + 10.0, rect.x1 - 14.0, rect.y0 + 42.0), "Закрыть", Tone::Plain) {
        view.closing = Some(now);
    }
    ui.hline(rect.x0, rect.x1, rect.y0 + 52.0, 1.0, pal.rule);
    let body = Rect::new(rect.x0, rect.y0 + 53.0, rect.x1, rect.y1);

    match &view.files {
        None => {
            ui.line("Считаю изменения", Face::Body, 15.0, body.x0 + 20.0, body.y0 + 30.0, 300.0, pal.ink_weak);
        }
        Some(Err(error)) => {
            let error = format!("Не удалось посчитать изменения: {error}");
            ui.paragraph(&error, Face::Body, 14.5, Point::new(body.x0 + 20.0, body.y0 + 20.0), body.width() - 40.0, pal.danger);
        }
        Some(Ok(files)) if files.is_empty() => {
            ui.line("Изменений нет: содержимое совпадает.", Face::Body, 15.0, body.x0 + 20.0, body.y0 + 30.0, 400.0, pal.ink_weak);
        }
        Some(Ok(_)) => {
            // Список на время рисования забираем: выбор файла хранится рядом с ним.
            let taken = view.files.take();
            if let Some(Ok(files)) = &taken {
                file_list(ui, view, files, Rect::new(body.x0, body.y0, body.x0 + FILES_WIDTH, body.y1));
                let selected = view.selected.min(files.len() - 1);
                file_body(ui, &files[selected], selected, Rect::new(body.x0 + FILES_WIDTH, body.y0, body.x1, body.y1));
            }
            view.files = taken;
        }
    }
    ui.end_alpha();
    ui.unclip();
}

fn file_list(ui: &mut Ui, view: &mut DiffView, files: &[FileDiff], area: Rect) {
    let pal = ui.pal;
    let mut selected = view.selected.min(files.len() - 1);
    if ui.state.focus.is_none() {
        if ui.input.key(Key::Down) {
            selected = (selected + 1).min(files.len() - 1);
        }
        if ui.input.key(Key::Up) {
            selected = selected.saturating_sub(1);
        }
    }
    ui.hline_at(area.x1, area.y0, area.y1, pal.rule);
    let total = files.len() as f64 * FILE_ROW + 12.0;
    if selected != view.selected {
        ui.scroll_to("diff-files", area.height(), selected as f64 * FILE_ROW, (selected + 1) as f64 * FILE_ROW + 12.0);
    }
    ui.scroll("diff-files", area, |ui, offset| {
        ui.scroll_content("diff-files", total);
        let block = ui.spring(id("diff-file-block"), selected as f32, selected as f32, anim::SOFT) as f64;
        let top = area.y0 + 6.0 + block * FILE_ROW - offset;
        let bar = Rect::new(area.x0 + 8.0, top + 1.0, area.x1 - 8.0, top + FILE_ROW - 1.0);
        ui.fill(&RoundedRect::from_rect(bar, 8.0), pal.ink);
        let first = (offset / FILE_ROW) as usize;
        let last = (first + (area.height() / FILE_ROW) as usize + 2).min(files.len());
        for (i, file) in files.iter().enumerate().take(last).skip(first) {
            let top = area.y0 + 6.0 + i as f64 * FILE_ROW - offset;
            let row = Rect::new(area.x0 + 8.0, top, area.x1 - 8.0, top + FILE_ROW);
            let response = ui.interact(id(("diff-file", i, &file.path)), row);
            if response.clicked {
                selected = i;
            }
            let lit = ui.tween(("diff-file-lit", i), i == selected, anim::SNAP);
            let (ink, weak) = (mix(pal.ink, pal.sheet, lit), mix(pal.ink_weak, pal.sheet, lit));
            let y = row.center().y;
            ui.line(badge(file.kind), Face::Mono, 12.5, row.x0 + 10.0, y, 14.0, weak);
            let name = file.path.rsplit('/').next().unwrap_or(&file.path);
            let note = if file.binary { "двоичный".to_owned() } else { format!("+{} -{}", file.added, file.removed) };
            let width = ui.line_right(&note, Face::Mono, 11.5, row.x1 - 8.0, y, weak);
            ui.line(name, Face::Body, 13.5, row.x0 + 28.0, y, row.width() - 44.0 - width, ink);
            if response.focused {
                ui.focus_ring(row, 8.0);
            }
        }
    });
    if selected != view.selected {
        view.selected = selected;
    }
}

fn file_body(ui: &mut Ui, file: &FileDiff, selected: usize, area: Rect) {
    let pal = ui.pal;
    let header = Rect::new(area.x0, area.y0, area.x1, area.y0 + 38.0);
    let path = match &file.old_path {
        Some(old) => format!("{old}  →  {}", file.path),
        None => file.path.clone(),
    };
    ui.line(&path, Face::Body, 13.5, header.x0 + 18.0, header.center().y, header.width() - 180.0, pal.ink);
    let stat = format!("+{}  -{}", fmt::number(file.added), fmt::number(file.removed));
    ui.line_right(&stat, Face::Mono, 12.5, header.x1 - 18.0, header.center().y, pal.ink_weak);
    ui.hline(area.x0, area.x1, header.y1, 1.0, pal.rule);
    let body = Rect::new(area.x0, header.y1 + 1.0, area.x1, area.y1 - 6.0);
    if file.binary || file.too_large {
        let note = if file.binary { "Двоичный файл, строк в нём нет." } else { "Файл слишком большой: строки не показаны." };
        ui.line(note, Face::Body, 14.5, body.x0 + 18.0, body.y0 + 26.0, body.width() - 36.0, pal.ink_weak);
        return;
    }
    let (added, removed) = (pal.lines[2], pal.lines[0]);
    let key = ("diff-lines", selected, file.path.as_str());
    ui.scroll(key, body, |ui, offset| {
        ui.scroll_content(key, file.lines.len() as f64 * ROW + 12.0);
        let first = (offset / ROW) as usize;
        let last = (first + (body.height() / ROW) as usize + 2).min(file.lines.len());
        for (i, line) in file.lines.iter().enumerate().take(last).skip(first) {
            let top = body.y0 + 6.0 + i as f64 * ROW - offset;
            let row = Rect::new(body.x0, top, body.x1, top + ROW);
            let (band, mark) = match line.kind {
                LineKind::Added => (Some(fade(added, 0.14)), "+"),
                LineKind::Removed => (Some(fade(removed, 0.14)), "-"),
                LineKind::Hunk => (Some(fade(pal.ink, 0.06)), ""),
                LineKind::Context => (None, " "),
            };
            if let Some(band) = band {
                ui.fill(&row, band);
            }
            let y = row.center().y;
            if line.kind == LineKind::Hunk {
                ui.line(&line.text, Face::Mono, 12.0, body.x0 + 18.0, y, body.width() - 36.0, pal.ink_weak);
                continue;
            }
            let number = |n: Option<u32>| n.map_or_else(String::new, |n| n.to_string());
            ui.line_right(&number(line.old), Face::Mono, 11.5, body.x0 + 48.0, y, pal.ink_weak);
            ui.line_right(&number(line.new), Face::Mono, 11.5, body.x0 + 94.0, y, pal.ink_weak);
            let color = match line.kind {
                LineKind::Added => added,
                LineKind::Removed => removed,
                _ => pal.ink_weak,
            };
            ui.line(mark, Face::Mono, 13.0, body.x0 + 104.0, y, 12.0, color);
            // Табуляция в моноширинном тексте иначе съела бы отступы.
            let shown = line.text.replace('\t', "    ");
            let layout = ui.fonts.layout(&shown, Face::Mono, 13.0, None);
            let at = Affine::translate((body.x0 + 118.0, y - layout.height() as f64 / 2.0));
            text::draw(ui.scene, &layout, at, pal.ink);
        }
    });
}
