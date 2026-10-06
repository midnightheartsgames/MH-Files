//! Подтверждения. Удаление показывает точный список того, что будет удалено (PLAN.md §4/10).

use std::path::PathBuf;

use eframe::egui::{self, Id, Key, RichText};
use mh_files_platform::ops::FileOp;

use crate::app::FilesApp;
use crate::{theme, widgets};

pub enum Dialog {
    Delete { paths: Vec<PathBuf>, permanent: bool },
}

impl Dialog {
    pub fn delete(paths: Vec<PathBuf>, permanent: bool) -> Dialog {
        Dialog::Delete { paths, permanent }
    }
}

/// Сколько путей перечислить; остальные — числом.
const LISTED: usize = 12;

pub fn show(ctx: &egui::Context, app: &mut FilesApp) {
    let Some(Dialog::Delete { paths, permanent }) = &app.dialog else { return };
    let (paths, permanent) = (paths.clone(), *permanent);
    let mut result: Option<bool> = None;
    let modal = egui::Modal::new(Id::new("confirm-delete")).show(ctx, |ui| {
        ui.set_width(520.0);
        let title = if permanent {
            "Удалить насовсем?"
        } else {
            "Удалить в корзину?"
        };
        ui.label(RichText::new(title).font(theme::bold(20.0)));
        ui.add_space(6.0);
        if permanent {
            ui.label(RichText::new("Восстановить из корзины будет нельзя.").color(theme::CRITICAL));
        }
        ui.add_space(6.0);
        egui::Frame::new().fill(theme::FIELD).corner_radius(4).inner_margin(8).show(ui, |ui| {
            ui.set_width(ui.available_width());
            for path in paths.iter().take(LISTED) {
                ui.label(
                    RichText::new(path.display().to_string())
                        .font(theme::regular(13.0))
                        .color(theme::TEXT_SECONDARY),
                );
            }
            if paths.len() > LISTED {
                ui.label(
                    RichText::new(format!("и ещё {}", paths.len() - LISTED))
                        .color(theme::TEXT_DISABLED),
                );
            }
        });
        ui.add_space(12.0);
        ui.horizontal(|ui| {
            if widgets::button(ui, if permanent { "Удалить насовсем" } else { "В корзину" }, true)
                .clicked()
            {
                result = Some(true);
            }
            if widgets::button(ui, "Отмена", false).clicked() {
                result = Some(false);
            }
        });
        if ui.input(|i| i.key_pressed(Key::Enter)) {
            result = Some(true);
        }
    });
    if modal.should_close() || ctx.input(|i| i.key_pressed(Key::Escape)) {
        result.get_or_insert(false);
    }
    if let Some(confirmed) = result {
        app.dialog = None;
        if confirmed {
            app.ops.submit(FileOp::Delete { paths, permanent });
        }
    }
}
