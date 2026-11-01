//! Мастер установки и удаления. Открывается, когда программа запущена не из папки
//! установки (GitMap-Setup.exe из «Загрузок»), или по `--uninstall` из «Приложений» Windows.
//! Шаги мастера это станции на линии: линия дорастает до текущей, как схема в самой программе.

use crate::anim::{self, ease_back, ease_out};
use crate::app::{WindowCommand, Waker};
use crate::install::{self, InstallOptions, Step};
use crate::map::route;
use crate::text::{self, Face};
use crate::theme::{fade, mix};
use crate::ui::{Key, Tone, Ui, id};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, TryRecvError, channel};
use vello::kurbo::{Affine, Circle, Line, Point, Rect, RoundedRect, Stroke};

pub const TITLE: &str = "Установка: Карта веток";
pub const SIZE: (f64, f64) = (640.0, 640.0);
const PAD: f64 = 32.0;

#[derive(Clone, Copy, PartialEq)]
pub enum Mode {
    Install,
    Uninstall,
}

#[derive(Clone, Copy, PartialEq, Hash)]
enum Page {
    Welcome,
    Options,
    UninstallConfirm,
    Progress,
    Finish,
}

#[derive(Clone, Copy, PartialEq)]
enum Maintenance {
    Update,
    Remove,
}

#[derive(Clone, PartialEq)]
enum StepState {
    Pending,
    Running,
    Done,
    Failed(String),
}

enum WorkerMsg {
    Started(usize),
    Finished(usize, Result<(), String>),
}

pub enum Choice {
    Stay,
    /// «Запустить без установки».
    RunPortable,
}

pub struct Wizard {
    mode: Mode,
    page: Page,
    /// Уже установленная версия и её папка.
    existing: Option<(String, PathBuf)>,
    maintenance: Maintenance,
    opts: InstallOptions,
    dir_text: String,
    remove_settings: bool,
    plan: Vec<Step>,
    states: Vec<StepState>,
    worker: Option<Receiver<WorkerMsg>>,
    browse: Option<Receiver<Option<PathBuf>>>,
    launch_after: bool,
    entered_uninstall_directly: bool,
    page_changed: f64,
    waker: Waker,
    pub commands: Vec<WindowCommand>,
}

impl Wizard {
    pub fn new(mode: Mode, waker: Waker) -> Self {
        let existing = install::installed_version().zip(install::installed_dir());
        // При обновлении галочки показывают то, что установлено сейчас.
        let opts = if existing.is_some() {
            install::current_options()
        } else {
            InstallOptions { dir: install::default_dir(), start_menu: true, desktop: true }
        };
        Self {
            mode,
            page: if mode == Mode::Uninstall { Page::UninstallConfirm } else { Page::Welcome },
            existing,
            maintenance: Maintenance::Update,
            dir_text: opts.dir.to_string_lossy().to_string(),
            opts,
            remove_settings: false,
            plan: Vec::new(),
            states: Vec::new(),
            worker: None,
            browse: None,
            launch_after: true,
            entered_uninstall_directly: mode == Mode::Uninstall,
            page_changed: 0.0,
            waker,
            commands: Vec::new(),
        }
    }

    fn updating(&self) -> bool {
        self.existing.is_some()
    }

    fn go(&mut self, page: Page, now: f64) {
        self.page = page;
        self.page_changed = now;
    }

    fn start_install(&mut self, now: f64) {
        self.mode = Mode::Install;
        let mut dir = PathBuf::from(self.dir_text.trim());
        // Своя подпапка и для набранного вручную пути: программа не ложится россыпью
        // в общую папку вроде D:\Tools.
        if !self.updating() && !dir.ends_with(install::DIR_NAME) {
            dir.push(install::DIR_NAME);
        }
        self.opts.dir = dir;
        self.plan = install::install_plan();
        self.run_from(0, now);
    }

    fn start_uninstall(&mut self, now: f64) {
        self.mode = Mode::Uninstall;
        self.plan = install::uninstall_plan(self.remove_settings);
        self.run_from(0, now);
    }

