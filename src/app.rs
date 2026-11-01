//! Состояние окна и фоновые задачи. Кадр собирается здесь: схема во всё окно,
//! поверх неё листы указателя и сведений.

use crate::anim::{self, ease_out};
use crate::diff::{DiffView, Request};
use crate::git::{self, Details, FileChange, FileDiff, Graph, Health, Idx, RefKind, Summary, branch_name};
use crate::layout::{self, LineKind, Map, Options};
use crate::map::{self, Motion};
use crate::text::Face;
use crate::theme::{DARK, LIGHT, Palette, fade};
use crate::ui::{Key, Tone, Ui, id};
use crate::watch::{self, Watcher};
use crate::{actions, diff, export, fmt, index, install, overview, panel, scan, update};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex};
use vello::kurbo::{Line, Point, Rect, Stroke};

pub const INDEX_WIDTH: f64 = 304.0;
pub const PANEL_WIDTH: f64 = 336.0;
pub const MARGIN: f64 = 12.0;
const SUMMARY_THREADS: usize = 3;

pub type Waker = Arc<dyn Fn() + Send + Sync>;

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

#[derive(Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub root: Option<PathBuf>,
    pub sort: Sort,
    pub theme: ThemeChoice,
    pub collapsed: HashSet<String>,
    pub show_merged: bool,
    pub compress: bool,
    pub show_tags: bool,
    pub auto_update: bool,
    pub last_update_check: u64,
    /// Найденная новая версия: показывается и без сети, и до следующей проверки.
    pub update_available: Option<update::Manifest>,
    pub window: Option<(u32, u32)>,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            root: None,
            sort: Sort::Name,
            theme: ThemeChoice::System,
            collapsed: HashSet::new(),
            show_merged: true,
            compress: true,
            show_tags: true,
            auto_update: true,
            last_update_check: 0,
            update_available: None,
            window: None,
        }
    }
}

impl Settings {
    fn path() -> Option<PathBuf> {
        install::settings_dir().map(|dir| dir.join("settings.json"))
    }

    pub fn load() -> Self {
        Self::path()
            .and_then(|path| std::fs::read_to_string(path).ok())
            .and_then(|text| serde_json::from_str(&text).ok())
            .unwrap_or_default()
    }

    pub fn save(&self) {
        let Some(path) = Self::path() else { return };
        let Ok(text) = serde_json::to_string_pretty(self) else { return };
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        // Через временный файл: прерванная запись не должна съесть настройки.
        let temp = path.with_extension("tmp");
        if std::fs::write(&temp, text).is_ok() {
            let _ = std::fs::rename(temp, path);
        }
    }
}

/// Что сейчас с обновлением программы.
pub enum UpdateState {
    Idle,
    Checking,
    UpToDate,
    Available(update::Manifest),
    Downloading,
    Failed(String),
}

#[derive(Clone, Copy, PartialEq)]
pub enum Selection {
    None,
    Commit(Idx),
    Line(u32),
    /// Незакоммиченная работа.
    Work,
}

pub struct Repo {
    pub path: PathBuf,
    pub name: String,
    pub group: String,
    pub summary: Option<Result<Summary, String>>,
    pub health: Option<Health>,
    /// Голый репозиторий: зеркало или резервная копия, а не рабочий проект.
    pub bare: bool,
    pub found_at: f64,
}

