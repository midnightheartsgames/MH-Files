//! Главное окно: состояние программы, разбор событий воркеров, очередь действий, раскладка.
//!
//! Рисование ничего не меняет напрямую: оно складывает [`Action`] в очередь, а после кадра
//! очередь выполняется здесь и в `actions.rs`. Так кнопки, меню, хоткеи и палитра проходят
//! одним путём и не спорят за заимствования.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crossbeam_channel::Receiver;
use eframe::egui::{self, Color32, CornerRadius, Id, Rect, Sense, Stroke, UiBuilder, vec2};
use mh_files_core::Entry;
use mh_files_core::layout::{LayoutNode, MAX_RATIO, MIN_RATIO, PaneId, SplitDirection};
use mh_files_core::listing::LoadState;
use mh_files_core::listing::ViewOptions;
use mh_files_core::location::Location;
use mh_files_core::session::{self, PaneSession, Session, TabSession, ViewMode, WindowGeometry};
use mh_files_core::settings::Settings;
use mh_files_core::sort::SortColumn;
use mh_files_fs::{CancelToken, DirSize, Event, Indexer, Ticket, Workers};
use mh_files_platform::drives::{DriveInfo, DriveKind};
use mh_files_platform::folders::KnownFolder;
use mh_files_platform::ops::{Executor, FileOp, OpEvent};
use mh_files_platform::shell::MenuChoice;

use crate::commands::{CommandId, Keymap};
use crate::images::ImageCache;
use crate::tabs::{Pane, Tab};
use crate::{
    batch, dialogs, inspector, palette, pane_view, quick, settings_window, sidebar, statusbar,
    storage, theme,
};

/// Владельцы результатов, которые не вкладки.
pub const OWNER_INSPECTOR: u64 = u64::MAX - 1;
pub const OWNER_QUICK: u64 = u64::MAX - 2;
pub const OWNER_PALETTE: u64 = u64::MAX - 3;
pub const OWNER_CRUMBS: u64 = u64::MAX - 4;
pub const OWNER_BATCH: u64 = u64::MAX - 5;
pub const OWNER_SIZES: u64 = u64::MAX - 6;
pub const OWNER_COLUMNS: u64 = u64::MAX - 7;
/// Размеры вложенных папок в списке — сами, из индекса.
pub const OWNER_AUTO_SIZES: u64 = u64::MAX - 10;
/// Размер выбранной папки в Инспекторе — сам.
pub const OWNER_INSPECTOR_SIZE: u64 = u64::MAX - 11;

/// Как долго ждать второй шаг последовательности (`Alt+G` → `D`).
const CHORD_TIMEOUT: Duration = Duration::from_millis(1500);

const SESSION_SAVE_EVERY: Duration = Duration::from_secs(5);
/// Индекс изменился — открытая выдача обновляется не чаще этого.
const INDEX_REFRESH_EVERY: Duration = Duration::from_millis(1500);
/// Сколько ждать сохранения снимков индекса при выходе.
const INDEX_SAVE_WAIT: Duration = Duration::from_secs(4);
const STATUS_TIME: Duration = Duration::from_secs(8);
const SPLITTER: f32 = 5.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level {
    Info,
    Error,
}

pub struct StatusMessage {
    pub text: String,
    pub level: Level,
    pub at: Instant,
}

/// Куда открыть место.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Target {
    Current,
    NewTab,
    OtherPane,
    /// Новой панелью во весь левый (`first`) или правый край — бросок папки на край окна.
    EdgePane {
        first: bool,
    },
}

/// Что перетаскивают внутри окна.
#[derive(Debug, Clone)]
pub struct DragFiles {
    pub paths: Vec<PathBuf>,
    /// Тащат из архива: бросок извлекает, а не копирует.
    pub archive: Option<PathBuf>,
    /// Тащат правой кнопкой: при отпускании — меню «Копировать / Переместить / Ярлыки».
    pub right: bool,
    /// Тащат одну папку с диска: на краю окна она открывается новой панелью.
    pub folder: Option<PathBuf>,
}

/// Меню после броска правой кнопкой, как в Проводнике.
pub struct DropMenu {
    pub paths: Vec<PathBuf>,
    pub dest: PathBuf,
    pub archive: Option<PathBuf>,
    pub at: egui::Pos2,
}

/// Куда можно бросить файлы: папка-строка, вкладка, пункт боковой панели.
#[derive(Debug, Clone)]
pub struct DropZone {
    pub rect: Rect,
    pub dir: PathBuf,
    /// Строки внутри панели важнее самой панели.
    pub priority: u8,
    pub kind: ZoneKind,
}

/// Что делает бросок на место.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ZoneKind {
    /// Копировать или переместить в папку `dir`.
    Folder,
    /// Добавить в группу избранного, а не копировать.
    Favorites(usize),
    /// Удалить в корзину.
    RecycleBin,
}

/// Ширины столбцов таблицы (имя занимает остаток).
#[derive(Debug, Clone, Copy)]
pub struct Columns {
    pub modified: f32,
    pub kind: f32,
    pub size: f32,
}

impl Default for Columns {
    fn default() -> Columns {
        Columns { modified: 130.0, kind: 90.0, size: 86.0 }
    }
}

/// Где на экране панель: для броска вкладки (на полосу вкладок — в панель, в край — разбиение).
#[derive(Debug, Clone, Copy)]
pub struct PaneRegion {
    pub pane: PaneId,
    pub strip: Rect,
    pub content: Rect,
}

/// Что перетаскивают: вкладку.
#[derive(Debug, Clone, Copy)]
pub struct DragTab {
    pub pane: PaneId,
    pub tab: u64,
}

/// Куда переносится вкладка.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TabTarget {
    /// В другую панель, последней вкладкой.
    Into(PaneId),
    /// Новой панелью рядом с `pane`.
    Split { pane: PaneId, direction: SplitDirection, first: bool },
}

/// Список соседних папок под стрелкой строки пути.
pub struct CrumbMenu {
    pub pane: PaneId,
    pub dir: PathBuf,
    pub at: egui::Pos2,
    pub names: Option<Vec<String>>,
    pub generation: u64,
}

#[derive(Debug, Clone)]
pub enum Action {
    Run(CommandId),
    Open {
        location: Location,
        target: Target,
    },
    FocusPane(PaneId),
    SelectTab {
        pane: PaneId,
        index: usize,
    },
    CloseTab {
        pane: PaneId,
        index: usize,
    },
    NewTabIn(PaneId),
    SetSort {
        pane: PaneId,
        column: SortColumn,
    },
    SetView(ViewMode),
    CommitRename {
        tab: u64,
        path: PathBuf,
        new_name: String,
    },
    Drop {
        paths: Vec<PathBuf>,
        dest: PathBuf,
        copy: Option<bool>,
    },
    AddFavorites {
        group: usize,
        paths: Vec<PathBuf>,
    },
    Sidebar(sidebar::Edit),
    CrumbMenu {
        pane: PaneId,
        dir: PathBuf,
        at: egui::Pos2,
    },
    Shell(mh_files_fs::ShellJob),
    Status(String, Level),
    MoveTab {
        from: PaneId,
        tab: u64,
        target: TabTarget,
    },
    /// Посчитать размеры этих папок.
    FolderSizes(Vec<PathBuf>),
    RemoveSavedSearch(usize),
    /// Перейти и встать на объект (щелчок в боковой колонке).
    OpenSelect {
        location: Location,
        select: Option<PathBuf>,
    },
}

/// Проверка обновлений по кнопке.
#[derive(Default)]
pub struct UpdateState {
    pub checking: bool,
    pub result: Option<Result<mh_files_core::update::Check, String>>,
}

