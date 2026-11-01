//! Чтение истории через gix. Программа только читает: ничего не пишет ни в рабочую
//! папку, ни в `.git`, поэтому её можно спокойно наводить на чужие клоны.

use gix::ObjectId;
use gix::bstr::{BString, ByteSlice};
use gix::revision::walk::Sorting;
use gix::traverse::commit::simple::CommitTimeOrder;
use std::collections::hash_map::DefaultHasher;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::hash::{Hash, Hasher};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

pub type Idx = u32;

/// Больше этого история не читается: хватает для любой карты, а схема из миллиона
/// коммитов всё равно не складывается в обозримую картинку.
pub const MAX_COMMITS: usize = 60_000;

/// Сводка считается для сотен репозиториев сразу: на проверку рабочей папки одного
/// отводится это время, дальше счётчик остаётся таким, каким успел стать.
const STATUS_BUDGET: Duration = Duration::from_millis(1500);
const WEEK_SECONDS: i64 = 7 * 24 * 60 * 60;
const STALE_SECONDS: i64 = 90 * 24 * 60 * 60;
const CACHE_BYTES: usize = 8 * 1024 * 1024;

const CONTEXT_LINES: u32 = 3;
const MAX_SIDE_BYTES: u64 = 1024 * 1024;
/// Предел на всё сравнение: тысячи больших файлов не должны съесть память.
const MAX_DIFF_BYTES: u64 = 64 * 1024 * 1024;
const BINARY_PROBE_BYTES: usize = 8 * 1024;

/// Те же имена и в том же порядке, что и у основной ветки на схеме.
const MAIN_NAMES: [&str; 5] = ["main", "master", "trunk", "develop", "dev"];

