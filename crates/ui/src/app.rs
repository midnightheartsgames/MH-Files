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
use mh_files_core::layout::{LayoutNode, MAX_RATIO, MIN_RATIO, PaneId, SplitDirection};
use mh_files_core::listing::ViewOptions;
use mh_files_core::location::Location;
use mh_files_core::session::{self, PaneSession, Session, TabSession, ViewMode, WindowGeometry};
use mh_files_core::settings::Settings;
use mh_files_core::sort::SortColumn;
use mh_files_fs::{CancelToken, DirSize, Event, Ticket, Workers};
use mh_files_platform::drives::{DriveInfo, DriveKind};
use mh_files_platform::folders::KnownFolder;
use mh_files_platform::ops::{Executor, OpEvent};
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

/// Как долго ждать второй шаг последовательности (`Alt+G` → `D`).
const CHORD_TIMEOUT: Duration = Duration::from_millis(1500);

const SESSION_SAVE_EVERY: Duration = Duration::from_secs(5);
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
}

/// Что перетаскивают внутри окна.
#[derive(Debug, Clone)]
pub struct DragFiles {
    pub paths: Vec<PathBuf>,
}

/// Куда можно бросить файлы: папка-строка, вкладка, пункт боковой панели.
#[derive(Debug, Clone)]
pub struct DropZone {
    pub rect: Rect,
    pub dir: PathBuf,
    /// Строки внутри панели важнее самой панели.
    pub priority: u8,
    /// Бросок на группу избранного добавляет в неё, а не копирует.
    pub favorite_group: Option<usize>,
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
}

