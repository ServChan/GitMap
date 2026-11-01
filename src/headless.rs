//! Только для отладочной сборки: прогон сценария без окна. Время идёт по шагам 1/60 с,
//! поэтому анимации воспроизводятся точно и их можно разглядывать по кадрам.
//!
//! Сценарий, команды через `;`:
//!   size 1480 920 | dark | wait | run 0.5 | shot файл.png
//!   move x y | click x y | wheel x y дельта | key Home | text абв

use crate::app::{App, StartArgs};
use crate::installer::{self, Mode, Wizard};
use crate::gfx::Gpu;
use crate::text::Fonts;
use crate::ui::{Input, Key, State, Ui};
use std::path::Path;
use std::sync::Arc;
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use vello::peniko::Fill;

const STEP: f32 = 1.0 / 60.0;

fn key_named(name: &str) -> Option<Key> {
    Some(match name {
        "Home" => Key::Home,
        "End" => Key::End,
        "Escape" => Key::Escape,
        "Enter" => Key::Enter,
        "Tab" => Key::Tab,
        "Left" => Key::Left,
        "Right" => Key::Right,
        "Find" => Key::Find,
        "Plus" => Key::Plus,
        "Minus" => Key::Minus,
        _ => return None,
    })
}

struct Rig {
    app: App,
    wizard: Option<Wizard>,
    fonts: Fonts,
    state: State,
    input: Input,
    size: (f64, f64),
    scene: Scene,
}

impl Rig {
    fn step(&mut self) {
        self.input.dt = STEP;
        self.input.time += STEP as f64;
        self.scene.reset();
        let pal = self.app.palette();
        self.scene.fill(Fill::NonZero, Affine::IDENTITY, pal.ground, None, &Rect::new(0.0, 0.0, self.size.0, self.size.1));
        let mut ui = Ui::new(&mut self.scene, &mut self.fonts, &self.input, &mut self.state, pal, self.size, false);
        match &mut self.wizard {
            Some(wizard) => {
                wizard.frame(&mut ui);
                wizard.commands.clear();
            }
            None => self.app.frame(&mut ui),
        }
        self.input.end_frame();
    }

    fn run(&mut self, seconds: f64) {
        for _ in 0..(seconds * 60.0).round() as usize {
            self.step();
        }
    }
}

pub fn run(script: &str, args: StartArgs) -> Result<(), String> {
    let mut gpu = Gpu::headless()?;
    let waker: crate::app::Waker = Arc::new(|| {});
    let mut rig = Rig {
        app: App::new(args, waker.clone()),
        wizard: None,
        fonts: Fonts::new(),
        state: State::default(),
        input: Input::default(),
        size: (1480.0, 920.0),
        scene: Scene::new(),
    };
    for command in script.split(';').map(str::trim).filter(|c| !c.is_empty()) {
        eprintln!("> {command}");
        let parts: Vec<&str> = command.split_whitespace().collect();
        let number = |i: usize| parts.get(i).and_then(|p| p.parse::<f64>().ok()).unwrap_or_default();
        match parts[0] {
            "size" => rig.size = (number(1), number(2)),
            "dark" => rig.app.system_dark = true,
            "wizard" => {
                rig.size = installer::SIZE;
                let mode = if parts.get(1) == Some(&"uninstall") { Mode::Uninstall } else { Mode::Install };
                rig.wizard = Some(Wizard::new(mode, waker.clone()));
            }
            // Настоящая пауза для фоновых задач, кадры при этом идут.
            "sleep" => {
                for _ in 0..(number(1) / 16.0) as usize {
                    std::thread::sleep(std::time::Duration::from_millis(16));
                    rig.step();
                }
            }
            "run" => rig.run(number(1)),
            // Фоновые потоки работают в реальном времени: ждём их, не двигая часы схемы.
            "wait" => {
                for _ in 0..200 {
                    std::thread::sleep(std::time::Duration::from_millis(20));
                    let saved = rig.input.time;
                    rig.step();
                    rig.input.time = saved;
                    if !rig.app.scanning && rig.app.session.is_some() {
                        break;
                    }
                }
            }
            "move" => {
                rig.input.pointer = Some(Point::new(number(1), number(2)));
                rig.run(0.3);
            }
            "click" => {
                rig.input.pointer = Some(Point::new(number(1), number(2)));
                rig.step();
                rig.input.pressed = true;
                rig.input.down = true;
                rig.step();
                rig.input.released = true;
                rig.input.down = false;
                rig.step();
            }
            "wheel" => {
                rig.input.pointer = Some(Point::new(number(1), number(2)));
                rig.step();
                rig.input.wheel = number(3);
                rig.step();
            }
            "key" => {
                if let Some(key) = parts.get(1).and_then(|n| key_named(n)) {
                    rig.input.keys.push(key);
                    rig.step();
                }
            }
            "text" => {
                rig.input.text = parts[1..].join(" ");
                rig.step();
            }
            "shot" => {
                let base = rig.app.palette().ground;
                let (w, h) = rig.size;
                let start = std::time::Instant::now();
                rig.step();
                let encoded = start.elapsed();
                gpu.render_png(&rig.scene, w as u32, h as u32, base, Path::new(parts[1]))?;
                println!("{}: кадр {encoded:?}, вместе с GPU {:?}", parts[1], start.elapsed());
            }
            other => return Err(format!("неизвестная команда: {other}")),
        }
    }
    Ok(())
}
