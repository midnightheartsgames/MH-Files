//! Служба поиска по смыслу.
//!
//! Один поток-хозяин делает долгое по очереди: скачивает модель (только когда поиск по смыслу
//! включён), запускает `llama-server`, обходит выбранные папки и считает векторы новых и
//! изменённых файлов. Индекс сохраняется по ходу и при выходе; при следующем запуске
//! пересчитывается только изменённое. Поиск идёт в своём потоке и сам поднимает сервер, если
//! тот выгружен: после пяти минут без дела модель уходит из памяти.
//!
//! Модель — EmbeddingGemma 2 (текст, картинки, звук в одном пространстве), подробности — в
//! `mh_files_core::semantic`.

pub mod extract;
pub mod server;
pub mod setup;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, RecvTimeoutError, Sender, unbounded};
use mh_files_core::Entry;
use mh_files_core::category::{self, Category};
use mh_files_core::semantic::model::{self as model_files, Input, ServerConfig};
use mh_files_core::semantic::store::{Doc, Part, Store};
use mh_files_core::semantic::{self as semantic_model};
use mh_files_core::settings::SemanticSettings;
use parking_lot::{Mutex, RwLock};

use self::extract::Item;
use self::server::{EmbedError, Server};
use crate::{CancelToken, Event, Ticket, Workers};

