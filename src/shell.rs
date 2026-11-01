//! Окно: события winit превращаются во ввод для интерфейса, кадр уходит на поверхность.
//! Рамки Windows нет, поэтому перетаскивание и изменение размера сделаны здесь.
//! Пока ничего не движется, кадры не рисуются: простаивающая программа не грузит видеокарту.

use crate::app::{App, StartArgs, WindowCommand};
use crate::gfx::{self, Gpu};
use crate::installer::{self, Choice, Mode, Wizard};
use crate::text::Fonts;
use crate::theme::{DARK, LIGHT, Palette};
use crate::ui::{Cursor, Input, Key, State, Ui};
use std::sync::Arc;
use std::time::{Duration, Instant};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use vello::peniko::Fill;
use vello::util::{RenderContext, RenderSurface};
use winit::application::ApplicationHandler;
use winit::dpi::{LogicalSize, PhysicalSize};
use winit::event::{ElementState, Ime, MouseButton, MouseScrollDelta, StartCause, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop, EventLoopProxy};
use winit::keyboard::{Key as WinitKey, ModifiersState, NamedKey};
use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};
use winit::window::{CursorIcon, Icon, ResizeDirection, Theme, Window, WindowId};

/// Постоянное фоновое движение: поезда и сигнал. Ему хватает 30 кадров в секунду.
const AMBIENT_FRAME: Duration = Duration::from_millis(33);
const EDGE: f64 = 5.0;

enum Root {
    Main(Box<App>),
    Wizard { wizard: Box<Wizard>, args: Option<StartArgs>, _lock: Option<crate::sys::SetupLock> },
}

struct Shell<'s> {
    root: Root,
    window: Option<Arc<Window>>,
    surface: Option<RenderSurface<'s>>,
    gpu: Option<Gpu>,
    context: Option<RenderContext>,
    scene: Scene,
    outer: Scene,
    fonts: Fonts,
    state: State,
    input: Input,
    started: Instant,
    last_frame: Instant,
    last_click: Instant,
    modifiers: ModifiersState,
    focused: bool,
    busy: bool,
    ambient: bool,
    shown: bool,
    system_dark: bool,
    calm: bool,
    proxy: EventLoopProxy<()>,
    edge: Option<ResizeDirection>,
    frames: u64,
}

fn resize_edge(pointer: Point, size: (f64, f64)) -> Option<ResizeDirection> {
    let (w, h) = size;
    let (left, right) = (pointer.x < EDGE, pointer.x > w - EDGE);
    let (top, bottom) = (pointer.y < EDGE, pointer.y > h - EDGE);
    Some(match (left, right, top, bottom) {
        (true, _, true, _) => ResizeDirection::NorthWest,
        (_, true, true, _) => ResizeDirection::NorthEast,
        (true, _, _, true) => ResizeDirection::SouthWest,
        (_, true, _, true) => ResizeDirection::SouthEast,
        (true, ..) => ResizeDirection::West,
        (_, true, ..) => ResizeDirection::East,
        (_, _, true, _) => ResizeDirection::North,
        (_, _, _, true) => ResizeDirection::South,
        _ => return None,
    })
}

fn edge_cursor(direction: ResizeDirection) -> CursorIcon {
    match direction {
        ResizeDirection::East | ResizeDirection::West => CursorIcon::EwResize,
        ResizeDirection::North | ResizeDirection::South => CursorIcon::NsResize,
        ResizeDirection::NorthEast | ResizeDirection::SouthWest => CursorIcon::NeswResize,
        ResizeDirection::NorthWest | ResizeDirection::SouthEast => CursorIcon::NwseResize,
    }
}

/// Windows 11 скругляет окно сам, если попросить: без рамки он этого не делает.
fn round_corners(window: &Window) {
    use windows_sys::Win32::Graphics::Dwm::{DWMWA_WINDOW_CORNER_PREFERENCE, DWMWCP_ROUND, DwmSetWindowAttribute};
    let Ok(handle) = window.window_handle() else { return };
    if let RawWindowHandle::Win32(handle) = handle.as_raw() {
        let preference = DWMWCP_ROUND;
        unsafe {
            DwmSetWindowAttribute(
                handle.hwnd.get() as _,
                DWMWA_WINDOW_CORNER_PREFERENCE as u32,
                (&raw const preference).cast(),
                size_of_val(&preference) as u32,
            );
        }
    }
}

