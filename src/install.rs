//! Установка и удаление. Программа ставит сама себя: копирует свой exe, создаёт ярлыки
//! и запись в «Приложениях». Всё для одного пользователя: права администратора
//! ей не нужны, и окно UAC здесь было бы лишним.

use crate::sys::{self, Folder};
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;
use winreg::RegKey;
use winreg::enums::HKEY_CURRENT_USER;

pub const APP_NAME: &str = "Карта веток";
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
pub const DIR_NAME: &str = "GitMap";
const EXE_NAME: &str = "gitmap.exe";
const UNINSTALL_KEY: &str = r"SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall\GitMap";
const CREATE_NO_WINDOW: u32 = 0x0800_0000;
const DETACHED_PROCESS: u32 = 0x0000_0008;
/// Скачанные обновления лежат в папке установки и убираются при следующем запуске.
pub const UPDATES_DIR: &str = "updates";
/// Открытая программа успевает сохранить настройки и закрыться сама.
const STOP_WAIT_SECONDS: u32 = 6;

#[derive(Clone)]
pub struct InstallOptions {
    pub dir: PathBuf,
    pub start_menu: bool,
    pub desktop: bool,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Step {
    StopRunning,
    CopyFiles,
    Shortcuts,
    Register,
    RemoveShortcuts,
    Unregister,
    RemoveSettings,
    RemoveFiles,
}

impl Step {
    pub fn title(self) -> &'static str {
        match self {
            Step::StopRunning => "Закрываю запущенную программу",
            Step::CopyFiles => "Копирую файлы",
            Step::Shortcuts => "Создаю ярлыки",
            Step::Register => "Добавляю в «Приложения» Windows",
            Step::RemoveShortcuts => "Удаляю ярлыки",
            Step::Unregister => "Убираю из «Приложений» Windows",
            Step::RemoveSettings => "Удаляю настройки",
            Step::RemoveFiles => "Удаляю файлы программы",
        }
    }
}

pub fn install_plan() -> Vec<Step> {
    vec![Step::StopRunning, Step::CopyFiles, Step::Shortcuts, Step::Register]
}

pub fn uninstall_plan(remove_settings: bool) -> Vec<Step> {
    let mut plan = vec![Step::StopRunning, Step::RemoveShortcuts, Step::Unregister];
    if remove_settings {
        plan.push(Step::RemoveSettings);
    }
    plan.push(Step::RemoveFiles);
    plan
}

pub fn settings_dir() -> Option<PathBuf> {
    sys::known_folder(Folder::RoamingAppData).map(|dir| dir.join(DIR_NAME))
}

pub fn default_dir() -> PathBuf {
    let base = sys::known_folder(Folder::LocalAppData).unwrap_or_else(|| PathBuf::from("."));
    base.join("Programs").join(DIR_NAME)
}

fn uninstall_key() -> Option<RegKey> {
    RegKey::predef(HKEY_CURRENT_USER).open_subkey(UNINSTALL_KEY).ok()
}

/// Папка, куда программа уже установлена.
pub fn installed_dir() -> Option<PathBuf> {
    uninstall_key()?
        .get_value::<String, _>("InstallLocation")
        .ok()
        .map(PathBuf::from)
        .filter(|dir| dir.join(EXE_NAME).exists())
}

pub fn installed_version() -> Option<String> {
    installed_dir()?;
    uninstall_key()?.get_value("DisplayVersion").ok()
}

fn same_path(a: &Path, b: &Path) -> bool {
    match (a.canonicalize(), b.canonicalize()) {
        (Ok(a), Ok(b)) => a == b,
        _ => false,
    }
}

pub fn running_installed() -> bool {
    let Some(dir) = installed_dir() else { return false };
    std::env::current_exe().is_ok_and(|exe| same_path(&exe, &dir.join(EXE_NAME)))
}

fn start_menu_link() -> Option<PathBuf> {
    sys::known_folder(Folder::Programs).map(|dir| dir.join(format!("{APP_NAME}.lnk")))
}

fn desktop_link() -> Option<PathBuf> {
    sys::known_folder(Folder::Desktop).map(|dir| dir.join(format!("{APP_NAME}.lnk")))
}

pub fn current_options() -> InstallOptions {
    InstallOptions {
        dir: installed_dir().unwrap_or_else(default_dir),
        start_menu: start_menu_link().is_some_and(|link| link.exists()),
        desktop: desktop_link().is_some_and(|link| link.exists()),
    }
}

