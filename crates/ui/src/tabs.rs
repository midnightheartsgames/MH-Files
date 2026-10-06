//! Вкладки и панели во время работы: модель из `mh-files-core` плюс то, что связывает её с
//! воркерами — поколение запроса, отмена, наблюдатель, ход поиска.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use mh_files_core::Entry;
use mh_files_core::history::History;
use mh_files_core::layout::PaneId;
use mh_files_core::listing::{Listing, LoadState, ViewOptions};
use mh_files_core::location::Location;
use mh_files_core::selection::Selection;
use mh_files_core::session::{TabSession, ViewMode};
use mh_files_fs::{CancelToken, DirWatch, SearchQuery, Ticket, Workers};

/// Как часто досортировывать список, пока он ещё грузится.
const REFRESH_EVERY: Duration = Duration::from_millis(120);
/// Набор имени с клавиатуры: пауза дольше — начать заново.
const TYPE_AHEAD_RESET: Duration = Duration::from_millis(900);

/// Переименование прямо в строке списка.
#[derive(Debug, Clone)]
pub struct InlineRename {
    pub path: PathBuf,
    pub text: String,
    /// В первом кадре выделить имя без расширения.
    pub fresh: bool,
}

/// Рамка выделения. Начало хранится в координатах содержимого списка (с учётом прокрутки),
/// чтобы рамка тянулась и при прокрутке.
#[derive(Debug, Clone)]
pub struct Band {
    pub origin: eframe::egui::Pos2,
    /// Выделенное до рамки: с Ctrl рамка добавляет к нему.
    pub base: std::collections::HashSet<PathBuf>,
}

#[derive(Debug, Clone, Default)]
pub struct SearchProgress {
    pub scanned: usize,
    pub done: bool,
    pub error: Option<String>,
}

pub struct Tab {
    pub id: u64,
    pub location: Location,
    pub history: History,
    pub listing: Listing,
    pub selection: Selection,
    pub view: ViewMode,
    pub filter: String,
    pub filter_open: bool,
    pub focus_filter: bool,
    /// Поле адреса вместо строки пути; текст в нём.
    pub address: Option<String>,
    pub focus_address: bool,
    pub search: Option<SearchProgress>,
    pub rename: Option<InlineRename>,
    /// Прокрутить к строке в следующем кадре.
    pub scroll_to: Option<usize>,
    /// Колонок в режиме плиток в последнем кадре — для стрелок вверх/вниз.
    pub grid_columns: usize,
    /// Строк на экране в последнем кадре — для PageUp/PageDown.
    pub page_rows: usize,
    /// Прокрутка и высота видимой области в последнем кадре.
    pub scroll_offset: f32,
    pub viewport_height: f32,
    /// Верх строк на экране в последнем кадре: от него считается рамка выделения.
    pub list_top: f32,
    /// Прокрутить к этому смещению в следующем кадре (рамка у края списка).
    pub scroll_request: Option<f32>,
    /// Выделение рамкой мышью, пока кнопка нажата.
    pub band: Option<Band>,
    /// Выделить этот объект, когда он появится (папка, из которой поднялись).
    pub pending_select: Option<PathBuf>,
    generation: u64,
    watch_generation: u64,
    cancel: Option<CancelToken>,
    watch: Option<DirWatch>,
    /// Мягкое перечитывание: новые записи копятся здесь и заменяют старые разом, без мигания.
    staging: Option<Vec<Entry>>,
    dirty: bool,
    last_refresh: Instant,
    /// Сколько занял последний пересчёт: на огромных папках досортировка реже.
    refresh_cost: Duration,
    type_ahead: (String, Instant),
}

impl Tab {
    pub fn new(id: u64, location: Location, view: ViewMode, options: ViewOptions) -> Tab {
        Tab {
            id,
            location,
            history: History::default(),
            listing: Listing::new(options),
            selection: Selection::default(),
            view,
            filter: String::new(),
            filter_open: false,
            focus_filter: false,
            address: None,
            focus_address: false,
            search: None,
            rename: None,
            scroll_to: None,
            grid_columns: 1,
            page_rows: 10,
            scroll_offset: 0.0,
            viewport_height: 0.0,
            list_top: 0.0,
            scroll_request: None,
            band: None,
            pending_select: None,
            generation: 0,
            watch_generation: 0,
            cancel: None,
            watch: None,
            staging: None,
            dirty: false,
            last_refresh: Instant::now(),
            refresh_cost: Duration::ZERO,
            type_ahead: (String::new(), Instant::now()),
        }
    }

