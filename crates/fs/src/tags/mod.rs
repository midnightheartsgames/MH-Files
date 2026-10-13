//! Служба автотегов (1.3).
//!
//! Поток-хозяин по очереди: скачивает модель и ONNX Runtime (только когда автотеги включены),
//! запускает помощника с моделью ([`host`]) и считает теги файлов, которые нужны интерфейсу:
//! сначала видимых в списке и в Инспекторе, потом остальных картинок и видео открытой
//! папки. Теги запоминаются по пути, размеру и времени изменения (`tags.bin`): второй раз
//! файл не смотрится, пока не изменится. После пяти минут без дела модель уходит из памяти.

pub mod host;
pub mod picture;
pub mod setup;

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, RecvTimeoutError, Sender, TryRecvError, unbounded};
use mh_files_core::entry::extension_of;
use mh_files_core::settings::TagSettings;
use mh_files_core::tags::{self as model, FileTags, Record, Shown, TagStore, Vocabulary};
use parking_lot::{Mutex, RwLock};

pub use self::host::{HOST_FLAG, Host, HostCommand, RunError, Spec};
use crate::{Event, Workers};

/// Модель выгружается из памяти после стольких минут без работы.
const IDLE_UNLOAD: Duration = Duration::from_secs(5 * 60);
/// Теги сохраняются во время долгой работы не реже.
const SAVE_EVERY: Duration = Duration::from_secs(2 * 60);
/// Очередь опустела — сохранить, если с прошлого раза прошло столько.
const SAVE_IDLE: Duration = Duration::from_secs(20);
const PROGRESS_EVERY: Duration = Duration::from_millis(250);
/// Готовых строк для списка больше не держится: кэш сбрасывается.
const MAX_VIEWS: usize = 20_000;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TagState {
    /// Выключены в настройках.
    Off,
    /// Скачивается модель или ONNX Runtime.
    Downloading {
        what: String,
        done: u64,
        total: u64,
    },
    Ready,
    Failed(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TagStatus {
    pub state: TagState,
    /// Файлов с посчитанными тегами.
    pub tagged: usize,
    /// Ждут очереди.
    pub queued: usize,
    /// Модель в памяти (работает помощник).
    pub loaded: bool,
    pub model: Option<String>,
    /// Сколько ушло на последний файл.
    pub last: Option<Duration>,
    /// Файлов, от которых модель отказалась.
    pub skipped: usize,
    /// Модель считает на видеокарте.
    pub gpu: bool,
    /// Видеокарту просили, но она не подошла — считается на процессоре.
    pub gpu_fallback: bool,
}

impl Default for TagStatus {
    fn default() -> TagStatus {
        TagStatus {
            state: TagState::Off,
            tagged: 0,
            queued: 0,
            loaded: false,
            model: None,
            last: None,
            skipped: 0,
            gpu: false,
            gpu_fallback: false,
        }
    }
}

impl TagStatus {
    /// Скачивается или считает.
    pub fn busy(&self) -> bool {
        matches!(self.state, TagState::Downloading { .. })
            || (self.state == TagState::Ready && self.queued > 0)
    }

    /// Состояние одной фразой.
    pub fn describe(&self) -> String {
        use mh_files_core::format;
        match &self.state {
            TagState::Off => "выключены".into(),
            TagState::Downloading { what, done, total } if *total > 0 => format!(
                "скачивается {what}: {} из {} ({}%)",
                format::size(*done),
                format::size(*total),
                done * 100 / total.max(&1)
            ),
            TagState::Downloading { what, done, .. } => {
                format!("скачивается {what}: {}", format::size(*done))
            }
            TagState::Ready if self.queued > 0 => {
                format!("определяются: в очереди {}", format::count(self.queued))
            }
            TagState::Ready => "готовы".into(),
            TagState::Failed(error) => format!("ошибка: {error}"),
        }
    }
}

/// Файл, теги которого нужны: путь, размер, время изменения (секунды Unix).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Want {
    pub path: PathBuf,
    pub size: u64,
    pub modified: i64,
}

impl Want {
    pub fn of(entry: &mh_files_core::Entry) -> Want {
        Want {
            path: entry.path(),
            size: entry.size,
            modified: entry.modified.map_or(0, mh_files_core::index::unix),
        }
    }
}

/// Что показать у файла.
#[derive(Debug, Clone, PartialEq)]
pub enum Lookup {
    /// Тегов нет и не будет: не картинка и не видео, слишком большой или автотеги выключены.
    None,
    /// Скоро будут.
    Pending,
    Ready(Arc<Shown>),
}

/// Модель, которая считает теги: в программе — помощник, в тестах — подделка.
pub trait Engine: Send {
    fn spec(&self) -> Spec;
    fn run(&mut self, input: &[f32]) -> Result<Vec<f32>, RunError>;
    fn alive(&mut self) -> bool {
        true
    }
}

impl Engine for Host {
    fn spec(&self) -> Spec {
        Host::spec(self)
    }

    fn run(&mut self, input: &[f32]) -> Result<Vec<f32>, RunError> {
        Host::run(self, input)
    }

    fn alive(&mut self) -> bool {
        Host::alive(self)
    }
}

/// Как запустить модель: пути файлов, журнал и просить ли видеокарту.
pub type Launcher =
    Arc<dyn Fn(&setup::Paths, &Path, bool) -> Result<Box<dyn Engine>, String> + Send + Sync>;

/// Помощник — этот же exe с [`HOST_FLAG`].
pub fn host_launcher(command: HostCommand) -> Launcher {
    Arc::new(move |paths, log, gpu| {
        // Все ядра, кроме одного: процесс и так с пониженным приоритетом.
        let threads = std::thread::available_parallelism().map_or(2, |n| n.get());
        let threads = threads.saturating_sub(1).max(1);
        Host::start(&command, &paths.runtime, &paths.model, threads, gpu, log)
            .map(|host| Box::new(host) as Box<dyn Engine>)
    })
}

enum Job {
    /// Настройки сменились (или первый запуск).
    Configure,
    /// В очереди новое.
    Wake,
    /// Забыть все теги и посчитать заново.
    Forget,
    Stop(Sender<()>),
}

/// Теги для показа и размер с временем файла, для которого они готовы.
type View = (u64, i64, Arc<Shown>);

#[derive(Default)]
struct Queue {
    /// Видимое на экране — в порядке, в каком его спросили.
    visible: VecDeque<Want>,
    /// Остальное из открытой папки.
    background: VecDeque<Want>,
}

impl Queue {
    fn len(&self) -> usize {
        self.visible.len() + self.background.len()
    }
}

struct Shared {
    workers: Workers,
    /// Папка модели, ONNX Runtime и тегов.
    dir: PathBuf,
    launcher: Launcher,
    settings: Mutex<Option<TagSettings>>,
    status: Mutex<TagStatus>,
    store: RwLock<TagStore>,
    vocabulary: RwLock<Option<Arc<Vocabulary>>>,
    paths: Mutex<Option<setup::Paths>>,
    /// Готовое для показа: путь → размер, время, теги.
    views: Mutex<HashMap<PathBuf, View>>,
    queue: Mutex<Queue>,
    /// Что спросил интерфейс в этом кадре и в прошлом.
    frame: Mutex<(Vec<Want>, Vec<Want>)>,
    engine: Mutex<Option<Box<dyn Engine>>>,
    jobs: Sender<Job>,
    /// Растёт при каждой смене настроек: скачивание по старым настройкам прерывается.
    generation: AtomicU64,
    last_used: Mutex<Instant>,
    last_notice: Mutex<Instant>,
    /// Есть несохранённые теги.
    dirty: AtomicBool,
    stopping: AtomicBool,
}

/// Служба автотегов. Дешёвая в клонировании.
#[derive(Clone)]
pub struct Tagger {
    shared: Arc<Shared>,
}

impl Tagger {
    /// `dir` — папка данных тегов. Служба ничего не делает до [`Tagger::configure`] с
    /// включёнными тегами.
    pub fn new(workers: Workers, dir: PathBuf, launcher: Launcher) -> Tagger {
        let (jobs, receiver) = unbounded();
        let shared = Arc::new(Shared {
            workers,
            dir,
            launcher,
            settings: Mutex::new(None),
            status: Mutex::new(TagStatus::default()),
            store: RwLock::new(TagStore::default()),
            vocabulary: RwLock::new(None),
            paths: Mutex::new(None),
            views: Mutex::new(HashMap::new()),
            queue: Mutex::new(Queue::default()),
            frame: Mutex::new((Vec::new(), Vec::new())),
            engine: Mutex::new(None),
            jobs,
            generation: AtomicU64::new(0),
            last_used: Mutex::new(Instant::now()),
            last_notice: Mutex::new(Instant::now()),
            dirty: AtomicBool::new(false),
            stopping: AtomicBool::new(false),
        });
        let worker = shared.clone();
        shared.workers.spawn("tags", move |_| run(&worker, &receiver));
        Tagger { shared }
    }

    /// Применить настройки: модель или включение поменялись — служба перестраивается в фоне,
    /// пороги — меняется только показ.
    pub fn configure(&self, settings: &TagSettings) {
        {
            let mut current = self.shared.settings.lock();
            if current.as_ref() == Some(settings) {
                return;
            }
            let same_engine =
                current.as_ref().is_some_and(|c| c.for_engine() == settings.for_engine());
            *current = Some(settings.clone());
            self.shared.views.lock().clear();
            if same_engine {
                drop(current);
                self.shared.notify(true, true);
                return;
            }
        }
        self.shared.generation.fetch_add(1, Ordering::SeqCst);
        let _ = self.shared.jobs.send(Job::Configure);
    }

    pub fn status(&self) -> TagStatus {
        let mut status = self.shared.status.lock().clone();
        status.queued = self.shared.queue.lock().len();
        status
    }

    /// Включены ли автотеги в настройках.
    pub fn enabled(&self) -> bool {
        self.shared.settings.lock().as_ref().is_some_and(|s| s.enabled)
    }

    /// Теги файла для показа. Нет и могут быть — файл встаёт в очередь видимого (его порядок
    /// — порядок вопросов за кадр; кадр закрывает [`Tagger::end_frame`]). Только память:
    /// можно звать из потока интерфейса на каждую видимую строку.
    pub fn lookup(&self, path: &Path, size: u64, modified: i64) -> Lookup {
        let limit = match self.shared.settings.lock().as_ref() {
            Some(settings) if settings.enabled => u64::from(settings.max_file_mb) << 20,
            _ => return Lookup::None,
        };
        let name = path.file_name().map(|n| n.to_string_lossy()).unwrap_or_default();
        if size > limit || !picture::taggable(&extension_of(&name)) {
            return Lookup::None;
        }
        if let Some((s, m, shown)) = self.shared.views.lock().get(path)
            && *s == size
            && *m == modified
        {
            return Lookup::Ready(shown.clone());
        }
        if let Some(shown) = self.shared.show(path, size, modified) {
            return Lookup::Ready(shown);
        }
        match self.shared.status.lock().state {
            TagState::Off | TagState::Failed(_) => return Lookup::None,
            TagState::Downloading { .. } | TagState::Ready => {}
        }
        let want = Want { path: path.to_path_buf(), size, modified };
        let mut frame = self.shared.frame.lock();
        if !frame.0.contains(&want) {
            frame.0.push(want);
        }
        Lookup::Pending
    }

    /// Кадр нарисован: то, что спросили в нём, — новая очередь видимого.
    pub fn end_frame(&self) {
        let wanted = {
            let mut frame = self.shared.frame.lock();
            let (current, previous) = &mut *frame;
            if current == previous {
                current.clear();
                return;
            }
            *previous = std::mem::take(current);
            previous.clone()
        };
        let visible: HashSet<&Want> = wanted.iter().collect();
        let mut queue = self.shared.queue.lock();
        queue.background.retain(|w| !visible.contains(w));
        queue.visible = wanted.into();
        drop(queue);
        let _ = self.shared.jobs.send(Job::Wake);
    }

    /// Остальные картинки и видео открытой папки — после видимого. Заменяет прошлый список.
    pub fn background(&self, wants: Vec<Want>) {
        if !self.enabled() {
            return;
        }
        let limit =
            self.shared.settings.lock().as_ref().map_or(0, |s| u64::from(s.max_file_mb) << 20);
        let store = self.shared.store.read();
        let wants: VecDeque<Want> = wants
            .into_iter()
            .filter(|w| {
                let name = w.path.file_name().map(|n| n.to_string_lossy()).unwrap_or_default();
                w.size <= limit
                    && picture::taggable(&extension_of(&name))
                    && store.fresh(&w.path, w.size, w.modified).is_none()
            })
            .collect();
        drop(store);
        self.shared.queue.lock().background = wants;
        let _ = self.shared.jobs.send(Job::Wake);
    }

    /// Забыть посчитанные теги: все файлы посмотрятся заново.
    pub fn forget(&self) {
        let _ = self.shared.jobs.send(Job::Forget);
    }

    /// Сохранить теги и выгрузить модель; ждать не дольше `wait`.
    pub fn shutdown(&self, wait: Duration) {
        self.shared.stopping.store(true, Ordering::SeqCst);
        self.shared.generation.fetch_add(1, Ordering::SeqCst);
        let (reply, done) = unbounded();
        if self.shared.jobs.send(Job::Stop(reply)).is_ok() {
            // Картинка на процессоре — секунды: помощника — долой, и хозяин сохранит теги.
            if let Some(mut engine) = self.shared.engine.try_lock_for(Duration::from_millis(200)) {
                *engine = None;
            }
            let _ = done.recv_timeout(wait);
        }
        *self.shared.engine.lock() = None;
    }
}

impl Shared {
    fn settings(&self) -> Option<TagSettings> {
        self.settings.lock().clone()
    }

    fn set_state(&self, state: TagState) {
        let mut status = self.status.lock();
        if status.state != state {
            status.state = state;
            drop(status);
            self.notify(false, true);
        }
    }

    fn update(&self, change: impl FnOnce(&mut TagStatus)) {
        change(&mut self.status.lock());
        self.notify(false, false);
    }

    /// Сообщить интерфейсу: ход — не чаще [`PROGRESS_EVERY`], новые теги и смена
    /// состояния — сразу.
    fn notify(&self, content: bool, force: bool) {
        let mut last = self.last_notice.lock();
        if !force && !content && last.elapsed() < PROGRESS_EVERY {
            return;
        }
        *last = Instant::now();
        drop(last);
        self.workers.send(Event::TagsChanged);
    }

    /// Теги из снимка — в готовый для показа вид (и в кэш).
    fn show(&self, path: &Path, size: u64, modified: i64) -> Option<Arc<Shown>> {
        let vocabulary = self.vocabulary.read().clone()?;
        let thresholds = self.settings.lock().as_ref().map(TagSettings::thresholds)?;
        let shown = {
            let store = self.store.read();
            let tags = store.fresh(path, size, modified)?;
            Arc::new(model::show(tags, &vocabulary, thresholds))
        };
        let mut views = self.views.lock();
        if views.len() >= MAX_VIEWS {
            views.clear();
        }
        views.insert(path.to_path_buf(), (size, modified, shown.clone()));
        Some(shown)
    }

    fn save(&self) {
        if !self.dirty.swap(false, Ordering::SeqCst) {
            return;
        }
        let bytes = self.store.read().write();
        let path = self.dir.join("tags.bin");
        let temp = path.with_extension("bin.tmp");
        if std::fs::create_dir_all(&self.dir).is_ok() && std::fs::write(&temp, bytes).is_ok() {
            let _ = std::fs::rename(&temp, &path);
        }
    }

    fn unload(&self) {
        if self.engine.lock().take().is_some() {
            self.update(|s| s.loaded = false);
        }
    }

    /// Следующий файл: видимое раньше остального; уже посчитанное пропускается.
    fn next(&self) -> Option<Want> {
        let mut queue = self.queue.lock();
        let store = self.store.read();
        loop {
            let want = queue.visible.pop_front().or_else(|| queue.background.pop_front())?;
            if store.fresh(&want.path, want.size, want.modified).is_none() {
                return Some(want);
            }
        }
    }

    fn has_work(&self) -> bool {
        self.queue.lock().len() > 0
    }
}

fn run(shared: &Arc<Shared>, jobs: &Receiver<Job>) {
    // Эскизы Windows (HEIC, RAW, кадры видео) — из потока COM STA.
    let _com = mh_files_platform::thumbs::init_worker_thread();
    mh_files_platform::volume::background_thread();
    let mut last_save = Instant::now();
    loop {
        let ready = shared.status.lock().state == TagState::Ready;
        let job = if ready && shared.has_work() {
            match jobs.try_recv() {
                Ok(job) => Some(job),
                Err(TryRecvError::Empty) => None,
                Err(TryRecvError::Disconnected) => return,
            }
        } else {
            match jobs.recv_timeout(Duration::from_secs(20)) {
                Ok(job) => Some(job),
                Err(RecvTimeoutError::Timeout) => None,
                Err(RecvTimeoutError::Disconnected) => return,
            }
        };
        match job {
            Some(Job::Stop(reply)) => {
                shared.save();
                shared.unload();
                let _ = reply.send(());
                return;
            }
            Some(Job::Configure) => activate(shared),
            Some(Job::Forget) => {
                shared.store.write().clear();
                shared.views.lock().clear();
                shared.dirty.store(true, Ordering::SeqCst);
                shared.save();
                shared.update(|s| {
                    s.tagged = 0;
                    s.skipped = 0;
                });
                shared.notify(true, true);
            }
            Some(Job::Wake) | None => {}
        }
        if shared.status.lock().state == TagState::Ready
            && let Some(want) = shared.next()
        {
            tag(shared, &want);
            if last_save.elapsed() >= SAVE_EVERY {
                last_save = Instant::now();
                shared.save();
            }
            continue;
        }
        // Работы нет.
        if shared.dirty.load(Ordering::SeqCst) && last_save.elapsed() >= SAVE_IDLE {
            last_save = Instant::now();
            shared.save();
        }
        if shared.last_used.lock().elapsed() >= IDLE_UNLOAD {
            shared.unload();
        }
    }
}

/// Привести службу к настройкам: скачать недостающее, прочитать словарь и снимок тегов.
/// Модель загружается позже — когда появится что считать.
fn activate(shared: &Arc<Shared>) {
    let Some(settings) = shared.settings().filter(|s| s.enabled) else {
        shared.unload();
        shared.save();
        *shared.store.write() = TagStore::default();
        *shared.vocabulary.write() = None;
        *shared.paths.lock() = None;
        shared.views.lock().clear();
        *shared.queue.lock() = Queue::default();
        *shared.status.lock() = TagStatus::default();
        shared.notify(true, true);
        return;
    };
    let generation = shared.generation.load(Ordering::SeqCst);
    let mut last = Instant::now() - PROGRESS_EVERY;
    let paths = setup::ensure(&shared.dir, &settings, &mut |what, done, total| {
        if last.elapsed() >= PROGRESS_EVERY {
            last = Instant::now();
            shared.set_state(TagState::Downloading { what: what.to_string(), done, total });
        }
        shared.generation.load(Ordering::SeqCst) == generation
    });
    let paths = match paths {
        Ok(paths) => paths,
        Err(_) if shared.generation.load(Ordering::SeqCst) != generation => return,
        Err(error) => {
            shared.set_state(TagState::Failed(error));
            return;
        }
    };
    let vocabulary = match std::fs::read_to_string(&paths.tags)
        .map_err(|e| format!("{} не читается: {e}", paths.tags.display()))
        .and_then(|text| Vocabulary::parse(&text))
    {
        Ok(vocabulary) => Arc::new(vocabulary),
        Err(error) => {
            shared.set_state(TagState::Failed(error));
            return;
        }
    };
    let key = TagStore::model_key(&settings.model, &vocabulary);
    if shared.store.read().model != key {
        shared.save();
        let store = std::fs::read(shared.dir.join("tags.bin"))
            .ok()
            .and_then(|bytes| TagStore::read(&bytes).ok())
            .filter(|store| store.model == key)
            .unwrap_or_else(|| TagStore::new(&key));
        *shared.store.write() = store;
    }
    // Сменилась модель, ONNX Runtime или видеокарта — помощник запустится заново.
    shared.unload();
    *shared.paths.lock() = Some(paths);
    *shared.vocabulary.write() = Some(vocabulary);
    shared.views.lock().clear();
    let tagged = shared.store.read().len();
    let name = model::model_info(&settings.model)
        .map_or_else(|| settings.model.clone(), |m| m.repo.to_string());
    shared.update(|s| {
        s.tagged = tagged;
        s.model = Some(name);
        s.state = TagState::Ready;
    });
    shared.notify(true, true);
}

/// Работающая модель; нет — запустить (секунды). Ошибка — автотеги не работают.
fn engine<'a>(
    shared: &'a Shared,
) -> Result<parking_lot::MutexGuard<'a, Option<Box<dyn Engine>>>, String> {
    if shared.stopping.load(Ordering::SeqCst) {
        return Err("программа закрывается".into());
    }
    *shared.last_used.lock() = Instant::now();
    let mut guard = shared.engine.lock();
    if guard.as_mut().is_some_and(|engine| !engine.alive()) {
        *guard = None;
    }
    if guard.is_none() {
        let paths = shared.paths.lock().clone().ok_or("модель ещё не готова")?;
        let wanted = shared.settings().is_some_and(|s| s.gpu);
        let engine = (shared.launcher)(&paths, &shared.dir.join("tagger.log"), wanted)?;
        let gpu = engine.spec().gpu;
        *guard = Some(engine);
        drop(guard);
        shared.update(|s| {
            s.loaded = true;
            s.gpu = gpu;
            s.gpu_fallback = wanted && !gpu;
        });
        guard = shared.engine.lock();
    }
    Ok(guard)
}

