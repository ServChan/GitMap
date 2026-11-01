//! Чтение истории через gix. Программа только читает: ничего не пишет ни в рабочую
//! папку, ни в `.git`, поэтому её можно спокойно наводить на чужие клоны.

use gix::ObjectId;
use gix::bstr::ByteSlice;
use gix::revision::walk::Sorting;
use gix::traverse::commit::simple::CommitTimeOrder;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

pub type Idx = u32;

/// Больше этого история не читается: хватает для любой карты, а схема из миллиона
/// коммитов всё равно не складывается в обозримую картинку.
pub const MAX_COMMITS: usize = 60_000;

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

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum FileKind {
    Added,
    Modified,
    Deleted,
    Renamed,
}

pub struct FileChange {
    pub kind: FileKind,
    pub path: String,
}

pub struct Details {
    pub message: String,
    pub files: Vec<FileChange>,
}

fn open(path: &Path) -> Result<gix::Repository, String> {
    gix::open_opts(path, gix::open::Options::isolated()).map_err(|e| e.to_string())
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

pub fn load(path: &Path) -> Result<Graph, String> {
    let mut repo = open(path)?;
    repo.object_cache_size_if_unset(64 * 1024 * 1024);

    let mut tips: Vec<(String, RefKind, ObjectId)> = Vec::new();
    let mut remote_default = None;
    {
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
    }
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
