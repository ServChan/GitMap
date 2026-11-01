//! Правая панель: условные обозначения схемы, сведения о ветке и о коммите.

use crate::app::{Selection, Session, fit, row};
use crate::git::{FileKind, Idx};
use crate::layout::{Line, LineKind, Options};
use crate::map;
use crate::theme::{Palette, semibold};
use crate::fmt;
use eframe::egui::{self, Color32, FontId, Rect, RichText, Sense, Stroke, pos2, vec2};
use std::collections::HashMap;

pub const WIDTH: f32 = 320.0;
const MARGIN: f32 = 16.0;
const MERGED_PREVIEW: usize = 12;

#[derive(Default)]
pub struct Change {
    pub relayout: bool,
    pub labels: bool,
}

fn margin() -> egui::Frame {
    egui::Frame::new().inner_margin(egui::Margin::symmetric(MARGIN as i8, 0))
}

/// Подзаголовок раздела: тонкая линия сверху, как у граф в таблице.
fn section(ui: &mut egui::Ui, title: &str, note: &str, pal: &Palette) {
    ui.add_space(14.0);
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 26.0), Sense::hover());
    let painter = ui.painter();
    painter.hline(rect.x_range(), rect.max.y - 0.5, Stroke::new(1.0, pal.ink));
    painter.text(rect.left_center(), egui::Align2::LEFT_CENTER, title, FontId::new(13.5, semibold()), pal.ink);
    painter.text(rect.right_center(), egui::Align2::RIGHT_CENTER, note, FontId::proportional(12.5), pal.ink_weak);
}

fn line_swatch(painter: &egui::Painter, rect: Rect, line: &Line, pal: &Palette, selected: bool) {
    let color = match line.kind {
        LineKind::Main => if selected { pal.paper } else { pal.ink },
        _ if selected => pal.paper,
        _ => pal.lines[line.color],
    };
    let width = match line.kind {
        LineKind::Main => 6.0,
        LineKind::Active => 4.0,
        LineKind::Merged => 2.0,
    };
    let (a, b) = (rect.left_center(), rect.right_center());
    painter.line_segment([a, b], Stroke::new(width, color));
    let fill = if selected { pal.ink } else { pal.paper };
    painter.circle(rect.center(), width / 2.0 + 2.0, fill, Stroke::new(1.5, color));
}

fn line_title(line: &Line) -> String {
    if line.name.is_empty() { "Ветка без имени".to_owned() } else { line.name.clone() }
}

fn line_row(ui: &mut egui::Ui, s: &mut Session, id: usize, total: Option<usize>, canvas: Rect, pal: &Palette) {
    let line = &s.map.lines[id];
    let selected = s.selection == Selection::Line(id as u32);
    let title = line_title(line);
    let count = total.unwrap_or(line.commits.len());
    let response = row(ui, 28.0, selected, &title, pal, |painter, rect, ink, weak| {
        let swatch = Rect::from_center_size(rect.left_center() + vec2(MARGIN + 14.0, 0.0), vec2(28.0, 12.0));
        line_swatch(painter, swatch, line, pal, selected);
        let width = rect.width() - MARGIN * 2.0 - 40.0 - 44.0;
        let name = fit(painter, title.clone(), FontId::proportional(14.0), ink, width);
        painter.galley(pos2(swatch.max.x + 10.0, rect.center().y - name.size().y / 2.0), name, ink);
        painter.text(rect.right_center() - vec2(MARGIN, 0.0), egui::Align2::RIGHT_CENTER, fmt::number(count), FontId::proportional(12.5), weak);
    });
    if response.clicked() {
        let to = map::focus_line(&s.map, &s.map.lines[id], canvas);
        s.select(Selection::Line(id as u32));
        s.want_camera = Some(to);
    }
}

fn commit_row(ui: &mut egui::Ui, s: &mut Session, commit: Idx, zoom_to: bool, pal: &Palette) {
    let c = &s.graph.commits[commit as usize];
    let selected = s.selection == Selection::Commit(commit);
    let subject = if c.subject.is_empty() { "Без описания".to_owned() } else { c.subject.clone() };
    let age = fmt::age(c.time);
    let response = row(ui, 28.0, selected, &subject, pal, |painter, rect, ink, weak| {
        let width = rect.width() - MARGIN * 2.0 - 58.0;
        let name = fit(painter, subject.clone(), FontId::proportional(13.5), ink, width);
        painter.galley(pos2(rect.min.x + MARGIN, rect.center().y - name.size().y / 2.0), name, ink);
        painter.text(rect.right_center() - vec2(MARGIN, 0.0), egui::Align2::RIGHT_CENTER, &age, FontId::proportional(12.5), weak);
    });
    if response.clicked() {
        if zoom_to {
            s.want_camera = Some(map::focus_commit(&s.map, commit, s.cam.zoom));
        }
        s.select(Selection::Commit(commit));
    }
}

