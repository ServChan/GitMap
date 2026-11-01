//! Обзор всей папки: что менялось за неделю и где что не так.
//! Главное здесь не цифры, а список проектов, с которыми стоит что-то сделать.

use crate::anim::{self, ease_out};
use crate::app::App;
use crate::fmt;
use crate::git::Health;
use crate::text::{self, Face};
use crate::theme::{fade, mix};
use crate::ui::{Tone, Ui, id};
use std::path::PathBuf;
use vello::kurbo::{Affine, Circle, Point, Rect, RoundedRect};

const ROW: f64 = 38.0;
const PREVIEW: usize = 6;

/// Строка раздела: проект, что с ним и насколько это важно для сортировки.
struct Entry {
    repo: usize,
    note: String,
    weight: usize,
}

struct Section {
    key: &'static str,
    title: &'static str,
    /// Что делать: одна короткая фраза под заголовком.
    hint: &'static str,
    color: usize,
    entries: Vec<Entry>,
}

fn collect(app: &App) -> Vec<Section> {
    let healthy = || app.repos.iter().enumerate().filter(|(_, r)| !r.bare).filter_map(|(i, r)| r.health.as_ref().map(|h| (i, h)));
    let build = |key, title, hint, color, pick: &dyn Fn(&Health) -> Option<(String, usize)>| {
        let mut entries: Vec<Entry> =
            healthy().filter_map(|(repo, health)| pick(health).map(|(note, weight)| Entry { repo, note, weight })).collect();
        entries.sort_by_key(|e| (std::cmp::Reverse(e.weight), app.repos[e.repo].name.to_lowercase()));
        Section { key, title, hint, color, entries }
    };
    vec![
        build("unpushed", "Не отправлено", "Коммиты есть только здесь: пока их не отправили, на сервере их нет.", 0, &|h| {
            (h.unpushed > 0).then(|| (fmt::count(h.unpushed, "коммит", "коммита", "коммитов"), h.unpushed))
        }),
        build("dirty", "Не закоммичено", "Работа лежит в файлах, в истории её ещё нет.", 4, &|h| {
            (h.dirty > 0).then(|| (fmt::count(h.dirty, "файл", "файла", "файлов"), h.dirty))
        }),
        build("local", "Только на этом диске", "Нет удалённого репозитория: эта копия единственная.", 3, &|h| {
            (!h.has_remote).then(|| ("нет remote".to_owned(), 1))
        }),
        build("stale", "Заброшенные ветки", "Не влиты и не менялись больше трёх месяцев.", 7, &|h| {
            (!h.stale_branches.is_empty())
                .then(|| (fmt::count(h.stale_branches.len(), "ветка", "ветки", "веток"), h.stale_branches.len()))
        }),
    ]
}