    /// Запускает шаги начиная с `first` (так работает и «Повторить»).
    fn run_from(&mut self, first: usize, now: f64) {
        if first == 0 {
            self.states = vec![StepState::Pending; self.plan.len()];
        }
        for state in &mut self.states[first..] {
            *state = StepState::Pending;
        }
        self.go(Page::Progress, now);
        let (tx, rx) = channel();
        let plan = self.plan.clone();
        let (mode, opts) = (self.mode, self.opts.clone());
        let dir = self.existing.as_ref().map(|(_, d)| d.clone());
        let waker = self.waker.clone();
        std::thread::spawn(move || {
            for (i, step) in plan.iter().enumerate().skip(first) {
                let _ = tx.send(WorkerMsg::Started(i));
                waker();
                let result = match mode {
                    Mode::Install => install::run_install_step(*step, &opts),
                    Mode::Uninstall => install::run_uninstall_step(*step, dir.as_deref()),
                };
                let failed = result.is_err();
                let _ = tx.send(WorkerMsg::Finished(i, result));
                waker();
                if failed {
                    break;
                }
            }
        });
        self.worker = Some(rx);
    }

    fn poll(&mut self, now: f64) {
        if let Some(rx) = &self.worker {
            loop {
                match rx.try_recv() {
                    Ok(WorkerMsg::Started(i)) => self.states[i] = StepState::Running,
                    Ok(WorkerMsg::Finished(i, Ok(()))) => self.states[i] = StepState::Done,
                    Ok(WorkerMsg::Finished(i, Err(e))) => self.states[i] = StepState::Failed(e),
                    Err(TryRecvError::Empty) => break,
                    // Поток шагов упал: показываем сбой, иначе окно ждало бы вечно.
                    Err(TryRecvError::Disconnected) => {
                        let failed = self.states.iter().any(|s| matches!(s, StepState::Failed(_)));
                        if !failed && let Some(state) = self.states.iter_mut().find(|s| **s != StepState::Done) {
                            *state = StepState::Failed("внутренняя ошибка установщика".into());
                        }
                        break;
                    }
                }
            }
            if self.states.iter().all(|s| *s == StepState::Done) {
                self.worker = None;
                self.go(Page::Finish, now);
            }
        }
        if let Some(rx) = &self.browse
            && let Ok(picked) = rx.try_recv()
        {
            self.browse = None;
            if let Some(mut dir) = picked {
                if !dir.ends_with(install::DIR_NAME) {
                    dir.push(install::DIR_NAME);
                }
                self.dir_text = dir.to_string_lossy().to_string();
            }
        }
    }

    fn browse_folder(&mut self) {
        let (tx, rx) = channel();
        let start = PathBuf::from(self.dir_text.trim());
        let waker = self.waker.clone();
        std::thread::spawn(move || {
            let start = start.parent().map(PathBuf::from).unwrap_or(start);
            let picked = rfd::FileDialog::new().set_title("Куда установить программу").set_directory(start).pick_folder();
            let _ = tx.send(picked);
            waker();
        });
        self.browse = Some(rx);
    }

    fn failed_index(&self) -> Option<usize> {
        self.states.iter().position(|s| matches!(s, StepState::Failed(_)))
    }

    /// Пока идут шаги, окно не закрывается: прерванная установка оставила бы
    /// программу наполовину (файл скопирован, а записи в «Приложениях» нет).
    pub fn can_close(&self) -> bool {
        self.worker.is_none() || self.failed_index().is_some()
    }

    pub fn frame(&mut self, ui: &mut Ui) -> Choice {
        let now = ui.now();
        self.poll(now);
        let (w, h) = ui.size;
        let mut choice = Choice::Stay;

        self.caption(ui);
        self.header(ui);
        let body = Rect::new(PAD, 172.0, w - PAD, h - 72.0);
        // Страница въезжает справа: шаг вперёд виден сам по себе.
        let phase = ease_out(((now - self.page_changed) / 0.28) as f32);
        if phase < 1.0 {
            ui.animating = true;
            ui.alpha(phase, Rect::new(0.0, body.y0 - 8.0, w, body.y1));
        }
        let shifted = body + vello::kurbo::Vec2::new((1.0 - phase) as f64 * 26.0, 0.0);
        match self.page {
            Page::Welcome => {
                if self.welcome(ui, shifted) {
                    choice = Choice::RunPortable;
                }
            }
            Page::Options => self.options(ui, shifted),
            Page::UninstallConfirm => self.uninstall_confirm(ui, shifted),
            Page::Progress => self.progress(ui, shifted),
            Page::Finish => self.finish(ui, shifted),
        }
        if phase < 1.0 {
            ui.end_alpha();
        }
        self.footer(ui, Rect::new(PAD, h - 58.0, w - PAD, h - 18.0));
        ui.finish();
        choice
    }

