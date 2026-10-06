//! Индекс тома: все имена диска в памяти, поиск по ним за миллисекунды.
//!
//! Хранение — массивы, а не дерево объектов: на миллионах записей это в разы меньше памяти и
//! проход по всем именам упирается в скорость памяти. Имена хранятся дважды — как есть (для
//! показа) и в нижнем регистре (для поиска подстрок через `memchr`).
//!
//! Узел не перемещается и не переиспользуется: переименование — это удаление и добавление.
//! Поэтому родитель всегда старше ребёнка, а удалённые узлы просто помечаются и выбрасываются
//! при сохранении снимка.

use std::collections::HashMap;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use memchr::memmem;

use crate::entry::{Attributes, Entry, EntryKind};

pub mod query;
pub mod snapshot;

pub use query::{Query, Term};

/// «Нет узла».
pub const NONE: u32 = u32::MAX;
pub const ROOT: u32 = 0;

const DIR: u8 = 1;
const HIDDEN: u8 = 2;
const SYSTEM: u8 = 4;
const DELETED: u8 = 8;

/// Что известно о файле или папке.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct NodeInfo {
    pub is_dir: bool,
    pub size: u64,
    /// Секунды Unix; `i64::MIN` — неизвестно.
    pub modified: i64,
    pub hidden: bool,
    pub system: bool,
}

impl NodeInfo {
    pub fn of(entry: &Entry) -> NodeInfo {
        NodeInfo {
            is_dir: entry.is_dir(),
            size: if entry.is_dir() { 0 } else { entry.size },
            modified: entry.modified.map_or(i64::MIN, unix),
            hidden: entry.attributes.hidden(),
            system: entry.attributes.system(),
        }
    }

    fn flags(self) -> u8 {
        (if self.is_dir { DIR } else { 0 })
            | (if self.hidden { HIDDEN } else { 0 })
            | (if self.system { SYSTEM } else { 0 })
    }
}

pub fn unix(time: SystemTime) -> i64 {
    match time.duration_since(UNIX_EPOCH) {
        Ok(d) => d.as_secs() as i64,
        Err(e) => -(e.duration().as_secs() as i64),
    }
}

fn system_time(secs: i64) -> Option<SystemTime> {
    match secs {
        i64::MIN => None,
        s if s >= 0 => Some(UNIX_EPOCH + Duration::from_secs(s as u64)),
        s => UNIX_EPOCH.checked_sub(Duration::from_secs(s.unsigned_abs())),
    }
}

/// Положение журнала USN, с которого снимок актуален.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct JournalMark {
    pub id: u64,
    pub next_usn: i64,
}

#[derive(Debug, Clone)]
pub struct VolumeIndex {
    root: PathBuf,
    names: Vec<u8>,
    name_start: Vec<u32>,
    name_len: Vec<u16>,
    lower: Vec<u8>,
    lower_start: Vec<u32>,
    lower_len: Vec<u16>,
    parent: Vec<u32>,
    first_child: Vec<u32>,
    next_sibling: Vec<u32>,
    size: Vec<u64>,
    modified: Vec<i64>,
    flags: Vec<u8>,
    deleted: usize,
    /// Когда закончен последний полный обход, секунды Unix.
    pub built_at: i64,
    pub journal: Option<JournalMark>,
}

