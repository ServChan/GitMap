//! Левый лист: указатель проектов. Выбранная строка отмечена чёрным блоком, который
//! плавно едет от строки к строке; строки появляются волной по мере сканирования.

use crate::anim::{self, ease_out};
use crate::app::{App, Sort, ThemeChoice, UpdateState};
use crate::layout::PALETTE_SIZE;
use crate::text::Face;
use crate::theme::{fade, mix};
use crate::ui::{Tone, Ui, id};
use crate::{fmt, install};
use std::collections::HashMap;
use vello::kurbo::{Circle, Line, Point, Rect, RoundedRect, Stroke};

const ROW: f64 = 34.0;
const GROUP: f64 = 36.0;

enum Item {
    Overview,
    Group { name: String, count: usize, open: f64 },
    Repo(usize),
}

fn grouped(app: &App, needle: &str) -> Vec<(String, Vec<usize>)> {
    let mut groups: HashMap<&str, Vec<usize>> = HashMap::new();
    for (i, repo) in app.repos.iter().enumerate() {
        if needle.is_empty() || repo.name.to_lowercase().contains(needle) {
            groups.entry(&repo.group).or_default().push(i);
        }
    }
    let time = |i: usize| match &app.repos[i].summary {
        Some(Ok(summary)) => summary.last_commit.unwrap_or_default(),
        _ => 0,
    };
    let mut list: Vec<(String, Vec<usize>)> = groups
        .into_iter()
        .map(|(group, mut members)| {
            match app.settings.sort {
                Sort::Name => members.sort_by_key(|&i| app.repos[i].name.to_lowercase()),
                Sort::Recent => members.sort_by_key(|&i| (std::cmp::Reverse(time(i)), app.repos[i].name.to_lowercase())),
            }
            (group.to_owned(), members)
        })
        .collect();
    // Корень первым, служебные папки на `_` в конце.
    list.sort_by_key(|(group, _)| (group.starts_with('_'), !group.is_empty(), group.to_lowercase()));
    list
}