pub fn show(ui: &mut Ui, app: &mut App, free: Rect) {
    let pal = ui.pal;
    let now = ui.now();
    let t = (now - app.overview_opened) as f32;
    if t < 1.2 {
        ui.animating = true;
    }
    let area = Rect::new(free.x0 + 34.0, 44.0, free.x1 - 14.0, free.y1 - 14.0);
    let projects = || app.repos.iter().filter(|r| !r.bare);
    let total = projects().count();
    let ready = projects().filter(|r| r.health.is_some()).count();
    let week: usize = projects().filter_map(|r| r.health.as_ref()).map(|h| h.week_commits).sum();
    let active = projects().filter(|r| r.health.as_ref().is_some_and(|h| h.week_commits > 0)).count();
    let sections = collect(app);
    let name = app
        .settings
        .root
        .as_ref()
        .and_then(|r| r.file_name())
        .map_or_else(|| "Проекты".to_owned(), |n| n.to_string_lossy().into_owned());

    let wide = area.width() > 900.0;
    let side_width = if wide { (area.width() * 0.34).clamp(280.0, 380.0) } else { 0.0 };
    let main_width = area.width() - side_width - if wide { 40.0 } else { 12.0 };
    let mut open: Option<PathBuf> = None;
    let mut expand: Option<&'static str> = None;

    ui.scroll("overview", area, |ui, offset| {
        let mut y = area.y0 + 6.0 - offset;
        let title = ui.fonts.layout(&name, Face::Display, 76.0, None);
        let rise = (1.0 - ease_out(t / 0.5)) as f64 * title.height() as f64;
        ui.clip(Rect::new(area.x0 - 8.0, y.max(area.y0), area.x1, y + title.height() as f64 + 2.0), 0.0);
        text::draw(ui.scene, &title, Affine::translate((area.x0, y + rise)), pal.ink);
        ui.unclip();
        y += title.height() as f64 + 4.0;
        let appear = ease_out((t - 0.15) / 0.4);
        let sentence = if ready < total {
            format!("Проверяю проекты: {ready} из {total}.")
        } else if week == 0 {
            format!("{}. За неделю коммитов не было.", fmt::count(total, "проект", "проекта", "проектов"))
        } else {
            format!(
                "{}. За неделю {} в {}.",
                fmt::count(total, "проект", "проекта", "проектов"),
                fmt::count(week, "коммит", "коммита", "коммитов"),
                fmt::count(active, "проекте", "проектах", "проектах"),
            )
        };
        ui.line(&sentence, Face::Body, 17.0, area.x0 + 2.0, y + 11.0, area.width(), fade(pal.ink_weak, appear));
        y += 44.0;

        // Левая колонка шире: то, что требует действий, важнее ленты активности.
        let top = y;
        let mut row_index = 0;
        let quiet = sections.iter().all(|s| s.entries.is_empty());
        if quiet && ready == total {
            y += ui.paragraph("Всё отправлено и закоммичено. Выбери проект слева.", Face::Body, 16.0, Point::new(area.x0, y), main_width, pal.ink);
        }
        for section in &sections {
            if section.entries.is_empty() {
                continue;
            }
            let open_all = app.overview_expanded.contains(section.key);
            let shown = if open_all { section.entries.len() } else { section.entries.len().min(PREVIEW) };
            let color = pal.lines[section.color];
            ui.fill(&Circle::new((area.x0 + 5.0, y + 13.0), 5.5), pal.sheet);
            ui.stroke(&Circle::new((area.x0 + 5.0, y + 13.0), 5.5), 3.0, color);
            ui.line(section.title, Face::Semi, 17.0, area.x0 + 14.0, y + 13.0, main_width - 70.0, pal.ink);
            ui.line_right(&fmt::number(section.entries.len()), Face::Semi, 14.0, area.x0 + main_width, y + 13.0, pal.ink_weak);
            ui.line(section.hint, Face::Body, 13.5, area.x0 + 14.0, y + 35.0, main_width - 14.0, pal.ink_weak);
            y += 52.0;
            for entry in &section.entries[..shown] {
                let wait = 0.2 + (row_index as f32 * 0.018).min(0.5);
                row_index += 1;
                let appear = ease_out((t - wait) / 0.3);
                let row = Rect::new(area.x0 - 8.0, y, area.x0 + main_width + 8.0, y + ROW);
                if row.y1 > area.y0 && row.y0 < area.y1 {
                    let repo = &app.repos[entry.repo];
                    let key = id(("overview-row", section.key, &repo.path));
                    let response = ui.interact(key, row);
                    let hover = ui.tween((key, "hover"), response.hovered, anim::SNAP);
                    ui.fill(&RoundedRect::from_rect(row.inset((0.0, -1.0)), 9.0), fade(pal.sheet, 0.55 + 0.45 * hover));
                    let shift = (1.0 - appear) as f64 * 16.0 + hover as f64 * 3.0;
                    let center = row.center().y;
                    ui.line(&repo.name, Face::Body, 15.0, row.x0 + 14.0 + shift, center, row.width() * 0.56, fade(pal.ink, appear));
                    let group = if repo.group.is_empty() { String::new() } else { repo.group.clone() };
                    ui.line_right(&entry.note, Face::Semi, 13.5, row.x1 - 12.0, center, fade(mix(color, pal.ink, 0.25), appear));
                    ui.line(&group, Face::Body, 12.5, row.x0 + row.width() * 0.6, center, row.width() * 0.22, fade(pal.ink_weak, appear));
                    if response.clicked {
                        open = Some(repo.path.clone());
                    }
                    if response.focused {
                        ui.focus_ring(row, 9.0);
                    }
                }
                y += ROW;
            }
            if shown < section.entries.len() {
                let label = format!("Показать все {}", fmt::number(section.entries.len()));
                let rect = Rect::from_origin_size((area.x0 - 2.0, y + 4.0), (190.0, 30.0));
                if ui.button(("overview-more", section.key), rect, &label, Tone::Quiet) {
                    expand = Some(section.key);
                }
                y += 34.0;
            }
            y += 22.0;
        }

        // Правая колонка: где шла работа и что родственно.
        let side_x = if wide { area.x0 + main_width + 28.0 } else { area.x0 };
        let mut side_y = if wide { top } else { y + 8.0 };
        let side = if wide { side_width } else { main_width };
        side_y = week_block(ui, app, side_x, side_y, side, t, &mut open);
        side_y = families_block(ui, app, side_x, side_y + 22.0, side, &mut open);
        ui.scroll_content("overview", y.max(side_y) + 20.0 + offset - area.y0);
    });
    if let Some(key) = expand {
        app.overview_expanded.insert(key);
    }
    if let Some(path) = open {
        app.open_repo(path, false);
    }
}

