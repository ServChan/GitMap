//! Состояние окна, фоновые задачи и левая панель с указателем проектов.

use crate::git::{self, Details, Graph, Idx, RefKind, Summary, branch_name};
use crate::layout::{self, LineKind, Map, Options};
use crate::map::{self, Cam, Tween};
use crate::theme::{self, Palette, semibold};
use crate::{fmt, panel, scan};
use eframe::egui::{
    self, Color32, CornerRadius, FontId, Galley, Painter, Rect, Response, Sense, Stroke, StrokeKind,
    pos2, text::{LayoutJob, TextWrapping}, vec2,
};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex};

const INDEX_WIDTH: f32 = 288.0;
const SUMMARY_THREADS: usize = 3;
const STATE_KEY: &str = "gitmap";

#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Sort {
    Name,
    Recent,
}

#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ThemeChoice {
    System,
    Light,
    Dark,
}

impl ThemeChoice {
    fn next(self) -> Self {
        match self {
            Self::System => Self::Light,
            Self::Light => Self::Dark,
            Self::Dark => Self::System,
        }
    }
    fn label(self) -> &'static str {
        match self {
            Self::System => "Тема: как в системе",
            Self::Light => "Тема: бумага",
            Self::Dark => "Тема: ночная",
        }
    }
    fn preference(self) -> egui::ThemePreference {
        match self {
            Self::System => egui::ThemePreference::System,
            Self::Light => egui::ThemePreference::Light,
            Self::Dark => egui::ThemePreference::Dark,
        }
    }
}

#[derive(Serialize, Deserialize)]
struct Saved {
    root: Option<PathBuf>,
    sort: Sort,
    theme: ThemeChoice,
    collapsed: HashSet<String>,
    show_merged: bool,
    compress: bool,
    show_tags: bool,
}

impl Default for Saved {
    fn default() -> Self {
        Self {
            root: None,
            sort: Sort::Name,
            theme: ThemeChoice::System,
            collapsed: HashSet::new(),
            show_merged: true,
            compress: true,
            show_tags: true,
        }
    }
}

#[derive(Clone, Copy, PartialEq)]
pub enum Selection {
    None,
    Commit(Idx),
    Line(u32),
}

struct Repo {
    path: PathBuf,
    name: String,
    group: String,
    summary: Option<Result<Summary, String>>,
}

/// Открытый репозиторий со всем, что нужно схеме и правой панели.
pub struct Session {
    pub graph: Arc<Graph>,
    pub map: Map,
    pub cam: Cam,
    pub tween: Option<Tween>,
    pub needs_framing: bool,
    pub auto_frame: bool,
    pub framed_size: egui::Vec2,
    pub selection: Selection,
    /// Откуда пришли к коммиту: кнопка «Назад» вернёт к этой ветке.
    pub back: Option<u32>,
    pub title: String,
    pub caption: String,
    /// Подписи у станций: коммит и текст, упорядочено по x.
    pub labels: HashMap<Idx, String>,
    pub label_order: Vec<Idx>,
    pub search: String,
    pub search_hits: Vec<Idx>,
    pub details: Option<(Idx, Result<Details, String>)>,
    pub want_details: Option<Idx>,
    pub want_camera: Option<Cam>,
    pub line_stats: Option<(u32, LineStats)>,
    pub show_all_merged: bool,
}

pub struct LineStats {
    pub ahead: usize,
    pub behind: usize,
    pub authors: Vec<(String, usize)>,
    pub merged_into: Option<String>,
}

impl Session {
    fn new(graph: Arc<Graph>, map: Map, path: &Path, show_tags: bool) -> Self {
        let title = path.file_name().map_or_else(|| path.display().to_string(), |n| n.to_string_lossy().into_owned());
        let branches: HashSet<&str> = graph
            .refs
            .iter()
            .filter(|r| r.kind != RefKind::Tag)
            .map(|r| branch_name(r.kind, &r.name))
            .collect();
        let active = map.lines.iter().filter(|l| l.kind == LineKind::Active).count();
        let oldest = graph.commits.iter().map(|c| c.time).min().unwrap_or_default();
        let mut caption = format!(
            "{}. {}, {}",
            map.lines[0].name,
            fmt::count(graph.commits.len(), "коммит", "коммита", "коммитов"),
            fmt::count(branches.len(), "ветка", "ветки", "веток"),
        );
        if active > 0 {
            caption.push_str(&format!(", {active} в работе"));
        }
        caption.push_str(&format!(". Начало: {}", fmt::month_year(oldest)));
        if graph.truncated {
            caption.push_str(". Показана новая часть истории");
        }
        let mut session = Self {
            graph,
            map,
            cam: Cam { x: 0.0, y: 0.0, zoom: 1.0 },
            tween: None,
            needs_framing: true,
            auto_frame: true,
            framed_size: egui::Vec2::ZERO,
            selection: Selection::None,
            back: None,
            title,
            caption,
            labels: HashMap::new(),
            label_order: Vec::new(),
            search: String::new(),
            search_hits: Vec::new(),
            details: None,
            want_details: None,
            want_camera: None,
            line_stats: None,
            show_all_merged: false,
        };
        session.rebuild_labels(show_tags);
        session
    }

