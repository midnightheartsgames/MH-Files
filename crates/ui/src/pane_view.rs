//! Одна панель: вкладки, навигация, строка пути, фильтр и список — таблица или плитки.
//!
//! Список виртуальный: рисуются только видимые строки, поэтому сто тысяч записей прокручиваются
//! так же, как десять. Ввод-вывода здесь нет: всё, что нужно с диска, просится у воркеров.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use eframe::egui::{
    self, Align, Align2, Color32, CornerRadius, FontId, Galley, Id, Layout, Rect, RichText,
    ScrollArea, Sense, Stroke, TextureHandle, Ui, pos2, text::LayoutJob, vec2,
};
use mh_files_core::Entry;
use mh_files_core::entry::stem_of;
use mh_files_core::format;
use mh_files_core::listing::LoadState;
use mh_files_core::location::{Location, path_label};
use mh_files_core::selection::Modifiers;
use mh_files_core::session::ViewMode;
use mh_files_core::sort::SortColumn;
use mh_files_platform::shell::MenuTarget;

use crate::app::{Action, DragFiles, DragTab, DropZone, FilesApp, Target};
use crate::commands::CommandId;
use crate::tabs::{Band, InlineRename, Pane, Tab};
use crate::{icons, theme, widgets};

pub const TAB_HEIGHT: f32 = 30.0;
const NAV_HEIGHT: f32 = 34.0;
const HEADER_HEIGHT: f32 = 24.0;

pub fn show(ui: &mut Ui, pane: &mut Pane, app: &mut FilesApp, focused: bool) {
    ui.spacing_mut().item_spacing = vec2(4.0, 0.0);
    tab_strip(ui, pane, app, focused);
    let pane_id = pane.id;
    let tab = &mut pane.tabs[pane.active];
    nav_bar(ui, pane_id, tab, app);
    if tab.filter_open {
        filter_bar(ui, tab, app);
    }
    if let Location::Search { root, query, .. } = &tab.location {
        search_header(ui, tab.search.as_ref(), root, query, tab.listing.len());
    }
    if matches!(tab.location, Location::Index { .. }) {
        index_bar(ui, tab, app);
    }
    if matches!(tab.location, Location::Duplicates { .. }) {
        duplicates_bar(ui, tab, app);
    }
    let rect = ui.available_rect_before_wrap();
    let mut content =
        ui.new_child(egui::UiBuilder::new().max_rect(rect).layout(Layout::top_down(Align::Min)));
    match &tab.location {
        Location::Computer => computer_view(&mut content, app),
        Location::Sort { .. } => crate::sorter::show(&mut content, tab, app),
        _ => list_view(&mut content, pane_id, tab, app, focused),
    }
}

// ── Вкладки ──────────────────────────────────────────────────────────────────────────

fn tab_strip(ui: &mut Ui, pane: &mut Pane, app: &mut FilesApp, focused: bool) {
    let (strip, _) = ui.allocate_exact_size(vec2(ui.available_width(), TAB_HEIGHT), Sense::hover());
    let painter = ui.painter_at(strip);
    painter.rect_filled(strip, CornerRadius::ZERO, theme::PANEL);
    let menu_width = 30.0;
    let plus_width = 28.0;
    let space = (strip.width() - menu_width - plus_width - 8.0).max(60.0);
    let count = pane.tabs.len().max(1);
    let width = (space / count as f32).clamp(70.0, 210.0);
    let mut x = strip.left() + 4.0;
    for index in 0..pane.tabs.len() {
        let rect =
            Rect::from_min_size(pos2(x, strip.top() + 3.0), vec2(width - 2.0, TAB_HEIGHT - 3.0));
        x += width;
        let tab = &pane.tabs[index];
        let active = index == pane.active;
        let id = Id::new(("tab", tab.id));
        let response = ui.interact(rect, id, Sense::click_and_drag());
        if response.drag_started() {
            egui::DragAndDrop::set_payload(ui.ctx(), DragTab { pane: pane.id, tab: tab.id });
        }
        let hovered = response.hovered();
        let fill = if active {
            theme::WINDOW_BACKGROUND
        } else if hovered {
            theme::CARD
        } else {
            Color32::TRANSPARENT
        };
        let radius = CornerRadius { nw: 5, ne: 5, sw: 0, se: 0 };
        painter.rect_filled(rect, radius, fill);
        if active && focused {
            let bar = Rect::from_min_size(rect.min, vec2(rect.width(), 2.0));
            painter.rect_filled(bar, CornerRadius { nw: 5, ne: 5, sw: 0, se: 0 }, theme::accent());
        }
        let icon =
            Rect::from_center_size(pos2(rect.left() + 14.0, rect.center().y), vec2(14.0, 14.0));
        match tab.location {
            Location::Computer => icons::computer(&painter, icon, theme::TEXT_SECONDARY),
            Location::Search { .. } => icons::search(&painter, icon, theme::TEXT_SECONDARY),
            Location::Index { .. } => icons::search(&painter, icon, theme::accent()),
            Location::Archive { .. } => icons::archive(&painter, icon, theme::accent()),
            Location::Duplicates { .. } => icons::duplicates(&painter, icon, theme::accent()),
            Location::Sort { .. } => icons::sort(&painter, icon, theme::accent()),
            Location::Dir(_) => icons::folder(&painter, icon, theme::accent().gamma_multiply(0.8)),
        }
        let close_rect =
            Rect::from_center_size(pos2(rect.right() - 12.0, rect.center().y), vec2(16.0, 16.0));
        let text_width = rect.width() - 28.0 - if active || hovered { 20.0 } else { 6.0 };
        let color = if active { theme::TEXT_PRIMARY } else { theme::TEXT_SECONDARY };
        let galley = elided(ui, &tab.title(), theme::regular(13.5), color, text_width);
        painter.galley(
            pos2(rect.left() + 26.0, rect.center().y - galley.size().y / 2.0),
            galley,
            color,
        );
        if let Some(dir) = tab.dir() {
            app.drop_zones.push(DropZone { rect, dir, priority: 3, favorite_group: None });
        }
        let mut close = false;
        if active || hovered {
            let close_response = ui.interact(close_rect, id.with("close"), Sense::click());
            let color =
                if close_response.hovered() { theme::TEXT_PRIMARY } else { theme::TEXT_DISABLED };
            icons::close(&painter, close_rect.shrink(4.0), color);
            close = close_response.clicked();
        }
        if close || response.middle_clicked() {
            app.actions.push(Action::CloseTab { pane: pane.id, index });
        } else if response.clicked() {
            app.actions.push(Action::SelectTab { pane: pane.id, index });
        }
        let tab_title = tab.location.clone();
        response.on_hover_text(match &tab_title {
            Location::Dir(path) => path.display().to_string(),
            other => other.title(),
        });
    }
    let plus = Rect::from_min_size(pos2(x + 2.0, strip.top() + 4.0), vec2(24.0, 24.0));
    let plus_response = ui.interact(plus, Id::new(("new-tab", pane.id.0)), Sense::click());
    if plus_response.hovered() {
        painter.rect_filled(plus, CornerRadius::same(4), theme::CARD);
    }
    icons::plus(&painter, plus.shrink(7.0), theme::TEXT_SECONDARY);
    if plus_response.on_hover_text("Новая вкладка (Ctrl+T)").clicked() {
        app.actions.push(Action::NewTabIn(pane.id));
    }
    let menu =
        Rect::from_min_size(pos2(strip.right() - menu_width, strip.top() + 4.0), vec2(26.0, 24.0));
    let menu_response = ui.interact(menu, Id::new(("pane-menu", pane.id.0)), Sense::click());
    if menu_response.hovered() {
        painter.rect_filled(menu, CornerRadius::same(4), theme::CARD);
    }
    icons::dots(&painter, menu.shrink(7.0), theme::TEXT_SECONDARY);
    egui::Popup::menu(&menu_response).show(|ui| {
        ui.set_min_width(260.0);
        for command in [
            CommandId::NewTab,
            CommandId::DuplicateTab,
            CommandId::ReopenTab,
            CommandId::SplitRight,
            CommandId::SplitDown,
            CommandId::ClosePane,
        ] {
            menu_item(ui, app, command);
        }
        ui.separator();
        for command in [
            CommandId::ToggleSidebar,
            CommandId::ToggleInspector,
            CommandId::ToggleHidden,
            CommandId::CommandPalette,
            CommandId::GoTo,
            CommandId::Settings,
        ] {
            menu_item(ui, app, command);
        }
    });
}

/// Пункт меню: имя команды, сочетание справа, недоступные — серые.
pub fn menu_item(ui: &mut Ui, app: &mut FilesApp, command: CommandId) {
    let enabled = app.available_cached(command);
    let shortcut = app.keymap.label(command);
    let button = egui::Button::new(command.name())
        .shortcut_text(RichText::new(shortcut).color(theme::TEXT_DISABLED));
    if ui.add_enabled(enabled, button).clicked() {
        app.actions.push(Action::Run(command));
        ui.close();
    }
}

// ── Навигация и строка пути ──────────────────────────────────────────────────────────