    fn caption(&mut self, ui: &mut Ui) {
        let pal = ui.pal;
        let (w, _) = ui.size;
        let band = Rect::new(0.0, 0.0, w - 46.0, 36.0);
        if ui.pointer_in(band) && ui.input.pressed {
            self.commands.push(WindowCommand::Drag);
        }
        let rect = Rect::new(w - 46.0, 0.0, w, 36.0);
        let response = ui.interact(id("wizard-close"), rect);
        let hover = ui.tween("wizard-close-hover", response.hovered, anim::SNAP);
        ui.fill(&rect, fade(pal.danger, hover));
        let color = mix(pal.ink, pal.on_danger, hover);
        let c = rect.center();
        let thin = Stroke::new(1.3);
        ui.scene.stroke(&thin, Affine::IDENTITY, color, None, &Line::new((c.x - 5.0, c.y - 5.0), (c.x + 5.0, c.y + 5.0)));
        ui.scene.stroke(&thin, Affine::IDENTITY, color, None, &Line::new((c.x + 5.0, c.y - 5.0), (c.x - 5.0, c.y + 5.0)));
        if response.clicked && self.can_close() {
            self.commands.push(WindowCommand::Close);
        }
    }

    fn header(&self, ui: &mut Ui) {
        let pal = ui.pal;
        let (w, _) = ui.size;
        let title = match self.mode {
            Mode::Install if self.updating() => "Обновление",
            Mode::Install => "Карта веток",
            Mode::Uninstall => "Удаление",
        };
        let layout = ui.fonts.layout(title, Face::Display, 52.0, None);
        text::draw(ui.scene, &layout, Affine::translate((PAD, 22.0)), pal.ink);
        ui.line_right(&format!("Версия {}", install::VERSION), Face::Body, 12.5, w - PAD, 76.0, pal.ink_weak);
        let stops: &[(&str, Page)] = match self.mode {
            Mode::Install => {
                &[("Начало", Page::Welcome), ("Параметры", Page::Options), ("Установка", Page::Progress), ("Готово", Page::Finish)]
            }
            Mode::Uninstall => &[("Что удалить", Page::UninstallConfirm), ("Удаление", Page::Progress), ("Готово", Page::Finish)],
        };
        let current = stops.iter().position(|(_, page)| *page == self.page).unwrap_or(0);
        route_strip(ui, Rect::new(PAD, 104.0, w - PAD, 152.0), stops.iter().map(|(name, _)| *name), current);
    }

    fn welcome(&mut self, ui: &mut Ui, area: Rect) -> bool {
        let pal = ui.pal;
        let (x, width) = (area.x0, area.width());
        if let Some((version, dir)) = self.existing.clone() {
            let mut y = area.y0;
            y += ui.paragraph("Программа уже установлена", Face::Semi, 21.0, Point::new(x, y), width, pal.ink) + 4.0;
            let note = format!("Версия {version} в папке {}.", dir.display());
            y += ui.paragraph(&note, Face::Body, 14.0, Point::new(x, y), width, pal.ink_weak) + 16.0;
            let label = if crate::update::newer(install::VERSION, &version) {
                format!("Обновить до версии {}", install::VERSION)
            } else {
                format!("Установить заново, версия {}", install::VERSION)
            };
            let update = Rect::from_origin_size((x, y), (width, 62.0));
            if choice_row(ui, update, self.maintenance == Maintenance::Update, &label, "Выбранная папка и настройки сохранятся.") {
                self.maintenance = Maintenance::Update;
            }
            let remove = Rect::from_origin_size((x, y + 70.0), (width, 62.0));
            if choice_row(ui, remove, self.maintenance == Maintenance::Remove, "Удалить программу", "Твои репозитории это не затрагивает.") {
                self.maintenance = Maintenance::Remove;
            }
            return false;
        }
        let mut y = area.y0;
        y += ui.paragraph("История проектов на одной схеме", Face::Semi, 21.0, Point::new(x, y), width, pal.ink) + 6.0;
        let text = "Выбираешь папку, программа находит в ней все git-репозитории и рисует ветки каждого как схему метро: основная ветка по центру, остальные расходятся цветными линиями.";
        y += ui.paragraph(text, Face::Body, 14.5, Point::new(x, y), width, pal.ink_weak) + 12.0;
        sample_map(ui, Rect::new(x, y, x + width, y + 120.0), (ui.now() - self.page_changed) as f32);
        y += 132.0;
        y += ui.paragraph("Программа только читает репозитории и ничего в них не меняет.", Face::Body, 14.5, Point::new(x, y), width, pal.ink_weak) + 8.0;
        ui.button("portable", Rect::from_origin_size((x - 12.0, y), (208.0, 32.0)), "Запустить без установки", Tone::Quiet)
    }