    /// Указатели, которые не совпадают с именем своей линии, подписываются у станции.
    fn rebuild_labels(&mut self, show_tags: bool) {
        let mut grouped: HashMap<Idx, Vec<&str>> = HashMap::new();
        for r in &self.graph.refs {
            if r.kind == RefKind::Tag && !show_tags {
                continue;
            }
            let line = &self.map.lines[self.map.line_of[r.commit as usize] as usize];
            let tip = line.commits[line.commits.len() - 1];
            if r.commit == tip && branch_name(r.kind, &r.name) == line.name {
                continue;
            }
            grouped.entry(r.commit).or_default().push(&r.name);
        }
        self.labels = grouped.into_iter().map(|(commit, names)| (commit, names.join(", "))).collect();
        self.label_order = self.labels.keys().copied().collect();
        self.label_order.sort_by(|&a, &b| self.map.x[a as usize].total_cmp(&self.map.x[b as usize]));
    }

    pub fn select(&mut self, selection: Selection) {
        self.back = match (self.selection, selection) {
            (Selection::Line(line), Selection::Commit(_)) => Some(line),
            (Selection::Commit(_), Selection::Commit(_)) => self.back,
            _ => None,
        };
        self.selection = selection;
        self.details = None;
        match selection {
            Selection::Commit(commit) => self.want_details = Some(commit),
            Selection::Line(line) => self.compute_line_stats(line),
            Selection::None => {}
        }
    }

    fn compute_line_stats(&mut self, id: u32) {
        let line = &self.map.lines[id as usize];
        let graph = &self.graph;
        let tip = line.commits[line.commits.len() - 1];
        let from_tip = layout::reachable(graph, tip);
        let in_main = &self.map.in_main;
        let ahead = from_tip.iter().zip(in_main).filter(|(tip, main)| **tip && !**main).count();
        let behind = from_tip.iter().zip(in_main).filter(|(tip, main)| !**tip && **main).count();
        let mut authors: HashMap<&str, usize> = HashMap::new();
        for &commit in &line.commits {
            *authors.entry(&graph.commits[commit as usize].author).or_default() += 1;
        }
        let mut authors: Vec<(String, usize)> = authors.into_iter().map(|(a, n)| (a.to_owned(), n)).collect();
        authors.sort_by_key(|(name, n)| (std::cmp::Reverse(*n), name.clone()));
        let merged_into = self
            .map
            .links
            .iter()
            .filter(|l| !l.fork && self.map.line_of[l.from as usize] == id)
            .max_by(|a, b| self.map.x[a.to as usize].total_cmp(&self.map.x[b.to as usize]))
            .map(|l| self.map.lines[self.map.line_of[l.to as usize] as usize].name.clone());
        self.line_stats = Some((id, LineStats { ahead, behind, authors, merged_into }));
    }

    pub fn run_search(&mut self) {
        let needle = self.search.trim().to_lowercase();
        self.search_hits.clear();
        if needle.is_empty() {
            return;
        }
        let graph = &self.graph;
        self.search_hits = (0..graph.commits.len() as Idx)
            .filter(|&i| {
                let c = &graph.commits[i as usize];
                c.subject.to_lowercase().contains(&needle)
                    || c.author.to_lowercase().contains(&needle)
                    || c.id.to_hex().to_string().starts_with(&needle)
            })
            .collect();
    }
}