impl Shell<'_> {
    fn is_wizard(&self) -> bool {
        matches!(self.root, Root::Wizard { .. })
    }

    fn palette(&self) -> &'static Palette {
        match &self.root {
            Root::Main(app) => app.palette(),
            Root::Wizard { .. } => {
                if self.system_dark { &DARK } else { &LIGHT }
            }
        }
    }

    fn logical_size(&self) -> (f64, f64) {
        let Some(window) = &self.window else { return (1.0, 1.0) };
        let size = window.inner_size().to_logical::<f64>(window.scale_factor());
        (size.width, size.height)
    }

    fn redraw(&self) {
        if let Some(window) = &self.window {
            window.request_redraw();
        }
    }

    fn apply(&mut self, commands: Vec<WindowCommand>, event_loop: &ActiveEventLoop) {
        let Some(window) = self.window.clone() else { return };
        for command in commands {
            match command {
                WindowCommand::Close => self.close(event_loop),
                WindowCommand::Minimize => window.set_minimized(true),
                WindowCommand::ToggleMaximize => window.set_maximized(!window.is_maximized()),
                WindowCommand::Drag => {
                    let _ = window.drag_window();
                }
                WindowCommand::SavePng(path) => self.save_png(&path),
            }
        }
    }

    /// Снимок текущей схемы без панелей, в двойном разрешении.
    fn save_png(&mut self, path: &std::path::Path) {
        let size = self.logical_size();
        let pal = self.palette();
        let Root::Main(app) = &mut self.root else { return };
        let input = Input { time: self.input.time, ..Input::default() };
        self.scene.reset();
        self.scene.fill(Fill::NonZero, Affine::IDENTITY, pal.ground, None, &Rect::new(0.0, 0.0, size.0, size.1));
        app.map_only = true;
        {
            let mut ui = Ui::new(&mut self.scene, &mut self.fonts, &input, &mut self.state, pal, size, true);
            app.frame(&mut ui);
        }
        app.map_only = false;
        self.outer.reset();
        self.outer.append(&self.scene, Some(Affine::scale(2.0)));
        let result = match &mut self.gpu {
            Some(gpu) => gpu.render_png(&self.outer, (size.0 * 2.0) as u32, (size.1 * 2.0) as u32, pal.ground, path),
            None => Err("видеокарта не готова".to_owned()),
        };
        let now = self.input.time;
        app.notify(
            match result {
                Ok(()) => format!("Схема сохранена: {}", path.display()),
                Err(error) => format!("Не удалось сохранить схему: {error}"),
            },
            now,
        );
        self.redraw();
    }

    fn close(&mut self, event_loop: &ActiveEventLoop) {
        if let Root::Main(app) = &mut self.root {
            if let Some(window) = &self.window
                && !window.is_maximized()
            {
                let size = window.inner_size();
                app.settings.window = Some((size.width, size.height));
            }
            app.settings.save();
        }
        event_loop.exit();
    }

    fn frame(&mut self, event_loop: &ActiveEventLoop) {
        self.frames += 1;
        let now = Instant::now();
        self.input.dt = (now - self.last_frame).as_secs_f32().min(0.05);
        self.last_frame = now;
        self.input.time = (now - self.started).as_secs_f64();
        let size = self.logical_size();
        let scale = self.window.as_ref().map_or(1.0, |w| w.scale_factor());
        let pal = self.palette();
        self.scene.reset();
        self.scene.fill(Fill::NonZero, Affine::IDENTITY, pal.ground, None, &Rect::new(0.0, 0.0, size.0, size.1));
        let mut portable = false;
        let mut commands;
        let (cursor, animating, ambient);
        {
            let mut ui = Ui::new(&mut self.scene, &mut self.fonts, &self.input, &mut self.state, pal, size, self.calm);
            match &mut self.root {
                Root::Main(app) => {
                    app.frame(&mut ui);
                    commands = std::mem::take(&mut app.commands);
                }
                Root::Wizard { wizard, .. } => {
                    portable = matches!(wizard.frame(&mut ui), Choice::RunPortable);
                    commands = std::mem::take(&mut wizard.commands);
                }
            }
            cursor = ui.cursor;
            animating = ui.animating;
            ambient = ui.ambient;
        }
        self.input.end_frame();
        self.busy = animating;
        self.ambient = ambient && self.focused;

        if let Some(window) = self.window.clone() {
            let icon = match (self.edge, cursor) {
                (Some(direction), _) => edge_cursor(direction),
                (None, Cursor::Default) => CursorIcon::Default,
                (None, Cursor::Pointer) => CursorIcon::Pointer,
                (None, Cursor::Text) => CursorIcon::Text,
                (None, Cursor::Grab) => CursorIcon::Grab,
                (None, Cursor::Grabbing) => CursorIcon::Grabbing,
            };
            window.set_cursor(icon);
            self.outer.reset();
            self.outer.append(&self.scene, Some(Affine::scale(scale)));
            if let (Some(gpu), Some(surface)) = (&mut self.gpu, &self.surface)
                && let Err(error) = gpu.present(surface, &self.outer, pal.ground)
                && cfg!(debug_assertions)
            {
                eprintln!("{error}");
            }
            // Окно показывается только с готовым кадром: без белой вспышки при запуске.
            if !self.shown {
                self.shown = true;
                window.set_visible(true);
            }
        }
        if portable {
            self.go_portable();
            commands.clear();
        }
        self.apply(commands, event_loop);
    }

    fn go_portable(&mut self) {
        let Root::Wizard { args, .. } = &mut self.root else { return };
        let args = args.take().unwrap_or(StartArgs { path: None, theme: None });
        let proxy = self.proxy.clone();
        let waker: crate::app::Waker = Arc::new(move || {
            let _ = proxy.send_event(());
        });
        self.root = Root::Main(Box::new(App::new(args, waker)));
        if let Some(window) = &self.window {
            window.set_title("Карта веток");
            window.set_resizable(true);
            window.set_min_inner_size(Some(LogicalSize::new(960.0, 600.0)));
            let _ = window.request_inner_size(LogicalSize::new(1480.0, 920.0));
        }
        self.sync_theme();
    }

    fn sync_theme(&mut self) {
        let dark = self.system_dark;
        if let Root::Main(app) = &mut self.root {
            app.system_dark = dark;
        }
    }

    fn key(&mut self, event: &winit::event::KeyEvent) {
        let ctrl = self.modifiers.control_key();
        let mapped = match &event.logical_key {
            WinitKey::Named(NamedKey::ArrowLeft) => Some(Key::Left),
            WinitKey::Named(NamedKey::ArrowRight) => Some(Key::Right),
            WinitKey::Named(NamedKey::ArrowUp) => Some(Key::Up),
            WinitKey::Named(NamedKey::ArrowDown) => Some(Key::Down),
            WinitKey::Named(NamedKey::Home) => Some(Key::Home),
            WinitKey::Named(NamedKey::End) => Some(Key::End),
            WinitKey::Named(NamedKey::Enter) => Some(Key::Enter),
            WinitKey::Named(NamedKey::Space) => Some(Key::Space),
            WinitKey::Named(NamedKey::Escape) => Some(Key::Escape),
            WinitKey::Named(NamedKey::Tab) => Some(Key::Tab),
            WinitKey::Named(NamedKey::Backspace) => Some(Key::Backspace),
            WinitKey::Named(NamedKey::Delete) => Some(Key::Delete),
            WinitKey::Named(NamedKey::F5) => Some(Key::F5),
            WinitKey::Character(text) if ctrl => {
                // По физической клавише: сочетания работают и в русской раскладке.
                use winit::keyboard::{KeyCode, PhysicalKey};
                match event.physical_key {
                    PhysicalKey::Code(KeyCode::KeyF) => Some(Key::Find),
                    PhysicalKey::Code(KeyCode::KeyA) => Some(Key::SelectAll),
                    PhysicalKey::Code(KeyCode::KeyC) => Some(Key::Copy),
                    PhysicalKey::Code(KeyCode::KeyX) => Some(Key::Cut),
                    PhysicalKey::Code(KeyCode::KeyV) => Some(Key::Paste),
                    _ => {
                        let _ = text;
                        None
                    }
                }
            }
            WinitKey::Character(text) if text == "+" || text == "=" => Some(Key::Plus),
            WinitKey::Character(text) if text == "-" => Some(Key::Minus),
            _ => None,
        };
        if let Some(key) = mapped {
            self.input.keys.push(key);
        }
        if !ctrl && let Some(text) = &event.text {
            self.input.text.push_str(text);
        }
    }
}