pub struct FilesApp {
    pub settings: Settings,
    pub keymap: Keymap,
    pub layout: LayoutNode,
    pub panes: Vec<Pane>,
    pub focused: PaneId,
    next_id: u64,
    pub workers: Workers,
    pub indexer: Indexer,
    /// Замеры: запуск, чтение папок.
    pub diag: crate::diag::Diagnostics,
    /// Пути от следующих запусков программы.
    incoming: Option<crossbeam_channel::Receiver<Vec<crate::startup::Target>>>,
    /// Сервер единственной копии.
    _instance: Option<Box<dyn std::any::Any>>,
    /// Прошлый запуск кончился сбоем: отчёт для «Настройки › Система».
    pub previous: crate::crash::Previous,
    /// Проверка обновлений: идёт ли и чем кончилась.
    pub update: UpdateState,
    /// Когда последний раз проверялось расписание сортировки.
    pub schedule_checked: Option<Instant>,
    /// Встраивание в Проводник: что есть (узнаётся в фоне).
    pub integration: Option<mh_files_platform::integration::Status>,
    /// Сортировщик: категории и журналы.
    pub sorter: crate::sorter::Shared,
    /// Идущие извлечения из архивов.
    pub extractions: Vec<crate::archives::ExtractView>,
    pub next_extract: u64,
    /// Содержимое папок для боковых колонок вида «Колонки».
    pub column_cache: crate::columns::Cache,
    events: Receiver<Event>,
    pub ops: Executor,
    pub operations: crate::operations::Operations,
    pub drives: Vec<DriveInfo>,
    /// Сколько в корзине; `None` — ещё не знаем или корзины нет (не Windows).
    pub recycle_bin: Option<mh_files_platform::recycle::BinInfo>,
    /// Окно было в фокусе в прошлом кадре: вернулись в окно — корзину опросить снова.
    was_focused: bool,
    pub places: Vec<(KnownFolder, PathBuf)>,
    pub recent: Vec<PathBuf>,
    pub closed: Vec<TabSession>,
    pub images: ImageCache,
    pub inspector: inspector::State,
    pub quick: Option<quick::State>,
    pub palette: Option<palette::State>,
    pub dialog: Option<dialogs::Dialog>,
    pub batch: Option<batch::State>,
    pub settings_window: settings_window::State,
    pub status: Option<StatusMessage>,
    /// Вырезанное: рисуется бледным до вставки.
    pub cut: HashSet<PathBuf>,
    pub actions: Vec<Action>,
    pub drop_zones: Vec<DropZone>,
    /// Куда бросят, если отпустить сейчас (по прошлому кадру) — для подсветки.
    pub drop_hover: Option<PathBuf>,
    /// Цветные метки файлов и папок.
    pub labels: mh_files_core::labels::Labels,
    /// Файлы из другой программы тянут правой кнопкой: при броске — меню.
    pub external_right: bool,
    pub crumb_menu: Option<CrumbMenu>,
    pub drop_menu: Option<DropMenu>,
    pub columns: Columns,
    /// Первый шаг последовательности клавиш и когда он нажат.
    pub pending_chord: Option<(egui::KeyboardShortcut, Instant)>,
    /// Ctrl+прокрутка, не дотянувшая до шага масштаба, и время последней (`input.time`).
    pub zoom_rest: (f32, f64),
    /// Пункты меню Windows для открытого сейчас контекстного меню.
    pub shell_menu: Option<crate::shell_menu::ShellMenu>,
    /// Меню Windows, заказанное при нажатии правой кнопки, — до отпускания.
    pub shell_prefetch: Option<crate::shell_menu::ShellMenu>,
    pub shell_menu_generation: u64,
    pub shell_menu_heights: crate::shell_menu::Heights,
    /// Расширения меню Windows уже загружены пробным меню после запуска.
    pub shell_warmed: bool,
    /// Посчитанные размеры папок (целиком) и те, что ещё считаются.
    pub folder_sizes: HashMap<PathBuf, DirSize>,
    pub sizes_pending: HashSet<PathBuf>,
    sizes_cancel: Option<CancelToken>,
    /// Размеры вложенных папок открытой папки — из индекса.
    auto_sizes_cancel: Option<CancelToken>,
    sizes_generation: u64,
    pub pane_regions: Vec<PaneRegion>,
    /// Доступность команд на начало кадра: меню рисуются, пока панель вынута из списка.
    availability: HashMap<CommandId, bool>,
    session_json: String,
    session_checked: Instant,
    window: Option<WindowGeometry>,
}

impl FilesApp {
    pub fn new(
        cc: &eframe::CreationContext<'_>,
        settings: Settings,
        session: Option<Session>,
        notice: Option<String>,
        startup: crate::startup::Startup,
    ) -> FilesApp {
        let _ = startup.repaint.set(cc.egui_ctx.clone());
        theme::set_accent(settings.appearance.accent);
        theme::install(&cc.egui_ctx);
        // Двойной щелчок — по настройке Windows, как и переименование вторым щелчком.
        let double_click = crate::pane_view::double_click_time().as_secs_f64();
        cc.egui_ctx.options_mut(|o| o.input_options.max_double_click_delay = double_click);
        cc.egui_ctx.set_zoom_factor(settings.appearance.font_scale);
        set_owner_window(cc);

        let ctx = cc.egui_ctx.clone();
        let (workers, events) = Workers::new(std::sync::Arc::new(move || ctx.request_repaint()));
        let ctx = cc.egui_ctx.clone();
        let ops = Executor::new(std::sync::Arc::new(move || ctx.request_repaint()));
        let (keymap, key_errors) = Keymap::new(&settings.keys);
        let indexer = Indexer::new(workers.clone(), storage::index_dir());
        indexer.configure(&settings.index);

        let mut images = ImageCache::new();
        images.system_icons = settings.appearance.system_icons;
        images.thumbnails_enabled = settings.preview.thumbnails;

        let mut app = FilesApp {
            keymap,
            layout: LayoutNode::Pane(PaneId(1)),
            panes: Vec::new(),
            focused: PaneId(1),
            next_id: 1,
            workers,
            indexer,
            column_cache: Default::default(),
            sorter: crate::sorter::Shared::load(&storage::config_dir()),
            diag: crate::diag::Diagnostics::new(startup.started),
            incoming: startup.incoming,
            _instance: startup.keepalive,
            previous: startup.previous.clone(),
            integration: None,
            update: UpdateState::default(),
            schedule_checked: None,
            extractions: Vec::new(),
            next_extract: 0,
            events,
            ops,
            operations: Default::default(),
            drives: Vec::new(),
            recycle_bin: None,
            was_focused: true,
            places: Vec::new(),
            recent: Vec::new(),
            closed: Vec::new(),
            images,
            inspector: inspector::State::default(),
            quick: None,
            palette: None,
            dialog: None,
            batch: None,
            settings_window: settings_window::State::default(),
            status: None,
            cut: HashSet::new(),
            actions: Vec::new(),
            drop_zones: Vec::new(),
            drop_hover: None,
            labels: startup.labels.clone(),
            external_right: false,
            crumb_menu: None,
            drop_menu: None,
            columns: Columns::default(),
            pending_chord: None,
            zoom_rest: (0.0, 0.0),
            shell_menu: None,
            shell_prefetch: None,
            shell_menu_generation: 0,
            shell_menu_heights: Default::default(),
            shell_warmed: false,
            folder_sizes: HashMap::new(),
            sizes_pending: HashSet::new(),
            sizes_cancel: None,
            auto_sizes_cancel: None,
            sizes_generation: 0,
            pane_regions: Vec::new(),
            availability: HashMap::new(),
            session_json: String::new(),
            session_checked: Instant::now(),
            window: None,
            settings,
        };
        let restored = session.filter(|_| app.settings.panes.restore_session);
        let fresh = restored.is_none();
        app.restore(restored.unwrap_or_else(|| Session::single(app.home_location())));
        app.workers.watch_drives();
        app.workers.known_folders();
        app.workers.recycle_bin();
        app.workers.integration(None);
        // Пути из командной строки: без восстановленного сеанса первый — в домашнюю вкладку.
        app.open_targets_from_outside(startup.open, fresh);
        // Сбой — только паника с отчётом; без отчёта процесс сняли или выключили компьютер.
        if startup.previous.report.is_some() {
            app.set_status(
                "прошлый запуск закончился сбоем — отчёт в «Настройки › Система»",
                Level::Error,
            );
        } else if let Some(old) = &startup.upgraded_from {
            app.set_status(
                format!(
                    "MH Files обновлён с {old} до {} — прежние настройки сохранены в backup",
                    env!("CARGO_PKG_VERSION")
                ),
                Level::Info,
            );
        } else if !startup.missing.is_empty() {
            let list: Vec<String> =
                startup.missing.iter().map(|p| p.display().to_string()).collect();
            app.set_status(format!("не найдено: {}", list.join(", ")), Level::Error);
        } else if !startup.unknown_flags.is_empty() {
            app.set_status(
                format!("незнакомые ключи: {}", startup.unknown_flags.join(" ")),
                Level::Error,
            );
        }
        if let Some(notice) = notice {
            app.set_status(notice, Level::Error);
        } else if !key_errors.is_empty() {
            app.set_status(
                format!("не разобраны сочетания: {}", key_errors.join(", ")),
                Level::Error,
            );
        }
        app
    }

    /// Открыть пути, пришедшие снаружи (командная строка, следующий запуск): папки —
    /// вкладками, архивы — как папки (так их открывает «Открыть с помощью → MH Files»),
    /// остальные файлы — своей папкой с выделением. `replace_first` — первый путь занимает
    /// текущую вкладку вместо новой.
    pub fn open_targets_from_outside(
        &mut self,
        targets: Vec<crate::startup::Target>,
        mut replace_first: bool,
    ) {
        for (path, is_dir) in targets {
            let archive = !is_dir
                && path.file_name().is_some_and(|name| {
                    mh_files_fs::archive::is_archive_name(&name.to_string_lossy())
                });
            if archive {
                let target = if replace_first { Target::Current } else { Target::NewTab };
                replace_first = false;
                self.open_location(
                    Location::Archive { archive: path, inner: String::new() },
                    target,
                );
                continue;
            }
            let (dir, select) = if is_dir {
                (path, None)
            } else {
                match path.parent() {
                    Some(parent) => (parent.to_path_buf(), Some(path.clone())),
                    None => continue,
                }
            };
            let target = if replace_first { Target::Current } else { Target::NewTab };
            replace_first = false;
            self.open_location(Location::Dir(dir), target);
            if let Some(select) = select {
                let tab = self.tab_mut();
                tab.pending_select = Some(select);
                tab.reveal_pending();
            }
        }
    }