fn nav_bar(ui: &mut Ui, pane: mh_files_core::layout::PaneId, tab: &mut Tab, app: &mut FilesApp) {
    let (bar, _) = ui.allocate_exact_size(vec2(ui.available_width(), NAV_HEIGHT), Sense::hover());
    ui.painter().rect_filled(bar, CornerRadius::ZERO, theme::WINDOW_BACKGROUND);
    let mut row = ui.new_child(
        egui::UiBuilder::new()
            .max_rect(bar.shrink2(vec2(6.0, 4.0)))
            .layout(Layout::left_to_right(Align::Center)),
    );
    let ui = &mut row;
    ui.spacing_mut().item_spacing.x = 2.0;
    let run = |app: &mut FilesApp, command| {
        app.actions.push(Action::FocusPane(pane));
        app.actions.push(Action::Run(command));
    };
    if widgets::icon_button(ui, tab.history.can_back(), "Назад (Alt+Left)", icons::arrow_left)
        .clicked()
    {
        run(app, CommandId::GoBack);
    }
    if widgets::icon_button(ui, tab.history.can_forward(), "Вперёд (Alt+Right)", icons::arrow_right)
        .clicked()
    {
        run(app, CommandId::GoForward);
    }
    if widgets::icon_button(ui, tab.location.parent().is_some(), "Вверх (Alt+Up)", icons::arrow_up)
        .clicked()
    {
        run(app, CommandId::GoUp);
    }
    if widgets::icon_button(ui, true, "Обновить (F5)", icons::refresh).clicked() {
        run(app, CommandId::Refresh);
    }
    ui.add_space(4.0);
    let right = 3.0 * 28.0;
    let width = (ui.available_width() - right).max(80.0);
    let (crumb_rect, _) = ui.allocate_exact_size(vec2(width, 26.0), Sense::hover());
    if tab.address.is_some() {
        address_field(ui, crumb_rect, pane, tab, app);
    } else {
        breadcrumbs(ui, crumb_rect, pane, tab, app);
    }
    ui.add_space(4.0);
    let filter_tip = "Фильтр (Ctrl+F)";
    if widgets::icon_button(ui, true, filter_tip, icons::search).clicked() {
        if tab.filter_open {
            tab.filter_open = false;
            tab.set_filter(String::new());
        } else {
            tab.filter_open = true;
            tab.focus_filter = true;
        }
    }
    // Кнопка показывает следующий вид по кругу: таблица → плитки → колонки.
    type Icon = fn(&egui::Painter, Rect, Color32);
    let (icon, tip, next) = match tab.view {
        ViewMode::Details => (icons::grid as Icon, "Плитки (Ctrl+2)", ViewMode::Grid),
        ViewMode::Grid => (icons::columns as Icon, "Колонки (Ctrl+3)", ViewMode::Columns),
        ViewMode::Columns => (icons::list as Icon, "Таблица (Ctrl+1)", ViewMode::Details),
    };
    if widgets::icon_button(ui, true, tip, icon).clicked() {
        tab.view = next;
    }
    let inspector_tip = "Инспектор (Alt+P)";
    if widgets::icon_button(ui, true, inspector_tip, icons::inspector).clicked() {
        app.actions.push(Action::Run(CommandId::ToggleInspector));
    }
}

fn breadcrumbs(
    ui: &mut Ui,
    rect: Rect,
    pane: mh_files_core::layout::PaneId,
    tab: &mut Tab,
    app: &mut FilesApp,
) {
    ui.painter().rect_filled(rect, CornerRadius::same(4), theme::FIELD);
    // Щелчок по пустому месту строки — ввод пути, как в Проводнике.
    let background = ui.interact(rect, Id::new(("crumbs-bg", tab.id)), Sense::click());
    if background.clicked() {
        app.actions.push(Action::FocusPane(pane));
        app.actions.push(Action::Run(CommandId::EditAddress));
    }
    let mut child = ui.new_child(
        egui::UiBuilder::new()
            .max_rect(rect.shrink2(vec2(4.0, 0.0)))
            .layout(Layout::left_to_right(Align::Center)),
    );
    child.set_clip_rect(rect);
    ScrollArea::horizontal()
        .id_salt(("crumbs", tab.id))
        .stick_to_right(true)
        .scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::AlwaysHidden)
        .show(&mut child, |ui| {
            ui.horizontal_centered(|ui| {
                ui.spacing_mut().item_spacing.x = 0.0;
                let crumbs = tab.location.crumbs();
                let last = crumbs.len() - 1;
                for (index, crumb) in crumbs.into_iter().enumerate() {
                    let color =
                        if index == last { theme::TEXT_PRIMARY } else { theme::TEXT_SECONDARY };
                    let text = RichText::new(&crumb.label).font(theme::regular(14.0)).color(color);
                    let response =
                        ui.add(egui::Button::new(text).frame(false).min_size(vec2(0.0, 22.0)));
                    if response.clicked() && index != last {
                        app.actions.push(Action::FocusPane(pane));
                        app.actions.push(Action::Open {
                            location: crumb.location.clone(),
                            target: Target::Current,
                        });
                    }
                    if response.middle_clicked() {
                        app.actions.push(Action::FocusPane(pane));
                        app.actions.push(Action::Open {
                            location: crumb.location.clone(),
                            target: Target::NewTab,
                        });
                    }
                    if let Location::Dir(dir) = &crumb.location {
                        app.drop_zones.push(DropZone {
                            rect: response.rect,
                            dir: dir.clone(),
                            priority: 3,
                            favorite_group: None,
                        });
                        let path_text = dir.display().to_string();
                        response.context_menu(|ui| {
                            if ui.button("Открыть в новой вкладке").clicked() {
                                app.actions.push(Action::Open {
                                    location: crumb.location.clone(),
                                    target: Target::NewTab,
                                });
                                ui.close();
                            }
                            if ui.button("Копировать путь").clicked() {
                                ui.ctx().copy_text(path_text.clone());
                                ui.close();
                            }
                        });
                    }
                    // Стрелка после звена — подпапки этого звена.
                    let dir = match &crumb.location {
                        Location::Dir(dir) => Some(dir.clone()),
                        _ => None,
                    };
                    if index < last || dir.is_some() {
                        let (arrow, arrow_response) =
                            ui.allocate_exact_size(vec2(16.0, 22.0), Sense::click());
                        let color = if arrow_response.hovered() {
                            theme::accent()
                        } else {
                            theme::TEXT_DISABLED
                        };
                        icons::chevron_right(ui.painter(), arrow.shrink2(vec2(3.0, 6.0)), color);
                        if arrow_response.clicked()
                            && let Some(dir) = dir
                        {
                            app.actions.push(Action::CrumbMenu {
                                pane,
                                dir,
                                at: arrow.left_bottom(),
                            });
                        }
                    }
                }
            });
        });
}

fn address_field(
    ui: &mut Ui,
    rect: Rect,
    pane: mh_files_core::layout::PaneId,
    tab: &mut Tab,
    app: &mut FilesApp,
) {
    let Some(text) = &mut tab.address else { return };
    let id = Id::new(("address", tab.id));
    let response = ui.put(
        rect,
        egui::TextEdit::singleline(text)
            .id(id)
            .font(theme::regular(14.0))
            .hint_text("Путь, %переменная% или \\\\сервер\\ресурс")
            .margin(egui::Margin::symmetric(6, 3)),
    );
    if std::mem::take(&mut tab.focus_address) {
        response.request_focus();
        select_all(ui.ctx(), id, text.chars().count());
    }
    let enter = response.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
    if enter {
        let expanded = mh_files_core::goto::expand(text);
        if !expanded.is_empty() {
            let path = mh_files_core::location::normalize(Path::new(&expanded));
            app.actions.push(Action::FocusPane(pane));
            app.actions
                .push(Action::Open { location: Location::Dir(path), target: Target::Current });
        }
        tab.address = None;
    } else if response.lost_focus() {
        tab.address = None;
    }
}

/// Список подпапок под стрелкой строки пути.
pub fn crumb_menu(ctx: &egui::Context, app: &mut FilesApp) {
    let Some(menu) = &app.crumb_menu else { return };
    let (pane, dir, at) = (menu.pane, menu.dir.clone(), menu.at);
    let names = menu.names.clone();
    let mut close = false;
    let area = egui::Area::new(Id::new("crumb-menu"))
        .order(egui::Order::Foreground)
        .fixed_pos(at)
        .show(ctx, |ui| {
            egui::Frame::popup(ui.style()).show(ui, |ui| {
                ui.set_min_width(200.0);
                match &names {
                    None => {
                        ui.label(RichText::new("Загрузка…").color(theme::TEXT_SECONDARY));
                    }
                    Some(names) if names.is_empty() => {
                        ui.label(RichText::new("Нет подпапок").color(theme::TEXT_SECONDARY));
                    }
                    Some(names) => {
                        ScrollArea::vertical().max_height(420.0).show(ui, |ui| {
                            for name in names {
                                if ui.add(egui::Button::new(name).frame(false)).clicked() {
                                    app.actions.push(Action::FocusPane(pane));
                                    app.actions.push(Action::Open {
                                        location: Location::Dir(dir.join(name)),
                                        target: Target::Current,
                                    });
                                    close = true;
                                }
                            }
                        });
                    }
                }
            });
        });
    let clicked_outside = ctx.input(|i| i.pointer.any_pressed())
        && !area
            .response
            .rect
            .contains(ctx.input(|i| i.pointer.interact_pos()).unwrap_or_default());
    if close
        || ctx.input(|i| i.key_pressed(egui::Key::Escape))
        || (clicked_outside
            && area.response.rect.width() > 0.0
            && !area.response.contains_pointer())
    {
        app.crumb_menu = None;
    }
}