pub struct Commit {
    pub id: ObjectId,
    /// Индексы родителей, первый родитель идёт первым.
    pub parents: Vec<Idx>,
    pub time: i64,
    pub author: String,
    pub subject: String,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum RefKind {
    Local,
    Remote,
    Tag,
}

pub struct Ref {
    /// Короткое имя: `main`, `origin/main`, `v1.0`.
    pub name: String,
    pub kind: RefKind,
    pub commit: Idx,
}

pub struct Graph {
    pub path: PathBuf,
    /// От новых к старым.
    pub commits: Vec<Commit>,
    pub refs: Vec<Ref>,
    pub head: Option<Idx>,
    pub head_branch: Option<String>,
    /// Основная ветка удалённого репозитория (`origin/HEAD`).
    pub remote_default: Option<String>,
    pub truncated: bool,
}

#[derive(Clone, Copy, Default)]
pub struct Summary {
    pub branches: usize,
    pub last_commit: Option<i64>,
}

#[derive(Clone, Default)]
pub struct Health {
    /// Адрес удалённого репозитория для браузера: только https, без логина и `.git`. Для `git@github.com:a/b.git` тоже.
    pub web_url: Option<String>,
    pub has_remote: bool,
    /// Коммиты локальных веток, которых нет ни в одной удалённой ветке.
    pub unpushed: usize,
    /// Изменённые, добавленные, удалённые и неотслеживаемые файлы рабочей папки.
    pub dirty: usize,
    /// Локальные ветки (кроме текущей), где не было коммитов больше 90 дней и которые не влиты в основную.
    pub stale_branches: Vec<String>,
    /// Коммитов за последние 7 дней по всем веткам.
    pub week_commits: usize,
    /// Коммиты без родителей, достижимые из веток: по ним находятся родственные проекты.
    pub roots: Vec<ObjectId>,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum FileKind {
    Added,
    Modified,
    Deleted,
    Renamed,
}

#[derive(Clone)]
pub struct FileChange {
    pub kind: FileKind,
    pub path: String,
}

pub struct Details {
    pub message: String,
    pub files: Vec<FileChange>,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum LineKind {
    Context,
    Added,
    Removed,
    /// Заголовок участка: «@@ ... @@»
    Hunk,
}

#[derive(Clone)]
pub struct DiffLine {
    pub kind: LineKind,
    pub old: Option<u32>,
    pub new: Option<u32>,
    pub text: String,
}

#[derive(Clone)]
pub struct FileDiff {
    pub kind: FileKind,
    pub path: String,
    pub old_path: Option<String>,
    pub binary: bool,
    /// Файл слишком большой: строки не показаны. Порог 1 МиБ на сторону.
    pub too_large: bool,
    pub added: usize,
    pub removed: usize,
    pub lines: Vec<DiffLine>,
}

fn open(path: &Path) -> Result<gix::Repository, String> {
    gix::open_opts(path, gix::open::Options::isolated()).map_err(|e| e.to_string())
}

/// Открытие для сравнения с рабочей папкой. Изолированный режим не видит системных
/// настроек, а git для Windows обычно держит там `core.autocrlf=true`: без поправки каждый
/// файл с CRLF на диске и LF в истории выглядел бы изменённым. Режим `input` только
/// приводит диск к виду истории и не трогает файлы, где CR записан в самой истории.
/// Фильтры из `.git/config` выключены: чужая команда вроде `git-lfs clean` пишет в `.git`.
/// Всё это правки конфигурации в памяти, на диск они не попадают.
fn open_worktree(path: &Path) -> Result<gix::Repository, String> {
    let mut repo = open(path)?;
    let crlf_is_set = repo.config_snapshot().string("core.autocrlf").is_some();
    let mut config = repo.config_snapshot_mut();
    let drivers: Vec<BString> = config
        .sections_by_name("filter")
        .into_iter()
        .flatten()
        .filter_map(|section| section.header().subsection_name().map(ToOwned::to_owned))
        .collect();
    for name in &drivers {
        config.remove_section("filter", Some(name.as_bstr()));
    }
    if !crlf_is_set {
        config.append_config(["core.autocrlf=input"], gix::config::Source::Api).map_err(|e| e.to_string())?;
    }
    config.commit().map_err(|e| e.to_string())?;
    Ok(repo)
}

/// Быстрая сводка для строки в указателе: без обхода истории.
pub fn summary(path: &Path) -> Result<Summary, String> {
    let repo = open(path)?;
    let mut names = HashSet::new();
    let mut last = None;
    let platform = repo.references().map_err(|e| e.to_string())?;
    let refs = platform.all().map_err(|e| e.to_string())?;
    for reference in refs.flatten() {
        let full = reference.name().as_bstr().to_str_lossy().into_owned();
        let Some((kind, short)) = classify(&full) else { continue };
        if kind == RefKind::Tag || short.ends_with("/HEAD") {
            continue;
        }
        let mut reference = reference;
        let Ok(commit) = reference.peel_to_commit() else { continue };
        names.insert(branch_name(kind, &short).to_owned());
        if let Ok(time) = commit.time() {
            last = last.max(Some(time.seconds));
        }
    }
    Ok(Summary { branches: names.len(), last_commit: last })
}

fn classify(full: &str) -> Option<(RefKind, String)> {
    if let Some(short) = full.strip_prefix("refs/heads/") {
        Some((RefKind::Local, short.to_owned()))
    } else if let Some(short) = full.strip_prefix("refs/remotes/") {
        Some((RefKind::Remote, short.to_owned()))
    } else {
        full.strip_prefix("refs/tags/").map(|short| (RefKind::Tag, short.to_owned()))
    }
}

/// Имя ветки без имени удалённого репозитория.
pub fn branch_name(kind: RefKind, short: &str) -> &str {
    match kind {
        RefKind::Remote => short.split_once('/').map_or(short, |(_, rest)| rest),
        _ => short,
    }
}

type Tip = (String, RefKind, ObjectId);

/// Вершины веток и тегов и имя основной ветки удалённого репозитория (`origin/HEAD`).
fn read_tips(repo: &gix::Repository) -> Result<(Vec<Tip>, Option<String>), String> {
    let mut tips = Vec::new();
    let mut remote_default = None;
    let platform = repo.references().map_err(|e| e.to_string())?;
    for reference in platform.all().map_err(|e| e.to_string())?.flatten() {
        let full = reference.name().as_bstr().to_str_lossy().into_owned();
        let Some((kind, short)) = classify(&full) else { continue };
        if kind == RefKind::Remote && short.ends_with("/HEAD") {
            if let Some(target) = reference.target().try_name() {
                let target = target.as_bstr().to_str_lossy().into_owned();
                if let Some((RefKind::Remote, name)) = classify(&target) {
                    remote_default.get_or_insert(branch_name(RefKind::Remote, &name).to_owned());
                }
            }
            continue;
        }
        let mut reference = reference;
        // Тег может указывать не на коммит: на карте ему делать нечего.
        if let Ok(commit) = reference.peel_to_commit() {
            tips.push((short, kind, commit.id));
        }
    }
    Ok((tips, remote_default))
}

pub fn load(path: &Path) -> Result<Graph, String> {
    let mut repo = open(path)?;
    repo.object_cache_size_if_unset(64 * 1024 * 1024);

    let (tips, remote_default) = read_tips(&repo)?;
    let head_id = repo.head_id().ok().map(|id| id.detach());
    let head_branch = repo
        .head_name()
        .ok()
        .flatten()
        .and_then(|name| classify(&name.as_bstr().to_str_lossy()))
        .map(|(_, short)| short);

    let mut roots: Vec<ObjectId> = tips.iter().map(|tip| tip.2).collect();
    roots.extend(head_id);
    roots.sort();
    roots.dedup();

    let mut graph = Graph {
        path: path.to_path_buf(),
        commits: Vec::new(),
        refs: Vec::new(),
        head: None,
        head_branch,
        remote_default,
        truncated: false,
    };
    if roots.is_empty() {
        return Ok(graph);
    }

    let walk = repo
        .rev_walk(roots)
        .sorting(Sorting::ByCommitTime(CommitTimeOrder::NewestFirst))
        .use_commit_graph(true)
        .all()
        .map_err(|e| e.to_string())?;

    let mut index: HashMap<ObjectId, Idx> = HashMap::new();
    let mut raw_parents: Vec<Vec<ObjectId>> = Vec::new();
    for info in walk {
        // Обрыв на середине (например, неполный клон) не повод терять уже прочитанное.
        let Ok(info) = info else {
            graph.truncated = true;
            break;
        };
        if graph.commits.len() >= MAX_COMMITS {
            graph.truncated = true;
            break;
        }
        let Ok(object) = info.object() else { continue };
        let Ok(decoded) = object.decode() else { continue };
        let subject = decoded.message.lines().next().unwrap_or_default().to_str_lossy().trim().to_owned();
        let author = decoded.author().map(|a| a.name.to_str_lossy().into_owned()).unwrap_or_default();
        let time = object.time().map(|t| t.seconds).unwrap_or_default();
        index.insert(object.id, graph.commits.len() as Idx);
        raw_parents.push(object.parent_ids().map(|id| id.detach()).collect());
        graph.commits.push(Commit { id: object.id, parents: Vec::new(), time, author, subject });
    }
    for (commit, parents) in graph.commits.iter_mut().zip(raw_parents) {
        commit.parents = parents.iter().filter_map(|id| index.get(id).copied()).collect();
    }
    for (name, kind, id) in tips {
        if let Some(&commit) = index.get(&id) {
            graph.refs.push(Ref { name, kind, commit });
        }
    }
    graph.head = head_id.and_then(|id| index.get(&id).copied());
    Ok(graph)
}

pub fn details(path: &Path, id: ObjectId) -> Result<Details, String> {
    let repo = open(path)?;
    let commit = repo.find_commit(id).map_err(|e| e.to_string())?;
    let decoded = commit.decode().map_err(|e| e.to_string())?;
    let message = decoded.message.to_str_lossy().trim_end().to_owned();

    let tree = commit.tree().map_err(|e| e.to_string())?;
    let parent_tree = match commit.parent_ids().next() {
        Some(parent) => {
            let parent = parent.object().map_err(|e| e.to_string())?.into_commit();
            Some(parent.tree().map_err(|e| e.to_string())?)
        }
        None => None,
    };
    let changes = repo.diff_tree_to_tree(parent_tree.as_ref(), &tree, None).map_err(|e| e.to_string())?;
    let mut files: Vec<FileChange> = changes
        .iter()
        .filter(|change| !change.entry_mode().is_tree())
        .map(|change| {
            use gix::object::tree::diff::ChangeDetached as C;
            let kind = match change {
                C::Addition { .. } => FileKind::Added,
                C::Deletion { .. } => FileKind::Deleted,
                C::Modification { .. } => FileKind::Modified,
                C::Rewrite { .. } => FileKind::Renamed,
            };
            FileChange { kind, path: change.location().to_str_lossy().into_owned() }
        })
        .collect();
    files.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(Details { message, files })
}

struct Node {
    time: i64,
    parents: Vec<ObjectId>,
}

/// Вся история одним проходом: по ней считаются и корни, и неотправленное, и слитые ветки.
/// Отдельный обход на каждый вопрос вышел бы дороже: корни всё равно требуют дойти до конца.
fn read_nodes(repo: &gix::Repository, tips: Vec<ObjectId>) -> HashMap<ObjectId, Node> {
    let mut nodes = HashMap::new();
    if tips.is_empty() {
        return nodes;
    }
    let walk = repo
        .rev_walk(tips)
        .sorting(Sorting::ByCommitTime(CommitTimeOrder::NewestFirst))
        .use_commit_graph(true)
        .all();
    let Ok(walk) = walk else { return nodes };
    // Обрыв на середине только уменьшает сводку: прочитанного не отменяет.
    for info in walk.take(MAX_COMMITS).map_while(Result::ok) {
        let parents = info.parent_ids.iter().copied().collect();
        nodes.insert(info.id, Node { time: info.commit_time.unwrap_or_default(), parents });
    }
    nodes
}

fn reachable(nodes: &HashMap<ObjectId, Node>, tips: impl Iterator<Item = ObjectId>) -> HashSet<ObjectId> {
    let mut seen = HashSet::new();
    let mut stack: Vec<ObjectId> = tips.collect();
    while let Some(id) = stack.pop() {
        if let Some(node) = nodes.get(&id)
            && seen.insert(id)
        {
            stack.extend(node.parents.iter().copied());
        }
    }
    seen
}

/// Вершины основной ветки, локальная и удалённая: влито в любую из них значит влито.
fn main_tips(
    tips: &[(String, RefKind, ObjectId)],
    remote_default: Option<&str>,
    head: Option<ObjectId>,
) -> Vec<ObjectId> {
    let branch_tips = |name: &str| -> Vec<ObjectId> {
        tips.iter()
            .filter(|(short, kind, _)| *kind != RefKind::Tag && branch_name(*kind, short) == name)
            .map(|tip| tip.2)
            .collect()
    };
    remote_default
        .into_iter()
        .chain(MAIN_NAMES)
        .map(branch_tips)
        .find(|found| !found.is_empty())
        .unwrap_or_else(|| head.into_iter().collect())
}

/// Адрес для браузера из адреса удалённого репозитория. Локальные пути и адреса, у которых
/// нет страницы, дают `None`. В scp-записи без точки в имени узла стоит псевдоним из
/// настроек ssh или буква диска: ни то ни другое браузер не откроет.
fn web_url(raw: &str) -> Option<String> {
    let raw = raw.trim();
    let (host, path) = if let Some((scheme, rest)) = raw.split_once("://") {
        let scheme = scheme.to_ascii_lowercase();
        if !matches!(scheme.as_str(), "https" | "http" | "ssh" | "git" | "git+ssh" | "git+https") {
            return None;
        }
        let (authority, path) = rest.split_once('/')?;
        let host = authority.rsplit('@').next()?;
        // Порт ssh или git к веб-странице отношения не имеет.
        let host = if scheme == "https" { host } else { host.split(':').next()? };
        (host, path)
    } else {
        let (left, path) = raw.split_once(':')?;
        let host = left.rsplit('@').next()?;
        if left.contains(['/', '\\']) || !host.contains('.') {
            return None;
        }
        (host, path)
    };
    let path = path.trim_matches('/');
    let path = path.strip_suffix(".git").unwrap_or(path).trim_end_matches('/');
    let unsafe_char = |c: char| c.is_whitespace() || c.is_control() || c == '"' || c == '\\';
    if host.is_empty() || path.is_empty() || host.contains(unsafe_char) || path.contains(unsafe_char) {
        return None;
    }
    Some(format!("https://{host}/{path}"))
}

pub fn health(path: &Path) -> Result<Health, String> {
    let mut repo = open_worktree(path)?;
    repo.object_cache_size_if_unset(CACHE_BYTES);

    let remotes = repo.remote_names();
    let has_remote = !remotes.is_empty();
    let remote = remotes.iter().find(|name| name.as_bytes() == b"origin").or_else(|| remotes.iter().next());
    let web = remote.and_then(|name| repo.config_snapshot().string(format!("remote.{name}.url").as_str()));
    let web_url = web.and_then(|url| self::web_url(&url.to_str_lossy()));

    let (tips, remote_default) = read_tips(&repo)?;
    let head = repo.head_id().ok().map(|id| id.detach());
    let head_branch = repo
        .head_name()
        .ok()
        .flatten()
        .and_then(|name| classify(&name.as_bstr().to_str_lossy()))
        .map(|(_, short)| short);

    let mut starts: Vec<ObjectId> =
        tips.iter().filter(|tip| tip.1 != RefKind::Tag).map(|tip| tip.2).chain(head).collect();
    starts.sort();
    starts.dedup();
    let nodes = read_nodes(&repo, starts);

    let unpushed = if has_remote {
        let ids = |wanted: RefKind| tips.iter().filter(move |tip| tip.1 == wanted).map(|tip| tip.2);
        let pushed = reachable(&nodes, ids(RefKind::Remote));
        reachable(&nodes, ids(RefKind::Local)).difference(&pushed).count()
    } else {
        0
    };

    let now = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |since| since.as_secs() as i64);
    let merged = reachable(&nodes, main_tips(&tips, remote_default.as_deref(), head).into_iter());
    let mut stale_branches: Vec<String> = tips
        .iter()
        .filter(|(short, kind, id)| {
            // Вершины нет среди прочитанного только у очень длинной истории: тогда неизвестно, влита ли она.
            *kind == RefKind::Local
                && Some(short) != head_branch.as_ref()
                && !merged.contains(id)
                && nodes.get(id).is_some_and(|node| now - node.time > STALE_SECONDS)
        })
        .map(|tip| tip.0.clone())
        .collect();
    stale_branches.sort();

