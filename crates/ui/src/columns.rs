//! Вид «Колонки» (колонки Миллера): слева — папки, через которые сюда пришли, в середине —
//! текущая папка со всеми действиями обычного списка, справа — содержимое папки под курсором.
//!
//! Текущая колонка — это обычный список вкладки. Остальные колонки только показывают: их
//! содержимое читается теми же воркерами в общий кэш и обновляется, если устарело.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use eframe::egui::{self, Align2, CornerRadius, Rect, ScrollArea, Sense, Stroke, Ui, pos2, vec2};
use mh_files_core::Entry;
use mh_files_core::layout::PaneId;
use mh_files_core::listing::{Listing, LoadState, ViewOptions};
use mh_files_core::location::Location;
use mh_files_fs::{CancelToken, Ticket, Workers};

use crate::app::{Action, FilesApp, OWNER_COLUMNS};
use crate::pane_view::{
    Look, elided, item, paint_icon, row_font, row_height, row_icon, scroll_for,
};
use crate::tabs::Tab;
use crate::{icons, theme};

/// Уже́ колонка не бывает: лишние родители не показываются.
const MIN_WIDTH: f32 = 220.0;
/// Колонка старше — перечитывается при следующем показе.
const STALE: Duration = Duration::from_secs(30);
/// Сколько папок держать в кэше.
const CACHE_LIMIT: usize = 48;

struct Column {
    listing: Listing,
    /// Перечитывание без мигания: новое копится здесь.
    staging: Option<Vec<Entry>>,
    loaded: Instant,
    generation: u64,
    _cancel: Option<CancelToken>,
}

/// Содержимое папок для боковых колонок.
#[derive(Default)]
pub struct Cache {
    columns: HashMap<Location, Column>,
    requests: HashMap<u64, Location>,
    next: u64,
}

impl Cache {
    /// Содержимое места; нет или устарело — просится у воркеров.
    fn get(&mut self, location: &Location, workers: &Workers, options: ViewOptions) -> &Listing {
        let stale = self.columns.get(location).is_none_or(|c| {
            c.loaded.elapsed() > STALE && c.staging.is_none() && !c.listing.is_loading()
        });
        if stale {
            self.request(location, workers, options);
        }
        let column = self.columns.get_mut(location).expect("колонка запрошена");
        column.listing.set_options(options);
        &column.listing
    }

    fn request(&mut self, location: &Location, workers: &Workers, options: ViewOptions) {
        self.next += 1;
        let generation = self.next;
        let ticket = Ticket { owner: OWNER_COLUMNS, generation };
        let cancel = match location {
            Location::Dir(dir) => Some(workers.list(ticket, dir.clone())),
            Location::Archive { archive, inner } => {
                Some(workers.list_archive(ticket, archive.clone(), inner.clone()))
            }
            _ => None,
        };
        self.requests.insert(generation, location.clone());
        match self.columns.get_mut(location) {
            Some(column) => {
                column.staging = Some(Vec::new());
                column.generation = generation;
                column.loaded = Instant::now();
                column._cancel = cancel;
            }
            None => {
                self.columns.insert(
                    location.clone(),
                    Column {
                        listing: Listing::new(options),
                        staging: None,
                        loaded: Instant::now(),
                        generation,
                        _cancel: cancel,
                    },
                );
            }
        }
        if self.columns.len() > CACHE_LIMIT
            && let Some(oldest) =
                self.columns.iter().min_by_key(|(_, c)| c.loaded).map(|(l, _)| l.clone())
        {
            self.columns.remove(&oldest);
        }
    }

    fn column(&mut self, generation: u64) -> Option<&mut Column> {
        let location = self.requests.get(&generation)?;
        self.columns.get_mut(location).filter(|c| c.generation == generation)
    }

    pub fn on_batch(&mut self, generation: u64, batch: Vec<Entry>) {
        if let Some(column) = self.column(generation) {
            match &mut column.staging {
                Some(staging) => staging.extend(batch),
                None => column.listing.extend(batch),
            }
        }
    }

    pub fn on_done(&mut self, generation: u64, result: Result<(), String>) {
        if let Some(column) = self.column(generation) {
            if let Some(staging) = column.staging.take()
                && result.is_ok()
            {
                column.listing.replace(staging);
            }
            column.listing.finish(result);
            column.loaded = Instant::now();
        }
        self.requests.remove(&generation);
    }

    /// Папки изменились (свои операции) — перечитать при следующем показе.
    pub fn invalidate(&mut self, dirs: &[std::path::PathBuf]) {
        for (location, column) in &mut self.columns {
            if location.own_path().is_some_and(|p| dirs.contains(&p)) {
                column.loaded = Instant::now() - STALE * 2;
            }
        }
    }
}

/// Куда ведёт объект как колонка справа: папка, папка в архиве или сам архив.
pub fn preview_location(location: &Location, entry: &Entry) -> Option<Location> {
    if entry.is_dir() {
        return Some(location.enter(&entry.path()));
    }
    if matches!(location, Location::Dir(_)) && mh_files_fs::archive::is_archive_name(&entry.name) {
        return Some(Location::Archive { archive: entry.path(), inner: String::new() });
    }
    None
}