    fn options(&mut self, ui: &mut Ui, area: Rect) {
        let pal = ui.pal;
        let (x, width) = (area.x0, area.width());
        let mut y = area.y0;
        y += ui.paragraph("Папка установки", Face::Semi, 15.0, Point::new(x, y), width, pal.ink) + 6.0;
        let field = Rect::from_origin_size((x, y), (width - 104.0, 36.0));
        if self.updating() {
            ui.fill(&RoundedRect::from_rect(field, 9.0), pal.sheet_hover);
            ui.line(&self.dir_text.clone(), Face::Body, 14.0, x + 12.0, field.center().y, field.width() - 24.0, pal.ink_weak);
        } else {
            ui.input_field("install-dir", field, &mut self.dir_text, "Папка");
            let browse = Rect::from_origin_size((x + width - 96.0, y), (96.0, 36.0));
            if ui.button("browse", browse, "Обзор", Tone::Plain) && self.browse.is_none() {
                self.browse_folder();
            }
        }
        y += 42.0;
        let (note, color) = if self.updating() {
            ("При обновлении папка не меняется.", pal.ink_weak)
        } else if self.dir_text.trim().is_empty() {
            ("Укажи папку установки.", pal.danger)
        } else if !Path::new(self.dir_text.trim()).is_absolute() {
            (r"Укажи полный путь, например C:\Tools\GitMap.", pal.danger)
        } else {
            ("Права администратора не нужны: программа ставится для этого пользователя.", pal.ink_weak)
        };
        y += ui.paragraph(note, Face::Body, 12.5, Point::new(x, y), width, color) + 22.0;
        y += ui.paragraph("Ярлыки", Face::Semi, 15.0, Point::new(x, y), width, pal.ink) + 4.0;
        ui.toggle("menu", Rect::from_origin_size((x, y), (width, 32.0)), "В меню «Пуск»", &mut self.opts.start_menu);
        y += 32.0;
        ui.toggle("desktop", Rect::from_origin_size((x, y), (width, 32.0)), "На рабочем столе", &mut self.opts.desktop);
        y += 54.0;
        y += ui.paragraph("Обновления", Face::Semi, 15.0, Point::new(x, y), width, pal.ink) + 4.0;
        ui.paragraph("Раз в день программа проверяет новую версию и предлагает её установить. Выключается в окне программы.", Face::Body, 14.0, Point::new(x, y), width, pal.ink_weak);
    }

    fn uninstall_confirm(&mut self, ui: &mut Ui, area: Rect) {
        let pal = ui.pal;
        let (x, width) = (area.x0, area.width());
        let mut y = area.y0;
        y += ui.paragraph("Удалить Карту веток?", Face::Semi, 21.0, Point::new(x, y), width, pal.ink) + 6.0;
        let text = "Будут удалены программа, ярлыки и запись в «Приложениях» Windows. Твои репозитории это не затрагивает.";
        y += ui.paragraph(text, Face::Body, 14.5, Point::new(x, y), width, pal.ink_weak) + 20.0;
        ui.toggle("remove-settings", Rect::from_origin_size((x, y), (width, 32.0)), "Удалить настройки: папку, тему и вид схемы", &mut self.remove_settings);
        ui.paragraph("Оставь их, если собираешься установить программу снова.", Face::Body, 12.5, Point::new(x + 42.0, y + 34.0), width - 42.0, pal.ink_weak);
    }