    let week_commits = nodes.values().filter(|node| now - node.time <= WEEK_SECONDS).count();
    // У мелкого клона история обрезана: коммит на срезе родителей имеет, просто их здесь нет.
    let shallow = repo.is_shallow().unwrap_or(false);
    let mut roots: Vec<ObjectId> = if shallow {
        Vec::new()
    } else {
        nodes.iter().filter(|(_, node)| node.parents.is_empty()).map(|(id, _)| *id).collect()
    };
    roots.sort();

    // Сбой проверки рабочей папки не повод терять остальную сводку.
    let dirty = match repo.workdir() {
        Some(_) => work_changes(&repo, Some(STATUS_BUDGET)).map_or(0, |changes| changes.len()),
        None => 0,
    };

    Ok(Health { web_url, has_remote, unpushed, dirty, stale_branches, week_commits, roots })
}

/// Отпечаток состояния: меняется вместе с HEAD, ветками, тегами и индексом.
/// Только чтение метаданных файлов (время и размер), без разбора объектов: вызывается раз в пару секунд.
pub fn stamp(path: &Path) -> u64 {
    let (git_dir, common) = git_dirs(path);
    let mut sum = 0u64;
    for file in [git_dir.join("HEAD"), git_dir.join("index"), common.join("packed-refs")] {
        sum = sum.wrapping_add(file_stamp(&file, std::fs::metadata(&file).ok()));
    }
    refs_stamp(&common.join("refs"), &mut sum);
    // У связанной рабочей папки есть и собственные ссылки.
    if git_dir != common {
        refs_stamp(&git_dir.join("refs"), &mut sum);
    }
    sum
}

/// Папка `.git` и общая папка репозитория. В связанной рабочей папке и подмодуле `.git`
/// это файл со ссылкой, а ветки живут в общей папке. Голый репозиторий и есть своя папка.
fn git_dirs(path: &Path) -> (PathBuf, PathBuf) {
    let dot = path.join(".git");
    let git_dir = match std::fs::metadata(&dot) {
        Ok(meta) if meta.is_file() => std::fs::read_to_string(&dot)
            .ok()
            .and_then(|text| text.trim().strip_prefix("gitdir:").map(|target| path.join(target.trim())))
            .unwrap_or(dot),
        Ok(_) => dot,
        Err(_) => path.to_path_buf(),
    };
    let common = std::fs::read_to_string(git_dir.join("commondir"))
        .map_or_else(|_| git_dir.clone(), |text| git_dir.join(text.trim()));
    (git_dir, common)
}

fn file_stamp(path: &Path, meta: Option<std::fs::Metadata>) -> u64 {
    let mut hasher = DefaultHasher::new();
    path.hash(&mut hasher);
    if let Some(meta) = meta {
        meta.len().hash(&mut hasher);
        meta.modified().ok().hash(&mut hasher);
    }
    hasher.finish()
}

/// Складывается, а не хешируется подряд: порядок чтения папки не должен влиять на итог.
fn refs_stamp(dir: &Path, sum: &mut u64) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let Ok(kind) = entry.file_type() else { continue };
        let path = entry.path();
        if kind.is_dir() {
            refs_stamp(&path, sum);
        } else {
            *sum = sum.wrapping_add(file_stamp(&path, entry.metadata().ok()));
        }
    }
}

struct WorkChange {
    kind: FileKind,
    path: String,
    old_path: Option<String>,
}

/// Изменения относительно HEAD. git отдаёт две половины: HEAD против индекса и индекс
/// против диска. Для каждого файла из них выводится одно: был ли он в HEAD и есть ли на диске.
/// `budget` значит быстрый проход для сводки: один поток, без подмодулей и без поиска
/// переименований, поэтому переименование там считается за два файла. Проход ограничен по времени.
fn work_changes(repo: &gix::Repository, budget: Option<Duration>) -> Result<Vec<WorkChange>, String> {
    use gix::diff::index::Change;
    use gix::status::index_worktree::Item as DiskItem;
    use gix::status::plumbing::index_as_worktree_with_renames::Summary;
    use gix::status::tree_index::TrackRenames;
    use gix::status::{Item, Submodule, UntrackedFiles};

    let stop = Arc::new(AtomicBool::new(false));
    let platform = repo
        .status(gix::progress::Discard)
        .map_err(|e| e.to_string())?
        .untracked_files(UntrackedFiles::Files)
        .should_interrupt_owned(stop.clone());
    // Результат прохода назад в индекс не записывается: `Outcome::write_changes` не вызывается.
    let platform = if budget.is_some() {
        platform
            .index_worktree_submodules(None::<Submodule>)
            .tree_index_track_renames(TrackRenames::Disabled)
            .index_worktree_options_mut(|options| options.thread_limit = Some(1))
    } else {
        let ignore = gix::submodule::config::Ignore::Dirty;
        platform
            .index_worktree_submodules(Submodule::Given { ignore, check_dirty: false })
            .index_worktree_rewrites(Some(gix::diff::Rewrites::default()))
    };

    let watchdog = budget.map(|budget| {
        let (done, wait) = mpsc::channel::<()>();
        let stop = stop.clone();
        std::thread::spawn(move || {
            if wait.recv_timeout(budget) == Err(mpsc::RecvTimeoutError::Timeout) {
                stop.store(true, Ordering::Relaxed);
            }
        });
        done
    });
    let (mut staged, mut on_disk) = (Vec::new(), Vec::new());
    for item in platform.into_iter(None).map_err(|e| e.to_string())? {
        match item {
            Ok(Item::TreeIndex(change)) => staged.push(change),
            Ok(Item::IndexWorktree(item)) => on_disk.push(item),
            // Остановка по времени приходит как ошибка: найденного до неё она не отменяет.
            Err(_) if stop.load(Ordering::Relaxed) => break,
            Err(error) => return Err(error.to_string()),
        }
    }
    drop(watchdog);

    let text = |path: &gix::bstr::BStr| path.to_str_lossy().into_owned();
    // Для каждого пути: есть ли он в HEAD и есть ли на диске.
    let mut sides: BTreeMap<String, (bool, bool)> = BTreeMap::new();
    let mut moves: Vec<(String, String)> = Vec::new();
    for change in staged {
        match change {
            Change::Addition { location, .. } => {
                sides.insert(text(location.as_ref()), (false, true));
            }
            Change::Deletion { location, .. } => {
                sides.insert(text(location.as_ref()), (true, false));
            }
            Change::Modification { location, .. } => {
                sides.insert(text(location.as_ref()), (true, true));
            }
            Change::Rewrite { source_location, location, copy, .. } => {
                let (source, target) = (text(source_location.as_ref()), text(location.as_ref()));
                sides.insert(target.clone(), (false, true));
                if !copy {
                    sides.insert(source.clone(), (true, false));
                    moves.push((source, target));
                }
            }
        }
    }
    for item in on_disk {
        let path = text(item.rela_path());
        if let DiskItem::Rewrite { source, copy, .. } = &item {
            sides.entry(path.clone()).or_insert((false, true)).1 = true;
            if !copy {
                let source = text(source.rela_path());
                sides.entry(source.clone()).or_insert((true, true)).1 = false;
                moves.push((source, path));
            }
            continue;
        }
        match item.summary() {
            None => {}
            Some(Summary::Removed) => sides.entry(path).or_insert((true, true)).1 = false,
            Some(Summary::Added | Summary::IntentToAdd) => sides.entry(path).or_insert((false, true)).1 = true,
            Some(_) => {
                sides.entry(path).or_insert((true, true));
            }
        }
    }

    // Переименование считается только там, где исходный файл и правда исчез, а новый появился.
    let mut sources = HashMap::new();
    for (source, target) in moves {
        if sides.get(&source) == Some(&(true, false)) && sides.get(&target) == Some(&(false, true)) {
            sides.remove(&source);
            sources.insert(target, source);
        }
    }
    let changes = sides.into_iter().filter_map(|(path, sides)| {
        let old_path = sources.remove(&path);
        let kind = match sides {
            _ if old_path.is_some() => FileKind::Renamed,
            (false, true) => FileKind::Added,
            (true, false) => FileKind::Deleted,
            (true, true) => FileKind::Modified,
            // Добавлен в индекс и удалён с диска: для HEAD ничего не случилось.
            (false, false) => return None,
        };
        Some(WorkChange { kind, path, old_path })
    });
    Ok(changes.collect())
}

