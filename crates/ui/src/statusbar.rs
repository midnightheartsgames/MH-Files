//! Строка состояния: сколько объектов, что выделено, ход операций, сообщения, вид.

use eframe::egui::{Align, Layout, Popup, Rect, RichText, Sense, Spinner, Ui, pos2, vec2};
use mh_files_core::format;
use mh_files_core::location::Location;
use mh_files_core::session::ViewMode;

use crate::app::{Action, FilesApp, Level};
use crate::{icons, theme, widgets};

pub fn show(ui: &mut Ui, app: &mut FilesApp) {
    ui.horizontal_centered(|ui| {
        ui.spacing_mut().item_spacing.x = 14.0;
        let tab = app.tab();
        let secondary = |text: String| {
            RichText::new(text).font(theme::regular(13.0)).color(theme::TEXT_SECONDARY)
        };
        match &tab.location {
            Location::Computer => {
                ui.label(secondary(format!("Дисков: {}", app.drives.len())));
            }
            _ => {
                let totals = tab.listing.totals();
                ui.label(secondary(format::items(totals.dirs + totals.files)));
                let selected = tab.selection.selected_paths(&tab.listing);
                if !selected.is_empty() {
                    let bytes: u64 = selected
                        .iter()
                        .filter_map(|path| tab.entry(path))
                        .filter(|entry| !entry.is_dir())
                        .map(|entry| entry.size)
                        .sum();
                    let text = format!("выделено {} · {}", selected.len(), format::size(bytes));
                    ui.label(RichText::new(text).font(theme::regular(13.0)).color(theme::accent()));
                }
                if tab.listing.is_loading() {
                    ui.add(eframe::egui::Spinner::new().size(12.0).color(theme::accent()));
                }
            }
        }
        if !app.operations.list.is_empty() {
            operations(ui, app);
        }
        if let Some((first, _)) = app.pending_chord {
            let text = format!("{} …", crate::commands::format_shortcut(&first));
            ui.label(RichText::new(text).font(theme::regular(13.0)).color(theme::accent()));
        }
        let index = app.indexer.status();
        if index.busy() {
            ui.add(Spinner::new().size(12.0).color(theme::accent()));
            let text = format!("Индекс: {}", crate::pane_view::index_state(&index));
            ui.label(RichText::new(text).font(theme::regular(13.0)).color(theme::TEXT_SECONDARY))
                .on_hover_text("Поиск по дискам уже работает, но пока находит не всё");
            ui.ctx().request_repaint_after(std::time::Duration::from_millis(500));
        }
        if let Some(status) = &app.status {
            let color = match status.level {
                Level::Info => theme::TEXT_SECONDARY,
                Level::Error => theme::CRITICAL,
            };
            ui.label(RichText::new(&status.text).font(theme::regular(13.0)).color(color));
        }
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            ui.spacing_mut().item_spacing.x = 2.0;
            let zoom = (app.settings.appearance.font_scale * 100.0).round();
            ui.label(
                RichText::new(format!("{zoom}%"))
                    .font(theme::regular(12.5))
                    .color(theme::TEXT_DISABLED),
            );
            ui.add_space(8.0);
            let view = app.tab().view;
            let grid =
                widgets::icon_button(ui, view != ViewMode::Grid, "Плитки (Ctrl+2)", icons::grid);
            if grid.clicked() {
                app.actions.push(Action::SetView(ViewMode::Grid));
            }
            let list = widgets::icon_button(
                ui,
                view != ViewMode::Details,
                "Таблица (Ctrl+1)",
                icons::list,
            );
            if list.clicked() {
                app.actions.push(Action::SetView(ViewMode::Details));
            }
            ui.add_space(8.0);
            if let Some(dir) = app.tab().dir() {
                let root = crate::app::root_of(&dir);
                if let Some(drive) = app.drives.iter().find(|d| d.root == root && d.total > 0) {
                    let text = format!("свободно {}", format::size(drive.free));
                    ui.label(
                        RichText::new(text).font(theme::regular(12.5)).color(theme::TEXT_DISABLED),
                    );
                }
            }
        });
    });
}

/// Текущая операция с полосой хода; по щелчку — список всех с паузой и отменой.
fn operations(ui: &mut Ui, app: &mut FilesApp) {
    let list = app.operations.list.clone();
    let current = list.iter().find(|op| op.running).unwrap_or(&list[0]);
    let paused = app.ops.is_paused(current.id);
    let response = ui
        .horizontal(|ui| {
            if paused {
                ui.label(RichText::new("пауза").font(theme::regular(13.0)).color(theme::WARN));
            } else {
                ui.add(Spinner::new().size(12.0).color(theme::accent()));
            }
            ui.label(
                RichText::new(&current.label).font(theme::regular(13.0)).color(theme::accent()),
            );
            progress_bar(ui, current.progress.fraction(), 110.0);
            if list.len() > 1 {
                let more = format!("+{} в очереди", list.len() - 1);
                ui.label(
                    RichText::new(more).font(theme::regular(12.5)).color(theme::TEXT_SECONDARY),
                );
            }
        })
        .response
        .interact(Sense::click())
        .on_hover_text("Операции: пауза и отмена");
    Popup::from_toggle_button_response(&response).show(|ui| {
        ui.set_min_width(380.0);
        for op in &list {
            ui.horizontal(|ui| {
                let paused = app.ops.is_paused(op.id);
                let state = if !op.running {
                    "в очереди"
                } else if paused {
                    "пауза"
                } else {
                    ""
                };
                ui.label(RichText::new(&op.label).font(theme::regular(14.0)));
                if !state.is_empty() {
                    ui.label(RichText::new(state).color(theme::TEXT_SECONDARY));
                }
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if ui.button("Отменить").clicked() {
                        app.ops.cancel(op.id);
                    }
                    let toggle = if paused { "Продолжить" } else { "Пауза" };
                    if op.running && ui.button(toggle).clicked() {
                        app.ops.pause(op.id, !paused);
                    }
                });
            });
            progress_bar(ui, op.progress.fraction(), ui.available_width());
            if let Some(item) = &op.progress.item {
                ui.label(
                    RichText::new(item.display().to_string())
                        .font(theme::regular(12.0))
                        .color(theme::TEXT_DISABLED),
                );
            }
            ui.add_space(6.0);
        }
    });
}

/// Полоса хода; без известного объёма — бегущая.
fn progress_bar(ui: &mut Ui, fraction: Option<f32>, width: f32) {
    let (rect, _) = ui.allocate_exact_size(vec2(width, 6.0), Sense::hover());
    let painter = ui.painter();
    painter.rect_filled(rect, 3, theme::FIELD);
    match fraction {
        Some(fraction) => {
            let mut filled = rect;
            filled.set_width((rect.width() * fraction).max(rect.height()));
            painter.rect_filled(filled, 3, theme::accent());
        }
        None => {
            let t = ui.input(|i| i.time) as f32;
            let span = rect.width() * 0.3;
            let x = rect.left() + (t * 0.8).fract() * (rect.width() - span);
            let part = Rect::from_min_size(pos2(x, rect.top()), vec2(span, rect.height()));
            painter.rect_filled(part, 3, theme::accent());
            ui.ctx().request_repaint();
        }
    }
}