    fn progress(&self, ui: &mut Ui, area: Rect) {
        let pal = ui.pal;
        let (x, width) = (area.x0, area.width());
        let headline = match (self.mode, self.failed_index().is_some()) {
            (_, true) => "Не получилось",
            (Mode::Install, false) => "Устанавливаю",
            (Mode::Uninstall, false) => "Удаляю",
        };
        let mut y = area.y0;
        y += ui.paragraph(headline, Face::Semi, 21.0, Point::new(x, y), width, pal.ink) + 14.0;
        let count = self.plan.len();
        for (i, (step, state)) in self.plan.iter().zip(&self.states).enumerate() {
            let c = Point::new(x + 12.0, y + 18.0);
            let done = *state == StepState::Done;
            let fill = ui.tween(("step-line", i), done, anim::SOFT) as f64;
            if i + 1 < count {
                let stroke = Stroke::new(2.0);
                ui.scene.stroke(&stroke, Affine::IDENTITY, pal.rule_strong, None, &Line::new(c, (c.x, c.y + 40.0)));
                if fill > 0.01 {
                    let solid = Stroke::new(5.0);
                    ui.scene.stroke(&solid, Affine::IDENTITY, pal.ink, None, &Line::new(c, (c.x, c.y + 40.0 * fill)));
                }
            }
            match state {
                StepState::Pending => {
                    ui.fill(&Circle::new(c, 7.0), pal.ground);
                    ui.stroke(&Circle::new(c, 7.0), 2.0, pal.rule_strong);
                }
                StepState::Running => {
                    // Станция, на которой идёт работа: вокруг неё расходится сигнал.
                    let phase = (ui.now() * 1.4).fract() as f32;
                    ui.stroke(&Circle::new(c, 8.0 + 12.0 * ease_out(phase) as f64), 2.0, fade(pal.ink, 0.6 * (1.0 - phase)));
                    ui.fill(&Circle::new(c, 8.0), pal.sheet);
                    ui.stroke(&Circle::new(c, 8.0), 3.0, pal.ink);
                    ui.ambient = true;
                    ui.animating = true;
                }
                StepState::Done => {
                    let pop = ui.tween(("step-done", i), true, anim::BOUNCE).max(0.2) as f64;
                    ui.fill(&Circle::new(c, 8.0 * pop), pal.ink);
                }
                StepState::Failed(_) => {
                    ui.fill(&Circle::new(c, 10.0), pal.danger);
                    let stroke = Stroke::new(2.0);
                    for (dx, dy) in [(3.5, 3.5), (3.5, -3.5)] {
                        let a = c + vello::kurbo::Vec2::new(-dx, -dy);
                        let b = c + vello::kurbo::Vec2::new(dx, dy);
                        ui.scene.stroke(&stroke, Affine::IDENTITY, pal.on_danger, None, &Line::new(a, b));
                    }
                }
            }
            let (face, color) = match state {
                StepState::Pending => (Face::Body, pal.ink_weak),
                StepState::Running => (Face::Semi, pal.ink),
                StepState::Done => (Face::Body, pal.ink),
                StepState::Failed(_) => (Face::Semi, pal.danger),
            };
            ui.line(step.title(), face, 15.0, c.x + 26.0, c.y, width - 40.0, color);
            y += 40.0;
            if let StepState::Failed(error) = state {
                y += ui.paragraph(error, Face::Body, 14.0, Point::new(c.x + 26.0, y - 8.0), width - 40.0, pal.ink) + 4.0;
            }
        }
    }

    fn finish(&mut self, ui: &mut Ui, area: Rect) {
        let pal = ui.pal;
        let (x, width) = (area.x0, area.width());
        let (title, text) = match self.mode {
            Mode::Install if self.updating() => ("Программа обновлена", "Выбранная папка и настройки на месте."),
            Mode::Install => ("Программа установлена", "Выбери папку с проектами, и программа нарисует их схемы."),
            Mode::Uninstall if self.remove_settings => ("Программа удалена", "Удалены программа, ярлыки и настройки."),
            Mode::Uninstall => ("Программа удалена", "Настройки остались: они пригодятся, если установишь её снова."),
        };
        let mut y = area.y0;
        y += ui.paragraph(title, Face::Semi, 21.0, Point::new(x, y), width, pal.ink) + 6.0;
        y += ui.paragraph(text, Face::Body, 14.5, Point::new(x, y), width, pal.ink_weak) + 18.0;
        if self.mode == Mode::Install {
            let shortcuts = match (self.opts.start_menu, self.opts.desktop) {
                (true, true) => "меню «Пуск» и рабочий стол",
                (true, false) => "меню «Пуск»",
                (false, true) => "рабочий стол",
                (false, false) => "не созданы",
            };
            let dir = self.opts.dir.to_string_lossy().to_string();
            for (key, value) in [("Папка", dir.as_str()), ("Ярлыки", shortcuts)] {
                ui.line(key, Face::Body, 12.5, x, y + 9.0, 70.0, pal.ink_weak);
                y += ui.paragraph(value, Face::Body, 14.0, Point::new(x + 76.0, y), width - 76.0, pal.ink).max(18.0) + 6.0;
            }
            ui.toggle("launch", Rect::from_origin_size((x, y + 10.0), (width, 32.0)), "Открыть Карту веток", &mut self.launch_after);
        }
    }