/// Незакоммиченные изменения: индекс и рабочая папка против HEAD, с неотслеживаемыми (с учётом .gitignore).
pub fn worktree_changes(path: &Path) -> Result<Vec<FileChange>, String> {
    let repo = open_worktree(path)?;
    if repo.workdir().is_none() {
        return Ok(Vec::new());
    }
    let changes = work_changes(&repo, None)?;
    Ok(changes.into_iter().map(|change| FileChange { kind: change.kind, path: change.path }).collect())
}

/// Остаток допустимого объёма на одно сравнение.
struct Budget(u64);

impl Budget {
    fn take(&mut self, size: u64) -> bool {
        let fits = size <= MAX_SIDE_BYTES && size <= self.0;
        if fits {
            self.0 -= size;
        }
        fits
    }
}

/// `None` значит, что содержимое не показывается: оно велико или его не удалось прочитать
/// (неполный клон, файл занят другой программой).
type Content = Option<Vec<u8>>;

fn subproject_text(id: impl std::fmt::Display) -> Vec<u8> {
    format!("Subproject commit {id}\n").into_bytes()
}

fn blob_content(repo: &gix::Repository, id: ObjectId, budget: &mut Budget) -> Content {
    let size = repo.find_header(id).ok()?.size();
    if !budget.take(size) {
        return None;
    }
    Some(repo.find_object(id).ok()?.detach().data)
}

/// В дереве подмодуль записан ссылкой на коммит чужого репозитория: показывается как у git, одной строкой.
fn entry_content(
    repo: &gix::Repository,
    mode: gix::object::tree::EntryMode,
    id: ObjectId,
    budget: &mut Budget,
) -> Content {
    if mode.is_commit() { Some(subproject_text(id)) } else { blob_content(repo, id, budget) }
}

/// Содержимое файла с диска в том виде, в каком оно попало бы в историю.
fn disk_content(
    root: &Path,
    relative: &str,
    filter: &mut gix::filter::Pipeline<'_>,
    index: &gix::index::State,
    budget: &mut Budget,
) -> Content {
    use gix::filter::plumbing::pipeline::convert::ToGitOutcome;

    let full = root.join(relative);
    let meta = std::fs::symlink_metadata(&full).ok()?;
    if meta.file_type().is_symlink() {
        // git хранит в блобе путь, на который указывает ссылка.
        let target = std::fs::read_link(&full).ok()?;
        return Some(target.to_string_lossy().replace('\\', "/").into_bytes());
    }
    if meta.is_dir() {
        let head = gix::open_opts(&full, gix::open::Options::isolated()).ok()?.head_id().ok()?.detach();
        return Some(subproject_text(head));
    }
    if !budget.take(meta.len()) {
        return None;
    }
    let file = std::fs::File::open(&full).ok()?;
    let mut data = Vec::new();
    match filter.convert_to_git(file, Path::new(relative), index).ok()? {
        ToGitOutcome::Unchanged(mut file) => file.read_to_end(&mut data).ok()?,
        ToGitOutcome::Process(mut stream) => stream.read_to_end(&mut data).ok()?,
        ToGitOutcome::Buffer(buffer) => {
            data.extend_from_slice(buffer);
            data.len()
        }
    };
    Some(data)
}

fn is_binary(data: &[u8]) -> bool {
    data[..data.len().min(BINARY_PROBE_BYTES)].contains(&0)
}

/// Текст строки без перевода строки. Сами строки сравниваются вместе с ним, поэтому смена
/// CRLF на LF и появление перевода в конце файла видны как изменение, как и в git.
fn shown_text(line: &[u8]) -> String {
    let line = line.strip_suffix(b"\n").unwrap_or(line);
    let line = line.strip_suffix(b"\r").unwrap_or(line);
    String::from_utf8_lossy(line).into_owned()
}

fn hunk_range(start: u32, len: u32) -> String {
    match len {
        0 => format!("{start},0"),
        1 => format!("{}", start + 1),
        _ => format!("{},{len}", start + 1),
    }
}

fn line_diff(old: &[u8], new: &[u8]) -> (Vec<DiffLine>, usize, usize) {
    use gix::diff::blob::{Algorithm, InternedInput, diff_with_slider_heuristics};

    let input = InternedInput::new(old, new);
    let diff = diff_with_slider_heuristics(Algorithm::Histogram, &input);
    let hunks: Vec<_> = diff.hunks().collect();
    let before = |at: u32| shown_text(input.interner[input.before[at as usize]]);
    let after = |at: u32| shown_text(input.interner[input.after[at as usize]]);
    let before_len = input.before.len() as u32;

    let mut lines = Vec::new();
    let (mut added, mut removed) = (0, 0);
    let mut first = 0;
    while first < hunks.len() {
        // Правки, между которыми умещается только их общий контекст, идут одним участком.
        let mut last = first;
        while last + 1 < hunks.len() && hunks[last + 1].before.start - hunks[last].before.end <= CONTEXT_LINES * 2 {
            last += 1;
        }
        let (head, tail) = (&hunks[first], &hunks[last]);
        let lead = head.before.start.min(CONTEXT_LINES);
        let trail = (before_len - tail.before.end).min(CONTEXT_LINES);
        let (start_old, start_new) = (head.before.start - lead, head.after.start - lead);
        let (end_old, end_new) = (tail.before.end + trail, tail.after.end + trail);
        lines.push(DiffLine {
            kind: LineKind::Hunk,
            old: None,
            new: None,
            text: format!(
                "@@ -{} +{} @@",
                hunk_range(start_old, end_old - start_old),
                hunk_range(start_new, end_new - start_new)
            ),
        });

        let (mut at_old, mut at_new) = (start_old, start_new);
        let context = |lines: &mut Vec<DiffLine>, at_old: u32, at_new: u32| {
            lines.push(DiffLine {
                kind: LineKind::Context,
                old: Some(at_old + 1),
                new: Some(at_new + 1),
                text: before(at_old),
            });
        };
        for hunk in &hunks[first..=last] {
            while at_old < hunk.before.start {
                context(&mut lines, at_old, at_new);
                at_old += 1;
                at_new += 1;
            }
            for at in hunk.before.clone() {
                lines.push(DiffLine { kind: LineKind::Removed, old: Some(at + 1), new: None, text: before(at) });
                removed += 1;
            }
            for at in hunk.after.clone() {
                lines.push(DiffLine { kind: LineKind::Added, old: None, new: Some(at + 1), text: after(at) });
                added += 1;
            }
            at_old = hunk.before.end;
            at_new = hunk.after.end;
        }
        while at_old < end_old {
            context(&mut lines, at_old, at_new);
            at_old += 1;
            at_new += 1;
        }
        first = last + 1;
    }
    (lines, added, removed)
}

fn file_diff(kind: FileKind, path: String, old_path: Option<String>, old: Content, new: Content) -> FileDiff {
    let mut diff = FileDiff {
        kind,
        path,
        old_path,
        binary: false,
        too_large: false,
        added: 0,
        removed: 0,
        lines: Vec::new(),
    };
    match (old, new) {
        (Some(old), Some(new)) if is_binary(&old) || is_binary(&new) => diff.binary = true,
        (Some(old), Some(new)) => (diff.lines, diff.added, diff.removed) = line_diff(&old, &new),
        _ => diff.too_large = true,
    }
    diff
}

/// Различия между двумя коммитами. `from: None` значит первый родитель `to` (для корневого коммита пустое дерево).
/// Три строки контекста. Файлы по алфавиту.
pub fn diff_commits(path: &Path, from: Option<ObjectId>, to: ObjectId) -> Result<Vec<FileDiff>, String> {
    use gix::object::tree::diff::ChangeDetached as C;

    let repo = open(path)?;
    let target = repo.find_commit(to).map_err(|e| e.to_string())?;
    let base = from.or_else(|| target.parent_ids().next().map(|id| id.detach()));
    let base_tree = match base {
        Some(id) => {
            let commit = repo.find_commit(id).map_err(|e| e.to_string())?;
            Some(commit.tree().map_err(|e| e.to_string())?)
        }
        None => None,
    };
    let target_tree = target.tree().map_err(|e| e.to_string())?;
    let changes = repo.diff_tree_to_tree(base_tree.as_ref(), &target_tree, None).map_err(|e| e.to_string())?;

    let mut budget = Budget(MAX_DIFF_BYTES);
    let mut files = Vec::new();
    for change in changes.iter().filter(|change| !change.entry_mode().is_tree()) {
        let text = |path: &gix::bstr::BString| path.to_str_lossy().into_owned();
        let file = match change {
            C::Addition { location, entry_mode, id, .. } => {
                let new = entry_content(&repo, *entry_mode, *id, &mut budget);
                file_diff(FileKind::Added, text(location), None, Some(Vec::new()), new)
            }
            C::Deletion { location, entry_mode, id, .. } => {
                let old = entry_content(&repo, *entry_mode, *id, &mut budget);
                file_diff(FileKind::Deleted, text(location), None, old, Some(Vec::new()))
            }
            C::Modification { location, previous_entry_mode, previous_id, entry_mode, id } => {
                let old = entry_content(&repo, *previous_entry_mode, *previous_id, &mut budget);
                let new = entry_content(&repo, *entry_mode, *id, &mut budget);
                file_diff(FileKind::Modified, text(location), None, old, new)
            }
            C::Rewrite { source_location, source_entry_mode, source_id, location, entry_mode, id, .. } => {
                let old = entry_content(&repo, *source_entry_mode, *source_id, &mut budget);
                let new = entry_content(&repo, *entry_mode, *id, &mut budget);
                file_diff(FileKind::Renamed, text(location), Some(text(source_location)), old, new)
            }
        };
        files.push(file);
    }
    files.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(files)
}