pub fn show(ui: &mut Ui, app: &mut App, rect: Rect) {
    let pal = ui.pal;
    ui.sheet(rect, 16.0, 1.0);
    let x = rect.x0 + 18.0;
    let width = rect.width() - 36.0;

    let name = app
        .settings
        .root
        .as_ref()
        .map_or_else(|| "Проекты".to_owned(), |r| r.file_name().map_or_else(|| r.display().to_string(), |n| n.to_string_lossy().into_owned()));
    let title = ui.fonts.fit(&name, Face::Display, 34.0, width as f32);
    crate::text::draw(ui.scene, &title, vello::kurbo::Affine::translate((x, rect.y0 + 12.0)), pal.ink);
    if let Some(root) = app.settings.root.clone() {
        ui.line(&root.display().to_string(), Face::Body, 12.5, x, rect.y0 + 62.0, width, pal.ink_weak);
    }
    let change = Rect::from_origin_size((x, rect.y0 + 76.0), (width, 34.0));
    let label = if app.settings.root.is_some() { "Сменить папку" } else { "Выбрать папку" };
    if ui.button("change-folder", change, label, Tone::Plain) {
        app.pick_folder();
    }
    let field = Rect::from_origin_size((x, rect.y0 + 118.0), (width, 36.0));
    ui.input_field("project-filter", field, &mut app.filter, "Найти проект");

    // Шапка таблицы: щелчок меняет порядок строк.
    let head = rect.y0 + 174.0;
    for (sort, label, right) in [(Sort::Name, "Проект", false), (Sort::Recent, "Изменён", true)] {
        let on = app.settings.sort == sort;
        let area = if right {
            Rect::new(rect.x1 - 92.0, head - 11.0, rect.x1 - 12.0, head + 11.0)
        } else {
            Rect::new(x - 6.0, head - 11.0, x + 70.0, head + 11.0)
        };
        let response = ui.interact(id(("sort", label)), area);
        if response.clicked {
            app.settings.sort = sort;
        }
        let color = if on || response.hovered { pal.ink } else { pal.ink_weak };
        let face = if on { Face::Semi } else { Face::Body };
        if right {
            ui.line_right(label, face, 12.5, rect.x1 - 18.0, head, color);
        } else {
            ui.line(label, face, 12.5, x, head, 80.0, color);
        }
        if response.focused {
            ui.focus_ring(area, 4.0);
        }
    }
    ui.hline(rect.x0, rect.x1, head + 13.0, 1.0, pal.rule);

    let footer = if app.can_update { 150.0 } else { 86.0 };
    let list = Rect::new(rect.x0, head + 14.0, rect.x1, rect.y1 - footer);
    let needle = app.filter.trim().to_lowercase();
    let groups = grouped(app, &needle);

    // Сначала раскладка: блок выбора должен знать свою строку раньше, чем строки нарисованы.
    let mut items: Vec<(Item, f64, f64)> = Vec::new();
    items.push((Item::Overview, 6.0, ROW));
    let mut y = 6.0 + ROW + 2.0;
    let mut selected_y = app.open_path.is_none().then_some(6.0);
    for (group, members) in &groups {
        let collapsed = needle.is_empty() && app.settings.collapsed.contains(group);
        let open = ui.tween(("group", group), !collapsed, anim::SOFT) as f64;
        if !group.is_empty() {
            items.push((Item::Group { name: group.clone(), count: members.len(), open }, y, GROUP));
            y += GROUP;
        }
        if open > 0.01 {
            for &at in members {
                let height = ROW * open;
                if app.open_path.as_ref() == Some(&app.repos[at].path) {
                    selected_y = Some(y);
                }
                items.push((Item::Repo(at), y, height));
                y += height;
            }
        }
    }
    let total = y + 8.0;
    if app.reveal_open
        && let Some(target) = selected_y
    {
        ui.scroll_to("projects", list.height(), target - ROW * 3.0, target + ROW * 4.0);
        app.reveal_open = false;
    }

    let mut open_path = None;
    let mut toggle = None;
    let mut go_overview = false;
    let now = ui.now();
    ui.scroll("projects", list, |ui, offset| {
        ui.scroll_content("projects", total);
        if let Some(target) = selected_y {
            let block = ui.spring(id("selection-block"), target as f32, target as f32, anim::SOFT) as f64;
            let top = list.y0 + block - offset;
            let bar = Rect::new(list.x0 + 8.0, top + 1.0, list.x1 - 8.0, top + ROW - 1.0);
            ui.fill(&RoundedRect::from_rect(bar, 9.0), pal.ink);
        }
        for (position, (item, y, height)) in items.iter().enumerate() {
            let top = list.y0 + y - offset;
            if top + height < list.y0 || top > list.y1 {
                continue;
            }
            let row = Rect::new(list.x0 + 8.0, top, list.x1 - 8.0, top + height);
            match item {
                Item::Overview => {
                    let key = id("overview-row");
                    let selected = app.open_path.is_none();
                    let response = ui.interact(key, row);
                    if response.clicked {
                        go_overview = true;
                    }
                    let hover = ui.tween((key, "hover"), response.hovered, anim::SNAP);
                    let lit = ui.tween((key, "lit"), selected, anim::SNAP);
                    if lit < 0.5 {
                        ui.fill(&RoundedRect::from_rect(row.inset((0.0, -1.0)), 9.0), fade(pal.sheet_hover, hover));
                    }
                    let (ink, weak) = (mix(pal.ink, pal.sheet, lit), mix(pal.ink_weak, pal.sheet, lit));
                    ui.line("Обзор папки", Face::Semi, 14.0, row.x0 + 10.0, row.center().y, row.width() - 80.0, ink);
                    let trouble = app
                        .repos
                        .iter()
                        .filter_map(|r| r.health.as_ref())
                        .filter(|h| h.unpushed > 0 || h.dirty > 0)
                        .count();
                    if trouble > 0 {
                        ui.line_right(&fmt::number(trouble), Face::Body, 12.5, row.x1 - 10.0, row.center().y, weak);
                    }
                    if response.focused {
                        ui.focus_ring(row, 9.0);
                    }
                }
                Item::Group { name, count, open } => {
                    let key = id(("group-row", name));
                    let response = ui.interact(key, row);
                    if response.clicked {
                        toggle = Some(name.clone());
                    }
                    let hover = ui.tween((key, "hover"), response.hovered, anim::SNAP);
                    ui.fill(&RoundedRect::from_rect(row.inset((0.0, -3.0)), 8.0), fade(pal.sheet_hover, hover));
                    let c = Point::new(row.x0 + 16.0, row.center().y);
                    // Стрелка поворачивается вместе с раскрытием группы.
                    let turn = (1.0 - *open) * -std::f64::consts::FRAC_PI_2;
                    let place = vello::kurbo::Affine::translate(c.to_vec2()) * vello::kurbo::Affine::rotate(turn);
                    let mut arrow = vello::kurbo::BezPath::new();
                    arrow.move_to((-4.5, -2.5));
                    arrow.line_to((0.0, 2.5));
                    arrow.line_to((4.5, -2.5));
                    let stroke = Stroke::new(1.8).with_caps(vello::kurbo::Cap::Round);
                    ui.scene.stroke(&stroke, place, pal.ink, None, &arrow);
                    ui.line(name, Face::Semi, 13.5, row.x0 + 30.0, row.center().y, row.width() - 70.0, pal.ink);
                    ui.line_right(&fmt::number(*count), Face::Body, 12.5, row.x1 - 10.0, row.center().y, pal.ink_weak);
                    if response.focused {
                        ui.focus_ring(row, 8.0);
                    }
                }
                Item::Repo(at) => {
                    let repo = &app.repos[*at];
                    let selected = app.open_path.as_ref() == Some(&repo.path);
                    let key = id(("repo", &repo.path));
                    let response = ui.interact(key, row);
                    if response.clicked {
                        open_path = Some(repo.path.clone());
                    }
                    let hover = ui.tween((key, "hover"), response.hovered, anim::SNAP);
                    let lit = ui.tween((key, "lit"), selected, anim::SNAP);
                    if lit < 0.5 {
                        ui.fill(&RoundedRect::from_rect(row.inset((0.0, -1.0)), 9.0), fade(pal.sheet_hover, hover));
                    }
                    // Волна при появлении: строки выезжают одна за другой.
                    let wait = (position as f64 * 0.014).min(0.5);
                    let appear = ease_out(((now - repo.found_at - wait) / 0.3) as f32);
                    if appear < 1.0 {
                        ui.animating = true;
                    }
                    let shift = (1.0 - appear) as f64 * -14.0 + hover as f64 * 3.0 * (1.0 - lit as f64);
                    let (ink, weak) = (mix(pal.ink, pal.sheet, lit), mix(pal.ink_weak, pal.sheet, lit));
                    let (ink, weak) = (fade(ink, appear), fade(weak, appear));
                    let (age, branches) = match &repo.summary {
                        Some(Ok(summary)) => (summary.last_commit.map(fmt::age).unwrap_or_default(), summary.branches),
                        _ => (String::new(), 0),
                    };
                    let y = row.center().y;
                    let age_width = ui.line_right(&age, Face::Body, 12.5, row.x1 - 10.0, y, weak);
                    let stripes = branch_stripes(ui, row.x1 - 18.0 - age_width.max(44.0), y, branches, lit, appear);
                    // Кольцо: в проекте есть что отправить или закоммитить. Подробности в обзоре.
                    let pending = repo.health.as_ref().is_some_and(|h| h.unpushed > 0 || h.dirty > 0);
                    let mark = if pending { 14.0 } else { 0.0 };
                    if pending {
                        let dot = Circle::new((row.x0 + 14.0 + shift, y), 3.5);
                        ui.stroke(&dot, 2.0, mix(pal.lines[0], pal.sheet, lit));
                    }
                    let room = row.width() - 18.0 - age_width.max(44.0) - stripes - 16.0 - mark;
                    ui.line(&repo.name, Face::Body, 14.0, row.x0 + 10.0 + shift + mark, y, room, ink);
                    if response.focused {
                        ui.focus_ring(row, 9.0);
                    }
                }
            }
        }
        if app.repos.is_empty() && !app.scanning && app.settings.root.is_some() {
            ui.paragraph("В этой папке нет git-репозиториев.", Face::Body, 14.0, Point::new(x, list.y0 + 12.0), width, pal.ink_weak);
        }
    });
    if let Some(path) = open_path {
        app.open_repo(path, false);
    }
    if go_overview {
        app.close_repo(now);
    }
    if let Some(group) = toggle
        && !app.settings.collapsed.remove(&group)
    {
        app.settings.collapsed.insert(group);
    }

    footer_block(ui, app, Rect::new(rect.x0, rect.y1 - footer, rect.x1, rect.y1));
}

