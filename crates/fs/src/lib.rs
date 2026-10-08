//! Фоновая работа MH Files. Поток UI только ставит задачи и забирает [`Event`] из одного канала.
//!
//! Правила (PLAN.md §4):
//! * каждый результат помечен [`Ticket`] — номером владельца (вкладки) и поколением запроса;
//!   UI выбрасывает результаты устаревших поколений, поэтому медленный ответ из старой папки
//!   никогда не затрёт новую;
//! * любую долгую задачу можно отменить [`CancelToken`];
//! * ссылки и junction рекурсивно не обходятся.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use crossbeam_channel::{Receiver, Sender, unbounded};
use mh_files_core::Entry;
use mh_files_platform::Waker;
use mh_files_platform::clipboard::ClipboardFiles;
use mh_files_platform::drives::{DriveInfo, DriveKind};
use mh_files_platform::folders::KnownFolder;
use mh_files_platform::integration;
use mh_files_platform::shell::{MenuChoice, ShellMenuItem};

pub mod archive;
mod archive_tool;
pub mod duplicates;
pub mod font;
pub mod images;
pub mod indexer;
pub mod listing;
pub mod preview;
pub mod read;
pub mod rename;
pub mod search;
pub mod shell;
pub mod sizes;
pub mod sorting;
pub mod transfer;
pub mod watch;

pub use duplicates::{DuplicateOptions, DuplicateProgress, LinkReport};
pub use images::{ImageKey, ImageKind, ImageResult};
pub use indexer::{IndexResults, IndexStatus, Indexer, VolumeState, VolumeStatus};
pub use preview::{Preview, PreviewRequest};
pub use search::SearchQuery;
pub use shell::ShellJob;
pub use sizes::DirSize;
pub use transfer::{Conflict, Transfer};
pub use watch::DirWatch;

/// Чей это результат: владелец (обычно вкладка) и поколение его запроса.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Ticket {
    pub owner: u64,
    pub generation: u64,
}

/// Флаг отмены. Клоны общие; отмена видна всем.
#[derive(Debug, Clone, Default)]
pub struct CancelToken(Arc<AtomicBool>);

impl CancelToken {
    pub fn cancel(&self) {
        self.0.store(true, Ordering::Relaxed);
    }

    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::Relaxed)
    }

    /// Сам флаг — для кода, который принимает `&AtomicBool` (сортировщик).
    pub fn flag(&self) -> &AtomicBool {
        &self.0
    }
}

