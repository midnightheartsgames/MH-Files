//! Служба автотегов (1.3).
//!
//! Поток-хозяин по очереди: скачивает модель и ONNX Runtime (только когда автотеги включены),
//! запускает помощника с моделью ([`host`]) и считает теги файлов, которые нужны интерфейсу:
//! сначала видимых в списке и в Инспекторе, потом остальных картинок и видео открытой
//! папки. Теги запоминаются по пути, размеру и времени изменения (`tags.bin`): второй раз
//! файл не смотрится, пока не изменится. После пяти минут без дела модель уходит из памяти.
//!
//! С 1.3.3 — ещё и звук: аудио и звуковая дорожка видео слушаются моделью звука (AST, свой
//! помощник), а видео смотрится по нескольким кадрам. У файла — части разбора (картинка,
//! звук): включили звук — у видео досчитается только он.

pub mod host;
pub mod listen;
pub mod picture;
pub mod setup;

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, RecvTimeoutError, Sender, TryRecvError, unbounded};
use mh_files_core::category::Category;
use mh_files_core::entry::extension_of;
use mh_files_core::settings::TagSettings;
use mh_files_core::sound::{self as sound_model, SoundVocabulary};
use mh_files_core::tags::{
    self as model, FileTags, PART_IMAGE, PART_SOUND, Record, Shown, TagQuery, TagStore, Vocabulary,
};
use parking_lot::{Mutex, RwLock};

pub use self::host::{HOST_FLAG, Host, HostCommand, RunError, Spec};
use crate::{CancelToken, Event, Ticket, Workers};

/// Модель выгружается из памяти после стольких минут без работы.
const IDLE_UNLOAD: Duration = Duration::from_secs(5 * 60);
/// Теги сохраняются во время долгой работы не реже.
const SAVE_EVERY: Duration = Duration::from_secs(2 * 60);
/// Очередь опустела — сохранить, если с прошлого раза прошло столько.
const SAVE_IDLE: Duration = Duration::from_secs(20);
const PROGRESS_EVERY: Duration = Duration::from_millis(250);
/// Найденного по тегам больше не показывается.
const MAX_RESULTS: usize = 5000;
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
    /// Модель звука готова (скачана, классы прочитаны).
    pub sound: bool,
    /// Модель звука в памяти и где считает.
    pub sound_loaded: bool,
    pub sound_gpu: bool,
    /// Модель звука не заработала: звуковые теги не считаются до смены настроек.
    pub sound_error: Option<String>,
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
            sound: false,
            sound_loaded: false,
            sound_gpu: false,
            sound_error: None,
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
    /// Тегов нет и не будет: не картинка, не видео и не звук, слишком большой или автотеги
    /// выключены.
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

/// Как запустить модель: ONNX Runtime, файл модели, журнал и просить ли видеокарту.
pub type Launcher =
    Arc<dyn Fn(&Path, &Path, &Path, bool) -> Result<Box<dyn Engine>, String> + Send + Sync>;

/// Помощник — этот же exe с [`HOST_FLAG`].
pub fn host_launcher(command: HostCommand) -> Launcher {
    Arc::new(move |runtime, model, log, gpu| {
        // Все ядра, кроме одного: процесс и так с пониженным приоритетом.
        let threads = std::thread::available_parallelism().map_or(2, |n| n.get());
        let threads = threads.saturating_sub(1).max(1);
        Host::start(&command, runtime, model, threads, gpu, log)
            .map(|host| Box::new(host) as Box<dyn Engine>)
    })
}

/// Известные теги для подсказок: имя и у скольких файлов он виден.
pub type KnownTags = Arc<Vec<(String, u32)>>;