fn filter_bar(ui: &mut Ui, tab: &mut Tab, app: &mut FilesApp) {
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 32.0), Sense::hover());
    ui.painter().rect_filled(rect, CornerRadius::ZERO, theme::WINDOW_BACKGROUND);
    let mut row = ui.new_child(
        egui::UiBuilder::new()
            .max_rect(rect.shrink2(vec2(8.0, 3.0)))
            .layout(Layout::left_to_right(Align::Center)),
    );
    let ui = &mut row;
    icons::filter(
        ui.painter(),
        Rect::from_center_size(ui.cursor().left_center() + vec2(8.0, 0.0), vec2(14.0, 14.0)),
        theme::accent(),
    );
    ui.add_space(22.0);
    let id = Id::new(("filter", tab.id));
    let mut text = tab.filter.clone();
    let response = ui.add(
        egui::TextEdit::singleline(&mut text)
            .id(id)
            .desired_width((ui.available_width() - 160.0).max(100.0))
            .hint_text("Фильтр: слова в любом порядке или маска *.png")
            .margin(egui::Margin::symmetric(6, 3)),
    );
    if std::mem::take(&mut tab.focus_filter) {
        response.request_focus();
    }
    if response.changed() {
        tab.set_filter(text);
    }
    if response.has_focus() {
        let (down, up, enter, escape) = ui.input(|i| {
            (
                i.key_pressed(egui::Key::ArrowDown),
                i.key_pressed(egui::Key::ArrowUp),
                i.key_pressed(egui::Key::Enter),
                i.key_pressed(egui::Key::Escape),
            )
        });
        if down {
            tab.move_cursor(1, Modifiers::default());
        }
        if up {
            tab.move_cursor(-1, Modifiers::default());
        }
        if escape {
            tab.filter_open = false;
            tab.set_filter(String::new());
        }
        let _ = enter;
    } else if response.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
        app.actions.push(Action::Run(CommandId::Open));
    }
    let shown = tab.listing.len();
    let total = tab.listing.total_len();
    ui.label(RichText::new(format!("{shown} из {total}")).color(theme::TEXT_SECONDARY));
    if widgets::icon_button(ui, true, "Закрыть фильтр (Esc)", icons::close).clicked() {
        tab.filter_open = false;
        tab.set_filter(String::new());
    }
}

fn search_header(
    ui: &mut Ui,
    progress: Option<&crate::tabs::SearchProgress>,
    root: &Path,
    query: &str,
    found: usize,
) {
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 28.0), Sense::hover());
    let painter = ui.painter();
    painter.rect_filled(rect, CornerRadius::ZERO, theme::WINDOW_BACKGROUND);
    let (text, color) = match progress {
        Some(p) if p.error.is_some() => (
            format!(
                "«{query}» в {}: {} — {}",
                path_label(root),
                format::items(found),
                p.error.as_deref().unwrap_or("")
            ),
            theme::WARN,
        ),
        Some(p) if p.done => (
            format!(
                "«{query}» в {}: {} · просмотрено папок: {}",
                path_label(root),
                format::items(found),
                p.scanned
            ),
            theme::TEXT_SECONDARY,
        ),
        Some(p) => (
            format!("Поиск «{query}»… {} · папок: {}", format::items(found), p.scanned),
            theme::accent(),
        ),
        None => (String::new(), theme::TEXT_SECONDARY),
    };
    painter.text(
        rect.left_center() + vec2(10.0, 0.0),
        Align2::LEFT_CENTER,
        text,
        theme::regular(13.0),
        color,
    );
}

/// Поле поиска по дискам и итог: сколько найдено, за сколько, в каком состоянии индекс.
fn index_bar(ui: &mut Ui, tab: &mut Tab, app: &mut FilesApp) {
    let Some(view) = &mut tab.index else { return };
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 62.0), Sense::hover());
    ui.painter().rect_filled(rect, CornerRadius::ZERO, theme::WINDOW_BACKGROUND);
    let field = Rect::from_min_size(rect.min + vec2(10.0, 6.0), vec2(rect.width() - 150.0, 30.0));
    ui.painter().rect_filled(field, CornerRadius::same(5), theme::CARD);
    ui.painter().rect_stroke(
        field,
        CornerRadius::same(5),
        Stroke::new(1.0, theme::accent().gamma_multiply(0.6)),
        egui::StrokeKind::Inside,
    );
    icons::search(
        ui.painter(),
        Rect::from_center_size(pos2(field.left() + 15.0, field.center().y), vec2(14.0, 14.0)),
        theme::accent(),
    );
    let edit_rect = Rect::from_min_max(field.min + vec2(30.0, 0.0), field.max - vec2(8.0, 0.0));
    let mut text = view.text.clone();
    let edit = ui.place(
        edit_rect,
        egui::TextEdit::singleline(&mut text)
            .id(Id::new(("index-query", tab.id)))
                        .frame(egui::Frame::NONE)
            .vertical_align(Align::Center)
            .font(theme::regular(14.0))
            .hint_text("Имя, маска или фильтры: ext:pdf size:>10mb dm:неделя type:папка in:\"C:\\Проекты\"")
            .desired_width(edit_rect.width()),
    );
    if std::mem::take(&mut view.focus) {
        edit.request_focus();
    }
    let leave = edit.has_focus()
        && ui.input(|i| i.key_pressed(egui::Key::Enter) || i.key_pressed(egui::Key::ArrowDown));
    let save_rect = Rect::from_min_size(pos2(field.right() + 8.0, field.top()), vec2(130.0, 30.0));
    let can_save = !text.trim().is_empty();
    let save = ui.place(
        save_rect,
        egui::Button::new(RichText::new("Сохранить поиск").font(theme::regular(13.5)))
            .corner_radius(CornerRadius::same(5)),
    );
    let save = save.on_hover_text("В боковую панель, раздел «Поиски»");
    if save.clicked() && can_save {
        app.actions.push(Action::Run(CommandId::SaveSearch));
    }

    let status = app.indexer.status();
    let (line, color) = if let Some(error) = &view.error {
        (error.clone(), theme::WARN)
    } else if !status.enabled {
        (
            "Индекс выключен — включите его в настройках, раздел «Поиск по дискам»".to_string(),
            theme::WARN,
        )
    } else if text.trim().is_empty() {
        (
            format!("В индексе {} · {}", format::items(status.entries()), index_state(&status)),
            theme::TEXT_SECONDARY,
        )
    } else {
        let shown = tab.listing.len();
        let mut line = if view.total > shown {
            format!(
                "Найдено {} — показаны первые {}",
                format::count(view.total),
                format::count(shown)
            )
        } else {
            format!("Найдено: {}", format::count(view.total))
        };
        line.push_str(&format!(" · {} мс", view.elapsed.as_millis()));
        if status.busy() {
            line.push_str(&format!(" · {} — результаты неполные", index_state(&status)));
        }
        (line, if status.busy() { theme::accent() } else { theme::TEXT_SECONDARY })
    };
    ui.painter().text(
        pos2(rect.left() + 12.0, rect.bottom() - 13.0),
        Align2::LEFT_CENTER,
        line,
        theme::regular(13.0),
        color,
    );
    if text != view.text {
        tab.set_index_query(text, &app.workers);
    }
    if leave {
        edit.surrender_focus();
        tab.cursor_to(0, Modifiers::default());
    }
}

/// Ход поиска дубликатов, итог и отметка лишних копий.
/// Пороги размера для поиска дубликатов.
const MIN_SIZES: [(u64, &str); 7] = [
    (1, "любые"),
    (4 << 10, "от 4 КБ"),
    (100 << 10, "от 100 КБ"),
    (1 << 20, "от 1 МБ"),
    (10 << 20, "от 10 МБ"),
    (100 << 20, "от 100 МБ"),
    (1 << 30, "от 1 ГБ"),
];