    pub fn title(&self) -> String {
        self.location.title()
    }

    pub fn ticket(&self) -> Ticket {
        Ticket { owner: self.id, generation: self.generation }
    }

    pub fn dir(&self) -> Option<PathBuf> {
        self.location.dir().map(PathBuf::from)
    }

    pub fn session(&self) -> TabSession {
        TabSession {
            location: self.location.clone(),
            view: self.view,
            sort: self.listing.options().sort,
        }
    }

    /// Объекты для команды: выделенные или под курсором.
    pub fn targets(&self) -> Vec<PathBuf> {
        self.selection.targets(&self.listing)
    }

    /// Запись по пути среди загруженных.
    pub fn entry(&self, path: &std::path::Path) -> Option<&Entry> {
        self.listing.find(path)
    }

    /// Перейти в другое место. `record` — запомнить текущее в истории.
    pub fn navigate(&mut self, location: Location, workers: &Workers, record: bool) {
        if location == self.location && record {
            self.reload(workers, true);
            return;
        }
        // Поднимаясь выше, выделить папку, из которой пришли.
        self.pending_select = match (&self.location, &location) {
            (Location::Dir(from), Location::Dir(to)) if from.parent() == Some(to.as_path()) => {
                Some(from.clone())
            }
            (Location::Dir(from), Location::Computer) if from.parent().is_none() => None,
            _ => None,
        };
        if record {
            self.history.visit(self.location.clone());
        }
        self.location = location;
        self.selection.reset();
        self.filter.clear();
        self.filter_open = false;
        self.listing.set_filter("");
        self.address = None;
        self.rename = None;
        self.scroll_to = Some(0);
        self.reload(workers, false);
    }

    /// Перечитать. `soft` — не очищать список до прихода нового (после операций, F5).
    pub fn reload(&mut self, workers: &Workers, soft: bool) {
        if let Some(cancel) = self.cancel.take() {
            cancel.cancel();
        }
        self.generation += 1;
        let ticket = self.ticket();
        match self.location.clone() {
            Location::Computer => {
                self.watch = None;
                self.staging = None;
                self.listing.reset();
                self.listing.finish(Ok(()));
            }
            Location::Dir(dir) => {
                if soft && self.listing.state != LoadState::Loading {
                    self.staging = Some(Vec::new());
                } else {
                    self.staging = None;
                    self.listing.reset();
                }
                let same_watch = self.watch.as_ref().is_some_and(|w| w.dir == dir);
                if !same_watch {
                    self.watch_generation = self.generation;
                    self.watch = workers.watch(ticket, dir.clone());
                }
                self.cancel = Some(workers.list(ticket, dir));
            }
            Location::Search { root, query } => {
                self.watch = None;
                self.staging = None;
                self.listing.reset();
                self.search = Some(SearchProgress::default());
                let query =
                    SearchQuery { text: query, include_hidden: self.listing.options().show_hidden };
                self.cancel = Some(workers.search(ticket, root, query));
            }
        }
        if !matches!(self.location, Location::Search { .. }) {
            self.search = None;
        }
    }

    /// Остановить фоновую работу вкладки (закрытие).
    pub fn stop(&mut self) {
        if let Some(cancel) = self.cancel.take() {
            cancel.cancel();
        }
        self.watch = None;
    }

    fn current(&self, ticket: Ticket) -> bool {
        ticket.owner == self.id && ticket.generation == self.generation
    }

    pub fn on_batch(&mut self, ticket: Ticket, batch: Vec<Entry>) {
        if !self.current(ticket) {
            return;
        }
        match &mut self.staging {
            Some(staging) => staging.extend(batch),
            None => {
                self.listing.extend(batch);
                self.dirty = true;
            }
        }
    }

    pub fn on_done(&mut self, ticket: Ticket, result: Result<(), String>) {
        if !self.current(ticket) {
            return;
        }
        self.cancel = None;
        if let Some(staging) = self.staging.take()
            && result.is_ok()
        {
            self.listing.replace(staging);
        }
        if result.is_err() {
            self.watch = None;
        }
        self.listing.finish(result);
        self.dirty = false;
        self.after_change();
    }

    pub fn on_search(&mut self, ticket: Ticket, batch: Vec<Entry>, scanned: usize, done: bool) {
        if !self.current(ticket) {
            return;
        }
        self.listing.extend(batch);
        self.dirty = true;
        if let Some(progress) = &mut self.search {
            progress.scanned = scanned;
            progress.done = done;
        }
    }

