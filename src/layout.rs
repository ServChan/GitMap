//! Раскладка схемы. Основная ветка идёт прямой линией по центру (дорожка 0), всё
//! остальное расходится вверх и вниз. Чистая логика без рисования: на входе граф
//! коммитов, на выходе линии, соединения и координаты мира.
//!
//! Порядок работы:
//! 1. разобрать коммиты на линии по цепочкам первых родителей;
//! 2. расставить коммиты по колонкам, сохраняя и топологию, и хронологию;
//! 3. раздать линиям дорожки так, чтобы они не наезжали друг на друга;
//! 4. раздвинуть колонки, чтобы переходы между дорожками оставались под 45°.

use crate::git::{Graph, Idx, RefKind, branch_name};
use std::cmp::Reverse;
use std::collections::{BinaryHeap, HashMap};

pub const COLUMN: f32 = 30.0;
pub const LANE: f32 = 44.0;
pub const PALETTE_SIZE: usize = 10;

/// Ширина колонки без событий относительно обычной: длинные прямые участки сжимаются.
const QUIET_COLUMN: f32 = 0.22;
/// Дальше этого числа дорожек переход становится круче 45°, иначе схема расползается.
const MAX_DIAGONAL_LANES: f32 = 4.0;
const NONE: u32 = u32::MAX;

#[derive(Clone, Copy, PartialEq, Eq)]
pub struct Options {
    pub show_merged: bool,
    pub compress: bool,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum LineKind {
    Main,
    /// Есть указатель, и он ещё не влит в основную ветку.
    Active,
    Merged,
}

pub struct Line {
    pub name: String,
    pub kind: LineKind,
    /// От старых к новым.
    pub commits: Vec<Idx>,
    pub parent: Option<u32>,
    pub depth: u32,
    pub lane: i32,
    pub color: usize,
    pub visible: bool,
    /// Занятый участок дорожки в координатах мира, с учётом хвостов до слияний.
    pub span: (f32, f32),
}

/// Соединение между линиями: ответвление или слияние.
pub struct Link {
    pub from: Idx,
    pub to: Idx,
    /// Линия, цветом которой рисуется соединение.
    pub owner: u32,
    /// Переход в начале пути: ответвление сразу уходит на свою дорожку.
    /// Иначе переход в конце: слияние идёт по своей дорожке до самой цели.
    pub fork: bool,
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum TickLevel {
    Day,
    Month,
    Year,
}

pub struct Tick {
    pub x: f32,
    pub time: i64,
    pub level: TickLevel,
}

pub struct Map {
    pub lines: Vec<Line>,
    pub links: Vec<Link>,
    /// Линия каждого коммита.
    pub line_of: Vec<u32>,
    /// Координата x каждого коммита.
    pub x: Vec<f32>,
    /// Важен ли коммит: развилка, слияние, указатель, начало или конец линии.
    pub event: Vec<bool>,
    /// Коммиты, достижимые из основной ветки.
    pub in_main: Vec<bool>,
    pub ticks: Vec<Tick>,
    pub width: f32,
    pub min_lane: i32,
    pub max_lane: i32,
}

impl Map {
    pub fn y(&self, commit: Idx) -> f32 {
        self.lane_y(self.lines[self.line_of[commit as usize] as usize].lane)
    }

    pub fn lane_y(&self, lane: i32) -> f32 {
        lane as f32 * LANE
    }