pub fn run_install_step(step: Step, opts: &InstallOptions) -> Result<(), String> {
    let exe = opts.dir.join(EXE_NAME);
    match step {
        Step::StopRunning => {
            stop_running(&exe);
            Ok(())
        }
        Step::CopyFiles => copy_self(&opts.dir, &exe),
        Step::Shortcuts => {
            for (want, link) in [(opts.start_menu, start_menu_link()), (opts.desktop, desktop_link())] {
                let Some(link) = link else { continue };
                if want {
                    create_shortcut(&link, &exe)?;
                } else {
                    let _ = std::fs::remove_file(link);
                }
            }
            Ok(())
        }
        Step::Register => register_uninstall(&opts.dir, &exe),
        _ => Ok(()),
    }
}

/// `dir` мастер запоминает до начала: запись в реестре к последнему шагу уже удалена.
pub fn run_uninstall_step(step: Step, dir: Option<&Path>) -> Result<(), String> {
    match step {
        Step::StopRunning => {
            if let Some(dir) = dir {
                stop_running(&dir.join(EXE_NAME));
            }
            Ok(())
        }
        Step::RemoveShortcuts => {
            for link in [start_menu_link(), desktop_link()].into_iter().flatten() {
                let _ = std::fs::remove_file(link);
            }
            Ok(())
        }
        Step::Unregister => match RegKey::predef(HKEY_CURRENT_USER).delete_subkey_all(UNINSTALL_KEY) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(format!("Не удалось убрать запись: {e}")),
            _ => Ok(()),
        },
        Step::RemoveSettings => {
            let Some(settings) = settings_dir() else { return Ok(()) };
            match std::fs::remove_dir_all(settings) {
                Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(format!("Не удалось удалить настройки: {e}")),
                _ => Ok(()),
            }
        }
        Step::RemoveFiles => {
            // Только свои файлы: папку могли выбрать общую, и чужое в ней должно остаться.
            let Some(dir) = dir else { return Ok(()) };
            let _ = std::fs::remove_dir_all(dir.join(UPDATES_DIR));
            if running_from(dir) {
                schedule_removal(dir);
                return Ok(());
            }
            match std::fs::remove_file(dir.join(EXE_NAME)) {
                Err(e) if e.kind() != std::io::ErrorKind::NotFound => {
                    Err(format!("Не удалось удалить {}: {e}", dir.join(EXE_NAME).display()))
                }
                _ => {
                    let _ = std::fs::remove_dir(dir);
                    Ok(())
                }
            }
        }
        _ => Ok(()),
    }
}

fn running_from(dir: &Path) -> bool {
    std::env::current_exe().ok().and_then(|exe| exe.parent().map(|parent| same_path(parent, dir))).unwrap_or(false)
}

/// Свой exe удалить нельзя, пока он работает: cmd удалит его после выхода,
/// повторяя попытки до двух минут, а затем папку, если в ней больше ничего нет.
fn schedule_removal(dir: &Path) {
    // Знак % в имени папки cmd понял бы как переменную: такую папку не трогаем.
    if dir.to_string_lossy().contains('%') {
        return;
    }
    let (d, e, p) = (dir.display(), dir.join(EXE_NAME), sys::tool("ping"));
    let (e, p) = (e.display(), p.display());
    let script = format!(
        "/C \"for /L %i in (1,1,120) do @(if exist \"{e}\" (del /f /q \"{e}\" 2>nul & \"{p}\" -n 2 127.0.0.1 >nul)) & rmdir \"{d}\" 2>nul\""
    );
    let _ = Command::new(sys::tool("cmd"))
        .raw_arg(script)
        .current_dir(sys::system_dir())
        .creation_flags(CREATE_NO_WINDOW | DETACHED_PROCESS)
        .spawn();
}