pub fn show(ui: &mut Ui, pane: PaneId, tab: &mut Tab, app: &mut FilesApp, focused: bool) {
    let area = ui.available_rect_before_wrap();
    let max = ((area.width() / MIN_WIDTH) as usize).max(2);
    let mut parents = Vec::new();
    let mut current = tab.location.clone();
    while parents.len() + 2 < max {
        match current.parent() {
            Some(parent) if parent.own_path().is_some() => {
                parents.push(parent.clone());
                current = parent;
            }
            _ => break,
        }
    }
    parents.reverse();
    let preview = tab
        .selection
        .cursor()
        .and_then(|path| tab.entry(path))
        .cloned()
        .map(|entry| (preview_location(&tab.location, &entry), entry));
    let count = parents.len() + 2;
    let width = (area.width() / count as f32).floor();
    let slot = |i: usize| {
        Rect::from_min_size(
            pos2(area.left() + i as f32 * width, area.top()),
            vec2(width, area.height()),
        )
    };
    if tab.columns_shown.len() < count {
        tab.columns_shown.resize(count, None);
    }
    for (i, parent) in parents.iter().enumerate() {
        let toward = parents.get(i + 1).unwrap_or(&tab.location).own_path();
        side_column(ui, tab, app, parent, toward, slot(i), i);
    }
    let middle = slot(parents.len());
    current_column(ui, pane, tab, app, focused, middle);
    let right = slot(parents.len() + 1);
    match preview {
        Some((Some(location), _)) => {
            side_column(ui, tab, app, &location, None, right, parents.len() + 1)
        }
        Some((None, entry)) => file_card(ui, app, &entry, right),
        None => {}
    }
    let painter = ui.painter();
    for i in 1..count {
        let x = area.left() + i as f32 * width;
        painter.vline(x, area.y_range(), Stroke::new(1.0, theme::CARD_STROKE));
    }
}

/// Колонка родителя или содержимого папки под курсором: только просмотр и переходы.
fn side_column(
    ui: &mut Ui,
    tab: &mut Tab,
    app: &mut FilesApp,
    location: &Location,
    toward: Option<std::path::PathBuf>,
    rect: Rect,
    slot: usize,
) {
    let options = ViewOptions { sort: Default::default(), ..app.view_options() };
    let workers = app.workers.clone();
    let (len, state, highlight) = {
        let listing = app.column_cache.get(location, &workers, options);
        let highlight = toward.as_ref().and_then(|p| listing.row_of(p));
        (listing.len(), listing.state.clone(), highlight)
    };
    let mut child = ui.new_child(egui::UiBuilder::new().max_rect(rect.shrink2(vec2(2.0, 0.0))));
    child.set_clip_rect(rect.intersect(ui.clip_rect()));
    if len == 0 {
        let text = match state {
            LoadState::Loading => return,
            LoadState::Failed(error) => error,
            LoadState::Done => "Пусто".to_string(),
        };
        let galley =
            elided(&child, &text, theme::regular(13.0), theme::TEXT_DISABLED, rect.width() - 20.0);
        child.painter().galley(
            pos2(rect.left() + 10.0, rect.top() + 12.0),
            galley,
            theme::TEXT_DISABLED,
        );
        return;
    }
    let row_height = row_height(app);
    let mut scroll =
        ScrollArea::vertical().id_salt(("column", tab.id, slot)).auto_shrink([false, false]);
    // Колонка сменила папку — прокрутить к дороге сюда.
    if tab.columns_shown[slot].as_ref() != Some(location) {
        tab.columns_shown[slot] = Some(location.clone());
        let row = highlight.unwrap_or(0) as f32;
        scroll = scroll.vertical_scroll_offset((row * row_height - rect.height() / 3.0).max(0.0));
    }
    child.spacing_mut().item_spacing.y = 0.0;
    scroll.show_rows(&mut child, row_height, len, |ui, rows| {
        ui.spacing_mut().item_spacing.y = 0.0;
        let entries: Vec<(usize, Entry)> = {
            let listing = app.column_cache.get(location, &workers, options);
            rows.filter_map(|row| listing.get(row).cloned().map(|e| (row, e))).collect()
        };
        for (row, entry) in entries {
            let (rect, response) =
                ui.allocate_exact_size(vec2(ui.available_width(), row_height), Sense::click());
            let on_path = highlight == Some(row);
            let body = rect.shrink2(vec2(2.0, 1.0));
            let fill = if on_path {
                theme::selection_fill().gamma_multiply(0.7)
            } else if response.hovered() {
                theme::CARD.gamma_multiply(0.8)
            } else {
                egui::Color32::TRANSPARENT
            };
            ui.painter().rect_filled(body, CornerRadius::same(4), fill);
            let icon_size = row_icon(app, rect.height());
            let icon = Rect::from_center_size(
                pos2(rect.left() + 6.0 + icon_size / 2.0, rect.center().y),
                vec2(icon_size, icon_size),
            );
            let dim = entry.hidden();
            paint_icon(ui, app, &entry, icon, dim);
            let color = if dim {
                theme::TEXT_DISABLED
            } else if on_path {
                theme::TEXT_PRIMARY
            } else {
                theme::TEXT_SECONDARY.gamma_multiply(1.15)
            };
            let right = if entry.is_dir() { rect.right() - 22.0 } else { rect.right() - 8.0 };
            let text_left = rect.left() + 10.0 + icon_size;
            let galley = elided(ui, &entry.name, row_font(app, 14.0), color, right - text_left);
            ui.painter().galley(
                pos2(text_left, rect.center().y - galley.size().y / 2.0),
                galley,
                color,
            );
            if entry.is_dir() {
                let chevron = Rect::from_center_size(
                    pos2(rect.right() - 12.0, rect.center().y),
                    vec2(10.0, 10.0),
                );
                icons::chevron_right(ui.painter(), chevron, theme::TEXT_DISABLED);
            }
            let path = entry.path();
            if response.clicked() {
                // Папка — открыть её; файл — перейти в его папку и встать на него.
                let action = match preview_location(location, &entry) {
                    Some(inside) if entry.is_dir() => {
                        Action::OpenSelect { location: inside, select: None }
                    }
                    _ => Action::OpenSelect {
                        location: location.clone(),
                        select: Some(path.clone()),
                    },
                };
                app.actions.push(action);
            }
            if crate::widgets::double_clicked(&response) && !entry.is_dir() {
                app.actions.push(Action::Run(crate::commands::CommandId::Open));
            }
            response.on_hover_text(&entry.name);
        }
    });
}