#[derive(Debug)]
pub enum Event {
    /// Очередная пачка записей папки.
    Listing {
        ticket: Ticket,
        batch: Vec<Entry>,
    },
    ListingDone {
        ticket: Ticket,
        result: Result<(), String>,
    },
    /// Правки от наблюдателя: что обновить, что убрать; `reload` — перечитать целиком.
    Patch {
        ticket: Ticket,
        upserts: Vec<Entry>,
        removes: Vec<PathBuf>,
        reload: bool,
    },
    /// Найденное поиском и сколько папок уже просмотрено.
    Search {
        ticket: Ticket,
        batch: Vec<Entry>,
        scanned: usize,
    },
    SearchDone {
        ticket: Ticket,
        result: Result<(), String>,
        scanned: usize,
    },
    Preview {
        ticket: Ticket,
        path: PathBuf,
        preview: Preview,
    },
    /// Подпапки для дописывания пути в GoTo.
    Completion {
        ticket: Ticket,
        dir: PathBuf,
        names: Vec<String>,
    },
    DriveRoots(Vec<(PathBuf, DriveKind)>),
    Drive(DriveInfo),
    KnownFolders(Vec<(KnownFolder, PathBuf)>),
    Image {
        key: ImageKey,
        result: ImageResult,
    },
    /// Есть ли у расширений обработчики эскизов Windows ([`Workers::thumbnail_types`]).
    ThumbnailTypes(Vec<(String, bool)>),
    /// Итог действия Shell; ошибку показать пользователю.
    Shell {
        what: String,
        result: Result<(), String>,
    },
    /// Файлы из буфера обмена для вставки в `dest`.
    Paste {
        dest: PathBuf,
        files: Option<ClipboardFiles>,
    },
    RenameDone {
        ticket: Ticket,
        result: Result<usize, String>,
    },
    /// Итог проверки копирования/перемещения: занятые имена в папке назначения.
    Preflight {
        transfer: Transfer,
        conflicts: Vec<Conflict>,
    },
    /// Размер папки целиком.
    FolderSize {
        ticket: Ticket,
        path: PathBuf,
        size: DirSize,
    },
    /// Пункты меню Windows для своего контекстного меню (поколение запроса — `generation`).
    /// Приходят дважды: без `complete` — подменю ещё заполняются. Номер выбранной команды —
    /// в `commands`; отпустить все отправители — меню закрыто.
    ShellMenu {
        generation: u64,
        items: Result<Vec<ShellMenuItem>, String>,
        complete: bool,
        commands: Sender<u32>,
    },
    /// Меню Windows закрыто; `paths` — для чего его открывали.
    Menu {
        paths: Vec<PathBuf>,
        choice: Result<MenuChoice, String>,
    },
    /// Итог поиска по индексу дисков.
    IndexResults {
        ticket: Ticket,
        result: Result<IndexResults, String>,
    },
    /// Индекс изменился: состояние (обход, число записей) и, если `content`, содержимое —
    /// открытые результаты стоит обновить.
    IndexChanged {
        content: bool,
    },
    /// Ход извлечения из архива.
    ExtractProgress {
        id: u64,
        done: u64,
        total: u64,
    },
    /// Извлечение закончено: созданные пути верхнего уровня.
    Extracted {
        extraction: Extraction,
        result: Result<Vec<PathBuf>, String>,
    },
    DuplicatesProgress {
        ticket: Ticket,
        progress: DuplicateProgress,
    },
    DuplicatesDone {
        ticket: Ticket,
        result: Result<Vec<mh_files_core::duplicates::Group>, String>,
    },
    /// Каких из проверенных путей больше нет на диске.
    Missing {
        owner: u64,
        paths: Vec<PathBuf>,
    },
    /// Сортировщик: сколько файлов просмотрено при построении плана.
    SortScanProgress {
        ticket: Ticket,
        files: usize,
    },
    /// План готов (или папка не прочиталась — ошибки внутри плана).
    SortScanned {
        ticket: Ticket,
        plan: mh_files_core::sorting::Plan,
    },
    /// Ход сортировки или отмены: сделано, всего, текущий файл.
    SortProgress {
        ticket: Ticket,
        done: usize,
        total: usize,
        current: String,
    },
    /// Сортировка или отмена закончена. `journal` — журнал этой операции (для отмены).
    SortDone {
        ticket: Ticket,
        report: mh_files_core::sorting::Report,
        journal: PathBuf,
    },
    /// categories.json записан из настроек: ошибка или путь копии испорченного файла.
    CategoriesSaved {
        result: Result<Option<PathBuf>, String>,
    },
    /// Лишние копии заменены жёсткими ссылками.
    DuplicatesLinked {
        ticket: Ticket,
        report: LinkReport,
    },
    /// Итог проверки обновлений.
    Update(Result<mh_files_core::update::Check, String>),
    /// Что из встраивания в Проводник сейчас есть; ошибка — если менять не вышло.
    Integration {
        status: mh_files_platform::integration::Status,
        error: Option<String>,
    },
    /// Последняя операция сортировщика, которую ещё можно отменить.
    SortLast {
        ticket: Ticket,
        last: Option<(PathBuf, mh_files_core::sorting::Journal)>,
    },
}

/// Что сделать с извлечённым.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AfterExtract {
    /// Просто сообщить.
    Report,
    /// Открыть (файл из архива по двойному щелчку).
    Open,
    /// Положить в буфер обмена как файлы (Ctrl+C в архиве).
    Clipboard,
}

/// Задание на извлечение.
#[derive(Debug, Clone)]
pub struct Extraction {
    pub id: u64,
    pub archive: PathBuf,
    /// Пути внутри архива; папки — целиком.
    pub inners: Vec<String>,
    pub dest: PathBuf,
    pub then: AfterExtract,
}

/// Как часто сверять список букв дисков: флешка появляется в окне не позже чем через это.
const DRIVES_POLL: Duration = Duration::from_secs(1);