    fn footer(&mut self, ui: &mut Ui, rect: Rect) {
        let pal = ui.pal;
        let now = ui.now();
        ui.hline(0.0, ui.size.0, rect.y0 - 12.0, 1.0, pal.rule);
        let enter = ui.input.key(Key::Enter) && ui.state.focus.is_none();
        let escape = ui.input.key(Key::Escape);
        let running = self.worker.is_some() && self.failed_index().is_none();
        let (w, h) = (rect.width(), rect.height());
        let left = Rect::from_origin_size((rect.x0, rect.y0), (120.0, h));
        let primary = Rect::from_origin_size((rect.x0 + w - 150.0, rect.y0), (150.0, h));
        let second = Rect::from_origin_size((rect.x0 + w - 270.0, rect.y0), (112.0, h));
        let can_cancel = !running && self.page != Page::Finish;
        let label = if self.page == Page::Progress { "Закрыть" } else { "Отмена" };
        if can_cancel && (ui.button("cancel", left, label, Tone::Plain) || escape) {
            self.commands.push(WindowCommand::Close);
        }
        match self.page {
            Page::Welcome => {
                if ui.button("next", primary, "Далее", Tone::Primary) || enter {
                    let remove = self.updating() && self.maintenance == Maintenance::Remove;
                    self.go(if remove { Page::UninstallConfirm } else { Page::Options }, now);
                }
            }
            Page::Options => {
                let ok = Path::new(self.dir_text.trim()).is_absolute();
                let label = if self.updating() { "Обновить" } else { "Установить" };
                if (ui.button("install", primary, label, if ok { Tone::Primary } else { Tone::Plain }) || enter) && ok {
                    self.start_install(now);
                }
                if ui.button("back", second, "Назад", Tone::Plain) {
                    self.go(Page::Welcome, now);
                }
            }
            Page::UninstallConfirm => {
                if ui.button("remove", primary, "Удалить", Tone::Danger) {
                    self.start_uninstall(now);
                }
                if !self.entered_uninstall_directly && ui.button("back", second, "Назад", Tone::Plain) {
                    self.go(Page::Welcome, now);
                }
            }
            Page::Progress => {
                if let Some(i) = self.failed_index()
                    && ui.button("retry", primary, "Повторить", Tone::Primary)
                {
                    self.run_from(i, now);
                }
            }
            Page::Finish => {
                if ui.button("done", primary, "Готово", Tone::Primary) || enter {
                    if self.mode == Mode::Install && self.launch_after {
                        let _ = install::launch(&self.opts.dir);
                    }
                    self.commands.push(WindowCommand::Close);
                }
            }
        }
    }
}

/// Шаги мастера как участок линии: толстая линия дорастает до текущей станции.
fn route_strip<'a>(ui: &mut Ui, rect: Rect, stops: impl ExactSizeIterator<Item = &'a str>, current: usize) {
    let pal = ui.pal;
    let count = stops.len().max(2);
    let y = rect.y0 + 10.0;
    let x = |i: f64| rect.x0 + 10.0 + (rect.width() - 20.0) * i / (count - 1) as f64;
    let track = Stroke::new(2.0);
    ui.scene.stroke(&track, Affine::IDENTITY, pal.rule_strong, None, &Line::new((x(0.0), y), (x(count as f64 - 1.0), y)));
    let target = current as f32;
    let front = ui.spring(id("wizard-front"), target, target, anim::SOFT) as f64;
    if front > 0.001 {
        let solid = Stroke::new(6.0).with_caps(vello::kurbo::Cap::Round);
        ui.scene.stroke(&solid, Affine::IDENTITY, pal.ink, None, &Line::new((x(0.0), y), (x(front), y)));
    }
    for (i, name) in stops.enumerate() {
        let c = Point::new(x(i as f64), y);
        let passed = ((front - i as f64 + 0.5).clamp(0.0, 0.5) * 2.0) as f32;
        if i == current {
            ui.fill(&Circle::new(c, 9.0), pal.sheet);
            ui.stroke(&Circle::new(c, 9.0), 3.0, pal.ink);
            ui.fill(&Circle::new(c, 4.0), pal.ink);
        } else {
            ui.fill(&Circle::new(c, 6.5), mix(pal.ground, pal.ink, passed));
            ui.stroke(&Circle::new(c, 6.5), 2.0, mix(pal.rule_strong, pal.ink, passed));
        }
        let (face, color) = if i == current { (Face::Semi, pal.ink) } else { (Face::Body, pal.ink_weak) };
        let width = ui.fonts.width(name, face, 13.0) as f64;
        let left = match i {
            0 => c.x - 10.0,
            _ if i + 1 == count => c.x + 10.0 - width,
            _ => c.x - width / 2.0,
        };
        ui.line(name, face, 13.0, left, rect.y0 + 34.0, width + 2.0, color);
    }
}