/// Незакоммиченное: HEAD против рабочей папки.
pub fn diff_worktree(path: &Path) -> Result<Vec<FileDiff>, String> {
    let repo = open_worktree(path)?;
    let Some(root) = repo.workdir().map(Path::to_path_buf) else { return Ok(Vec::new()) };
    let changes = work_changes(&repo, None)?;
    let head = repo.head_tree_id_or_empty().map_err(|e| e.to_string())?;
    let tree = head.object().map_err(|e| e.to_string())?.into_tree();
    let (mut filter, index) = repo.filter_pipeline(None).map_err(|e| e.to_string())?;

    let mut budget = Budget(MAX_DIFF_BYTES);
    let mut files = Vec::new();
    for change in changes {
        let old = if change.kind == FileKind::Added {
            Some(Vec::new())
        } else {
            let at_head = change.old_path.as_deref().unwrap_or(&change.path);
            tree.lookup_entry_by_path(at_head)
                .ok()
                .flatten()
                .and_then(|entry| entry_content(&repo, entry.mode(), entry.object_id(), &mut budget))
        };
        let new = if change.kind == FileKind::Deleted {
            Some(Vec::new())
        } else {
            disk_content(&root, &change.path, &mut filter, &index, &mut budget)
        };
        files.push(file_diff(change.kind, change.path, change.old_path, old, new));
    }
    Ok(files)
}