/// Точка постановки задач. Дешёвая в клонировании.
#[derive(Clone)]
pub struct Workers {
    tx: Sender<Event>,
    waker: Waker,
    images: images::Pool,
}

impl Workers {
    pub fn new(waker: Waker) -> (Workers, Receiver<Event>) {
        let (tx, rx) = unbounded();
        let images = images::Pool::new(tx.clone(), waker.clone());
        (Workers { tx, waker, images }, rx)
    }

    pub(crate) fn send(&self, event: Event) {
        let _ = self.tx.send(event);
        (self.waker)();
    }

    /// Новый поток для задачи; имя видно в отладчике.
    pub(crate) fn spawn(&self, name: &str, job: impl FnOnce(Workers) + Send + 'static) {
        let workers = self.clone();
        let _ = std::thread::Builder::new().name(name.into()).spawn(move || job(workers));
    }

    /// Прочитать папку. Записи приходят пачками, затем `ListingDone`.
    pub fn list(&self, ticket: Ticket, dir: PathBuf) -> CancelToken {
        let cancel = CancelToken::default();
        let token = cancel.clone();
        self.spawn("list", move |workers| listing::run(&workers, ticket, &dir, &token));
        cancel
    }

    /// Рекурсивный поиск по `root`.
    pub fn search(&self, ticket: Ticket, root: PathBuf, query: SearchQuery) -> CancelToken {
        let cancel = CancelToken::default();
        let token = cancel.clone();
        self.spawn("search", move |workers| search::run(&workers, ticket, &root, &query, &token));
        cancel
    }

    /// Следить за папкой. Наблюдение идёт, пока жив возвращённый объект.
    pub fn watch(&self, ticket: Ticket, dir: PathBuf) -> Option<DirWatch> {
        watch::start(self.clone(), ticket, dir)
    }

    pub fn preview(&self, ticket: Ticket, request: PreviewRequest) -> CancelToken {
        let cancel = CancelToken::default();
        let token = cancel.clone();
        self.spawn("preview", move |workers| {
            let _com = mh_files_platform::thumbs::init_worker_thread();
            let preview = preview::load(&request, &token);
            if !token.is_cancelled() {
                workers.send(Event::Preview { ticket, path: request.path, preview });
            }
        });
        cancel
    }

    /// Подпапки `dir`, чьи имена начинаются с `prefix`.
    pub fn complete(&self, ticket: Ticket, dir: PathBuf, prefix: String) {
        self.spawn("complete", move |workers| {
            let names = listing::subdirs(&dir, &prefix, 64);
            workers.send(Event::Completion { ticket, dir, names });
        });
    }

    /// Опрос дисков: сразу список корней, затем каждый диск отдельным потоком (F5 на «Этом
    /// компьютере» — свежее свободное место).
    pub fn drives(&self) {
        self.spawn("drives", |workers| {
            let roots = mh_files_platform::drives::drive_roots();
            workers.send(Event::DriveRoots(roots.clone()));
            for (root, kind) in roots {
                workers.spawn("drive", move |workers| {
                    let info = mh_files_platform::drives::drive_info(&root, kind);
                    workers.send(Event::Drive(info));
                });
            }
        });
    }

    /// Следить за списком дисков всё время работы: раз в секунду сверяется маска букв
    /// (мгновенно, к дискам не обращается). Изменилась — новый список корней и сведения о
    /// новых дисках; прежние не опрашиваются — уснувший HDD не будится. Первый проход — сразу.
    pub fn watch_drives(&self) {
        self.spawn("drives-watch", |workers| {
            let mut last = None;
            let mut known: Vec<(PathBuf, mh_files_platform::drives::DriveKind)> = Vec::new();
            loop {
                let mask = mh_files_platform::drives::drive_mask();
                if last != Some(mask) {
                    last = Some(mask);
                    let roots = mh_files_platform::drives::drive_roots();
                    workers.send(Event::DriveRoots(roots.clone()));
                    for (root, kind) in roots.iter().filter(|r| !known.contains(r)).cloned() {
                        workers.spawn("drive", move |workers| {
                            let info = mh_files_platform::drives::drive_info(&root, kind);
                            workers.send(Event::Drive(info));
                        });
                    }
                    known = roots;
                }
                std::thread::sleep(DRIVES_POLL);
            }
        });
    }