#[derive(Default)]
struct Known {
    tags: Option<KnownTags>,
    /// Теги менялись с тех пор, как список собран.
    stale: bool,
    building: bool,
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
    /// Классы модели звука — если звуковые теги включены и модель на месте.
    sounds: RwLock<Option<Arc<SoundVocabulary>>>,
    /// Модель звука не заработала: звук не просится до смены настроек.
    sound_failed: AtomicBool,
    known: Mutex<Known>,
    paths: Mutex<Option<setup::Paths>>,
    /// Готовое для показа: путь → размер, время, теги.
    views: Mutex<HashMap<PathBuf, View>>,
    queue: Mutex<Queue>,
    /// Что спросил интерфейс в этом кадре и в прошлом.
    frame: Mutex<(Vec<Want>, Vec<Want>)>,
    engine: Mutex<Option<Box<dyn Engine>>>,
    sound_engine: Mutex<Option<Box<dyn Engine>>>,
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
            sounds: RwLock::new(None),
            sound_failed: AtomicBool::new(false),
            known: Mutex::new(Known::default()),
            paths: Mutex::new(None),
            views: Mutex::new(HashMap::new()),
            queue: Mutex::new(Queue::default()),
            frame: Mutex::new((Vec::new(), Vec::new())),
            engine: Mutex::new(None),
            sound_engine: Mutex::new(None),
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
            self.shared.known.lock().stale = true;
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
        let wanted = self.shared.wanted(path, size);
        if wanted == 0 {
            return Lookup::None;
        }
        let shown = self.tags_of(path, size, modified);
        if self.shared.store.read().missing(path, size, modified, wanted) == 0
            && let Some(shown) = shown
        {
            return Lookup::Ready(shown);
        }
        // Чего-то ещё нет (или посчитана только картинка видео, а звука нет) — в очередь;
        // показать то, что уже есть.
        let ready = Lookup::Ready;
        match self.shared.status.lock().state {
            TagState::Off | TagState::Failed(_) => return shown.map_or(Lookup::None, ready),
            TagState::Downloading { .. } | TagState::Ready => {}
        }
        let want = Want { path: path.to_path_buf(), size, modified };
        let mut frame = self.shared.frame.lock();
        if !frame.0.contains(&want) {
            frame.0.push(want);
        }
        shown.map_or(Lookup::Pending, ready)
    }

    /// Какие части разбора нужны файлу ([`PART_IMAGE`], [`PART_SOUND`]); 0 — тегов у него не
    /// бывает.
    pub fn wanted_parts(&self, path: &Path, size: u64) -> u8 {
        self.shared.wanted(path, size)
    }

    /// Теги, что встречаются у посчитанных файлов, — для подсказок при вводе, самые частые
    /// первыми. Список собирается в фоне: первый раз (и после новых тегов) — прежний или
    /// `None`, готовый придёт с [`Event::TagsChanged`].
    pub fn known_tags(&self) -> Option<KnownTags> {
        let mut known = self.shared.known.lock();
        if (known.tags.is_none() || known.stale) && !known.building && self.enabled() {
            known.building = true;
            known.stale = false;
            let shared = self.shared.clone();
            self.shared.workers.spawn("tags-known", move |_| {
                let tags = collect_known(&shared);
                let mut known = shared.known.lock();
                known.building = false;
                if let Some(tags) = tags {
                    known.tags = Some(Arc::new(tags));
                }
                drop(known);
                shared.notify(true, true);
            });
        }
        known.tags.clone()
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

    /// Теги файла, если они уже посчитаны, — без очереди: для фильтра папки по тегам. Только
    /// память.
    pub fn tags_of(&self, path: &Path, size: u64, modified: i64) -> Option<Arc<Shown>> {
        if !self.enabled() {
            return None;
        }
        if let Some((s, m, shown)) = self.shared.views.lock().get(path)
            && *s == size
            && *m == modified
        {
            return Some(shown.clone());
        }
        self.shared.show(path, size, modified)
    }

    /// Найти по тегам среди всех посчитанных: `cat outdoors -dog -rating:explicit`.
    pub fn search(&self, ticket: Ticket, text: String) -> CancelToken {
        let cancel = CancelToken::default();
        let token = cancel.clone();
        let shared = self.shared.clone();
        self.shared.workers.spawn("tags-search", move |workers| {
            let result = search(&shared, &text, &token);
            if !token.is_cancelled() {
                workers.send(Event::TagResults { ticket, result });
            }
        });
        cancel
    }

    /// Остальные картинки, видео и звук открытой папки — после видимого. Заменяет прошлый
    /// список.
    pub fn background(&self, wants: Vec<Want>) {
        if !self.enabled() {
            return;
        }
        let wants: Vec<(Want, u8)> = wants
            .into_iter()
            .map(|w| {
                let parts = self.shared.wanted(&w.path, w.size);
                (w, parts)
            })
            .filter(|(_, parts)| *parts != 0)
            .collect();
        let store = self.shared.store.read();
        let wants: VecDeque<Want> = wants
            .into_iter()
            .filter(|(w, parts)| store.missing(&w.path, w.size, w.modified, *parts) != 0)
            .map(|(w, _)| w)
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
            // Картинка на процессоре — секунды: помощников — долой, и хозяин сохранит теги.
            for engine in [&self.shared.engine, &self.shared.sound_engine] {
                if let Some(mut engine) = engine.try_lock_for(Duration::from_millis(200)) {
                    *engine = None;
                }
            }
            let _ = done.recv_timeout(wait);
        }
        *self.shared.engine.lock() = None;
        *self.shared.sound_engine.lock() = None;
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

    /// Нужные файлу части разбора (см. [`Tagger::wanted_parts`]).
    fn wanted(&self, path: &Path, size: u64) -> u8 {
        let settings = self.settings.lock();
        let Some(settings) = settings.as_ref().filter(|s| s.enabled) else { return 0 };
        let name = path.file_name().map(|n| n.to_string_lossy()).unwrap_or_default();
        let ext = extension_of(&name);
        let sound = if settings.sound && !self.sound_failed.load(Ordering::Relaxed) {
            PART_SOUND
        } else {
            0
        };
        if picture::is_video(&ext) {
            PART_IMAGE | sound
        } else if Category::Audio.extensions().contains(&ext.as_str()) {
            sound
        } else if picture::taggable(&ext) && size <= u64::from(settings.max_file_mb) << 20 {
            PART_IMAGE
        } else {
            0
        }
    }

    /// Теги из снимка — в готовый для показа вид (и в кэш).
    fn show(&self, path: &Path, size: u64, modified: i64) -> Option<Arc<Shown>> {
        let vocabulary = self.vocabulary.read().clone()?;
        let sounds = self.sounds.read().clone();
        let thresholds = self.settings.lock().as_ref().map(TagSettings::thresholds)?;
        let shown = {
            let store = self.store.read();
            let tags = store.fresh(path, size, modified)?;
            Arc::new(model::show(tags, &vocabulary, sounds.as_deref(), thresholds))
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
        let image = self.engine.lock().take().is_some();
        let sound = self.sound_engine.lock().take().is_some();
        if image || sound {
            self.update(|s| {
                s.loaded = false;
                s.sound_loaded = false;
            });
        }
    }

    /// Следующий файл и недостающие части: видимое раньше остального; уже посчитанное
    /// пропускается.
    fn next(&self) -> Option<(Want, u8)> {
        loop {
            let want = {
                let mut queue = self.queue.lock();
                queue.visible.pop_front().or_else(|| queue.background.pop_front())?
            };
            let wanted = self.wanted(&want.path, want.size);
            let missing = self.store.read().missing(&want.path, want.size, want.modified, wanted);
            if missing != 0 {
                return Some((want, missing));
            }
        }
    }

    fn sound_failed(&self, error: String) {
        self.sound_failed.store(true, Ordering::SeqCst);
        *self.sound_engine.lock() = None;
        self.update(|s| {
            s.sound_loaded = false;
            s.sound_error = Some(error);
        });
        self.notify(true, true);
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
                shared.known.lock().stale = true;
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
            && let Some((want, parts)) = shared.next()
        {
            tag(shared, &want, parts);
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
        *shared.sounds.write() = None;
        *shared.known.lock() = Known::default();
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
    let sounds = match &paths.sound {
        Some(sound) => match std::fs::read_to_string(&sound.config)
            .map_err(|e| format!("{} не читается: {e}", sound.config.display()))
            .and_then(|text| SoundVocabulary::parse(&text))
        {
            Ok(sounds) => Some(Arc::new(sounds)),
            Err(error) => {
                shared.set_state(TagState::Failed(error));
                return;
            }
        },
        None => None,
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
    // Звук выключен — его теги остаются в снимке, просто не видны.
    if let Some(sounds) = &sounds {
        let sound_key = TagStore::sound_key(&settings.sound_model, sounds);
        if shared.store.read().sound != sound_key {
            shared.store.write().set_sound_model(&sound_key);
            shared.dirty.store(true, Ordering::SeqCst);
        }
    }
    // Сменилась модель, ONNX Runtime или видеокарта — помощники запустятся заново.
    shared.unload();
    shared.sound_failed.store(false, Ordering::SeqCst);
    *shared.paths.lock() = Some(paths);
    *shared.vocabulary.write() = Some(vocabulary);
    let has_sound = sounds.is_some();
    *shared.sounds.write() = sounds;
    shared.views.lock().clear();
    shared.known.lock().stale = true;
    let tagged = shared.store.read().len();
    let name = model::model_info(&settings.model)
        .map_or_else(|| settings.model.clone(), |m| m.repo.to_string());
    shared.update(|s| {
        s.tagged = tagged;
        s.model = Some(name);
        s.sound = has_sound;
        s.sound_error = None;
        s.state = TagState::Ready;
    });
    shared.notify(true, true);
}

/// Работающая модель (картинки или звука); нет — запустить (секунды).
fn engine(
    shared: &Shared,
    sound: bool,
) -> Result<parking_lot::MutexGuard<'_, Option<Box<dyn Engine>>>, String> {
    if shared.stopping.load(Ordering::SeqCst) {
        return Err("программа закрывается".into());
    }
    *shared.last_used.lock() = Instant::now();
    let slot = if sound { &shared.sound_engine } else { &shared.engine };
    let mut guard = slot.lock();
    if guard.as_mut().is_some_and(|engine| !engine.alive()) {
        *guard = None;
    }
    if guard.is_none() {
        let paths = shared.paths.lock().clone().ok_or("модель ещё не готова")?;
        let wanted = shared.settings().is_some_and(|s| s.gpu);
        let (model, log) = match (&paths.sound, sound) {
            (Some(sound), true) => (sound.model.clone(), "tagger-sound.log"),
            (None, true) => return Err("звуковые теги выключены".into()),
            (_, false) => (paths.model.clone(), "tagger.log"),
        };
        let engine = (shared.launcher)(&paths.runtime, &model, &shared.dir.join(log), wanted)?;
        let gpu = engine.spec().gpu;
        *guard = Some(engine);
        drop(guard);
        shared.update(|s| {
            if sound {
                s.sound_loaded = true;
                s.sound_gpu = gpu;
            } else {
                s.loaded = true;
                s.gpu = gpu;
                s.gpu_fallback = wanted && !gpu;
            }
        });
        guard = slot.lock();
    }
    Ok(guard)
}

/// Прогнать входы через модель: `prepare` готовит их по её форме (без замка — кадры видео
/// читаются долго). Отказ модели на одном входе — он пропускается (`true` во втором поле);
/// помощник умер — перезапуск, один раз на вход; второй раз или не запустился — ошибка.
fn run_model(
    shared: &Shared,
    sound: bool,
    prepare: impl FnOnce(Spec) -> Vec<Vec<f32>>,
) -> Result<(Vec<Vec<f32>>, bool), String> {
    let spec = {
        let mut guard = engine(shared, sound)?;
        guard.as_mut().ok_or("модель выгружена")?.spec()
    };
    let inputs = prepare(spec);
    let mut outputs = Vec::with_capacity(inputs.len());
    let mut skipped = false;
    for input in &inputs {
        let mut attempt = 0;
        loop {
            let mut guard = engine(shared, sound)?;
            let engine = guard.as_mut().ok_or("модель выгружена")?;
            match engine.run(input) {
                Ok(output) => {
                    outputs.push(output);
                    break;
                }
                Err(RunError::Failed(_)) => {
                    skipped = true;
                    break;
                }
                Err(RunError::Died(error)) => {
                    *guard = None;
                    drop(guard);
                    shared.update(|s| {
                        if sound {
                            s.sound_loaded = false;
                        } else {
                            s.loaded = false;
                        }
                    });
                    attempt += 1;
                    if attempt > 1 {
                        return Err(error);
                    }
                }
            }
        }
    }
    Ok((outputs, skipped))
}

/// Посчитать недостающие части тегов файла (`parts`). Модель картинок не запускается —
/// автотеги в ошибку, очередь пуста; модель звука — без звука до смены настроек.
fn tag(shared: &Shared, want: &Want, parts: u8) {
    let Some(vocabulary) = shared.vocabulary.read().clone() else { return };
    let Some(settings) = shared.settings() else { return };
    let started = Instant::now();
    let name = want.path.file_name().map(|n| n.to_string_lossy()).unwrap_or_default();
    let ext = extension_of(&name);
    let mut record =
        shared.store.read().record(&want.path, want.size, want.modified).cloned().unwrap_or(
            Record {
                size: want.size,
                modified: want.modified,
                parts: 0,
                tags: FileTags::default(),
            },
        );
    let mut skipped = false;
    if parts & PART_IMAGE != 0 {
        let video = picture::is_video(&ext);
        let step = f64::from(settings.video_step.max(1));
        let frames = settings.video_frames.max(1) as usize;
        let ran = run_model(shared, false, |spec| {
            let pictures = if video {
                picture::video_frames(&want.path, spec.side, step, frames)
            } else {
                picture::load(&want.path, &ext, spec.side).into_iter().collect()
            };
            pictures.iter().filter_map(|rgb| model::input_tensor(rgb, spec.side)).collect()
        });
        let (outputs, refused) = match ran {
            Ok(ran) => ran,
            Err(error) => return fail(shared, error),
        };
        skipped |= refused;
        let image = match outputs.as_slice() {
            [] => FileTags::default(),
            [one] => match FileTags::from_scores(one, &vocabulary) {
                Ok(tags) => tags,
                // Словарь не от этой модели: дальше смотреть бессмысленно.
                Err(error) => return fail(shared, error),
            },
            frames => {
                let scores = model::combine_frames(frames, &vocabulary);
                match FileTags::from_scores(&scores, &vocabulary) {
                    Ok(tags) => tags,
                    Err(error) => return fail(shared, error),
                }
            }
        };
        record.tags.set_image(&image);
        record.parts |= PART_IMAGE;
    }
    if parts & PART_SOUND != 0
        && let Some(sounds) = shared.sounds.read().clone()
    {
        let clips = settings.sound_clips.clamp(1, 16) as usize;
        match listen::clips(&want.path, &ext, clips) {
            Ok(clips) => match run_model(shared, true, move |_| clips) {
                Ok((outputs, refused)) => {
                    skipped |= refused;
                    let scores = sound_model::combine_clips(&outputs);
                    match record.tags.set_sound(&scores, &sounds) {
                        Ok(()) => record.parts |= PART_SOUND,
                        Err(error) => shared.sound_failed(error),
                    }
                }
                Err(error) => shared.sound_failed(error),
            },
            // Звука нет или он не читается: смотреть снова незачем.
            Err(_) => {
                record.tags.set_sound(&[], &sounds).ok();
                record.parts |= PART_SOUND;
            }
        }
    }
    if record.parts == 0 {
        return;
    }
    // Файл без тегов тоже запоминается: смотреть его снова незачем, пока он не изменится.
    shared.store.write().insert(want.path.clone(), record);
    shared.views.lock().remove(&want.path);
    shared.known.lock().stale = true;
    shared.dirty.store(true, Ordering::SeqCst);
    let tagged = shared.store.read().len();
    shared.update(|s| {
        s.tagged = tagged;
        s.last = Some(started.elapsed());
        s.skipped += usize::from(skipped);
    });
    shared.notify(true, true);
}

/// Теги посчитанных файлов по порогам показа и сколько у кого.
fn collect_known(shared: &Shared) -> Option<Vec<(String, u32)>> {
    let vocabulary = shared.vocabulary.read().clone()?;
    let sounds = shared.sounds.read().clone();
    let thresholds = shared.settings()?.thresholds();
    let mut counts: HashMap<String, u32> = HashMap::new();
    {
        let store = shared.store.read();
        for (_, record) in store.iter() {
            let shown = model::show(&record.tags, &vocabulary, sounds.as_deref(), thresholds);
            for tag in shown.all() {
                *counts.entry(tag.name.clone()).or_default() += 1;
            }
        }
    }
    let mut tags: Vec<(String, u32)> = counts.into_iter().collect();
    tags.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    Some(tags)
}

/// Итог поиска по тегам.
#[derive(Debug)]
pub struct TagResults {
    /// Лучшие первыми: сумма уверенности нужных тегов.
    pub entries: Vec<mh_files_core::Entry>,
    /// Подошло всего (показано не больше [`MAX_RESULTS`]).
    pub total: usize,
    pub elapsed: Duration,
}

fn search(shared: &Shared, text: &str, cancel: &CancelToken) -> Result<TagResults, String> {
    let started = Instant::now();
    let empty =
        |started: Instant| TagResults { entries: Vec::new(), total: 0, elapsed: started.elapsed() };
    let settings = shared
        .settings()
        .filter(|s| s.enabled)
        .ok_or("Автотеги выключены — включите их в «Настройки › Автотеги»")?;
    let (query, _) = TagQuery::parse(text, true);
    if !query.selects() {
        return Ok(empty(started));
    }
    let vocabulary =
        shared.vocabulary.read().clone().ok_or_else(|| {
            format!("Автотеги ещё не готовы: {}", shared.status.lock().describe())
        })?;
    let sounds = shared.sounds.read().clone();
    let thresholds = settings.thresholds();
    let mut found: Vec<(f32, PathBuf, u64, i64)> = {
        let store = shared.store.read();
        store
            .iter()
            .filter_map(|(path, record)| {
                let shown = model::show(&record.tags, &vocabulary, sounds.as_deref(), thresholds);
                query
                    .matches(Some(&shown))
                    .then(|| (query.score(&shown), path.clone(), record.size, record.modified))
            })
            .collect()
    };
    if cancel.is_cancelled() {
        return Err("отменено".into());
    }
    found.sort_by(|a, b| b.0.total_cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
    let total = found.len();
    let mut entries = Vec::new();
    for (_, path, size, modified) in found {
        if entries.len() >= MAX_RESULTS || cancel.is_cancelled() {
            break;
        }
        // Удалённое и изменённое с тех пор, как теги посчитаны, не показывается.
        let Ok(entry) = crate::read::stat(&path) else { continue };
        if entry.size == size && entry.modified.map_or(0, mh_files_core::index::unix) == modified {
            entries.push(entry);
        }
    }
    Ok(TagResults { entries, total, elapsed: started.elapsed() })
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

    /// «Модель звука»: всегда музыка.
    struct FakeSound;

    impl Engine for FakeSound {
        fn spec(&self) -> Spec {
            Spec { side: 0, outputs: 3, gpu: false }
        }

        fn run(&mut self, input: &[f32]) -> Result<Vec<f32>, RunError> {
            assert_eq!(input.len(), sound_model::FRAMES * sound_model::MELS);
            Ok(vec![3.0, -6.0, -6.0])
        }
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
        let sounds = data.join("models").join(model::model_folder(sound_model::SOUND_REPO));
        std::fs::create_dir_all(sounds.join("onnx")).unwrap();
        std::fs::write(sounds.join(&settings.sound_model), "m").unwrap();
        std::fs::write(
            sounds.join(sound_model::SOUND_CONFIG),
            r#"{"id2label": {"0": "Music", "1": "Dog", "2": "Speech"}}"#,
        )
        .unwrap();
        let calls = Arc::new(AtomicUsize::new(0));
        let die_once = Arc::new(AtomicBool::new(false));
        let launches = Arc::new(AtomicUsize::new(0));
        let launcher: Launcher = {
            let (calls, die_once, launches) = (calls.clone(), die_once.clone(), launches.clone());
            Arc::new(move |_, model: &Path, _, _| {
                if model.to_string_lossy().contains("ast-finetuned") {
                    return Ok(Box::new(FakeSound) as Box<dyn Engine>);
                }
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
        // Поиск по тегам: и по посчитанному, и с рейтингом.
        let asked = std::cell::Cell::new(0);
        let found = |text: &str| {
            asked.set(asked.get() + 1);
            let ticket = Ticket { owner: 1, generation: asked.get() };
            f.tagger.search(ticket, text.into());
            loop {
                match f.events.recv_timeout(Duration::from_secs(10)).unwrap() {
                    Event::TagResults { ticket: answer, result } if answer == ticket => {
                        break result
                            .unwrap()
                            .entries
                            .into_iter()
                            .map(|e| e.name)
                            .collect::<Vec<_>>();
                    }
                    _ => continue,
                }
            }
        };
        let relaxed = TagSettings { general: 35, ..f.tagger.shared.settings().unwrap() };
        f.tagger.configure(&relaxed);
        assert_eq!(found("green"), vec!["зелёный.jpg"]);
        assert_eq!(found("tag:green -rating:general"), Vec::<String>::new());
        // У «красного» теги посчитаны для другого времени изменения — он не показывается.
        assert_eq!(found("rating:general"), vec!["зелёный.jpg"]);
        assert!(found("").is_empty(), "пустой запрос — пусто, а не всё подряд");
        assert!(f.tagger.tags_of(&green.path, green.size, green.modified).is_some());
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

    /// WAV: тон 440 Гц, моно 16 бит.
    fn tone(path: &Path, seconds: f64) -> Want {
        let rate = 16_000u32;
        let data: Vec<u8> = (0..(f64::from(rate) * seconds) as usize)
            .flat_map(|i| {
                let v = (0.3
                    * (2.0 * std::f64::consts::PI * 440.0 * i as f64 / 16_000.0).sin()
                    * 32767.0) as i16;
                v.to_le_bytes()
            })
            .collect();
        let mut out = b"RIFF\0\0\0\0WAVEfmt ".to_vec();
        out.extend_from_slice(&16u32.to_le_bytes());
        out.extend_from_slice(&[1, 0, 1, 0]);
        out.extend_from_slice(&rate.to_le_bytes());
        out.extend_from_slice(&(rate * 2).to_le_bytes());
        out.extend_from_slice(&[2, 0, 16, 0]);
        out.extend_from_slice(b"data");
        out.extend_from_slice(&(data.len() as u32).to_le_bytes());
        out.extend_from_slice(&data);
        std::fs::write(path, out).unwrap();
        let meta = std::fs::metadata(path).unwrap();
        Want {
            path: path.to_path_buf(),
            size: meta.len(),
            modified: meta.modified().map_or(0, mh_files_core::index::unix),
        }
    }

    #[test]
    fn listens_to_sound() {
        let f = fixture();
        let wav = tone(&f.dir.path().join("мелодия.wav"), 3.0);
        assert_eq!(f.tagger.wanted_parts(&wav.path, wav.size), PART_SOUND);
        let Lookup::Ready(shown) = ask(&f.tagger, &wav) else { panic!("нет тегов") };
        let heard: Vec<&str> = shown.sounds.iter().map(|t| t.name.as_str()).collect();
        assert_eq!(heard, vec!["music"]);
        assert!(shown.tags.is_empty() && shown.rating.is_none(), "у звука нет картинки");
        assert_eq!(f.calls.load(Ordering::SeqCst), 0, "модель картинок не звалась");
        // Подсказки: известные теги с числом файлов.
        let started = Instant::now();
        let known = loop {
            if let Some(known) = f.tagger.known_tags()
                && !known.is_empty()
            {
                break known;
            }
            assert!(started.elapsed() < Duration::from_secs(10));
            std::thread::sleep(Duration::from_millis(10));
        };
        assert_eq!(known.as_slice(), [("music".to_string(), 1)]);
        // Звук выключили — звуковые теги не видны, и звук больше не нужен.
        let quiet = TagSettings { sound: false, ..f.tagger.shared.settings().unwrap() };
        f.tagger.configure(&quiet);
        wait(&f.tagger, |s| s.state == TagState::Ready && !s.sound);
        assert_eq!(f.tagger.wanted_parts(&wav.path, wav.size), 0);
        assert_eq!(f.tagger.lookup(&wav.path, wav.size, wav.modified), Lookup::None);
        f.tagger.shutdown(Duration::from_secs(5));
    }

    #[test]
    fn disabled_tags_ask_nothing() {
        let (workers, _events) = Workers::new(mh_files_platform::no_waker());
        let dir = tempfile::tempdir().unwrap();
        let launcher: Launcher = Arc::new(|_, _, _, _| Err("не должна запускаться".into()));
        let tagger = Tagger::new(workers, dir.path().to_path_buf(), launcher);
        tagger.configure(&TagSettings::default());
        assert_eq!(tagger.lookup(Path::new("a.jpg"), 1, 1), Lookup::None);
        tagger.end_frame();
        assert_eq!(tagger.status().state, TagState::Off);
        tagger.shutdown(Duration::from_secs(5));
    }
}