fn legend(ui: &mut egui::Ui, s: &mut Session, options: &mut Options, show_tags: &mut bool, canvas: Rect, pal: &Palette, change: &mut Change) {
    let now = ui.input(|i| i.time);
    margin().show(ui, |ui| {
        ui.add_space(14.0);
        let search_id = ui.scope_id().with("search");
        if ui.input_mut(|i| i.consume_key(egui::Modifiers::COMMAND, egui::Key::F)) {
            ui.memory_mut(|m| m.request_focus(search_id));
        }
        let edit = egui::TextEdit::singleline(&mut s.search)
            .id(search_id)
            .hint_text("Найти коммит: текст, автор, хеш")
            .desired_width(f32::INFINITY);
        if ui.add(edit).changed() {
            s.run_search();
        }
    });

    if !s.search.trim().is_empty() {
        let hits = s.search_hits.len();
        margin().show(ui, |ui| {
            section(ui, "Найдено", &fmt::number(hits), pal);
        });
        if hits == 0 {
            margin().show(ui, |ui| {
                ui.add_space(8.0);
                ui.label(RichText::new("Таких коммитов нет. Проверь написание или ищи по автору.").color(pal.ink_weak));
            });
            return;
        }
        ui.spacing_mut().item_spacing.y = 0.0;
        egui::ScrollArea::vertical().id_salt("hits").auto_shrink(false).show_rows(ui, 28.0, hits, |ui, range| {
            for i in range {
                let commit = s.search_hits[i];
                commit_row(ui, s, commit, true, pal);
            }
        });
        return;
    }

    let mut view_changed = false;
    margin().show(ui, |ui| {
        section(ui, "Вид", "", pal);
        ui.add_space(6.0);
        view_changed |= ui.checkbox(&mut options.show_merged, "Слитые ветки").changed();
        view_changed |= ui.checkbox(&mut options.compress, "Сжимать прямые участки").changed();
        change.labels |= ui.checkbox(show_tags, "Теги").changed();
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            if ui.button("Вся схема").on_hover_text("Home").clicked() {
                map::move_camera(s, map::whole(&s.map, canvas), now);
            }
            if ui.button("К концу").on_hover_text("End").clicked() {
                map::move_camera(s, map::latest(&s.map, canvas), now);
            }
        });
    });
    change.relayout |= view_changed;

    let mut active: Vec<usize> = (1..s.map.lines.len()).filter(|&i| s.map.lines[i].kind == LineKind::Active).collect();
    let last_commit = |s: &Session, i: usize| {
        let line = &s.map.lines[i];
        s.graph.commits[line.commits[line.commits.len() - 1] as usize].time
    };
    active.sort_by_key(|&i| std::cmp::Reverse(last_commit(s, i)));
    let mut merged: Vec<usize> = (1..s.map.lines.len())
        .filter(|&i| s.map.lines[i].kind == LineKind::Merged && !s.map.lines[i].name.is_empty() && s.map.lines[i].visible)
        .collect();
    merged.sort_by_key(|&i| std::cmp::Reverse(last_commit(s, i)));
    // Ветку, которую вливали не раз, показываем одной строкой с общим числом коммитов.
    let mut seen: HashMap<String, usize> = HashMap::new();
    for &i in &merged {
        *seen.entry(s.map.lines[i].name.clone()).or_default() += s.map.lines[i].commits.len();
    }
    let totals = seen.clone();
    merged.retain(|&i| seen.remove(&s.map.lines[i].name).is_some());
    let unnamed = (1..s.map.lines.len()).filter(|&i| s.map.lines[i].kind == LineKind::Merged && s.map.lines[i].name.is_empty()).count();

    ui.spacing_mut().item_spacing.y = 0.0;
    egui::ScrollArea::vertical().id_salt("legend").auto_shrink(false).show(ui, |ui| {
        ui.spacing_mut().item_spacing.y = 0.0;
        margin().show(ui, |ui| section(ui, "Основная", "коммиты", pal));
        line_row(ui, s, 0, None, canvas, pal);
        if !active.is_empty() {
            margin().show(ui, |ui| section(ui, "В работе", &fmt::number(active.len()), pal));
            for &id in &active {
                line_row(ui, s, id, None, canvas, pal);
            }
        }
        if !merged.is_empty() {
            margin().show(ui, |ui| section(ui, "Слитые", &fmt::number(merged.len()), pal));
            let shown = if s.show_all_merged { merged.len() } else { merged.len().min(MERGED_PREVIEW) };
            for &id in &merged[..shown] {
                let total = totals.get(&s.map.lines[id].name).copied();
                line_row(ui, s, id, total, canvas, pal);
            }
            if shown < merged.len() {
                margin().show(ui, |ui| {
                    ui.add_space(6.0);
                    if ui.button(format!("Показать все {}", fmt::number(merged.len()))).clicked() {
                        s.show_all_merged = true;
                    }
                });
            }
        }
        if unnamed > 0 && options.show_merged {
            margin().show(ui, |ui| {
                ui.add_space(10.0);
                let text = format!("Ещё {} без имени: их указатели удалены.", fmt::count(unnamed, "ветка", "ветки", "веток"));
                ui.label(RichText::new(text).size(12.5).color(pal.ink_weak));
                ui.add_space(14.0);
            });
        }
    });
}