enum Msg {
    Found { generation: u64, path: PathBuf },
    ScanDone { generation: u64 },
    Summary { path: PathBuf, result: Result<Summary, String> },
    Loaded { generation: u64, path: PathBuf, keep_camera: bool, result: Result<Option<(Arc<Graph>, Map)>, String> },
    Relaid { generation: u64, options: Options, map: Option<Map> },
    Details { commit: Idx, result: Result<Details, String> },
    Picked(PathBuf),
}

pub struct App {
    tx: Sender<Msg>,
    rx: Receiver<Msg>,
    summaries: Sender<PathBuf>,
    root: Option<PathBuf>,
    repos: Vec<Repo>,
    scanning: bool,
    scan_generation: u64,
    scan_cancel: Arc<AtomicBool>,
    filter: String,
    sort: Sort,
    theme: ThemeChoice,
    collapsed: HashSet<String>,
    session: Option<Session>,
    empty_repo: Option<PathBuf>,
    open_path: Option<PathBuf>,
    loading: bool,
    load_generation: u64,
    load_error: Option<String>,
    options: Options,
    show_tags: bool,
    /// Репозиторий, который надо открыть, как только сканирование его найдёт.
    open_when_found: Option<PathBuf>,
    /// Прокрутить список к проекту, открытому не щелчком по нему.
    reveal_open: bool,
    canvas: Rect,
}

pub struct StartArgs {
    pub path: Option<PathBuf>,
    pub theme: Option<&'static str>,
}

impl App {
    pub fn new(cc: &eframe::CreationContext<'_>, args: StartArgs) -> Self {
        theme::install(&cc.egui_ctx);
        let saved: Saved = cc.storage.and_then(|s| eframe::get_value(s, STATE_KEY)).unwrap_or_default();
        let theme = match args.theme {
            Some("light") => ThemeChoice::Light,
            Some("dark") => ThemeChoice::Dark,
            _ => saved.theme,
        };
        cc.egui_ctx.set_theme(theme.preference());

        let (tx, rx) = channel();
        let (summaries, queue) = channel::<PathBuf>();
        let queue = Arc::new(Mutex::new(queue));
        for _ in 0..SUMMARY_THREADS {
            let (queue, tx, ctx) = (queue.clone(), tx.clone(), cc.egui_ctx.clone());
            std::thread::spawn(move || {
                loop {
                    let next = queue.lock().ok().and_then(|q| q.recv().ok());
                    let Some(path) = next else { return };
                    let result = git::summary(&path);
                    let _ = tx.send(Msg::Summary { path, result });
                    ctx.request_repaint();
                }
            });
        }

        let mut app = Self {
            tx,
            rx,
            summaries,
            root: None,
            repos: Vec::new(),
            scanning: false,
            scan_generation: 0,
            scan_cancel: Arc::new(AtomicBool::new(false)),
            filter: String::new(),
            sort: saved.sort,
            theme,
            collapsed: saved.collapsed,
            session: None,
            empty_repo: None,
            open_path: None,
            loading: false,
            load_generation: 0,
            load_error: None,
            options: Options { show_merged: saved.show_merged, compress: saved.compress },
            show_tags: saved.show_tags,
            open_when_found: None,
            reveal_open: false,
            canvas: Rect::ZERO,
        };
        // Путь из командной строки: папка проектов или сам проект.
        match args.path {
            Some(path) if scan::is_repo(&path) => {
                let root = path.parent().map_or_else(|| path.clone(), Path::to_path_buf);
                app.open_when_found = Some(path);
                app.start_scan(root, &cc.egui_ctx);
            }
            Some(path) => app.start_scan(path, &cc.egui_ctx),
            None => {
                if let Some(root) = saved.root.filter(|r| r.is_dir()) {
                    app.start_scan(root, &cc.egui_ctx);
                }
            }
        }
        app
    }

    fn start_scan(&mut self, root: PathBuf, ctx: &egui::Context) {
        self.scan_cancel.store(true, Ordering::Relaxed);
        self.scan_cancel = Arc::new(AtomicBool::new(false));
        self.scan_generation += 1;
        self.root = Some(root.clone());
        self.repos.clear();
        self.session = None;
        self.empty_repo = None;
        self.open_path = None;
        self.load_error = None;
        self.scanning = true;
        let generation = self.scan_generation;
        let (cancel, tx, ctx) = (self.scan_cancel.clone(), self.tx.clone(), ctx.clone());
        std::thread::spawn(move || {
            scan::run(&root, &cancel, &|found| {
                let message = match found {
                    scan::Found::Repo(path) => Msg::Found { generation, path },
                    scan::Found::Done => Msg::ScanDone { generation },
                };
                let _ = tx.send(message);
                ctx.request_repaint();
            });
        });
    }

