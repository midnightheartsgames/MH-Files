//! Вкладки и панели во время работы: модель из `mh-files-core` плюс то, что связывает её с
//! воркерами — поколение запроса, отмена, наблюдатель, ход поиска.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use mh_files_core::Entry;
use mh_files_core::duplicates::Group;
use mh_files_core::history::History;
use mh_files_core::layout::PaneId;
use mh_files_core::listing::{Listing, LoadState, ViewOptions};
use mh_files_core::location::Location;
use mh_files_core::selection::Selection;
use mh_files_core::session::{TabSession, ViewMode};
use mh_files_core::sort::{SortColumn, SortOrder};
use mh_files_fs::{
    CancelToken, DirWatch, DuplicateOptions, DuplicateProgress, IndexResults, Indexer, SearchQuery,
    Ticket, Workers,
};

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

/// Поиск по дискам во вкладке: поле запроса и итог.
#[derive(Debug, Clone)]
pub struct IndexView {
    /// Текст поля запроса — как набран, с пробелами по краям.
    pub text: String,
    /// Поставить фокус в поле в следующем кадре.
    pub focus: bool,
    /// Запустить поиск в конце кадра.
    pub pending: bool,
    /// Индекс изменился после последнего поиска — выдачу стоит обновить.
    pub stale: bool,
    pub searching: bool,
    /// Сколько подошло всего (показано не больше предела).
    pub total: usize,
    pub elapsed: Duration,
    pub error: Option<String>,
    /// Когда запущен последний поиск.
    pub last: Instant,
}

impl IndexView {
    fn new(text: &str) -> IndexView {
        IndexView {
            text: text.to_string(),
            // Фокус ставит команда «Поиск по дискам»; вкладка из сеанса или сохранённый
            // поиск клавиатуру не забирают.
            focus: false,
            pending: true,
            stale: false,
            searching: false,
            total: 0,
            elapsed: Duration::ZERO,
            error: None,
            last: Instant::now(),
        }
    }
}