fn back_button(ui: &mut egui::Ui, s: &mut Session, pal: &Palette) {
    let target = s.back.map_or(Selection::None, Selection::Line);
    let label = if s.back.is_some() { "Назад к ветке" } else { "Назад к обозначениям" };
    ui.add_space(12.0);
    let button = egui::Button::new(RichText::new(label).color(pal.ink)).frame(false);
    if ui.add(button).on_hover_text("Esc").clicked() {
        s.select(target);
    }
}

fn pair(ui: &mut egui::Ui, key: &str, value: &str, pal: &Palette) {
    ui.horizontal_top(|ui| {
        let (rect, _) = ui.allocate_exact_size(vec2(72.0, 18.0), Sense::hover());
        ui.painter().text(rect.left_top() + vec2(0.0, 2.0), egui::Align2::LEFT_TOP, key, FontId::proportional(12.5), pal.ink_weak);
        ui.add(egui::Label::new(RichText::new(value).color(pal.ink)).wrap());
    });
}

fn line_view(ui: &mut egui::Ui, s: &mut Session, id: u32, pal: &Palette) {
    let line = &s.map.lines[id as usize];
    let commits: Vec<Idx> = line.commits.iter().rev().copied().collect();
    let status = line_status(s, id);
    let (first, last) = (commits[commits.len() - 1], commits[0]);
    let (since, until) = (fmt::day(s.graph.commits[first as usize].time), fmt::day(s.graph.commits[last as usize].time));
    let dates = if since == until { since } else { format!("с {since} по {until}") };
    let authors = s.line_stats.as_ref().filter(|(l, _)| *l == id).map(|(_, stats)| {
        let top: Vec<String> = stats.authors.iter().take(3).map(|(a, n)| format!("{a} ({n})")).collect();
        top.join(", ")
    });
    let title = line_title(line);
    margin().show(ui, |ui| {
        back_button(ui, s, pal);
        ui.add_space(4.0);
        ui.add(egui::Label::new(RichText::new(title).heading().color(pal.ink)).wrap());
        ui.add_space(2.0);
        ui.add(egui::Label::new(RichText::new(status).color(pal.ink)).wrap());
        ui.add_space(8.0);
        pair(ui, "Период", &dates, pal);
        if let Some(authors) = authors {
            pair(ui, "Авторы", &authors, pal);
        }
        section(ui, "Коммиты", &fmt::number(commits.len()), pal);
    });
    ui.spacing_mut().item_spacing.y = 0.0;
    egui::ScrollArea::vertical().id_salt("line-commits").auto_shrink(false).show_rows(ui, 28.0, commits.len(), |ui, range| {
        for i in range {
            commit_row(ui, s, commits[i], false, pal);
        }
    });
}

