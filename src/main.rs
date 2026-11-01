#![windows_subsystem = "windows"]

mod app;
mod fmt;
mod git;
mod layout;
mod map;
mod panel;
mod scan;
#[cfg(debug_assertions)]
mod shot;
mod theme;

use eframe::egui;
use std::path::PathBuf;

fn main() -> eframe::Result {
    let mut args = app::StartArgs { path: None, theme: None };
    let mut rest = std::env::args().skip(1);
    while let Some(arg) = rest.next() {
        if arg == "--theme" {
            args.theme = match rest.next().as_deref() {
                Some("light") => Some("light"),
                Some("dark") => Some("dark"),
                _ => None,
            };
        } else {
            args.path = Some(PathBuf::from(arg));
        }
    }
    let viewport = egui::ViewportBuilder::default()
        .with_title("Карта веток")
        .with_app_id("gitmap")
        .with_inner_size([1480.0, 920.0])
        .with_min_inner_size([960.0, 600.0])
        .with_icon(egui::IconData {
            rgba: include_bytes!("../assets/icon-64.rgba").to_vec(),
            width: 64,
            height: 64,
        });
    // Отладочная сборка в проверках не должна отнимать фокус у того, что открыто на экране.
    #[cfg(debug_assertions)]
    let viewport = viewport.with_active(std::env::var_os("GITMAP_BACKGROUND").is_none());
    let options = eframe::NativeOptions { viewport, ..Default::default() };
    eframe::run_native("gitmap", options, Box::new(move |cc| Ok(Box::new(app::App::new(cc, args)))))
}