/// Общий предок двух коммитов: сравнение ветки с основной считается от него.
pub fn merge_base(path: &Path, a: ObjectId, b: ObjectId) -> Result<Option<ObjectId>, String> {
    let repo = open(path)?;
    let base = repo.merge_base(a, b).map_err(|e| e.to_string())?;
    Ok(base.map(|id| id.detach()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::process::Command;
    use std::sync::atomic::AtomicUsize;

    const DAY: i64 = 24 * 60 * 60;

    /// Временная папка, которая убирается за собой и при упавшем тесте.
    struct Temp(PathBuf);

    impl Temp {
        fn new() -> Temp {
            static NEXT: AtomicUsize = AtomicUsize::new(0);
            let number = NEXT.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!("gitmap-test-{}-{number}", std::process::id()));
            fs::create_dir_all(&path).unwrap();
            Temp(path)
        }

        fn repo(&self) -> PathBuf {
            let path = self.0.join("repo");
            fs::create_dir_all(&path).unwrap();
            git(&path, &["init", "-q", "-b", "main"]);
            path
        }
    }

    impl Drop for Temp {
        fn drop(&mut self) {
            fs::remove_dir_all(&self.0).ok();
        }
    }

    fn now() -> i64 {
        SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs() as i64
    }

    fn git_at(dir: &Path, time: i64, args: &[&str]) -> String {
        let date = format!("{time} +0000");
        let output = Command::new("git")
            .current_dir(dir)
            .args(args)
            // Настройки машины не должны влиять на тесты: autocrlf задаётся там, где он нужен.
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", dir.join("no-such-config"))
            .env("GIT_AUTHOR_NAME", "Test")
            .env("GIT_AUTHOR_EMAIL", "test@example.com")
            .env("GIT_COMMITTER_NAME", "Test")
            .env("GIT_COMMITTER_EMAIL", "test@example.com")
            .env("GIT_AUTHOR_DATE", &date)
            .env("GIT_COMMITTER_DATE", &date)
            .output()
            .unwrap();
        assert!(output.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&output.stderr));
        String::from_utf8_lossy(&output.stdout).trim().to_owned()
    }

    fn git(dir: &Path, args: &[&str]) -> String {
        git_at(dir, now(), args)
    }

    fn commit_at(dir: &Path, time: i64, message: &str) -> ObjectId {
        git_at(dir, time, &["add", "-A"]);
        git_at(dir, time, &["commit", "-q", "--allow-empty", "-m", message]);
        ObjectId::from_hex(git(dir, &["rev-parse", "HEAD"]).as_bytes()).unwrap()
    }

    fn commit(dir: &Path, message: &str) -> ObjectId {
        commit_at(dir, now(), message)
    }

    fn write(dir: &Path, name: &str, data: impl AsRef<[u8]>) {
        let path = dir.join(name);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, data).unwrap();
    }

    fn changes(dir: &Path) -> Vec<(FileKind, String)> {
        worktree_changes(dir).unwrap().into_iter().map(|c| (c.kind, c.path)).collect()
    }

    fn body(diff: &FileDiff) -> Vec<(LineKind, Option<u32>, Option<u32>, &str)> {
        diff.lines.iter().map(|l| (l.kind, l.old, l.new, l.text.as_str())).collect()
    }

    fn numbered(lines: std::ops::Range<u32>) -> String {
        lines.map(|n| format!("line {n}\n")).collect()
    }

    #[test]
    fn web_url_forms() {
        let cases = [
            ("git@github.com:a/b.git", Some("https://github.com/a/b")),
            ("https://github.com/a/b.git", Some("https://github.com/a/b")),
            ("https://github.com/a/b/", Some("https://github.com/a/b")),
            ("https://user:token@github.com/a/b.git", Some("https://github.com/a/b")),
            ("http://gitlab.example.com/a/b", Some("https://gitlab.example.com/a/b")),
            ("https://gitlab.example.com:8443/a/b", Some("https://gitlab.example.com:8443/a/b")),
            ("ssh://git@github.com:22/a/b.git", Some("https://github.com/a/b")),
            ("git://github.com/a/b.git", Some("https://github.com/a/b")),
            ("  git@github.com:a/b.git\n", Some("https://github.com/a/b")),
            ("C:\\repos\\b.git", None),
            ("E:/repos/b.git", None),
            ("/srv/git/b.git", None),
            ("../b.git", None),
            ("file:///srv/git/b.git", None),
            ("work-alias:a/b.git", None),
            ("https://github.com", None),
            ("https://github.com/a b", None),
        ];
        for (raw, expected) in cases {
            assert_eq!(web_url(raw).as_deref(), expected, "{raw}");
        }
    }

    #[test]
    fn health_without_remote() {
        let temp = Temp::new();
        let repo = temp.repo();
        write(&repo, "a.txt", "one\n");
        let root = commit(&repo, "first");
        commit(&repo, "second");
        let health = health(&repo).unwrap();
        assert!(!health.has_remote);
        assert_eq!(health.web_url, None);
        assert_eq!(health.unpushed, 0);
        assert_eq!(health.dirty, 0);
        assert_eq!(health.week_commits, 2);
        assert_eq!(health.roots, vec![root]);
    }

    #[test]
    fn health_of_empty_repository() {
        let temp = Temp::new();
        let repo = temp.repo();
        write(&repo, "a.txt", "one\n");
        let health = health(&repo).unwrap();
        assert_eq!(health.dirty, 1);
        assert!(health.roots.is_empty());
        assert_eq!(health.week_commits, 0);
    }

    #[test]
    fn health_counts_unpushed_commits() {
        let temp = Temp::new();
        let bare = temp.0.join("server.git");
        fs::create_dir_all(&bare).unwrap();
        git(&bare, &["init", "-q", "--bare", "-b", "main"]);
        git(&temp.0, &["clone", "-q", bare.to_str().unwrap(), "clone"]);
        let repo = temp.0.join("clone");
        git(&repo, &["checkout", "-q", "-B", "main"]);
        write(&repo, "a.txt", "one\n");
        commit(&repo, "first");
        commit(&repo, "second");
        git(&repo, &["push", "-q", "-u", "origin", "main"]);
        assert_eq!(health(&repo).unwrap().unpushed, 0);

        commit(&repo, "third");
        commit(&repo, "fourth");
        git(&repo, &["checkout", "-q", "-b", "topic"]);
        commit(&repo, "fifth");
        git(&repo, &["remote", "set-url", "origin", "git@github.com:someone/thing.git"]);
        let health = health(&repo).unwrap();
        assert!(health.has_remote);
        assert_eq!(health.unpushed, 3);
        assert_eq!(health.web_url.as_deref(), Some("https://github.com/someone/thing"));
        // Удалённая ветка считается как и локальная: история у них общая.
        assert_eq!(health.week_commits, 5);
    }

    #[test]
    fn health_finds_stale_branches() {
        let temp = Temp::new();
        let repo = temp.repo();
        let old = now() - 200 * DAY;
        write(&repo, "a.txt", "one\n");
        commit_at(&repo, old, "first");
        git(&repo, &["branch", "merged-old"]);
        git(&repo, &["checkout", "-q", "-b", "forgotten"]);
        write(&repo, "b.txt", "one\n");
        commit_at(&repo, old, "abandoned work");
        git(&repo, &["checkout", "-q", "-b", "current", "main"]);
        write(&repo, "c.txt", "one\n");
        commit_at(&repo, old, "the branch we are on");
        git(&repo, &["checkout", "-q", "-b", "fresh", "main"]);
        write(&repo, "d.txt", "one\n");
        commit(&repo, "recent work");
        git(&repo, &["checkout", "-q", "current"]);

        let health = health(&repo).unwrap();
        assert_eq!(health.stale_branches, vec!["forgotten".to_owned()]);
        assert_eq!(health.week_commits, 1);
    }

    #[test]
    fn health_lists_every_root() {
        let temp = Temp::new();
        let repo = temp.repo();
        write(&repo, "a.txt", "one\n");
        commit(&repo, "first");
        git(&repo, &["checkout", "-q", "--orphan", "other"]);
        commit(&repo, "second history");
        let expected = git(&repo, &["rev-list", "--max-parents=0", "--branches"]);
        let mut expected: Vec<ObjectId> =
            expected.lines().map(|id| ObjectId::from_hex(id.as_bytes()).unwrap()).collect();
        expected.sort();
        assert_eq!(expected.len(), 2);
        assert_eq!(health(&repo).unwrap().roots, expected);
    }

    #[test]
    fn stamp_follows_refs_and_index() {
        let temp = Temp::new();
        let repo = temp.repo();
        write(&repo, "a.txt", "one\n");
        commit(&repo, "first");
        let mut last = stamp(&repo);
        assert_eq!(last, stamp(&repo));

        let mut changed = |what: &str, repo: &Path| {
            let current = stamp(repo);
            assert_ne!(current, last, "{what}");
            last = current;
        };
        commit(&repo, "second");
        changed("коммит", &repo);
        git(&repo, &["branch", "topic"]);
        changed("новая ветка", &repo);
        git(&repo, &["tag", "v1"]);
        changed("тег", &repo);
        git(&repo, &["checkout", "-q", "topic"]);
        changed("переключение", &repo);
        write(&repo, "b.txt", "two\n");
        git(&repo, &["add", "b.txt"]);
        changed("индекс", &repo);
        git(&repo, &["pack-refs", "--all"]);
        changed("упаковка ссылок", &repo);
    }

    #[test]
    fn stamp_handles_other_layouts() {
        let temp = Temp::new();
        let repo = temp.repo();
        write(&repo, "a.txt", "one\n");
        commit(&repo, "first");

        // Связанная рабочая папка: `.git` здесь файл, а ветки лежат в основном репозитории.
        let linked = temp.0.join("linked");
        git(&repo, &["worktree", "add", "-q", "-b", "side", linked.to_str().unwrap()]);
        let before = stamp(&linked);
        git(&repo, &["update-ref", "refs/heads/other", "HEAD"]);
        assert_ne!(stamp(&linked), before);
        let before = stamp(&linked);
        commit(&linked, "in the linked worktree");
        assert_ne!(stamp(&linked), before);

        // Подмодуль: `.git` тоже файл, но путь в нём относительный.
        let parent = temp.0.join("parent");
        fs::create_dir_all(&parent).unwrap();
        git(&parent, &["init", "-q", "-b", "main"]);
        let source = repo.to_str().unwrap();
        git(&parent, &["-c", "protocol.file.allow=always", "submodule", "add", "-q", source, "sub"]);
        let sub = parent.join("sub");
        let before = stamp(&sub);
        commit(&sub, "in the submodule");
        assert_ne!(stamp(&sub), before);

        // Голый репозиторий.
        let bare = temp.0.join("bare.git");
        git(&temp.0, &["clone", "-q", "--bare", repo.to_str().unwrap(), "bare.git"]);
        let before = stamp(&bare);
        git(&bare, &["branch", "new-one", "main"]);
        assert_ne!(stamp(&bare), before);

        // Отсутствующая папка просто даёт какое-то значение.
        stamp(&temp.0.join("no-such-folder"));
    }

    #[test]
    fn worktree_changes_cover_all_kinds() {
        let temp = Temp::new();
        let repo = temp.repo();
        write(&repo, ".gitignore", "*.log\nbuild/\n");
        write(&repo, "modified.txt", "one\n");
        write(&repo, "deleted.txt", "one\n");
        write(&repo, "staged.txt", "one\n");
        commit(&repo, "first");

        write(&repo, "modified.txt", "two\n");
        fs::remove_file(repo.join("deleted.txt")).unwrap();
        write(&repo, "staged.txt", "two\n");
        git(&repo, &["add", "staged.txt"]);
        // Содержимое разное: иначе удалённый и новый файл законно считаются переименованием.
        write(&repo, "new/untracked.txt", "fresh\n");
        write(&repo, "ignored.log", "one\n");
        write(&repo, "build/out.txt", "one\n");
        write(&repo, "added.txt", "another\n");
        git(&repo, &["add", "added.txt"]);

        let expected = vec![
            (FileKind::Added, "added.txt".to_owned()),
            (FileKind::Deleted, "deleted.txt".to_owned()),
            (FileKind::Modified, "modified.txt".to_owned()),
            (FileKind::Added, "new/untracked.txt".to_owned()),
            (FileKind::Modified, "staged.txt".to_owned()),
        ];
        assert_eq!(changes(&repo), expected);
        assert_eq!(health(&repo).unwrap().dirty, expected.len());
    }

    #[test]
    fn worktree_changes_merge_both_halves() {
        let temp = Temp::new();
        let repo = temp.repo();
        write(&repo, "kept.txt", "one\n");
        write(&repo, "removed.txt", "one\n");
        commit(&repo, "first");

        // Добавлен в индекс и удалён с диска: относительно HEAD ничего нет.
        write(&repo, "ghost.txt", "one\n");
        git(&repo, &["add", "ghost.txt"]);
        fs::remove_file(repo.join("ghost.txt")).unwrap();
        // Убран из индекса, но лежит на диске: относительно HEAD файл остался.
        git(&repo, &["rm", "-q", "--cached", "kept.txt"]);
        write(&repo, "kept.txt", "two\n");
        // Изменён в индексе и удалён с диска.
        write(&repo, "removed.txt", "two\n");
        git(&repo, &["add", "removed.txt"]);
        fs::remove_file(repo.join("removed.txt")).unwrap();

        let expected = vec![(FileKind::Modified, "kept.txt".to_owned()), (FileKind::Deleted, "removed.txt".to_owned())];
        assert_eq!(changes(&repo), expected);
    }

    #[test]
    fn worktree_changes_track_renames() {
        let temp = Temp::new();
        let repo = temp.repo();
        write(&repo, "staged.txt", numbered(0..30));
        write(&repo, "unstaged.txt", numbered(100..130));
        commit(&repo, "first");
        git(&repo, &["mv", "staged.txt", "staged-new.txt"]);
        fs::rename(repo.join("unstaged.txt"), repo.join("unstaged-new.txt")).unwrap();

        let expected = vec![
            (FileKind::Renamed, "staged-new.txt".to_owned()),
            (FileKind::Renamed, "unstaged-new.txt".to_owned()),
        ];
        assert_eq!(changes(&repo), expected);

        let diffs = diff_worktree(&repo).unwrap();
        assert_eq!(diffs.len(), 2);
        assert_eq!(diffs[0].old_path.as_deref(), Some("staged.txt"));
        assert_eq!(diffs[1].old_path.as_deref(), Some("unstaged.txt"));
        assert!(diffs.iter().all(|diff| diff.lines.is_empty()));
    }

    #[test]
    fn worktree_of_bare_repository_is_empty() {
        let temp = Temp::new();
        let repo = temp.repo();
        commit(&repo, "first");
        git(&temp.0, &["clone", "-q", "--bare", repo.to_str().unwrap(), "bare.git"]);
        let bare = temp.0.join("bare.git");
        assert!(worktree_changes(&bare).unwrap().is_empty());
        assert!(diff_worktree(&bare).unwrap().is_empty());
        assert_eq!(health(&bare).unwrap().dirty, 0);
    }

    #[test]
    fn status_stops_at_the_time_limit() {
        let temp = Temp::new();
        let repo = temp.repo();
        commit(&repo, "first");
        for number in 0..2000 {
            write(&repo, &format!("many/{}/{number}.txt", number % 50), "one\n");
        }
        let opened = open_worktree(&repo).unwrap();
        assert_eq!(work_changes(&opened, Some(Duration::from_secs(60))).unwrap().len(), 2000);
        let started = std::time::Instant::now();
        let found = work_changes(&opened, Some(Duration::ZERO)).unwrap();
        assert!(found.len() < 2000);
        assert!(started.elapsed() < Duration::from_secs(2));
    }

    #[test]
    fn crlf_on_disk_is_not_a_change() {
        let temp = Temp::new();
        let repo = temp.repo();
        write(&repo, "a.txt", "one\ntwo\n");
        commit(&repo, "first");
        // Так файлы выкладывает git для Windows с системным `core.autocrlf=true`.
        fs::remove_file(repo.join("a.txt")).unwrap();
        git(&repo, &["-c", "core.autocrlf=true", "checkout", "--", "a.txt"]);
        assert_eq!(fs::read(repo.join("a.txt")).unwrap(), b"one\r\ntwo\r\n");
        // Новая запись сбивает метку времени в индексе, и файл приходится читать.
        std::thread::sleep(Duration::from_millis(20));
        write(&repo, "a.txt", "one\r\ntwo\r\n");
        assert!(changes(&repo).is_empty());
        assert_eq!(health(&repo).unwrap().dirty, 0);

        write(&repo, "a.txt", "one\r\nTWO, longer\r\n");
        let diffs = diff_worktree(&repo).unwrap();
        assert_eq!(diffs.len(), 1);
        assert_eq!((diffs[0].added, diffs[0].removed), (1, 1));
        assert!(diffs[0].lines.iter().any(|l| l.kind == LineKind::Context && l.text == "one"));
    }

    #[test]
    fn crlf_in_history_stays_as_it_is() {
        let temp = Temp::new();
        let repo = temp.repo();
        write(&repo, "a.txt", "one\r\ntwo\r\nthree\r\n");
        let first = commit(&repo, "first");
        write(&repo, "a.txt", "one\r\nTWO\r\nthree\r\n");
        let second = commit(&repo, "second");

        let diffs = diff_commits(&repo, None, second).unwrap();
        assert_eq!(
            body(&diffs[0]),
            vec![
                (LineKind::Hunk, None, None, "@@ -1,3 +1,3 @@"),
                (LineKind::Context, Some(1), Some(1), "one"),
                (LineKind::Removed, Some(2), None, "two"),
                (LineKind::Added, None, Some(2), "TWO"),
                (LineKind::Context, Some(3), Some(3), "three"),
            ]
        );

        // Смена концов строк в истории это настоящее изменение каждой строки.
        write(&repo, "a.txt", "one\ntwo\nthree\n");
        let third = commit(&repo, "third");
        let diffs = diff_commits(&repo, Some(first), third).unwrap();
        assert_eq!((diffs[0].added, diffs[0].removed), (3, 3));

        // Файл с CRLF в истории на диске сравнивается как есть.
        git(&repo, &["reset", "-q", "--hard", first.to_string().as_str()]);
        write(&repo, "a.txt", "one\r\ntwo\r\nthree, more\r\n");
        let diffs = diff_worktree(&repo).unwrap();
        assert_eq!((diffs[0].added, diffs[0].removed), (1, 1));
    }

    #[test]
    fn diff_splits_and_joins_hunks() {
        let temp = Temp::new();
        let repo = temp.repo();
        write(&repo, "far.txt", numbered(0..30));
        write(&repo, "near.txt", numbered(0..30));
        commit(&repo, "first");
        write(&repo, "far.txt", numbered(0..30).replace("line 1\n", "one\n").replace("line 25\n", "twenty five\n"));
        write(&repo, "near.txt", numbered(0..30).replace("line 10\n", "ten\n").replace("line 16\n", "sixteen\n"));
        let second = commit(&repo, "second");

        let diffs = diff_commits(&repo, None, second).unwrap();
        let hunks = |diff: &FileDiff| -> Vec<String> {
            diff.lines.iter().filter(|l| l.kind == LineKind::Hunk).map(|l| l.text.clone()).collect()
        };
        assert_eq!(hunks(&diffs[0]), vec!["@@ -1,5 +1,5 @@", "@@ -23,7 +23,7 @@"]);
        assert_eq!(hunks(&diffs[1]), vec!["@@ -8,13 +8,13 @@"]);
        assert_eq!((diffs[1].added, diffs[1].removed), (2, 2));
        let removed: Vec<u32> =
            diffs[1].lines.iter().filter(|l| l.kind == LineKind::Removed).filter_map(|l| l.old).collect();
        assert_eq!(removed, vec![11, 17]);
    }

    #[test]
    fn diff_of_root_commit_adds_everything() {
        let temp = Temp::new();
        let repo = temp.repo();
        write(&repo, "z.txt", "one\ntwo\nthree\n");
        write(&repo, "a.txt", "one\n");
        write(&repo, "empty.txt", "");
        let root = commit(&repo, "first");

        let diffs = diff_commits(&repo, None, root).unwrap();
        let names: Vec<&str> = diffs.iter().map(|d| d.path.as_str()).collect();
        assert_eq!(names, vec!["a.txt", "empty.txt", "z.txt"]);
        assert!(diffs.iter().all(|diff| diff.kind == FileKind::Added));
        assert_eq!(diffs[2].added, 3);
        assert_eq!(diffs[2].lines[0].text, "@@ -0,0 +1,3 @@");
        assert_eq!(diffs[0].lines[0].text, "@@ -0,0 +1 @@");
        assert!(diffs[1].lines.is_empty());
    }

    #[test]
    fn diff_between_any_commits_and_rename() {
        let temp = Temp::new();
        let repo = temp.repo();
        write(&repo, "old-name.txt", numbered(0..30));
        write(&repo, "gone.txt", "bye\n");
        let first = commit(&repo, "first");
        git(&repo, &["mv", "old-name.txt", "new-name.txt"]);
        write(&repo, "new-name.txt", numbered(0..30).replace("line 3\n", "three\n"));
        fs::remove_file(repo.join("gone.txt")).unwrap();
        commit(&repo, "second");
        write(&repo, "later.txt", "hello\n");
        let third = commit(&repo, "third");

        let diffs = diff_commits(&repo, Some(first), third).unwrap();
        let summary: Vec<(FileKind, &str, Option<&str>)> =
            diffs.iter().map(|d| (d.kind, d.path.as_str(), d.old_path.as_deref())).collect();
        assert_eq!(
            summary,
            vec![
                (FileKind::Deleted, "gone.txt", None),
                (FileKind::Added, "later.txt", None),
                (FileKind::Renamed, "new-name.txt", Some("old-name.txt")),
            ]
        );
        assert_eq!((diffs[2].added, diffs[2].removed), (1, 1));
    }

    #[test]
    fn diff_detects_binary_and_large_files() {
        let temp = Temp::new();
        let repo = temp.repo();
        let mut late_zero = vec![b'a'; BINARY_PROBE_BYTES + 100];
        late_zero.push(0);
        late_zero.push(b'\n');
        write(&repo, "binary.dat", b"abc\0def\n");
        write(&repo, "late-zero.txt", &late_zero);
        write(&repo, "large.txt", "line\n".repeat(300_000));
        write(&repo, "small.txt", "one\n");
        commit(&repo, "first");
        write(&repo, "binary.dat", b"abc\0xyz\n");
        late_zero.push(b'b');
        write(&repo, "late-zero.txt", &late_zero);
        write(&repo, "large.txt", "line\n".repeat(300_001));
        write(&repo, "small.txt", "two\n");
        let second = commit(&repo, "second");

        let from_commits = diff_commits(&repo, None, second).unwrap();
        git(&repo, &["reset", "-q", "--mixed", "HEAD~1"]);
        let from_worktree = diff_worktree(&repo).unwrap();
        for diffs in [from_commits, from_worktree] {
            let flags: Vec<(&str, bool, bool)> =
                diffs.iter().map(|d| (d.path.as_str(), d.binary, d.too_large)).collect();
            assert_eq!(
                flags,
                vec![
                    ("binary.dat", true, false),
                    ("large.txt", false, true),
                    ("late-zero.txt", false, false),
                    ("small.txt", false, false),
                ]
            );
            assert!(diffs[0].lines.is_empty() && diffs[1].lines.is_empty());
            assert_eq!(diffs[3].added, 1);
        }
    }

    #[test]
    fn budget_limits_one_side_and_the_total() {
        let mut budget = Budget(MAX_SIDE_BYTES);
        assert!(budget.take(MAX_SIDE_BYTES));
        assert!(!budget.take(1));
        let mut budget = Budget(MAX_DIFF_BYTES);
        assert!(!budget.take(MAX_SIDE_BYTES + 1));
        assert_eq!(budget.0, MAX_DIFF_BYTES);
    }

    #[test]
    fn diff_reads_awkward_text() {
        let temp = Temp::new();
        let repo = temp.repo();
        write(&repo, "latin.txt", b"caf\xE9\nsame\n");
        write(&repo, "tail.txt", "one\ntwo");
        commit(&repo, "first");
        write(&repo, "latin.txt", b"caf\xE8\nsame\n");
        write(&repo, "tail.txt", "one\ntwo\nthree\n");
        let second = commit(&repo, "second");

        let diffs = diff_commits(&repo, None, second).unwrap();
        assert_eq!(
            body(&diffs[0]),
            vec![
                (LineKind::Hunk, None, None, "@@ -1,2 +1,2 @@"),
                (LineKind::Removed, Some(1), None, "caf\u{FFFD}"),
                (LineKind::Added, None, Some(1), "caf\u{FFFD}"),
                (LineKind::Context, Some(2), Some(2), "same"),
            ]
        );
        // Без перевода строки в конце последняя строка другая, хоть и выглядит так же.
        assert_eq!(
            body(&diffs[1]),
            vec![
                (LineKind::Hunk, None, None, "@@ -1,2 +1,3 @@"),
                (LineKind::Context, Some(1), Some(1), "one"),
                (LineKind::Removed, Some(2), None, "two"),
                (LineKind::Added, None, Some(2), "two"),
                (LineKind::Added, None, Some(3), "three"),
            ]
        );
    }

    #[test]
    fn diff_worktree_shows_each_kind() {
        let temp = Temp::new();
        let repo = temp.repo();
        write(&repo, ".gitignore", "*.log\n");
        write(&repo, "changed.txt", "one\ntwo\n");
        write(&repo, "deleted.txt", "bye\n");
        commit(&repo, "first");
        write(&repo, "changed.txt", "one\nTWO\n");
        fs::remove_file(repo.join("deleted.txt")).unwrap();
        write(&repo, "untracked.txt", "hello\n");
        write(&repo, "skip.log", "hidden\n");

        let diffs = diff_worktree(&repo).unwrap();
        let summary: Vec<(FileKind, &str, usize, usize)> =
            diffs.iter().map(|d| (d.kind, d.path.as_str(), d.added, d.removed)).collect();
        assert_eq!(
            summary,
            vec![
                (FileKind::Modified, "changed.txt", 1, 1),
                (FileKind::Deleted, "deleted.txt", 0, 1),
                (FileKind::Added, "untracked.txt", 1, 0),
            ]
        );
    }

    #[test]
    fn worktree_on_unborn_head() {
        let temp = Temp::new();
        let repo = temp.repo();
        write(&repo, "a.txt", "one\n");
        write(&repo, "b.txt", "two\n");
        git(&repo, &["add", "a.txt"]);
        let diffs = diff_worktree(&repo).unwrap();
        assert_eq!(diffs.len(), 2);
        assert!(diffs.iter().all(|diff| diff.kind == FileKind::Added && diff.added == 1));
    }

    #[test]
    fn submodule_and_nested_repository() {
        let temp = Temp::new();
        let inner = temp.repo();
        write(&inner, "a.txt", "one\n");
        commit(&inner, "first");
        let parent = temp.0.join("parent");
        fs::create_dir_all(&parent).unwrap();
        git(&parent, &["init", "-q", "-b", "main"]);
        let source = inner.to_str().unwrap();
        git(&parent, &["-c", "protocol.file.allow=always", "submodule", "add", "-q", source, "sub"]);
        commit(&parent, "add submodule");
        let moved = commit(&parent.join("sub"), "moved on");

        assert_eq!(changes(&parent), vec![(FileKind::Modified, "sub".to_owned())]);
        let diffs = diff_worktree(&parent).unwrap();
        assert_eq!((diffs[0].added, diffs[0].removed), (1, 1));
        assert!(diffs[0].lines.iter().any(|l| l.text == format!("Subproject commit {moved}")));
        // Сводка подмодули не открывает.
        assert_eq!(health(&parent).unwrap().dirty, 0);

        // Чужой репозиторий внутри рабочей папки виден как одна запись и не обходится.
        let repo = temp.0.join("outer");
        fs::create_dir_all(&repo).unwrap();
        git(&repo, &["init", "-q", "-b", "main"]);
        commit(&repo, "first");
        let nested = repo.join("nested");
        fs::create_dir_all(&nested).unwrap();
        git(&nested, &["init", "-q", "-b", "main"]);
        write(&nested, "a.txt", "one\n");
        commit(&nested, "first");
        assert_eq!(diff_worktree(&repo).unwrap().len(), 1);
    }

    #[test]
    fn merge_base_of_branches() {
        let temp = Temp::new();
        let repo = temp.repo();
        write(&repo, "a.txt", "one\n");
        let fork = commit(&repo, "first");
        git(&repo, &["checkout", "-q", "-b", "topic"]);
        let topic = commit(&repo, "on topic");
        git(&repo, &["checkout", "-q", "main"]);
        let main = commit(&repo, "on main");
        assert_eq!(merge_base(&repo, topic, main).unwrap(), Some(fork));
        assert_eq!(merge_base(&repo, fork, topic).unwrap(), Some(fork));

        git(&repo, &["checkout", "-q", "--orphan", "other"]);
        let other = commit(&repo, "unrelated");
        assert_eq!(merge_base(&repo, other, main).unwrap(), None);
    }

    fn listing(dir: &Path, out: &mut Vec<(PathBuf, u64, SystemTime)>) {
        for entry in fs::read_dir(dir).unwrap().flatten() {
            // Сведения из самого перечисления папки на NTFS запаздывают.
            let meta = fs::metadata(entry.path()).unwrap();
            if meta.is_dir() {
                listing(&entry.path(), out);
            }
            out.push((entry.path(), meta.len(), meta.modified().unwrap()));
        }
    }

    #[test]
    fn nothing_is_written() {
        let temp = Temp::new();
        let repo = temp.repo();
        write(&repo, "a.txt", numbered(0..30));
        write(&repo, "b.txt", "one\n");
        let first = commit(&repo, "first");
        write(&repo, "b.txt", "two\n");
        let second = commit(&repo, "second");
        // Метка времени разошлась с индексом: git на этом месте переписал бы индекс.
        std::thread::sleep(Duration::from_millis(20));
        write(&repo, "a.txt", numbered(0..30));
        write(&repo, "c.txt", "new\n");
        fs::rename(repo.join("b.txt"), repo.join("d.txt")).unwrap();

        let mut before = Vec::new();
        listing(&repo, &mut before);
        health(&repo).unwrap();
        worktree_changes(&repo).unwrap();
        diff_worktree(&repo).unwrap();
        diff_commits(&repo, None, second).unwrap();
        merge_base(&repo, first, second).unwrap();
        stamp(&repo);
        let mut after = Vec::new();
        listing(&repo, &mut after);
        before.sort();
        after.sort();
        assert_eq!(before, after);
    }

    #[test]
    fn repository_filters_do_not_run() {
        let temp = Temp::new();
        let repo = temp.repo();
        write(&repo, ".gitattributes", "*.txt filter=mark\n");
        write(&repo, "a.txt", "one\n");
        commit(&repo, "first");
        git(&repo, &["config", "filter.mark.clean", "touch marker-from-filter; cat"]);
        std::thread::sleep(Duration::from_millis(20));
        write(&repo, "a.txt", "one\n");
        write(&repo, "b.txt", "two\n");
        changes(&repo);
        diff_worktree(&repo).unwrap();
        health(&repo).unwrap();
        assert!(!repo.join("marker-from-filter").exists());
    }

    /// Ручная проверка на настоящих клонах: только чтение, `cargo test -- --ignored`.
    #[test]
    #[ignore]
    fn real_clones_sanity() {
        for path in [r"E:\Coding\Minecraft\voxy", r"E:\Coding\Minecraft\Iris"] {
            let path = Path::new(path);
            for round in 0..3 {
                let started = std::time::Instant::now();
                let health = health(path).unwrap();
                println!(
                    "{} #{round}: health {:?}: url {:?}, remote {}, unpushed {}, dirty {}, stale {:?}, week {}, roots {}",
                    path.display(),
                    started.elapsed(),
                    health.web_url,
                    health.has_remote,
                    health.unpushed,
                    health.dirty,
                    health.stale_branches,
                    health.week_commits,
                    health.roots.len(),
                );
            }
            let started = std::time::Instant::now();
            let changes = worktree_changes(path).unwrap();
            println!("worktree_changes {:?}: {}", started.elapsed(), changes.len());
            for change in changes.iter().take(10) {
                println!("  {:?} {}", change.kind, change.path);
            }
            let started = std::time::Instant::now();
            let diffs = diff_worktree(path).unwrap();
            println!("diff_worktree {:?}: {}", started.elapsed(), diffs.len());
            let repo = open(path).unwrap();
            let head = repo.head_id().unwrap().detach();
            let started = std::time::Instant::now();
            let diffs = diff_commits(path, None, head).unwrap();
            let lines: usize = diffs.iter().map(|diff| diff.lines.len()).sum();
            println!("diff_commits {:?}: {} files, {lines} lines", started.elapsed(), diffs.len());
            let started = std::time::Instant::now();
            let value = stamp(path);
            println!("stamp {:?}: {value}", started.elapsed());
        }
    }
}