    pub fn on_search_done(&mut self, ticket: Ticket, result: Result<(), String>, scanned: usize) {
        if !self.current(ticket) {
            return;
        }
        self.cancel = None;
        if let Some(progress) = &mut self.search {
            progress.scanned = scanned;
            progress.done = true;
            progress.error = result.err();
        }
        self.listing.finish(Ok(()));
        self.dirty = false;
    }

    /// Правка от наблюдателя. `true` — нужно перечитать целиком.
    pub fn on_patch(
        &mut self,
        ticket: Ticket,
        upserts: Vec<Entry>,
        removes: Vec<PathBuf>,
        reload: bool,
    ) -> bool {
        if ticket.owner != self.id || ticket.generation != self.watch_generation {
            return false;
        }
        if reload {
            return true;
        }
        let Some(dir) = self.location.dir() else { return false };
        for path in &removes {
            self.listing.remove(path);
        }
        for entry in upserts {
            if *entry.parent == *dir {
                self.listing.upsert(entry);
            }
        }
        self.listing.refresh();
        self.after_change();
        false
    }

    /// Досортировать загружаемый список не чаще раза в REFRESH_EVERY.
    pub fn tick(&mut self) -> bool {
        if self.dirty && self.last_refresh.elapsed() >= REFRESH_EVERY.max(self.refresh_cost * 8) {
            let started = Instant::now();
            self.listing.refresh();
            self.refresh_cost = started.elapsed();
            self.dirty = false;
            self.last_refresh = Instant::now();
            self.after_change();
        }
        self.dirty || self.listing.is_loading()
    }

    fn after_change(&mut self) {
        if let Some(path) = self.pending_select.clone()
            && let Some(row) = self.listing.row_of(&path)
        {
            self.selection.select_only(path);
            self.scroll_to = Some(row);
            self.pending_select = None;
        }
        self.selection.retain_visible(&self.listing);
        if self.listing.state != LoadState::Loading {
            self.pending_select = None;
        }
    }

    pub fn set_options(&mut self, options: ViewOptions) {
        self.listing.set_options(options);
        self.selection.retain_visible(&self.listing);
    }

    pub fn set_filter(&mut self, text: String) {
        self.listing.set_filter(&text);
        self.filter = text;
        self.selection.retain_visible(&self.listing);
        if self.selection.cursor().is_none() && !self.listing.is_empty() {
            self.cursor_to(0, Default::default());
        }
    }

    /// Курсор на строку с прокруткой к ней.
    pub fn cursor_to(&mut self, row: usize, modifiers: mh_files_core::selection::Modifiers) {
        if self.listing.is_empty() {
            return;
        }
        let row = row.min(self.listing.len() - 1);
        self.selection.move_to(&self.listing, row, modifiers);
        self.scroll_to = Some(row);
    }

    /// Сдвиг курсора на `delta` строк.
    pub fn move_cursor(&mut self, delta: isize, modifiers: mh_files_core::selection::Modifiers) {
        if self.listing.is_empty() {
            return;
        }
        let row = match self.selection.cursor_row(&self.listing) {
            Some(row) => (row as isize + delta).clamp(0, self.listing.len() as isize - 1) as usize,
            None if delta < 0 => self.listing.len() - 1,
            None => 0,
        };
        self.cursor_to(row, modifiers);
    }

    /// Набор начала имени с клавиатуры.
    pub fn type_ahead(&mut self, text: &str) {
        let (buffer, at) = &mut self.type_ahead;
        if at.elapsed() > TYPE_AHEAD_RESET {
            buffer.clear();
        }
        *at = Instant::now();
        // Одна и та же буква подряд — следующий объект на эту букву.
        let repeat = buffer.chars().count() == 1 && buffer.as_str() == text;
        if !repeat {
            buffer.push_str(text);
        }
        let prefix = buffer.clone();
        let start =
            self.selection.cursor_row(&self.listing).map_or(0, |row| row + usize::from(repeat));
        if let Some(row) = self.listing.find_prefix(&prefix, start) {
            self.cursor_to(row, Default::default());
        }
    }
}

pub struct Pane {
    pub id: PaneId,
    pub tabs: Vec<Tab>,
    pub active: usize,
}

impl Pane {
    pub fn tab(&self) -> &Tab {
        &self.tabs[self.active]
    }

    pub fn tab_mut(&mut self) -> &mut Tab {
        &mut self.tabs[self.active]
    }
}
