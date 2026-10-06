//! Строка состояния: сколько объектов, что выделено, ход операций, сообщения, вид.

use eframe::egui::{Align, Layout, RichText, Ui};
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
        for (_, text) in &app.ops_running {
            ui.add(eframe::egui::Spinner::new().size(12.0).color(theme::accent()));
            ui.label(
                RichText::new(format!("{text}…")).font(theme::regular(13.0)).color(theme::accent()),
            );
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
