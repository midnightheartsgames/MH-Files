//! Файловые операции: копирование, перемещение, удаление, переименование, новая папка.
//!
//! В Windows всё идёт через `IFileOperation` в отдельном STA-потоке: корзина, запрос прав
//! администратора, «Отменить» в Проводнике. Прогресс приходит событиями [`OpEvent::Progress`],
//! пауза и отмена — через [`Executor::pause`] и [`Executor::cancel`]. Операции выполняются по
//! очереди в порядке поступления.
//!
//! Пути длиннее 260 символов Shell разобрать не может: такие операции выполняет запасная
//! реализация на `std::fs` (она же — единственная вне Windows).
//!
//! Правило PLAN.md §4/10: операция получает **точный** список путей; никаких масок и
//! «всё в папке».

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, Sender, unbounded};

use crate::Waker;

/// Что делать, если в папке назначения уже есть объект с тем же именем.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Hash)]
pub enum OnConflict {
    /// Спросить системным диалогом Windows (вне Windows — как [`OnConflict::KeepBoth`] при
    /// копировании в ту же папку, иначе ошибка).
    #[default]
    Ask,
    /// Заменить; папки сливаются, файлы внутри заменяются.
    Replace,
    /// Оставить оба: новый получает свободное имя.
    KeepBoth,
    /// Пропустить.
    Skip,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FileOp {
    /// Копировать в папку `dest`. Если папка та же, что у источника, — копия рядом с новым именем.
    Copy {
        sources: Vec<PathBuf>,
        dest: PathBuf,
        on_conflict: OnConflict,
    },
    /// Переместить в папку `dest`.
    Move {
        sources: Vec<PathBuf>,
        dest: PathBuf,
        on_conflict: OnConflict,
    },
    /// `permanent: false` — в корзину.
    Delete {
        paths: Vec<PathBuf>,
        permanent: bool,
    },
    /// Новое имя без пути.
    Rename {
        path: PathBuf,
        new_name: String,
    },
    NewFolder {
        parent: PathBuf,
        name: String,
    },
}

impl FileOp {
    pub fn copy(sources: Vec<PathBuf>, dest: PathBuf) -> FileOp {
        FileOp::Copy { sources, dest, on_conflict: OnConflict::Ask }
    }

    pub fn moving(sources: Vec<PathBuf>, dest: PathBuf) -> FileOp {
        FileOp::Move { sources, dest, on_conflict: OnConflict::Ask }
    }

    /// Короткое описание для строки состояния.
    pub fn describe(&self) -> String {
        let count = |n: usize| match n {
            1 => "1 объект".to_string(),
            n => format!("{n} об."),
        };
        match self {
            FileOp::Copy { sources, .. } => format!("Копирование: {}", count(sources.len())),
            FileOp::Move { sources, .. } => format!("Перемещение: {}", count(sources.len())),
            FileOp::Delete { paths, permanent: false } => {
                format!("В корзину: {}", count(paths.len()))
            }
            FileOp::Delete { paths, permanent: true } => {
                format!("Удаление: {}", count(paths.len()))
            }
            FileOp::Rename { new_name, .. } => format!("Переименование в «{new_name}»"),
            FileOp::NewFolder { name, .. } => format!("Новая папка «{name}»"),
        }
    }

    /// Папки, содержимое которых меняется: их стоит перечитать, если наблюдатель не успел.
    pub fn affected_dirs(&self) -> Vec<PathBuf> {
        let parents = |paths: &[PathBuf]| -> Vec<PathBuf> {
            paths.iter().filter_map(|path| path.parent().map(PathBuf::from)).collect()
        };
        let mut dirs = match self {
            FileOp::Copy { dest, .. } => vec![dest.clone()],
            FileOp::Move { sources, dest, .. } => {
                let mut dirs = parents(sources);
                dirs.push(dest.clone());
                dirs
            }
            FileOp::Delete { paths, .. } => parents(paths),
            FileOp::Rename { path, .. } => parents(std::slice::from_ref(path)),
            FileOp::NewFolder { parent, .. } => vec![parent.clone()],
        };
        dirs.sort();
        dirs.dedup();
        dirs
    }