/// Цветные полоски по числу веток: каждую строку списка видно издали.
fn branch_stripes(ui: &mut Ui, right: f64, y: f64, count: usize, lit: f32, alpha: f32) -> f64 {
    let pal = ui.pal;
    if count < 2 {
        return 0.0;
    }
    if count > 7 {
        return ui.line_right(&count.to_string(), Face::Semi, 12.5, right, y, fade(mix(pal.ink_weak, pal.sheet, lit), alpha));
    }
    let step = 5.0;
    for i in 0..count {
        let x = right - i as f64 * step;
        let color = mix(pal.lines[i % PALETTE_SIZE], pal.sheet, lit);
        ui.scene.stroke(
            &Stroke::new(2.6).with_caps(vello::kurbo::Cap::Round),
            Default::default(),
            fade(color, alpha),
            None,
            &Line::new((x, y - 6.0), (x, y + 6.0)),
        );
    }
    count as f64 * step
}

fn footer_block(ui: &mut Ui, app: &mut App, rect: Rect) {
    let pal = ui.pal;
    let x = rect.x0 + 18.0;
    let width = rect.width() - 36.0;
    ui.hline(rect.x0, rect.x1, rect.y0, 1.0, pal.rule);
    let status = if app.scanning {
        format!("Ищу репозитории: {}", app.repos.len())
    } else {
        fmt::count(app.repos.len(), "репозиторий", "репозитория", "репозиториев")
    };
    ui.line(&status, Face::Body, 12.5, x, rect.y0 + 20.0, width, pal.ink_weak);

    let mut choice = match app.settings.theme {
        ThemeChoice::System => 0,
        ThemeChoice::Light => 1,
        ThemeChoice::Dark => 2,
    };
    let segments = Rect::from_origin_size((x, rect.y0 + 36.0), (width, 32.0));
    if ui.segments("theme", segments, &["Авто", "День", "Ночь"], &mut choice) {
        app.settings.theme = [ThemeChoice::System, ThemeChoice::Light, ThemeChoice::Dark][choice];
    }
    if !app.can_update {
        return;
    }
    let line = Rect::from_origin_size((x, rect.y0 + 76.0), (width, 32.0));
    if let UpdateState::Available(manifest) = &app.update {
        let label = format!("Обновить до версии {}", manifest.version);
        if ui.button("update", line, &label, Tone::Primary) {
            app.download_update();
        }
    } else {
        let status = match &app.update {
            UpdateState::Checking => "Проверяю обновления".to_owned(),
            UpdateState::UpToDate => format!("Версия {}, она последняя", install::VERSION),
            UpdateState::Downloading => "Скачиваю и проверяю подпись".to_owned(),
            UpdateState::Failed(error) => format!("Не вышло: {error}"),
            _ => format!("Версия {}", install::VERSION),
        };
        let color = if matches!(app.update, UpdateState::Failed(_)) { pal.danger } else { pal.ink_weak };
        ui.line(&status, Face::Body, 12.5, x, line.center().y, width - 92.0, color);
        let busy = matches!(app.update, UpdateState::Checking | UpdateState::Downloading);
        if !busy && ui.button("check-update", Rect::new(line.x1 - 88.0, line.y0, line.x1, line.y1), "Проверить", Tone::Quiet) {
            app.check_update(true);
        }
    }
    let auto = Rect::from_origin_size((x, rect.y0 + 112.0), (width, 28.0));
    ui.toggle("auto-update", auto, "Проверять раз в день", &mut app.settings.auto_update);
}