/// Открытой программе даётся время закрыться самой, потом она завершается.
fn stop_running(exe: &Path) {
    for _ in 0..STOP_WAIT_SECONDS * 5 {
        if sys::processes_of(exe).is_empty() {
            return;
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    sys::terminate(&sys::processes_of(exe));
    std::thread::sleep(Duration::from_millis(400));
}

fn copy_self(dir: &Path, target: &Path) -> Result<(), String> {
    let source = std::env::current_exe().map_err(|e| e.to_string())?;
    std::fs::create_dir_all(dir).map_err(|e| format!("Не удалось создать {}: {e}", dir.display()))?;
    if same_path(&source, target) {
        return Ok(());
    }
    let mut last_error = None;
    for _ in 0..10 {
        match std::fs::copy(&source, target) {
            Ok(_) => return Ok(()),
            Err(e) => {
                last_error = Some(e);
                std::thread::sleep(Duration::from_millis(400));
            }
        }
    }
    Err(format!(
        "Не удалось скопировать программу в {}: {}. Закрой её, если она открыта, и нажми «Повторить».",
        dir.display(),
        last_error.map(|e| e.to_string()).unwrap_or_default()
    ))
}

fn register_uninstall(dir: &Path, exe: &Path) -> Result<(), String> {
    let write = || -> std::io::Result<()> {
        let (key, _) = RegKey::predef(HKEY_CURRENT_USER).create_subkey(UNINSTALL_KEY)?;
        let exe_text = exe.to_string_lossy().to_string();
        let size_kb = std::fs::metadata(exe).map(|m| (m.len() / 1024) as u32).unwrap_or(0);
        key.set_value("DisplayName", &APP_NAME)?;
        key.set_value("DisplayVersion", &VERSION)?;
        key.set_value("Publisher", &"ServChan")?;
        key.set_value("DisplayIcon", &exe_text)?;
        key.set_value("InstallLocation", &dir.to_string_lossy().to_string())?;
        key.set_value("UninstallString", &format!("\"{exe_text}\" --uninstall"))?;
        key.set_value("NoModify", &1u32)?;
        key.set_value("NoRepair", &1u32)?;
        key.set_value("EstimatedSize", &size_kb)
    };
    write().map_err(|e| format!("Не удалось записать сведения об установке: {e}"))
}

fn create_shortcut(link: &Path, target: &Path) -> Result<(), String> {
    let quote = |p: &Path| p.to_string_lossy().replace('\'', "''");
    let script = format!(
        "$s = (New-Object -ComObject WScript.Shell).CreateShortcut('{}'); \
         $s.TargetPath = '{}'; $s.WorkingDirectory = '{}'; $s.IconLocation = '{},0'; \
         $s.Description = 'История git-репозиториев в виде схемы'; $s.Save()",
        quote(link),
        quote(target),
        quote(target.parent().unwrap_or(Path::new("."))),
        quote(target)
    );
    let output = Command::new(sys::tool("powershell"))
        .args(["-NoProfile", "-NonInteractive", "-Command", &script])
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .map_err(|e| format!("Не удалось создать ярлык {}: {e}", link.display()))?;
    if output.status.success() {
        Ok(())
    } else {
        Err(format!("Не удалось создать ярлык {}: {}", link.display(), String::from_utf8_lossy(&output.stderr).trim()))
    }
}

/// `--update`: тихое обновление без мастера, затем запуск программы. Ярлыки и запись
/// в «Приложениях» не обязательны: их сбой не должен останавливать обновление.
pub fn silent_update() -> Result<(), String> {
    if let Some(have) = installed_version().filter(|v| crate::update::newer(v, VERSION)) {
        return Err(format!("Уже установлена более новая версия {have}, а это {VERSION}."));
    }
    let opts = current_options();
    let mut result = Ok(());
    for step in install_plan() {
        match run_install_step(step, &opts) {
            Err(_) if matches!(step, Step::Shortcuts | Step::Register) => {}
            Err(e) => {
                result = Err(format!("{}: {e}", step.title()));
                break;
            }
            Ok(()) => {}
        }
    }
    // Даже при сбое остаётся прежняя версия: окно должно открыться снова.
    result.and(launch(&opts.dir))
}

pub fn launch(dir: &Path) -> Result<(), String> {
    use windows_sys::Win32::UI::WindowsAndMessaging::AllowSetForegroundWindow;
    // Разрешаем новому окну выйти вперёд: иначе Windows откроет его позади.
    unsafe { AllowSetForegroundWindow(u32::MAX) };
    Command::new(dir.join(EXE_NAME))
        .creation_flags(DETACHED_PROCESS)
        .spawn()
        .map(|_| ())
        .map_err(|e| format!("Не удалось запустить программу: {e}"))
}

/// Установщик, с которого шло обновление, уже отработал и больше не нужен.
pub fn clean_downloaded_updates() {
    if let Some(dir) = installed_dir() {
        let _ = std::fs::remove_dir_all(dir.join(UPDATES_DIR));
    }
}