    /// Обратные операции для «Отменить» по итогу этой. `None` — отменить нельзя (корзину
    /// возвращает Проводник, удаление насовсем не вернуть).
    pub fn inverse(&self, outcome: &OpOutcome) -> Option<Vec<FileOp>> {
        let OpOutcome::Done { created, pairs } = outcome else { return None };
        // Заменённые файлы не вернуть: отмена удалила бы и новые — без отмены надёжнее.
        if let FileOp::Copy { on_conflict: OnConflict::Replace, .. }
        | FileOp::Move { on_conflict: OnConflict::Replace, .. } = self
        {
            return None;
        }
        match self {
            FileOp::Rename { .. } => {
                let (from, to) = pairs.first()?;
                let name = from.file_name()?.to_string_lossy().into_owned();
                Some(vec![FileOp::Rename { path: to.clone(), new_name: name }])
            }
            FileOp::NewFolder { .. } | FileOp::Copy { .. } if !created.is_empty() => {
                Some(vec![FileOp::Delete { paths: created.clone(), permanent: false }])
            }
            FileOp::Move { .. } if !pairs.is_empty() => {
                // Назад по исходным папкам; если имя менялось при конфликте — ещё и вернуть имя.
                let mut groups: Vec<(PathBuf, Vec<PathBuf>)> = Vec::new();
                let mut renames = Vec::new();
                for (from, to) in pairs {
                    let parent = from.parent()?.to_path_buf();
                    match groups.iter_mut().find(|(dir, _)| *dir == parent) {
                        Some((_, items)) => items.push(to.clone()),
                        None => groups.push((parent.clone(), vec![to.clone()])),
                    }
                    if from.file_name() != to.file_name() {
                        let back = parent.join(to.file_name()?);
                        let name = from.file_name()?.to_string_lossy().into_owned();
                        renames.push(FileOp::Rename { path: back, new_name: name });
                    }
                }
                let mut ops: Vec<FileOp> = groups
                    .into_iter()
                    .map(|(dest, sources)| FileOp::Move {
                        sources,
                        dest,
                        on_conflict: OnConflict::Ask,
                    })
                    .collect();
                ops.extend(renames);
                Some(ops)
            }
            _ => None,
        }
    }

    /// Все пути, которые операция читает или создаёт.
    fn paths(&self) -> Vec<PathBuf> {
        let into = |sources: &[PathBuf], dest: &Path| -> Vec<PathBuf> {
            let mut paths: Vec<PathBuf> = sources.to_vec();
            paths.extend(sources.iter().filter_map(|s| s.file_name()).map(|n| dest.join(n)));
            paths
        };
        match self {
            FileOp::Copy { sources, dest, .. } | FileOp::Move { sources, dest, .. } => {
                into(sources, dest)
            }
            FileOp::Delete { paths, .. } => paths.clone(),
            FileOp::Rename { path, new_name } => vec![path.clone(), path.with_file_name(new_name)],
            FileOp::NewFolder { parent, name } => vec![parent.join(name)],
        }
    }