fn line_status(s: &Session, id: u32) -> String {
    let line = &s.map.lines[id as usize];
    let main = &s.map.lines[0].name;
    let Some((_, stats)) = s.line_stats.as_ref().filter(|(l, _)| *l == id) else {
        return String::new();
    };
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

fn file_badge(kind: FileKind) -> (&'static str, &'static str) {
    match kind {
        FileKind::Added => ("A", "Добавлен"),
        FileKind::Modified => ("M", "Изменён"),
        FileKind::Deleted => ("D", "Удалён"),
        FileKind::Renamed => ("R", "Переименован"),
    }
}

fn commit_view(ui: &mut egui::Ui, s: &mut Session, commit: Idx, pal: &Palette) {
    let ctx = ui.ctx().clone();
    let c = &s.graph.commits[commit as usize];
    let hash = c.id.to_string();
    let subject = if c.subject.is_empty() { "Без описания".to_owned() } else { c.subject.clone() };
    let author = c.author.clone();
    let date = fmt::date(c.time);
    let line_id = s.map.line_of[commit as usize];
    let line_name = line_title(&s.map.lines[line_id as usize]);
    let refs: Vec<String> = s.graph.refs.iter().filter(|r| r.commit == commit).map(|r| r.name.clone()).collect();
    let is_head = s.graph.head == Some(commit);
    let parents: Vec<Idx> = c.parents.clone();
    let mut jump = None;

    egui::ScrollArea::vertical().id_salt("commit").auto_shrink(false).show(ui, |ui| {
        margin().show(ui, |ui| {
            back_button(ui, s, pal);
            ui.add_space(4.0);
            ui.add(egui::Label::new(RichText::new(&subject).size(20.0).family(semibold()).color(pal.ink)).wrap());
            ui.add_space(8.0);

            ui.horizontal(|ui| {
                ui.label(RichText::new(&hash[..hash.len().min(12)]).monospace().color(pal.ink));
                if ui.small_button("Копировать хеш").clicked() {
                    ctx.copy_text(hash.clone());
                }
            });
            ui.add_space(6.0);
            pair(ui, "Автор", &author, pal);
            pair(ui, "Дата", &date, pal);
            pair(ui, "Ветка", &line_name, pal);
            if !refs.is_empty() {
                pair(ui, "Указатели", &refs.join(", "), pal);
            }
            if is_head {
                pair(ui, "HEAD", "Этот коммит открыт в рабочей папке.", pal);
            }

            match s.details.as_ref().filter(|(c, _)| *c == commit) {
                Some((_, Ok(details))) => {
                    let body = details.message.split_once('\n').map_or("", |(_, rest)| rest).trim();
                    if !body.is_empty() {
                        section(ui, "Описание", "", pal);
                        ui.add_space(6.0);
                        ui.add(egui::Label::new(RichText::new(body).color(pal.ink)).wrap());
                    }
                    if !details.email.is_empty() {
                        ui.add_space(4.0);
                    }
                    section(ui, "Файлы", &fmt::number(details.files.len()), pal);
                    ui.add_space(6.0);
                    if details.files.is_empty() {
                        ui.label(RichText::new("Файлы не менялись.").color(pal.ink_weak));
                    }
                    ui.spacing_mut().item_spacing.y = 3.0;
                    for file in details.files.iter().take(400) {
                        let (letter, meaning) = file_badge(file.kind);
                        ui.horizontal_top(|ui| {
                            ui.spacing_mut().item_spacing.x = 6.0;
                            ui.label(RichText::new(letter).monospace().color(pal.ink_weak)).on_hover_text(meaning);
                            ui.add(egui::Label::new(RichText::new(&file.path).size(12.5).color(pal.ink)).wrap());
                        });
                    }
                    if details.files.len() > 400 {
                        ui.label(RichText::new(format!("И ещё {}.", fmt::number(details.files.len() - 400))).color(pal.ink_weak));
                    }
                }
                Some((_, Err(error))) => {
                    section(ui, "Файлы", "", pal);
                    ui.add_space(6.0);
                    ui.label(RichText::new(format!("Не удалось прочитать изменения: {error}")).color(pal.error));
                }
                None => {
                    section(ui, "Файлы", "", pal);
                    ui.add_space(6.0);
                    ui.label(RichText::new("Читаю изменения.").color(pal.ink_weak));
                }
            }
            if !parents.is_empty() {
                let title = if parents.len() > 1 { "Родители, это слияние" } else { "Родитель" };
                section(ui, title, "", pal);
                ui.add_space(4.0);
                for &parent in &parents {
                    let p = &s.graph.commits[parent as usize];
                    let text = format!("{}  {}", p.id.to_hex_with_len(8), p.subject);
                    let galley = fit(ui.painter(), text, FontId::proportional(13.0), Color32::PLACEHOLDER, ui.available_width() - 16.0);
                    let button = egui::Button::new(galley).frame(false);
                    if ui.add(button).clicked() {
                        jump = Some(parent);
                    }
                }
                ui.add_space(14.0);
            }
        });
    });
    if let Some(parent) = jump {
        s.want_camera = Some(map::focus_commit(&s.map, parent, s.cam.zoom));
        s.select(Selection::Commit(parent));
    }
}

pub fn show(ui: &mut egui::Ui, s: &mut Session, options: &mut Options, show_tags: &mut bool, canvas: Rect, pal: &Palette) -> Change {
    let full = ui.max_rect();
    ui.painter().vline(full.min.x + 0.5, full.y_range(), Stroke::new(1.0, pal.ink));
    let mut change = Change::default();
    match s.selection {
        Selection::None => legend(ui, s, options, show_tags, canvas, pal, &mut change),
        Selection::Line(id) => line_view(ui, s, id, pal),
        Selection::Commit(commit) => commit_view(ui, s, commit, pal),
    }
    if let Some(to) = s.want_camera.take() {
        let now = ui.input(|i| i.time);
        map::move_camera(s, to, now);
    }
    change
}