/// Посчитать теги одного файла. Модель не запускается — автотеги в ошибку, очередь пуста.
fn tag(shared: &Shared, want: &Want) {
    let Some(vocabulary) = shared.vocabulary.read().clone() else { return };
    let started = Instant::now();
    let mut skipped = false;
    let mut attempt = 0;
    let tags = loop {
        let mut guard = match engine(shared) {
            Ok(guard) => guard,
            Err(error) => return fail(shared, error),
        };
        let Some(engine) = guard.as_mut() else { return };
        let spec = engine.spec();
        let name = want.path.file_name().map(|n| n.to_string_lossy()).unwrap_or_default();
        let input = picture::load(&want.path, &extension_of(&name), spec.side)
            .and_then(|rgb| model::input_tensor(&rgb, spec.side));
        let Some(input) = input else { break FileTags::default() };
        match engine.run(&input) {
            Ok(scores) => match FileTags::from_scores(&scores, &vocabulary) {
                Ok(tags) => break tags,
                // Словарь не от этой модели: дальше смотреть бессмысленно.
                Err(error) => return fail(shared, error),
            },
            Err(RunError::Failed(_)) => {
                skipped = true;
                break FileTags::default();
            }
            // Помощник умер: перезапуск, один раз на файл.
            Err(RunError::Died(error)) => {
                *guard = None;
                drop(guard);
                shared.update(|s| s.loaded = false);
                attempt += 1;
                if attempt > 1 {
                    return fail(shared, error);
                }
            }
        }
    };
    // Файл без тегов тоже запоминается: смотреть его снова незачем, пока он не изменится.
    shared
        .store
        .write()
        .insert(want.path.clone(), Record { size: want.size, modified: want.modified, tags });
    shared.views.lock().remove(&want.path);
    shared.dirty.store(true, Ordering::SeqCst);
    let tagged = shared.store.read().len();
    shared.update(|s| {
        s.tagged = tagged;
        s.last = Some(started.elapsed());
        s.skipped += usize::from(skipped);
    });
    shared.notify(true, true);
}