/// Поиск дубликатов во вкладке.
#[derive(Debug, Clone, Default)]
pub struct DuplicatesView {
    pub progress: Option<DuplicateProgress>,
    pub groups: Vec<Group>,
    /// Номер группы каждого файла — для полос в списке.
    pub group_of: std::collections::HashMap<PathBuf, usize>,
    pub error: Option<String>,
    pub done: bool,
    /// Какую копию оставлять при «Отметить лишние».
    pub keep: mh_files_core::duplicates::Keep,
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
    pub index: Option<IndexView>,
    pub duplicates: Option<DuplicatesView>,
    /// Сортировщик во вкладке.
    pub sort: Option<Box<crate::sorter::SortView>>,
    /// Вид «Колонки»: какое место было в каждой колонке в прошлом кадре — чтобы прокрутить
    /// колонку к дороге сюда только при смене места.
    pub columns_shown: Vec<Option<Location>>,
    /// Встать на первую строку, когда список загрузится (вход в папку стрелкой вправо).
    pub select_first: bool,
    /// Когда начато чтение — для замеров.
    pub load_started: Instant,
    /// Вид списка, как его задали настройки и пользователь. Поиск по дискам показывает
    /// скрытое независимо от него (см. [`options_for`]).
    pub options: ViewOptions,
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
    pub fn new(id: u64, location: Location, view: ViewMode, mut options: ViewOptions) -> Tab {
        // Новая вкладка поиска или дубликатов — в порядке выдачи.
        if ranked(&location) && options.sort == SortOrder::default() {
            options.sort = SortOrder { column: SortColumn::Relevance, descending: false };
        }
        let listing = Listing::new(options_for(&location, options));
        Tab {
            id,
            location,
            history: History::default(),
            listing,
            options,
            selection: Selection::default(),
            view,
            filter: String::new(),
            filter_open: false,
            focus_filter: false,
            address: None,
            focus_address: false,
            search: None,
            index: None,
            duplicates: None,
            sort: None,
            columns_shown: Vec::new(),
            select_first: false,
            load_started: Instant::now(),
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
        TabSession { location: self.location.clone(), view: self.view, sort: self.options.sort }
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
        // Поднимаясь выше, выделить папку (или архив), из которой пришли.
        self.pending_select = if self.location.parent().as_ref() == Some(&location) {
            self.location.own_path()
        } else {
            None
        };
        self.select_first = false;
        if record {
            self.history.visit(self.location.clone());
        }
        // Выдача поиска по дискам и дубликаты упорядочены сами; в папках — обычная сортировка.
        let to_index = ranked(&location);
        if to_index != ranked(&self.location) {
            if to_index {
                self.options.sort = SortOrder { column: SortColumn::Relevance, descending: false };
            } else if self.options.sort.column == SortColumn::Relevance {
                self.options.sort = SortOrder::default();
            }
        }
        self.location = location;
        self.apply_options();
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
        self.load_started = Instant::now();
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
                let query = SearchQuery { text: query, include_hidden: self.options.show_hidden };
                self.cancel = Some(workers.search(ticket, root, query));
            }
            Location::Archive { archive, inner } => {
                self.watch = None;
                if soft && self.listing.state != LoadState::Loading {
                    self.staging = Some(Vec::new());
                } else {
                    self.staging = None;
                    self.listing.reset();
                }
                self.cancel = Some(workers.list_archive(ticket, archive, inner));
            }
            Location::Duplicates { roots } => {
                self.watch = None;
                self.staging = None;
                self.listing.reset();
                self.duplicates = Some(DuplicatesView::default());
                let options = DuplicateOptions {
                    include_hidden: self.options.show_hidden,
                    ..DuplicateOptions::default()
                };
                self.cancel = Some(workers.duplicates(ticket, roots, options));
            }
            Location::Sort { .. } => {
                // План строит окно в конце кадра: категории и галочки — у него.
                self.watch = None;
                self.staging = None;
                self.listing.reset();
                self.listing.finish(Ok(()));
                match &mut self.sort {
                    Some(view) => view.rescan(),
                    None => self.sort = Some(Box::new(crate::sorter::SortView::new())),
                }
            }
            Location::Index { query } => {
                // Сам поиск запускает окно в конце кадра: у вкладки нет доступа к индексу.
                self.watch = None;
                self.staging = None;
                if !soft {
                    self.listing.reset();
                }
                let view = self.index.get_or_insert_with(|| IndexView::new(&query));
                if view.text.trim() != query {
                    view.text = query;
                }
                view.pending = true;
            }
        }
        if !matches!(self.location, Location::Search { .. }) {
            self.search = None;
        }
        if !matches!(self.location, Location::Index { .. }) {
            self.index = None;
        }
        if !matches!(self.location, Location::Duplicates { .. }) {
            self.duplicates = None;
        }
        if !matches!(self.location, Location::Sort { .. }) {
            self.sort = None;
        }
    }

    pub fn on_duplicates_progress(&mut self, ticket: Ticket, progress: DuplicateProgress) {
        if self.current(ticket)
            && let Some(view) = &mut self.duplicates
        {
            view.progress = Some(progress);
        }
    }

    pub fn on_duplicates_done(&mut self, ticket: Ticket, result: Result<Vec<Group>, String>) {
        if !self.current(ticket) {
            return;
        }
        self.cancel = None;
        let Some(view) = &mut self.duplicates else { return };
        view.done = true;
        let groups = match result {
            Ok(groups) => groups,
            Err(error) => {
                view.error = Some(error);
                Vec::new()
            }
        };
        let mut entries = Vec::new();
        for (number, group) in groups.iter().enumerate() {
            for member in &group.files {
                let parent: std::sync::Arc<std::path::Path> =
                    std::sync::Arc::from(member.path.parent().unwrap_or(&member.path));
                let name = member.path.file_name().unwrap_or_default().to_string_lossy();
                view.group_of.insert(member.path.clone(), number);
                entries.push(Entry {
                    name: name.into_owned(),
                    parent,
                    kind: mh_files_core::EntryKind::File,
                    size: group.size,
                    modified: member.modified,
                    created: None,
                    attributes: Default::default(),
                });
            }
        }
        view.groups = groups;
        self.listing.replace(entries);
        self.listing.state = LoadState::Done;
        self.after_change();
    }

    /// Убрать из результатов исчезнувшие файлы (удалили лишние копии). Группы, где осталась
    /// одна копия, уходят целиком.
    pub fn forget_paths(&mut self, gone: &[PathBuf]) {
        let Some(view) = &mut self.duplicates else { return };
        if gone.is_empty() {
            return;
        }
        for group in &mut view.groups {
            group.files.retain(|m| !gone.contains(&m.path));
        }
        let lonely: Vec<PathBuf> = view
            .groups
            .iter()
            .filter(|g| g.files.len() < 2)
            .flat_map(|g| g.files.iter().map(|m| m.path.clone()))
            .collect();
        view.groups.retain(|g| g.files.len() > 1);
        view.group_of.clear();
        for (number, group) in view.groups.iter().enumerate() {
            for member in &group.files {
                view.group_of.insert(member.path.clone(), number);
            }
        }
        for path in gone.iter().chain(&lonely) {
            self.listing.remove(path);
        }
        self.listing.refresh();
        self.after_change();
    }

    /// Применить отложенное выделение сразу, если объект уже в списке.
    pub fn reveal_pending(&mut self) {
        self.after_change();
    }

    /// Новый текст в поле поиска по дискам. История не пополняется на каждую букву.
    pub fn set_index_query(&mut self, text: String, workers: &Workers) {
        let query = text.trim().to_string();
        if let Some(view) = &mut self.index {
            view.text = text;
        }
        if self.location != (Location::Index { query: query.clone() }) {
            if !matches!(self.location, Location::Index { .. }) {
                self.options.sort = SortOrder { column: SortColumn::Relevance, descending: false };
            }
            self.location = Location::Index { query };
            self.apply_options();
            self.reload(workers, true);
        }
    }

    /// Запустить поиск по индексу, если он нужен.
    pub fn start_index_search(&mut self, indexer: &Indexer, show_hidden: bool) {
        let Location::Index { query } = &self.location else { return };
        let Some(view) = &mut self.index else { return };
        if !view.pending {
            return;
        }
        view.pending = false;
        view.stale = false;
        view.searching = true;
        view.last = Instant::now();
        let query = query.clone();
        if let Some(cancel) = self.cancel.take() {
            cancel.cancel();
        }
        self.generation += 1;
        let ticket = self.ticket();
        self.cancel = Some(indexer.search(ticket, query, show_hidden));
    }

    pub fn on_index_results(&mut self, ticket: Ticket, result: Result<IndexResults, String>) {
        if !self.current(ticket) {
            return;
        }
        self.cancel = None;
        let Some(view) = &mut self.index else { return };
        view.searching = false;
        let entries = match result {
            Ok(found) => {
                view.total = found.total;
                view.elapsed = found.elapsed;
                view.error = None;
                found.entries
            }
            Err(error) => {
                view.total = 0;
                view.error = Some(error);
                Vec::new()
            }
        };
        self.listing.replace(entries);
        self.listing.state = LoadState::Done;
        self.dirty = false;
        self.after_change();
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
        if self.select_first && !self.listing.is_empty() {
            self.select_first = false;
            if self.selection.cursor().is_none() {
                self.cursor_to(0, Default::default());
            }
        }
        self.selection.retain_visible(&self.listing);
        if self.listing.state != LoadState::Loading {
            self.pending_select = None;
        }
    }

    pub fn set_options(&mut self, options: ViewOptions) {
        self.options = options;
        self.apply_options();
    }

    pub fn set_sort(&mut self, sort: SortOrder) {
        self.options.sort = sort;
        self.apply_options();
    }

    fn apply_options(&mut self) {
        self.listing.set_options(options_for(&self.location, self.options));
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

/// В поиске по дискам скрытое отбирает сам индекс (с учётом `hidden:да` в запросе), список
/// показывает всё, что пришло.
/// Места, где порядок задаёт выдача, а не сортировка.
fn ranked(location: &Location) -> bool {
    matches!(location, Location::Index { .. } | Location::Duplicates { .. })
}

fn options_for(location: &Location, mut options: ViewOptions) -> ViewOptions {
    if matches!(location, Location::Index { .. }) {
        options.show_hidden = true;
        options.show_system = true;
    }
    options
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