    fn open_repo(&mut self, path: PathBuf, keep_camera: bool, ctx: &egui::Context) {
        self.load_generation += 1;
        self.loading = true;
        self.load_error = None;
        self.open_path = Some(path.clone());
        self.empty_repo = None;
        let generation = self.load_generation;
        let options = self.options;
        let (tx, ctx) = (self.tx.clone(), ctx.clone());
        std::thread::spawn(move || {
            let result = git::load(&path).map(|graph| {
                let map = layout::build(&graph, options);
                map.map(|map| (Arc::new(graph), map))
            });
            let _ = tx.send(Msg::Loaded { generation, path, keep_camera, result });
            ctx.request_repaint();
        });
    }

    fn relayout(&mut self, ctx: &egui::Context) {
        let Some(session) = &self.session else { return };
        self.load_generation += 1;
        let generation = self.load_generation;
        let (graph, options) = (session.graph.clone(), self.options);
        let (tx, ctx) = (self.tx.clone(), ctx.clone());
        std::thread::spawn(move || {
            let map = layout::build(&graph, options);
            let _ = tx.send(Msg::Relaid { generation, options, map });
            ctx.request_repaint();
        });
    }

    fn pick_folder(&self, ctx: &egui::Context) {
        let (tx, ctx) = (self.tx.clone(), ctx.clone());
        let start = self.root.clone();
        std::thread::spawn(move || {
            let mut dialog = rfd::FileDialog::new().set_title("Папка с проектами");
            if let Some(start) = start {
                dialog = dialog.set_directory(start);
            }
            if let Some(path) = dialog.pick_folder() {
                let _ = tx.send(Msg::Picked(path));
                ctx.request_repaint();
            }
        });
    }

    fn drain(&mut self, ctx: &egui::Context) {
        while let Ok(message) = self.rx.try_recv() {
            match message {
                Msg::Found { generation, path } if generation == self.scan_generation => {
                    let root = self.root.clone().unwrap_or_default();
                    let name = path.file_name().map_or_else(|| path.display().to_string(), |n| n.to_string_lossy().into_owned());
                    let group = path
                        .parent()
                        .and_then(|parent| parent.strip_prefix(&root).ok())
                        .map(|rel| rel.to_string_lossy().replace('\\', "/"))
                        .unwrap_or_default();
                    let _ = self.summaries.send(path.clone());
                    if self.open_when_found.as_ref() == Some(&path) {
                        self.open_when_found = None;
                        self.reveal_open = true;
                        self.open_repo(path.clone(), false, ctx);
                    }
                    self.repos.push(Repo { path, name, group, summary: None });
                }
                Msg::ScanDone { generation } if generation == self.scan_generation => self.scanning = false,
                Msg::Summary { path, result } => {
                    if let Some(repo) = self.repos.iter_mut().find(|r| r.path == path) {
                        repo.summary = Some(result);
                    }
                }
                Msg::Loaded { generation, path, keep_camera, result } if generation == self.load_generation => {
                    self.loading = false;
                    match result {
                        Ok(Some((graph, map))) => {
                            let camera = self.session.as_ref().filter(|_| keep_camera).map(|s| s.cam);
                            let mut session = Session::new(graph, map, &path, self.show_tags);
                            if let Some(camera) = camera {
                                session.cam = camera;
                                session.needs_framing = false;
                                session.auto_frame = false;
                            }
                            self.session = Some(session);
                        }
                        Ok(None) => {
                            self.session = None;
                            self.empty_repo = Some(path);
                        }
                        Err(error) => {
                            self.session = None;
                            self.load_error = Some(error);
                        }
                    }
                }
                Msg::Relaid { generation, options, map } if generation == self.load_generation => {
                    if let (Some(session), Some(map), true) = (&mut self.session, map, options == self.options) {
                        session.map = map;
                        session.needs_framing = true;
                        if matches!(session.selection, Selection::Line(_)) {
                            session.selection = Selection::None;
                        }
                        session.line_stats = None;
                        session.rebuild_labels(self.show_tags);
                    }
                }
                Msg::Details { commit, result } => {
                    if let Some(session) = &mut self.session
                        && session.selection == Selection::Commit(commit)
                    {
                        session.details = Some((commit, result));
                    }
                }
                Msg::Picked(path) => self.start_scan(path, ctx),
                _ => {}
            }
        }
    }