    pub fn is_visible(&self, commit: Idx) -> bool {
        self.lines[self.line_of[commit as usize] as usize].visible
    }
}

/// Имя и вершина основной ветки. Если локальная копия отстала от удалённой, вершиной
/// считается более новая из них.
pub fn main_branch(graph: &Graph) -> Option<(String, Idx)> {
    let remote = graph.remote_default.as_deref();
    let candidates = remote.into_iter().chain(["main", "master", "trunk", "develop", "dev"]);
    for name in candidates {
        let local = graph.refs.iter().find(|r| r.kind == RefKind::Local && r.name == name);
        let far = graph.refs.iter().find(|r| r.kind == RefKind::Remote && branch_name(r.kind, &r.name) == name);
        let tip = match (local, far) {
            (Some(l), Some(r)) => {
                if reachable(graph, r.commit)[l.commit as usize] { r.commit } else { l.commit }
            }
            (Some(l), None) => l.commit,
            (None, Some(r)) => r.commit,
            (None, None) => continue,
        };
        return Some((name.to_owned(), tip));
    }
    if let Some(head) = graph.head {
        return Some((graph.head_branch.clone().unwrap_or_else(|| "HEAD".to_owned()), head));
    }
    let newest = graph.refs.iter().min_by_key(|r| r.commit)?;
    Some((branch_name(newest.kind, &newest.name).to_owned(), newest.commit))
}

pub fn reachable(graph: &Graph, tip: Idx) -> Vec<bool> {
    let mut seen = vec![false; graph.commits.len()];
    let mut stack = vec![tip];
    while let Some(commit) = stack.pop() {
        if !std::mem::replace(&mut seen[commit as usize], true) {
            stack.extend(&graph.commits[commit as usize].parents);
        }
    }
    seen
}

/// Имя влитой ветки из сообщения слияния, когда сама ветка уже удалена.
pub fn merged_branch_name(subject: &str) -> Option<String> {
    let quoted = |rest: &str| rest.strip_prefix('\'').and_then(|r| r.split('\'').next()).map(str::to_owned);
    if let Some(rest) = subject.strip_prefix("Merge branch ") {
        return quoted(rest);
    }
    if let Some(rest) = subject.strip_prefix("Merge remote-tracking branch ") {
        return quoted(rest).map(|name| branch_name(RefKind::Remote, &name).to_owned());
    }
    if let Some(rest) = subject.strip_prefix("Merge pull request ") {
        let source = rest.split(" from ").nth(1)?.split_whitespace().next()?;
        return Some(source.split_once('/').map_or(source, |(_, branch)| branch).to_owned());
    }
    None
}

struct Tracer<'a> {
    graph: &'a Graph,
    line_of: Vec<u32>,
    lines: Vec<Line>,
}

impl Tracer<'_> {
    /// Новая линия от вершины вниз по первым родителям до уже занятого коммита.
    fn trace(&mut self, tip: Idx, name: String, kind: LineKind) {
        if self.line_of[tip as usize] != NONE {
            return;
        }
        let id = self.lines.len() as u32;
        let mut commits = Vec::new();
        let mut at = Some(tip);
        while let Some(commit) = at {
            if self.line_of[commit as usize] != NONE {
                break;
            }
            self.line_of[commit as usize] = id;
            commits.push(commit);
            at = self.graph.commits[commit as usize].parents.first().copied();
        }
        commits.reverse();
        let parent = at.map(|commit| self.line_of[commit as usize]);
        let depth = parent.map_or(0, |p| self.lines[p as usize].depth + 1);
        self.lines.push(Line {
            name,
            kind,
            commits,
            parent,
            depth,
            lane: 0,
            color: 0,
            visible: true,
            span: (0.0, 0.0),
        });
    }
}

/// Вершины для трассировки. Копии одной ветки (`x` и `origin/x`) представлены самой
/// свежей, остальные идут после всех: обычно они лягут на уже готовую линию.
/// Старые вершины первыми: ветка от ветки тогда становится ответвлением, а не наоборот.
fn tips_in_order(graph: &Graph) -> Vec<(Idx, String)> {
    let mut groups: HashMap<&str, Vec<&crate::git::Ref>> = HashMap::new();
    for r in graph.refs.iter().filter(|r| r.kind != RefKind::Tag) {
        groups.entry(branch_name(r.kind, &r.name)).or_default().push(r);
    }
    let time = |commit: Idx| graph.commits[commit as usize].time;
    let mut leads = Vec::new();
    let mut rest = Vec::new();
    for (name, mut refs) in groups {
        refs.sort_by_key(|r| (Reverse(time(r.commit)), r.kind != RefKind::Local, r.name.clone()));
        for (i, r) in refs.into_iter().enumerate() {
            if i == 0 {
                leads.push((r.commit, name.to_owned()));
            } else {
                rest.push((r.commit, r.name.clone()));
            }
        }
    }
    let order = |list: &mut Vec<(Idx, String)>| list.sort_by_key(|(c, n)| (time(*c), n.clone()));
    order(&mut leads);
    order(&mut rest);
    let mut tags: Vec<(Idx, String)> =
        graph.refs.iter().filter(|r| r.kind == RefKind::Tag).map(|r| (r.commit, r.name.clone())).collect();
    order(&mut tags);
    leads.into_iter().chain(rest).chain(tags).collect()
}