fn duplicates_bar(ui: &mut Ui, tab: &mut Tab, app: &mut FilesApp) {
    use mh_files_core::duplicates::{Keep, totals};
    use mh_files_fs::DuplicateProgress;
    let Some(view) = &mut tab.duplicates else { return };
    let (text, color) = if let Some(error) = &view.error {
        (error.clone(), theme::WARN)
    } else if view.done {
        let (groups, extra, wasted) = totals(&view.groups);
        if groups == 0 {
            ("Дубликатов нет".to_string(), theme::TEXT_SECONDARY)
        } else {
            (
                format!(
                    "Групп: {} · лишних копий: {} · можно освободить {}",
                    format::count(groups),
                    format::count(extra),
                    format::size(wasted)
                ),
                theme::TEXT_PRIMARY,
            )
        }
    } else {
        let text = match view.progress {
            None => "Обход папок…".to_string(),
            Some(DuplicateProgress::Scanning { files }) => {
                format!("Обход папок: {}", format::items(files))
            }
            Some(DuplicateProgress::Hashing { done, total }) => {
                format!("Сравнение содержимого: {} из {}", format::size(done), format::size(total))
            }
        };
        ui.ctx().request_repaint_after(std::time::Duration::from_millis(150));
        (text, theme::accent())
    };
    let mut rerun = false;
    let mut link = false;
    egui::Frame::new()
        .fill(theme::WINDOW_BACKGROUND)
        .inner_margin(egui::Margin::symmetric(12, 6))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                ui.label(RichText::new(text).font(theme::regular(13.5)).color(color));
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    // Исключения: по одному на строку, применяются поиском заново.
                    let rules = &app.settings.duplicates.exclude;
                    let label = match rules.len() {
                        0 => "Исключения".to_string(),
                        n => format!("Исключения: {n}"),
                    };
                    let text = view.exclude_text.get_or_insert_with(|| rules.join("\n"));
                    let mut apply = false;
                    ui.menu_button(label, |ui| {
                        ui.set_width(300.0);
                        ui.label(
                            RichText::new("По одному на строку: имя (node_modules), маска (*.tmp) или полный путь папки.")
                                .color(theme::TEXT_SECONDARY),
                        );
                        ui.add(egui::TextEdit::multiline(text).desired_rows(5).desired_width(280.0));
                        if ui.button("Искать заново").clicked() {
                            apply = true;
                            ui.close();
                        }
                    });
                    if apply {
                        app.settings.duplicates.exclude = text
                            .lines()
                            .map(str::trim)
                            .filter(|line| !line.is_empty())
                            .map(String::from)
                            .collect();
                        rerun = true;
                    }
                    let current = app.settings.duplicates.min_size.max(1);
                    let selected = MIN_SIZES
                        .iter()
                        .find(|(size, _)| *size == current)
                        .map_or_else(|| format!("от {}", format::size(current)), |(_, l)| l.to_string());
                    egui::ComboBox::from_id_salt(("dup-min", tab.id))
                        .selected_text(selected)
                        .width(110.0)
                        .show_ui(ui, |ui| {
                            for (size, label) in MIN_SIZES {
                                if ui.selectable_label(size == current, label).clicked()
                                    && size != current
                                {
                                    app.settings.duplicates.min_size = size;
                                    rerun = true;
                                }
                            }
                        });
                    ui.label(RichText::new("Файлы").color(theme::TEXT_SECONDARY));
                });
            });
            if view.done && !view.groups.is_empty() {
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    egui::ComboBox::from_id_salt(("dup-keep", tab.id))
                        .selected_text(view.keep.title())
                        .width(230.0)
                        .show_ui(ui, |ui| {
                            for keep in Keep::ALL {
                                ui.selectable_value(&mut view.keep, keep, keep.title());
                            }
                        });
                    if ui.button("Отметить лишние").on_hover_text("Delete отправит их в корзину").clicked() {
                        app.actions.push(Action::Run(CommandId::SelectExtraCopies));
                    }
                    if ui
                        .button("Жёсткие ссылки вместо копий…")
                        .on_hover_text("Лишние копии остаются на местах, но перестают занимать место")
                        .clicked()
                    {
                        link = true;
                    }
                });
            }
        });
    if link && let Some(view) = &tab.duplicates {
        let pairs: Vec<_> = view
            .groups
            .iter()
            .flat_map(|group| {
                group
                    .link_plan(view.keep)
                    .into_iter()
                    .map(|(keeper, extra)| (keeper, extra, group.size))
            })
            .collect();
        if !pairs.is_empty() {
            app.dialog = Some(crate::dialogs::Dialog::HardLinks { tab: tab.id, pairs });
        }
    }
    if rerun {
        app.save_settings();
        tab.duplicate_options = app.duplicate_options();
        tab.reload(&app.workers, false);
    }
}

/// Состояние индекса одной фразой.
pub fn index_state(status: &mh_files_fs::IndexStatus) -> String {
    use mh_files_fs::VolumeState;
    let mut parts = Vec::new();
    for volume in &status.volumes {
        let label = path_label(&volume.root);
        match &volume.state {
            VolumeState::Loading => parts.push(format!("{label}: загрузка")),
            VolumeState::ReadingMft => parts.push(format!("{label}: чтение MFT")),
            VolumeState::Scanning { dirs } => {
                parts.push(format!("{label}: обход, папок {}", format::count(*dirs)))
            }
            VolumeState::CatchingUp { done, total } => parts.push(format!(
                "{label}: досмотр изменений {}/{}",
                format::count(*done),
                format::count(*total)
            )),
            VolumeState::Failed(error) => parts.push(format!("{label}: {error}")),
            VolumeState::Ready => {}
        }
    }
    if parts.is_empty() {
        if status.volumes.iter().all(|v| v.live) {
            "обновляется на лету".to_string()
        } else {
            "готов".to_string()
        }
    } else {
        parts.join(", ")
    }
}

// ── Список ───────────────────────────────────────────────────────────────────────────

fn list_view(
    ui: &mut Ui,
    pane: mh_files_core::layout::PaneId,
    tab: &mut Tab,
    app: &mut FilesApp,
    focused: bool,
) {
    let area = ui.available_rect_before_wrap();
    ui.painter().rect_filled(area, CornerRadius::ZERO, theme::BACKGROUND);
    // В открытый zip можно бросать файлы: их дописывает Проводник.
    let zip_dir = match &tab.location {
        Location::Archive { archive, inner } => Some(Location::archive_path(archive, inner))
            .filter(|dir| crate::actions::writable_zip(dir).is_some()),
        _ => None,
    };
    if let Some(dir) = tab.dir().or(zip_dir) {
        app.drop_zones.push(DropZone { rect: area, dir, priority: 1, favorite_group: None });
        if app.drop_hover.as_deref() == tab.dir().as_deref() {
            ui.painter().rect_stroke(
                area.shrink(1.0),
                CornerRadius::same(4),
                Stroke::new(1.5, theme::accent()),
                egui::StrokeKind::Inside,
            );
        }
    }
    let background = ui.interact(area, Id::new(("list-bg", tab.id)), Sense::click_and_drag());
    if background.clicked() {
        tab.selection.clear();
    }
    if background.drag_started()
        && let Some(pos) = background.interact_pointer_pos()
    {
        start_band(ui, tab, pos);
    }
    if let Some(dir) = tab.dir() {
        crate::shell_menu::prefetch_on_press(app, &background, || MenuTarget::Background(dir));
    }
    background.context_menu(|ui| background_menu(ui, app, tab));
    if ui.rect_contains_pointer(area) {
        app.wheel_resize(ui.ctx(), tab.view);
    }

    if tab.view == ViewMode::Columns {
        crate::columns::show(ui, pane, tab, app, focused);
        return;
    }
    if tab.listing.is_empty() {
        let message = match &tab.listing.state {
            LoadState::Loading => None,
            LoadState::Failed(error) => Some((error.clone(), theme::CRITICAL)),
            LoadState::Done if tab.listing.has_filter() => {
                Some(("Ничего не подходит под фильтр".to_string(), theme::TEXT_SECONDARY))
            }
            LoadState::Done if matches!(tab.location, Location::Index { .. }) => {
                let empty = matches!(&tab.location, Location::Index { query } if query.is_empty());
                let searching = tab.index.as_ref().is_some_and(|v| v.searching || v.pending);
                if empty {
                    Some((
                        "Поиск по именам на всех дисках. Пробел — «и», минус — «без»: отчёт -черновик"
                            .to_string(),
                        theme::TEXT_DISABLED,
                    ))
                } else if searching {
                    None
                } else if app.indexer.status().busy() {
                    Some((
                        "Пока не найдено: индекс ещё строится, выдача обновится сама".to_string(),
                        theme::TEXT_SECONDARY,
                    ))
                } else {
                    Some(("Ничего не найдено".to_string(), theme::TEXT_SECONDARY))
                }
            }
            LoadState::Done if matches!(tab.location, Location::Search { .. }) => {
                let done = tab.search.as_ref().is_some_and(|s| s.done);
                done.then(|| ("Ничего не найдено".to_string(), theme::TEXT_SECONDARY))
            }
            LoadState::Done => Some(("Папка пуста".to_string(), theme::TEXT_DISABLED)),
        };
        let center = area.center() - vec2(0.0, area.height() * 0.15);
        match message {
            Some((text, color)) => {
                ui.painter().text(center, Align2::CENTER_CENTER, text, theme::regular(15.0), color);
            }
            None => {
                let spinner = Rect::from_center_size(center, vec2(22.0, 22.0));
                egui::Spinner::new().color(theme::accent()).paint_at(ui, spinner);
            }
        }
        return;
    }
    match tab.view {
        ViewMode::Details => details(ui, pane, tab, app, focused),
        ViewMode::Grid => grid(ui, pane, tab, app, focused),
        ViewMode::Columns => {}
    }
}