impl ApplicationHandler for Shell<'_> {
    fn new_events(&mut self, _event_loop: &ActiveEventLoop, cause: StartCause) {
        if matches!(cause, StartCause::ResumeTimeReached { .. }) {
            self.redraw();
        }
    }

    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_some() {
            return;
        }
        let wizard = self.is_wizard();
        let (title, size) = if wizard {
            (installer::TITLE, LogicalSize::new(installer::SIZE.0, installer::SIZE.1))
        } else {
            ("Карта веток", LogicalSize::new(1480.0, 920.0))
        };
        let mut attributes = Window::default_attributes()
            .with_title(title)
            .with_inner_size(size)
            .with_decorations(false)
            .with_transparent(false)
            .with_resizable(!wizard)
            .with_visible(false)
            .with_window_icon(Icon::from_rgba(include_bytes!("../assets/icon-64.rgba").to_vec(), 64, 64).ok());
        if !wizard {
            attributes = attributes.with_min_inner_size(LogicalSize::new(960.0, 600.0));
        }
        if let Root::Main(app) = &self.root
            && let Some((width, height)) = app.settings.window
        {
            attributes = attributes.with_inner_size(PhysicalSize::new(width.max(640), height.max(480)));
        }
        {
            use winit::platform::windows::WindowAttributesExtWindows;
            attributes = attributes.with_undecorated_shadow(true);
        }
        #[cfg(debug_assertions)]
        {
            attributes = attributes.with_active(std::env::var_os("GITMAP_BACKGROUND").is_none());
        }
        let window = Arc::new(event_loop.create_window(attributes).expect("окно не создано"));
        round_corners(&window);
        window.set_ime_allowed(true);
        self.system_dark = window.theme() == Some(Theme::Dark);
        self.sync_theme();
        self.focused = true;
        let physical = window.inner_size();
        let mut context = self.context.take().unwrap_or_else(RenderContext::new);
        match gfx::create_surface(&mut context, window.clone(), physical.width, physical.height) {
            Ok(surface) => {
                self.gpu = Some(Gpu::for_surface(context, &surface));
                self.surface = Some(surface);
            }
            Err(error) => {
                crate::sys::message_box(&format!("Не удалось запустить графику: {error}"));
                event_loop.exit();
            }
        }
        self.window = Some(window);
        self.last_frame = Instant::now();
        // Скрытое окно не получает перерисовку: первый кадр рисуем сами и только потом показываем.
        self.frame(event_loop);
    }

    fn user_event(&mut self, _event_loop: &ActiveEventLoop, _event: ()) {
        self.redraw();
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        match event {
            WindowEvent::CloseRequested => {
                let closable = match &self.root {
                    Root::Wizard { wizard, .. } => wizard.can_close(),
                    Root::Main(_) => true,
                };
                if closable {
                    self.close(event_loop);
                }
            }
            WindowEvent::Resized(size) => {
                if size.width > 0
                    && size.height > 0
                    && let (Some(gpu), Some(surface)) = (&self.gpu, &mut self.surface)
                {
                    gpu.context.resize_surface(surface, size.width, size.height);
                }
                // Кадр сразу, а не по очереди: при растягивании окна нет пустых полос.
                if self.shown && size.width > 0 && size.height > 0 {
                    self.frame(event_loop);
                }
            }
            WindowEvent::Ime(Ime::Commit(text)) => {
                self.input.text.push_str(&text);
                self.redraw();
            }
            WindowEvent::ScaleFactorChanged { .. } | WindowEvent::Occluded(false) => self.redraw(),
            WindowEvent::ThemeChanged(theme) => {
                self.system_dark = theme == Theme::Dark;
                self.sync_theme();
                self.redraw();
            }
            WindowEvent::Focused(focused) => {
                self.focused = focused;
                self.redraw();
            }
            WindowEvent::ModifiersChanged(modifiers) => {
                self.modifiers = modifiers.state();
                self.input.shift = self.modifiers.shift_key();
            }
            WindowEvent::CursorMoved { position, .. } => {
                let scale = self.window.as_ref().map_or(1.0, |w| w.scale_factor());
                let pointer = Point::new(position.x / scale, position.y / scale);
                self.input.pointer = Some(pointer);
                let maximized = self.window.as_ref().is_some_and(|w| w.is_maximized());
                self.edge = if self.is_wizard() || maximized { None } else { resize_edge(pointer, self.logical_size()) };
                self.redraw();
            }
            WindowEvent::CursorLeft { .. } => {
                self.input.pointer = None;
                self.edge = None;
                self.redraw();
            }
            WindowEvent::MouseInput { state, button: MouseButton::Left, .. } => {
                match state {
                    ElementState::Pressed => {
                        if let (Some(direction), Some(window)) = (self.edge, &self.window) {
                            let _ = window.drag_resize_window(direction);
                            return;
                        }
                        self.input.pressed = true;
                        self.input.down = true;
                        self.input.double_click = self.last_click.elapsed() < Duration::from_millis(350);
                        self.last_click = Instant::now();
                    }
                    ElementState::Released => {
                        self.input.released = true;
                        self.input.down = false;
                    }
                }
                self.redraw();
            }
            WindowEvent::MouseWheel { delta, .. } => {
                let (x, y) = match delta {
                    MouseScrollDelta::LineDelta(x, y) => (x as f64 * 60.0, y as f64 * 60.0),
                    MouseScrollDelta::PixelDelta(d) => (d.x, d.y),
                };
                if self.input.shift {
                    self.input.wheel_x += y;
                } else {
                    self.input.wheel += y;
                    self.input.wheel_x += x;
                }
                self.redraw();
            }
            WindowEvent::KeyboardInput { event, .. } => {
                if event.state == ElementState::Pressed {
                    self.key(&event);
                    self.redraw();
                }
            }
            WindowEvent::DroppedFile(path) => {
                let now = self.started.elapsed().as_secs_f64();
                if let Root::Main(app) = &mut self.root {
                    app.dropped(path, now);
                }
                self.redraw();
            }
            WindowEvent::RedrawRequested => self.frame(event_loop),
            _ => {}
        }
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        // Проверка настоящего окна: закрывается само и оставляет отметку о кадрах.
        #[cfg(debug_assertions)]
        if let Some(seconds) = std::env::var("GITMAP_EXIT_AFTER").ok().and_then(|v| v.parse::<f64>().ok())
            && self.started.elapsed().as_secs_f64() > seconds
        {
            if let Ok(path) = std::env::var("GITMAP_EXIT_NOTE") {
                let _ = std::fs::write(path, format!("{} кадров, размер {:?}", self.frames, self.logical_size()));
            }
            self.close(event_loop);
            return;
        }
        #[cfg(debug_assertions)]
        if std::env::var_os("GITMAP_EXIT_AFTER").is_some() {
            event_loop.set_control_flow(ControlFlow::WaitUntil(Instant::now() + AMBIENT_FRAME));
            if self.busy {
                self.redraw();
            }
            return;
        }
        if self.busy {
            event_loop.set_control_flow(ControlFlow::Wait);
            self.redraw();
        } else if self.ambient {
            event_loop.set_control_flow(ControlFlow::WaitUntil(Instant::now() + AMBIENT_FRAME));
        } else {
            event_loop.set_control_flow(ControlFlow::Wait);
        }
    }
}

