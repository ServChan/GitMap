//! Правый лист: условные обозначения схемы, сведения о ветке и о коммите.
//! Страницы сменяются сдвигом: новая въезжает, сразу видно, что произошло.

use crate::anim::{self, ease_out};
use crate::app::{Action, Selection, Session};
use crate::diff::Request;
use crate::fmt;
use crate::git::{FileKind, Idx};
use crate::layout::{Line, LineKind, Options};
use crate::map;
use crate::text::Face;
use crate::theme::{fade, mix};
use crate::ui::{Tone, Ui, copy_text, id};
use std::collections::HashMap;
use vello::kurbo::{Circle, Line as Segment, Point, Rect, RoundedRect, Stroke};

const PAD: f64 = 18.0;
const ROW: f64 = 32.0;
const MERGED_PREVIEW: usize = 12;

#[derive(Default)]
pub struct Change {
    pub relayout: bool,
    pub labels: bool,
    pub action: Option<Action>,
}

/// Сведения о проекте из обзора папки.
pub struct Info {
    pub web_url: Option<String>,
    pub family: Vec<(String, std::path::PathBuf)>,
}

fn line_title(line: &Line) -> String {
    if line.name.is_empty() { "Ветка без имени".to_owned() } else { line.name.clone() }
}

fn swatch(ui: &mut Ui, c: Point, line: &Line, active: f32) {
    let pal = ui.pal;
    let color = match line.kind {
        LineKind::Main => pal.ink,
        _ => pal.lines[line.color],
    };
    let color = mix(color, pal.sheet, active);
    let width = match line.kind {
        LineKind::Main => 6.0,
        LineKind::Active => 4.0,
        LineKind::Merged => 2.0,
    };
    ui.scene.stroke(&Stroke::new(width), Default::default(), color, None, &Segment::new((c.x - 14.0, c.y), (c.x + 14.0, c.y)));
    let fill = mix(pal.sheet, pal.ink, active);
    let ring = Circle::new(c, width / 2.0 + 2.5);
    ui.fill(&ring, fill);
    ui.stroke(&ring, 1.6, color);
}

/// Заголовок раздела: линейка снизу, число справа.
fn section(ui: &mut Ui, x: f64, y: f64, width: f64, title: &str, note: &str) -> f64 {
    let pal = ui.pal;
    ui.line(title, Face::Semi, 13.5, x, y + 13.0, width - 60.0, pal.ink);
    ui.line_right(note, Face::Body, 12.5, x + width, y + 13.0, pal.ink_weak);
    ui.hline(x, x + width, y + 27.0, 1.0, pal.rule);
    y + 32.0
}

fn pair(ui: &mut Ui, x: f64, y: f64, width: f64, key: &str, value: &str) -> f64 {
    let pal = ui.pal;
    ui.line(key, Face::Body, 12.5, x, y + 9.0, 72.0, pal.ink_weak);
    let height = ui.paragraph(value, Face::Body, 14.0, Point::new(x + 76.0, y), width - 76.0, pal.ink);
    y + height.max(18.0) + 6.0
}

fn line_row(ui: &mut Ui, s: &mut Session, rect: Rect, line_id: usize, total: Option<usize>, free: Rect, peek: &mut Option<u32>) {
    let pal = ui.pal;
    let line = &s.map.lines[line_id];
    let selected = s.selection == Selection::Line(line_id as u32);
    let title = line_title(line);
    let count = total.unwrap_or(line.commits.len());
    let key = id(("line-row", &title, line_id));
    let response = ui.interact(key, rect);
    let hover = ui.tween((key, "hover"), response.hovered, anim::SNAP);
    let lit = ui.tween((key, "lit"), selected, anim::SNAP);
    if response.hovered {
        *peek = Some(line_id as u32);
    }
    let shape = RoundedRect::from_rect(rect.inset((-6.0, -1.0)), 8.0);
    ui.fill(&shape, fade(pal.sheet_hover, hover * (1.0 - lit)));
    ui.fill(&shape, fade(pal.ink, lit));
    let y = rect.center().y;
    let line = &s.map.lines[line_id];
    swatch(ui, Point::new(rect.x0 + PAD + 8.0 + hover as f64 * 2.0, y), line, lit);
    let ink = mix(pal.ink, pal.sheet, lit);
    let weak = mix(pal.ink_weak, pal.sheet, lit);
    let width = rect.width() - PAD * 2.0 - 40.0 - 44.0;
    ui.line(&title, Face::Body, 14.0, rect.x0 + PAD + 34.0, y, width, ink);
    ui.line_right(&fmt::number(count), Face::Body, 12.5, rect.x1 - PAD, y, weak);
    if response.focused {
        ui.focus_ring(rect.inset((-6.0, 0.0)), 8.0);
    }
    if response.clicked {
        let shot = map::focus_line(&s.map, &s.map.lines[line_id], free);
        s.select(Selection::Line(line_id as u32));
        s.motion.camera.aim(shot);
        s.motion.auto_frame = false;
        s.page_changed = ui.now();
    }
}