/// Открытый репозиторий со всем, что нужно схеме и правой панели.
pub struct Session {
    pub graph: Arc<Graph>,
    pub map: Map,
    pub motion: Motion,
    pub activity: Vec<u32>,
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
    pub line_stats: Option<(u32, LineStats)>,
    pub show_all_merged: bool,
    /// Когда сменилась страница правой панели.
    pub page_changed: f64,
    /// Незакоммиченные файлы: на схеме это пунктирная станция после HEAD.
    pub work: Vec<FileChange>,
    pub diff: Option<DiffView>,
    pub want_diff: Option<Request>,
    diff_generation: u64,
    /// Первый коммит сравнения: следующий щелчок по схеме выберет второй.
    pub compare_from: Option<Idx>,
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
        let branches: HashSet<&str> =
            graph.refs.iter().filter(|r| r.kind != RefKind::Tag).map(|r| branch_name(r.kind, &r.name)).collect();
        let active = map.lines.iter().filter(|l| l.kind == LineKind::Active).count();
        let oldest = graph.commits.iter().map(|c| c.time).min().unwrap_or_default();
        let mut caption = format!(
            "{}  ·  {}  ·  {}",
            map.lines[0].name,
            fmt::count(graph.commits.len(), "коммит", "коммита", "коммитов"),
            fmt::count(branches.len(), "ветка", "ветки", "веток"),
        );
        if active > 0 {
            caption.push_str(&format!("  ·  {active} в работе"));
        }
        caption.push_str(&format!("  ·  с {}", fmt::month_year_from(oldest)));
        if graph.truncated {
            caption.push_str("  ·  показана новая часть истории");
        }
        let motion = Motion::new(&map);
        let activity = map::activity(&map);
        let mut session = Self {
            graph,
            map,
            motion,
            activity,
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
            line_stats: None,
            show_all_merged: false,
            page_changed: 0.0,
            work: Vec::new(),
            diff: None,
            want_diff: None,
            diff_generation: 0,
            compare_from: None,
        };
        session.rebuild_labels(show_tags);
        session
    }

    /// Указатели, которые не совпадают с именем своей линии, подписываются у станции.
    pub fn rebuild_labels(&mut self, show_tags: bool) {
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
            Selection::None | Selection::Work => {}
        }
    }

    pub fn open_diff(&mut self, title: String, request: Request, prefer: Option<String>, now: f64) {
        self.diff_generation += 1;
        self.want_diff = Some(request);
        self.diff = Some(DiffView { title, files: None, selected: 0, opened: now, closing: None, prefer });
    }

    /// Сравнение двух коммитов: старый всегда слева, как бы их ни выбрали.
    pub fn compare(&mut self, a: Idx, b: Idx, now: f64) {
        let commits = &self.graph.commits;
        let (old, new) = if commits[a as usize].time <= commits[b as usize].time { (a, b) } else { (b, a) };
        let (from, to) = (commits[old as usize].id, commits[new as usize].id);
        let title = format!("{} и {}", from.to_hex_with_len(8), to.to_hex_with_len(8));
        self.open_diff(title, Request::Commits { from: Some(from), to }, None, now);
    }

    /// Репозиторий перечитан: вид, выбор и поиск остаются, новые коммиты подъезжают на линию.
    fn adopt(&mut self, previous: Session, now: f64) {
        let old_ids: HashSet<_> = previous.graph.commits.iter().map(|c| c.id).collect();
        let index: HashMap<_, Idx> = self.graph.commits.iter().enumerate().map(|(i, c)| (c.id, i as Idx)).collect();
        let remap = |commit: Idx| index.get(&previous.graph.commits[commit as usize].id).copied();
        self.selection = match previous.selection {
            Selection::Commit(commit) => remap(commit).map_or(Selection::None, Selection::Commit),
            Selection::Line(line) => {
                let name = &previous.map.lines[line as usize].name;
                let found = self.map.lines.iter().position(|l| !name.is_empty() && l.name == *name);
                found.map_or(Selection::None, |id| Selection::Line(id as u32))
            }
            other => other,
        };
        self.back = None;
        self.search = previous.search;
        self.work = previous.work;
        self.diff = previous.diff;
        self.diff_generation = previous.diff_generation;
        self.show_all_merged = previous.show_all_merged;
        self.details = None;
        match self.selection {
            Selection::Commit(commit) => self.want_details = Some(commit),
            Selection::Line(line) => self.compute_line_stats(line),
            _ => {}
        }
        self.run_search();
        self.motion = previous.motion;
        let fresh: HashSet<Idx> =
            self.graph.commits.iter().enumerate().filter(|(_, c)| !old_ids.contains(&c.id)).map(|(i, _)| i as Idx).collect();
        self.motion.absorb(&self.map, &fresh, now);
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
    Loaded { generation: u64, path: PathBuf, keep_view: bool, result: Result<Option<(Arc<Graph>, Map)>, String> },
    Relaid { generation: u64, options: Options, map: Option<Map> },
    Details { commit: Idx, result: Result<Details, String> },
    Picked(PathBuf),
    UpdateChecked { manual: bool, result: Result<Option<update::Manifest>, String> },
    UpdateDownloaded(Result<PathBuf, String>),
    Health { path: PathBuf, health: Health },
    Watch(watch::Event),
    Diff { generation: u64, result: Result<Vec<FileDiff>, String> },
    Notice(String),
    SavePng(PathBuf),
}

#[derive(Clone, PartialEq)]
pub enum WindowCommand {
    Close,
    Minimize,
    ToggleMaximize,
    Drag,
    /// Снять текущую схему без панелей, в двойном разрешении.
    SavePng(PathBuf),
}

/// Что попросила панель: выполняется снаружи, потому что выходит за пределы окна.
#[derive(Clone, PartialEq)]
pub enum Action {
    Editor,
    Terminal,
    Folder,
    Web(String),
    ExportPng,
    ExportSvg,
    Open(PathBuf),
}