fn fail(shared: &Shared, error: String) {
    *shared.queue.lock() = Queue::default();
    shared.set_state(TagState::Failed(error));
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::AtomicUsize;

    use super::*;

    const CSV: &str = "tag_id,name,category,count\n\
        1,general,9,1\n2,sensitive,9,1\n3,red,0,1\n4,green,0,1\n5,cat_girl,4,1\n";

    /// «Модель»: красная картинка — «red», зелёная — «green»; рейтинг всегда general.
    struct Fake {
        calls: Arc<AtomicUsize>,
        die_once: Arc<AtomicBool>,
    }

    impl Engine for Fake {
        fn spec(&self) -> Spec {
            Spec { side: 8, outputs: 5, gpu: false }
        }

        fn run(&mut self, input: &[f32]) -> Result<Vec<f32>, RunError> {
            if self.die_once.swap(false, Ordering::SeqCst) {
                return Err(RunError::Died("упал".into()));
            }
            self.calls.fetch_add(1, Ordering::SeqCst);
            // Середина картинки, BGR.
            let at = (4 * 8 + 4) * 3;
            let (b, g, r) = (input[at], input[at + 1], input[at + 2]);
            Ok(vec![
                0.9,
                0.1,
                if r > g && r > b { 0.95 } else { 0.0 },
                if g > r { 0.8 } else { 0.0 },
                0.0,
            ])
        }
    }

    struct Fixture {
        tagger: Tagger,
        calls: Arc<AtomicUsize>,
        die_once: Arc<AtomicBool>,
        launches: Arc<AtomicUsize>,
        dir: tempfile::TempDir,
        events: Receiver<Event>,
    }

    fn fixture() -> Fixture {
        let dir = tempfile::tempdir().unwrap();
        let data = dir.path().join("data");
        let runtime = dir.path().join("libonnxruntime.so");
        std::fs::write(&runtime, "").unwrap();
        let settings = TagSettings { enabled: true, runtime, ..TagSettings::default() };
        let models = data.join("models").join(model::model_folder(&settings.model));
        std::fs::create_dir_all(&models).unwrap();
        std::fs::write(models.join(model::MODEL_FILE), "m").unwrap();
        std::fs::write(models.join(model::TAGS_FILE), CSV).unwrap();
        let calls = Arc::new(AtomicUsize::new(0));
        let die_once = Arc::new(AtomicBool::new(false));
        let launches = Arc::new(AtomicUsize::new(0));
        let launcher: Launcher = {
            let (calls, die_once, launches) = (calls.clone(), die_once.clone(), launches.clone());
            Arc::new(move |_, _, _| {
                launches.fetch_add(1, Ordering::SeqCst);
                Ok(Box::new(Fake { calls: calls.clone(), die_once: die_once.clone() })
                    as Box<dyn Engine>)
            })
        };
        let (workers, events) = Workers::new(mh_files_platform::no_waker());
        let tagger = Tagger::new(workers, data, launcher);
        tagger.configure(&settings);
        wait(&tagger, |s| s.state == TagState::Ready);
        Fixture { tagger, calls, die_once, launches, dir, events }
    }

    fn wait(tagger: &Tagger, done: impl Fn(&TagStatus) -> bool) -> TagStatus {
        let started = Instant::now();
        loop {
            let status = tagger.status();
            if done(&status) || started.elapsed() > Duration::from_secs(20) {
                return status;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    fn picture(path: &Path, color: [u8; 3]) -> Want {
        image::RgbImage::from_pixel(200, 120, image::Rgb(color)).save(path).unwrap();
        let meta = std::fs::metadata(path).unwrap();
        Want {
            path: path.to_path_buf(),
            size: meta.len(),
            modified: meta.modified().map_or(0, mh_files_core::index::unix),
        }
    }

    /// Спрашивать, как интерфейс в каждом кадре, пока теги не появятся.
    fn ask(tagger: &Tagger, want: &Want) -> Lookup {
        let started = Instant::now();
        loop {
            let lookup = tagger.lookup(&want.path, want.size, want.modified);
            tagger.end_frame();
            if lookup != Lookup::Pending || started.elapsed() > Duration::from_secs(20) {
                return lookup;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    #[test]
    fn tags_what_is_on_screen_and_remembers() {
        let f = fixture();
        let red = picture(&f.dir.path().join("красный.png"), [220, 10, 10]);
        let green = picture(&f.dir.path().join("зелёный.jpg"), [10, 220, 10]);
        let text = f.dir.path().join("заметка.txt");
        std::fs::write(&text, "текст").unwrap();
        assert_eq!(f.tagger.lookup(&text, 5, 0), Lookup::None, "не картинка");
        let Lookup::Ready(shown) = ask(&f.tagger, &red) else { panic!("нет тегов") };
        assert_eq!(shown.line, "red");
        assert_eq!(shown.rating.as_ref().map(|t| t.name.as_str()), Some("general"));
        let Lookup::Ready(shown) = ask(&f.tagger, &green) else { panic!("нет тегов") };
        assert_eq!(shown.line, "green");
        assert_eq!(f.calls.load(Ordering::SeqCst), 2);
        assert!(f.events.try_iter().any(|e| matches!(e, Event::TagsChanged)));
        // Порог выше уверенности — тег не показывается, а модель не зовётся.
        let strict = TagSettings { general: 90, ..f.tagger.shared.settings().unwrap() };
        f.tagger.configure(&strict);
        let Lookup::Ready(shown) = f.tagger.lookup(&green.path, green.size, green.modified) else {
            panic!("теги пропали")
        };
        assert_eq!(shown.line, "");
        assert_eq!(f.calls.load(Ordering::SeqCst), 2);
        // Файл изменился — смотрится заново.
        let changed = picture(&red.path, [10, 220, 10]);
        let changed = Want { modified: changed.modified + 1, ..changed };
        assert!(matches!(ask(&f.tagger, &changed), Lookup::Ready(_)));
        assert_eq!(f.calls.load(Ordering::SeqCst), 3);
        f.tagger.shutdown(Duration::from_secs(5));
        let store =
            TagStore::read(&std::fs::read(f.dir.path().join("data/tags.bin")).unwrap()).unwrap();
        assert_eq!(store.len(), 2, "теги сохранены при выходе");
    }

    #[test]
    fn folder_in_background_and_restart_after_crash() {
        let f = fixture();
        let wants: Vec<Want> =
            (0..3).map(|i| picture(&f.dir.path().join(format!("{i}.png")), [200, 0, 0])).collect();
        f.die_once.store(true, Ordering::SeqCst);
        f.tagger.background(wants.clone());
        let status = wait(&f.tagger, |s| s.tagged == 3);
        assert_eq!(status.tagged, 3, "{status:?}");
        assert_eq!(status.state, TagState::Ready, "упавший помощник перезапущен");
        assert_eq!(f.launches.load(Ordering::SeqCst), 2);
        // Посчитанное второй раз в очередь не встаёт.
        f.tagger.background(wants);
        assert_eq!(f.tagger.status().queued, 0);
        f.tagger.forget();
        let status = wait(&f.tagger, |s| s.tagged == 0);
        assert_eq!(status.tagged, 0);
        f.tagger.shutdown(Duration::from_secs(5));
    }

    #[test]
    fn disabled_tags_ask_nothing() {
        let (workers, _events) = Workers::new(mh_files_platform::no_waker());
        let dir = tempfile::tempdir().unwrap();
        let launcher: Launcher = Arc::new(|_, _, _| Err("не должна запускаться".into()));
        let tagger = Tagger::new(workers, dir.path().to_path_buf(), launcher);
        tagger.configure(&TagSettings::default());
        assert_eq!(tagger.lookup(Path::new("a.jpg"), 1, 1), Lookup::None);
        tagger.end_frame();
        assert_eq!(tagger.status().state, TagState::Off);
        tagger.shutdown(Duration::from_secs(5));
    }
}