/// Контекстное меню пустого места.
fn background_menu(ui: &mut Ui, app: &mut FilesApp, tab: &mut Tab) {
    ui.set_min_width(250.0);
    for command in [CommandId::Paste, CommandId::NewFolder] {
        menu_item(ui, app, command);
    }
    ui.separator();
    ui.menu_button("Вид", |ui| {
        for (view, title) in [
            (ViewMode::Details, "Таблица"),
            (ViewMode::Grid, "Плитки"),
            (ViewMode::Columns, "Колонки"),
        ] {
            if ui.radio(tab.view == view, title).clicked() {
                tab.view = view;
                ui.close();
            }
        }
    });
    ui.menu_button("Сортировка", |ui| {
        let order = tab.options.sort;
        let index = matches!(tab.location, Location::Index { .. });
        let columns = [
            SortColumn::Relevance,
            SortColumn::Name,
            SortColumn::Modified,
            SortColumn::Type,
            SortColumn::Size,
            SortColumn::Created,
        ];
        for column in columns.into_iter().filter(|&c| index || c != SortColumn::Relevance) {
            if ui.radio(order.column == column, column.title()).clicked() {
                tab.set_sort(order.toggled(column));
                ui.close();
            }
        }
        ui.separator();
        let mut descending = order.descending;
        if ui.checkbox(&mut descending, "По убыванию").changed() {
            tab.set_sort(mh_files_core::sort::SortOrder { descending, ..order });
        }
    });
    for command in [
        CommandId::Refresh,
        CommandId::ToggleHidden,
        CommandId::SelectAll,
        CommandId::FolderSizes,
        CommandId::FindDuplicates,
        CommandId::SortFolder,
        CommandId::Undo,
    ] {
        menu_item(ui, app, command);
    }
    ui.separator();
    menu_item(ui, app, CommandId::AddFavorite);
    menu_item(ui, app, CommandId::OpenTerminal);
    match tab.dir() {
        Some(dir) if crate::shell_menu::enabled(app) => {
            crate::shell_menu::section(ui, app, MenuTarget::Background(dir));
            ui.separator();
        }
        _ => menu_item(ui, app, CommandId::WindowsMenu),
    }
    menu_item(ui, app, CommandId::Properties);
}

/// Контекстное меню объектов: свои команды, ниже — пункты Windows для `targets`.
fn item_menu(ui: &mut Ui, app: &mut FilesApp, is_dir: bool, many: bool, targets: Vec<PathBuf>) {
    ui.set_min_width(270.0);
    menu_item(ui, app, CommandId::Open);
    if is_dir {
        menu_item(ui, app, CommandId::OpenInNewTab);
        menu_item(ui, app, CommandId::OpenInOtherPane);
    } else {
        menu_item(ui, app, CommandId::OpenWith);
    }
    menu_item(ui, app, CommandId::QuickLook);
    if app.available_cached(CommandId::Extract) {
        menu_item(ui, app, CommandId::Extract);
        if app.available_cached(CommandId::ExtractHere) {
            menu_item(ui, app, CommandId::ExtractHere);
        }
    }
    ui.separator();
    for command in
        [CommandId::Cut, CommandId::Copy, CommandId::CopyToOtherPane, CommandId::MoveToOtherPane]
    {
        menu_item(ui, app, command);
    }
    ui.menu_button("Копировать как текст", |ui| {
        menu_item(ui, app, CommandId::CopyPath);
        menu_item(ui, app, CommandId::CopyName);
    });
    ui.separator();
    menu_item(ui, app, if many { CommandId::BatchRename } else { CommandId::Rename });
    menu_item(ui, app, CommandId::Delete);
    menu_item(ui, app, CommandId::DeletePermanent);
    ui.separator();
    if is_dir {
        menu_item(ui, app, CommandId::AddFavorite);
        menu_item(ui, app, CommandId::FolderSizes);
        menu_item(ui, app, CommandId::FindDuplicates);
        menu_item(ui, app, CommandId::SortFolder);
    }
    menu_item(ui, app, CommandId::RevealInExplorer);
    // В архиве объектов нет на диске — и меню Windows для них нет.
    if crate::shell_menu::enabled(app) && app.available_cached(CommandId::WindowsMenu) {
        crate::shell_menu::section(ui, app, MenuTarget::Items(targets));
        ui.separator();
    } else {
        menu_item(ui, app, CommandId::WindowsMenu);
    }
    menu_item(ui, app, CommandId::Properties);
}

pub(crate) fn row_height(app: &FilesApp) -> f32 {
    let base = if app.settings.appearance.compact { 22.0 } else { 27.0 };
    (base * app.settings.appearance.list_scale).round()
}

/// Значок в строке высотой `height`: растёт вместе с размером строк.
pub(crate) fn row_icon(app: &FilesApp, height: f32) -> f32 {
    (height - 9.0).clamp(14.0, 20.0 * app.settings.appearance.list_scale)
}

/// Шрифт строки списка с учётом размера строк.
pub(crate) fn row_font(app: &FilesApp, size: f32) -> egui::FontId {
    theme::regular(size * app.settings.appearance.list_scale)
}

/// Прокрутка, при которой строка `top..top+height` видна.
pub(crate) fn scroll_for(tab: &Tab, top: f32, height: f32) -> f32 {
    let offset = tab.scroll_offset;
    let view = tab.viewport_height.max(height);
    if top < offset {
        top
    } else if top + height > offset + view {
        top + height - view
    } else {
        offset
    }
}

fn details(
    ui: &mut Ui,
    pane: mh_files_core::layout::PaneId,
    tab: &mut Tab,
    app: &mut FilesApp,
    focused: bool,
) {
    let row_height = row_height(app);
    let search = tab.location.is_search();
    header(ui, pane, tab, app, search);
    let mut scroll = ScrollArea::vertical().id_salt(("rows", tab.id)).auto_shrink([false, false]);
    if let Some(row) = tab.scroll_to.take() {
        scroll =
            scroll.vertical_scroll_offset(scroll_for(tab, row as f32 * row_height, row_height));
    } else if let Some(offset) = tab.scroll_request.take() {
        scroll = scroll.vertical_scroll_offset(offset);
    }
    let count = tab.listing.len();
    ui.spacing_mut().item_spacing.y = 0.0;
    let output = scroll.show_rows(ui, row_height, count, |ui, rows| {
        ui.spacing_mut().item_spacing.y = 0.0;
        for row in rows {
            let (rect, _) =
                ui.allocate_exact_size(vec2(ui.available_width(), row_height), Sense::hover());
            item(ui, pane, tab, app, focused, row, rect, Look::Row { search });
        }
    });
    tab.scroll_offset = output.state.offset.y;
    tab.viewport_height = output.inner_rect.height();
    tab.list_top = output.inner_rect.top();
    tab.page_rows = ((output.inner_rect.height() / row_height) as usize).saturating_sub(1).max(1);
    update_band(ui, tab, output.inner_rect, Geometry::Rows { height: row_height });
}

/// Положение столбцов в строке шириной `width`. В узкой панели сначала прячется «Тип»,
/// потом «Изменён»: имя и размер важнее.
struct ColumnLayout {
    name: (f32, f32),
    modified: Option<(f32, f32)>,
    kind: Option<(f32, f32)>,
    size: (f32, f32),
}

const MIN_NAME: f32 = 180.0;

fn column_layout(app: &FilesApp, left: f32, width: f32, search: bool) -> ColumnLayout {
    let c = app.columns;
    let kind_width = if search { c.kind * 2.2 } else { c.kind };
    let show_kind = width - (c.modified + kind_width + c.size) >= MIN_NAME;
    let show_modified = width - (c.modified + c.size) >= MIN_NAME;
    let fixed = c.size
        + if show_modified { c.modified } else { 0.0 }
        + if show_kind { kind_width } else { 0.0 };
    let name_width = (width - fixed).max(80.0);
    let mut x = left;
    let mut take = |w: f32| {
        let span = (x, x + w);
        x += w;
        span
    };
    let name = take(name_width);
    let modified = show_modified.then(|| take(c.modified));
    let kind = show_kind.then(|| take(kind_width));
    ColumnLayout { name, modified, kind, size: take(c.size) }
}

fn header(
    ui: &mut Ui,
    pane: mh_files_core::layout::PaneId,
    tab: &Tab,
    app: &mut FilesApp,
    search: bool,
) {
    let (rect, _) =
        ui.allocate_exact_size(vec2(ui.available_width(), HEADER_HEIGHT), Sense::hover());
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, CornerRadius::ZERO, theme::WINDOW_BACKGROUND);
    painter.hline(rect.x_range(), rect.bottom() - 0.5, Stroke::new(1.0, theme::CARD_STROKE));
    let layout = column_layout(app, rect.left(), rect.width() - 12.0, search);
    let order = tab.listing.options().sort;
    let columns = [
        (SortColumn::Name, Some(layout.name), "Имя", false),
        (SortColumn::Modified, layout.modified, "Изменён", false),
        (SortColumn::Type, layout.kind, if search { "Папка" } else { "Тип" }, false),
        (SortColumn::Size, Some(layout.size), "Размер", true),
    ];
    for (index, (column, span, title, right)) in columns.into_iter().enumerate() {
        let Some((x0, x1)) = span else { continue };
        let cell = Rect::from_x_y_ranges(x0..=x1, rect.y_range());
        let response = ui.interact(cell, Id::new(("header", tab.id, index)), Sense::click());
        if response.hovered() {
            painter.rect_filled(cell, CornerRadius::ZERO, theme::CARD);
        }
        let active = order.column == column;
        let color = if active { theme::TEXT_PRIMARY } else { theme::TEXT_SECONDARY };
        let (anchor, x) = if right {
            (Align2::RIGHT_CENTER, x1 - 22.0)
        } else {
            (Align2::LEFT_CENTER, x0 + if index == 0 { 34.0 } else { 8.0 })
        };
        let text_rect =
            painter.text(pos2(x, cell.center().y), anchor, title, theme::regular(13.0), color);
        if active {
            let center = pos2(text_rect.right() + 9.0, cell.center().y);
            let (tip, base) = if order.descending { (3.0, -3.0) } else { (-3.0, 3.0) };
            let points =
                vec![center + vec2(-4.0, base), center + vec2(4.0, base), center + vec2(0.0, tip)];
            painter.add(egui::Shape::convex_polygon(points, theme::accent(), Stroke::NONE));
        }
        if response.clicked() && !(search && column == SortColumn::Type) {
            app.actions.push(Action::SetSort { pane, column });
        }
        // Граница столбца тянется за левый край: ширина фиксированного столбца меняется.
        if index > 0 {
            let handle = Rect::from_x_y_ranges((x0 - 3.0)..=(x0 + 3.0), rect.y_range());
            let drag =
                ui.interact(handle, Id::new(("header-resize", tab.id, index)), Sense::drag());
            if drag.hovered() || drag.dragged() {
                ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeColumn);
            }
            painter.vline(
                x0,
                (rect.top() + 5.0)..=(rect.bottom() - 5.0),
                Stroke::new(1.0, theme::CARD_STROKE),
            );
            if drag.dragged() {
                let delta = -drag.drag_delta().x;
                let width = match index {
                    1 => &mut app.columns.modified,
                    2 => &mut app.columns.kind,
                    _ => &mut app.columns.size,
                };
                *width = (*width + delta).clamp(50.0, 400.0);
            }
        }
    }
}