fn build_lines(graph: &Graph, main_name: String, main_tip: Idx, in_main: &[bool]) -> (Vec<Line>, Vec<u32>) {
    let mut t = Tracer { graph, line_of: vec![NONE; graph.commits.len()], lines: Vec::new() };
    t.trace(main_tip, main_name.clone(), LineKind::Main);
    for (tip, name) in tips_in_order(graph) {
        let kind = if in_main[tip as usize] { LineKind::Merged } else { LineKind::Active };
        t.trace(tip, name, kind);
    }
    if let Some(head) = graph.head {
        let name = "HEAD".to_owned();
        let kind = if in_main[head as usize] { LineKind::Merged } else { LineKind::Active };
        t.trace(head, name, kind);
    }
    // Всё, что осталось, пришло вторыми родителями слияний: ветки, которых уже нет.
    let mut next = 0;
    while next < t.lines.len() {
        for at in (0..t.lines[next].commits.len()).rev() {
            let commit = t.lines[next].commits[at];
            let data = &graph.commits[commit as usize];
            for &parent in &data.parents {
                if t.line_of[parent as usize] == NONE {
                    let name = merged_branch_name(&data.subject).filter(|n| *n != main_name).unwrap_or_default();
                    t.trace(parent, name, LineKind::Merged);
                }
            }
        }
        next += 1;
    }
    (t.lines, t.line_of)
}

fn build_links(graph: &Graph, lines: &[Line], line_of: &[u32]) -> Vec<Link> {
    let mut links = Vec::new();
    for (id, line) in lines.iter().enumerate() {
        for (at, &commit) in line.commits.iter().enumerate() {
            let previous = at.checked_sub(1).map(|p| line.commits[p]);
            for (nth, &parent) in graph.commits[commit as usize].parents.iter().enumerate() {
                if Some(parent) == previous {
                    continue;
                }
                let parent_line = line_of[parent as usize];
                // Вливание основной ветки в боковую тоже сразу уходит с центра:
                // иначе оно шло бы по дорожке 0, притворяясь её продолжением.
                let fork = (nth == 0 && at == 0) || parent_line == 0;
                let owner = if fork { id as u32 } else { parent_line };
                links.push(Link { from: parent, to: commit, owner, fork });
            }
        }
    }
    links
}

/// Кан с приоритетом по времени: родители всегда раньше детей, а при свободе
/// выбора раньше идёт то, что случилось раньше.
fn chronological(graph: &Graph, shown: &[bool]) -> Vec<Idx> {
    let count = graph.commits.len();
    let mut waiting = vec![0u32; count];
    let mut children: Vec<Vec<Idx>> = vec![Vec::new(); count];
    for (i, commit) in graph.commits.iter().enumerate() {
        if !shown[i] {
            continue;
        }
        for &parent in &commit.parents {
            if shown[parent as usize] {
                waiting[i] += 1;
                children[parent as usize].push(i as Idx);
            }
        }
    }
    let mut ready: BinaryHeap<Reverse<(i64, Idx)>> = (0..count)
        .filter(|&i| shown[i] && waiting[i] == 0)
        .map(|i| Reverse((graph.commits[i].time, i as Idx)))
        .collect();
    let mut order = Vec::with_capacity(count);
    while let Some(Reverse((_, commit))) = ready.pop() {
        order.push(commit);
        for &child in &children[commit as usize] {
            waiting[child as usize] -= 1;
            if waiting[child as usize] == 0 {
                ready.push(Reverse((graph.commits[child as usize].time, child)));
            }
        }
    }
    order
}

