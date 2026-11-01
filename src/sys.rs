//! Обёртки над Windows: системные папки, блокировка второго установщика, процессы.

use std::path::{Path, PathBuf};
use windows_sys::Win32::Foundation::{CloseHandle, ERROR_ALREADY_EXISTS, GetLastError, HANDLE, INVALID_HANDLE_VALUE};
use windows_sys::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW, TH32CS_SNAPPROCESS,
};
use windows_sys::Win32::System::SystemInformation::GetSystemDirectoryW;
use windows_sys::Win32::System::Threading::{
    CreateMutexW, GetCurrentProcessId, OpenProcess, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
    PROCESS_TERMINATE, QueryFullProcessImageNameW, TerminateProcess,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{MB_ICONWARNING, MB_OK, MessageBoxW};

#[derive(Clone, Copy)]
pub enum Folder {
    LocalAppData,
    Programs,
    Desktop,
    RoamingAppData,
}

pub fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}

/// Папка Windows. Берётся у системы: переменные окружения могут быть подменены.
pub fn known_folder(folder: Folder) -> Option<PathBuf> {
    use windows_sys::Win32::UI::Shell::{
        FOLDERID_Desktop, FOLDERID_LocalAppData, FOLDERID_Programs, FOLDERID_RoamingAppData, SHGetKnownFolderPath,
    };
    let id = match folder {
        Folder::LocalAppData => FOLDERID_LocalAppData,
        Folder::Programs => FOLDERID_Programs,
        Folder::Desktop => FOLDERID_Desktop,
        Folder::RoamingAppData => FOLDERID_RoamingAppData,
    };
    let mut out: windows_sys::core::PWSTR = std::ptr::null_mut();
    let hr = unsafe { SHGetKnownFolderPath(&id, 0, std::ptr::null_mut(), &mut out) };
    let path = (hr >= 0 && !out.is_null()).then(|| {
        let len = (0..).take_while(|&i| unsafe { *out.add(i) } != 0).count();
        PathBuf::from(String::from_utf16_lossy(unsafe { std::slice::from_raw_parts(out, len) }))
    });
    unsafe { windows_sys::Win32::System::Com::CoTaskMemFree(out as *const _) };
    path
}

/// Полный путь к утилите Windows: иначе могла бы запуститься одноимённая из текущей папки.
pub fn tool(name: &str) -> PathBuf {
    let dir = system_dir();
    match name {
        "powershell" => dir.join(r"WindowsPowerShell\v1.0\powershell.exe"),
        other => dir.join(format!("{other}.exe")),
    }
}

pub fn system_dir() -> PathBuf {
    let mut buffer = [0u16; 260];
    let len = unsafe { GetSystemDirectoryW(buffer.as_mut_ptr(), buffer.len() as u32) } as usize;
    PathBuf::from(String::from_utf16_lossy(&buffer[..len.min(buffer.len())]))
}

pub fn message_box(text: &str) {
    let (text, title) = (wide(text), wide(crate::install::APP_NAME));
    unsafe { MessageBoxW(std::ptr::null_mut(), text.as_ptr(), title.as_ptr(), MB_OK | MB_ICONWARNING) };
}

/// Держится, пока открыт мастер: второй установщик не запустится.
pub struct SetupLock(HANDLE);

impl Drop for SetupLock {
    fn drop(&mut self) {
        unsafe { CloseHandle(self.0) };
    }
}

// Дескриптор мьютекса не привязан к потоку, его только закрывают при выходе.
unsafe impl Send for SetupLock {}

pub fn setup_lock() -> Option<SetupLock> {
    let name = wide("Local\\GitMapSetup");
    let handle = unsafe { CreateMutexW(std::ptr::null(), 0, name.as_ptr()) };
    if handle.is_null() {
        return None;
    }
    if unsafe { GetLastError() } == ERROR_ALREADY_EXISTS {
        unsafe { CloseHandle(handle) };
        return None;
    }
    Some(SetupLock(handle))
}

/// Запущенные копии именно этого файла, кроме текущего процесса. Сравнивается полный
/// путь: сборка из папки проекта называется так же, но её трогать нельзя.
pub fn processes_of(exe: &Path) -> Vec<u32> {
    let Some(name) = exe.file_name().map(|n| n.to_string_lossy().to_lowercase()) else { return Vec::new() };
    let target = exe.to_string_lossy().to_lowercase();
    let me = unsafe { GetCurrentProcessId() };
    let snapshot = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) };
    if snapshot == INVALID_HANDLE_VALUE {
        return Vec::new();
    }
    let mut found = Vec::new();
    let mut entry: PROCESSENTRY32W = unsafe { std::mem::zeroed() };
    entry.dwSize = size_of::<PROCESSENTRY32W>() as u32;
    let mut more = unsafe { Process32FirstW(snapshot, &mut entry) } != 0;
    while more {
        let len = entry.szExeFile.iter().position(|&c| c == 0).unwrap_or(entry.szExeFile.len());
        let process = String::from_utf16_lossy(&entry.szExeFile[..len]).to_lowercase();
        if process == name && entry.th32ProcessID != me && image_path(entry.th32ProcessID).as_deref() == Some(&target) {
            found.push(entry.th32ProcessID);
        }
        more = unsafe { Process32NextW(snapshot, &mut entry) } != 0;
    }
    unsafe { CloseHandle(snapshot) };
    found
}

fn image_path(pid: u32) -> Option<String> {
    let process = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
    if process.is_null() {
        return None;
    }
    let mut buffer = [0u16; 1024];
    let mut size = buffer.len() as u32;
    let ok = unsafe { QueryFullProcessImageNameW(process, PROCESS_NAME_WIN32, buffer.as_mut_ptr(), &mut size) } != 0;
    unsafe { CloseHandle(process) };
    ok.then(|| String::from_utf16_lossy(&buffer[..size as usize]).to_lowercase())
}

pub fn terminate(pids: &[u32]) {
    for &pid in pids {
        let process = unsafe { OpenProcess(PROCESS_TERMINATE, 0, pid) };
        if !process.is_null() {
            unsafe {
                TerminateProcess(process, 1);
                CloseHandle(process);
            }
        }
    }
}

/// Выключены ли анимации в настройках Windows.
pub fn reduced_motion() -> bool {
    use windows_sys::Win32::UI::WindowsAndMessaging::{SPI_GETCLIENTAREAANIMATION, SystemParametersInfoW};
    let mut enabled: i32 = 1;
    let ok = unsafe { SystemParametersInfoW(SPI_GETCLIENTAREAANIMATION, 0, (&raw mut enabled).cast(), 0) };
    ok != 0 && enabled == 0
}

/// Случайный суффикс для имён файлов.
pub fn random_suffix() -> String {
    let mut bytes = [0u8; 8];
    if getrandom::fill(&mut bytes).is_err() {
        let time = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
        bytes = (time as u64).to_le_bytes();
    }
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn folders_and_tools_resolve() {
        for folder in [Folder::LocalAppData, Folder::Programs, Folder::Desktop, Folder::RoamingAppData] {
            assert!(known_folder(folder).is_some_and(|p| p.is_absolute()));
        }
        for name in ["cmd", "ping", "powershell"] {
            assert!(tool(name).exists(), "{name}");
        }
    }

    #[test]
    fn own_process_is_not_listed() {
        let me = std::env::current_exe().unwrap();
        assert!(processes_of(&me).is_empty());
    }
}
