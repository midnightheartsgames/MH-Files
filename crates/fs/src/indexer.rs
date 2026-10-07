//! Служба индекса дисков: поиск «везде» за миллисекунды.
//!
//! У каждого тома свой поток. Он загружает снимок с диска (поиск работает сразу), затем
//! досматривает то, что изменилось, пока программа была закрыта: по журналу USN, если есть
//! права администратора, иначе полным обходом в фоне. Дальше индекс держится свежим
//! наблюдателем за всем деревом (`ReadDirectoryChangesW`) и сверкой папок, которые
//! пользователь открывает сам. Снимок сохраняется после обхода, раз в четверть часа при
//! изменениях и при выходе.
//!
//! Обход пишет в общий индекс по одной папке под коротким замком, поэтому поиск видит
//! частичные результаты уже во время первого обхода.

use std::collections::{BTreeSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, RecvTimeoutError, Sender, unbounded};
use mh_files_core::Entry;
use mh_files_core::index::{self, JournalMark, NodeInfo, ROOT, VolumeIndex, snapshot};
use mh_files_core::settings::IndexSettings;
use mh_files_platform::volume;
use mh_files_platform::watch::{WatchEvent, Watcher};
use parking_lot::{Mutex, RwLock};

use crate::{CancelToken, Event, Ticket, Workers, read};