pub fn run(wizard: Option<Mode>, args: StartArgs, lock: Option<crate::sys::SetupLock>) {
    let event_loop = EventLoop::<()>::with_user_event().build().expect("цикл событий не создан");
    let proxy = event_loop.create_proxy();
    let wake = proxy.clone();
    let waker: crate::app::Waker = Arc::new(move || {
        let _ = wake.send_event(());
    });
    let root = match wizard {
        Some(mode) => Root::Wizard { wizard: Box::new(Wizard::new(mode, waker)), args: Some(args), _lock: lock },
        None => Root::Main(Box::new(App::new(args, waker))),
    };
    let now = Instant::now();
    let mut shell = Shell {
        root,
        window: None,
        surface: None,
        gpu: None,
        context: None,
        scene: Scene::new(),
        outer: Scene::new(),
        fonts: Fonts::new(),
        state: State::default(),
        input: Input::default(),
        started: now,
        last_frame: now,
        last_click: now - Duration::from_secs(10),
        modifiers: ModifiersState::empty(),
        focused: true,
        busy: true,
        ambient: false,
        shown: false,
        system_dark: false,
        calm: crate::sys::reduced_motion(),
        proxy,
        edge: None,
        frames: 0,
    };
    let _ = event_loop.run_app(&mut shell);
}