#[derive(Clone, Copy)]
pub(crate) enum Look {
    Row {
        search: bool,
    },
    Tile,
    /// Строка колонки Миллера: значок, имя, у папок — стрелка вглубь.
    Column,
}

/// Строка таблицы или плитка: рисование и все взаимодействия с объектом.
#[allow(clippy::too_many_arguments)]
pub(crate) fn item(
    ui: &mut Ui,
    pane: mh_files_core::layout::PaneId,
    tab: &mut Tab,
    app: &mut FilesApp,
    focused: bool,
    row: usize,
    rect: Rect,
    look: Look,
) {
    let Some(entry) = tab.listing.get(row).cloned() else { return };
    let path = entry.path();
    let selected = tab.selection.is_selected(&path);
    let cursor = tab.selection.cursor() == Some(&path);
    let id = Id::new(("item", tab.id, &path));
    let response = ui.interact(rect, id, Sense::click_and_drag());
    let drop_target = entry.is_dir() && app.drop_hover.as_deref() == Some(path.as_path());

    let painter = ui.painter_at(rect);
    let radius = CornerRadius::same(4);
    // Дубликаты: полоса группы слева и черта между группами.
    if let Some(view) = &tab.duplicates
        && let Some(&group) = view.group_of.get(&path)
    {
        let color =
            if group % 2 == 0 { theme::accent() } else { theme::accent().gamma_multiply(0.4) };
        painter.rect_filled(
            Rect::from_min_size(rect.min + vec2(0.0, 2.0), vec2(3.0, rect.height() - 4.0)),
            CornerRadius::same(1),
            color,
        );
        let previous = row.checked_sub(1).and_then(|r| tab.listing.get(r)).map(|e| e.path());
        if previous.is_some_and(|p| view.group_of.get(&p) != Some(&group)) {
            painter.hline(rect.x_range(), rect.top(), Stroke::new(1.0, theme::CARD_STROKE));
        }
    }
    let fill = if drop_target {
        theme::accent().gamma_multiply(0.35)
    } else if selected {
        theme::selection_fill()
    } else if response.hovered() {
        theme::CARD.gamma_multiply(0.8)
    } else {
        Color32::TRANSPARENT
    };
    let body = if matches!(look, Look::Row { .. } | Look::Column) {
        rect.shrink2(vec2(2.0, 1.0))
    } else {
        rect.shrink(2.0)
    };
    painter.rect_filled(body, radius, fill);
    if cursor && focused {
        painter.rect_stroke(
            body,
            radius,
            Stroke::new(1.0, theme::accent().gamma_multiply(0.7)),
            egui::StrokeKind::Inside,
        );
    }

    let dim = app.cut.contains(&path) || entry.hidden();
    // Где кончается столбец имени: потянуть строку правее — рамка, а не перенос файлов.
    let mut name_end = f32::INFINITY;
    let text_color = if dim { theme::TEXT_DISABLED } else { theme::TEXT_PRIMARY };
    let shown_name = display_name(&entry, app.settings.files.show_extensions);
    let renaming = tab.rename.as_ref().is_some_and(|r| r.path == path);

    match look {
        Look::Row { search } => {
            let layout = column_layout(app, rect.left(), rect.width() - 12.0, search);
            let icon_size = row_icon(app, rect.height());
            let icon_rect = Rect::from_center_size(
                pos2(layout.name.0 + 8.0 + icon_size / 2.0, rect.center().y),
                vec2(icon_size, icon_size),
            );
            paint_icon(ui, app, &entry, icon_rect, dim);
            let name_rect = Rect::from_x_y_ranges(
                (layout.name.0 + 14.0 + icon_size)..=(layout.name.1 - 6.0),
                rect.y_range(),
            );
            if renaming {
                rename_editor(ui, tab, app, name_rect.shrink2(vec2(0.0, 2.0)));
            } else {
                let galley =
                    elided(ui, &shown_name, row_font(app, 14.0), text_color, name_rect.width());
                painter.galley(
                    pos2(name_rect.left(), rect.center().y - galley.size().y / 2.0),
                    galley,
                    text_color,
                );
            }
            let secondary = theme::TEXT_SECONDARY;
            let font = row_font(app, 13.0);
            if let (Some(modified), Some((x0, x1))) = (entry.modified, layout.modified) {
                let text = if app.settings.files.relative_dates {
                    format::date(modified)
                } else {
                    format::date_full(modified)
                };
                let galley = elided(ui, &text, font.clone(), secondary, x1 - x0 - 12.0);
                painter.galley(
                    pos2(x0 + 8.0, rect.center().y - galley.size().y / 2.0),
                    galley,
                    secondary,
                );
            }
            if let Some((x0, x1)) = layout.kind {
                let kind = if search {
                    entry.parent.display().to_string()
                } else {
                    format::kind(&entry.extension(), entry.is_dir())
                };
                let width = x1 - x0 - 12.0;
                // У пути важен конец — папка, в которой лежит файл.
                let galley = if search {
                    elided_start(ui, &kind, font.clone(), secondary, width)
                } else {
                    elided(ui, &kind, font.clone(), secondary, width)
                };
                painter.galley(
                    pos2(x0 + 8.0, rect.center().y - galley.size().y / 2.0),
                    galley,
                    secondary,
                );
            }
            let size = if !entry.is_dir() {
                Some(format::size_column(entry.size))
            } else if let Some(size) = app.folder_sizes.get(&path) {
                Some(format::size(size.bytes))
            } else if app.sizes_pending.contains(&path) {
                Some("…".to_string())
            } else {
                None
            };
            if let Some(size) = size {
                let at = pos2(layout.size.1 - 10.0, rect.center().y);
                painter.text(at, Align2::RIGHT_CENTER, size, font, secondary);
            }
            name_end = layout.name.1;
        }
        Look::Column => {
            let icon_size = row_icon(app, rect.height());
            let icon_rect = Rect::from_center_size(
                pos2(rect.left() + 6.0 + icon_size / 2.0, rect.center().y),
                vec2(icon_size, icon_size),
            );
            paint_icon(ui, app, &entry, icon_rect, dim);
            let right = if entry.is_dir() { rect.right() - 22.0 } else { rect.right() - 8.0 };
            let name_rect =
                Rect::from_x_y_ranges((rect.left() + 10.0 + icon_size)..=right, rect.y_range());
            if renaming {
                rename_editor(ui, tab, app, name_rect.shrink2(vec2(0.0, 2.0)));
            } else {
                let galley =
                    elided(ui, &shown_name, row_font(app, 14.0), text_color, name_rect.width());
                painter.galley(
                    pos2(name_rect.left(), rect.center().y - galley.size().y / 2.0),
                    galley,
                    text_color,
                );
            }
            if entry.is_dir() {
                let chevron = Rect::from_center_size(
                    pos2(rect.right() - 12.0, rect.center().y),
                    vec2(10.0, 10.0),
                );
                icons::chevron_right(&painter, chevron, theme::TEXT_DISABLED);
            }
            // Тянуть за имя — перенос файлов; рамки в колонке нет.
            name_end = f32::INFINITY;
        }
        Look::Tile => {
            let label_height = 34.0;
            let image_rect = Rect::from_min_max(
                rect.min + vec2(8.0, 6.0),
                pos2(rect.right() - 8.0, rect.bottom() - label_height),
            );
            paint_preview(ui, app, &entry, image_rect, dim);
            let label_rect = Rect::from_min_max(
                pos2(rect.left() + 4.0, rect.bottom() - label_height),
                rect.max - vec2(4.0, 2.0),
            );
            if renaming {
                rename_editor(
                    ui,
                    tab,
                    app,
                    Rect::from_min_size(label_rect.min, vec2(label_rect.width(), 22.0)),
                );
            } else {
                // Выравнивание по центру: позиция галереи — середина строки.
                let galley = wrapped(
                    ui,
                    &shown_name,
                    theme::regular(13.0),
                    text_color,
                    label_rect.width(),
                    2,
                );
                painter.galley(
                    pos2(label_rect.center().x, label_rect.top() + 1.0),
                    galley,
                    text_color,
                );
            }
        }
    }

    // Взаимодействия.
    let modifiers = ui.input(|i| i.modifiers);
    let mods = Modifiers { ctrl: modifiers.ctrl || modifiers.command, shift: modifiers.shift };
    if response.clicked() && !renaming {
        if tab.rename.is_some() {
            commit_inline_rename(tab, app);
        }
        tab.selection.click(&tab.listing, row, mods);
    }
    if response.double_clicked() && !renaming {
        tab.selection.select_only(path.clone());
        app.actions.push(Action::FocusPane(pane));
        app.actions.push(Action::Run(CommandId::Open));
    }
    if response.middle_clicked() && entry.is_dir() {
        app.actions.push(Action::FocusPane(pane));
        let location = tab.location.enter(&path);
        app.actions.push(Action::Open { location, target: Target::NewTab });
    }
    if response.secondary_clicked() && !selected {
        tab.selection.select_only(path.clone());
    }
    let press = ui.input(|i| i.pointer.press_origin());
    if response.drag_started() && !renaming && press.is_some_and(|p| p.x > name_end) {
        if let Some(pos) = press {
            start_band(ui, tab, pos);
        }
    } else if response.drag_started() && !renaming {
        if !selected {
            tab.selection.select_only(path.clone());
        }
        let archive = match &tab.location {
            Location::Archive { archive, .. } => Some(archive.clone()),
            _ => None,
        };
        let right = response.drag_started_by(egui::PointerButton::Secondary);
        egui::DragAndDrop::set_payload(
            ui.ctx(),
            DragFiles { paths: tab.targets(), archive, right },
        );
    }
    // Папка, а ещё zip — бросок в него дописывает архив.
    if entry.is_dir() || crate::actions::writable_zip(&path).is_some() {
        app.drop_zones.push(DropZone {
            rect,
            dir: path.clone(),
            priority: 2,
            favorite_group: None,
        });
    }
    // В архиве объектов нет на диске — и меню Windows для них нет.
    if !matches!(tab.location, Location::Archive { .. }) {
        crate::shell_menu::prefetch_on_press(app, &response, || {
            // Невыделенный объект при щелчке станет единственным выделенным.
            MenuTarget::Items(if selected { tab.targets() } else { vec![path.clone()] })
        });
    }
    let many = tab.selection.len() > 1;
    let is_dir = entry.is_dir();
    response.context_menu(|ui| item_menu(ui, app, is_dir, many, tab.targets()));
}