    /// Есть ли путь длиннее, чем разбирает Shell (MAX_PATH без завершающего нуля).
    pub fn has_long_paths(&self) -> bool {
        self.paths().iter().any(|path| path_units(path) > LEGACY_MAX_PATH)
    }
}

/// Предел пути для Shell.
pub const LEGACY_MAX_PATH: usize = 259;

/// Длина пути в единицах UTF-16 — так её считает Windows.
pub fn path_units(path: &Path) -> usize {
    path.to_string_lossy().encode_utf16().count()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct OpId(pub u64);

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OpOutcome {
    Done {
        /// Новые пути верхнего уровня: новая папка, переименованный объект, копии.
        created: Vec<PathBuf>,
        /// Откуда → куда для каждого скопированного, перемещённого или переименованного
        /// объекта верхнего уровня. По ним строится «Отменить».
        pairs: Vec<(PathBuf, PathBuf)>,
    },
    /// Отменено пользователем — нашей кнопкой или в системном диалоге. Часть работы могла
    /// быть сделана.
    Aborted,
}

impl OpOutcome {
    pub fn done(created: Vec<PathBuf>, pairs: Vec<(PathBuf, PathBuf)>) -> OpOutcome {
        OpOutcome::Done { created, pairs }
    }
}

/// Ход операции. Единицы — байты, если их удалось посчитать, иначе объекты.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Progress {
    pub done: u64,
    pub total: u64,
    /// Над чем идёт работа сейчас.
    pub item: Option<PathBuf>,
}

impl Progress {
    pub fn fraction(&self) -> Option<f32> {
        (self.total > 0).then(|| (self.done as f64 / self.total as f64).clamp(0.0, 1.0) as f32)
    }
}

/// Пауза и отмена одной операции. Реализация спрашивает [`Control::checkpoint`] между
/// порциями работы.
#[derive(Debug, Default)]
pub struct Control {
    cancelled: AtomicBool,
    paused: AtomicBool,
}

impl Control {
    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::Relaxed);
    }

    pub fn set_paused(&self, paused: bool) {
        self.paused.store(paused, Ordering::Relaxed);
    }

    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Relaxed)
    }

    pub fn is_paused(&self) -> bool {
        self.paused.load(Ordering::Relaxed)
    }

    /// Стоит на паузе, пока её не снимут. `false` — операцию отменили.
    pub fn checkpoint(&self) -> bool {
        while self.is_paused() && !self.is_cancelled() {
            std::thread::sleep(Duration::from_millis(50));
        }
        !self.is_cancelled()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OpEvent {
    Started { id: OpId, op: FileOp },
    Progress { id: OpId, progress: Progress },
    Finished { id: OpId, op: FileOp, result: Result<OpOutcome, String> },
}

type Controls = Arc<Mutex<HashMap<OpId, Arc<Control>>>>;

/// Очередь операций со своим потоком. Закрывается вместе с последней копией.
pub struct Executor {
    jobs: Sender<(OpId, FileOp, Arc<Control>)>,
    events: Receiver<OpEvent>,
    controls: Controls,
}

static NEXT_ID: AtomicU64 = AtomicU64::new(1);

/// Прогресс в UI уходит не чаще этого.
const PROGRESS_EVERY: Duration = Duration::from_millis(100);

impl Executor {
    pub fn new(waker: Waker) -> Executor {
        let (jobs, job_rx) = unbounded();
        let (event_tx, events) = unbounded();
        let controls: Controls = Arc::default();
        let thread_controls = controls.clone();
        std::thread::Builder::new()
            .name("file-ops".into())
            .spawn(move || run(job_rx, event_tx, waker, thread_controls))
            .expect("поток файловых операций");
        Executor { jobs, events, controls }
    }

    pub fn submit(&self, op: FileOp) -> OpId {
        let id = OpId(NEXT_ID.fetch_add(1, Ordering::Relaxed));
        let control = Arc::new(Control::default());
        self.controls.lock().unwrap().insert(id, control.clone());
        // Поток живёт, пока жив отправитель, — отправка не падает.
        let _ = self.jobs.send((id, op, control));
        id
    }

    /// Отменить: идущую — при следующей проверке, ждущую в очереди — не начиная.
    pub fn cancel(&self, id: OpId) {
        if let Some(control) = self.controls.lock().unwrap().get(&id) {
            control.cancel();
        }
    }

    pub fn pause(&self, id: OpId, paused: bool) {
        if let Some(control) = self.controls.lock().unwrap().get(&id) {
            control.set_paused(paused);
        }
    }

    pub fn is_paused(&self, id: OpId) -> bool {
        self.controls.lock().unwrap().get(&id).is_some_and(|control| control.is_paused())
    }

    pub fn events(&self) -> &Receiver<OpEvent> {
        &self.events
    }
}

fn run(
    jobs: Receiver<(OpId, FileOp, Arc<Control>)>,
    events: Sender<OpEvent>,
    waker: Waker,
    controls: Controls,
) {
    #[cfg(windows)]
    let _com = crate::win::com::Apartment::sta();
    for (id, op, control) in jobs {
        let result = if control.is_cancelled() {
            Ok(OpOutcome::Aborted)
        } else {
            let _ = events.send(OpEvent::Started { id, op: op.clone() });
            waker();
            let mut last = Instant::now() - PROGRESS_EVERY;
            let mut report = |progress: Progress| {
                if last.elapsed() >= PROGRESS_EVERY {
                    last = Instant::now();
                    let _ = events.send(OpEvent::Progress { id, progress });
                    waker();
                }
            };
            execute(&op, &control, &mut report)
        };
        controls.lock().unwrap().remove(&id);
        let _ = events.send(OpEvent::Finished { id, op, result });
        waker();
    }
}

/// Выполняет одну операцию в текущем потоке. В Windows поток должен быть STA.
pub fn execute(
    op: &FileOp,
    control: &Control,
    progress: &mut dyn FnMut(Progress),
) -> Result<OpOutcome, String> {
    #[cfg(windows)]
    {
        if op.has_long_paths() {
            if let FileOp::Delete { permanent: false, .. } = op {
                return Err("путь длиннее 260 символов не помещается в корзину; \
                     удалите насовсем (Shift+Delete)"
                    .into());
            }
            return portable::execute(op, control, progress);
        }
        crate::win::ops::execute(op, control, progress)
    }
    #[cfg(not(windows))]
    {
        portable::execute(op, control, progress)
    }
}

/// Реализация на `std::fs`: вне Windows — основная, в Windows — для длинных путей.
mod portable {
    use std::fs;
    use std::io::{Read, Write};
    use std::path::{Path, PathBuf};

    use super::{Control, FileOp, OnConflict, OpOutcome, Progress};

    const CHUNK: usize = 1 << 20;

    /// Общий ход копирования/удаления: сколько единиц всего и сколько сделано.
    struct Meter<'a> {
        done: u64,
        total: u64,
        report: &'a mut dyn FnMut(Progress),
    }

    impl Meter<'_> {
        fn add(&mut self, amount: u64, item: &Path) {
            self.done += amount;
            (self.report)(Progress {
                done: self.done,
                total: self.total,
                item: Some(item.to_path_buf()),
            });
        }
    }

    /// Ошибка отмены отличается от прочих, чтобы вернуть `Aborted`, а не текст.
    enum Stop {
        Cancelled,
        Failed(String),
    }

    impl From<String> for Stop {
        fn from(error: String) -> Stop {
            Stop::Failed(error)
        }
    }

    pub fn execute(
        op: &FileOp,
        control: &Control,
        report: &mut dyn FnMut(Progress),
    ) -> Result<OpOutcome, String> {
        match run(op, control, report) {
            Ok(outcome) => Ok(outcome),
            Err(Stop::Cancelled) => Ok(OpOutcome::Aborted),
            Err(Stop::Failed(error)) => Err(error),
        }
    }

    fn run(
        op: &FileOp,
        control: &Control,
        report: &mut dyn FnMut(Progress),
    ) -> Result<OpOutcome, Stop> {
        match op {
            FileOp::Copy { sources, dest, on_conflict } => {
                let total = sources.iter().map(|s| tree_size(s)).sum();
                let mut meter = Meter { done: 0, total, report };
                let mut pairs = Vec::new();
                for source in sources {
                    let Some(target) = target_for(source, dest, *on_conflict, true)? else {
                        meter.add(tree_size(source), source);
                        continue;
                    };
                    copy_tree(
                        source,
                        &target,
                        *on_conflict == OnConflict::Replace,
                        control,
                        &mut meter,
                    )?;
                    pairs.push((source.clone(), target));
                }
                let created = pairs.iter().map(|(_, to)| to.clone()).collect();
                Ok(OpOutcome::done(created, pairs))
            }
            FileOp::Move { sources, dest, on_conflict } => {
                let total = sources.len() as u64;
                let mut meter = Meter { done: 0, total, report };
                let mut pairs = Vec::new();
                for source in sources {
                    if !control.checkpoint() {
                        return Err(Stop::Cancelled);
                    }
                    let Some(target) = target_for(source, dest, *on_conflict, false)? else {
                        meter.add(1, source);
                        continue;
                    };
                    if target.exists() && *on_conflict == OnConflict::Replace {
                        remove(&target).map_err(|e| describe(&target, e))?;
                    }
                    if fs::rename(source, &target).is_err() {
                        // Другой диск: копия и удаление исходника.
                        let mut inner = Meter { done: 0, total: 0, report: &mut |_| {} };
                        copy_tree(source, &target, false, control, &mut inner)?;
                        remove(source).map_err(|e| describe(source, e))?;
                    }
                    meter.add(1, source);
                    pairs.push((source.clone(), target));
                }
                let created = pairs.iter().map(|(_, to)| to.clone()).collect();
                Ok(OpOutcome::done(created, pairs))
            }
            FileOp::Delete { paths, permanent } => {
                if !permanent {
                    return Err(Stop::Failed(
                        "корзина есть только в Windows; удалите насовсем (Shift+Delete)".into(),
                    ));
                }
                let mut meter = Meter { done: 0, total: paths.len() as u64, report };
                for path in paths {
                    if !control.checkpoint() {
                        return Err(Stop::Cancelled);
                    }
                    remove(path).map_err(|e| describe(path, e))?;
                    meter.add(1, path);
                }
                Ok(OpOutcome::done(Vec::new(), Vec::new()))
            }
            FileOp::Rename { path, new_name } => {
                let target = path.with_file_name(new_name);
                if target.exists() && !same_ignoring_case(path, &target) {
                    return Err(Stop::Failed(format!("уже существует: {}", target.display())));
                }
                fs::rename(path, &target).map_err(|e| describe(path, e))?;
                Ok(OpOutcome::done(vec![target.clone()], vec![(path.clone(), target)]))
            }
            FileOp::NewFolder { parent, name } => {
                let target = parent.join(name);
                fs::create_dir(&target).map_err(|e| describe(&target, e))?;
                Ok(OpOutcome::done(vec![target], Vec::new()))
            }
        }
    }

    fn same_ignoring_case(a: &Path, b: &Path) -> bool {
        a.to_string_lossy().to_lowercase() == b.to_string_lossy().to_lowercase()
    }

    fn file_name(path: &Path) -> Result<String, String> {
        path.file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .ok_or_else(|| format!("нет имени: {}", path.display()))
    }

    /// Куда положить `source` в `dest` с учётом конфликта. `None` — пропустить.
    fn target_for(
        source: &Path,
        dest: &Path,
        on_conflict: OnConflict,
        copying: bool,
    ) -> Result<Option<PathBuf>, String> {
        let name = file_name(source)?;
        let target = dest.join(&name);
        if !target.exists() {
            return Ok(Some(target));
        }
        let same_dir = source.parent() == Some(dest);
        if same_dir && !copying {
            // Перемещение в ту же папку — ничего не делать.
            return Ok(None);
        }
        match on_conflict {
            OnConflict::Skip => Ok(None),
            OnConflict::Replace if !same_dir => Ok(Some(target)),
            OnConflict::KeepBoth | OnConflict::Replace => Ok(Some(free_name(source, dest, &name))),
            OnConflict::Ask if same_dir => Ok(Some(free_name(source, dest, &name))),
            OnConflict::Ask => Err(format!("уже существует: {}", target.display())),
        }
    }

    /// Свободное имя: «имя - копия», «имя - копия (2)»… как в Проводнике.
    fn free_name(source: &Path, dest: &Path, name: &str) -> PathBuf {
        let (stem, ext) = match name.rfind('.') {
            Some(dot) if dot > 0 && !source.is_dir() => (&name[..dot], &name[dot..]),
            _ => (name, ""),
        };
        (1..)
            .map(|n| {
                let suffix = if n == 1 {
                    " - копия".to_string()
                } else {
                    format!(" - копия ({n})")
                };
                dest.join(format!("{stem}{suffix}{ext}"))
            })
            .find(|candidate| !candidate.exists())
            .expect("бесконечная последовательность")
    }

    /// Размер дерева в байтах, без перехода по ссылкам.
    fn tree_size(path: &Path) -> u64 {
        let Ok(meta) = fs::symlink_metadata(path) else { return 0 };
        if !meta.is_dir() {
            return meta.len();
        }
        fs::read_dir(path)
            .map(|entries| entries.flatten().map(|entry| tree_size(&entry.path())).sum())
            .unwrap_or(0)
    }

    fn copy_tree(
        source: &Path,
        target: &Path,
        replace: bool,
        control: &Control,
        meter: &mut Meter,
    ) -> Result<(), Stop> {
        let meta = fs::symlink_metadata(source).map_err(|e| describe(source, e))?;
        if meta.is_dir() {
            if !(replace && target.is_dir()) {
                fs::create_dir(target).map_err(|e| describe(target, e))?;
            }
            let entries = fs::read_dir(source).map_err(|e| describe(source, e))?;
            for entry in entries {
                let entry = entry.map_err(|e| describe(source, e))?;
                copy_tree(&entry.path(), &target.join(entry.file_name()), replace, control, meter)?;
            }
            return Ok(());
        }
        copy_file(source, target, control, meter)
    }

    /// Копирование порциями: между ними — пауза, отмена и прогресс. Отменённый файл удаляется.
    fn copy_file(
        source: &Path,
        target: &Path,
        control: &Control,
        meter: &mut Meter,
    ) -> Result<(), Stop> {
        let mut input = fs::File::open(source).map_err(|e| describe(source, e))?;
        let mut output = fs::File::create(target).map_err(|e| describe(target, e))?;
        let mut buffer = vec![0u8; CHUNK];
        loop {
            if !control.checkpoint() {
                drop(output);
                let _ = fs::remove_file(target);
                return Err(Stop::Cancelled);
            }
            let read = input.read(&mut buffer).map_err(|e| describe(source, e))?;
            if read == 0 {
                break;
            }
            output.write_all(&buffer[..read]).map_err(|e| describe(target, e))?;
            meter.add(read as u64, source);
        }
        if let Ok(modified) = fs::metadata(source).and_then(|m| m.modified()) {
            let _ = output.set_modified(modified);
        }
        Ok(())
    }

    fn remove(path: &Path) -> std::io::Result<()> {
        let meta = fs::symlink_metadata(path)?;
        if meta.is_dir() { fs::remove_dir_all(path) } else { fs::remove_file(path) }
    }

    fn describe(path: &Path, error: std::io::Error) -> String {
        format!("{}: {error}", path.display())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("mh-files-ops-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn exec(op: &FileOp) -> Result<OpOutcome, String> {
        portable::execute(op, &Control::default(), &mut |_| {})
    }

    #[test]
    fn copy_into_same_folder_makes_a_copy() {
        let dir = temp_dir("copy");
        std::fs::write(dir.join("a.txt"), "x").unwrap();
        let op = FileOp::copy(vec![dir.join("a.txt")], dir.clone());
        exec(&op).unwrap();
        let second = exec(&op).unwrap();
        assert!(dir.join("a - копия.txt").exists());
        assert!(dir.join("a - копия (2).txt").exists());
        let OpOutcome::Done { pairs, .. } = second else { panic!() };
        assert_eq!(pairs, [(dir.join("a.txt"), dir.join("a - копия (2).txt"))]);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn conflicts_follow_the_policy() {
        let dir = temp_dir("conflict");
        std::fs::create_dir_all(dir.join("src")).unwrap();
        std::fs::create_dir_all(dir.join("dst")).unwrap();
        std::fs::write(dir.join("src/f"), "new").unwrap();
        std::fs::write(dir.join("dst/f"), "old").unwrap();
        let sources = vec![dir.join("src/f")];
        let dest = dir.join("dst");
        let with = |on_conflict| FileOp::Copy {
            sources: sources.clone(),
            dest: dest.clone(),
            on_conflict,
        };
        assert!(exec(&with(OnConflict::Ask)).is_err(), "вне Windows спросить некому");
        exec(&with(OnConflict::Skip)).unwrap();
        assert_eq!(std::fs::read_to_string(dir.join("dst/f")).unwrap(), "old");
        exec(&with(OnConflict::KeepBoth)).unwrap();
        assert!(dir.join("dst/f - копия").exists());
        exec(&with(OnConflict::Replace)).unwrap();
        assert_eq!(std::fs::read_to_string(dir.join("dst/f")).unwrap(), "new");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn move_rename_and_new_folder() {
        let dir = temp_dir("move");
        exec(&FileOp::NewFolder { parent: dir.clone(), name: "sub".into() }).unwrap();
        std::fs::write(dir.join("f"), "x").unwrap();
        let moved = exec(&FileOp::moving(vec![dir.join("f")], dir.join("sub"))).unwrap();
        assert_eq!(
            moved,
            OpOutcome::done(vec![dir.join("sub/f")], vec![(dir.join("f"), dir.join("sub/f"))])
        );
        exec(&FileOp::Rename { path: dir.join("sub/f"), new_name: "g".into() }).unwrap();
        assert!(dir.join("sub/g").exists());
        let err = exec(&FileOp::Delete { paths: vec![dir.join("sub")], permanent: false });
        assert!(err.is_err(), "без корзины ничего не удаляется");
        exec(&FileOp::Delete { paths: vec![dir.join("sub")], permanent: true }).unwrap();
        assert!(!dir.join("sub").exists());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn cancelled_copy_stops_and_cleans_up() {
        let dir = temp_dir("cancel");
        std::fs::write(dir.join("big"), vec![0u8; 3 << 20]).unwrap();
        std::fs::create_dir_all(dir.join("dst")).unwrap();
        let control = Control::default();
        let mut reports = 0;
        let op = FileOp::copy(vec![dir.join("big")], dir.join("dst"));
        let result = portable::execute(&op, &control, &mut |progress| {
            reports += 1;
            assert_eq!(progress.total, 3 << 20);
            control.cancel();
        });
        assert_eq!(result, Ok(OpOutcome::Aborted));
        assert_eq!(reports, 1);
        assert!(!dir.join("dst/big").exists(), "недокопированный файл удалён");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn queued_operation_cancelled_before_start_does_nothing() {
        let dir = temp_dir("queued");
        let (jobs, job_rx) = unbounded();
        let (event_tx, events) = unbounded();
        let done = Arc::new(Control::default());
        let cancelled = Arc::new(Control::default());
        cancelled.cancel();
        jobs.send((OpId(1), FileOp::NewFolder { parent: dir.clone(), name: "a".into() }, done))
            .unwrap();
        jobs.send((
            OpId(2),
            FileOp::NewFolder { parent: dir.clone(), name: "b".into() },
            cancelled,
        ))
        .unwrap();
        drop(jobs);
        run(job_rx, event_tx, crate::no_waker(), Controls::default());
        let finished: Vec<_> = events
            .try_iter()
            .filter_map(|event| match event {
                OpEvent::Finished { id, result, .. } => Some((id, result)),
                _ => None,
            })
            .collect();
        assert!(matches!(&finished[0], (OpId(1), Ok(OpOutcome::Done { .. }))));
        assert_eq!(finished[1], (OpId(2), Ok(OpOutcome::Aborted)));
        assert!(dir.join("a").exists() && !dir.join("b").exists());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn inverses() {
        let p = PathBuf::from;
        let rename = FileOp::Rename { path: p("/d/a"), new_name: "b".into() };
        let done = OpOutcome::done(vec![p("/d/b")], vec![(p("/d/a"), p("/d/b"))]);
        assert_eq!(
            rename.inverse(&done),
            Some(vec![FileOp::Rename { path: p("/d/b"), new_name: "a".into() }])
        );
        let moving = FileOp::moving(vec![p("/x/a"), p("/y/b")], p("/z"));
        let done =
            OpOutcome::done(vec![], vec![(p("/x/a"), p("/z/a")), (p("/y/b"), p("/z/b - копия"))]);
        let ops = moving.inverse(&done).unwrap();
        assert_eq!(ops.len(), 3);
        assert_eq!(ops[0], FileOp::moving(vec![p("/z/a")], p("/x")));
        assert_eq!(ops[1], FileOp::moving(vec![p("/z/b - копия")], p("/y")));
        assert_eq!(ops[2], FileOp::Rename { path: p("/y/b - копия"), new_name: "b".into() });
        let copy = FileOp::copy(vec![p("/x/a")], p("/z"));
        let done = OpOutcome::done(vec![p("/z/a")], vec![(p("/x/a"), p("/z/a"))]);
        assert_eq!(
            copy.inverse(&done),
            Some(vec![FileOp::Delete { paths: vec![p("/z/a")], permanent: false }])
        );
        let replaced = FileOp::Copy {
            sources: vec![p("/x/a")],
            dest: p("/z"),
            on_conflict: OnConflict::Replace,
        };
        assert_eq!(replaced.inverse(&done), None, "заменённое не вернуть — отмены нет");
        let delete = FileOp::Delete { paths: vec![p("/a")], permanent: false };
        assert_eq!(delete.inverse(&OpOutcome::done(vec![], vec![])), None);
        assert_eq!(rename.inverse(&OpOutcome::Aborted), None);
    }

    #[test]
    fn long_paths_are_detected() {
        let long = PathBuf::from(format!("/{}", "a".repeat(300)));
        assert!(FileOp::Delete { paths: vec![long], permanent: true }.has_long_paths());
        let short = FileOp::Rename { path: "/a/b".into(), new_name: "c".into() };
        assert!(!short.has_long_paths());
    }
}