    /// Узнать, у каких расширений (без точки) в Windows есть обработчик эскизов: реестр —
    /// не для потока UI.
    pub fn thumbnail_types(&self, exts: Vec<String>) {
        self.spawn("thumbnail-types", move |workers| {
            let _com = mh_files_platform::thumbs::init_worker_thread();
            let types = exts
                .into_iter()
                .map(|ext| {
                    let has = mh_files_platform::thumbs::has_thumbnail_provider(&ext);
                    (ext, has)
                })
                .collect();
            workers.send(Event::ThumbnailTypes(types));
        });
    }

    pub fn known_folders(&self) {
        self.spawn("known-folders", |workers| {
            let _com = mh_files_platform::thumbs::init_worker_thread();
            workers.send(Event::KnownFolders(mh_files_platform::folders::known_folders()));
        });
    }

    /// Действие Shell в отдельном потоке: зависшее расширение не задержит остальные.
    pub fn shell(&self, job: ShellJob) {
        self.spawn("shell", move |workers| shell::run(&workers, job));
    }

    pub fn image(&self, key: ImageKey) {
        self.images.request(key);
    }

    /// Забыть заявки на картинки, которые больше не нужны (ушли из видимой области).
    pub fn retain_images(&self, keep: impl Fn(&ImageKey) -> bool) {
        self.images.retain(keep);
    }

    /// Найти конфликты имён перед копированием или перемещением.
    pub fn preflight(&self, transfer: Transfer) {
        self.spawn("preflight", move |workers| {
            let conflicts = transfer::conflicts(&transfer);
            workers.send(Event::Preflight { transfer, conflicts });
        });
    }

    /// Посчитать размеры папок по очереди; каждый итог — отдельным событием.
    pub fn folder_sizes(&self, ticket: Ticket, dirs: Vec<PathBuf>) -> CancelToken {
        let cancel = CancelToken::default();
        let token = cancel.clone();
        self.spawn("folder-sizes", move |workers| {
            for path in dirs {
                let Some(size) = sizes::dir_size(&path, &token) else { return };
                workers.send(Event::FolderSize { ticket, path, size });
            }
        });
        cancel
    }

    /// Прочитать папку внутри архива — теми же событиями, что и обычную.
    pub fn list_archive(&self, ticket: Ticket, archive: PathBuf, inner: String) -> CancelToken {
        let cancel = CancelToken::default();
        let token = cancel.clone();
        self.spawn("list-archive", move |workers| {
            let result = archive::list(&archive, &inner);
            if token.is_cancelled() {
                return;
            }
            let result = result.map(|batch| workers.send(Event::Listing { ticket, batch }));
            workers.send(Event::ListingDone { ticket, result });
        });
        cancel
    }

    /// Извлечь из архива в фоне; ход — `ExtractProgress`, итог — `Extracted`.
    pub fn extract(&self, extraction: Extraction) -> CancelToken {
        let cancel = CancelToken::default();
        let token = cancel.clone();
        self.spawn("extract", move |workers| {
            let id = extraction.id;
            let total = archive::total_size(&extraction.archive, &extraction.inners).unwrap_or(0);
            let mut done = 0;
            let mut last = std::time::Instant::now();
            let result = archive::extract(
                &extraction.archive,
                &extraction.inners,
                &extraction.dest,
                &token,
                |bytes| {
                    done += bytes;
                    if last.elapsed() >= std::time::Duration::from_millis(100) {
                        last = std::time::Instant::now();
                        workers.send(Event::ExtractProgress { id, done, total });
                    }
                },
            );
            workers.send(Event::Extracted { extraction, result });
        });
        cancel
    }

    /// Проверить, какие пути исчезли (после удаления лишних копий).
    pub fn missing(&self, owner: u64, paths: Vec<PathBuf>) {
        self.spawn("missing", move |workers| {
            let paths: Vec<PathBuf> =
                paths.into_iter().filter(|p| std::fs::symlink_metadata(p).is_err()).collect();
            if !paths.is_empty() {
                workers.send(Event::Missing { owner, paths });
            }
        });
    }