fn commit_row(ui: &mut Ui, s: &mut Session, rect: Rect, commit: Idx, zoom_to: bool) {
    let pal = ui.pal;
    let c = &s.graph.commits[commit as usize];
    let selected = s.selection == Selection::Commit(commit);
    let subject = if c.subject.is_empty() { "Без описания".to_owned() } else { c.subject.clone() };
    let age = fmt::age(c.time);
    let key = id(("commit-row", commit));
    let response = ui.interact(key, rect);
    let hover = ui.tween((key, "hover"), response.hovered, anim::SNAP);
    let shape = RoundedRect::from_rect(rect.inset((-6.0, -1.0)), 8.0);
    ui.fill(&shape, if selected { pal.ink } else { fade(pal.sheet_hover, hover) });
    let (ink, weak) = if selected { (pal.sheet, pal.sheet) } else { (pal.ink, pal.ink_weak) };
    let y = rect.center().y;
    let age_width = ui.line_right(&age, Face::Body, 12.5, rect.x1 - PAD, y, weak);
    ui.line(&subject, Face::Body, 13.5, rect.x0 + PAD, y, rect.width() - PAD * 2.0 - age_width - 10.0, ink);
    if response.focused {
        ui.focus_ring(rect.inset((-6.0, 0.0)), 8.0);
    }
    if response.clicked {
        if zoom_to {
            let zoom = map::current_zoom(s);
            s.motion.camera.aim(map::focus_commit(&s.map, commit, zoom));
            s.motion.auto_frame = false;
        }
        s.select(Selection::Commit(commit));
        s.motion.ring(commit, ui.now());
        s.page_changed = ui.now();
    }
}