/// Имя для показа: без расширения, если так настроено.
fn display_name(entry: &Entry, show_extensions: bool) -> String {
    if show_extensions || entry.is_dir() {
        entry.name.clone()
    } else {
        stem_of(&entry.name, false).to_string()
    }
}

fn paint_texture(ui: &Ui, texture: &TextureHandle, rect: Rect, dim: bool) {
    let size = texture.size_vec2();
    let scale = (rect.width() / size.x).min(rect.height() / size.y).min(if size.x <= 64.0 {
        1.0
    } else {
        f32::MAX
    });
    let scale =
        if size.x <= 64.0 { (rect.width() / size.x).min(rect.height() / size.y) } else { scale };
    let fitted = Rect::from_center_size(rect.center(), size * scale);
    let tint = if dim { Color32::from_white_alpha(110) } else { Color32::WHITE };
    ui.painter().image(
        texture.id(),
        fitted,
        Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)),
        tint,
    );
}

/// Значок объекта: системный, если есть, иначе свой.
pub(crate) fn paint_icon(ui: &Ui, app: &mut FilesApp, entry: &Entry, rect: Rect, dim: bool) {
    let pixels = rect.width() * ui.ctx().pixels_per_point();
    if let Some(texture) = app.images.icon(&app.workers, entry, pixels) {
        paint_texture(ui, &texture, rect, dim);
        return;
    }
    let painter = ui.painter();
    if entry.is_dir() {
        let color = if dim {
            theme::accent().gamma_multiply(0.4)
        } else {
            theme::accent().gamma_multiply(0.85)
        };
        icons::folder(painter, rect, color);
    } else {
        icons::file(painter, rect, &entry.extension());
    }
}

/// Плитка: эскиз содержимого, а без него — крупный значок.
fn paint_preview(ui: &Ui, app: &mut FilesApp, entry: &Entry, rect: Rect, dim: bool) {
    let pixels = rect.width().max(rect.height()) * ui.ctx().pixels_per_point();
    if let Some(texture) = app.images.thumbnail(&app.workers, entry, pixels) {
        paint_texture(ui, &texture, rect, dim);
        return;
    }
    let side = rect.width().min(rect.height()) * 0.62;
    let icon_rect = Rect::from_center_size(rect.center(), vec2(side, side));
    paint_icon(ui, app, entry, icon_rect, dim);
}

fn grid(
    ui: &mut Ui,
    pane: mh_files_core::layout::PaneId,
    tab: &mut Tab,
    app: &mut FilesApp,
    focused: bool,
) {
    let tile = app.settings.appearance.grid_size;
    let tile_size = vec2(tile + 16.0, tile + 40.0);
    let width = (ui.available_width() - 14.0).max(tile_size.x);
    let columns = ((width / tile_size.x).floor() as usize).max(1);
    let count = tab.listing.len();
    let rows = count.div_ceil(columns);
    tab.grid_columns = columns;
    let mut scroll = ScrollArea::vertical().id_salt(("tiles", tab.id)).auto_shrink([false, false]);
    if let Some(index) = tab.scroll_to.take() {
        let top = (index / columns) as f32 * tile_size.y;
        scroll = scroll.vertical_scroll_offset(scroll_for(tab, top, tile_size.y));
    } else if let Some(offset) = tab.scroll_request.take() {
        scroll = scroll.vertical_scroll_offset(offset);
    }
    ui.spacing_mut().item_spacing = vec2(0.0, 0.0);
    let output = scroll.show_rows(ui, tile_size.y, rows, |ui, range| {
        ui.spacing_mut().item_spacing = vec2(0.0, 0.0);
        for row in range {
            let (line, _) = ui.allocate_exact_size(vec2(width, tile_size.y), Sense::hover());
            for column in 0..columns {
                let index = row * columns + column;
                if index >= count {
                    break;
                }
                let rect = Rect::from_min_size(
                    line.min + vec2(column as f32 * tile_size.x + 4.0, 0.0),
                    tile_size,
                );
                item(ui, pane, tab, app, focused, index, rect, Look::Tile);
            }
        }
    });
    tab.scroll_offset = output.state.offset.y;
    tab.viewport_height = output.inner_rect.height();
    tab.list_top = output.inner_rect.top();
    tab.page_rows = ((output.inner_rect.height() / tile_size.y) as usize).max(1);
    let geometry =
        Geometry::Tiles { size: tile_size, columns, left: output.inner_rect.left() + 4.0 };
    update_band(ui, tab, output.inner_rect, geometry);
}

// ── Рамка выделения ──────────────────────────────────────────────────────────────────

/// Раскладка списка, по которой рамка находит строки или плитки.
#[derive(Clone, Copy)]
enum Geometry {
    Rows { height: f32 },
    Tiles { size: egui::Vec2, columns: usize, left: f32 },
}

/// Экран → координаты содержимого (прокрутка учтена).
fn to_content(tab: &Tab, pos: egui::Pos2) -> egui::Pos2 {
    pos2(pos.x, pos.y - tab.list_top + tab.scroll_offset)
}

fn start_band(ui: &Ui, tab: &mut Tab, pos: egui::Pos2) {
    let ctrl = ui.input(|i| i.modifiers.ctrl || i.modifiers.command);
    let base = if ctrl { tab.selection.snapshot() } else { Default::default() };
    tab.rename = None;
    tab.band = Some(Band { origin: to_content(tab, pos), base });
}

/// Строки (или плитки), которые задевает прямоугольник в координатах содержимого.
fn band_rows(area: Rect, geometry: Geometry, count: usize) -> Vec<usize> {
    if count == 0 {
        return Vec::new();
    }
    match geometry {
        Geometry::Rows { height } => {
            let first = (area.top() / height).floor().max(0.0) as usize;
            let last = ((area.bottom() / height).floor().max(0.0) as usize).min(count - 1);
            (first..=last).collect()
        }
        Geometry::Tiles { size, columns, left } => {
            let first_row = (area.top() / size.y).floor().max(0.0) as usize;
            let last_row = (area.bottom() / size.y).floor().max(0.0) as usize;
            let mut hit = Vec::new();
            for row in first_row..=last_row {
                for column in 0..columns {
                    let x0 = left + column as f32 * size.x;
                    let index = row * columns + column;
                    if index < count && x0 + size.x > area.left() && x0 < area.right() {
                        hit.push(index);
                    }
                }
            }
            hit
        }
    }
}