    pub fn check_updates(&mut self) {
        if !self.update.checking {
            self.update.checking = true;
            self.workers.check_updates();
            self.set_status("проверяю обновления…", Level::Info);
        }
    }

    fn on_update(&mut self, result: Result<mh_files_core::update::Check, String>) {
        use mh_files_core::update::Check;
        self.update.checking = false;
        match &result {
            Ok(Check::UpToDate) => self.set_status(
                format!("установлена последняя версия — {}", env!("CARGO_PKG_VERSION")),
                Level::Info,
            ),
            Ok(Check::Available(release)) => self.set_status(
                format!("доступна MH Files {} — «Настройки › О программе»", release.version),
                Level::Info,
            ),
            Err(error) => {
                self.set_status(format!("обновления не проверены: {error}"), Level::Error)
            }
        }
        self.update.result = Some(result);
    }

    pub fn home_location(&self) -> Location {
        mh_files_platform::folders::known_folder(KnownFolder::Home)
            .map_or(Location::Computer, Location::Dir)
    }

    pub fn new_id(&mut self) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        id
    }

    pub fn view_options(&self) -> ViewOptions {
        ViewOptions {
            sort: Default::default(),
            folders_first: self.settings.files.folders_first,
            show_hidden: self.settings.files.show_hidden,
            show_system: self.settings.files.show_system,
        }
    }

    /// Новая вкладка, сразу загружающая место.
    pub fn make_tab(&mut self, session: &TabSession) -> Tab {
        let id = self.new_id();
        let options = ViewOptions { sort: session.sort, ..self.view_options() };
        let mut tab = Tab::new(id, session.location.clone(), session.view, options);
        tab.duplicate_options = self.duplicate_options();
        tab.reload(&self.workers, false);
        tab
    }

    fn restore(&mut self, session: Session) {
        self.layout = session.layout.clone();
        self.focused = session.focused;
        self.recent = session.recent.clone();
        self.window = session.window;
        let max_pane = self.layout.panes().iter().map(|p| p.0).max().unwrap_or(0);
        self.next_id = max_pane + 1;
        for pane in &session.panes {
            let tabs = pane.tabs.iter().map(|tab| self.make_tab(tab)).collect();
            self.panes.push(Pane { id: pane.id, tabs, active: pane.active });
        }
    }

    pub fn session(&self) -> Session {
        Session {
            version: session::SESSION_VERSION,
            layout: self.layout.clone(),
            panes: self
                .panes
                .iter()
                .map(|pane| PaneSession {
                    id: pane.id,
                    tabs: pane.tabs.iter().map(Tab::session).collect(),
                    active: pane.active,
                })
                .collect(),
            focused: self.focused,
            window: self.window,
            recent: self.recent.clone(),
        }
    }

    pub fn pane(&self, id: PaneId) -> Option<&Pane> {
        self.panes.iter().find(|pane| pane.id == id)
    }

    pub fn pane_mut(&mut self, id: PaneId) -> Option<&mut Pane> {
        self.panes.iter_mut().find(|pane| pane.id == id)
    }

    pub fn focused_pane(&self) -> &Pane {
        self.pane(self.focused).unwrap_or(&self.panes[0])
    }

    pub fn focused_pane_mut(&mut self) -> &mut Pane {
        let id = self.focused;
        let index = self.panes.iter().position(|pane| pane.id == id).unwrap_or(0);
        &mut self.panes[index]
    }

    pub fn tab(&self) -> &Tab {
        self.focused_pane().tab()
    }

    pub fn tab_mut(&mut self) -> &mut Tab {
        self.focused_pane_mut().tab_mut()
    }

    pub fn tab_by_id(&mut self, id: u64) -> Option<&mut Tab> {
        self.panes.iter_mut().flat_map(|pane| pane.tabs.iter_mut()).find(|tab| tab.id == id)
    }

    /// Соседняя панель (следующая в обходе), если панелей больше одной.
    pub fn other_pane(&self) -> Option<PaneId> {
        let next = self.layout.next_pane(self.focused, false);
        (next != self.focused).then_some(next)
    }

    pub fn set_status(&mut self, text: impl Into<String>, level: Level) {
        self.status = Some(StatusMessage { text: text.into(), level, at: Instant::now() });
    }

    pub fn remember(&mut self, location: &Location) {
        if let Location::Dir(path) = location {
            session::remember(&mut self.recent, path.clone());
        }
    }

    /// Перечитать вкладки, показывающие эти папки.
    pub fn reload_dirs(&mut self, dirs: &[PathBuf]) {
        self.indexer.refresh(dirs);
        self.column_cache.invalidate(dirs);
        // Результаты дубликатов: проверить, не удалили ли что-то из них.
        for tab in self.panes.iter().flat_map(|pane| pane.tabs.iter()) {
            if let Some(view) = &tab.duplicates {
                let paths: Vec<PathBuf> = view
                    .group_of
                    .keys()
                    .filter(|p| p.parent().is_some_and(|parent| dirs.iter().any(|d| d == parent)))
                    .cloned()
                    .collect();
                if !paths.is_empty() {
                    self.workers.missing(tab.id, paths);
                }
            }
        }
        let workers = self.workers.clone();
        for tab in self.panes.iter_mut().flat_map(|pane| pane.tabs.iter_mut()) {
            // Архив перечитывается, если менялось что-то в нём (запись в zip).
            let archive_touched = match &tab.location {
                Location::Archive { archive, .. } => dirs.iter().any(|d| d.starts_with(archive)),
                _ => false,
            };
            if archive_touched || tab.dir().is_some_and(|dir| dirs.contains(&dir)) {
                tab.reload(&workers, true);
            }
        }
    }

    // ── События воркеров ─────────────────────────────────────────────────────────────

    fn poll(&mut self, ctx: &egui::Context) {
        let incoming: Vec<Vec<crate::startup::Target>> =
            self.incoming.as_ref().map(|rx| rx.try_iter().collect()).unwrap_or_default();
        for targets in incoming {
            // Следующий запуск без путей — просто показать окно.
            self.open_targets_from_outside(targets, false);
            mh_files_platform::window::bring_to_front();
            ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
        }
        let events: Vec<Event> = self.events.try_iter().collect();
        for event in events {
            self.on_event(ctx, event);
        }
        let op_events: Vec<OpEvent> = self.ops.events().try_iter().collect();
        for event in op_events {
            self.on_op(event);
        }
    }

    fn on_event(&mut self, ctx: &egui::Context, event: Event) {
        let workers = self.workers.clone();
        match event {
            Event::Listing { ticket, batch } if ticket.owner == OWNER_COLUMNS => {
                self.column_cache.on_batch(ticket.generation, batch);
            }
            Event::ListingDone { ticket, result } if ticket.owner == OWNER_COLUMNS => {
                self.column_cache.on_done(ticket.generation, result);
            }
            Event::Listing { ticket, batch } => {
                if let Some(tab) = self.tab_by_id(ticket.owner) {
                    tab.on_batch(ticket, batch);
                }
            }
            Event::ListingDone { ticket, result } => {
                let sizes = &self.folder_sizes;
                if let Some(tab) = self
                    .panes
                    .iter_mut()
                    .flat_map(|p| p.tabs.iter_mut())
                    .find(|t| t.id == ticket.owner)
                {
                    tab.on_done(ticket, result);
                    // Посчитанные раньше размеры папок — снова в столбец.
                    if !sizes.is_empty() {
                        apply_sizes(tab, sizes);
                    }
                    // Прочитанная папка заодно освежает индекс и попадает в замеры.
                    if let Some(dir) = tab.dir()
                        && tab.listing.state == LoadState::Done
                    {
                        self.indexer.observe(&dir, tab.listing.all());
                        let took = tab.load_started.elapsed();
                        self.diag.listing(dir, tab.listing.total_len(), took);
                        // Размеры вложенных папок — из индекса, без обращения к диску.
                        if self.settings.files.auto_folder_sizes {
                            let dirs: Vec<PathBuf> = tab
                                .listing
                                .all()
                                .iter()
                                .filter(|e| e.is_dir() && !e.attributes.reparse())
                                .map(Entry::path)
                                .collect();
                            if !dirs.is_empty() {
                                let ticket = Ticket { owner: OWNER_AUTO_SIZES, generation: 0 };
                                if let Some(cancel) = self.auto_sizes_cancel.take() {
                                    cancel.cancel();
                                }
                                self.auto_sizes_cancel =
                                    Some(self.indexer.folder_sizes(ticket, dirs, false));
                            }
                        }
                    }
                }
            }
            Event::Patch { ticket, upserts, removes, reload } => {
                if let Some(tab) = self.tab_by_id(ticket.owner)
                    && tab.on_patch(ticket, upserts, removes, reload)
                {
                    tab.reload(&workers, true);
                }
            }
            Event::Search { ticket, batch, scanned } => {
                if let Some(tab) = self.tab_by_id(ticket.owner) {
                    tab.on_search(ticket, batch, scanned, false);
                }
            }
            Event::SearchDone { ticket, result, scanned } => {
                if let Some(tab) = self.tab_by_id(ticket.owner) {
                    tab.on_search_done(ticket, result, scanned);
                }
            }
            Event::Preview { ticket, path, preview } => match ticket.owner {
                OWNER_INSPECTOR => self.inspector.on_preview(ctx, ticket, path, preview),
                OWNER_QUICK => {
                    if let Some(quick) = &mut self.quick {
                        quick.on_preview(ctx, ticket, path, preview);
                    }
                }
                _ => {}
            },
            Event::Completion { ticket, dir, names } => match ticket.owner {
                OWNER_PALETTE => {
                    if let Some(palette) = &mut self.palette {
                        palette.on_completion(ticket, dir, names);
                    }
                }
                OWNER_CRUMBS => {
                    if let Some(menu) = &mut self.crumb_menu
                        && menu.generation == ticket.generation
                    {
                        menu.names = Some(names);
                    }
                }
                _ => {}
            },
            Event::DriveRoots(roots) => {
                let old = std::mem::take(&mut self.drives);
                // Диск подключили или отключили (не первый список при запуске) — и индекс
                // поиска пересобирает тома сразу, а не при своём опросе.
                let changed = !old.is_empty()
                    && (old.len() != roots.len()
                        || old.iter().zip(&roots).any(|(a, (root, _))| &a.root != root));
                if changed {
                    self.indexer.drives_changed();
                }
                self.drives = roots
                    .into_iter()
                    .map(|(root, kind)| {
                        old.iter()
                            .find(|d| d.root == root)
                            .cloned()
                            .unwrap_or_else(|| DriveInfo::pending(root, kind))
                    })
                    .collect();
            }
            Event::Drive(info) => {
                if let Some(slot) = self.drives.iter_mut().find(|d| d.root == info.root) {
                    *slot = info;
                }
            }
            Event::KnownFolders(places) => self.places = places,
            Event::RecycleBin(info) => {
                // Корзина изменилась (удалили, восстановили, очистили — здесь или в Проводнике):
                // её вкладки перечитываются.
                if info != self.recycle_bin {
                    self.reload_recycle_tabs();
                }
                self.recycle_bin = info;
            }
            Event::ZipAdded { archive, result } => self.on_zip_added(archive, result),
            Event::RecycleListing { ticket, items } => {
                if let Some(tab) = self.tab_by_id(ticket.owner) {
                    tab.on_recycled(ticket, items);
                }
            }
            Event::Image { key, result } => self.images.on_result(ctx, key, result),
            Event::ThumbnailTypes(types) => self.images.on_types(types),
            Event::Shell { what, result } => {
                if let Err(error) = result {
                    self.set_status(format!("{what}: {error}"), Level::Error);
                }
            }
            Event::Paste { dest, files } => self.paste_files(dest, files),
            Event::RenameDone { ticket, result } => {
                if ticket.generation == 0 {
                    self.on_batch_done(result.is_ok());
                }
                match result {
                    Ok(count) => self.set_status(format!("переименовано: {count}"), Level::Info),
                    Err(error) => {
                        self.set_status(format!("переименование отменено: {error}"), Level::Error)
                    }
                }
                self.reload_all_dirs();
            }
            Event::Preflight { transfer, conflicts } => self.on_preflight(transfer, conflicts),
            Event::FolderSize { ticket, path, size } => self.on_folder_size(ticket, path, size),
            Event::Menu { paths, choice } => self.on_menu(paths, choice),
            Event::ShellMenu { generation, items, complete, commands } => {
                crate::shell_menu::on_ready(self, ctx, generation, items, complete, commands);
            }
            Event::IndexResults { ticket, result } => {
                if let Some(tab) = self.tab_by_id(ticket.owner) {
                    tab.on_index_results(ticket, result);
                }
            }
            Event::ExtractProgress { id, done, total } => self.on_extract_progress(id, done, total),
            Event::SortScanProgress { ticket, files } => self.on_sort_scan_progress(ticket, files),
            Event::SortScanned { ticket, plan } => self.on_sort_scanned(ticket, plan),
            Event::SortProgress { ticket, done, total, current } => {
                self.on_sort_progress(ticket, done, total, current)
            }
            Event::SortDone { ticket, report, journal } => {
                self.on_sort_done(ticket, report, journal)
            }
            Event::SortLast { ticket, last } => self.on_sort_last(ticket, last),
            Event::CategoriesSaved { result } => match result {
                Ok(None) => self.set_status("категории сохранены", Level::Info),
                Ok(Some(copy)) => self.set_status(
                    format!("категории сохранены; прежний файл с ошибкой — {}", copy.display()),
                    Level::Info,
                ),
                Err(error) => self.set_status(error, Level::Error),
            },
            Event::Update(result) => self.on_update(result),
            Event::Integration { status, error } => {
                self.integration = Some(status);
                if let Some(error) = error {
                    self.set_status(error, Level::Error);
                }
            }
            Event::Missing { owner, paths } => {
                if let Some(tab) = self.tab_by_id(owner) {
                    tab.forget_paths(&paths);
                }
            }
            Event::Extracted { extraction, result } => self.on_extracted(extraction, result),
            Event::DuplicatesProgress { ticket, progress } => {
                if let Some(tab) = self.tab_by_id(ticket.owner) {
                    tab.on_duplicates_progress(ticket, progress);
                }
            }
            Event::DuplicatesLinked { ticket, report } => {
                let level = if report.failed.is_empty() { Level::Info } else { Level::Error };
                let mut text = format!(
                    "жёсткими ссылками заменено {} — освобождено {}",
                    mh_files_core::format::items(report.linked),
                    mh_files_core::format::size(report.freed)
                );
                if let Some(first) = report.failed.first() {
                    text.push_str(&format!(
                        "; не вышло: {} (первое: {first})",
                        report.failed.len()
                    ));
                }
                self.set_status(text, level);
                let workers = self.workers.clone();
                if let Some(tab) = self.tab_by_id(ticket.owner) {
                    tab.reload(&workers, false);
                }
            }
            Event::DuplicatesDone { ticket, result } => {
                if let Some(tab) = self.tab_by_id(ticket.owner) {
                    tab.on_duplicates_done(ticket, result);
                }
            }
            Event::IndexChanged { content } => {
                if content {
                    for tab in self.panes.iter_mut().flat_map(|pane| pane.tabs.iter_mut()) {
                        if let Some(view) = &mut tab.index {
                            view.stale = true;
                        }
                    }
                }
            }
        }
    }

    /// Запустить нужные поиски по индексу. Видимая выдача обновляется после изменений
    /// индекса, но не чаще [`INDEX_REFRESH_EVERY`].
    fn drive_index_tabs(&mut self, ctx: &egui::Context) {
        let show_hidden = self.settings.files.show_hidden;
        let mut waiting = false;
        for pane in &mut self.panes {
            let active = pane.active;
            for (index, tab) in pane.tabs.iter_mut().enumerate() {
                let Some(view) = &mut tab.index else { continue };
                if view.stale && index == active && !view.searching && !view.text.trim().is_empty()
                {
                    if view.last.elapsed() >= INDEX_REFRESH_EVERY {
                        view.pending = true;
                    } else {
                        waiting = true;
                    }
                }
                tab.start_index_search(&self.indexer, show_hidden);
            }
        }
        if waiting {
            ctx.request_repaint_after(INDEX_REFRESH_EVERY);
        }
    }

    /// Посчитать размеры папок в фоне; прежний подсчёт отменяется.
    pub fn request_folder_sizes(&mut self, dirs: Vec<PathBuf>) {
        if dirs.is_empty() {
            return;
        }
        if let Some(cancel) = self.sizes_cancel.take() {
            cancel.cancel();
        }
        self.sizes_generation += 1;
        self.sizes_pending = dirs.iter().cloned().collect();
        let ticket = Ticket { owner: OWNER_SIZES, generation: self.sizes_generation };
        self.sizes_cancel = Some(self.workers.folder_sizes(ticket, dirs));
    }

    /// Размер папки посчитан — по Ctrl+Shift+S, сам из индекса для списка или для
    /// Инспектора. Размер верен в любом случае; поколение важно только для «считается…».
    fn on_folder_size(&mut self, ticket: Ticket, path: PathBuf, size: DirSize) {
        if ticket.owner == OWNER_SIZES && ticket.generation == self.sizes_generation {
            self.sizes_pending.remove(&path);
        }
        if ticket.owner == OWNER_INSPECTOR_SIZE {
            self.inspector.size_done(&path);
        }
        self.folder_sizes.insert(path.clone(), size);
        for tab in self.panes.iter_mut().flat_map(|pane| pane.tabs.iter_mut()) {
            if tab.listing.set_dir_size(&path, size.bytes) {
                // Пачка размеров из индекса — пересортировка одна, в ближайший такт.
                tab.mark_dirty();
            }
        }
    }

    /// Меню Windows закрылось: «Переименовать» делаем своим переименованием в строке,
    /// остальное могло поменять папку — перечитать.
    fn on_menu(&mut self, paths: Vec<PathBuf>, choice: Result<MenuChoice, String>) {
        match choice {
            Ok(MenuChoice::Dismissed) => {}
            Ok(MenuChoice::Invoked { verb }) => {
                if verb.as_deref() == Some("rename")
                    && let Some(path) = paths.first()
                    && self.tab().listing.row_of(path).is_some()
                {
                    self.tab_mut().selection.select_only(path.clone());
                    self.actions.push(Action::Run(CommandId::Rename));
                }
                let mut dirs: Vec<PathBuf> =
                    paths.iter().filter_map(|p| p.parent().map(PathBuf::from)).collect();
                dirs.extend(paths.iter().cloned());
                self.reload_dirs(&dirs);
            }
            Err(error) => self.set_status(error, Level::Error),
        }
    }

    /// Первый шаг последовательности ещё ждёт второй?
    pub fn chord_pending(&mut self) -> Option<egui::KeyboardShortcut> {
        match self.pending_chord {
            Some((first, at)) if at.elapsed() < CHORD_TIMEOUT => Some(first),
            Some(_) => {
                self.pending_chord = None;
                None
            }
            None => None,
        }
    }

    fn reload_all_dirs(&mut self) {
        let workers = self.workers.clone();
        for tab in self.panes.iter_mut().flat_map(|pane| pane.tabs.iter_mut()) {
            if tab.dir().is_some() {
                tab.reload(&workers, true);
            }
        }
    }

    // ── Кадр ─────────────────────────────────────────────────────────────────────────

    fn frame(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        self.diag.frame();
        self.poll(&ctx);
        // Корзину могли очистить или пополнить в Проводнике, пока окно было в фоне.
        let focused = ctx.input(|i| i.viewport().focused.unwrap_or(true));
        if focused && !self.was_focused {
            self.workers.recycle_bin();
        }
        self.was_focused = focused;
        crate::popup::begin_frame(&ctx);
        self.handle_keys(&ctx);
        self.run_actions(&ctx);
        self.drop_zones.clear();
        self.pane_regions.clear();
        self.availability =
            CommandId::ALL.iter().map(|&command| (command, self.available(command))).collect();

        if self.settings.appearance.custom_title_bar {
            egui::Panel::top("titlebar")
                .exact_size(crate::titlebar::HEIGHT)
                .frame(egui::Frame::new().fill(theme::PANEL))
                .show(ui, |ui| crate::titlebar::show(ui, self));
        } else {
            mh_files_platform::window::set_maximize_button(None);
        }

        egui::Panel::bottom("status")
            .exact_size(28.0)
            .frame(
                egui::Frame::new().fill(theme::PANEL).inner_margin(egui::Margin::symmetric(10, 4)),
            )
            .show(ui, |ui| statusbar::show(ui, self));

        if self.settings.panes.show_sidebar {
            let response = egui::Panel::left("sidebar")
                .resizable(true)
                .default_size(self.settings.panes.sidebar_width)
                .size_range(150.0..=480.0)
                .frame(
                    egui::Frame::new()
                        .fill(theme::PANEL)
                        .inner_margin(egui::Margin::symmetric(8, 10)),
                )
                .show(ui, |ui| sidebar::show(ui, self));
            self.settings.panes.sidebar_width = response.response.rect.width();
        }

        if self.settings.panes.show_inspector {
            let response = egui::Panel::right("inspector")
                .resizable(true)
                .default_size(self.settings.panes.inspector_width)
                .size_range(200.0..=600.0)
                .frame(
                    egui::Frame::new()
                        .fill(theme::PANEL)
                        .inner_margin(egui::Margin::symmetric(12, 10)),
                )
                .show(ui, |ui| inspector::show(ui, self));
            self.settings.panes.inspector_width = response.response.rect.width();
        }

        egui::CentralPanel::default()
            .frame(egui::Frame::new().fill(theme::BACKGROUND).inner_margin(egui::Margin::same(4)))
            .show(ui, |ui| {
                let rect = ui.available_rect_before_wrap();
                let mut layout = std::mem::replace(&mut self.layout, LayoutNode::Pane(PaneId(0)));
                self.show_layout(ui, &mut layout, rect);
                self.layout = layout;
            });

        pane_view::crumb_menu(&ctx, self);
        self.show_drop_menu(&ctx);
        if let Some(mut palette) = self.palette.take()
            && palette::show(&ctx, self, &mut palette)
        {
            self.palette = Some(palette);
        }
        if self.quick.is_some() {
            quick::show(&ctx, self);
        }
        if self.batch.is_some() {
            batch::show(&ctx, self);
        }
        if self.dialog.is_some() {
            dialogs::show(&ctx, self);
        }
        settings_window::show(&ctx, self);
        crate::shell_menu::end_frame(self);
        crate::popup::end_frame(&ctx);
        crate::shell_menu::warm_up(&ctx, self);

        self.handle_drops(&ctx);
        self.run_actions(&ctx);
        self.drive_index_tabs(&ctx);
        self.drive_sorters();
        self.run_schedules(&ctx);
        if self.settings.appearance.custom_title_bar {
            crate::titlebar::borders(&ctx);
        }

        let mut busy = false;
        for tab in self.panes.iter_mut().flat_map(|pane| pane.tabs.iter_mut()) {
            busy |= tab.tick();
        }
        if self.pending_chord.is_some() {
            ctx.request_repaint_after(CHORD_TIMEOUT);
        }
        if busy || self.operations.is_busy() {
            ctx.request_repaint_after(Duration::from_millis(120));
        }
        if self.status.as_ref().is_some_and(|s| s.at.elapsed() > STATUS_TIME) {
            self.status = None;
        }
        inspector::follow(self);
        inspector::sync_host(&ctx, self);
        self.images.end_frame(&self.workers);
        self.track_window(&ctx);
        self.save_session_if_changed(false);
    }

    /// Рисует дерево раскладки: панели и перетаскиваемые разделители.
    fn show_layout(&mut self, ui: &mut egui::Ui, node: &mut LayoutNode, rect: Rect) {
        match node {
            LayoutNode::Pane(id) => {
                let id = *id;
                let Some(index) = self.panes.iter().position(|pane| pane.id == id) else { return };
                let focused = self.focused == id && self.panes.len() > 1;
                let strip_height = crate::pane_view::TAB_HEIGHT;
                self.pane_regions.push(PaneRegion {
                    pane: id,
                    strip: Rect::from_min_size(rect.min, vec2(rect.width(), strip_height)),
                    content: Rect::from_min_max(rect.min + vec2(0.0, strip_height), rect.max),
                });
                let mut child = ui.new_child(
                    UiBuilder::new()
                        .max_rect(rect)
                        .id_salt(("pane", id.0))
                        .layout(egui::Layout::top_down(egui::Align::Min)),
                );
                child.set_clip_rect(rect.intersect(ui.clip_rect()));
                // Любой щелчок в панели делает её активной.
                if child.input(|i| i.pointer.any_pressed())
                    && child.rect_contains_pointer(rect)
                    && self.focused != id
                {
                    self.actions.push(Action::FocusPane(id));
                }
                let mut pane = std::mem::replace(
                    &mut self.panes[index],
                    Pane { id, tabs: Vec::new(), active: 0 },
                );
                pane_view::show(&mut child, &mut pane, self, focused || self.focused == id);
                self.panes[index] = pane;
                if focused {
                    ui.painter().rect_stroke(
                        rect,
                        CornerRadius::same(4),
                        Stroke::new(1.0, theme::accent().gamma_multiply(0.5)),
                        egui::StrokeKind::Inside,
                    );
                }
            }
            LayoutNode::Split { direction, ratio, first, second } => {
                let horizontal = *direction == SplitDirection::Horizontal;
                let total = if horizontal { rect.width() } else { rect.height() };
                let first_size = (total - SPLITTER) * *ratio;
                let (a, splitter, b) = if horizontal {
                    let a = Rect::from_min_size(rect.min, vec2(first_size, rect.height()));
                    let s = Rect::from_min_size(
                        egui::pos2(a.right(), rect.top()),
                        vec2(SPLITTER, rect.height()),
                    );
                    let b = Rect::from_min_max(egui::pos2(s.right(), rect.top()), rect.max);
                    (a, s, b)
                } else {
                    let a = Rect::from_min_size(rect.min, vec2(rect.width(), first_size));
                    let s = Rect::from_min_size(
                        egui::pos2(rect.left(), a.bottom()),
                        vec2(rect.width(), SPLITTER),
                    );
                    let b = Rect::from_min_max(egui::pos2(rect.left(), s.bottom()), rect.max);
                    (a, s, b)
                };
                let id = Id::new(("splitter", first.panes().first().map(|p| p.0)));
                let response = ui.interact(splitter, id, Sense::drag());
                if response.dragged()
                    && let Some(pointer) = response.interact_pointer_pos()
                {
                    let offset =
                        if horizontal { pointer.x - rect.left() } else { pointer.y - rect.top() };
                    *ratio = (offset / total).clamp(MIN_RATIO, MAX_RATIO);
                }
                if response.hovered() || response.dragged() {
                    ui.ctx().set_cursor_icon(if horizontal {
                        egui::CursorIcon::ResizeHorizontal
                    } else {
                        egui::CursorIcon::ResizeVertical
                    });
                }
                let color = if response.hovered() || response.dragged() {
                    theme::accent().gamma_multiply(0.6)
                } else {
                    Color32::TRANSPARENT
                };
                ui.painter().rect_filled(splitter.shrink(1.0), CornerRadius::same(2), color);
                self.show_layout(ui, first, a);
                self.show_layout(ui, second, b);
            }
        }
    }

    // ── Перетаскивание ───────────────────────────────────────────────────────────────

    fn zone_at(&self, pos: egui::Pos2) -> Option<&DropZone> {
        self.drop_zones
            .iter()
            .filter(|zone| zone.rect.contains(pos))
            .max_by_key(|zone| zone.priority)
    }

    fn handle_drops(&mut self, ctx: &egui::Context) {
        self.handle_tab_drop(ctx);
        let pointer = ctx.input(|i| i.pointer.hover_pos());
        let dragging = egui::DragAndDrop::has_payload_of_type::<DragFiles>(ctx);
        if dragging && self.drag_left_window(ctx) {
            return;
        }
        // Файлы из Проводника и других программ. Пока их тянут, мышь захвачена источником:
        // окну не приходят ни движения мыши, ни клавиши — где курсор и что нажато,
        // спрашивается у системы.
        let hovering = ctx.input(|i| !i.raw.hovered_files.is_empty());
        let dropped: Vec<PathBuf> = ctx.input(|i| {
            i.raw
                .dropped_files
                .iter()
                .map(|f| f.path().to_path_buf())
                .filter(|p| !p.as_os_str().is_empty())
                .collect()
        });
        let external = !dragging && (hovering || !dropped.is_empty());
        let outside_pointer = if external {
            let ppp = ctx.pixels_per_point();
            mh_files_platform::window::cursor_in_window()
                .map(|(x, y)| egui::pos2(x / ppp, y / ppp))
                .or(pointer)
        } else {
            None
        };
        if hovering {
            // Пока тянут, событий нет — подсветка цели следит за курсором сама.
            ctx.request_repaint_after(std::time::Duration::from_millis(30));
            let keys = mh_files_platform::window::drag_keys();
            // Правая кнопка к моменту броска уже отпущена — запомнить, пока тянут.
            self.external_right |= keys.right;
        }
        let at = if external { outside_pointer } else { pointer };
        self.drop_hover = if dragging || hovering {
            at.and_then(|pos| self.zone_at(pos)).map(|zone| zone.dir.clone())
        } else {
            None
        };
        if !dropped.is_empty() {
            let right = std::mem::take(&mut self.external_right);
            let at = at.or_else(|| ctx.input(|i| i.pointer.latest_pos()));
            match at.and_then(|pos| self.zone_at(pos).cloned().map(|zone| (pos, zone))) {
                Some((pos, zone)) if right && zone.kind == ZoneKind::Folder => {
                    self.drop_menu =
                        Some(DropMenu { paths: dropped, dest: zone.dir, archive: None, at: pos });
                }
                Some((_, zone)) => self.drop_on(zone, dropped, ctx),
                None => self.set_status("бросьте файлы на папку или панель", Level::Info),
            }
        } else if !hovering {
            self.external_right = false;
        }
        // Одну папку бросают на левый или правый край окна — она открывается новой панелью.
        let edge = match (egui::DragAndDrop::payload::<DragFiles>(ctx), pointer) {
            (Some(payload), Some(pos)) if payload.folder.is_some() && !payload.right => {
                self.edge_target(pos)
            }
            _ => None,
        };
        if edge.is_some() {
            self.drop_hover = None;
        }
        if dragging
            && ctx.input(|i| i.pointer.any_released())
            && let Some((first, _)) = edge
            && let Some(folder) =
                egui::DragAndDrop::take_payload::<DragFiles>(ctx).and_then(|p| p.folder.clone())
        {
            self.actions.push(Action::Open {
                location: Location::Dir(folder),
                target: Target::EdgePane { first },
            });
            return;
        }
        if dragging && ctx.input(|i| i.pointer.any_released()) {
            let payload = egui::DragAndDrop::take_payload::<DragFiles>(ctx);
            if let (Some(payload), Some(pos)) = (payload, pointer)
                && let Some(zone) = self.zone_at(pos).cloned()
            {
                if payload.right && zone.kind == ZoneKind::Folder {
                    self.drop_menu = Some(DropMenu {
                        paths: payload.paths.clone(),
                        dest: zone.dir,
                        archive: payload.archive.clone(),
                        at: pos,
                    });
                    return;
                }
                match (&payload.archive, zone.kind) {
                    (Some(archive), ZoneKind::Folder) => {
                        self.extract_drop(archive.clone(), payload.paths.clone(), zone.dir)
                    }
                    // Внутри архива удалять нечего: файлов на диске нет.
                    (Some(_), ZoneKind::RecycleBin) => {}
                    _ => self.drop_on(zone, payload.paths.clone(), ctx),
                }
            }
        }
        if dragging
            && let Some(pos) = pointer
            && let Some(payload) = egui::DragAndDrop::payload::<DragFiles>(ctx)
        {
            if let Some((_, rect)) = edge {
                paint_drop_area(ctx, Id::new("edge-drop"), rect);
            }
            let hint = self.drag_hint(ctx, &payload, edge.map(|(first, _)| first));
            drag_label(ctx, pos, &hint);
        }
    }

    /// Край окна, на который можно бросить папку: `true` — левый. С прямоугольником, где
    /// встанет новая панель.
    fn edge_target(&self, pos: egui::Pos2) -> Option<(bool, Rect)> {
        let area =
            self.pane_regions.iter().map(|r| r.strip.union(r.content)).reduce(|a, b| a.union(b))?;
        // Узкая полоса: дальше от края — обычный бросок в папку под курсором.
        let edge = (area.width() * 0.04).clamp(24.0, 40.0);
        if !area.contains(pos) {
            return None;
        }
        let first = if pos.x < area.left() + edge {
            true
        } else if pos.x > area.right() - edge {
            false
        } else {
            return None;
        };
        let share = 1.0 / (self.layout.columns(SplitDirection::Horizontal) + 1) as f32;
        let width = area.width() * share;
        let rect = if first {
            Rect::from_min_max(area.min, egui::pos2(area.left() + width, area.bottom()))
        } else {
            Rect::from_min_max(egui::pos2(area.right() - width, area.top()), area.max)
        };
        Some((first, rect))
    }

    /// Подсказка у курсора: что случится, если отпустить здесь.
    fn drag_hint(&self, ctx: &egui::Context, payload: &DragFiles, edge: Option<bool>) -> String {
        use mh_files_core::location::path_label;
        if let (Some(first), Some(folder)) = (edge, &payload.folder) {
            let side = if first { "слева" } else { "справа" };
            return format!("Открыть «{}» новой панелью {side}", short(&path_label(folder)));
        }
        let Some(zone) = self.drop_hover.as_ref().and_then(|dir| {
            self.drop_zones.iter().filter(|z| &z.dir == dir).max_by_key(|z| z.priority)
        }) else {
            return "Наведите на папку или панель".into();
        };
        let what = match payload.paths.len() {
            1 => String::new(),
            n => format!(" {}", mh_files_core::format::items(n)),
        };
        match zone.kind {
            ZoneKind::Folder => {}
            ZoneKind::Favorites(_) => return "Добавить в избранное".into(),
            ZoneKind::RecycleBin if payload.archive.is_some() => {
                return "Из архива в корзину нельзя".into();
            }
            ZoneKind::RecycleBin => return format!("Удалить{what} в корзину"),
        }
        let name = short(&path_label(&zone.dir));
        if payload.archive.is_some() {
            return format!("Извлечь{what} в «{name}»");
        }
        if payload.paths.iter().all(|p| p.parent() == Some(zone.dir.as_path())) {
            return format!("Уже в «{name}»");
        }
        if payload.paths.iter().any(|p| zone.dir.starts_with(p)) {
            return "Папку нельзя положить в саму себя".into();
        }
        if payload.right {
            return format!("В «{name}»: копировать, переместить или ярлыки");
        }
        if crate::actions::writable_zip(&zone.dir).is_some() {
            return format!("Добавить{what} в архив «{name}»");
        }
        let modifiers = ctx.input(|i| i.modifiers);
        let action = if modifiers.ctrl && modifiers.shift || modifiers.alt {
            "Создать ярлыки"
        } else if modifiers.ctrl {
            "Копировать"
        } else if modifiers.shift {
            "Переместить"
        } else if payload.paths.iter().any(|p| root_of(p) != root_of(&zone.dir)) {
            "Копировать"
        } else {
            "Переместить"
        };
        format!("{action}{what} в «{name}»")
    }

    /// Файлы вытащили за край окна: дальше перетаскивание ведёт Windows (OLE), и их можно
    /// бросить в Проводник, мессенджер, редактор. `true` — так и случилось.
    fn drag_left_window(&mut self, ctx: &egui::Context) -> bool {
        if !cfg!(windows) {
            return false;
        }
        let window = ctx.content_rect().shrink(1.0);
        let (outside, down) = ctx.input(|i| {
            let left = match i.pointer.latest_pos() {
                Some(pos) => !window.contains(pos),
                None => true,
            };
            (
                left || i.pointer.hover_pos().is_none(),
                i.pointer.primary_down() || i.pointer.secondary_down(),
            )
        });
        if !(outside && down) {
            return false;
        }
        // Из архива наружу не вытащить: файлов на диске ещё нет.
        if egui::DragAndDrop::payload::<DragFiles>(ctx).is_some_and(|p| p.archive.is_some()) {
            return false;
        }
        let Some(payload) = egui::DragAndDrop::take_payload::<DragFiles>(ctx) else { return false };
        match mh_files_platform::dnd::drag_out(&payload.paths, payload.right) {
            Ok(mh_files_platform::dnd::DropEffect::Move) => {
                let dirs: Vec<PathBuf> =
                    payload.paths.iter().filter_map(|p| p.parent().map(PathBuf::from)).collect();
                self.reload_dirs(&dirs);
            }
            Ok(_) => {}
            Err(error) => self.set_status(error, Level::Error),
        }
        true
    }

    /// Меню броска правой кнопкой: «Копировать сюда», «Переместить сюда», «Создать ярлыки»;
    /// для файлов из архива — «Извлечь сюда».
    fn show_drop_menu(&mut self, ctx: &egui::Context) {
        let Some(menu) = &self.drop_menu else { return };
        let mut choice = None;
        let in_archive = menu.archive.is_some();
        let area = egui::Area::new(Id::new("drop-menu"))
            .order(egui::Order::Foreground)
            .fixed_pos(menu.at)
            .show(ctx, |ui| {
                egui::Frame::popup(ui.style()).show(ui, |ui| {
                    ui.set_min_width(190.0);
                    let mut item = |ui: &mut egui::Ui, text: &str, value: u8| {
                        if ui.add(egui::Button::new(text).frame(false)).clicked() {
                            choice = Some(value);
                        }
                    };
                    if in_archive {
                        item(ui, "Извлечь сюда", 1);
                    } else {
                        item(ui, "Копировать сюда", 1);
                        item(ui, "Переместить сюда", 2);
                        item(ui, "Создать ярлыки", 3);
                    }
                    ui.separator();
                    item(ui, "Отмена", 0);
                });
            });
        let pressed_outside = ctx.input(|i| i.pointer.any_pressed())
            && !area.response.contains_pointer()
            && area.response.rect.width() > 0.0;
        let escape = ctx.input(|i| i.key_pressed(egui::Key::Escape));
        if choice.is_none() && !pressed_outside && !escape {
            return;
        }
        let Some(menu) = self.drop_menu.take() else { return };
        match (choice, menu.archive) {
            (Some(1), Some(archive)) => self.extract_drop(archive, menu.paths, menu.dest),
            (Some(1), None) => self.actions.push(Action::Drop {
                paths: menu.paths,
                dest: menu.dest,
                copy: Some(true),
            }),
            (Some(2), None) => self.actions.push(Action::Drop {
                paths: menu.paths,
                dest: menu.dest,
                copy: Some(false),
            }),
            (Some(3), None) => self.workers.shell(mh_files_fs::ShellJob::CreateShortcuts {
                targets: menu.paths,
                dest: menu.dest,
            }),
            _ => {}
        }
    }

    /// Куда встанет перетаскиваемая вкладка, если отпустить в `pos`.
    fn tab_target(&self, pos: egui::Pos2) -> Option<(TabTarget, Rect)> {
        let region =
            self.pane_regions.iter().find(|r| r.strip.contains(pos) || r.content.contains(pos))?;
        if region.strip.contains(pos) {
            return Some((TabTarget::Into(region.pane), region.content));
        }
        let rect = region.content;
        let edge_x = (rect.width() * 0.25).clamp(40.0, 220.0);
        let edge_y = (rect.height() * 0.25).clamp(40.0, 220.0);
        let side = |direction, first, part: Rect| {
            Some((TabTarget::Split { pane: region.pane, direction, first }, part))
        };
        if pos.x < rect.left() + edge_x {
            side(
                SplitDirection::Horizontal,
                true,
                Rect::from_min_max(rect.min, egui::pos2(rect.center().x, rect.bottom())),
            )
        } else if pos.x > rect.right() - edge_x {
            side(
                SplitDirection::Horizontal,
                false,
                Rect::from_min_max(egui::pos2(rect.center().x, rect.top()), rect.max),
            )
        } else if pos.y < rect.top() + edge_y {
            side(
                SplitDirection::Vertical,
                true,
                Rect::from_min_max(rect.min, egui::pos2(rect.right(), rect.center().y)),
            )
        } else if pos.y > rect.bottom() - edge_y {
            side(
                SplitDirection::Vertical,
                false,
                Rect::from_min_max(egui::pos2(rect.left(), rect.center().y), rect.max),
            )
        } else {
            Some((TabTarget::Into(region.pane), rect))
        }
    }

    /// Перетаскивание вкладки: подсветка места и перенос при отпускании.
    fn handle_tab_drop(&mut self, ctx: &egui::Context) {
        let Some(payload) = egui::DragAndDrop::payload::<DragTab>(ctx) else { return };
        let Some(pos) = ctx.input(|i| i.pointer.latest_pos()) else { return };
        let target = self.tab_target(pos);
        if ctx.input(|i| i.pointer.any_released()) {
            egui::DragAndDrop::clear_payload(ctx);
            if let Some((target, _)) = target {
                self.actions.push(Action::MoveTab { from: payload.pane, tab: payload.tab, target });
            }
            return;
        }
        if let Some((_, rect)) = target {
            paint_drop_area(ctx, Id::new("tab-drop"), rect);
        }
    }

    fn drop_on(&mut self, zone: DropZone, paths: Vec<PathBuf>, ctx: &egui::Context) {
        match zone.kind {
            ZoneKind::Folder => {}
            ZoneKind::Favorites(group) => {
                self.actions.push(Action::AddFavorites { group, paths });
                return;
            }
            ZoneKind::RecycleBin => {
                if self.settings.files.confirm_recycle {
                    self.dialog = Some(crate::dialogs::Dialog::delete(paths, false));
                } else {
                    self.submit(FileOp::Delete { paths, permanent: false }, None);
                }
                return;
            }
        }
        // Окно без фокуса (файлы тянули из Проводника) не знает о клавишах — спросить систему.
        let keys = mh_files_platform::window::drag_keys();
        let modifiers = ctx.input(|i| i.modifiers);
        let (ctrl, shift) = (modifiers.ctrl || keys.ctrl, modifiers.shift || keys.shift);
        let copy = if ctrl && shift || keys.alt {
            // Ctrl+Shift или Alt — ярлыки, как в Проводнике.
            self.workers
                .shell(mh_files_fs::ShellJob::CreateShortcuts { targets: paths, dest: zone.dir });
            return;
        } else if ctrl {
            Some(true)
        } else if shift {
            Some(false)
        } else {
            None
        };
        self.actions.push(Action::Drop { paths, dest: zone.dir, copy });
    }

    // ── Окно и сеанс ─────────────────────────────────────────────────────────────────

    fn track_window(&mut self, ctx: &egui::Context) {
        let (rect, maximized, minimized) = ctx.input(|i| {
            let viewport = i.viewport();
            (
                viewport.outer_rect,
                viewport.maximized.unwrap_or(false),
                viewport.minimized.unwrap_or(false),
            )
        });
        if minimized {
            return;
        }
        if maximized {
            if let Some(window) = &mut self.window {
                window.maximized = true;
            }
        } else if let Some(rect) = rect {
            self.window = Some(WindowGeometry {
                x: rect.left(),
                y: rect.top(),
                width: rect.width(),
                height: rect.height(),
                maximized: false,
            });
        }
    }

    /// Сеанс пишется в фоне, только если он изменился.
    pub fn save_session_if_changed(&mut self, force: bool) {
        if !force && self.session_checked.elapsed() < SESSION_SAVE_EVERY {
            return;
        }
        self.session_checked = Instant::now();
        let json = self.session().to_json();
        if json == self.session_json {
            return;
        }
        self.session_json = json.clone();
        let path = storage::session_path();
        let write = move || {
            let _ = storage::write_atomic(&path, &json);
        };
        if force {
            write();
        } else {
            std::thread::spawn(write);
        }
    }

    /// Цветные метки — в фоне, через временный файл.
    pub fn save_labels(&self) {
        let json = self.labels.to_json();
        std::thread::spawn(move || {
            let _ = storage::write_atomic(&storage::labels_path(), &json);
        });
    }

    /// Перечитать вкладки корзины: в ней что-то удалили, восстановили или стёрли.
    pub fn reload_recycle_tabs(&mut self) {
        for tab in self.panes.iter_mut().flat_map(|p| p.tabs.iter_mut()) {
            if tab.location == Location::RecycleBin {
                tab.reload(&self.workers, true);
            }
        }
    }

    pub fn save_settings(&self) {
        let json = self.settings.to_json();
        std::thread::spawn(move || {
            let _ = storage::write_atomic(&storage::settings_path(), &json);
        });
    }

    /// Новые настройки: перестроить то, что от них зависит.
    pub fn apply_settings(&mut self, ctx: &egui::Context, settings: Settings) {
        let (keymap, errors) = Keymap::new(&settings.keys);
        self.keymap = keymap;
        if !errors.is_empty() {
            self.set_status(format!("не разобраны сочетания: {}", errors.join(", ")), Level::Error);
        }
        theme::set_accent(settings.appearance.accent);
        theme::refresh_accent(ctx);
        ctx.set_zoom_factor(settings.appearance.font_scale);
        self.images.system_icons = settings.appearance.system_icons;
        self.images.thumbnails_enabled = settings.preview.thumbnails;
        self.indexer.configure(&settings.index);
        if settings.appearance.custom_title_bar != self.settings.appearance.custom_title_bar {
            ctx.send_viewport_cmd(egui::ViewportCommand::Decorations(
                !settings.appearance.custom_title_bar,
            ));
        }
        let mut settings = settings;
        // Громкость меняют в быстром просмотре, а не в окне настроек: его копия могла устареть.
        settings.preview.volume = self.settings.preview.volume;
        // Пока окно настроек было открыто, расписание могло отработать: его время новее.
        for schedule in &mut settings.sort_schedules {
            if let Some(current) =
                self.settings.sort_schedules.iter().find(|s| s.folder == schedule.folder)
            {
                schedule.last_run = schedule.last_run.max(current.last_run);
            }
        }
        self.settings = settings;
        self.refresh_view_options();
        self.save_settings();
    }

    pub fn refresh_view_options(&mut self) {
        let base = self.view_options();
        let duplicates = self.duplicate_options();
        for tab in self.panes.iter_mut().flat_map(|pane| pane.tabs.iter_mut()) {
            let sort = tab.options.sort;
            tab.set_options(ViewOptions { sort, ..base });
            tab.duplicate_options = duplicates.clone();
        }
    }

    /// Порог размера и исключения для поиска дубликатов — из настроек.
    pub fn duplicate_options(&self) -> mh_files_fs::DuplicateOptions {
        mh_files_fs::DuplicateOptions {
            min_size: self.settings.duplicates.min_size.max(1),
            exclude: self.settings.duplicates.exclude.clone(),
            ..Default::default()
        }
    }

    pub fn available_cached(&self, command: CommandId) -> bool {
        self.availability.get(&command).copied().unwrap_or(true)
    }
}

