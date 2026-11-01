//! Слежение за открытым репозиторием. Отпечаток состояния дешёвый и проверяется часто,
//! незакоммиченные файлы считаются реже: для этого приходится читать рабочую папку.
//! Поток живёт отдельно от кадров: простаивающее окно ничего не рисует.

use crate::git::{self, FileChange};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

const TICK: Duration = Duration::from_millis(1500);
/// Каждый какой такт пересчитывается рабочая папка.
const WORK_EVERY: u32 = 3;

pub enum Event {
    /// Изменились ветки, HEAD или индекс.
    Changed(PathBuf),
    Work(PathBuf, Vec<FileChange>),
}

#[derive(Clone)]
pub struct Watcher {
    target: Arc<Mutex<Option<PathBuf>>>,
}

fn fingerprint(files: &[FileChange]) -> u64 {
    crate::ui::id(files.iter().map(|f| (format!("{:?}", f.kind), f.path.as_str())).collect::<Vec<_>>())
}

impl Watcher {
    pub fn start(emit: impl Fn(Event) + Send + 'static) -> Self {
        let target: Arc<Mutex<Option<PathBuf>>> = Arc::new(Mutex::new(None));
        let shared = target.clone();
        std::thread::spawn(move || {
            let mut current: Option<PathBuf> = None;
            let (mut stamp, mut work, mut tick) = (0, 0, 0u32);
            loop {
                std::thread::sleep(TICK);
                let wanted = shared.lock().ok().and_then(|t| t.clone());
                let Some(path) = wanted else {
                    current = None;
                    continue;
                };
                let fresh = current.as_ref() != Some(&path);
                let now = git::stamp(&path);
                if fresh {
                    current = Some(path.clone());
                    stamp = now;
                    work = 0;
                    tick = 0;
                }
                let changed = now != stamp;
                if changed {
                    stamp = now;
                    emit(Event::Changed(path.clone()));
                }
                tick += 1;
                if fresh || changed || tick.is_multiple_of(WORK_EVERY) {
                    let files = git::worktree_changes(&path).unwrap_or_default();
                    let print = fingerprint(&files);
                    if fresh || print != work {
                        work = print;
                        emit(Event::Work(path, files));
                    }
                }
            }
        });
        Self { target }
    }

    pub fn follow(&self, path: Option<PathBuf>) {
        if let Ok(mut target) = self.target.lock() {
            *target = path;
        }
    }
}