/// Тянется рамка: выделить задетое, прокрутить у края, нарисовать.
fn update_band(ui: &mut Ui, tab: &mut Tab, view: Rect, geometry: Geometry) {
    let Some(band) = &tab.band else { return };
    let (down, pointer) = ui.input(|i| (i.pointer.primary_down(), i.pointer.latest_pos()));
    let Some(pointer) = pointer.filter(|_| down) else {
        tab.band = None;
        return;
    };
    let origin = band.origin;
    let base = band.base.clone();
    let area = Rect::from_two_pos(origin, to_content(tab, pointer));
    let rows = band_rows(area, geometry, tab.listing.len());
    tab.selection.set_rows(&tab.listing, rows, &base);
    // У края списка рамка сама прокручивает.
    let edge = 24.0;
    if pointer.y < view.top() + edge {
        tab.scroll_request = Some((tab.scroll_offset - 14.0).max(0.0));
    } else if pointer.y > view.bottom() - edge {
        tab.scroll_request = Some(tab.scroll_offset + 14.0);
    }
    let start = pos2(origin.x, origin.y + tab.list_top - tab.scroll_offset);
    let screen = Rect::from_two_pos(start, pointer).intersect(view);
    let painter = ui.painter_at(view);
    painter.rect_filled(screen, CornerRadius::same(2), theme::accent().gamma_multiply(0.12));
    let stroke = Stroke::new(1.0, theme::accent());
    painter.rect_stroke(screen, CornerRadius::same(2), stroke, egui::StrokeKind::Inside);
    ui.ctx().request_repaint();
}

// ── Переименование в строке ──────────────────────────────────────────────────────────

fn rename_editor(ui: &mut Ui, tab: &mut Tab, app: &mut FilesApp, rect: Rect) {
    let is_dir = tab.rename.as_ref().and_then(|r| tab.entry(&r.path)).is_some_and(Entry::is_dir);
    let Some(rename) = &mut tab.rename else { return };
    let id = Id::new(("rename", tab.id));
    let response = ui.put(
        rect,
        egui::TextEdit::singleline(&mut rename.text)
            .id(id)
            .font(theme::regular(14.0))
            .margin(egui::Margin::symmetric(4, 1)),
    );
    if std::mem::take(&mut rename.fresh) {
        response.request_focus();
        let stem = stem_of(&rename.text, is_dir).chars().count();
        select_range(ui.ctx(), id, 0, stem);
        return;
    }
    if response.lost_focus() {
        if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
            tab.rename = None;
        } else {
            commit_inline_rename(tab, app);
        }
    }
}

fn commit_inline_rename(tab: &mut Tab, app: &mut FilesApp) {
    if let Some(InlineRename { path, text, .. }) = tab.rename.take() {
        app.actions.push(Action::CommitRename { tab: tab.id, path, new_name: text });
    }
}

fn select_range(ctx: &egui::Context, id: Id, from: usize, to: usize) {
    if let Some(mut state) = egui::TextEdit::load_state(ctx, id) {
        let range = egui::text::CCursorRange::two(
            egui::text::CCursor::new(from),
            egui::text::CCursor::new(to),
        );
        state.cursor.set_char_range(Some(range));
        state.store(ctx, id);
    } else {
        let mut state = egui::text_edit::TextEditState::default();
        let range = egui::text::CCursorRange::two(
            egui::text::CCursor::new(from),
            egui::text::CCursor::new(to),
        );
        state.cursor.set_char_range(Some(range));
        state.store(ctx, id);
    }
}

pub fn select_all(ctx: &egui::Context, id: Id, chars: usize) {
    select_range(ctx, id, 0, chars);
}

// ── Этот компьютер ───────────────────────────────────────────────────────────────────

fn computer_view(ui: &mut Ui, app: &mut FilesApp) {
    let area = ui.available_rect_before_wrap();
    ui.painter().rect_filled(area, CornerRadius::ZERO, theme::BACKGROUND);
    ScrollArea::vertical().id_salt("computer").auto_shrink([false, false]).show(ui, |ui| {
        ui.add_space(14.0);
        ui.horizontal(|ui| {
            ui.add_space(16.0);
            widgets::section_label(ui, "Диски");
        });
        ui.add_space(6.0);
        let card = vec2(270.0, 70.0);
        let width = ui.available_width() - 24.0;
        let columns = ((width / (card.x + 10.0)).floor() as usize).max(1);
        let drives = app.drives.clone();
        for chunk in drives.chunks(columns) {
            ui.horizontal(|ui| {
                ui.add_space(16.0);
                ui.spacing_mut().item_spacing.x = 10.0;
                for drive in chunk {
                    drive_card(ui, app, drive, card);
                }
            });
            ui.add_space(10.0);
        }
        if !app.places.is_empty() {
            ui.add_space(10.0);
            ui.horizontal(|ui| {
                ui.add_space(16.0);
                widgets::section_label(ui, "Места");
            });
            ui.add_space(6.0);
            let places = app.places.clone();
            ui.horizontal_wrapped(|ui| {
                ui.add_space(16.0);
                ui.spacing_mut().item_spacing = vec2(8.0, 8.0);
                for (folder, path) in places {
                    let response = ui.add(
                        egui::Button::new(RichText::new(folder.title()).font(theme::regular(14.0)))
                            .fill(theme::CARD)
                            .stroke(Stroke::new(1.0, theme::CARD_STROKE))
                            .min_size(vec2(130.0, 34.0)),
                    );
                    app.drop_zones.push(DropZone {
                        rect: response.rect,
                        dir: path.clone(),
                        priority: 2,
                        favorite_group: None,
                    });
                    if response.clicked() {
                        app.actions.push(Action::Open {
                            location: Location::Dir(path),
                            target: Target::Current,
                        });
                    }
                }
            });
        }
    });
}

fn drive_card(
    ui: &mut Ui,
    app: &mut FilesApp,
    drive: &mh_files_platform::drives::DriveInfo,
    size: egui::Vec2,
) {
    let (rect, response) = ui.allocate_exact_size(size, Sense::click());
    let painter = ui.painter();
    let fill = if response.hovered() { theme::CARD } else { theme::PANEL };
    painter.rect_filled(rect, CornerRadius::same(6), fill);
    painter.rect_stroke(
        rect,
        CornerRadius::same(6),
        Stroke::new(1.0, theme::CARD_STROKE),
        egui::StrokeKind::Inside,
    );
    let icon = Rect::from_min_size(rect.min + vec2(12.0, 14.0), vec2(26.0, 26.0));
    icons::drive(painter, icon, theme::accent());
    let left = rect.left() + 50.0;
    painter.text(
        pos2(left, rect.top() + 16.0),
        Align2::LEFT_CENTER,
        crate::app::drive_title(drive),
        theme::regular(14.5),
        theme::TEXT_PRIMARY,
    );
    let bar =
        Rect::from_min_size(pos2(left, rect.top() + 32.0), vec2(rect.right() - left - 14.0, 7.0));
    widgets::paint_usage_bar(painter, bar, drive.used_fraction());
    let detail = if drive.total > 0 {
        format!("свободно {} из {}", format::size(drive.free), format::size(drive.total))
    } else if drive.ready || !drive.file_system.is_empty() {
        String::new()
    } else {
        "опрос…".to_string()
    };
    painter.text(
        pos2(left, rect.top() + 54.0),
        Align2::LEFT_CENTER,
        detail,
        theme::regular(12.5),
        theme::TEXT_SECONDARY,
    );
    app.drop_zones.push(DropZone {
        rect,
        dir: drive.root.clone(),
        priority: 2,
        favorite_group: None,
    });
    if response.clicked() {
        app.actions.push(Action::Open {
            location: Location::Dir(drive.root.clone()),
            target: Target::Current,
        });
    }
    let root = drive.root.clone();
    response.context_menu(|ui| {
        if ui.button("Открыть в новой вкладке").clicked() {
            app.actions.push(Action::Open {
                location: Location::Dir(root.clone()),
                target: Target::NewTab,
            });
            ui.close();
        }
        if ui.button("Свойства").clicked() {
            app.actions.push(Action::Shell(mh_files_fs::ShellJob::Properties(vec![root.clone()])));
            ui.close();
        }
    });
}

// ── Текст ────────────────────────────────────────────────────────────────────────────

/// Однострочный текст с многоточием, если не помещается.
pub fn elided(ui: &Ui, text: &str, font: FontId, color: Color32, width: f32) -> Arc<Galley> {
    wrapped(ui, text, font, color, width, 1)
}

/// Одна строка, не влезло — обрезается начало: `…\Проекты\Отчёты`.
pub fn elided_start(ui: &Ui, text: &str, font: FontId, color: Color32, width: f32) -> Arc<Galley> {
    let layout = |text: String| ui.painter().layout_no_wrap(text, font.clone(), color);
    let full = layout(text.to_string());
    if full.size().x <= width {
        return full;
    }
    let chars: Vec<char> = text.chars().collect();
    // Сколько последних символов влезает вместе с многоточием: двоичный поиск.
    let (mut low, mut high) = (0, chars.len());
    while low < high {
        let mid = (low + high).div_ceil(2);
        let tail: String = chars[chars.len() - mid..].iter().collect();
        if layout(format!("…{tail}")).size().x <= width {
            low = mid;
        } else {
            high = mid - 1;
        }
    }
    let tail: String = chars[chars.len() - low..].iter().collect();
    layout(format!("…{tail}"))
}

pub fn wrapped(
    ui: &Ui,
    text: &str,
    font: FontId,
    color: Color32,
    width: f32,
    rows: usize,
) -> Arc<Galley> {
    let mut job = LayoutJob::simple(text.to_string(), font, color, width.max(10.0));
    job.wrap.max_rows = rows;
    job.wrap.break_anywhere = rows == 1;
    job.wrap.overflow_character = Some('…');
    if rows > 1 {
        job.halign = Align::Center;
    }
    ui.painter().layout_job(job)
}