/// Колонки. Основная линия задаёт ход времени, боковые коммиты встают напротив того,
/// что в этот момент происходило на ней, и сдвигаются правее только ради своих родителей.
fn columns(graph: &Graph, order: &[Idx], line_of: &[u32]) -> (Vec<u32>, u32) {
    let mut column = vec![0u32; graph.commits.len()];
    let mut cursor = 0u32;
    let mut last = 0u32;
    for &commit in order {
        let after_parents = graph.commits[commit as usize]
            .parents
            .iter()
            .map(|&p| column[p as usize] + 1)
            .max()
            .unwrap_or(0);
        let at = if line_of[commit as usize] == 0 {
            cursor = after_parents.max(cursor + 1);
            cursor
        } else {
            after_parents.max(cursor)
        };
        column[commit as usize] = at;
        last = last.max(at);
    }
    (column, last + 1)
}

fn place_lanes(lines: &mut [Line]) {
    let mut order: Vec<usize> = (1..lines.len()).filter(|&i| lines[i].visible).collect();
    order.sort_by(|&a, &b| {
        let (la, lb) = (&lines[a], &lines[b]);
        let key = |l: &Line| (l.depth, l.kind != LineKind::Active);
        key(la).cmp(&key(lb)).then(la.span.0.total_cmp(&lb.span.0))
    });
    let mut taken: HashMap<i32, Vec<(f32, f32)>> = HashMap::new();
    let mut load = [0.0f32; 2];
    let free = |taken: &HashMap<i32, Vec<(f32, f32)>>, lane: i32, span: (f32, f32)| {
        taken.get(&lane).is_none_or(|used| used.iter().all(|u| span.1 < u.0 || span.0 > u.1))
    };
    for i in order {
        let span = lines[i].span;
        let parent_lane = lines[i].parent.map_or(0, |p| lines[p as usize].lane);
        let lane = if parent_lane != 0 {
            // Дочерняя линия остаётся на стороне родителя и уходит дальше от центра.
            let side = parent_lane.signum();
            (parent_lane.abs() + 1..).map(|k| k * side).find(|&l| free(&taken, l, span))
        } else {
            (1..).find_map(|k| {
                let fits = [free(&taken, -k, span), free(&taken, k, span)];
                match fits {
                    [true, true] => Some(if load[0] <= load[1] { -k } else { k }),
                    [true, false] => Some(-k),
                    [false, true] => Some(k),
                    [false, false] => None,
                }
            })
        }
        .unwrap_or(1);
        lines[i].lane = lane;
        taken.entry(lane).or_default().push(span);
        load[usize::from(lane > 0)] += span.1 - span.0;
    }
}

/// Соседние по дорожке и родственные линии не должны совпадать цветом.
fn place_colors(lines: &mut [Line]) {
    let mut uses = [0u32; PALETTE_SIZE];
    for i in 1..lines.len() {
        if !lines[i].visible {
            continue;
        }
        let mut blocked = [false; PALETTE_SIZE];
        for j in 1..i {
            let other = &lines[j];
            let overlaps = other.visible
                && other.span.0 <= lines[i].span.1
                && lines[i].span.0 <= other.span.1
                && (other.lane - lines[i].lane).abs() <= 2;
            if overlaps || lines[i].parent == Some(j as u32) {
                blocked[other.color] = true;
            }
        }
        let color = (0..PALETTE_SIZE)
            .min_by_key(|&c| (blocked[c], uses[c], c))
            .unwrap_or_default();
        lines[i].color = color;
        uses[color] += 1;
    }
}