    fn fetch_details(&mut self, ctx: &egui::Context) {
        let Some(session) = &mut self.session else { return };
        let Some(commit) = session.want_details.take() else { return };
        let (path, id) = (session.graph.path.clone(), session.graph.commits[commit as usize].id);
        let (tx, ctx) = (self.tx.clone(), ctx.clone());
        std::thread::spawn(move || {
            let result = git::details(&path, id);
            let _ = tx.send(Msg::Details { commit, result });
            ctx.request_repaint();
        });
    }

    fn handle_shortcuts(&mut self, ctx: &egui::Context) {
        if ctx.input(|i| i.key_pressed(egui::Key::F5))
            && let Some(path) = self.open_path.clone()
        {
            self.open_repo(path, true, ctx);
        }
        if let Some(session) = &mut self.session {
            if ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
                if let Some(line) = session.back {
                    session.select(Selection::Line(line));
                } else {
                    session.select(Selection::None);
                }
            }
            map::handle_keys(ctx, session, self.canvas);
        }
        let dropped = ctx.input(|i| i.raw.dropped_files.first().map(|f| f.path().to_path_buf()));
        if let Some(path) = dropped.filter(|p| p.is_dir()) {
            self.start_scan(path, ctx);
        }
    }

    fn index_panel(&mut self, ui: &mut egui::Ui, pal: &Palette) {
        let ctx = ui.ctx().clone();
        let full = ui.max_rect();
        ui.painter().vline(full.max.x - 0.5, full.y_range(), Stroke::new(1.0, pal.ink));

        egui::Frame::new().inner_margin(egui::Margin { left: 16, right: 16, top: 16, bottom: 8 }).show(ui, |ui| {
            ui.set_width(INDEX_WIDTH - 33.0);
            match &self.root {
                Some(root) => {
                    let name = root.file_name().map_or_else(|| root.display().to_string(), |n| n.to_string_lossy().into_owned());
                    let font = FontId::new(28.0, theme::display());
                    let galley = fit(ui.painter(), name, font, pal.ink, INDEX_WIDTH - 33.0);
                    let (rect, _) = ui.allocate_exact_size(galley.size(), Sense::hover());
                    ui.painter().galley(rect.min, galley, pal.ink);
                    let path = root.display().to_string();
                    let galley = fit(ui.painter(), path.clone(), FontId::proportional(12.5), pal.ink_weak, INDEX_WIDTH - 33.0);
                    let (rect, response) = ui.allocate_exact_size(galley.size(), Sense::hover());
                    ui.painter().galley(rect.min, galley, pal.ink_weak);
                    response.on_hover_text(path);
                }
                None => {
                    ui.label(egui::RichText::new("Проекты").heading());
                }
            }
            ui.add_space(4.0);
            if ui.button(if self.root.is_some() { "Сменить папку" } else { "Выбрать папку" }).clicked() {
                self.pick_folder(&ctx);
            }
            ui.add_space(2.0);
            ui.add(
                egui::TextEdit::singleline(&mut self.filter)
                    .hint_text("Найти проект")
                    .desired_width(f32::INFINITY),
            );
        });

        // Шапка таблицы: щелчок меняет порядок строк.
        let header = ui.allocate_exact_size(vec2(INDEX_WIDTH - 1.0, 24.0), Sense::hover()).0;
        ui.painter().hline(header.x_range(), header.max.y - 0.5, Stroke::new(1.0, pal.ink));
        let small = FontId::proportional(12.5);
        for (rect, label, sort, align) in [
            (Rect::from_min_size(header.min + vec2(16.0, 0.0), vec2(120.0, 24.0)), "Проект", Sort::Name, egui::Align2::LEFT_CENTER),
            (Rect::from_min_size(pos2(header.max.x - 96.0, header.min.y), vec2(80.0, 24.0)), "Изменён", Sort::Recent, egui::Align2::RIGHT_CENTER),
        ] {
            let response = ui.interact(rect, ui.scope_id().with(label), Sense::click());
            response.widget_info(|| {
                egui::WidgetInfo::selected(egui::Role::Button, true, self.sort == sort, format!("Сортировать: {label}"))
            });
            let color = if self.sort == sort || response.hovered() { pal.ink } else { pal.ink_weak };
            let font = if self.sort == sort { FontId::new(12.5, semibold()) } else { small.clone() };
            let anchor = if align == egui::Align2::LEFT_CENTER { rect.left_center() } else { rect.right_center() };
            ui.painter().text(anchor, align, label, font, color);
            if response.has_focus() {
                ui.painter().rect_stroke(rect, CornerRadius::ZERO, Stroke::new(2.0, pal.ink), StrokeKind::Inside);
            }
            if response.clicked() {
                self.sort = sort;
            }
        }

        ui.painter().text(pos2(header.max.x - 78.0, header.center().y), egui::Align2::RIGHT_CENTER, "Веток", small.clone(), pal.ink_weak);

        let footer_height = 62.0;
        let list_height = (full.max.y - ui.cursor().min.y - footer_height).max(40.0);
        let needle = self.filter.trim().to_lowercase();
        let groups = self.grouped(&needle);
        let mut open = None;
        let mut toggle = None;
        let mut reveal_done = false;
        egui::ScrollArea::vertical().max_height(list_height).min_scrolled_height(list_height).auto_shrink(false).show(ui, |ui| {
            ui.spacing_mut().item_spacing.y = 0.0;
            for (group, members) in &groups {
                let collapsed = needle.is_empty() && self.collapsed.contains(group);
                if !group.is_empty() {
                    let title = group.clone();
                    let count = fmt::number(members.len());
                    let label = format!("Папка {title}, проектов: {count}");
                    let response = row(ui, 32.0, false, &label, pal, |painter, rect, ink, weak| {
                        let chevron = rect.left_center() + vec2(20.0, 1.0);
                        let shape = if collapsed {
                            vec![chevron + vec2(-3.0, -5.0), chevron + vec2(3.0, 0.0), chevron + vec2(-3.0, 5.0)]
                        } else {
                            vec![chevron + vec2(-5.0, -3.0), chevron + vec2(5.0, -3.0), chevron + vec2(0.0, 3.0)]
                        };
                        painter.add(egui::Shape::convex_polygon(shape, ink, Stroke::NONE));
                        let name = fit(painter, title.clone(), FontId::new(13.5, semibold()), ink, rect.width() - 84.0);
                        painter.galley(rect.left_center() + vec2(32.0, -name.size().y / 2.0), name, ink);
                        painter.text(rect.right_center() - vec2(16.0, 0.0), egui::Align2::RIGHT_CENTER, count.as_str(), FontId::proportional(12.5), weak);
                    });
                    if response.clicked() {
                        toggle = Some(group.clone());
                    }
                }
                if collapsed {
                    continue;
                }
                for &at in members {
                    let repo = &self.repos[at];
                    let selected = self.open_path.as_ref() == Some(&repo.path);
                    let (age, branches) = match &repo.summary {
                        Some(Ok(summary)) => (summary.last_commit.map(fmt::age).unwrap_or_default(), summary.branches),
                        _ => (String::new(), 0),
                    };
                    let response = row(ui, 28.0, selected, &repo.name, pal, |painter, rect, ink, weak| {
                        let age_width = 58.0;
                        let name = fit(painter, repo.name.clone(), FontId::proportional(14.0), ink, rect.width() - 32.0 - age_width - 28.0);
                        painter.galley(rect.left_center() + vec2(16.0, -name.size().y / 2.0), name, ink);
                        painter.text(rect.right_center() - vec2(16.0, 0.0), egui::Align2::RIGHT_CENTER, &age, FontId::proportional(12.5), weak);
                        if branches > 1 {
                            let x = rect.max.x - 16.0 - age_width - 4.0;
                            painter.text(pos2(x, rect.center().y), egui::Align2::RIGHT_CENTER, branches.to_string(), FontId::new(12.5, semibold()), weak);
                        }
                    });
                    if response.clicked() {
                        open = Some(repo.path.clone());
                    }
                    if selected && self.reveal_open {
                        response.scroll_to_me(Some(egui::Align::Center));
                        reveal_done = true;
                    }
                }
            }
            if self.repos.is_empty() && !self.scanning && self.root.is_some() {
                ui.add_space(16.0);
                ui.horizontal(|ui| {
                    ui.add_space(16.0);
                    ui.label(egui::RichText::new("В этой папке нет git-репозиториев.").color(pal.ink_weak));
                });
            }
        });
        if reveal_done {
            self.reveal_open = false;
        }
        if let Some(path) = open {
            self.open_repo(path, false, &ctx);
        }
        if let Some(group) = toggle
            && !self.collapsed.remove(&group)
        {
            self.collapsed.insert(group);
        }

        // Подвал: итог списка и тема.
        let foot = Rect::from_min_max(pos2(full.min.x, full.max.y - footer_height), pos2(full.max.x - 1.0, full.max.y));
        ui.painter().rect_filled(foot, CornerRadius::ZERO, pal.paper);
        ui.painter().hline(foot.x_range(), foot.min.y, Stroke::new(1.0, pal.ink));
        let status = if self.scanning {
            format!("Ищу репозитории. Найдено: {}", self.repos.len())
        } else {
            fmt::count(self.repos.len(), "репозиторий", "репозитория", "репозиториев")
        };
        ui.painter().text(foot.min + vec2(16.0, 10.0), egui::Align2::LEFT_TOP, status, small, pal.ink_weak);
        let button = Rect::from_min_size(foot.min + vec2(16.0, 28.0), vec2(180.0, 26.0));
        let toggle_theme = ui.interact(button, ui.scope_id().with("theme"), Sense::click());
        let label = self.theme.label();
        toggle_theme.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, label));
        ui.painter().text(button.left_center(), egui::Align2::LEFT_CENTER, label, FontId::proportional(13.5), pal.ink);
        if toggle_theme.hovered() {
            ui.painter().hline(button.min.x..=button.min.x + 130.0, button.center().y + 9.0, Stroke::new(1.0, pal.ink));
        }
        if toggle_theme.has_focus() {
            ui.painter().rect_stroke(button, CornerRadius::ZERO, Stroke::new(2.0, pal.ink), StrokeKind::Outside);
        }
        if toggle_theme.clicked() {
            self.theme = self.theme.next();
            ctx.set_theme(self.theme.preference());
        }
    }

    /// Группы с индексами строк. Корень первым, служебные папки на `_` в конце.
    fn grouped(&self, needle: &str) -> Vec<(String, Vec<usize>)> {
        let mut groups: HashMap<&str, Vec<usize>> = HashMap::new();
        for (i, repo) in self.repos.iter().enumerate() {
            if needle.is_empty() || repo.name.to_lowercase().contains(needle) {
                groups.entry(&repo.group).or_default().push(i);
            }
        }
        let time = |i: usize| match &self.repos[i].summary {
            Some(Ok(summary)) => summary.last_commit.unwrap_or_default(),
            _ => 0,
        };
        let mut list: Vec<(String, Vec<usize>)> = groups
            .into_iter()
            .map(|(group, mut members)| {
                match self.sort {
                    Sort::Name => members.sort_by_key(|&i| self.repos[i].name.to_lowercase()),
                    Sort::Recent => members.sort_by_key(|&i| (std::cmp::Reverse(time(i)), self.repos[i].name.to_lowercase())),
                }
                (group.to_owned(), members)
            })
            .collect();
        list.sort_by_key(|(group, _)| (group.starts_with('_'), !group.is_empty(), group.to_lowercase()));
        list
    }

    fn central(&mut self, ui: &mut egui::Ui, pal: &Palette) {
        let area = ui.available_rect_before_wrap();
        self.canvas = map::canvas_rect(area);
        let title_of = |path: &Option<PathBuf>| {
            path.as_ref().and_then(|p| p.file_name()).map_or_else(String::new, |n| n.to_string_lossy().into_owned())
        };
        if let Some(error) = &self.load_error {
            let title = title_of(&self.open_path);
            let body = format!("Не удалось прочитать репозиторий: {error}");
            map::placeholder(ui, pal, &title, &body, None);
        } else if let Some(session) = &mut self.session {
            map::show(ui, session, pal);
        } else if self.loading {
            map::placeholder(ui, pal, &title_of(&self.open_path), "Читаю историю.", None);
        } else if self.empty_repo.is_some() {
            let title = title_of(&self.open_path);
            map::placeholder(ui, pal, &title, "В этом репозитории пока нет коммитов. После первого коммита здесь появится схема.", None);
        } else if self.root.is_none() {
            let body = "Выбери папку с проектами. Программа найдёт в ней все git-репозитории и нарисует ветки каждого как схему метро. Папку можно просто перетащить в окно.";
            if map::placeholder(ui, pal, "Карта веток", body, Some("Выбрать папку")) {
                self.pick_folder(ui.ctx());
            }
        } else if self.scanning && self.repos.is_empty() {
            map::placeholder(ui, pal, "Карта веток", "Ищу репозитории.", None);
        } else if self.repos.is_empty() {
            let body = "В этой папке нет git-репозиториев. Выбери другую папку.";
            if map::placeholder(ui, pal, "Пусто", body, Some("Выбрать папку")) {
                self.pick_folder(ui.ctx());
            }
        } else {
            let body = format!(
                "{}. Выбери проект слева, чтобы открыть его схему.",
                fmt::count(self.repos.len(), "репозиторий найден", "репозитория найдено", "репозиториев найдено")
            );
            map::placeholder(ui, pal, "Карта веток", &body, None);
        }
    }
}