pub struct App {
    tx: Sender<Msg>,
    rx: Receiver<Msg>,
    summaries: Sender<PathBuf>,
    waker: Waker,
    pub settings: Settings,
    pub repos: Vec<Repo>,
    pub scanning: bool,
    scan_generation: u64,
    scan_cancel: Arc<AtomicBool>,
    pub filter: String,
    pub session: Option<Session>,
    empty_repo: Option<PathBuf>,
    pub open_path: Option<PathBuf>,
    loading: bool,
    load_generation: u64,
    load_error: Option<String>,
    /// Репозиторий, который надо открыть, как только сканирование его найдёт.
    open_when_found: Option<PathBuf>,
    /// Прокрутить список к проекту, открытому не щелчком по нему.
    pub reveal_open: bool,
    /// Обновлять из программы можно только установленную копию.
    pub can_update: bool,
    pub update: UpdateState,
    pub commands: Vec<WindowCommand>,
    forced_theme: Option<ThemeChoice>,
    pub system_dark: bool,
    started: f64,
    pending_start: Option<PathBuf>,
    watcher: Watcher,
    /// Проекты с общей историей: индексы в `repos`.
    pub families: Vec<Vec<usize>>,
    pub overview_opened: f64,
    pub overview_expanded: HashSet<&'static str>,
    notice: Option<(String, f64)>,
    /// Кадр для выгрузки: только схема, без панелей.
    pub map_only: bool,
}

pub struct StartArgs {
    pub path: Option<PathBuf>,
    pub theme: Option<&'static str>,
}

impl App {
    pub fn new(args: StartArgs, waker: Waker) -> Self {
        let settings = Settings::load();
        let (tx, rx) = channel();
        let (summaries, queue) = channel::<PathBuf>();
        let queue = Arc::new(Mutex::new(queue));
        for _ in 0..SUMMARY_THREADS {
            let (queue, tx, waker) = (queue.clone(), tx.clone(), waker.clone());
            std::thread::spawn(move || {
                loop {
                    let next = queue.lock().ok().and_then(|q| q.recv().ok());
                    let Some(path) = next else { return };
                    let result = git::summary(&path);
                    let _ = tx.send(Msg::Summary { path: path.clone(), result });
                    waker();
                    // Состояние считается отдельно: оно дольше, а список нужен сразу.
                    if let Ok(health) = git::health(&path) {
                        let _ = tx.send(Msg::Health { path, health });
                        waker();
                    }
                }
            });
        }
        let update = match &settings.update_available {
            Some(manifest) if update::newer(&manifest.version, install::VERSION) => UpdateState::Available(manifest.clone()),
            _ => UpdateState::Idle,
        };
        let forced_theme = match args.theme {
            Some("light") => Some(ThemeChoice::Light),
            Some("dark") => Some(ThemeChoice::Dark),
            _ => None,
        };
        let watcher = {
            let (tx, waker) = (tx.clone(), waker.clone());
            Watcher::start(move |event| {
                let _ = tx.send(Msg::Watch(event));
                waker();
            })
        };
        let mut app = Self {
            tx,
            rx,
            summaries,
            waker,
            settings,
            repos: Vec::new(),
            scanning: false,
            scan_generation: 0,
            scan_cancel: Arc::new(AtomicBool::new(false)),
            filter: String::new(),
            session: None,
            empty_repo: None,
            open_path: None,
            loading: false,
            load_generation: 0,
            load_error: None,
            open_when_found: None,
            reveal_open: false,
            can_update: update::configured() && install::running_installed(),
            update,
            commands: Vec::new(),
            forced_theme,
            system_dark: false,
            started: 0.0,
            pending_start: None,
            watcher,
            families: Vec::new(),
            overview_opened: 0.0,
            overview_expanded: HashSet::new(),
            notice: None,
            map_only: false,
        };
        if app.can_update {
            std::thread::spawn(install::clean_downloaded_updates);
            let due = update::now_secs().saturating_sub(app.settings.last_update_check) >= update::CHECK_EVERY_SECS;
            if app.settings.auto_update && due {
                app.check_update(false);
            }
        }
        // Путь из командной строки: папка проектов или сам проект.
        match args.path {
            Some(path) if scan::is_repo(&path) => {
                let root = path.parent().map_or_else(|| path.clone(), Path::to_path_buf);
                app.open_when_found = Some(path);
                app.pending_start = Some(root);
            }
            Some(path) => app.pending_start = Some(path),
            None => app.pending_start = app.settings.root.clone().filter(|r| r.is_dir()),
        }
        app
    }