impl VolumeIndex {
    /// Пустой индекс с одним корнем.
    pub fn new(root: PathBuf) -> VolumeIndex {
        let mut index = VolumeIndex {
            root,
            names: Vec::new(),
            name_start: Vec::new(),
            name_len: Vec::new(),
            lower: Vec::new(),
            lower_start: Vec::new(),
            lower_len: Vec::new(),
            parent: Vec::new(),
            first_child: Vec::new(),
            next_sibling: Vec::new(),
            size: Vec::new(),
            modified: Vec::new(),
            flags: Vec::new(),
            deleted: 0,
            built_at: 0,
            journal: None,
        };
        index.push(NONE, "", NodeInfo { is_dir: true, ..Default::default() });
        index
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Живых узлов, без корня.
    pub fn len(&self) -> usize {
        self.parent.len() - self.deleted - 1
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Доля удалённых — пора ли ужать при сохранении.
    pub fn garbage(&self) -> f32 {
        self.deleted as f32 / self.parent.len().max(1) as f32
    }

    fn push(&mut self, parent: u32, name: &str, info: NodeInfo) -> u32 {
        let id = self.parent.len() as u32;
        let lower = name.to_lowercase();
        self.name_start.push(self.names.len() as u32);
        self.name_len.push(name.len().min(u16::MAX as usize) as u16);
        self.names.extend_from_slice(name.as_bytes());
        self.lower_start.push(self.lower.len() as u32);
        self.lower_len.push(lower.len().min(u16::MAX as usize) as u16);
        self.lower.extend_from_slice(lower.as_bytes());
        self.parent.push(parent);
        self.first_child.push(NONE);
        self.next_sibling.push(NONE);
        self.size.push(info.size);
        self.modified.push(info.modified);
        self.flags.push(info.flags());
        if parent != NONE {
            let p = parent as usize;
            self.next_sibling[id as usize] = self.first_child[p];
            self.first_child[p] = id;
        }
        id
    }

    /// Добавить ребёнка без проверки повторов — для построения обходом.
    pub fn add(&mut self, parent: u32, name: &str, info: NodeInfo) -> u32 {
        self.push(parent, name, info)
    }

    pub fn name(&self, node: u32) -> &str {
        let i = node as usize;
        let start = self.name_start[i] as usize;
        std::str::from_utf8(&self.names[start..start + self.name_len[i] as usize]).unwrap_or("")
    }

    fn lower_name(&self, node: u32) -> &[u8] {
        let i = node as usize;
        let start = self.lower_start[i] as usize;
        &self.lower[start..start + self.lower_len[i] as usize]
    }

    pub fn is_dir(&self, node: u32) -> bool {
        self.flags[node as usize] & DIR != 0
    }

    fn alive(&self, node: u32) -> bool {
        self.flags[node as usize] & DELETED == 0
    }

    pub fn parent_of(&self, node: u32) -> u32 {
        self.parent[node as usize]
    }

    pub fn children(&self, node: u32) -> impl Iterator<Item = u32> + '_ {
        let mut next = self.first_child[node as usize];
        std::iter::from_fn(move || {
            while next != NONE {
                let current = next;
                next = self.next_sibling[current as usize];
                if self.alive(current) {
                    return Some(current);
                }
            }
            None
        })
    }

    /// Ребёнок с этим именем, без учёта регистра.
    pub fn child(&self, parent: u32, name: &str) -> Option<u32> {
        let lower = name.to_lowercase();
        self.children(parent).find(|&c| self.lower_name(c) == lower.as_bytes())
    }

    /// Узел по полному пути; `None` — путь не на этом томе или его нет в индексе.
    pub fn lookup(&self, path: &Path) -> Option<u32> {
        let relative = path.strip_prefix(&self.root).ok()?;
        let mut node = ROOT;
        for component in relative.components() {
            if let Component::Normal(name) = component {
                node = self.child(node, &name.to_string_lossy())?;
            }
        }
        Some(node)
    }

    pub fn path(&self, node: u32) -> PathBuf {
        let mut names = Vec::new();
        let mut current = node;
        while current != ROOT && current != NONE {
            names.push(self.name(current));
            current = self.parent[current as usize];
        }
        let mut path = self.root.clone();
        for name in names.iter().rev() {
            path.push(name);
        }
        path
    }

    /// Убрать узел и всё под ним.
    pub fn remove(&mut self, node: u32) {
        if node == ROOT || !self.alive(node) {
            return;
        }
        let parent = self.parent[node as usize];
        // Отцепить от родителя: удалённый узел больше не встретится среди детей.
        let mut link = self.first_child[parent as usize];
        if link == node {
            self.first_child[parent as usize] = self.next_sibling[node as usize];
        } else {
            while link != NONE {
                let next = self.next_sibling[link as usize];
                if next == node {
                    self.next_sibling[link as usize] = self.next_sibling[node as usize];
                    break;
                }
                link = next;
            }
        }
        let mut stack = vec![node];
        while let Some(current) = stack.pop() {
            if self.flags[current as usize] & DELETED == 0 {
                self.flags[current as usize] |= DELETED;
                self.deleted += 1;
            }
            let mut child = self.first_child[current as usize];
            while child != NONE {
                stack.push(child);
                child = self.next_sibling[child as usize];
            }
        }
    }

    fn set_info(&mut self, node: u32, info: NodeInfo) {
        let i = node as usize;
        self.size[i] = info.size;
        self.modified[i] = info.modified;
        self.flags[i] = (self.flags[i] & DELETED) | info.flags();
    }

    /// Узел папки по пути; недостающие папки по дороге создаются.
    pub fn ensure_dir(&mut self, path: &Path) -> Option<u32> {
        let relative = path.strip_prefix(&self.root).ok()?.to_path_buf();
        let mut node = ROOT;
        for component in relative.components() {
            if let Component::Normal(name) = component {
                let name = name.to_string_lossy();
                node = match self.child(node, &name) {
                    Some(child) if self.is_dir(child) => child,
                    Some(file) => {
                        self.remove(file);
                        self.push(node, &name, NodeInfo { is_dir: true, ..Default::default() })
                    }
                    None => self.push(node, &name, NodeInfo { is_dir: true, ..Default::default() }),
                };
            }
        }
        Some(node)
    }

    /// Сверить содержимое папки с тем, что сейчас на диске. Возвращает новые папки —
    /// их содержимое неизвестно, его нужно обойти.
    pub fn reconcile(&mut self, dir: &Path, actual: &[(String, NodeInfo)]) -> Vec<PathBuf> {
        let Some(node) = self.ensure_dir(dir) else { return Vec::new() };
        self.reconcile_node(node, actual)
            .into_iter()
            .zip(actual)
            .filter(|((_, new), (_, info))| *new && info.is_dir)
            .map(|(_, (name, _))| dir.join(name))
            .collect()
    }

    /// То же по номеру узла — для обхода диска, где путь искать заново дорого. Возвращает
    /// для каждой записи `actual` её узел и признак «новый»; пусто — узла уже нет (папку
    /// убрали, пока её читали).
    pub fn reconcile_node(&mut self, node: u32, actual: &[(String, NodeInfo)]) -> Vec<(u32, bool)> {
        if node as usize >= self.parent.len() || !self.alive(node) || !self.is_dir(node) {
            return Vec::new();
        }
        let mut existing: HashMap<Vec<u8>, u32> =
            self.children(node).map(|c| (self.lower_name(c).to_vec(), c)).collect();
        let mut nodes = Vec::with_capacity(actual.len());
        for (name, info) in actual {
            let key = name.to_lowercase().into_bytes();
            match existing.remove(&key) {
                Some(child) if self.is_dir(child) == info.is_dir && self.name(child) == name => {
                    self.set_info(child, *info);
                    nodes.push((child, false));
                }
                found => {
                    // Новый объект, другой тип или регистр имени поменялся.
                    if let Some(child) = found {
                        self.remove(child);
                    }
                    nodes.push((self.push(node, name, *info), true));
                }
            }
        }
        for (_, gone) in existing {
            self.remove(gone);
        }
        nodes
    }

    /// Жив ли узел: не удалён ни он, ни его папки.
    pub fn is_alive(&self, node: u32) -> bool {
        (node as usize) < self.parent.len() && self.alive(node)
    }

    /// Запись для показа в списке. `parents` — общий кэш путей папок, чтобы у результатов
    /// из одной папки был один `Arc`.
    pub fn entry(&self, node: u32, parents: &mut HashMap<u32, Arc<Path>>) -> Entry {
        let i = node as usize;
        let parent_id = self.parent[i];
        let parent =
            parents.entry(parent_id).or_insert_with(|| Arc::from(self.path(parent_id))).clone();
        let mut attributes = 0;
        if self.flags[i] & HIDDEN != 0 {
            attributes |= Attributes::HIDDEN;
        }
        if self.flags[i] & SYSTEM != 0 {
            attributes |= Attributes::SYSTEM;
        }
        Entry {
            name: self.name(node).to_string(),
            parent,
            kind: if self.is_dir(node) { EntryKind::Dir } else { EntryKind::File },
            size: self.size[i],
            modified: system_time(self.modified[i]),
            created: None,
            attributes: Attributes(attributes),
        }
    }

    /// Совпадения запроса, лучшие первыми; не больше `limit`. `cancelled` спрашивается
    /// по ходу — поиск можно бросить, когда пользователь набрал следующую букву.
    pub fn search(
        &self,
        query: &Query,
        limit: usize,
        cancelled: &(dyn Fn() -> bool + Sync),
    ) -> Hits {
        if query.is_empty() {
            return Hits::default();
        }
        let matcher = Matcher::new(self, query);
        let count = self.parent.len();
        let threads = std::thread::available_parallelism().map_or(4, |n| n.get()).min(16);
        let chunk = count.div_ceil(threads).max(4096);
        let mut found: Vec<(u32, i32)> = std::thread::scope(|scope| {
            let handles: Vec<_> = (1..count)
                .step_by(chunk)
                .map(|start| {
                    let end = (start + chunk).min(count);
                    let matcher = &matcher;
                    scope.spawn(move || {
                        let mut hits = Vec::new();
                        for node in start..end {
                            if node % 65536 == 0 && cancelled() {
                                break;
                            }
                            if let Some(score) = matcher.score(node as u32) {
                                hits.push((node as u32, score));
                            }
                        }
                        hits
                    })
                })
                .collect();
            handles.into_iter().flat_map(|h| h.join().unwrap_or_default()).collect()
        });
        let total = found.len();
        let by_rank = |a: &(u32, i32), b: &(u32, i32)| {
            b.1.cmp(&a.1).then_with(|| self.lower_name(a.0).cmp(self.lower_name(b.0)))
        };
        if found.len() > limit {
            found.select_nth_unstable_by(limit, by_rank);
            found.truncate(limit);
        }
        found.sort_unstable_by(by_rank);
        Hits { hits: found, total }
    }
}

/// Итог поиска по тому.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Hits {
    /// Узел и оценка, лучшие первыми. Оценки сравнимы между томами — по ним сливается выдача.
    pub hits: Vec<(u32, i32)>,
    /// Сколько подошло всего, включая не попавшие в предел.
    pub total: usize,
}