/// Текущая колонка: обычный список вкладки со всеми действиями.
fn current_column(
    ui: &mut Ui,
    pane: PaneId,
    tab: &mut Tab,
    app: &mut FilesApp,
    focused: bool,
    rect: Rect,
) {
    let mut child = ui.new_child(egui::UiBuilder::new().max_rect(rect.shrink2(vec2(2.0, 0.0))));
    child.set_clip_rect(rect.intersect(ui.clip_rect()));
    child.painter().rect_filled(
        rect,
        CornerRadius::ZERO,
        theme::WINDOW_BACKGROUND.gamma_multiply(0.6),
    );
    if tab.listing.is_empty() {
        let text = match &tab.listing.state {
            LoadState::Loading => None,
            LoadState::Failed(error) => Some(error.clone()),
            LoadState::Done if tab.listing.has_filter() => {
                Some("Ничего не подходит под фильтр".into())
            }
            LoadState::Done => Some("Папка пуста".to_string()),
        };
        match text {
            Some(text) => {
                let galley = elided(
                    &child,
                    &text,
                    theme::regular(13.0),
                    theme::TEXT_DISABLED,
                    rect.width() - 20.0,
                );
                child.painter().galley(
                    pos2(rect.left() + 10.0, rect.top() + 12.0),
                    galley,
                    theme::TEXT_DISABLED,
                );
            }
            None => {
                let spinner =
                    Rect::from_center_size(rect.center_top() + vec2(0.0, 30.0), vec2(20.0, 20.0));
                egui::Spinner::new().color(theme::accent()).paint_at(&child, spinner);
            }
        }
        return;
    }
    let row_height = row_height(app);
    let mut scroll =
        ScrollArea::vertical().id_salt(("column-current", tab.id)).auto_shrink([false, false]);
    if let Some(row) = tab.scroll_to.take() {
        scroll =
            scroll.vertical_scroll_offset(scroll_for(tab, row as f32 * row_height, row_height));
    }
    let count = tab.listing.len();
    child.spacing_mut().item_spacing.y = 0.0;
    let output = scroll.show_rows(&mut child, row_height, count, |ui, rows| {
        ui.spacing_mut().item_spacing.y = 0.0;
        for row in rows {
            let (rect, _) =
                ui.allocate_exact_size(vec2(ui.available_width(), row_height), Sense::hover());
            item(ui, pane, tab, app, focused, row, rect, Look::Column);
        }
    });
    tab.scroll_offset = output.state.offset.y;
    tab.viewport_height = output.inner_rect.height();
    tab.list_top = output.inner_rect.top();
    tab.page_rows = ((output.inner_rect.height() / row_height) as usize).saturating_sub(1).max(1);
}

/// Справа от файла — его карточка: крупный значок, имя, размер.
fn file_card(ui: &mut Ui, app: &mut FilesApp, entry: &Entry, rect: Rect) {
    let icon = Rect::from_center_size(rect.center_top() + vec2(0.0, 80.0), vec2(64.0, 64.0));
    paint_icon(ui, app, entry, icon, false);
    let galley = crate::pane_view::wrapped(
        ui,
        &entry.name,
        theme::regular(14.0),
        theme::TEXT_PRIMARY,
        rect.width() - 24.0,
        3,
    );
    ui.painter().galley(pos2(rect.center().x, icon.bottom() + 12.0), galley, theme::TEXT_PRIMARY);
    ui.painter().text(
        pos2(rect.center().x, icon.bottom() + 70.0),
        Align2::CENTER_TOP,
        mh_files_core::format::size(entry.size),
        theme::regular(13.0),
        theme::TEXT_SECONDARY,
    );
}
