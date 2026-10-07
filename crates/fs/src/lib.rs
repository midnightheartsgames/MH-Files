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

use crossbeam_channel::{Receiver, Sender, unbounded};
use mh_files_core::Entry;
use mh_files_platform::Waker;
use mh_files_platform::clipboard::ClipboardFiles;
use mh_files_platform::drives::{DriveInfo, DriveKind};
use mh_files_platform::folders::KnownFolder;
use mh_files_platform::shell::MenuChoice;

pub mod archive;
pub mod duplicates;
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

pub use duplicates::{DuplicateOptions, DuplicateProgress};
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
    /// Есть ли пункт «Открыть в MH Files» в меню Проводника; ошибка — если менять не вышло.
    ExplorerMenu {
        installed: bool,
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

    /// Опрос дисков: сразу список корней, затем каждый диск отдельным потоком.
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

    /// Найти последнюю операцию сортировщика, которую можно отменить.
    pub fn sort_last(&self, ticket: Ticket, history: PathBuf) {
        self.spawn("sort-last", move |workers| {
            let last = sorting::last_active(&history);
            workers.send(Event::SortLast { ticket, last });
        });
    }

    /// Пункт «Открыть в MH Files» в Проводнике: `Some(true)` — добавить, `Some(false)` —
    /// убрать, `None` — только узнать, есть ли. Реестр — в фоне.
    pub fn explorer_menu(&self, change: Option<bool>) {
        use mh_files_platform::integration;
        self.spawn("explorer-menu", move |workers| {
            let error = match change {
                Some(true) => match std::env::current_exe() {
                    Ok(exe) => integration::install_explorer_menu(&exe).err(),
                    Err(error) => Some(error.to_string()),
                },
                Some(false) => integration::uninstall_explorer_menu().err(),
                None => None,
            };
            let installed = integration::explorer_menu_installed();
            workers.send(Event::ExplorerMenu { installed, error });
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

    pub fn batch_rename(&self, ticket: Ticket, plan: mh_files_core::rename::Plan) {
        self.spawn("rename", move |workers| {
            let result = rename::apply(&plan);
            workers.send(Event::RenameDone { ticket, result });
        });
    }
}