    /// Построить план сортировки.
    pub fn sort_scan(
        &self,
        ticket: Ticket,
        options: mh_files_core::sorting::ScanOptions,
        classifier: Arc<mh_files_core::sorting::Classifier>,
    ) -> CancelToken {
        let cancel = CancelToken::default();
        let token = cancel.clone();
        self.spawn("sort-scan", move |workers| {
            let mut last = std::time::Instant::now();
            let plan = sorting::scan(&options, &classifier, token.flag(), &mut |files| {
                if last.elapsed() >= std::time::Duration::from_millis(100) {
                    last = std::time::Instant::now();
                    workers.send(Event::SortScanProgress { ticket, files });
                }
            });
            if let Some(plan) = plan {
                workers.send(Event::SortScanned { ticket, plan });
            }
        });
        cancel
    }

    /// Разложить по плану; журнал — новый файл в папке журналов `history`.
    pub fn sort_run(
        &self,
        ticket: Ticket,
        mut job: sorting::SortJob,
        history: PathBuf,
    ) -> CancelToken {
        let cancel = CancelToken::default();
        let token = cancel.clone();
        self.spawn("sort-run", move |workers| {
            job.journal_path = sorting::new_journal_path(&history);
            let journal = job.journal_path.clone();
            let history = Some(history);
            let mut last = std::time::Instant::now();
            let report = sorting::run_sort(job, token.flag(), &mut |done, total, current| {
                if last.elapsed() >= std::time::Duration::from_millis(80) || done == total {
                    last = std::time::Instant::now();
                    let current = current.to_string();
                    workers.send(Event::SortProgress { ticket, done, total, current });
                }
            });
            if let Some(history) = history {
                sorting::prune(&history, sorting::JOURNAL_KEEP);
            }
            workers.send(Event::SortDone { ticket, report, journal });
        });
        cancel
    }

    /// Отменить операцию сортировщика по её журналу.
    pub fn sort_undo(&self, ticket: Ticket, journal: PathBuf) -> CancelToken {
        let cancel = CancelToken::default();
        let token = cancel.clone();
        self.spawn("sort-undo", move |workers| {
            let report = match sorting::load_journal(&journal) {
                Ok(loaded) if loaded.undone => {
                    let action = mh_files_core::sorting::Action::Undo(loaded.mode);
                    let mut report = mh_files_core::sorting::Report::new(action, 0);
                    report.note = Some("эта операция уже отменена".into());
                    report
                }
                Ok(loaded) => {
                    let mut last = std::time::Instant::now();
                    sorting::run_undo(
                        &journal,
                        loaded,
                        token.flag(),
                        &mut |done, total, current| {
                            if last.elapsed() >= std::time::Duration::from_millis(80) {
                                last = std::time::Instant::now();
                                let current = current.to_string();
                                workers.send(Event::SortProgress { ticket, done, total, current });
                            }
                        },
                    )
                }
                Err(error) => {
                    let mode = mh_files_core::sorting::Mode::Move;
                    let mut report = mh_files_core::sorting::Report::new(
                        mh_files_core::sorting::Action::Undo(mode),
                        0,
                    );
                    report.note = Some(format!("журнал не прочитан: {error}"));
                    report
                }
            };
            workers.send(Event::SortDone { ticket, report, journal });
        });
        cancel
    }