fn action_row(ui: &mut Ui, x: f64, y: f64, width: f64, info: &Info, change: &mut Change) {
    let mut list = vec![("VS Code", Action::Editor), ("Терминал", Action::Terminal), ("Папка", Action::Folder)];
    if let Some(url) = &info.web_url {
        list.push(("Сайт", Action::Web(url.clone())));
    }
    let gap = 6.0;
    let cell = (width - gap * (list.len() as f64 - 1.0)) / list.len() as f64;
    for (i, (label, action)) in list.into_iter().enumerate() {
        let rect = Rect::from_origin_size((x + i as f64 * (cell + gap), y), (cell, 32.0));
        if ui.button(("action", label), rect, label, Tone::Plain) {
            change.action = Some(action);
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn legend(ui: &mut Ui, s: &mut Session, options: &mut Options, show_tags: &mut bool, rect: Rect, free: Rect, change: &mut Change, peek: &mut Option<u32>, info: &Info) {
    let pal = ui.pal;
    let x = rect.x0 + PAD;
    let width = rect.width() - PAD * 2.0;
    let field = Rect::from_origin_size((x, rect.y0 + 14.0), (width, 36.0));
    let search_key = id(("input", "commit-search"));
    if ui.input.key(crate::ui::Key::Find) {
        ui.state.focus = Some(search_key);
    }
    if ui.input_field("commit-search", field, &mut s.search, "Найти коммит: текст, автор, хеш") {
        s.run_search();
    }
    let mut y = rect.y0 + 58.0;

    if s.search.trim().is_empty() {
        action_row(ui, x, y, width, info, change);
        y += 40.0;
        if !s.work.is_empty() {
            let label = fmt::count(s.work.len(), "файл не в коммите", "файла не в коммите", "файлов не в коммите");
            if ui.button("show-work", Rect::from_origin_size((x, y), (width, 32.0)), &label, Tone::Primary) {
                s.select(Selection::Work);
                s.page_changed = ui.now();
            }
            y += 40.0;
        }
    }

    if !s.search.trim().is_empty() {
        let hits = s.search_hits.len();
        y = section(ui, x, y, width, "Найдено", &fmt::number(hits));
        if hits == 0 {
            ui.paragraph("Таких коммитов нет. Проверь написание или ищи по автору.", Face::Body, 14.0, Point::new(x, y + 6.0), width, pal.ink_weak);
            return;
        }
        let area = Rect::new(rect.x0, y, rect.x1, rect.y1 - 6.0);
        let list = s.search_hits.clone();
        ui.scroll("hits", area, |ui, offset| {
            ui.scroll_content("hits", list.len() as f64 * ROW);
            let first = (offset / ROW) as usize;
            let last = (first + (area.height() / ROW) as usize + 2).min(list.len());
            for (i, commit) in list.iter().enumerate().take(last).skip(first) {
                let top = area.y0 + i as f64 * ROW - offset;
                commit_row(ui, s, Rect::new(rect.x0 + 6.0, top, rect.x1 - 6.0, top + ROW), *commit, true);
            }
        });
        return;
    }

    y = section(ui, x, y, width, "Вид", "");
    let mut view_changed = false;
    for (label, value) in [("Слитые ветки", &mut options.show_merged), ("Сжимать прямые участки", &mut options.compress)] {
        view_changed |= ui.toggle(label, Rect::from_origin_size((x, y), (width, 30.0)), label, value);
        y += 30.0;
    }
    change.labels |= ui.toggle("Теги", Rect::from_origin_size((x, y), (width, 30.0)), "Теги", show_tags);
    y += 36.0;
    change.relayout |= view_changed;
    let half = (width - 8.0) / 2.0;
    if ui.button("whole", Rect::from_origin_size((x, y), (half, 32.0)), "Вся схема", Tone::Plain) {
        s.motion.camera.aim(map::whole(&s.map, free));
        s.motion.auto_frame = false;
    }
    if ui.button("latest", Rect::from_origin_size((x + half + 8.0, y), (half, 32.0)), "К концу", Tone::Plain) {
        s.motion.camera.aim(map::latest(&s.map, free));
        s.motion.auto_frame = false;
    }
    y += 38.0;
    if ui.button("export-png", Rect::from_origin_size((x, y), (half, 32.0)), "Сохранить PNG", Tone::Plain) {
        change.action = Some(Action::ExportPng);
    }
    if ui.button("export-svg", Rect::from_origin_size((x + half + 8.0, y), (half, 32.0)), "Сохранить SVG", Tone::Plain) {
        change.action = Some(Action::ExportSvg);
    }
    y += 40.0;

    let lines = &s.map.lines;
    let last_commit = |i: usize| s.graph.commits[lines[i].commits[lines[i].commits.len() - 1] as usize].time;
    let mut active: Vec<usize> = (1..lines.len()).filter(|&i| lines[i].kind == LineKind::Active).collect();
    active.sort_by_key(|&i| std::cmp::Reverse(last_commit(i)));
    let mut merged: Vec<usize> =
        (1..lines.len()).filter(|&i| lines[i].kind == LineKind::Merged && !lines[i].name.is_empty() && lines[i].visible).collect();
    merged.sort_by_key(|&i| std::cmp::Reverse(last_commit(i)));
    // Ветку, которую вливали не раз, показываем одной строкой с общим числом коммитов.
    let mut seen: HashMap<String, usize> = HashMap::new();
    for &i in &merged {
        *seen.entry(lines[i].name.clone()).or_default() += lines[i].commits.len();
    }
    let totals = seen.clone();
    merged.retain(|&i| seen.remove(&lines[i].name).is_some());
    let unnamed = (1..lines.len()).filter(|&i| lines[i].kind == LineKind::Merged && lines[i].name.is_empty()).count();

    let area = Rect::new(rect.x0, y, rect.x1, rect.y1 - 6.0);
    ui.scroll("legend", area, |ui, offset| {
        let mut y = area.y0 - offset;
        y = section(ui, x, y + 4.0, width, "Основная", "коммиты");
        let row = |y: f64| Rect::new(rect.x0 + 6.0, y, rect.x1 - 6.0, y + ROW);
        line_row(ui, s, row(y), 0, None, free, peek);
        y += ROW;
        if !active.is_empty() {
            y = section(ui, x, y + 10.0, width, "В работе", &fmt::number(active.len()));
            for &line in &active {
                if y + ROW > area.y0 && y < area.y1 {
                    line_row(ui, s, row(y), line, None, free, peek);
                }
                y += ROW;
            }
        }
        if !merged.is_empty() {
            y = section(ui, x, y + 10.0, width, "Слитые", &fmt::number(merged.len()));
            let shown = if s.show_all_merged { merged.len() } else { merged.len().min(MERGED_PREVIEW) };
            for &line in &merged[..shown] {
                if y + ROW > area.y0 && y < area.y1 {
                    let total = totals.get(&s.map.lines[line].name).copied();
                    line_row(ui, s, row(y), line, total, free, peek);
                }
                y += ROW;
            }
            if shown < merged.len() {
                let label = format!("Показать все {}", fmt::number(merged.len()));
                if ui.button("all-merged", Rect::from_origin_size((x, y + 6.0), (width, 32.0)), &label, Tone::Plain) {
                    s.show_all_merged = true;
                }
                y += 44.0;
            }
        }
        if unnamed > 0 && options.show_merged {
            let note = format!("Ещё {} без имени: их указатели удалены.", fmt::count(unnamed, "ветка", "ветки", "веток"));
            y += 10.0 + ui.paragraph(&note, Face::Body, 12.5, Point::new(x, y + 10.0), width, pal.ink_weak) + 12.0;
        }
        if !info.family.is_empty() {
            y = section(ui, x, y + 10.0, width, "Родственные проекты", &fmt::number(info.family.len()));
            for (name, path) in &info.family {
                let rect = Rect::from_origin_size((x - 8.0, y), (width + 16.0, 30.0));
                if ui.button(("family", name), rect, "", Tone::Quiet) {
                    change.action = Some(Action::Open(path.clone()));
                }
                ui.line(name, Face::Body, 14.0, x, y + 15.0, width, pal.ink);
                y += 32.0;
            }
        }
        ui.scroll_content("legend", y + offset - area.y0);
    });
}

fn back_button(ui: &mut Ui, s: &mut Session, rect: Rect) {
    let label = if s.back.is_some() { "Назад к ветке" } else { "Назад к обозначениям" };
    if ui.button("back", rect, label, Tone::Quiet) {
        let target = s.back.map_or(Selection::None, Selection::Line);
        s.select(target);
        s.page_changed = ui.now();
    }
}

fn line_status(s: &Session, line_id: u32) -> String {
    let line = &s.map.lines[line_id as usize];
    let main = &s.map.lines[0].name;
    let Some((_, stats)) = s.line_stats.as_ref().filter(|(l, _)| *l == line_id) else { return String::new() };
    match line.kind {
        LineKind::Main => "Основная ветка: идёт по центру схемы.".to_owned(),
        LineKind::Active => {
            let ahead = fmt::count(stats.ahead, "коммит", "коммита", "коммитов");
            let behind = fmt::count(stats.behind, "коммит", "коммита", "коммитов");
            if stats.behind == 0 {
                format!("В работе. Впереди {main} на {ahead}.")
            } else {
                format!("В работе. Впереди {main} на {ahead}, отстаёт на {behind}.")
            }
        }
        LineKind::Merged => match &stats.merged_into {
            Some(target) if !target.is_empty() => format!("Слита в {target}."),
            _ => "Слита, отдельного коммита слияния нет.".to_owned(),
        },
    }
}

fn line_view(ui: &mut Ui, s: &mut Session, line_id: u32, rect: Rect) {
    let pal = ui.pal;
    let x = rect.x0 + PAD;
    let width = rect.width() - PAD * 2.0;
    back_button(ui, s, Rect::from_origin_size((x - 8.0, rect.y0 + 10.0), (190.0, 30.0)));
    let line = &s.map.lines[line_id as usize];
    let commits: Vec<Idx> = line.commits.iter().rev().copied().collect();
    let first = commits[commits.len() - 1];
    let (since, until) = (fmt::day(s.graph.commits[first as usize].time), fmt::day(s.graph.commits[commits[0] as usize].time));
    let dates = if since == until { since } else { format!("с {since} по {until}") };
    let authors = s.line_stats.as_ref().filter(|(l, _)| *l == line_id).map(|(_, stats)| {
        let top: Vec<String> = stats.authors.iter().take(3).map(|(a, n)| format!("{a} ({n})")).collect();
        top.join(", ")
    });
    let title = line_title(line);
    let mut y = rect.y0 + 48.0;
    y += ui.paragraph(&title, Face::Display, 32.0, Point::new(x, y), width, pal.ink) + 6.0;
    let last = commits[0];
    y += ui.paragraph(&line_status(s, line_id), Face::Body, 14.5, Point::new(x, y), width, pal.ink) + 12.0;
    y = pair(ui, x, y, width, "Период", &dates);
    if let Some(authors) = authors {
        y = pair(ui, x, y, width, "Авторы", &authors);
    }
    if line_id != 0 {
        let main_name = s.map.lines[0].name.clone();
        let label = format!("Отличия от {main_name}");
        if ui.button("line-diff", Rect::from_origin_size((x, y + 4.0), (width, 32.0)), &label, Tone::Plain) {
            let main_tip = s.map.lines[0].commits[s.map.lines[0].commits.len() - 1];
            let (main, tip) = (s.graph.commits[main_tip as usize].id, s.graph.commits[last as usize].id);
            s.open_diff(format!("{title}: отличия от {main_name}"), Request::Branch { main, tip }, None, ui.now());
        }
        y += 40.0;
    }
    y = section(ui, x, y + 6.0, width, "Коммиты", &fmt::number(commits.len()));
    let area = Rect::new(rect.x0, y, rect.x1, rect.y1 - 6.0);
    ui.scroll("line-commits", area, |ui, offset| {
        ui.scroll_content("line-commits", commits.len() as f64 * ROW);
        let first = (offset / ROW) as usize;
        let last = (first + (area.height() / ROW) as usize + 2).min(commits.len());
        for (i, commit) in commits.iter().enumerate().take(last).skip(first) {
            let top = area.y0 + i as f64 * ROW - offset;
            commit_row(ui, s, Rect::new(rect.x0 + 6.0, top, rect.x1 - 6.0, top + ROW), *commit, false);
        }
    });
}

fn file_badge(kind: FileKind) -> &'static str {
    match kind {
        FileKind::Added => "A",
        FileKind::Modified => "M",
        FileKind::Deleted => "D",
        FileKind::Renamed => "R",
    }
}

fn commit_view(ui: &mut Ui, s: &mut Session, commit: Idx, rect: Rect) {
    let pal = ui.pal;
    let x = rect.x0 + PAD;
    let width = rect.width() - PAD * 2.0;
    back_button(ui, s, Rect::from_origin_size((x - 8.0, rect.y0 + 10.0), (190.0, 30.0)));
    let c = &s.graph.commits[commit as usize];
    let hash = c.id.to_string();
    let subject = if c.subject.is_empty() { "Без описания".to_owned() } else { c.subject.clone() };
    let (author, date) = (c.author.clone(), fmt::date(c.time));
    let parents = c.parents.clone();
    let line_name = line_title(&s.map.lines[s.map.line_of[commit as usize] as usize]);
    let refs: Vec<String> = s.graph.refs.iter().filter(|r| r.commit == commit).map(|r| r.name.clone()).collect();
    let is_head = s.graph.head == Some(commit);
    let id_of_commit = c.id;
    let mut jump = None;
    let mut want_diff: Option<Option<String>> = None;
    let area = Rect::new(rect.x0, rect.y0 + 44.0, rect.x1, rect.y1 - 6.0);
    ui.scroll("commit", area, |ui, offset| {
        let mut y = area.y0 + 4.0 - offset;
        y += ui.paragraph(&subject, Face::Semi, 20.0, Point::new(x, y), width, pal.ink) + 10.0;
        let shown = &hash[..hash.len().min(12)];
        ui.line(shown, Face::Mono, 13.5, x, y + 15.0, 120.0, pal.ink);
        if ui.button("copy-hash", Rect::from_origin_size((x + width - 150.0, y), (150.0, 30.0)), "Копировать хеш", Tone::Plain) {
            copy_text(hash.clone());
        }
        y += 40.0;
        let half = (width - 8.0) / 2.0;
        if ui.button("commit-diff", Rect::from_origin_size((x, y), (half, 32.0)), "Изменения", Tone::Primary) {
            want_diff = Some(None);
        }
        let waiting = s.compare_from == Some(commit);
        let label = if waiting { "Отмена" } else { "Сравнить" };
        if ui.button("commit-compare", Rect::from_origin_size((x + half + 8.0, y), (half, 32.0)), label, Tone::Plain) {
            s.compare_from = if waiting { None } else { Some(commit) };
        }
        y += 40.0;
        if waiting {
            y += ui.paragraph("Щёлкни второй коммит на схеме.", Face::Body, 13.5, Point::new(x, y), width, pal.ink_weak) + 8.0;
        }
        y = pair(ui, x, y, width, "Автор", &author);
        y = pair(ui, x, y, width, "Дата", &date);
        y = pair(ui, x, y, width, "Ветка", &line_name);
        if !refs.is_empty() {
            y = pair(ui, x, y, width, "Указатели", &refs.join(", "));
        }
        if is_head {
            y = pair(ui, x, y, width, "HEAD", "Этот коммит открыт в рабочей папке.");
        }
        match s.details.as_ref().filter(|(c, _)| *c == commit) {
            Some((_, Ok(details))) => {
                let body = details.message.split_once('\n').map_or("", |(_, rest)| rest).trim();
                if !body.is_empty() {
                    y = section(ui, x, y + 8.0, width, "Описание", "");
                    y += ui.paragraph(body, Face::Body, 14.0, Point::new(x, y + 4.0), width, pal.ink) + 8.0;
                }
                y = section(ui, x, y + 8.0, width, "Файлы", &fmt::number(details.files.len()));
                if details.files.is_empty() {
                    y += ui.paragraph("Файлы не менялись.", Face::Body, 14.0, Point::new(x, y + 4.0), width, pal.ink_weak) + 8.0;
                }
                for file in details.files.iter().take(400) {
                    let layout = ui.fonts.layout(&file.path, Face::Body, 12.5, Some((width - 18.0) as f32));
                    let height = layout.height() as f64;
                    let row = Rect::new(x - 8.0, y, x + width + 8.0, y + height + 5.0);
                    let key = id(("file-row", &file.path));
                    let response = ui.interact(key, row);
                    let hover = ui.tween((key, "hover"), response.hovered, anim::SNAP);
                    ui.fill(&RoundedRect::from_rect(row, 6.0), fade(pal.sheet_hover, hover));
                    if response.clicked {
                        want_diff = Some(Some(file.path.clone()));
                    }
                    ui.line(file_badge(file.kind), Face::Mono, 12.5, x, y + 10.0, 14.0, pal.ink_weak);
                    crate::text::draw(ui.scene, &layout, vello::kurbo::Affine::translate((x + 18.0, y + 2.0)), pal.ink);
                    y += height + 5.0;
                }
                if details.files.len() > 400 {
                    let more = format!("И ещё {}.", fmt::number(details.files.len() - 400));
                    y += ui.paragraph(&more, Face::Body, 12.5, Point::new(x, y + 2.0), width, pal.ink_weak) + 4.0;
                }
            }
            Some((_, Err(error))) => {
                y = section(ui, x, y + 8.0, width, "Файлы", "");
                let text = format!("Не удалось прочитать изменения: {error}");
                y += ui.paragraph(&text, Face::Body, 14.0, Point::new(x, y + 4.0), width, pal.danger) + 8.0;
            }
            None => {
                y = section(ui, x, y + 8.0, width, "Файлы", "");
                y += ui.paragraph("Читаю изменения.", Face::Body, 14.0, Point::new(x, y + 4.0), width, pal.ink_weak) + 8.0;
            }
        }
        if !parents.is_empty() {
            let title = if parents.len() > 1 { "Родители, это слияние" } else { "Родитель" };
            y = section(ui, x, y + 8.0, width, title, "");
            for &parent in &parents {
                let p = &s.graph.commits[parent as usize];
                let text = format!("{}  {}", p.id.to_hex_with_len(8), p.subject);
                if ui.button(("parent", parent), Rect::from_origin_size((x - 8.0, y), (width + 16.0, 30.0)), "", Tone::Quiet) {
                    jump = Some(parent);
                }
                ui.line(&text, Face::Body, 13.0, x, y + 15.0, width, pal.ink);
                y += 32.0;
            }
        }
        ui.scroll_content("commit", y + 14.0 + offset - area.y0);
    });
    if let Some(prefer) = want_diff {
        let title = subject.clone();
        s.open_diff(title, Request::Commits { from: None, to: id_of_commit }, prefer, ui.now());
    }
    if let Some(parent) = jump {
        let zoom = map::current_zoom(s);
        s.motion.camera.aim(map::focus_commit(&s.map, parent, zoom));
        s.motion.auto_frame = false;
        s.select(Selection::Commit(parent));
        s.motion.ring(parent, ui.now());
        s.page_changed = ui.now();
    }
}

/// Незакоммиченная работа: список файлов и переход к строкам.
fn work_view(ui: &mut Ui, s: &mut Session, rect: Rect) {
    let pal = ui.pal;
    let x = rect.x0 + PAD;
    let width = rect.width() - PAD * 2.0;
    back_button(ui, s, Rect::from_origin_size((x - 8.0, rect.y0 + 10.0), (190.0, 30.0)));
    let mut y = rect.y0 + 48.0;
    y += ui.paragraph("Незакоммиченная работа", Face::Semi, 20.0, Point::new(x, y), width, pal.ink) + 10.0;
    let open = ui.button("work-diff", Rect::from_origin_size((x, y), (width, 32.0)), "Показать изменения", Tone::Primary);
    y = section(ui, x, y + 42.0, width, "Файлы", &fmt::number(s.work.len()));
    let area = Rect::new(rect.x0, y, rect.x1, rect.y1 - 6.0);
    let files = s.work.clone();
    let mut prefer = None;
    ui.scroll("work-files", area, |ui, offset| {
        ui.scroll_content("work-files", files.len() as f64 * 26.0);
        let first = (offset / 26.0) as usize;
        let last = (first + (area.height() / 26.0) as usize + 2).min(files.len());
        for (i, file) in files.iter().enumerate().take(last).skip(first) {
            let top = area.y0 + i as f64 * 26.0 - offset;
            let row = Rect::new(rect.x0 + 10.0, top, rect.x1 - 10.0, top + 26.0);
            let key = id(("work-file", &file.path));
            let response = ui.interact(key, row);
            let hover = ui.tween((key, "hover"), response.hovered, anim::SNAP);
            ui.fill(&RoundedRect::from_rect(row, 6.0), fade(pal.sheet_hover, hover));
            ui.line(file_badge(file.kind), Face::Mono, 12.5, x, row.center().y, 14.0, pal.ink_weak);
            ui.line(&file.path, Face::Body, 12.5, x + 18.0, row.center().y, width - 18.0, pal.ink);
            if response.clicked {
                prefer = Some(file.path.clone());
            }
        }
    });
    if open || prefer.is_some() {
        s.open_diff("Незакоммиченная работа".to_owned(), Request::Worktree, prefer, ui.now());
    }
}

#[allow(clippy::too_many_arguments)]
pub fn show(ui: &mut Ui, s: &mut Session, options: &mut Options, show_tags: &mut bool, rect: Rect, free: Rect, info: &Info) -> Change {
    ui.sheet(rect, 16.0, 1.0);
    let mut change = Change::default();
    let mut peek = None;
    // Сдвиг страницы при смене: содержимое въезжает справа и проявляется.
    let phase = ease_out(((ui.now() - s.page_changed) / 0.26) as f32);
    if phase < 1.0 {
        ui.animating = true;
    }
    let content = rect + vello::kurbo::Vec2::new((1.0 - phase) as f64 * 22.0, 0.0);
    ui.clip(rect, 16.0);
    if phase < 1.0 {
        ui.alpha(phase, rect);
    }
    match s.selection {
        Selection::None => legend(ui, s, options, show_tags, content, free, &mut change, &mut peek, info),
        Selection::Work => work_view(ui, s, content),
        Selection::Line(line) => line_view(ui, s, line, content),
        Selection::Commit(commit) => commit_view(ui, s, commit, content),
    }
    if phase < 1.0 {
        ui.end_alpha();
    }
    ui.unclip();
    if ui.input.pointer.is_some_and(|p| rect.contains(p)) {
        s.motion.peek = peek;
    }
    change
}