fn build_ticks(graph: &Graph, map_x: &[f32], order: &[Idx]) -> Vec<Tick> {
    use chrono::{DateTime, Datelike, Local};
    let date = |time: i64| DateTime::from_timestamp(time, 0).map(|d| d.with_timezone(&Local));
    let mut ticks = Vec::new();
    let mut previous: Option<(i32, u32, u32)> = None;
    let mut newest = i64::MIN;
    let mut along: Vec<Idx> = order.to_vec();
    along.sort_by(|&a, &b| map_x[a as usize].total_cmp(&map_x[b as usize]));
    for commit in along {
        // Время может идти назад из-за чужих часов и перебазирования: ось стоит на месте.
        let time = graph.commits[commit as usize].time;
        if time <= newest {
            continue;
        }
        newest = time;
        let Some(d) = date(time) else { continue };
        let now = (d.year(), d.month(), d.day());
        let level = match previous {
            None => Some(TickLevel::Month),
            Some(p) if p.0 != now.0 => Some(TickLevel::Year),
            Some(p) if p.1 != now.1 => Some(TickLevel::Month),
            Some(p) if p.2 != now.2 => Some(TickLevel::Day),
            _ => None,
        };
        previous = Some(now);
        if let Some(level) = level {
            ticks.push(Tick { x: map_x[commit as usize], time, level });
        }
    }
    ticks
}

