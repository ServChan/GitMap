//! Только для отладочной сборки: снимок окна по файлу-сигналу, чтобы проверять вид
//! программы, не вытаскивая её поверх других окон. Переменная `GITMAP_SHOT` задаёт
//! путь к снимку (BMP), появление рядом файла `.go` просит снять кадр.

use eframe::egui;
use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::Duration;

pub fn tick(ctx: &egui::Context) {
    let Some(path) = std::env::var_os("GITMAP_SHOT").map(PathBuf::from) else { return };
    ctx.request_repaint_after(Duration::from_millis(120));
    let signal = path.with_extension("go");
    if signal.exists() {
        let _ = std::fs::remove_file(signal);
        let callback = egui::ScreenshotCallback::new(move |image| {
            let _ = std::fs::write(&path, bmp(&image));
        });
        ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(callback));
    }
}

/// Команды из файла `.do`: `move x y`, `click x y`, `wheel x y delta`, `key Name`,
/// `text ...`. Каждое событие уходит в свой кадр, как при настоящем вводе.
pub fn inject(raw: &mut egui::RawInput) {
    static QUEUE: Mutex<VecDeque<egui::Event>> = Mutex::new(VecDeque::new());
    let Some(path) = std::env::var_os("GITMAP_SHOT").map(PathBuf::from) else { return };
    let script = path.with_extension("do");
    let mut queue = QUEUE.lock().unwrap();
    if let Ok(text) = std::fs::read_to_string(&script) {
        let _ = std::fs::remove_file(script);
        for line in text.lines() {
            let parts: Vec<&str> = line.split_whitespace().collect();
            let number = |i: usize| parts.get(i).and_then(|p| p.parse::<f32>().ok()).unwrap_or_default();
            let pos = egui::pos2(number(1), number(2));
            let button = |pressed| egui::Event::PointerButton {
                pos,
                button: egui::PointerButton::Primary,
                pressed,
                modifiers: egui::Modifiers::NONE,
            };
            match parts.first().copied() {
                Some("move") => queue.push_back(egui::Event::PointerMoved(pos)),
                Some("click") => {
                    queue.push_back(egui::Event::PointerMoved(pos));
                    queue.push_back(button(true));
                    queue.push_back(button(false));
                }
                Some("wheel") => {
                    queue.push_back(egui::Event::PointerMoved(pos));
                    queue.push_back(egui::Event::MouseWheel {
                        unit: egui::MouseWheelUnit::Point,
                        delta: egui::vec2(0.0, number(3)),
                        phase: egui::TouchPhase::Move,
                        modifiers: egui::Modifiers::NONE,
                        source: egui::MouseWheelSource::Wheel,
                    });
                }
                Some("key") => {
                    if let Some(key) = parts.get(1).and_then(|n| egui::Key::from_name(n)) {
                        for pressed in [true, false] {
                            queue.push_back(egui::Event::Key {
                                key,
                                physical_key: None,
                                pressed,
                                repeat: false,
                                modifiers: egui::Modifiers::NONE,
                            });
                        }
                    }
                }
                Some("text") => queue.push_back(egui::Event::Text(parts[1..].join(" "))),
                _ => {}
            }
        }
    }
    raw.events.extend(queue.pop_front());
}

fn bmp(image: &egui::ColorImage) -> Vec<u8> {
    let (width, height) = (image.size[0] as u32, image.size[1] as u32);
    let mut out = Vec::with_capacity(54 + image.pixels.len() * 4);
    out.extend(b"BM");
    out.extend((54 + width * height * 4).to_le_bytes());
    out.extend(0u32.to_le_bytes());
    out.extend(54u32.to_le_bytes());
    out.extend(40u32.to_le_bytes());
    out.extend(width.to_le_bytes());
    out.extend((-(height as i32)).to_le_bytes());
    out.extend(1u16.to_le_bytes());
    out.extend(32u16.to_le_bytes());
    out.extend([0u8; 24]);
    for pixel in &image.pixels {
        out.extend([pixel.b(), pixel.g(), pixel.r(), 255]);
    }
    out
}