    pub fn palette(&self) -> &'static Palette {
        let dark = match self.forced_theme.unwrap_or(self.settings.theme) {
            ThemeChoice::System => self.system_dark,
            ThemeChoice::Light => false,
            ThemeChoice::Dark => true,
        };
        if dark { &DARK } else { &LIGHT }
    }

    pub fn start_scan(&mut self, root: PathBuf, now: f64) {
        self.scan_cancel.store(true, Ordering::Relaxed);
        self.scan_cancel = Arc::new(AtomicBool::new(false));
        self.scan_generation += 1;
        self.settings.root = Some(root.clone());
        self.repos.clear();
        self.session = None;
        self.empty_repo = None;
        self.open_path = None;
        self.load_error = None;
        self.scanning = true;
        self.started = now;
        self.overview_opened = now;
        self.families.clear();
        self.watcher.follow(None);
        let generation = self.scan_generation;
        let (cancel, tx, waker) = (self.scan_cancel.clone(), self.tx.clone(), self.waker.clone());
        std::thread::spawn(move || {
            scan::run(&root, &cancel, &|found| {
                let message = match found {
                    scan::Found::Repo(path) => Msg::Found { generation, path },
                    scan::Found::Done => Msg::ScanDone { generation },
                };
                let _ = tx.send(message);
                waker();
            });
        });
    }

    pub fn open_repo(&mut self, path: PathBuf, keep_view: bool) {
        self.load_generation += 1;
        self.loading = true;
        self.load_error = None;
        self.open_path = Some(path.clone());
        self.empty_repo = None;
        self.watcher.follow(Some(path.clone()));
        let generation = self.load_generation;
        let options = self.layout_options();
        let (tx, waker) = (self.tx.clone(), self.waker.clone());
        std::thread::spawn(move || {
            let result = git::load(&path).map(|graph| {
                let map = layout::build(&graph, options);
                map.map(|map| (Arc::new(graph), map))
            });
            let _ = tx.send(Msg::Loaded { generation, path, keep_view, result });
            waker();
        });
    }

    /// Назад к обзору папки.
    pub fn close_repo(&mut self, now: f64) {
        self.load_generation += 1;
        self.loading = false;
        self.session = None;
        self.open_path = None;
        self.empty_repo = None;
        self.load_error = None;
        self.overview_opened = now;
        self.watcher.follow(None);
    }

    /// Родство считается по общим корневым коммитам.
    fn rebuild_families(&mut self) {
        let mut by_root: HashMap<gix::ObjectId, Vec<usize>> = HashMap::new();
        for (i, repo) in self.repos.iter().enumerate().filter(|(_, r)| !r.bare) {
            for root in repo.health.iter().flat_map(|h| &h.roots) {
                by_root.entry(*root).or_default().push(i);
            }
        }
        // Проект может делить историю с разными соседями: собираем связные группы.
        let mut parent: Vec<usize> = (0..self.repos.len()).collect();
        fn find(parent: &mut [usize], mut at: usize) -> usize {
            while parent[at] != at {
                parent[at] = parent[parent[at]];
                at = parent[at];
            }
            at
        }
        for members in by_root.values() {
            for pair in members.windows(2) {
                let (a, b) = (find(&mut parent, pair[0]), find(&mut parent, pair[1]));
                parent[a] = b;
            }
        }
        let mut groups: HashMap<usize, Vec<usize>> = HashMap::new();
        for i in 0..self.repos.len() {
            if self.repos[i].health.is_some() && !self.repos[i].bare {
                groups.entry(find(&mut parent, i)).or_default().push(i);
            }
        }
        let mut families: Vec<Vec<usize>> = groups.into_values().filter(|g| g.len() > 1).collect();
        for family in &mut families {
            family.sort_by_key(|&i| self.repos[i].name.to_lowercase());
        }
        families.sort_by_key(|f| (std::cmp::Reverse(f.len()), self.repos[f[0]].name.to_lowercase()));
        self.families = families;
    }

    fn run_action(&mut self, action: Action) {
        let path = self.open_path.clone();
        let tx = self.tx.clone();
        let waker = self.waker.clone();
        let report = move |result: Result<(), String>| {
            if let Err(error) = result {
                let _ = tx.send(Msg::Notice(error));
                waker();
            }
        };
        match action {
            Action::Open(path) => self.open_repo(path, false),
            Action::Web(url) => report(actions::open_url(&url)),
            Action::Folder | Action::Editor | Action::Terminal => {
                let Some(path) = path else { return };
                // Запуск внешней программы может подумать: окно не должно ждать.
                std::thread::spawn(move || {
                    report(match action {
                        Action::Folder => actions::open_folder(&path),
                        Action::Editor => actions::open_editor(&path),
                        _ => actions::open_terminal(&path),
                    });
                });
            }
            Action::ExportSvg => {
                let Some(session) = &self.session else { return };
                let (svg, name) = (export::svg(session, self.palette()), session.title.clone());
                let (tx, waker) = (self.tx.clone(), self.waker.clone());
                std::thread::spawn(move || {
                    let picked = rfd::FileDialog::new().add_filter("SVG", &["svg"]).set_file_name(format!("{name}.svg")).save_file();
                    let Some(path) = picked else { return };
                    let text = match std::fs::write(&path, svg) {
                        Ok(()) => format!("Схема сохранена: {}", path.display()),
                        Err(error) => format!("Не удалось сохранить схему: {error}"),
                    };
                    let _ = tx.send(Msg::Notice(text));
                    waker();
                });
            }
            Action::ExportPng => {
                let Some(session) = &self.session else { return };
                let name = session.title.clone();
                let (tx, waker) = (self.tx.clone(), self.waker.clone());
                std::thread::spawn(move || {
                    let picked = rfd::FileDialog::new().add_filter("PNG", &["png"]).set_file_name(format!("{name}.png")).save_file();
                    if let Some(path) = picked {
                        let _ = tx.send(Msg::SavePng(path));
                        waker();
                    }
                });
            }
        }
    }

    pub fn notify(&mut self, text: String, now: f64) {
        self.notice = Some((text, now));
    }

    fn layout_options(&self) -> Options {
        Options { show_merged: self.settings.show_merged, compress: self.settings.compress }
    }

    pub fn relayout(&mut self) {
        let Some(session) = &self.session else { return };
        self.load_generation += 1;
        let generation = self.load_generation;
        let (graph, options) = (session.graph.clone(), self.layout_options());
        let (tx, waker) = (self.tx.clone(), self.waker.clone());
        std::thread::spawn(move || {
            let map = layout::build(&graph, options);
            let _ = tx.send(Msg::Relaid { generation, options, map });
            waker();
        });
    }

    pub fn pick_folder(&self) {
        let (tx, waker) = (self.tx.clone(), self.waker.clone());
        let start = self.settings.root.clone();
        std::thread::spawn(move || {
            let mut dialog = rfd::FileDialog::new().set_title("Папка с проектами");
            if let Some(start) = start {
                dialog = dialog.set_directory(start);
            }
            if let Some(path) = dialog.pick_folder() {
                let _ = tx.send(Msg::Picked(path));
                waker();
            }
        });
    }

    pub fn check_update(&mut self, manual: bool) {
        if manual {
            self.update = UpdateState::Checking;
        }
        let (tx, waker) = (self.tx.clone(), self.waker.clone());
        std::thread::spawn(move || {
            let _ = tx.send(Msg::UpdateChecked { manual, result: update::check() });
            waker();
        });
    }

    pub fn download_update(&mut self) {
        self.update = UpdateState::Downloading;
        let (tx, waker) = (self.tx.clone(), self.waker.clone());
        std::thread::spawn(move || {
            let _ = tx.send(Msg::UpdateDownloaded(update::download()));
            waker();
        });
    }

    fn drain(&mut self, now: f64) {
        while let Ok(message) = self.rx.try_recv() {
            match message {
                Msg::Found { generation, path } if generation == self.scan_generation => {
                    let root = self.settings.root.clone().unwrap_or_default();
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
                        self.open_repo(path.clone(), false);
                    }
                    let bare = !path.join(".git").exists();
                    self.repos.push(Repo { path, name, group, summary: None, health: None, bare, found_at: now });
                }
                Msg::ScanDone { generation } if generation == self.scan_generation => self.scanning = false,
                Msg::Summary { path, result } => {
                    if let Some(repo) = self.repos.iter_mut().find(|r| r.path == path) {
                        repo.summary = Some(result);
                    }
                }
                Msg::Loaded { generation, path, keep_view, result } if generation == self.load_generation => {
                    self.loading = false;
                    match result {
                        Ok(Some((graph, map))) => {
                            let previous = self.session.take().filter(|_| keep_view);
                            let mut session = Session::new(graph, map, &path, self.settings.show_tags);
                            if let Some(previous) = previous {
                                session.adopt(previous, now);
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
                    let same = options == self.layout_options();
                    if let (Some(session), Some(map), true) = (&mut self.session, map, same) {
                        session.map = map;
                        session.activity = map::activity(&session.map);
                        session.motion.replay(&session.map);
                        if matches!(session.selection, Selection::Line(_)) {
                            session.selection = Selection::None;
                        }
                        session.line_stats = None;
                        session.rebuild_labels(self.settings.show_tags);
                    }
                }
                Msg::Details { commit, result } => {
                    if let Some(session) = &mut self.session
                        && session.selection == Selection::Commit(commit)
                    {
                        session.details = Some((commit, result));
                    }
                }
                Msg::Picked(path) => self.start_scan(path, now),
                Msg::Health { path, health } => {
                    if let Some(repo) = self.repos.iter_mut().find(|r| r.path == path) {
                        repo.health = Some(health);
                    }
                    self.rebuild_families();
                }
                Msg::Watch(watch::Event::Changed(path)) => {
                    if self.open_path.as_ref() == Some(&path) && !self.loading {
                        self.open_repo(path, true);
                    }
                }
                Msg::Watch(watch::Event::Work(path, files)) => {
                    if let Some(repo) = self.repos.iter_mut().find(|r| r.path == path)
                        && let Some(health) = &mut repo.health
                    {
                        health.dirty = files.len();
                    }
                    if let Some(session) = &mut self.session
                        && self.open_path.as_ref() == Some(&path)
                    {
                        session.work = files;
                        if session.work.is_empty() && session.selection == Selection::Work {
                            session.select(Selection::None);
                        }
                    }
                }
                Msg::Diff { generation, result } => {
                    if let Some(session) = &mut self.session
                        && session.diff_generation == generation
                        && let Some(view) = &mut session.diff
                    {
                        if let (Ok(files), Some(prefer)) = (&result, &view.prefer) {
                            view.selected = files.iter().position(|f| f.path == *prefer).unwrap_or(0);
                        }
                        view.files = Some(result);
                    }
                }
                Msg::Notice(text) => self.notify(text, now),
                Msg::SavePng(path) => self.commands.push(WindowCommand::SavePng(path)),
                Msg::UpdateChecked { manual, result } => {
                    self.settings.last_update_check = update::now_secs();
                    match result {
                        Ok(Some(manifest)) => {
                            self.settings.update_available = Some(manifest.clone());
                            self.update = UpdateState::Available(manifest);
                        }
                        Ok(None) if manual => self.update = UpdateState::UpToDate,
                        Err(error) if manual => self.update = UpdateState::Failed(error),
                        // Сбой фоновой проверки не повод отвлекать: проверим завтра.
                        _ => {}
                    }
                }
                // Новая версия закроет эту копию сама, но настройки лучше сохранить заранее.
                Msg::UpdateDownloaded(result) => match result.and_then(|path| update::run(&path)) {
                    Ok(()) => {
                        self.settings.update_available = None;
                        self.settings.save();
                        self.commands.push(WindowCommand::Close);
                    }
                    Err(error) => self.update = UpdateState::Failed(error),
                },
                _ => {}
            }
        }
    }

    fn fetch_diff(&mut self) {
        let Some(session) = &mut self.session else { return };
        let Some(request) = session.want_diff.take() else { return };
        let (path, generation) = (session.graph.path.clone(), session.diff_generation);
        let (tx, waker) = (self.tx.clone(), self.waker.clone());
        std::thread::spawn(move || {
            let result = match request {
                Request::Commits { from, to } => git::diff_commits(&path, from, to),
                Request::Worktree => git::diff_worktree(&path),
                Request::Branch { main, tip } => git::merge_base(&path, main, tip)
                    .and_then(|base| git::diff_commits(&path, Some(base.unwrap_or(main)), tip)),
            };
            let _ = tx.send(Msg::Diff { generation, result });
            waker();
        });
    }

    fn fetch_details(&mut self) {
        self.fetch_diff();
        let Some(session) = &mut self.session else { return };
        let Some(commit) = session.want_details.take() else { return };
        let (path, id) = (session.graph.path.clone(), session.graph.commits[commit as usize].id);
        let (tx, waker) = (self.tx.clone(), self.waker.clone());
        std::thread::spawn(move || {
            let result = git::details(&path, id);
            let _ = tx.send(Msg::Details { commit, result });
            waker();
        });
    }

    pub fn dropped(&mut self, path: PathBuf, now: f64) {
        if path.is_dir() {
            self.start_scan(path, now);
        }
    }

    /// Один кадр всего окна.
    pub fn frame(&mut self, ui: &mut Ui) {
        let now = ui.now();
        if let Some(root) = self.pending_start.take() {
            self.start_scan(root, now);
        }
        self.drain(now);
        let (w, h) = ui.size;
        if self.map_only {
            if let Some(session) = &mut self.session {
                map::show(ui, session, Rect::new(0.0, 0.0, w, h), 1.0);
            }
            return;
        }
        if ui.input.key(Key::F5)
            && let Some(path) = self.open_path.clone()
        {
            self.open_repo(path, true);
        }

        let opened = self.session.is_some();
        let slide = ui.tween("panel-open", opened, anim::SOFT) as f64;
        let index_rect = Rect::new(MARGIN, MARGIN, MARGIN + INDEX_WIDTH, h - MARGIN);
        let panel_x = w - MARGIN - PANEL_WIDTH + (1.0 - slide) * (PANEL_WIDTH + MARGIN * 2.0);
        let panel_rect = Rect::new(panel_x, 44.0, panel_x + PANEL_WIDTH, h - MARGIN);
        let free = Rect::new(index_rect.x1, 0.0, panel_x.min(w), h);
        ui.covers.push(index_rect);
        if opened {
            ui.covers.push(panel_rect);
        }
        if let Some(session) = &self.session
            && session.diff.is_some()
        {
            ui.covers.push(diff::sheet_rect(free));
        }
        self.canvas(ui, free);
        self.caption_buttons(ui, free);
        index::show(ui, self, index_rect);
        let mut options = self.layout_options();
        let info = self.panel_info();
        if let Some(session) = &mut self.session {
            let change = panel::show(ui, session, &mut options, &mut self.settings.show_tags, panel_rect, free, &info);
            self.settings.show_merged = options.show_merged;
            self.settings.compress = options.compress;
            if change.relayout {
                self.relayout();
            }
            if change.labels
                && let Some(session) = &mut self.session
            {
                session.rebuild_labels(self.settings.show_tags);
            }
            if let Some(action) = change.action {
                self.run_action(action);
            }
        }
        if let Some(session) = &mut self.session {
            diff::show(ui, session, free);
        }
        self.paint_notice(ui, free);
        self.fetch_details();
        ui.finish();
    }

    fn panel_info(&self) -> panel::Info {
        let repo = self.repos.iter().position(|r| Some(&r.path) == self.open_path.as_ref());
        let web_url = repo.and_then(|i| self.repos[i].health.as_ref()).and_then(|h| h.web_url.clone());
        let family = repo
            .and_then(|i| self.families.iter().find(|f| f.contains(&i)).map(|f| (i, f)))
            .map(|(i, f)| {
                f.iter().filter(|&&m| m != i).map(|&m| (self.repos[m].name.clone(), self.repos[m].path.clone())).collect()
            })
            .unwrap_or_default();
        panel::Info { web_url, family }
    }

    /// Короткое сообщение внизу схемы: выезжает, держится и уходит само.
    fn paint_notice(&mut self, ui: &mut Ui, free: Rect) {
        const SHOWN: f64 = 4.0;
        let Some((text, start)) = self.notice.clone() else { return };
        let age = ui.now() - start;
        if age > SHOWN {
            self.notice = None;
            return;
        }
        ui.animating = true;
        let pal = ui.pal;
        let phase = ease_out((age / 0.28) as f32).min(ease_out(((SHOWN - age) / 0.28) as f32));
        let width = (ui.fonts.width(&text, Face::Body, 14.0) as f64 + 36.0).min(free.width() - 40.0);
        let y = free.y1 - 124.0 + (1.0 - phase) as f64 * 16.0;
        let rect = Rect::from_origin_size((free.center().x - width / 2.0, y), (width, 40.0));
        ui.sheet(rect, 10.0, phase);
        ui.line(&text, Face::Body, 14.0, rect.x0 + 18.0, rect.center().y, width - 36.0, fade(pal.ink, phase));
    }

    fn canvas(&mut self, ui: &mut Ui, free: Rect) {
        let title_of = |path: &Option<PathBuf>| {
            path.as_ref().and_then(|p| p.file_name()).map_or_else(String::new, |n| n.to_string_lossy().into_owned())
        };
        let open_title = title_of(&self.open_path);
        if let Some(error) = self.load_error.clone() {
            self.placeholder(ui, free, &open_title, &format!("Не удалось прочитать репозиторий: {error}"), None);
        } else if self.session.is_some() {
            if let Some(session) = &mut self.session {
                if session.diff.is_none() {
                    map::handle_keys(ui, session, free);
                    if ui.input.key(Key::Escape) {
                        if session.compare_from.is_some() {
                            session.compare_from = None;
                        } else {
                            let target = session.back.map_or(Selection::None, Selection::Line);
                            session.select(target);
                        }
                    }
                }
                map::show(ui, session, free, 1.0);
            }
        } else if self.loading {
            self.placeholder(ui, free, &open_title, "Читаю историю.", None);
        } else if self.empty_repo.is_some() {
            let body = "В этом репозитории пока нет коммитов. После первого коммита здесь появится схема.";
            self.placeholder(ui, free, &open_title, body, None);
        } else if self.settings.root.is_none() {
            let body = "Выбери папку с проектами. Программа найдёт в ней все git-репозитории и нарисует ветки каждого как схему метро. Папку можно просто перетащить в окно.";
            if self.placeholder(ui, free, "Карта веток", body, Some("Выбрать папку")) {
                self.pick_folder();
            }
        } else if !self.repos.is_empty() {
            overview::show(ui, self, free);
        } else if self.scanning {
            self.placeholder(ui, free, "Карта веток", "Ищу репозитории.", None);
        } else {
            let body = "В этой папке нет git-репозиториев. Выбери другую папку.";
            if self.placeholder(ui, free, "Пусто", body, Some("Выбрать папку")) {
                self.pick_folder();
            }
        }
    }

    fn placeholder(&self, ui: &mut Ui, free: Rect, title: &str, body: &str, action: Option<&str>) -> bool {
        let pal = ui.pal;
        let time = ui.now() - self.started;
        if time < 3.0 {
            ui.animating = true;
        }
        map::demo(ui, free, time);
        let x = free.x0 + 48.0;
        let y = (free.center().y - 300.0).max(56.0);
        let t = time as f32;
        let appear = ease_out(t / 0.5);
        let title_height = ui.paragraph(title, Face::Display, 72.0, Point::new(x, y + (1.0 - appear) as f64 * 16.0), free.width() - 96.0, fade(pal.ink, appear));
        let sub = ease_out((t - 0.12) / 0.5);
        let body_y = y + title_height + 6.0 + (1.0 - sub) as f64 * 10.0;
        let body_height = ui.paragraph(body, Face::Body, 16.5, Point::new(x, body_y), 560.0, fade(pal.ink_weak, sub));
        let mut clicked = false;
        if let Some(label) = action {
            let rect = Rect::from_origin_size((x, body_y + body_height + 18.0), (220.0, 42.0));
            clicked = ui.button("placeholder-action", rect, label, Tone::Primary);
        }
        clicked
    }

    /// Окно без рамки Windows: кнопки и область перетаскивания свои.
    fn caption_buttons(&mut self, ui: &mut Ui, free: Rect) {
        let pal = ui.pal;
        let (w, _) = ui.size;
        let band = Rect::new(free.x0, 0.0, w - 138.0, 36.0);
        if ui.pointer_in(band) && !ui.any_active() {
            if ui.input.double_click {
                self.commands.push(WindowCommand::ToggleMaximize);
            } else if ui.input.pressed {
                self.commands.push(WindowCommand::Drag);
            }
        }
        for (i, kind) in [WindowCommand::Minimize, WindowCommand::ToggleMaximize, WindowCommand::Close].into_iter().enumerate() {
            let rect = Rect::from_origin_size((w - 138.0 + i as f64 * 46.0, 0.0), (46.0, 36.0));
            let key = id(("caption", i));
            let response = ui.interact(key, rect);
            let hover = ui.tween((key, "hover"), response.hovered, anim::SNAP);
            let close = kind == WindowCommand::Close;
            let fill = if close { pal.danger } else { pal.sheet_hover };
            ui.fill(&rect, fade(fill, hover));
            let color = crate::theme::mix(pal.ink, if close { pal.on_danger } else { pal.ink }, hover);
            let c = rect.center();
            let thin = Stroke::new(1.3);
            match kind {
                WindowCommand::Minimize => {
                    ui.scene.stroke(&thin, Default::default(), color, None, &Line::new((c.x - 5.0, c.y), (c.x + 5.0, c.y)));
                }
                WindowCommand::ToggleMaximize => {
                    ui.scene.stroke(&thin, Default::default(), color, None, &Rect::new(c.x - 5.0, c.y - 5.0, c.x + 5.0, c.y + 5.0));
                }
                _ => {
                    ui.scene.stroke(&thin, Default::default(), color, None, &Line::new((c.x - 5.0, c.y - 5.0), (c.x + 5.0, c.y + 5.0)));
                    ui.scene.stroke(&thin, Default::default(), color, None, &Line::new((c.x + 5.0, c.y - 5.0), (c.x - 5.0, c.y + 5.0)));
                }
            }
            if response.clicked {
                self.commands.push(kind.clone());
            }
        }
    }

}