/// HWND главного окна — владелец системных диалогов (копирование, свойства, буфер обмена).
fn set_owner_window(cc: &eframe::CreationContext<'_>) {
    #[cfg(windows)]
    {
        use raw_window_handle::{HasWindowHandle, RawWindowHandle};
        if let Ok(handle) = cc.window_handle()
            && let RawWindowHandle::Win32(win32) = handle.as_raw()
        {
            mh_files_platform::window::set_owner_window(win32.hwnd.get());
        }
    }
    #[cfg(not(windows))]
    let _ = cc;
}

impl eframe::App for FilesApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.frame(ui);
        if ui.ctx().input(|i| i.viewport().close_requested()) {
            self.save_session_if_changed(true);
        }
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        self.save_session_if_changed(true);
        self.indexer.shutdown(INDEX_SAVE_WAIT);
        crate::crash::finish();
    }
}

/// Размеры папок из кэша — в записи вкладки.
fn apply_sizes(tab: &mut Tab, sizes: &HashMap<PathBuf, DirSize>) {
    let Some(dir) = tab.dir() else { return };
    let mut changed = false;
    for (path, size) in sizes {
        if path.parent() == Some(dir.as_path()) {
            changed |= tab.listing.set_dir_size(path, size.bytes);
        }
    }
    if changed {
        tab.listing.refresh();
    }
}