pub fn build(graph: &Graph, options: Options) -> Option<Map> {
    let (main_name, main_tip) = main_branch(graph)?;
    let in_main = reachable(graph, main_tip);
    let (mut lines, line_of) = build_lines(graph, main_name, main_tip, &in_main);

    if !options.show_merged {
        for line in &mut lines {
            line.visible = line.kind != LineKind::Merged;
        }
        // Линия в работе может расти из уже слитой: цепочку до центра не прячем.
        for i in 0..lines.len() {
            if lines[i].visible {
                let mut at = lines[i].parent;
                while let Some(parent) = at {
                    lines[parent as usize].visible = true;
                    at = lines[parent as usize].parent;
                }
            }
        }
    }
    let shown: Vec<bool> = line_of.iter().map(|&l| l != NONE && lines[l as usize].visible).collect();
    let links: Vec<Link> = build_links(graph, &lines, &line_of)
        .into_iter()
        .filter(|link| shown[link.from as usize] && shown[link.to as usize])
        .collect();

    let order = chronological(graph, &shown);
    let (column, column_count) = columns(graph, &order, &line_of);

    let mut children = vec![0u32; graph.commits.len()];
    for &commit in &order {
        for &parent in &graph.commits[commit as usize].parents {
            children[parent as usize] += u32::from(shown[parent as usize]);
        }
    }
    let mut event = vec![false; graph.commits.len()];
    for r in &graph.refs {
        event[r.commit as usize] = true;
    }
    if let Some(head) = graph.head {
        event[head as usize] = true;
    }
    for &commit in &order {
        let i = commit as usize;
        if graph.commits[i].parents.len() != 1 || children[i] != 1 {
            event[i] = true;
        }
    }
    for link in &links {
        event[link.from as usize] = true;
        event[link.to as usize] = true;
    }

    let mut widths = vec![if options.compress { QUIET_COLUMN } else { 1.0 }; column_count as usize];
    for &commit in &order {
        if event[commit as usize] {
            widths[column[commit as usize] as usize] = 1.0;
        }
    }
    let mut position = vec![0.0f32; column_count as usize];
    for i in 1..position.len() {
        position[i] = position[i - 1] + COLUMN * widths[i].max(widths[i - 1]);
    }

    // Участки дорожек по черновым координатам.
    let at = |commit: Idx| position[column[commit as usize] as usize];
    for line in &mut lines {
        line.span = (at(line.commits[0]), at(line.commits[line.commits.len() - 1]));
    }
    for link in &links {
        if link.fork {
            let span = &mut lines[line_of[link.to as usize] as usize].span;
            span.0 = span.0.min(at(link.from));
        }
        let span = &mut lines[line_of[link.from as usize] as usize].span;
        span.1 = span.1.max(at(link.to));
    }
    for line in &mut lines {
        // У линии в работе на конце стоит табличка с именем: ей тоже нужно место.
        let plate = if line.kind == LineKind::Active { 8.0 * line.name.chars().count() as f32 + 48.0 } else { 0.0 };
        line.span = (line.span.0 - COLUMN, line.span.1 + COLUMN + plate);
    }
    place_lanes(&mut lines);

    // Колонки раздвигаем только вправо: порядок не меняется, и наложений не появится.
    let mut constraints: Vec<(u32, u32, f32)> = links
        .iter()
        .map(|link| {
            let lane = |c: Idx| lines[line_of[c as usize] as usize].lane;
            let lanes = (lane(link.from) - lane(link.to)).abs() as f32;
            (column[link.from as usize], column[link.to as usize], lanes.min(MAX_DIAGONAL_LANES) * LANE)
        })
        .collect();
    constraints.sort_by_key(|c| c.1);
    let mut final_position = vec![0.0f32; column_count as usize];
    let mut next = 0;
    for i in 0..final_position.len() {
        if i > 0 {
            final_position[i] = final_position[i - 1] + position[i] - position[i - 1];
        }
        while next < constraints.len() && constraints[next].1 as usize == i {
            let (from, _, gap) = constraints[next];
            final_position[i] = final_position[i].max(final_position[from as usize] + gap);
            next += 1;
        }
    }
    let x: Vec<f32> = column.iter().map(|&c| final_position[c as usize]).collect();
    let stretch = |value: f32| {
        let at = position.partition_point(|&p| p < value).min(position.len() - 1);
        final_position[at] + value - position[at]
    };
    for line in &mut lines {
        line.span = (stretch(line.span.0), stretch(line.span.1));
    }
    place_colors(&mut lines);

    let ticks = build_ticks(graph, &x, &order);
    let width = final_position.last().copied().unwrap_or_default();
    let visible_lanes = || lines.iter().filter(|l| l.visible).map(|l| l.lane);
    let (min_lane, max_lane) = (visible_lanes().min().unwrap_or(0), visible_lanes().max().unwrap_or(0));
    Some(Map { lines, links, line_of, x, event, in_main, ticks, width, min_lane, max_lane })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git::{Commit, Ref};
    use std::path::PathBuf;

    /// `spec`: подпись, родители; подписи идут от новых к старым.
    fn graph(spec: &[(&str, &[usize])], refs: &[(&str, RefKind, usize)]) -> Graph {
        let count = spec.len() as i64;
        let commits = spec
            .iter()
            .enumerate()
            .map(|(i, (subject, parents))| Commit {
                id: gix::ObjectId::null(gix::hash::Kind::Sha1),
                parents: parents.iter().map(|&p| p as Idx).collect(),
                time: (count - i as i64) * 60,
                author: "a".into(),
                subject: (*subject).into(),
            })
            .collect();
        let refs = refs.iter().map(|&(name, kind, c)| Ref { name: name.into(), kind, commit: c as Idx }).collect();
        Graph {
            path: PathBuf::new(),
            commits,
            refs,
            head: Some(0),
            head_branch: Some("main".into()),
            remote_default: None,
            truncated: false,
        }
    }

    const ALL: Options = Options { show_merged: true, compress: false };

    /// main: 7-5-3-1-0, feature: 6-4 от 5, вторая ветка 2 влита в 1.
    fn sample() -> Graph {
        graph(
            &[
                ("merge", &[1, 2]),
                ("m3", &[3]),
                ("deleted", &[3]),
                ("m2", &[5]),
                ("f2", &[6]),
                ("m1", &[7]),
                ("f1", &[5]),
                ("root", &[]),
            ],
            &[("main", RefKind::Local, 0), ("feature", RefKind::Local, 4)],
        )
    }

    #[test]
    fn main_stays_on_lane_zero_and_owns_first_parent_chain() {
        let map = build(&sample(), ALL).unwrap();
        assert_eq!(map.lines[0].lane, 0);
        assert_eq!(map.lines[0].commits, vec![7, 5, 3, 1, 0]);
        assert!(map.lines[1..].iter().all(|l| l.lane != 0));
    }

    #[test]
    fn every_commit_gets_a_line() {
        let map = build(&sample(), ALL).unwrap();
        assert!(map.line_of.iter().all(|&l| l != NONE));
    }

    #[test]
    fn unmerged_tip_is_active_and_merged_side_is_merged() {
        let map = build(&sample(), ALL).unwrap();
        let line = |c: usize| &map.lines[map.line_of[c] as usize];
        assert_eq!(line(4).kind, LineKind::Active);
        assert_eq!(line(4).name, "feature");
        assert_eq!(line(2).kind, LineKind::Merged);
    }

    #[test]
    fn parents_are_always_left_of_children() {
        let g = sample();
        let map = build(&g, ALL).unwrap();
        for (i, commit) in g.commits.iter().enumerate() {
            for &p in &commit.parents {
                assert!(map.x[p as usize] < map.x[i], "{p} левее {i}");
            }
        }
    }

    #[test]
    fn lines_on_one_lane_do_not_overlap() {
        let map = build(&sample(), ALL).unwrap();
        let lines = &map.lines[1..];
        for (i, a) in lines.iter().enumerate() {
            for b in &lines[i + 1..] {
                if a.lane == b.lane {
                    assert!(a.span.1 < b.span.0 || b.span.1 < a.span.0);
                }
            }
        }
    }

    #[test]
    fn links_leave_room_for_a_45_degree_turn() {
        let map = build(&sample(), ALL).unwrap();
        for link in &map.links {
            let dy = (map.y(link.from) - map.y(link.to)).abs().min(4.0 * LANE);
            assert!(map.x[link.to as usize] - map.x[link.from as usize] >= dy);
        }
    }

    #[test]
    fn remote_copy_ahead_of_local_becomes_main_tip() {
        let g = graph(&[("b", &[1]), ("a", &[])], &[("main", RefKind::Local, 1), ("origin/main", RefKind::Remote, 0)]);
        let map = build(&g, ALL).unwrap();
        assert_eq!(map.lines.len(), 1);
        assert_eq!(map.lines[0].commits, vec![1, 0]);
    }

    #[test]
    fn local_and_remote_copy_of_a_branch_make_one_line() {
        let g = graph(
            &[("f", &[2]), ("m", &[2]), ("root", &[])],
            &[("main", RefKind::Local, 1), ("x", RefKind::Local, 0), ("origin/x", RefKind::Remote, 0)],
        );
        let map = build(&g, ALL).unwrap();
        assert_eq!(map.lines.len(), 2);
        assert_eq!(map.lines[1].name, "x");
    }

    #[test]
    fn branch_from_branch_nests_on_the_same_side() {
        let g = graph(
            &[("c", &[1]), ("b", &[3]), ("m", &[3]), ("root", &[])],
            &[("main", RefKind::Local, 2), ("a", RefKind::Local, 1), ("a-child", RefKind::Local, 0)],
        );
        let map = build(&g, ALL).unwrap();
        let line = |c: usize| &map.lines[map.line_of[c] as usize];
        assert_eq!(line(1).name, "a");
        assert_eq!(line(0).name, "a-child");
        assert_eq!(line(0).lane.signum(), line(1).lane.signum());
        assert!(line(0).lane.abs() > line(1).lane.abs());
    }

    #[test]
    fn hidden_merged_lines_keep_active_descendants_connected() {
        let g = graph(
            &[
                ("merge", &[2, 1]),
                ("side", &[3]),
                ("m", &[3]),
                ("root", &[]),
            ],
            &[("main", RefKind::Local, 0)],
        );
        let map = build(&g, Options { show_merged: false, compress: true }).unwrap();
        assert!(!map.lines[map.line_of[1] as usize].visible);
        assert!(map.links.is_empty());
    }

    #[test]
    fn orphan_history_is_a_separate_line() {
        let g = graph(&[("m", &[]), ("o", &[])], &[("main", RefKind::Local, 0), ("pages", RefKind::Local, 1)]);
        let map = build(&g, ALL).unwrap();
        assert_eq!(map.lines.len(), 2);
        assert_eq!(map.lines[1].parent, None);
    }

    #[test]
    fn merge_subjects_name_deleted_branches() {
        assert_eq!(merged_branch_name("Merge branch 'feature/x' into main").as_deref(), Some("feature/x"));
        assert_eq!(merged_branch_name("Merge branch 'y'").as_deref(), Some("y"));
        assert_eq!(merged_branch_name("Merge remote-tracking branch 'origin/z'").as_deref(), Some("z"));
        assert_eq!(merged_branch_name("Merge pull request #12 from user/fix/a").as_deref(), Some("fix/a"));
        assert_eq!(merged_branch_name("Fix everything"), None);
    }
}