impl eframe::App for App {
    #[cfg(debug_assertions)]
    fn raw_input_hook(&mut self, _ctx: &egui::Context, raw_input: &mut egui::RawInput) {
        crate::shot::inject(raw_input);
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        #[cfg(debug_assertions)]
        crate::shot::tick(&ctx);
        self.drain(&ctx);
        self.handle_shortcuts(&ctx);
        let pal = theme::palette(ui);
        let frame = egui::Frame::new().fill(pal.paper);

        egui::Panel::left("index")
            .exact_size(INDEX_WIDTH)
            .resizable(false)
            .show_separator_line(false)
            .frame(frame)
            .show(ui, |ui| self.index_panel(ui, pal));

        if let Some(session) = &mut self.session {
            let mut change = panel::Change::default();
            egui::Panel::right("details")
                .exact_size(panel::WIDTH)
                .resizable(false)
                .show_separator_line(false)
                .frame(frame)
                .show(ui, |ui| {
                    change = panel::show(ui, session, &mut self.options, &mut self.show_tags, self.canvas, pal);
                });
            if change.relayout {
                self.relayout(&ctx);
            }
            if change.labels
                && let Some(session) = &mut self.session
            {
                session.rebuild_labels(self.show_tags);
            }
        }
        egui::CentralPanel::no_frame().frame(frame).show(ui, |ui| self.central(ui, pal));
        self.fetch_details(&ctx);
    }

