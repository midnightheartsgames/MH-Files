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

pub mod images;
pub mod listing;
pub mod preview;
pub mod read;
pub mod rename;
pub mod search;
pub mod shell;
pub mod sizes;
pub mod transfer;
pub mod watch;

pub use images::{ImageKey, ImageKind, ImageResult};
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

    pub fn batch_rename(&self, ticket: Ticket, plan: mh_files_core::rename::Plan) {
        self.spawn("rename", move |workers| {
            let result = rename::apply(&plan);
            workers.send(Event::RenameDone { ticket, result });
        });
    }
}
