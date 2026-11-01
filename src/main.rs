#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod anim;
mod app;
mod fmt;
mod gfx;
mod git;
#[cfg(debug_assertions)]
mod headless;
mod index;
mod install;
mod installer;
mod layout;
mod map;
mod panel;
mod scan;
mod shell;
mod sys;
mod text;
mod theme;
mod ui;
mod update;

use std::path::PathBuf;

fn main() {
    let mut args = app::StartArgs { path: None, theme: None };
    let (mut uninstall, mut update, mut portable) = (false, false, false);
    let mut script: Option<String> = None;
    let mut rest = std::env::args().skip(1);
    while let Some(arg) = rest.next() {
        match arg.as_str() {
            "--uninstall" => uninstall = true,
            "--update" => update = true,
            "--portable" => portable = true,
            "--theme" => {
                args.theme = match rest.next().as_deref() {
                    Some("dark") => Some("dark"),
                    Some("light") => Some("light"),
                    _ => None,
                }
            }
            "--script" => script = rest.next(),
            _ => args.path = Some(PathBuf::from(arg)),
        }
    }
    #[cfg(debug_assertions)]
    if let Some(script) = &script {
        if let Err(error) = headless::run(script, args) {
            eprintln!("{error}");
        }
        return;
    }
    let _ = script;
    // Тихое обновление из скачанного установщика: без окна мастера.
    if update {
        if let Err(error) = install::silent_update() {
            sys::message_box(&format!("Обновление не удалось.\n\n{error}"));
        }
        return;
    }
    // Запуск не из папки установки открывает мастер. Отладочной сборке он не мешает.
    let wizard = if uninstall {
        Some(installer::Mode::Uninstall)
    } else if cfg!(debug_assertions) {
        std::env::var_os("GITMAP_DEBUG_INSTALLER").map(|_| installer::Mode::Install)
    } else if !install::running_installed() && !portable {
        Some(installer::Mode::Install)
    } else {
        None
    };
    let lock = match wizard {
        Some(_) => match sys::setup_lock() {
            Some(lock) => Some(lock),
            None => {
                sys::message_box("Установщик уже открыт. Заверши работу с ним.");
                return;
            }
        },
        None => None,
    };
    shell::run(wizard, args, lock);
}