/// Запрос, подготовленный для одного тома.
struct Matcher<'a> {
    index: &'a VolumeIndex,
    query: &'a Query,
    words: Vec<(memmem::Finder<'a>, bool, &'a [u8])>,
    within: Option<u32>,
    /// `in:` указывает на другой том или несуществующую папку — совпадений нет.
    impossible: bool,
}

impl<'a> Matcher<'a> {
    fn new(index: &'a VolumeIndex, query: &'a Query) -> Matcher<'a> {
        let words = query
            .terms
            .iter()
            .filter_map(|term| match term {
                Term::Word { needle, negate } => {
                    Some((memmem::Finder::new(needle.as_bytes()), *negate, needle.as_bytes()))
                }
                _ => None,
            })
            .collect();
        let within = query.within.as_ref().map(|path| index.lookup(path));
        Matcher {
            index,
            query,
            words,
            within: within.flatten(),
            impossible: matches!(within, Some(None)),
        }
    }

    /// Оценка совпадения; `None` — не подходит.
    fn score(&self, node: u32) -> Option<i32> {
        let index = self.index;
        let i = node as usize;
        let flags = index.flags[i];
        if self.impossible || flags & DELETED != 0 {
            return None;
        }
        let query = self.query;
        // Только «скрытый»: атрибут «системный» без него стоит и на обычных папках
        // («Документы» с desktop.ini), Проводник их показывает.
        if !query.hidden && flags & HIDDEN != 0 {
            return None;
        }
        let is_dir = flags & DIR != 0;
        if query.dirs.is_some_and(|dirs| dirs != is_dir) {
            return None;
        }
        let name = index.lower_name(node);
        if !query.exts.is_empty() {
            if is_dir {
                return None;
            }
            let ext =
                memchr::memrchr(b'.', name).filter(|&dot| dot > 0).map(|dot| &name[dot + 1..])?;
            if !query.exts.iter().any(|e| e.as_bytes() == ext) {
                return None;
            }
        }
        if let Some((min, max)) = query.size
            && (is_dir || index.size[i] < min || index.size[i] > max)
        {
            return None;
        }
        if let Some((min, max)) = query.modified {
            let modified = index.modified[i];
            if modified == i64::MIN || modified < min || modified > max {
                return None;
            }
        }
        let mut score = 0;
        for (finder, negate, needle) in &self.words {
            match finder.find(name) {
                Some(_) if *negate => return None,
                None if !*negate => return None,
                Some(at) => {
                    score += if name == *needle {
                        100
                    } else if at == 0 {
                        60
                    } else if !name[at - 1].is_ascii_alphanumeric() {
                        40
                    } else {
                        20
                    };
                }
                None => {}
            }
        }
        let mut path_needed = false;
        for term in &query.terms {
            match term {
                Term::Glob { pattern, negate } => {
                    let text: Vec<char> = String::from_utf8_lossy(name).chars().collect();
                    if crate::filter::glob_match(pattern, &text) == *negate {
                        return None;
                    }
                    score += 30;
                }
                Term::Path { .. } => path_needed = true,
                Term::Word { .. } => {}
            }
        }
        if let Some(within) = self.within {
            let mut current = index.parent[i];
            loop {
                if current == within {
                    break;
                }
                if current == NONE {
                    return None;
                }
                current = index.parent[current as usize];
            }
        }
        // Внутри скрытой папки (AppData, .git) — тоже скрытое. Проверяется последним: подъём
        // по родителям нужен только тем, кто прошёл остальное.
        if !query.hidden {
            let mut current = index.parent[i];
            while current != NONE {
                if index.flags[current as usize] & HIDDEN != 0 {
                    return None;
                }
                current = index.parent[current as usize];
            }
        }
        if path_needed {
            let path = index.path(node).to_string_lossy().to_lowercase().replace('/', "\\");
            for term in &query.terms {
                if let Term::Path { needle, negate } = term
                    && path.contains(needle.as_str()) == *negate
                {
                    return None;
                }
            }
            score += 10;
        }
        // Короткие имена — точнее; папки чуть выше файлов.
        score -= (name.len() / 8) as i32;
        if is_dir {
            score += 5;
        }
        Some(score)
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub fn file(size: u64, modified: i64) -> NodeInfo {
        NodeInfo { is_dir: false, size, modified, hidden: false, system: false }
    }

    pub fn dir() -> NodeInfo {
        NodeInfo { is_dir: true, ..Default::default() }
    }

    /// /d: Models/{qwen2.5-7b.Q4.gguf, llama.gguf, Qwen/}, Photos/{IMG_1.png}, notes.txt
    pub fn sample() -> VolumeIndex {
        let mut index = VolumeIndex::new(PathBuf::from("/d"));
        let models = index.add(ROOT, "Models", dir());
        index.add(models, "qwen2.5-7b.Q4.gguf", file(4 << 30, 1_700_000_000));
        index.add(models, "llama.gguf", file(7 << 30, 1_600_000_000));
        index.add(models, "Qwen", dir());
        let photos = index.add(ROOT, "Photos", dir());
        index.add(photos, "IMG_1.png", file(2 << 20, 1_750_000_000));
        let mut hidden = file(1, 1);
        hidden.hidden = true;
        index.add(photos, "secret.png", hidden);
        index.add(ROOT, "notes.txt", file(10, 1_000));
        index
    }

    fn names(index: &VolumeIndex, text: &str) -> Vec<String> {
        let query = query::parse(text, 1_760_000_000).unwrap();
        index
            .search(&query, 100, &|| false)
            .hits
            .iter()
            .map(|&(n, _)| index.name(n).to_string())
            .collect()
    }

    #[test]
    fn lookup_and_paths() {
        let index = sample();
        assert_eq!(index.len(), 8);
        let node = index.lookup(Path::new("/d/models/QWEN2.5-7b.q4.gguf")).unwrap();
        assert_eq!(index.path(node), PathBuf::from("/d/Models/qwen2.5-7b.Q4.gguf"));
        assert_eq!(index.lookup(Path::new("/d")), Some(ROOT));
        assert_eq!(index.lookup(Path::new("/elsewhere/x")), None);
        assert_eq!(index.lookup(Path::new("/d/Models/none")), None);
    }

    #[test]
    fn search_ranks_and_filters() {
        let index = sample();
        assert_eq!(names(&index, "qwen gguf"), ["qwen2.5-7b.Q4.gguf"]);
        assert_eq!(
            names(&index, "qwen"),
            ["Qwen", "qwen2.5-7b.Q4.gguf"],
            "точное имя и папка выше"
        );
        assert_eq!(names(&index, "*.gguf size:>5gb"), ["llama.gguf"]);
        assert_eq!(names(&index, "ext:png"), ["IMG_1.png"], "скрытые — только с hidden:");
        assert_eq!(names(&index, "ext:png hidden:"), ["IMG_1.png", "secret.png"]);
        // Содержимое скрытой папки скрыто; «системная» без «скрытой» — обычная папка.
        let mut index = sample();
        let mut cache = dir();
        cache.hidden = true;
        let cache = index.add(ROOT, "AppData", cache);
        index.add(cache, "cached.png", file(5, 5));
        let mut documents = dir();
        documents.system = true;
        let documents = index.add(ROOT, "Documents", documents);
        index.add(documents, "scan.png", file(5, 5));
        assert_eq!(names(&index, "ext:png"), ["IMG_1.png", "scan.png"]);
        assert_eq!(names(&index, "ext:png hidden:").len(), 4);
        let index = sample();
        assert_eq!(names(&index, "gguf -llama"), ["qwen2.5-7b.Q4.gguf"]);
        assert_eq!(names(&index, "type:папка q"), ["Qwen"]);
        assert_eq!(names(&index, "in:/d/Photos img"), ["IMG_1.png"]);
        assert!(names(&index, "in:/elsewhere img").is_empty());
        assert_eq!(names(&index, "models/llama"), ["llama.gguf"]);
        assert!(names(&index, "").is_empty(), "пустой запрос ничего не ищет");
        let query = query::parse("dm:2023", 1_760_000_000).unwrap();
        let hits = index.search(&query, 100, &|| false);
        assert_eq!(
            hits.hits.iter().map(|&(n, _)| index.name(n)).collect::<Vec<_>>(),
            ["qwen2.5-7b.Q4.gguf"]
        );
    }

    #[test]
    fn limit_keeps_the_best_and_counts_all() {
        let mut index = VolumeIndex::new(PathBuf::from("/d"));
        for i in 0..50_000 {
            index.add(ROOT, &format!("report-{i}.txt"), file(1, 1));
        }
        index.add(ROOT, "report", file(1, 1));
        let query = query::parse("report", 0).unwrap();
        let hits = index.search(&query, 10, &|| false);
        assert_eq!(hits.total, 50_001);
        assert_eq!(hits.hits.len(), 10);
        assert_eq!(index.name(hits.hits[0].0), "report");
    }

    #[test]
    fn reconcile_adds_removes_and_reports_new_dirs() {
        let mut index = sample();
        let new_dirs = index.reconcile(
            Path::new("/d/Models"),
            &[
                ("llama.gguf".into(), file(8 << 30, 1)),
                ("Mistral".into(), dir()),
                ("qwen".into(), file(1, 1)),
            ],
        );
        assert_eq!(new_dirs, [PathBuf::from("/d/Models/Mistral")]);
        let models = index.lookup(Path::new("/d/Models")).unwrap();
        let mut children: Vec<&str> = index.children(models).map(|c| index.name(c)).collect();
        children.sort();
        assert_eq!(children, ["Mistral", "llama.gguf", "qwen"], "папка Qwen стала файлом qwen");
        assert_eq!(names(&index, "qwen2.5"), Vec::<String>::new());
        assert_eq!(names(&index, "size:>7gb"), ["llama.gguf"]);
        // Папки, которой не было, — создаётся вместе с предками.
        index.reconcile(Path::new("/d/New/Deep"), &[("x.txt".into(), file(1, 1))]);
        assert!(index.lookup(Path::new("/d/New/Deep/x.txt")).is_some());
        let before = index.len();
        index.remove(index.lookup(Path::new("/d/New")).unwrap());
        assert_eq!(index.len(), before - 3);
        assert!(index.garbage() > 0.0);
    }

    /// Замер: `cargo test --release -p mh-files-core -- --ignored --nocapture index_speed`.
    #[test]
    #[ignore]
    fn index_speed() {
        let mut index = VolumeIndex::new(PathBuf::from("/d"));
        let started = std::time::Instant::now();
        for d in 0..2_000 {
            let folder = index.add(ROOT, &format!("folder-{d}"), dir());
            for f in 0..1_000 {
                index.add(
                    folder,
                    &format!("document_{d}_{f}.part{}.txt", f % 7),
                    file(f as u64, 1),
                );
            }
        }
        println!("построение 2 млн: {:?}", started.elapsed());
        for text in ["document_1999_999", "part3 txt", "*.txt size:>990", "nothing-here"] {
            let query = query::parse(text, 0).unwrap();
            let started = std::time::Instant::now();
            let hits = index.search(&query, 1000, &|| false);
            println!("«{text}»: {} за {:?}", hits.total, started.elapsed());
        }
        let started = std::time::Instant::now();
        let bytes = snapshot::write(&index);
        println!("снимок {} МБ за {:?}", bytes.len() >> 20, started.elapsed());
        let started = std::time::Instant::now();
        snapshot::read(&bytes).unwrap();
        println!("загрузка за {:?}", started.elapsed());
    }

    #[test]
    fn entries_share_parents() {
        let index = sample();
        let mut parents = HashMap::new();
        let a = index.entry(index.lookup(Path::new("/d/Models/llama.gguf")).unwrap(), &mut parents);
        let b = index.entry(index.lookup(Path::new("/d/Models/Qwen")).unwrap(), &mut parents);
        assert!(Arc::ptr_eq(&a.parent, &b.parent));
        assert_eq!(a.path(), PathBuf::from("/d/Models/llama.gguf"));
        assert!(b.is_dir());
        assert_eq!(a.modified.map(unix), Some(1_600_000_000));
    }
}