pub struct FilesApp {
    pub settings: Settings,
    pub keymap: Keymap,
    pub layout: LayoutNode,
    pub panes: Vec<Pane>,
    pub focused: PaneId,
    next_id: u64,
    pub workers: Workers,
    events: Receiver<Event>,
    pub ops: Executor,
    pub operations: crate::operations::Operations,
    pub drives: Vec<DriveInfo>,
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
    pub crumb_menu: Option<CrumbMenu>,
    pub columns: Columns,
    /// Первый шаг последовательности клавиш и когда он нажат.
    pub pending_chord: Option<(egui::KeyboardShortcut, Instant)>,
    /// Посчитанные размеры папок (целиком) и те, что ещё считаются.
    pub folder_sizes: HashMap<PathBuf, DirSize>,
    pub sizes_pending: HashSet<PathBuf>,
    sizes_cancel: Option<CancelToken>,
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
    ) -> FilesApp {
        theme::set_accent(settings.appearance.accent);
        theme::install(&cc.egui_ctx);
        cc.egui_ctx.set_zoom_factor(settings.appearance.font_scale);
        set_owner_window(cc);

        let ctx = cc.egui_ctx.clone();
        let (workers, events) = Workers::new(std::sync::Arc::new(move || ctx.request_repaint()));
        let ctx = cc.egui_ctx.clone();
        let ops = Executor::new(std::sync::Arc::new(move || ctx.request_repaint()));
        let (keymap, key_errors) = Keymap::new(&settings.keys);

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
            events,
            ops,
            operations: Default::default(),
            drives: Vec::new(),
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
            crumb_menu: None,
            columns: Columns::default(),
            pending_chord: None,
            folder_sizes: HashMap::new(),
            sizes_pending: HashSet::new(),
            sizes_cancel: None,
            sizes_generation: 0,
            pane_regions: Vec::new(),
            availability: HashMap::new(),
            session_json: String::new(),
            session_checked: Instant::now(),
            window: None,
            settings,
        };
        let session = session
            .filter(|_| app.settings.panes.restore_session)
            .unwrap_or_else(|| Session::single(app.home_location()));
        app.restore(session);
        app.workers.drives();
        app.workers.known_folders();
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
        let workers = self.workers.clone();
        for tab in self.panes.iter_mut().flat_map(|pane| pane.tabs.iter_mut()) {
            if tab.dir().is_some_and(|dir| dirs.contains(&dir)) {
                tab.reload(&workers, true);
            }
        }
    }

    // ── События воркеров ─────────────────────────────────────────────────────────────

    fn poll(&mut self, ctx: &egui::Context) {
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
            Event::Image { key, result } => self.images.on_result(ctx, key, result),
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

    fn on_folder_size(&mut self, ticket: Ticket, path: PathBuf, size: DirSize) {
        if ticket.generation != self.sizes_generation {
            return;
        }
        self.sizes_pending.remove(&path);
        self.folder_sizes.insert(path.clone(), size);
        for tab in self.panes.iter_mut().flat_map(|pane| pane.tabs.iter_mut()) {
            if tab.listing.set_dir_size(&path, size.bytes) {
                tab.listing.refresh();
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
        self.poll(&ctx);
        self.handle_keys(&ctx);
        self.run_actions(&ctx);
        self.drop_zones.clear();
        self.pane_regions.clear();
        self.availability =
            CommandId::ALL.iter().map(|&command| (command, self.available(command))).collect();

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

        self.handle_drops(&ctx);
        self.run_actions(&ctx);

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
        let external = ctx.input(|i| !i.raw.hovered_files.is_empty());
        self.drop_hover = if dragging || external {
            pointer.and_then(|pos| self.zone_at(pos)).map(|zone| zone.dir.clone())
        } else {
            None
        };
        // Файлы из Проводника и других программ.
        let dropped: Vec<PathBuf> = ctx.input(|i| {
            i.raw
                .dropped_files
                .iter()
                .map(|f| f.path().to_path_buf())
                .filter(|p| !p.as_os_str().is_empty())
                .collect()
        });
        if !dropped.is_empty() {
            let at = pointer.or_else(|| ctx.input(|i| i.pointer.latest_pos()));
            match at.and_then(|pos| self.zone_at(pos)).cloned() {
                Some(zone) => self.drop_on(zone, dropped, ctx),
                None => self.set_status("бросьте файлы на папку или панель", Level::Info),
            }
        }
        if dragging && ctx.input(|i| i.pointer.any_released()) {
            let payload = egui::DragAndDrop::take_payload::<DragFiles>(ctx);
            if let (Some(payload), Some(pos)) = (payload, pointer)
                && let Some(zone) = self.zone_at(pos).cloned()
            {
                self.drop_on(zone, payload.paths.clone(), ctx);
            }
        }
        if dragging && let Some(pos) = pointer {
            let label = match &self.drop_hover {
                Some(dir) => format!("В «{}»", mh_files_core::location::path_label(dir)),
                None => "…".to_string(),
            };
            egui::Area::new(Id::new("drag-label"))
                .order(egui::Order::Tooltip)
                .fixed_pos(pos + vec2(14.0, 10.0))
                .interactable(false)
                .show(ctx, |ui| {
                    egui::Frame::new()
                        .fill(theme::CARD)
                        .stroke(Stroke::new(1.0, theme::CARD_STROKE))
                        .corner_radius(CornerRadius::same(4))
                        .inner_margin(egui::Margin::symmetric(8, 4))
                        .show(ui, |ui| ui.label(label));
                });
        }
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
            (left || i.pointer.hover_pos().is_none(), i.pointer.primary_down())
        });
        if !(outside && down) {
            return false;
        }
        let Some(payload) = egui::DragAndDrop::take_payload::<DragFiles>(ctx) else { return false };
        match mh_files_platform::dnd::drag_out(&payload.paths) {
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
            egui::Area::new(Id::new("tab-drop"))
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
    }

    fn drop_on(&mut self, zone: DropZone, paths: Vec<PathBuf>, ctx: &egui::Context) {
        if let Some(group) = zone.favorite_group {
            self.actions.push(Action::AddFavorites { group, paths });
            return;
        }
        let modifiers = ctx.input(|i| i.modifiers);
        let copy = if modifiers.ctrl {
            Some(true)
        } else if modifiers.shift {
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
        self.settings = settings;
        self.refresh_view_options();
        self.save_settings();
    }

    pub fn refresh_view_options(&mut self) {
        let base = self.view_options();
        for tab in self.panes.iter_mut().flat_map(|pane| pane.tabs.iter_mut()) {
            let sort = tab.listing.options().sort;
            tab.set_options(ViewOptions { sort, ..base });
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

/// Корни дисков без опроса: показать сразу, пока идёт опрос.
pub fn drive_title(drive: &DriveInfo) -> String {
    let root = mh_files_core::location::path_label(&drive.root);
    let kind = match drive.kind {
        DriveKind::Network => "Сетевой диск",
        DriveKind::Removable => "Съёмный диск",
        DriveKind::Optical => "Привод",
        _ => "Локальный диск",
    };
    let label = if drive.label.is_empty() { kind } else { drive.label.as_str() };
    format!("{label} ({root})")
}

/// Корень диска для пути: по нему решается «переместить или копировать» при перетаскивании.
pub fn root_of(path: &Path) -> PathBuf {
    path.ancestors().last().map(PathBuf::from).unwrap_or_default()
}