/// Найденного показывается не больше: дальше сходство уже случайное.
pub const RESULT_LIMIT: usize = 300;
/// Модель выгружается из памяти после стольких минут без запросов.
const IDLE_UNLOAD: Duration = Duration::from_secs(5 * 60);
/// Папки пересматриваются так часто, пока программа открыта.
const RESCAN_EVERY: Duration = Duration::from_secs(30 * 60);
/// Индекс сохраняется во время долгого прохода не реже.
const SAVE_EVERY: Duration = Duration::from_secs(2 * 60);
const PROGRESS_EVERY: Duration = Duration::from_millis(250);
/// Больше файлов не индексируется: это уже не «мои папки», а весь диск.
const MAX_FILES: usize = 300_000;
/// Входов в одном запросе к модели (имена и куски текста).
const BATCH_INPUTS: usize = 32;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SemanticState {
    /// Выключен в настройках.
    Off,
    /// Скачивается модель или llama.cpp.
    Downloading {
        what: String,
        done: u64,
        total: u64,
    },
    /// Модель загружается в память.
    Starting,
    /// Обход папок: сколько файлов найдено.
    Scanning {
        files: usize,
    },
    /// Векторы новых и изменённых файлов.
    Indexing {
        done: usize,
        total: usize,
    },
    Ready,
    Failed(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SemanticStatus {
    pub state: SemanticState,
    /// Файлов в индексе и векторов.
    pub files: usize,
    pub parts: usize,
    /// Файл модели.
    pub model: Option<String>,
    /// Модель в памяти (работает `llama-server`).
    pub loaded: bool,
    /// Видеокарта не подошла — считается на процессоре.
    pub gpu_fallback: bool,
    /// Кодировщик картинок и звука не загрузился — понимается только текст и имена.
    pub media_fallback: bool,
    /// Сколько файлов в последнем проходе прочитать не вышло.
    pub skipped: usize,
}

impl Default for SemanticStatus {
    fn default() -> SemanticStatus {
        SemanticStatus {
            state: SemanticState::Off,
            files: 0,
            parts: 0,
            model: None,
            loaded: false,
            gpu_fallback: false,
            media_fallback: false,
            skipped: 0,
        }
    }
}

impl SemanticStatus {
    /// Идёт подготовка или индексация.
    pub fn busy(&self) -> bool {
        matches!(
            self.state,
            SemanticState::Downloading { .. }
                | SemanticState::Starting
                | SemanticState::Scanning { .. }
                | SemanticState::Indexing { .. }
        )
    }

    /// Состояние одной фразой.
    pub fn describe(&self) -> String {
        use mh_files_core::format;
        match &self.state {
            SemanticState::Off => "выключен".into(),
            SemanticState::Downloading { what, done, total } if *total > 0 => format!(
                "скачивается {what}: {} из {} ({}%)",
                format::size(*done),
                format::size(*total),
                done * 100 / total.max(&1)
            ),
            SemanticState::Downloading { what, done, .. } => {
                format!("скачивается {what}: {}", format::size(*done))
            }
            SemanticState::Starting => "загрузка модели…".into(),
            SemanticState::Scanning { files } => {
                format!("обход папок: {}", format::items(*files))
            }
            SemanticState::Indexing { done, total } => {
                format!("индексация: {} из {}", format::count(*done), format::count(*total))
            }
            SemanticState::Ready => "готов".into(),
            SemanticState::Failed(error) => format!("ошибка: {error}"),
        }
    }
}

/// Итог поиска по смыслу.
#[derive(Debug)]
pub struct SemanticResults {
    /// Лучшие первыми.
    pub entries: Vec<Entry>,
    /// Почему найдено: близость и подпись («по изображению», отрывок текста).
    pub reasons: HashMap<PathBuf, (f32, String)>,
    pub elapsed: Duration,
}

enum Job {
    /// Настройки сменились (или первый запуск).
    Configure,
    Rescan,
    Stop(Sender<()>),
}

struct Shared {
    workers: Workers,
    /// Папка модели, llama.cpp и индекса.
    dir: PathBuf,
    settings: Mutex<Option<SemanticSettings>>,
    status: Mutex<SemanticStatus>,
    store: RwLock<Store>,
    /// Работающий сервер; запуск — под этим же замком, так что второй ждёт первого.
    server: Mutex<Option<Server>>,
    paths: Mutex<Option<setup::Paths>>,
    jobs: Sender<Job>,
    /// Растёт при каждой смене настроек: скачивание по старым настройкам прерывается.
    generation: AtomicU64,
    last_used: Mutex<Instant>,
    last_notice: Mutex<Instant>,
    /// Программа закрывается: сервер больше не запускать.
    stopping: std::sync::atomic::AtomicBool,
}

/// Служба поиска по смыслу. Дешёвая в клонировании.
#[derive(Clone)]
pub struct Semantic {
    shared: Arc<Shared>,
}

impl Semantic {
    /// `dir` — папка данных поиска по смыслу. Служба ничего не делает до
    /// [`Semantic::configure`] с включённым поиском.
    pub fn new(workers: Workers, dir: PathBuf) -> Semantic {
        let (jobs, receiver) = unbounded();
        let shared = Arc::new(Shared {
            workers,
            dir,
            settings: Mutex::new(None),
            status: Mutex::new(SemanticStatus::default()),
            store: RwLock::new(Store::default()),
            server: Mutex::new(None),
            paths: Mutex::new(None),
            jobs,
            generation: AtomicU64::new(0),
            last_used: Mutex::new(Instant::now()),
            last_notice: Mutex::new(Instant::now()),
            stopping: std::sync::atomic::AtomicBool::new(false),
        });
        let worker = shared.clone();
        shared.workers.spawn("semantic", move |_| run(&worker, &receiver));
        Semantic { shared }
    }

    /// Применить настройки: что-то поменялось — служба перестраивается в фоне.
    pub fn configure(&self, settings: &SemanticSettings) {
        {
            let mut current = self.shared.settings.lock();
            if current.as_ref() == Some(settings) {
                return;
            }
            *current = Some(settings.clone());
        }
        self.shared.generation.fetch_add(1, Ordering::SeqCst);
        let _ = self.shared.jobs.send(Job::Configure);
    }

    pub fn status(&self) -> SemanticStatus {
        self.shared.status.lock().clone()
    }

    /// Пересмотреть папки сейчас.
    pub fn rescan(&self) {
        let _ = self.shared.jobs.send(Job::Rescan);
    }

    /// Найти по смыслу. В тексте может быть категория `kind:`.
    pub fn search(&self, ticket: Ticket, text: String) -> CancelToken {
        let cancel = CancelToken::default();
        let token = cancel.clone();
        let shared = self.shared.clone();
        self.shared.workers.spawn("semantic-search", move |workers| {
            let result = search(&shared, &text, &token);
            if !token.is_cancelled() {
                workers.send(Event::SemanticResults { ticket, result });
            }
        });
        cancel
    }

    /// Сохранить индекс и выгрузить модель; ждать не дольше `wait`.
    pub fn shutdown(&self, wait: Duration) {
        self.shared.stopping.store(true, Ordering::SeqCst);
        self.shared.generation.fetch_add(1, Ordering::SeqCst);
        let (reply, done) = unbounded();
        if self.shared.jobs.send(Job::Stop(reply)).is_ok() {
            // Запрос к модели может идти минуты (картинки на процессоре): сервер — долой,
            // запрос оборвётся, и хозяин сохранит индекс.
            if let Some(mut server) = self.shared.server.try_lock_for(Duration::from_millis(200)) {
                *server = None;
            }
            let _ = done.recv_timeout(wait);
        }
        *self.shared.server.lock() = None;
    }
}

/// Папки по умолчанию: Рабочий стол, Документы, Загрузки, Изображения, Видео, Музыка.
pub fn default_folders() -> Vec<PathBuf> {
    use mh_files_platform::folders::KnownFolder;
    let _com = mh_files_platform::thumbs::init_worker_thread();
    mh_files_platform::folders::known_folders()
        .into_iter()
        .filter(|(folder, _)| *folder != KnownFolder::Home)
        .map(|(_, path)| path)
        .collect()
}

impl Shared {
    fn set_state(&self, state: SemanticState) {
        let mut status = self.status.lock();
        if status.state != state {
            status.state = state;
            drop(status);
            self.notify(false, true);
        }
    }

    fn update(&self, change: impl FnOnce(&mut SemanticStatus)) {
        change(&mut self.status.lock());
        self.notify(false, false);
    }

    /// Сообщить UI: ход — не чаще [`PROGRESS_EVERY`], смена состояния — сразу.
    fn notify(&self, content: bool, force: bool) {
        let mut last = self.last_notice.lock();
        if !force && !content && last.elapsed() < PROGRESS_EVERY {
            return;
        }
        *last = Instant::now();
        drop(last);
        self.workers.send(Event::SemanticChanged { content });
    }

    fn settings(&self) -> Option<SemanticSettings> {
        self.settings.lock().clone()
    }

    fn index_path(&self) -> PathBuf {
        self.dir.join("index.bin")
    }

    fn save(&self) {
        let bytes = self.store.read().write();
        let path = self.index_path();
        let temp = path.with_extension("bin.tmp");
        if std::fs::create_dir_all(&self.dir).is_ok() && std::fs::write(&temp, bytes).is_ok() {
            let _ = std::fs::rename(&temp, &path);
        }
    }

    /// Порт работающего сервера; не работает — запустить (модель грузится секунды).
    fn ensure_server(&self) -> Result<u16, String> {
        if self.stopping.load(Ordering::SeqCst) {
            return Err("программа закрывается".into());
        }
        *self.last_used.lock() = Instant::now();
        let mut guard = self.server.lock();
        if let Some(server) = guard.as_mut() {
            if server.alive() {
                return Ok(server.port);
            }
            *guard = None;
        }
        let paths = self.paths.lock().clone().ok_or("модель ещё не готова")?;
        let settings = self.settings().unwrap_or_default();
        let threads = std::thread::available_parallelism().map_or(2, |n| n.get());
        let config = ServerConfig {
            model: paths.model.clone(),
            mmproj: paths.mmproj.clone(),
            port: 0,
            gpu: settings.gpu,
            // Половина ядер: индексация в фоне не должна отнимать компьютер.
            threads: (threads / 2).max(1),
        };
        let log = self.dir.join("llama-server.log");
        let server = Server::start(&paths.exe, &config, &log)?;
        let port = server.port;
        let (gpu, media) = (server.gpu_fallback, server.media_fallback);
        *guard = Some(server);
        drop(guard);
        self.update(|s| {
            s.loaded = true;
            s.gpu_fallback = gpu;
            s.media_fallback = media;
        });
        Ok(port)
    }

    /// Выгрузить модель.
    fn unload(&self) {
        if self.server.lock().take().is_some() {
            self.update(|s| s.loaded = false);
        }
    }

    /// Векторы для входов; упавший сервер перезапускается один раз.
    fn embed(&self, inputs: &[Input]) -> Result<Vec<Vec<f32>>, EmbedError> {
        let port = self.ensure_server().map_err(EmbedError::Unreachable)?;
        match server::embed(port, inputs) {
            Err(EmbedError::Unreachable(_)) => {
                {
                    let mut guard = self.server.lock();
                    if guard.as_ref().is_some_and(|s| s.port == port) {
                        *guard = None;
                    }
                }
                let port = self.ensure_server().map_err(EmbedError::Unreachable)?;
                server::embed(port, inputs)
            }
            other => other,
        }
    }
}

fn run(shared: &Arc<Shared>, jobs: &Receiver<Job>) {
    let mut last_scan: Option<Instant> = None;
    let mut pending: Option<Job> = None;
    let mut loaded_model: Option<String> = None;
    loop {
        let job = match pending.take() {
            Some(job) => Ok(job),
            None => jobs.recv_timeout(Duration::from_secs(20)),
        };
        match job {
            Ok(Job::Stop(reply)) => {
                if loaded_model.is_some() {
                    shared.save();
                }
                shared.unload();
                let _ = reply.send(());
                return;
            }
            Ok(Job::Configure | Job::Rescan) => {
                // Несколько смен настроек подряд — одна перестройка.
                loop {
                    match jobs.try_recv() {
                        Ok(Job::Configure | Job::Rescan) => continue,
                        Ok(stop @ Job::Stop(_)) => pending = Some(stop),
                        Err(_) => {}
                    }
                    break;
                }
                if pending.is_some() {
                    continue;
                }
                let (interrupted, scanned) = activate(shared, jobs, &mut loaded_model);
                pending = interrupted;
                if scanned {
                    last_scan = Some(Instant::now());
                }
            }
            Err(RecvTimeoutError::Timeout) => {
                if shared.last_used.lock().elapsed() >= IDLE_UNLOAD {
                    shared.unload();
                }
                let ready = shared.status.lock().state == SemanticState::Ready;
                if ready && last_scan.is_none_or(|t| t.elapsed() >= RESCAN_EVERY) {
                    pending = Some(Job::Rescan);
                }
            }
            Err(RecvTimeoutError::Disconnected) => return,
        }
    }
}

/// Привести службу к настройкам и пройти папки. Возвращает задание, пришедшее во время
/// прохода (его выполнить следующим), и был ли проход закончен.
fn activate(
    shared: &Arc<Shared>,
    jobs: &Receiver<Job>,
    loaded_model: &mut Option<String>,
) -> (Option<Job>, bool) {
    let Some(settings) = shared.settings().filter(|s| s.enabled) else {
        shared.unload();
        if loaded_model.take().is_some() {
            shared.save();
            *shared.store.write() = Store::default();
        }
        *shared.paths.lock() = None;
        shared.update(|s| *s = SemanticStatus::default());
        shared.notify(true, true);
        return (None, false);
    };
    mh_files_platform::volume::background_thread();
    let generation = shared.generation.load(Ordering::SeqCst);
    let mut last = Instant::now() - PROGRESS_EVERY;
    let paths = setup::ensure(&shared.dir, &settings, &mut |what, done, total| {
        if last.elapsed() >= PROGRESS_EVERY {
            last = Instant::now();
            shared.set_state(SemanticState::Downloading { what: what.to_string(), done, total });
        }
        shared.generation.load(Ordering::SeqCst) == generation
    });
    let paths = match paths {
        Ok(paths) => paths,
        Err(_) if shared.generation.load(Ordering::SeqCst) != generation => return (None, false),
        Err(error) => {
            shared.set_state(SemanticState::Failed(error));
            return (None, false);
        }
    };
    if shared.paths.lock().as_ref() != Some(&paths) {
        shared.unload();
        *shared.paths.lock() = Some(paths.clone());
    }
    // Модель — сразу: не запускается, так и сказать, а не молча обходить папки. От того,
    // запустилась ли она с кодировщиком картинок и звука, зависит, что брать из файлов.
    shared.set_state(SemanticState::Starting);
    if let Err(error) = shared.ensure_server() {
        shared.set_state(SemanticState::Failed(error));
        return (None, false);
    }
    let media = paths.mmproj.is_some() && !shared.status.lock().media_fallback;
    // Векторы зависят от модели и от того, что из файлов берётся.
    let file =
        paths.model.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let flags: String = [settings.images, settings.documents, settings.audio, media]
        .iter()
        .map(|&on| if on { '1' } else { '0' })
        .collect();
    let id = format!("{}|{flags}", model_files::model_id(&file));
    if loaded_model.as_deref() != Some(id.as_str()) {
        let store = std::fs::read(shared.index_path())
            .ok()
            .and_then(|bytes| Store::read(&bytes).ok())
            .filter(|store| store.model == id)
            .unwrap_or_else(|| Store::new(&id));
        *shared.store.write() = store;
        *loaded_model = Some(id);
    }
    {
        let store = shared.store.read();
        let (files, parts) = (store.len(), store.parts());
        drop(store);
        shared.update(|s| {
            s.files = files;
            s.parts = parts;
            s.model = Some(file);
        });
    }
    shared.notify(true, true);
    let result = index(shared, &settings, media, jobs);
    shared.save();
    match result {
        Ok(None) => {
            shared.set_state(SemanticState::Ready);
            shared.notify(true, true);
            (None, true)
        }
        Ok(Some(job)) => (Some(job), false),
        Err(error) => {
            shared.set_state(SemanticState::Failed(error));
            (None, false)
        }
    }
}

/// Файл для индекса: путь, размер, время.
struct Found {
    path: PathBuf,
    size: u64,
    modified: i64,
}

/// Обойти папки и посчитать векторы изменённого. `Ok(Some(job))` — прервано заданием.
fn index(
    shared: &Arc<Shared>,
    settings: &SemanticSettings,
    media: bool,
    jobs: &Receiver<Job>,
) -> Result<Option<Job>, String> {
    let interrupted = || jobs.try_recv().ok();
    shared.set_state(SemanticState::Scanning { files: 0 });
    let folders =
        if settings.folders.is_empty() { default_folders() } else { settings.folders.clone() };
    let mut found = Vec::new();
    for folder in &folders {
        if let Some(job) = walk(shared, folder, &settings.exclude, &mut found, &interrupted) {
            return Ok(Some(job));
        }
    }
    // Удалённые файлы и убранные из настроек папки — из индекса.
    let alive: std::collections::HashSet<&Path> = found.iter().map(|f| f.path.as_path()).collect();
    let removed = shared.store.write().retain(|path| alive.contains(path));
    let changed: Vec<&Found> = {
        let store = shared.store.read();
        found.iter().filter(|f| !store.fresh(&f.path, f.size, f.modified)).collect()
    };
    if removed > 0 {
        shared.notify(true, true);
    }
    let total = changed.len();
    shared.set_state(SemanticState::Indexing { done: 0, total });
    let _com = mh_files_platform::thumbs::init_worker_thread();
    let mut batch: Vec<(PathBuf, u64, i64, Vec<Item>)> = Vec::new();
    let mut batch_inputs = 0;
    let mut skipped = 0;
    let mut last_save = Instant::now();
    for (done, file) in changed.iter().enumerate() {
        if let Some(job) = interrupted() {
            flush(shared, &mut batch, &mut skipped)?;
            return Ok(Some(job));
        }
        let items = extract::items(&file.path, file.size, settings, media);
        let media = items.iter().any(Item::is_media);
        if media {
            // Картинки и звук — отдельным запросом: они считаются дольше всего остального.
            flush(shared, &mut batch, &mut skipped)?;
            batch_inputs = 0;
            let mut one = vec![(file.path.clone(), file.size, file.modified, items)];
            flush(shared, &mut one, &mut skipped)?;
        } else {
            batch_inputs += items.len();
            batch.push((file.path.clone(), file.size, file.modified, items));
            if batch_inputs >= BATCH_INPUTS {
                flush(shared, &mut batch, &mut skipped)?;
                batch_inputs = 0;
            }
        }
        if last_save.elapsed() >= SAVE_EVERY {
            last_save = Instant::now();
            shared.save();
            shared.notify(true, true);
        }
        let (files, parts) = {
            let store = shared.store.read();
            (store.len(), store.parts())
        };
        shared.update(|s| {
            s.state = SemanticState::Indexing { done: done + 1, total };
            s.files = files;
            s.parts = parts;
            s.skipped = skipped;
        });
    }
    flush(shared, &mut batch, &mut skipped)?;
    let (files, parts) = {
        let store = shared.store.read();
        (store.len(), store.parts())
    };
    shared.update(|s| {
        s.files = files;
        s.parts = parts;
        s.skipped = skipped;
    });
    Ok(None)
}

/// Обход папки: файлы без скрытых, системных, исключённых и ссылок. `Some(job)` — прервано.
fn walk(
    shared: &Shared,
    root: &Path,
    exclude: &[String],
    found: &mut Vec<Found>,
    interrupted: &dyn Fn() -> Option<Job>,
) -> Option<Job> {
    let mut last = Instant::now();
    let walker = walkdir::WalkDir::new(root).follow_links(false).into_iter();
    let walker = walker.filter_entry(|entry| {
        entry.depth() == 0
            || !(mh_files_core::duplicates::excluded(entry.path(), exclude)
                || entry.file_name().to_string_lossy().starts_with('.')
                || crate::read::stat(entry.path())
                    .is_ok_and(|e| e.hidden() || e.attributes.system()))
    });
    for entry in walker.flatten() {
        if !entry.file_type().is_file() || found.len() >= MAX_FILES {
            continue;
        }
        let Ok(meta) = entry.metadata() else { continue };
        let modified = meta.modified().map_or(0, mh_files_core::index::unix);
        found.push(Found { path: entry.into_path(), size: meta.len(), modified });
        if last.elapsed() >= PROGRESS_EVERY {
            last = Instant::now();
            if let Some(job) = interrupted() {
                return Some(job);
            }
            shared.update(|s| s.state = SemanticState::Scanning { files: found.len() });
        }
    }
    None
}

/// Посчитать векторы накопленных файлов и положить в индекс. Ошибка — только если модель
/// недоступна; отказ модели на отдельный вход (битая картинка) — пропуск этого входа.
fn flush(
    shared: &Shared,
    batch: &mut Vec<(PathBuf, u64, i64, Vec<Item>)>,
    skipped: &mut usize,
) -> Result<(), String> {
    if batch.is_empty() {
        return Ok(());
    }
    let inputs: Vec<Input> =
        batch.iter().flat_map(|(.., items)| items.iter().map(|i| i.input.clone())).collect();
    let vectors: Vec<Option<Vec<f32>>> = match shared.embed(&inputs) {
        Ok(vectors) => vectors.into_iter().map(Some).collect(),
        Err(EmbedError::Unreachable(error)) => return Err(error),
        Err(EmbedError::Refused(_)) => {
            // По одному: так отказ одного входа не теряет остальные.
            let mut out = Vec::with_capacity(inputs.len());
            for input in &inputs {
                match shared.embed(std::slice::from_ref(input)) {
                    Ok(mut one) => out.push(one.pop()),
                    Err(EmbedError::Refused(_)) => out.push(None),
                    Err(EmbedError::Unreachable(error)) => return Err(error),
                }
            }
            out
        }
    };
    let mut vectors = vectors.into_iter();
    let mut store = shared.store.write();
    for (path, size, modified, items) in batch.drain(..) {
        let mut parts = Vec::with_capacity(items.len());
        let mut failed = false;
        for item in items {
            match vectors.next().flatten() {
                Some(vector) => {
                    parts.push(Part::new(item.kind, item.number, item.snippet, &vector))
                }
                None => failed = true,
            }
        }
        if failed {
            *skipped += 1;
        }
        // Файл без единого вектора тоже запоминается: повторять его на каждом проходе незачем.
        store.insert(path, Doc { size, modified, parts });
    }
    Ok(())
}

/// Поиск: вектор запроса, близкие файлы, их свежие записи с диска.
fn search(shared: &Shared, text: &str, cancel: &CancelToken) -> Result<SemanticResults, String> {
    let started = Instant::now();
    let category = category::kind_of(text);
    let query = category::with_kind(text, Category::All);
    let query = query.trim();
    if query.is_empty() {
        return Ok(SemanticResults {
            entries: Vec::new(),
            reasons: HashMap::new(),
            elapsed: started.elapsed(),
        });
    }
    let status = shared.status.lock().clone();
    match &status.state {
        SemanticState::Off => {
            return Err("Поиск по смыслу выключен — включите его в настройках".into());
        }
        SemanticState::Downloading { .. } => {
            return Err(format!("Модель ещё не готова: {}", status.describe()));
        }
        SemanticState::Failed(error) if shared.paths.lock().is_none() => {
            return Err(error.clone());
        }
        _ => {}
    }
    let vector = shared
        .embed(&[Input::Text(semantic_model::query_prompt(query))])
        .map_err(|e| e.to_string())?
        .pop()
        .ok_or("модель не вернула вектор запроса")?;
    if cancel.is_cancelled() {
        return Err("отменено".into());
    }
    let words: Vec<String> = query
        .to_lowercase()
        .split_whitespace()
        .filter(|w| w.chars().count() >= 2)
        .map(String::from)
        .collect();
    let hits = shared.store.read().search(&vector, &words, RESULT_LIMIT * 2, |path| {
        let name = path.file_name().map(|n| n.to_string_lossy()).unwrap_or_default();
        category.matches(&name, false)
    });
    let mut entries = Vec::new();
    let mut reasons = HashMap::new();
    for hit in hits {
        if entries.len() >= RESULT_LIMIT || cancel.is_cancelled() {
            break;
        }
        // Удалённое с прошлого прохода не показывается.
        let Ok(entry) = crate::read::stat(&hit.path) else { continue };
        reasons.insert(hit.path.clone(), (hit.score, hit.reason()));
        entries.push(entry);
    }
    Ok(SemanticResults { entries, reasons, elapsed: started.elapsed() })
}

#[cfg(test)]
mod tests {
    use mh_files_core::semantic::store::PartKind;

    use super::*;

    /// Служба с поддельным сервером: индексирует папку и находит по «смыслу» (первой букве).
    #[test]
    fn indexes_folders_and_finds_by_meaning() {
        let (port, calls) = server::tests::fake_server();
        let dir = tempfile::tempdir().unwrap();
        let data = dir.path().join("data");
        let docs = dir.path().join("docs");
        std::fs::create_dir_all(docs.join("node_modules")).unwrap();
        std::fs::create_dir_all(docs.join(".git")).unwrap();
        std::fs::write(docs.join("отчёт.txt"), "квартальный отчёт о продажах").unwrap();
        std::fs::write(docs.join("рецепт.md"), "борщ со сметаной").unwrap();
        std::fs::write(docs.join("node_modules/лишнее.txt"), "квартальный").unwrap();
        std::fs::write(docs.join(".git/config"), "квартальный").unwrap();
        let (workers, events) = Workers::new(mh_files_platform::no_waker());
        let semantic = Semantic::new(workers, data.clone());
        // Сервер уже «запущен»: подставляем его вместо llama-server.
        let settings = SemanticSettings {
            enabled: true,
            folders: vec![docs.clone()],
            images: false,
            audio: false,
            ..SemanticSettings::default()
        };
        let fake = FakeRuntime::new(&data, &settings, port);
        semantic.configure(&settings);
        let status = wait(&semantic, |s| {
            s.state == SemanticState::Ready || matches!(s.state, SemanticState::Failed(_))
        });
        assert_eq!(status.state, SemanticState::Ready, "{status:?}");
        assert_eq!(status.files, 2, "исключённые и скрытые папки не индексируются");
        assert_eq!(status.parts, 4, "имя и текст у каждого");
        assert!(calls.load(Ordering::SeqCst) >= 1);
        // «квартальный» начинается с той же буквы, что текст отчёта.
        semantic.search(Ticket { owner: 1, generation: 1 }, "квартал kind:документы".into());
        let results = loop {
            match events.recv_timeout(Duration::from_secs(10)).unwrap() {
                Event::SemanticResults { result, .. } => break result.unwrap(),
                _ => continue,
            }
        };
        assert_eq!(results.entries[0].name, "отчёт.txt", "{:?}", results.reasons);
        let (_, reason) = &results.reasons[&docs.join("отчёт.txt")];
        assert!(reason.contains("квартальный"), "{reason}");
        // Второй проход ничего не пересчитывает; удалённый файл уходит из индекса.
        let before = calls.load(Ordering::SeqCst);
        std::fs::remove_file(docs.join("рецепт.md")).unwrap();
        semantic.rescan();
        std::thread::sleep(Duration::from_millis(300));
        let status = wait(&semantic, |s| s.state == SemanticState::Ready && s.files == 1);
        assert_eq!(status.parts, 2);
        assert_eq!(calls.load(Ordering::SeqCst), before, "неизменное не пересчитывается");
        semantic.shutdown(Duration::from_secs(5));
        // Индекс сохранён и читается.
        let store = Store::read(&std::fs::read(data.join("index.bin")).unwrap()).unwrap();
        assert_eq!(store.len(), 1);
        let doc = store.get(&docs.join("отчёт.txt")).unwrap();
        assert_eq!(doc.parts[0].kind, PartKind::Name);
        drop(fake);
    }

    #[test]
    fn disabled_search_explains_itself() {
        let (workers, events) = Workers::new(mh_files_platform::no_waker());
        let dir = tempfile::tempdir().unwrap();
        let semantic = Semantic::new(workers, dir.path().to_path_buf());
        semantic.configure(&SemanticSettings::default());
        semantic.search(Ticket { owner: 1, generation: 1 }, "кошка".into());
        let error = loop {
            match events.recv_timeout(Duration::from_secs(10)).unwrap() {
                Event::SemanticResults { result, .. } => break result.unwrap_err(),
                _ => continue,
            }
        };
        assert!(error.contains("выключен"), "{error}");
        semantic.shutdown(Duration::from_secs(5));
    }

    fn wait(semantic: &Semantic, done: impl Fn(&SemanticStatus) -> bool) -> SemanticStatus {
        let started = Instant::now();
        loop {
            let status = semantic.status();
            if done(&status) || started.elapsed() > Duration::from_secs(20) {
                return status;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    /// «Установленная» модель и llama-server-заглушка, которая только ждёт: настоящий сервер
    /// — поддельный, на `port`, его подставляет переменная окружения в `Server::start`.
    struct FakeRuntime;

    impl FakeRuntime {
        fn new(data: &Path, settings: &SemanticSettings, port: u16) -> FakeRuntime {
            std::fs::create_dir_all(data.join("models")).unwrap();
            std::fs::write(data.join("models/m.gguf"), "m").unwrap();
            let installed = setup::Installed {
                repo: settings.model_repo.clone(),
                quant: settings.quant.clone(),
                model: "m.gguf".into(),
                mmproj: None,
                runtime: Some("fake".into()),
            };
            std::fs::write(data.join("installed.json"), serde_json::to_string(&installed).unwrap())
                .unwrap();
            let exe = installed.runtime_exe(data).unwrap();
            std::fs::create_dir_all(exe.parent().unwrap()).unwrap();
            std::fs::write(&exe, "").unwrap();
            server::set_test_port(Some(port));
            FakeRuntime
        }
    }

    impl Drop for FakeRuntime {
        fn drop(&mut self) {
            server::set_test_port(None);
        }
    }
}
