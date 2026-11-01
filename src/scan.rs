//! Поиск репозиториев в выбранной папке. Находки уходят в окно по одной, чтобы
//! список наполнялся сразу, даже если папка огромная.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

const MAX_DEPTH: usize = 8;

/// Папки зависимостей и сборок: своих репозиториев там не бывает, а файлов много.
const SKIP: &[&str] = &[
    ".cache", ".cargo", ".claude", ".git", ".gradle", ".idea", ".m2", ".mypy_cache", ".next",
    ".nuget", ".nuxt", ".pnpm-store", ".pytest_cache", ".svelte-kit", ".tox", ".turbo", ".venv",
    ".vs", ".vscode", ".yarn", "$RECYCLE.BIN", "System Volume Information", "__pycache__", "bin",
    "build", "dist", "node_modules", "obj", "out", "target", "venv",
];

pub enum Found {
    Repo(PathBuf),
    Done,
}

/// Обходит `root` до конца или до `cancel`.
pub fn run(root: &Path, cancel: &Arc<AtomicBool>, emit: &dyn Fn(Found)) {
    let mut stack = vec![(root.to_path_buf(), 0usize)];
    while let Some((dir, depth)) = stack.pop() {
        if cancel.load(Ordering::Relaxed) {
            return;
        }
        if is_repo(&dir) {
            emit(Found::Repo(dir.clone()));
        }
        if depth >= MAX_DEPTH {
            continue;
        }
        let Ok(entries) = std::fs::read_dir(&dir) else { continue };
        for entry in entries.flatten() {
            let Ok(kind) = entry.file_type() else { continue };
            // Символические ссылки и точки соединения не открываем: сюда попадают
            // общие кэши сборок, и обход мог бы зациклиться.
            if !kind.is_dir() || kind.is_symlink() {
                continue;
            }
            let name = entry.file_name();
            if SKIP.iter().any(|skip| name.eq_ignore_ascii_case(skip)) {
                continue;
            }
            stack.push((entry.path(), depth + 1));
        }
    }
    emit(Found::Done);
}

pub fn is_repo(dir: &Path) -> bool {
    let dot_git = dir.join(".git");
    // У связанных рабочих копий и подмодулей `.git` это файл.
    if dot_git.exists() {
        return true;
    }
    dir.join("HEAD").is_file() && dir.join("objects").is_dir() && dir.join("refs").is_dir()
}