/// Больше результатов не показывается: дальше запрос стоит уточнить.
pub const RESULT_LIMIT: usize = 20_000;
/// Изменения на диске копятся столько, прежде чем папки перечитываются.
const SETTLE: Duration = Duration::from_millis(700);
const PROGRESS_EVERY: Duration = Duration::from_millis(250);
const SAVE_EVERY: Duration = Duration::from_secs(15 * 60);

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VolumeState {
    /// Читается снимок.
    Loading,
    /// Имена читаются из MFT (администратор, целый том NTFS).
    ReadingMft,
    /// Обход диска; сколько папок прочитано.
    Scanning {
        dirs: usize,
    },
    /// Досмотр папок, изменённых, пока программа была закрыта.
    CatchingUp {
        done: usize,
        total: usize,
    },
    Ready,
    Failed(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VolumeStatus {
    pub root: PathBuf,
    pub state: VolumeState,
    /// Файлов и папок в индексе.
    pub entries: usize,
    /// Изменения на диске попадают в индекс сразу.
    pub live: bool,
    /// Изменения за время простоя берутся из журнала USN.
    pub journal: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct IndexStatus {
    pub enabled: bool,
    pub volumes: Vec<VolumeStatus>,
}

impl IndexStatus {
    pub fn entries(&self) -> usize {
        self.volumes.iter().map(|v| v.entries).sum()
    }

    /// Идёт загрузка или обход хотя бы одного тома.
    pub fn busy(&self) -> bool {
        self.volumes.iter().any(|v| {
            matches!(
                v.state,
                VolumeState::Loading
                    | VolumeState::ReadingMft
                    | VolumeState::Scanning { .. }
                    | VolumeState::CatchingUp { .. }
            )
        })
    }
}

/// Итог поиска по индексу.
#[derive(Debug)]
pub struct IndexResults {
    /// Лучшие первыми, не больше [`RESULT_LIMIT`].
    pub entries: Vec<Entry>,
    /// Сколько подошло всего.
    pub total: usize,
    pub elapsed: Duration,
}

enum Job {
    Watch(Vec<WatchEvent>),
    /// Перечитать папки (после операций с файлами).
    Dirs(Vec<PathBuf>),
    /// Содержимое папки, уже прочитанное для показа.
    Seen {
        dir: PathBuf,
        actual: Vec<(String, NodeInfo)>,
        links: Vec<bool>,
    },
    Rescan,
    Stop(Sender<()>),
}

struct Volume {
    root: PathBuf,
    index: RwLock<VolumeIndex>,
    status: Mutex<VolumeStatus>,
    jobs: Sender<Job>,
    stop: AtomicBool,
    /// Исключённые папки в нижнем регистре.
    exclude: Vec<PathBuf>,
    /// Серийный номер тома съёмного диска: снимок у каждой флешки свой, хоть буква и та же.
    serial: Option<u32>,
}

struct Shared {
    workers: Workers,
    /// Папка снимков.
    store: PathBuf,
    elevated: bool,
    state: Mutex<State>,
    /// Перезапуски по смене настроек идут строго по очереди.
    restart: Mutex<()>,
    last_notice: Mutex<Instant>,
    /// Опрос подключённых дисков запущен (один на всё время работы).
    polling: AtomicBool,
}

#[derive(Default)]
struct State {
    settings: Option<IndexSettings>,
    volumes: Vec<Arc<Volume>>,
}

/// Служба индекса. Дешёвая в клонировании.
#[derive(Clone)]
pub struct Indexer {
    shared: Arc<Shared>,
}

impl Indexer {
    /// `store` — папка для снимков. Служба не запущена до [`Indexer::configure`].
    pub fn new(workers: Workers, store: PathBuf) -> Indexer {
        Indexer {
            shared: Arc::new(Shared {
                workers,
                store,
                elevated: volume::is_elevated(),
                state: Mutex::new(State::default()),
                restart: Mutex::new(()),
                last_notice: Mutex::new(Instant::now()),
                polling: AtomicBool::new(false),
            }),
        }
    }

    /// Применить настройки: если что-то поменялось, тома перезапускаются в фоне.
    pub fn configure(&self, settings: &IndexSettings) {
        if self.shared.state.lock().settings.as_ref() == Some(settings) {
            return;
        }
        if settings.enabled
            && settings.roots.is_empty()
            && !self.shared.polling.swap(true, Ordering::Relaxed)
        {
            let shared = self.shared.clone();
            self.shared.workers.spawn("index-drives", move |_| poll_drives(&shared));
        }
        self.shared.state.lock().settings = Some(settings.clone());
        let shared = self.shared.clone();
        let settings = settings.clone();
        self.shared.workers.spawn("index-restart", move |_| {
            let _order = shared.restart.lock();
            stop_all(&shared, Duration::from_secs(10));
            if shared.state.lock().settings.as_ref() != Some(&settings) {
                return; // Пока ждали, настройки сменились ещё раз — запустит следующий.
            }
            let volumes: Vec<Arc<Volume>> = if settings.enabled {
                roots(&settings)
                    .into_iter()
                    .map(|root| start_volume(&shared, root, &settings))
                    .collect()
            } else {
                Vec::new()
            };
            shared.state.lock().volumes = volumes;
            shared.notify(false, true);
        });
    }

    pub fn status(&self) -> IndexStatus {
        let state = self.shared.state.lock();
        IndexStatus {
            enabled: state.settings.as_ref().is_some_and(|s| s.enabled),
            volumes: state.volumes.iter().map(|v| v.status.lock().clone()).collect(),
        }
    }

    /// Запущен ли с правами администратора (тогда работает журнал USN).
    pub fn elevated(&self) -> bool {
        self.shared.elevated
    }

    /// Поиск по всем томам. `show_hidden` — показывать скрытые и системные, как в папках.
    pub fn search(&self, ticket: Ticket, text: String, show_hidden: bool) -> CancelToken {
        let cancel = CancelToken::default();
        let token = cancel.clone();
        let volumes = self.shared.state.lock().volumes.clone();
        self.shared.workers.spawn("index-search", move |workers| {
            let result = search(&volumes, &text, show_hidden, &token);
            if !token.is_cancelled() {
                workers.send(Event::IndexResults { ticket, result });
            }
        });
        cancel
    }

    /// Папка прочитана для показа: сверить с индексом. Так индекс свеж даже там, где
    /// наблюдателя нет (сетевые диски, не Windows).
    pub fn observe(&self, dir: &Path, entries: &[Entry]) {
        let Some(volume) = self.volume_of(dir) else { return };
        if volume.excluded(dir) {
            return;
        }
        let actual = entries.iter().map(|e| (e.name.clone(), NodeInfo::of(e))).collect();
        let links = entries.iter().map(|e| e.attributes.reparse()).collect();
        let _ = volume.jobs.send(Job::Seen { dir: dir.to_path_buf(), actual, links });
    }

    /// Перечитать папки — после операций с файлами.
    pub fn refresh(&self, dirs: &[PathBuf]) {
        let volumes = self.shared.state.lock().volumes.clone();
        for volume in volumes {
            let mine: Vec<PathBuf> =
                dirs.iter().filter(|d| d.starts_with(&volume.root)).cloned().collect();
            if !mine.is_empty() {
                let _ = volume.jobs.send(Job::Dirs(mine));
            }
        }
    }

    /// Список дисков изменился (сообщает UI): тома подключённых — запустить, отключённых —
    /// остановить; остальные не трогаются.
    pub fn drives_changed(&self) {
        request_sync(&self.shared);
    }

    /// Обойти все тома заново.
    pub fn rescan(&self) {
        for volume in self.shared.state.lock().volumes.iter() {
            let _ = volume.jobs.send(Job::Rescan);
        }
    }

    /// Остановить и сохранить снимки; ждать не дольше `wait`.
    pub fn shutdown(&self, wait: Duration) {
        stop_all(&self.shared, wait);
    }

    fn volume_of(&self, path: &Path) -> Option<Arc<Volume>> {
        let state = self.shared.state.lock();
        state
            .volumes
            .iter()
            .filter(|v| path.starts_with(&v.root))
            .max_by_key(|v| v.root.as_os_str().len())
            .cloned()
    }
}

impl Shared {
    /// Сообщить UI. Ход обхода — не чаще раза в [`PROGRESS_EVERY`]; смена состояния и
    /// содержимое после изменений на диске — сразу.
    fn notify(&self, content: bool, force: bool) {
        let mut last = self.last_notice.lock();
        if !force && last.elapsed() < PROGRESS_EVERY {
            return;
        }
        *last = Instant::now();
        drop(last);
        self.workers.send(Event::IndexChanged { content });
    }

    /// Файл снимка тома. У съёмного диска в имени и серийный номер: другая флешка на той же
    /// букве не получает чужой снимок.
    fn snapshot_path(&self, root: &Path, serial: Option<u32>) -> PathBuf {
        let text = root.to_string_lossy().to_lowercase();
        let label: String = text.chars().filter(|c| c.is_alphanumeric()).take(24).collect();
        let hash = text.bytes().fold(0xcbf2_9ce4_8422_2325u64, |h, b| {
            (h ^ u64::from(b)).wrapping_mul(0x0000_0100_0000_01b3)
        });
        match serial {
            Some(serial) => self.store.join(format!("{label}-{serial:08x}-{hash:016x}.idx")),
            None => self.store.join(format!("{label}-{hash:016x}.idx")),
        }
    }
}

/// Что индексировать: из настроек или все локальные диски (вне Windows — домашняя папка).
/// Вложенные корни отбрасываются: их содержимое уже в объемлющем.
fn roots(settings: &IndexSettings) -> Vec<PathBuf> {
    let mut roots: Vec<PathBuf> = if settings.roots.is_empty() {
        default_roots(settings)
    } else {
        settings.roots.iter().map(|r| mh_files_core::location::normalize(r)).collect()
    };
    roots.sort();
    roots.dedup();
    let all = roots.clone();
    roots.retain(|root| !all.iter().any(|other| other != root && root.starts_with(other)));
    roots
}

fn default_roots(settings: &IndexSettings) -> Vec<PathBuf> {
    if cfg!(windows) {
        use mh_files_platform::drives::{DriveKind, drive_roots, volume_serial};
        drive_roots()
            .into_iter()
            .filter(|(root, kind)| match kind {
                DriveKind::Fixed => true,
                // Кардридер без карты держит букву — индексировать нечего.
                DriveKind::Removable => settings.removable && volume_serial(root).is_some(),
                DriveKind::Network => settings.network,
                _ => false,
            })
            .map(|(root, _)| root)
            .collect()
    } else {
        std::env::var_os("HOME").map(PathBuf::from).into_iter().collect()
    }
}

/// Как часто проверять, не подключили ли или отключили диск, если UI не сообщил сам.
const DRIVES_EVERY: Duration = Duration::from_secs(10);

/// Запасной опрос дисков: UI сообщает о смене списка сразу ([`Indexer::drives_changed`]), но
/// кардридер, например, букву не меняет. Живёт всё время работы; когда индексируются
/// заданные папки, а не диски, ничего не делает.
fn poll_drives(shared: &Arc<Shared>) {
    loop {
        std::thread::sleep(DRIVES_EVERY);
        sync_drives(shared);
    }
}

fn request_sync(shared: &Arc<Shared>) {
    let shared = shared.clone();
    shared.workers.clone().spawn("index-drives", move |_| sync_drives(&shared));
}

/// Диски приходят и уходят: тома подключённых запускаются, отключённых (и флешек, сменившихся
/// на той же букве) — останавливаются, их снимок остаётся до следующего раза. Остальные тома
/// не трогаются.
fn sync_drives(shared: &Arc<Shared>) {
    let settings = shared.state.lock().settings.clone();
    let Some(settings) = settings.filter(|s| s.enabled && s.roots.is_empty()) else { return };
    let _order = shared.restart.lock();
    // Пока ждали очереди, настройки могли смениться — тогда тома запустил configure.
    if shared.state.lock().settings.as_ref() != Some(&settings) {
        return;
    }
    let wanted = roots(&settings);
    let (gone, kept): (Vec<_>, Vec<_>) =
        std::mem::take(&mut shared.state.lock().volumes).into_iter().partition(|volume| {
            !wanted.contains(&volume.root)
                || volume.serial.is_some_and(|serial| {
                    mh_files_platform::drives::volume_serial(&volume.root) != Some(serial)
                })
        });
    let added: Vec<PathBuf> =
        wanted.into_iter().filter(|root| !kept.iter().any(|v| &v.root == root)).collect();
    let changed = !gone.is_empty() || !added.is_empty();
    shared.state.lock().volumes = kept;
    stop_volumes(gone, Duration::from_secs(10));
    if shared.state.lock().settings.as_ref() != Some(&settings) {
        return;
    }
    let started: Vec<Arc<Volume>> =
        added.into_iter().map(|root| start_volume(shared, root, &settings)).collect();
    shared.state.lock().volumes.extend(started);
    if changed {
        shared.notify(true, true);
    }
}

/// Серийный номер тома, если это съёмный диск (флешка, карта, диск на USB).
fn removable_serial(root: &Path) -> Option<u32> {
    use mh_files_platform::drives::{DriveKind, drive_roots, volume_serial};
    let removable = drive_roots()
        .into_iter()
        .any(|(r, kind)| r.as_path() == root && kind == DriveKind::Removable);
    if removable { volume_serial(root) } else { None }
}

fn stop_all(shared: &Shared, wait: Duration) {
    let volumes = std::mem::take(&mut shared.state.lock().volumes);
    stop_volumes(volumes, wait);
}

fn stop_volumes(volumes: Vec<Arc<Volume>>, wait: Duration) {
    let deadline = Instant::now() + wait;
    let mut done = Vec::new();
    for volume in &volumes {
        volume.stop.store(true, Ordering::Relaxed);
        let (tx, rx) = unbounded();
        if volume.jobs.send(Job::Stop(tx)).is_ok() {
            done.push(rx);
        }
    }
    for rx in done {
        let _ = rx.recv_timeout(deadline.saturating_duration_since(Instant::now()));
    }
}

fn start_volume(shared: &Arc<Shared>, root: PathBuf, settings: &IndexSettings) -> Arc<Volume> {
    let (tx, rx) = unbounded();
    let exclude = settings.exclude.iter().map(|path| exclude_key(path)).collect();
    let serial = removable_serial(&root);
    let volume = Arc::new(Volume {
        index: RwLock::new(VolumeIndex::new(root.clone())),
        status: Mutex::new(VolumeStatus {
            root: root.clone(),
            state: VolumeState::Loading,
            entries: 0,
            live: false,
            journal: false,
        }),
        root,
        jobs: tx,
        stop: AtomicBool::new(false),
        exclude,
        serial,
    });
    let (shared, me, rescan) = (shared.clone(), volume.clone(), settings.rescan_on_start);
    shared.workers.clone().spawn("index-volume", move |_| run(&shared, &me, &rx, rescan));
    volume
}

fn lower(path: &Path) -> PathBuf {
    if cfg!(windows) { PathBuf::from(path.to_string_lossy().to_lowercase()) } else { path.into() }
}

/// Исключённая папка для сравнения: без кавычек («Копировать как путь» Проводника), без
/// `\\?\`, без `.`/`..` и хвостовой черты, в нижнем регистре.
fn exclude_key(path: &Path) -> PathBuf {
    let text = path.to_string_lossy();
    let text = text.trim().trim_matches('"');
    let plain = match text.strip_prefix(r"\\?\UNC\") {
        Some(rest) => format!(r"\\{rest}"),
        None => text.strip_prefix(r"\\?\").unwrap_or(text).to_string(),
    };
    lower(&mh_files_core::location::normalize(Path::new(&plain)))
}

impl Volume {
    fn excluded(&self, path: &Path) -> bool {
        if self.exclude.is_empty() {
            return false;
        }
        let path = lower(path);
        self.exclude.iter().any(|e| path.starts_with(e))
    }

    /// Содержимое папки `dir` без исключённых записей. Сверка папки тогда сама убирает из
    /// индекса исключённое раньше — вместе со всем, что под ним.
    fn without_excluded(&self, dir: &Path, (actual, links): Children) -> Children {
        if self.exclude.is_empty() {
            return (actual, links);
        }
        actual
            .into_iter()
            .zip(links)
            .filter(|((name, _), _)| !self.excluded(&dir.join(name)))
            .unzip()
    }

    /// Снимок мог быть построен до того, как папку исключили: убрать её из индекса. `true` —
    /// что-то убрано.
    fn prune_excluded(&self, index: &mut VolumeIndex) -> bool {
        let root = lower(&self.root);
        let mut pruned = false;
        for key in &self.exclude {
            let Ok(relative) = key.strip_prefix(&root) else { continue };
            if relative.as_os_str().is_empty() {
                continue; // Исключён весь корень тома — так индекс не выключают.
            }
            if let Some(node) = index.lookup(&self.root.join(relative)) {
                index.remove(node);
                pruned = true;
            }
        }
        pruned
    }

    fn set_state(&self, shared: &Shared, state: VolumeState) {
        let entries = self.index.read().len();
        let force = {
            let mut status = self.status.lock();
            let changed = std::mem::discriminant(&status.state) != std::mem::discriminant(&state);
            status.state = state;
            status.entries = entries;
            changed
        };
        shared.notify(false, force);
    }

    fn stopped(&self) -> bool {
        self.stop.load(Ordering::Relaxed)
    }

    /// Обойти дерево под узлом `start` (путь `path`), сверяя каждую папку. `false` —
    /// обход прерван остановкой.
    fn walk(&self, start: u32, path: PathBuf, mut progress: impl FnMut(usize)) -> bool {
        let mut queue = VecDeque::from([(start, path)]);
        let mut dirs = 0;
        while let Some((node, path)) = queue.pop_front() {
            if self.stopped() {
                return false;
            }
            dirs += 1;
            progress(dirs);
            // Нечитаемая папка (нет доступа) остаётся какой была.
            let Some(children) = read_children(&path) else { continue };
            let (actual, links) = self.without_excluded(&path, children);
            let nodes = self.index.write().reconcile_node(node, &actual);
            for (((child, _), (name, info)), link) in nodes.into_iter().zip(&actual).zip(links) {
                // Ссылки и junction не обходятся: петли и чужие тома.
                if info.is_dir && !link {
                    queue.push_back((child, path.join(name)));
                }
            }
        }
        true
    }

    /// Перечитать одну папку; новые папки внутри обходятся целиком.
    fn refresh_dir(&self, dir: &Path) {
        if self.excluded(dir) || !dir.starts_with(&self.root) {
            return;
        }
        match read_children(dir) {
            Some(children) => {
                let (actual, links) = self.without_excluded(dir, children);
                self.apply(dir, &actual, &links)
            }
            None => {
                if std::fs::symlink_metadata(dir).is_err() {
                    let mut index = self.index.write();
                    if let Some(node) = index.lookup(dir) {
                        index.remove(node);
                    }
                }
            }
        }
    }

    fn apply(&self, dir: &Path, actual: &[(String, NodeInfo)], links: &[bool]) {
        let new_dirs = {
            let mut index = self.index.write();
            let Some(node) = index.ensure_dir(dir) else { return };
            let nodes = index.reconcile_node(node, actual);
            let mut new_dirs = Vec::new();
            for (((child, new), (name, info)), link) in nodes.into_iter().zip(actual).zip(links) {
                if new && info.is_dir && !link {
                    new_dirs.push((child, dir.join(name)));
                }
            }
            new_dirs
        };
        for (node, path) in new_dirs {
            if !self.walk(node, path, |_| {}) {
                return;
            }
        }
    }

    /// Полный обход. `true` — дошёл до конца.
    fn scan(&self, shared: &Shared) -> bool {
        let mark = if shared.elevated { volume::journal_state(&self.root).ok() } else { None };
        // Пустой индекс целого тома у администратора — сначала все имена из MFT за секунды:
        // поиск работает сразу, а обход следом приносит размеры и даты.
        if mark.is_some() && self.index.read().is_empty() && is_volume_root(&self.root) {
            self.set_state(shared, VolumeState::ReadingMft);
            self.read_mft();
            shared.notify(true, true);
        }
        self.set_state(shared, VolumeState::Scanning { dirs: 0 });
        let mut last = Instant::now();
        let finished = self.walk(ROOT, self.root.clone(), |dirs| {
            if last.elapsed() >= PROGRESS_EVERY {
                last = Instant::now();
                self.set_state(shared, VolumeState::Scanning { dirs });
                shared.notify(true, false);
            }
        });
        if finished {
            let mut index = self.index.write();
            index.built_at = index::unix(std::time::SystemTime::now());
            index.journal = mark.map(|m| JournalMark { id: m.id, next_usn: m.next_usn });
        }
        finished
    }

    /// Имена тома из MFT; не вышло — ничего страшного, будет обычный обход.
    fn read_mft(&self) {
        let Ok(records) = volume::mft_records(&self.root) else { return };
        let records = records
            .into_iter()
            .map(|record| {
                let info = NodeInfo {
                    is_dir: record.is_dir,
                    size: 0,
                    modified: i64::MIN,
                    hidden: record.hidden,
                    system: record.system,
                };
                (record.id, record.parent, record.name, info)
            })
            .collect();
        let index =
            VolumeIndex::from_records(self.root.clone(), records, volume::MFT_ROOT, |path| {
                self.excluded(path)
            });
        if !self.stopped() {
            *self.index.write() = index;
        }
    }

    /// Досмотр по журналу USN. `false` — журналу доверять нельзя, нужен полный обход.
    fn catch_up(&self, shared: &Shared, mark: JournalMark) -> bool {
        let since = volume::Journal { id: mark.id, next_usn: mark.next_usn };
        let changes = match volume::changed_dirs(&self.root, since) {
            Ok(changes) if !changes.reset => changes,
            _ => return false,
        };
        let total = changes.dirs.len();
        let mut last = Instant::now();
        for (done, dir) in changes.dirs.iter().enumerate() {
            if self.stopped() {
                return true; // Положение журнала не сдвигается: в следующий раз досмотрим.
            }
            if last.elapsed() >= PROGRESS_EVERY {
                last = Instant::now();
                self.set_state(shared, VolumeState::CatchingUp { done, total });
            }
            self.refresh_dir(dir);
        }
        self.index.write().journal =
            Some(JournalMark { id: changes.journal.id, next_usn: changes.journal.next_usn });
        true
    }

    fn save(&self, shared: &Shared) {
        let path = shared.snapshot_path(&self.root, self.serial);
        let index = self.index.upgradable_read();
        let bytes = snapshot::write(&index);
        let written = std::fs::create_dir_all(&shared.store).is_ok() && {
            let temp = path.with_extension("tmp");
            std::fs::write(&temp, &bytes).is_ok() && std::fs::rename(&temp, &path).is_ok()
        };
        // Много удалённых узлов — заменить индекс ужатым из только что записанного снимка.
        // Читатели всё это время работают; ждут только писатели.
        if written
            && index.garbage() > 0.25
            && let Ok(compact) = snapshot::read(&bytes)
        {
            let mut index = parking_lot::RwLockUpgradableReadGuard::upgrade(index);
            *index = compact;
        }
    }
}

/// Корень тома (`C:\`), а не папка на нём: MFT описывает том целиком.
fn is_volume_root(root: &Path) -> bool {
    root.parent().is_none() && root.has_root()
}

/// Содержимое папки для индекса и признак «ссылка» у каждой записи.
type Children = (Vec<(String, NodeInfo)>, Vec<bool>);

fn read_children(dir: &Path) -> Option<Children> {
    let entries = std::fs::read_dir(dir).ok()?;
    let parent: Arc<Path> = Arc::from(dir);
    let (mut actual, mut links) = (Vec::new(), Vec::new());
    for entry in entries.flatten() {
        let Some(mut record) = read::from_dir_entry(&parent, &entry) else { continue };
        read::apply_dot_hidden(&mut record);
        let link = entry.file_type().is_ok_and(|t| t.is_symlink()) || record.attributes.reparse();
        actual.push((record.name.clone(), NodeInfo::of(&record)));
        links.push(link);
    }
    Some((actual, links))
}

/// Папки, которые нужно перечитать после событий наблюдателя. `None` — событий было
/// слишком много, нужен полный обход.
fn touched_dirs(events: Vec<WatchEvent>, dirs: &mut BTreeSet<PathBuf>) -> Option<()> {
    let mut parent_of = |path: &Path| {
        if let Some(parent) = path.parent() {
            dirs.insert(parent.to_path_buf());
        }
    };
    for event in events {
        match event {
            WatchEvent::Created(path) | WatchEvent::Modified(path) | WatchEvent::Removed(path) => {
                parent_of(&path)
            }
            WatchEvent::Renamed { from, to } => {
                parent_of(&from);
                parent_of(&to);
            }
            WatchEvent::Overflow => return None,
            WatchEvent::Stopped => {}
        }
    }
    Some(())
}

/// Жизнь тома: снимок → досмотр → наблюдение до остановки.
fn run(shared: &Arc<Shared>, volume: &Arc<Volume>, jobs: &Receiver<Job>, rescan_on_start: bool) {
    volume::background_thread();
    // 1. Снимок.
    let loaded = std::fs::read(shared.snapshot_path(&volume.root, volume.serial))
        .ok()
        .and_then(|bytes| snapshot::read(&bytes).ok())
        .filter(|index| index.root() == volume.root);
    let had_snapshot = loaded.as_ref().is_some_and(|index| index.built_at > 0);
    let mut pruned = false;
    if let Some(mut index) = loaded {
        pruned = volume.prune_excluded(&mut index);
        *volume.index.write() = index;
    }
    volume.set_state(shared, VolumeState::Loading);
    shared.notify(true, true);

    // 2. Наблюдатель — раньше досмотра, чтобы не потерять изменения, сделанные во время него.
    let sender = volume.jobs.clone();
    let watcher = Watcher::recursive(
        volume.root.clone(),
        Box::new(move |events| {
            let _ = sender.send(Job::Watch(events));
        }),
    );
    volume.status.lock().live = watcher.is_ok();

    // 3. Досмотр.
    let mark = volume.index.read().journal;
    // Убранное из снимка исключённое — переписать снимок.
    let mut dirty = pruned;
    let caught_up = match mark {
        Some(mark) if had_snapshot && shared.elevated => {
            volume.status.lock().journal = true;
            volume.catch_up(shared, mark)
        }
        _ => had_snapshot && !rescan_on_start,
    };
    if !caught_up {
        if volume.scan(shared) {
            volume.status.lock().journal = volume.index.read().journal.is_some();
            volume.save(shared);
        }
    } else {
        dirty |= mark.is_some();
    }
    if !volume.stopped() {
        volume.set_state(shared, VolumeState::Ready);
        shared.notify(true, true);
    }

    // 4. Наблюдение.
    let mut pending = BTreeSet::new();
    let mut deadline: Option<Instant> = None;
    let mut next_save = Instant::now() + SAVE_EVERY;
    loop {
        let wake = deadline.map_or(next_save, |d| d.min(next_save));
        let job = match jobs.recv_timeout(wake.saturating_duration_since(Instant::now())) {
            Ok(job) => Some(job),
            Err(RecvTimeoutError::Timeout) => None,
            Err(RecvTimeoutError::Disconnected) => return,
        };
        let mut changed = false;
        match job {
            Some(Job::Watch(events)) => {
                // Наблюдатель остановился, а корня больше нет — диск отключили: результаты
                // с него не показывать, тома пересобрать сейчас, а не при следующем опросе.
                if events.iter().any(|e| matches!(e, WatchEvent::Stopped))
                    && std::fs::metadata(&volume.root).is_err()
                {
                    volume.status.lock().live = false;
                    volume.set_state(shared, VolumeState::Failed("диск отключён".into()));
                    shared.notify(true, true);
                    request_sync(shared);
                }
                if touched_dirs(events, &mut pending).is_none() {
                    pending.clear();
                    let _ = volume.jobs.send(Job::Rescan);
                }
                deadline.get_or_insert_with(|| Instant::now() + SETTLE);
            }
            Some(Job::Dirs(dirs)) => {
                for dir in dirs {
                    volume.refresh_dir(&dir);
                }
                changed = true;
            }
            Some(Job::Seen { dir, actual, links }) => {
                let (actual, links) = volume.without_excluded(&dir, (actual, links));
                volume.apply(&dir, &actual, &links);
                changed = true;
            }
            Some(Job::Rescan) => {
                pending.clear();
                deadline = None;
                volume.scan(shared);
                volume.set_state(shared, VolumeState::Ready);
                changed = true;
            }
            Some(Job::Stop(done)) => {
                drop(watcher);
                // Недостроенный индекс тоже сохраняется: следующий запуск начнёт с него.
                if dirty || volume.index.read().built_at == 0 {
                    volume.save(shared);
                }
                let _ = done.send(());
                return;
            }
            None => {}
        }
        if deadline.is_some_and(|d| Instant::now() >= d) {
            deadline = None;
            for dir in std::mem::take(&mut pending) {
                volume.refresh_dir(&dir);
            }
            changed = true;
        }
        if changed {
            dirty = true;
            volume.status.lock().entries = volume.index.read().len();
            shared.notify(true, true);
        }
        if Instant::now() >= next_save {
            next_save = Instant::now() + SAVE_EVERY;
            if dirty {
                volume.save(shared);
                dirty = false;
            }
        }
    }
}

fn search(
    volumes: &[Arc<Volume>],
    text: &str,
    show_hidden: bool,
    cancel: &CancelToken,
) -> Result<IndexResults, String> {
    let started = Instant::now();
    let now = index::unix(std::time::SystemTime::now());
    let mut query = index::query::parse(text, now)?;
    query.hidden |= show_hidden;
    if query.is_empty() {
        return Ok(IndexResults { entries: Vec::new(), total: 0, elapsed: started.elapsed() });
    }
    if volumes.is_empty() {
        return Err("индекс выключен или ещё не запущен".into());
    }
    let cancelled = || cancel.is_cancelled();
    let mut found: Vec<(i32, Entry)> = Vec::new();
    let mut total = 0;
    // Отключённый диск: его записи — уже не файлы.
    let live = volumes.iter().filter(|v| !matches!(v.status.lock().state, VolumeState::Failed(_)));
    for volume in live {
        let index = volume.index.read();
        let hits = index.search(&query, RESULT_LIMIT, &cancelled);
        if cancel.is_cancelled() {
            return Ok(IndexResults { entries: Vec::new(), total: 0, elapsed: started.elapsed() });
        }
        total += hits.total;
        let mut parents = std::collections::HashMap::new();
        found.extend(
            hits.hits.into_iter().map(|(node, score)| (score, index.entry(node, &mut parents))),
        );
    }
    if volumes.len() > 1 {
        found.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.name.cmp(&b.1.name)));
        found.truncate(RESULT_LIMIT);
    }
    let entries = found.into_iter().map(|(_, entry)| entry).collect();
    Ok(IndexResults { entries, total, elapsed: started.elapsed() })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("mh-files-index-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn volume(root: &Path, exclude: Vec<PathBuf>) -> Volume {
        let (tx, _rx) = unbounded();
        Volume {
            root: root.to_path_buf(),
            index: RwLock::new(VolumeIndex::new(root.to_path_buf())),
            status: Mutex::new(VolumeStatus {
                root: root.to_path_buf(),
                state: VolumeState::Loading,
                entries: 0,
                live: false,
                journal: false,
            }),
            jobs: tx,
            stop: AtomicBool::new(false),
            exclude,
            serial: None,
        }
    }

    fn names(vol: &Arc<Volume>, text: &str) -> Vec<String> {
        let volumes = std::slice::from_ref(vol);
        let mut names: Vec<String> = search(volumes, text, true, &CancelToken::default())
            .unwrap()
            .entries
            .into_iter()
            .map(|e| e.name)
            .collect();
        names.sort();
        names
    }

    #[test]
    fn walks_refreshes_and_excludes() {
        let root = temp("walk");
        std::fs::create_dir_all(root.join("a/b")).unwrap();
        std::fs::create_dir_all(root.join("skip/deep")).unwrap();
        std::fs::write(root.join("a/b/report.pdf"), "x").unwrap();
        std::fs::write(root.join("a/notes.txt"), "x").unwrap();
        std::fs::write(root.join("skip/deep/report.doc"), "x").unwrap();
        let vol = Arc::new(volume(&root, vec![lower(&root.join("skip"))]));
        assert!(vol.walk(ROOT, root.clone(), |_| {}));
        assert_eq!(names(&vol, "report"), ["report.pdf"]);
        assert_eq!(names(&vol, "ext:txt"), ["notes.txt"]);

        // Новая папка с файлами и удалённый файл — после перечитывания родителя.
        std::fs::create_dir_all(root.join("a/new/inner")).unwrap();
        std::fs::write(root.join("a/new/inner/report.md"), "x").unwrap();
        std::fs::remove_file(root.join("a/notes.txt")).unwrap();
        vol.refresh_dir(&root.join("a"));
        assert_eq!(names(&vol, "report"), ["report.md", "report.pdf"]);
        assert!(names(&vol, "notes").is_empty());

        // Папку убрали целиком.
        std::fs::remove_dir_all(root.join("a/b")).unwrap();
        vol.refresh_dir(&root.join("a/b"));
        assert_eq!(names(&vol, "report"), ["report.md"]);
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn exclusion_added_later_drops_indexed_folder() {
        let root = temp("exclude-later");
        std::fs::create_dir_all(root.join("Skip/deep")).unwrap();
        std::fs::create_dir_all(root.join("keep")).unwrap();
        std::fs::write(root.join("Skip/deep/report.doc"), "x").unwrap();
        std::fs::write(root.join("keep/report.pdf"), "x").unwrap();
        let vol = Arc::new(volume(&root, Vec::new()));
        assert!(vol.walk(ROOT, root.clone(), |_| {}));
        assert_eq!(names(&vol, "report"), ["report.doc", "report.pdf"]);

        // Исключили после постройки — снимок чистится при загрузке; с хвостовой чертой, а в
        // Windows — в любом регистре.
        let name = if cfg!(windows) { "skip" } else { "Skip" };
        let key = exclude_key(&PathBuf::from(format!("{}/", root.join(name).display())));
        let excluded = Arc::new(volume(&root, vec![key]));
        let mut index = std::mem::replace(&mut *vol.index.write(), VolumeIndex::new(root.clone()));
        assert!(excluded.prune_excluded(&mut index));
        *excluded.index.write() = index;
        assert_eq!(names(&excluded, "report"), ["report.pdf"]);
        let index = std::mem::replace(&mut *excluded.index.write(), VolumeIndex::new(root.clone()));
        *excluded.index.write() = index;
        // И обходом: исключённая папка уходит из индекса вместе с содержимым.
        assert!(excluded.walk(ROOT, root.clone(), |_| {}));
        assert_eq!(names(&excluded, "report"), ["report.pdf"]);
        assert!(names(&excluded, "skip").is_empty());
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn exclude_key_strips_quotes_and_verbatim_prefix() {
        if cfg!(windows) {
            assert_eq!(exclude_key(Path::new(r#""C:\Windows\""#)), PathBuf::from(r"c:\windows"));
            assert_eq!(exclude_key(Path::new(r"\\?\C:\Windows")), PathBuf::from(r"c:\windows"));
        } else {
            assert_eq!(exclude_key(Path::new("/data/skip/")), PathBuf::from("/data/skip"));
        }
    }

    #[test]
    fn roots_drop_nested() {
        let settings = IndexSettings {
            roots: vec![PathBuf::from("/data/projects"), PathBuf::from("/data"), "/other".into()],
            ..IndexSettings::default()
        };
        assert_eq!(roots(&settings), [PathBuf::from("/data"), PathBuf::from("/other")]);
    }

    #[test]
    fn service_builds_saves_and_searches() {
        let root = temp("service");
        let store = temp("service-store");
        std::fs::create_dir_all(root.join("music")).unwrap();
        std::fs::write(root.join("music/song.flac"), "x").unwrap();
        let (workers, events) = Workers::new(Arc::new(|| {}));
        let indexer = Indexer::new(workers, store.clone());
        let settings = IndexSettings { roots: vec![root.clone()], ..IndexSettings::default() };
        indexer.configure(&settings);
        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline {
            let status = indexer.status();
            if !status.volumes.is_empty() && !status.busy() {
                break;
            }
            let _ = events.recv_timeout(Duration::from_millis(50));
        }
        let ticket = Ticket { owner: 1, generation: 1 };
        indexer.search(ticket, "song".into(), false);
        let found = loop {
            match events.recv_timeout(Duration::from_secs(5)).unwrap() {
                Event::IndexResults { result, .. } => break result.unwrap(),
                _ => continue,
            }
        };
        assert_eq!(found.entries.len(), 1);
        assert_eq!(found.entries[0].path(), root.join("music/song.flac"));
        indexer.shutdown(Duration::from_secs(5));
        let saved = std::fs::read_dir(&store).unwrap().count();
        assert_eq!(saved, 1, "снимок записан");
        std::fs::remove_dir_all(&root).unwrap();
        std::fs::remove_dir_all(&store).unwrap();
    }
}