fn week_block(ui: &mut Ui, app: &App, x: f64, mut y: f64, width: f64, t: f32, open: &mut Option<PathBuf>) -> f64 {
    let pal = ui.pal;
    let mut ranked: Vec<(usize, usize)> = app
        .repos
        .iter()
        .enumerate()
        .filter(|(_, r)| !r.bare)
        .filter_map(|(i, r)| r.health.as_ref().filter(|h| h.week_commits > 0).map(|h| (i, h.week_commits)))
        .collect();
    if ranked.is_empty() {
        return y;
    }
    ranked.sort_by_key(|&(i, n)| (std::cmp::Reverse(n), app.repos[i].name.to_lowercase()));
    let peak = ranked[0].1 as f64;
    ui.line("За неделю", Face::Semi, 17.0, x, y + 13.0, width, pal.ink);
    y += 32.0;
    for (position, &(repo, count)) in ranked.iter().take(10).enumerate() {
        let row = Rect::new(x, y, x + width, y + 34.0);
        let key = id(("week-row", repo));
        let response = ui.interact(key, row);
        let hover = ui.tween((key, "hover"), response.hovered, anim::SNAP);
        // Полоски вырастают волной, длина показывает долю от самого активного проекта.
        let grow = ease_out((t - 0.25 - position as f32 * 0.04) / 0.45) as f64;
        let bar = Rect::new(x, y + 27.0, x + (width - 40.0) * (count as f64 / peak) * grow, y + 30.0);
        ui.fill(&RoundedRect::from_rect(bar, 1.5), pal.lines[position % pal.lines.len()]);
        ui.line(&app.repos[repo].name, Face::Body, 14.0, x + hover as f64 * 3.0, y + 13.0, width - 46.0, pal.ink);
        ui.line_right(&fmt::number(count), Face::Semi, 13.0, x + width, y + 13.0, pal.ink_weak);
        if response.clicked {
            *open = Some(app.repos[repo].path.clone());
        }
        if response.focused {
            ui.focus_ring(row, 6.0);
        }
        y += 38.0;
    }
    y
}

/// Проекты с общим корневым коммитом: клоны, форки и сборки под другую версию.
fn families_block(ui: &mut Ui, app: &App, x: f64, mut y: f64, width: f64, open: &mut Option<PathBuf>) -> f64 {
    let pal = ui.pal;
    if app.families.is_empty() {
        return y;
    }
    ui.line("Родственные проекты", Face::Semi, 17.0, x, y + 13.0, width, pal.ink);
    ui.line("Общая история: клоны и сборки под другие версии.", Face::Body, 13.0, x, y + 34.0, width, pal.ink_weak);
    y += 52.0;
    for (index, family) in app.families.iter().take(12).enumerate() {
        let color = pal.lines[index % pal.lines.len()];
        let height = family.len() as f64 * 28.0;
        // Семья нарисована как короткая линия: проекты на ней это станции.
        ui.path(&[Point::new(x + 7.0, y + 14.0), Point::new(x + 7.0, y + height - 14.0)], 4.0, color);
        for (row_index, &repo) in family.iter().enumerate() {
            let row = Rect::new(x + 18.0, y + row_index as f64 * 28.0, x + width, y + (row_index + 1) as f64 * 28.0);
            ui.fill(&Circle::new((x + 7.0, row.center().y), 4.5), pal.sheet);
            ui.stroke(&Circle::new((x + 7.0, row.center().y), 4.5), 2.0, color);
            let key = id(("family-row", repo));
            let response = ui.interact(key, row);
            let hover = ui.tween((key, "hover"), response.hovered, anim::SNAP);
            ui.fill(&RoundedRect::from_rect(row, 7.0), fade(pal.sheet, hover));
            ui.line(&app.repos[repo].name, Face::Body, 14.0, row.x0 + 8.0, row.center().y, row.width() * 0.58, pal.ink);
            // Длинный путь обрезается, чтобы не наехать на имя.
            let group = ui.fonts.fit(&app.repos[repo].group, Face::Body, 12.5, (row.width() * 0.36) as f32);
            let at = (row.x1 - 8.0 - group.width() as f64, row.center().y - group.height() as f64 / 2.0);
            text::draw(ui.scene, &group, Affine::translate(at), pal.ink_weak);
            if response.clicked {
                *open = Some(app.repos[repo].path.clone());
            }
        }
        y += height + 10.0;
    }
    y
}