    fn save(&mut self, storage: &mut dyn eframe::Storage) {
        let saved = Saved {
            root: self.root.clone(),
            sort: self.sort,
            theme: self.theme,
            collapsed: self.collapsed.clone(),
            show_merged: self.options.show_merged,
            compress: self.options.compress,
            show_tags: self.show_tags,
        };
        eframe::set_value(storage, STATE_KEY, &saved);
    }
}

/// Текст в одну строку: не поместился, обрезается многоточием.
pub fn fit(painter: &Painter, text: String, font: FontId, color: Color32, width: f32) -> Arc<Galley> {
    let mut job = LayoutJob::simple_singleline(text, font, color);
    job.wrap = TextWrapping::truncate_at_width(width.max(1.0));
    painter.layout_job(job)
}

/// Строка списка: наведение, фокус, выбор. Выбранная строка инвертируется.
pub fn row(
    ui: &mut egui::Ui,
    height: f32,
    selected: bool,
    label: &str,
    pal: &Palette,
    paint: impl FnOnce(&Painter, Rect, Color32, Color32),
) -> Response {
    let (rect, response) = ui.allocate_exact_size(vec2(ui.available_width(), height), Sense::click());
    response.widget_info(|| egui::WidgetInfo::selected(egui::Role::Button, true, selected, label));
    let painter = ui.painter_at(rect);
    if selected {
        painter.rect_filled(rect, CornerRadius::ZERO, pal.ink);
    } else if response.hovered() {
        painter.rect_filled(rect, CornerRadius::ZERO, pal.paper_hover);
    }
    let (ink, weak) = if selected { (pal.paper, pal.paper) } else { (pal.ink, pal.ink_weak) };
    paint(&painter, rect, ink, weak);
    if response.has_focus() {
        let color = if selected { pal.paper } else { pal.ink };
        painter.rect_stroke(rect.shrink(1.0), CornerRadius::ZERO, Stroke::new(2.0, color), StrokeKind::Inside);
    }
    response
}