/// Небольшой образец схемы на первом экране: сразу видно, что получится.
fn sample_map(ui: &mut Ui, rect: Rect, t: f32) {
    let pal = ui.pal;
    if t < 1.6 {
        ui.animating = true;
    }
    let cy = rect.center().y;
    let px = |u: f64| rect.x0 + rect.width() * u;
    for (i, (from, to, rise, color)) in [(0.14, 0.46, -40.0, 1), (0.32, 0.66, 40.0, 0), (0.62, 0.92, -40.0, 2)].into_iter().enumerate() {
        let grow = ease_out((t - 0.2 - i as f32 * 0.14) / 0.7);
        if grow <= 0.0 {
            continue;
        }
        let y = cy + rise;
        let turn = px(to) - f64::abs(rise);
        let mut points = route(Point::new(px(from), cy), Point::new(turn, y), true, 10.0);
        points.extend(route(Point::new(turn, y), Point::new(px(to), cy), false, 10.0));
        let total: f64 = points.windows(2).map(|w| (w[1] - w[0]).hypot()).sum();
        let mut left = total * grow as f64;
        let mut shown = vec![points[0]];
        for w in points.windows(2) {
            let step = (w[1] - w[0]).hypot();
            if step >= left {
                shown.push(w[0] + (w[1] - w[0]) * (left / step.max(1e-6)));
                break;
            }
            left -= step;
            shown.push(w[1]);
        }
        ui.path(&shown, 4.5, pal.lines[color]);
    }
    let end = rect.x0 + rect.width() * ease_out(t / 0.8) as f64;
    ui.scene.stroke(
        &Stroke::new(7.0).with_caps(vello::kurbo::Cap::Round),
        Affine::IDENTITY,
        pal.ink,
        None,
        &Line::new((rect.x0, cy), (end, cy)),
    );
    for (i, u) in [0.14, 0.32, 0.46, 0.62, 0.66, 0.92].into_iter().enumerate() {
        let pop = ease_back((t - 0.1 - i as f32 * 0.08) / 0.35) as f64;
        if pop > 0.0 {
            ui.fill(&Circle::new((px(u), cy), 6.0 * pop), pal.sheet);
            ui.stroke(&Circle::new((px(u), cy), 6.0 * pop), 2.5, pal.ink);
        }
    }
}

/// Выбор из двух путей: выбранная строка заливается чёрным с подъездом.
fn choice_row(ui: &mut Ui, rect: Rect, selected: bool, title: &str, text: &str) -> bool {
    let pal = ui.pal;
    let key = id(("choice", title));
    let response = ui.interact(key, rect);
    let lit = ui.tween((key, "lit"), selected, anim::SOFT);
    let hover = ui.tween((key, "hover"), response.hovered, anim::SNAP);
    let shape = RoundedRect::from_rect(rect, 10.0);
    ui.fill(&shape, mix(fade(pal.sheet_hover, 0.3 + 0.7 * hover), pal.ink, lit));
    ui.stroke(&shape, 1.0, mix(pal.rule_strong, pal.ink, lit));
    let (ink, weak) = (mix(pal.ink, pal.sheet, lit), mix(pal.ink_weak, pal.sheet, lit));
    ui.line(title, Face::Semi, 15.0, rect.x0 + 16.0, rect.y0 + 21.0, rect.width() - 32.0, ink);
    ui.line(text, Face::Body, 13.0, rect.x0 + 16.0, rect.y0 + 42.0, rect.width() - 32.0, weak);
    if response.focused {
        ui.focus_ring(rect, 10.0);
    }
    response.clicked
}