/// Подпись диска: «C: Локальный диск», «D: Фото».
pub fn drive_title(drive: &DriveInfo) -> String {
    let root = mh_files_core::location::path_label(&drive.root);
    let kind = match drive.kind {
        DriveKind::Network => "Сетевой диск",
        DriveKind::Removable => "Съёмный диск",
        DriveKind::Optical => "Привод",
        _ => "Локальный диск",
    };
    let label = if drive.label.is_empty() { kind } else { drive.label.as_str() };
    // Буква первой: диски в списке выравниваются по ней и ищутся с первого символа.
    format!("{root} {label}")
}

/// Подсветка места, куда встанет панель.
fn paint_drop_area(ctx: &egui::Context, id: Id, rect: Rect) {
    egui::Area::new(id)
        .order(egui::Order::Foreground)
        .fixed_pos(rect.min)
        .interactable(false)
        .show(ctx, |ui| {
            ui.painter().rect_filled(
                rect,
                CornerRadius::same(6),
                theme::accent().gamma_multiply(0.18),
            );
            ui.painter().rect_stroke(
                rect.shrink(1.0),
                CornerRadius::same(6),
                Stroke::new(1.5, theme::accent()),
                egui::StrokeKind::Inside,
            );
        });
}

/// Плашка у курсора при перетаскивании. Одной строкой и целиком в окне: у правого края
/// встаёт слева от курсора, у нижнего — над ним.
fn drag_label(ctx: &egui::Context, pos: egui::Pos2, text: &str) {
    let screen = ctx.content_rect();
    let galley = ctx.fonts_mut(|fonts| {
        fonts.layout_no_wrap(text.to_string(), theme::regular(13.0), theme::TEXT_PRIMARY)
    });
    let size = galley.size() + vec2(16.0, 8.0);
    let mut at = pos + vec2(16.0, 12.0);
    if at.x + size.x > screen.right() - 4.0 {
        at.x = pos.x - 8.0 - size.x;
    }
    if at.y + size.y > screen.bottom() - 4.0 {
        at.y = pos.y - 8.0 - size.y;
    }
    at.x = at.x.max(screen.left() + 4.0);
    let rect = Rect::from_min_size(at, size);
    let painter =
        ctx.layer_painter(egui::LayerId::new(egui::Order::Tooltip, Id::new("drag-label")));
    painter.rect(
        rect,
        CornerRadius::same(4),
        theme::CARD,
        Stroke::new(1.0, theme::CARD_STROKE),
        egui::StrokeKind::Inside,
    );
    painter.galley(rect.min + vec2(8.0, 4.0), galley, theme::TEXT_PRIMARY);
}

/// Длинное имя — с многоточием в середине, чтобы плашка не тянулась через всё окно.
fn short(name: &str) -> String {
    const MAX: usize = 40;
    let chars: Vec<char> = name.chars().collect();
    if chars.len() <= MAX {
        return name.to_string();
    }
    let head: String = chars[..MAX / 2].iter().collect();
    let tail: String = chars[chars.len() - (MAX / 2 - 1)..].iter().collect();
    format!("{head}…{tail}")
}

/// Корень диска для пути: по нему решается «переместить или копировать» при перетаскивании.
pub fn root_of(path: &Path) -> PathBuf {
    path.ancestors().last().map(PathBuf::from).unwrap_or_default()
}