    /// Сортировка по расписанию: план и сразу выполнение всех его строк, с журналом (её
    /// можно отменить). Итог — `SortDone` с этим `ticket`.
    pub fn sort_scheduled(
        &self,
        ticket: Ticket,
        options: mh_files_core::sorting::ScanOptions,
        mode: mh_files_core::sorting::Mode,
        remove_empty: bool,
        classifier: Arc<mh_files_core::sorting::Classifier>,
        history: PathBuf,
    ) {
        self.spawn("sort-scheduled", move |workers| {
            use mh_files_core::sorting::{Action, Report};
            let never = AtomicBool::new(false);
            let refused = |note: String| {
                let mut report = Report::new(Action::Sort(mode), 0);
                report.note = Some(note);
                report
            };
            let (report, journal) =
                if let Some(reason) = sorting::danger_reason(&options.root, options.recursive) {
                    (refused(reason.to_string()), PathBuf::new())
                } else if !options.root.is_dir() {
                    (refused(format!("нет папки {}", options.root.display())), PathBuf::new())
                } else {
                    match sorting::scan(&options, &classifier, &never, &mut |_| {}) {
                        Some(plan) if !plan.moves.is_empty() => {
                            let job = sorting::SortJob {
                                source: plan.root,
                                output: plan.output,
                                mode,
                                remove_empty: remove_empty && options.recursive,
                                moves: plan.moves,
                                journal_path: sorting::new_journal_path(&history),
                            };
                            let journal = job.journal_path.clone();
                            let report = sorting::run_sort(job, &never, &mut |_, _, _| {});
                            sorting::prune(&history, sorting::JOURNAL_KEEP);
                            (report, journal)
                        }
                        _ => (Report::new(Action::Sort(mode), 0), PathBuf::new()),
                    }
                };
            workers.send(Event::SortDone { ticket, report, journal });
        });
    }

    /// Найти последнюю операцию сортировщика, которую можно отменить.
    pub fn sort_last(&self, ticket: Ticket, history: PathBuf) {
        self.spawn("sort-last", move |workers| {
            let last = sorting::last_active(&history);
            workers.send(Event::SortLast { ticket, last });
        });
    }

    /// Записать categories.json (правка категорий в настройках).
    pub fn save_categories(&self, config: mh_files_core::sorting::Config, path: PathBuf) {
        self.spawn("save-categories", move |workers| {
            let result = sorting::replace_categories(&config, &path);
            workers.send(Event::CategoriesSaved { result });
        });
    }

    /// Встраивание в Проводник: `Some((что, включить))` — изменить, `None` — только узнать,
    /// что сейчас есть. Реестр — в фоне.
    pub fn integration(&self, change: Option<(integration::Feature, bool)>) {
        self.spawn("integration", move |workers| {
            let error = change.and_then(|(feature, on)| match std::env::current_exe() {
                Ok(exe) => integration::set(feature, &exe, on).err(),
                Err(error) => Some(error.to_string()),
            });
            workers.send(Event::Integration { status: integration::status(), error });
        });
    }

    /// Проверить обновления на GitHub Releases (по кнопке, один запрос).
    pub fn check_updates(&self) {
        self.spawn("update-check", move |workers| {
            use mh_files_core::update;
            let headers =
                [("Accept", "application/vnd.github+json"), ("X-GitHub-Api-Version", "2022-11-28")];
            let result = mh_files_platform::net::get(
                "api.github.com",
                &update::releases_path(),
                &headers,
                4 << 20,
            )
            .map_err(|error| format!("GitHub недоступен: {error}"))
            .and_then(|body| {
                update::check(&String::from_utf8_lossy(&body), env!("CARGO_PKG_VERSION"))
            });
            workers.send(Event::Update(result));
        });
    }

    /// Найти дубликаты в папках.
    pub fn duplicates(
        &self,
        ticket: Ticket,
        roots: Vec<PathBuf>,
        options: DuplicateOptions,
    ) -> CancelToken {
        let cancel = CancelToken::default();
        let token = cancel.clone();
        self.spawn("duplicates", move |workers| {
            let result = duplicates::find(&roots, options, &token, |progress| {
                workers.send(Event::DuplicatesProgress { ticket, progress });
            });
            if !token.is_cancelled() {
                workers.send(Event::DuplicatesDone { ticket, result });
            }
        });
        cancel
    }

    /// Заменить лишние копии жёсткими ссылками (см. `duplicates::replace_with_links`).
    pub fn link_duplicates(
        &self,
        ticket: Ticket,
        pairs: Vec<(PathBuf, mh_files_core::duplicates::Member, u64)>,
    ) {
        self.spawn("duplicates-link", move |workers| {
            let report = duplicates::replace_with_links(&pairs, &CancelToken::default());
            workers.send(Event::DuplicatesLinked { ticket, report });
        });
    }

    pub fn batch_rename(&self, ticket: Ticket, plan: mh_files_core::rename::Plan) {
        self.spawn("rename", move |workers| {
            let result = rename::apply(&plan);
            workers.send(Event::RenameDone { ticket, result });
        });
    }
}
