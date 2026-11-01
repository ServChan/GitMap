//! Быстрые действия с проектом. Программа только открывает внешние инструменты:
//! сама она репозитории не меняет.

use crate::sys;
use std::os::windows::process::CommandExt;
use std::path::Path;
use std::process::Command;

const CREATE_NO_WINDOW: u32 = 0x0800_0000;

pub fn open_folder(path: &Path) -> Result<(), String> {
    Command::new(sys::system_dir().parent().map_or_else(|| "explorer.exe".into(), |w| w.join("explorer.exe")))
        .arg(path)
        .spawn()
        .map(|_| ())
        .map_err(|e| format!("Не удалось открыть папку: {e}"))
}

/// `code` это пакетный файл из PATH, напрямую его не запустить.
pub fn open_editor(path: &Path) -> Result<(), String> {
    let status = Command::new(sys::tool("cmd"))
        .arg("/C")
        .arg("code")
        .arg(path)
        .creation_flags(CREATE_NO_WINDOW)
        .status()
        .map_err(|e| format!("Не удалось запустить VS Code: {e}"))?;
    if status.success() { Ok(()) } else { Err("VS Code не найден: команды code нет в PATH".to_owned()) }
}

/// Windows Terminal, а если его нет, обычный PowerShell.
pub fn open_terminal(path: &Path) -> Result<(), String> {
    if Command::new("wt.exe").arg("-d").arg(path).spawn().is_ok() {
        return Ok(());
    }
    Command::new(sys::tool("powershell"))
        .current_dir(path)
        .spawn()
        .map(|_| ())
        .map_err(|e| format!("Не удалось открыть терминал: {e}"))
}

/// Адрес приходит из настроек репозитория, поэтому открывается только https.
pub fn open_url(url: &str) -> Result<(), String> {
    if !url.starts_with("https://") || url.chars().any(|c| c.is_whitespace() || c == '"') {
        return Err("Адрес выглядит неправильно".to_owned());
    }
    Command::new(sys::tool("rundll32"))
        .arg("url.dll,FileProtocolHandler")
        .arg(url)
        .spawn()
        .map(|_| ())
        .map_err(|e| format!("Не удалось открыть браузер: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_https_urls_are_opened() {
        assert!(open_url("http://example.com").is_err());
        assert!(open_url("file:///C:/Windows").is_err());
        assert!(open_url("https://a b").is_err());
    }
}
