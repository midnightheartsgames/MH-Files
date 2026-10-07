//! Подтверждения и конфликты. Удаление показывает точный список того, что будет удалено
//! (PLAN.md §4/10); конфликт имён решается для каждого объекта или для всех сразу.

use std::path::PathBuf;

use eframe::egui::{self, Align, Color32, Id, Key, Layout, RichText, ScrollArea};
use mh_files_core::format;
use mh_files_fs::{Conflict, Transfer};
use mh_files_platform::ops::{FileOp, OnConflict};

use crate::app::FilesApp;
use crate::operations::Followup;
use crate::{theme, widgets};

pub enum Dialog {
    Delete {
        paths: Vec<PathBuf>,
        permanent: bool,
    },
    Conflicts {
        transfer: Transfer,
        conflicts: Vec<Conflict>,
        choices: Vec<OnConflict>,
        followup: Option<Followup>,
    },
    /// Заменить лишние копии жёсткими ссылками: `(оставляемая, лишняя, размер)`.
    HardLinks {
        tab: u64,
        pairs: Vec<(PathBuf, mh_files_core::duplicates::Member, u64)>,
    },
}

impl Dialog {
    pub fn delete(paths: Vec<PathBuf>, permanent: bool) -> Dialog {
        Dialog::Delete { paths, permanent }
    }

    /// По умолчанию — «Заменить», как в первом вопросе Проводника.
    pub fn conflicts(
        transfer: Transfer,
        conflicts: Vec<Conflict>,
        followup: Option<Followup>,
    ) -> Dialog {
        let choices = vec![OnConflict::Replace; conflicts.len()];
        Dialog::Conflicts { transfer, conflicts, choices, followup }
    }
}

/// Сколько путей перечислить; остальные — числом.
const LISTED: usize = 12;

pub fn show(ctx: &egui::Context, app: &mut FilesApp) {
    match &app.dialog {
        Some(Dialog::Delete { .. }) => delete(ctx, app),
        Some(Dialog::Conflicts { .. }) => conflicts(ctx, app),
        Some(Dialog::HardLinks { .. }) => hard_links(ctx, app),
        None => {}
    }
}

fn delete(ctx: &egui::Context, app: &mut FilesApp) {
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
            let confirm =
                if permanent { "Удалить насовсем" } else { "В корзину" };
            if widgets::button(ui, confirm, true).clicked() {
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
            app.submit(FileOp::Delete { paths, permanent }, None);
        }
    }
}

/// Подтверждение замены копий жёсткими ссылками: точный список того, что заменится.
fn hard_links(ctx: &egui::Context, app: &mut FilesApp) {
    let Some(Dialog::HardLinks { tab, pairs }) = &app.dialog else { return };
    let (tab, freed) = (*tab, pairs.iter().map(|(_, _, size)| size).sum::<u64>());
    let mut result: Option<bool> = None;
    let modal = egui::Modal::new(Id::new("confirm-hard-links")).show(ctx, |ui| {
        ui.set_width(560.0);
        ui.label(RichText::new("Заменить копии жёсткими ссылками?").font(theme::bold(20.0)));
        ui.add_space(6.0);
        ui.label(
            RichText::new(format!(
                "{} останутся на своих местах, но станут одним файлом с оставляемой копией — освободится {}. Правка любого из них меняет все сразу; удаление одного не трогает остальные. Только в пределах одного диска NTFS.",
                format::items(pairs.len()),
                format::size(freed)
            ))
            .color(theme::TEXT_SECONDARY),
        );
        ui.add_space(6.0);
        egui::Frame::new().fill(theme::FIELD).corner_radius(4).inner_margin(8).show(ui, |ui| {
            ui.set_width(ui.available_width());
            for (keeper, extra, _) in pairs.iter().take(LISTED) {
                ui.label(
                    RichText::new(format!("{} → {}", extra.path.display(), keeper.display()))
                        .font(theme::regular(13.0))
                        .color(theme::TEXT_SECONDARY),
                );
            }
            if pairs.len() > LISTED {
                ui.label(
                    RichText::new(format!("и ещё {}", pairs.len() - LISTED))
                        .color(theme::TEXT_DISABLED),
                );
            }
        });
        ui.add_space(12.0);
        ui.horizontal(|ui| {
            if widgets::button(ui, "Заменить ссылками", true).clicked() {
                result = Some(true);
            }
            if widgets::button(ui, "Отмена", false).clicked() {
                result = Some(false);
            }
        });
    });
    if modal.should_close() || ctx.input(|i| i.key_pressed(Key::Escape)) {
        result.get_or_insert(false);
    }
    if let Some(confirmed) = result
        && let Some(Dialog::HardLinks { pairs, .. }) = app.dialog.take()
        && confirmed
    {
        let ticket = mh_files_fs::Ticket { owner: tab, generation: 0 };
        app.workers.link_duplicates(ticket, pairs);
        app.set_status("заменяю копии жёсткими ссылками…", crate::app::Level::Info);
    }
}

const CHOICES: [(OnConflict, &str); 3] = [
    (OnConflict::Replace, "Заменить"),
    (OnConflict::KeepBoth, "Оставить оба"),
    (OnConflict::Skip, "Пропустить"),
];

fn conflicts(ctx: &egui::Context, app: &mut FilesApp) {
    let Some(Dialog::Conflicts { transfer, conflicts, choices, .. }) = &mut app.dialog else {
        return;
    };
    let mut result: Option<bool> = None;
    let screen = ctx.content_rect();
    let modal = egui::Modal::new(Id::new("conflicts")).show(ctx, |ui| {
        ui.set_width((screen.width() - 80.0).clamp(420.0, 760.0));
        let verb = if transfer.copy { "Копирование" } else { "Перемещение" };
        ui.label(RichText::new(format!("{verb}: занятые имена")).font(theme::bold(20.0)));
        ui.add_space(4.0);
        let dest = mh_files_core::location::path_label(&transfer.dest);
        ui.label(
            RichText::new(format!(
                "В «{dest}» уже есть {} с такими именами. Что сделать?",
                format::items(conflicts.len())
            ))
            .color(theme::TEXT_SECONDARY),
        );
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            ui.label(RichText::new("Для всех:").color(theme::TEXT_SECONDARY));
            for (choice, title) in CHOICES {
                if ui.button(title).clicked() {
                    choices.iter_mut().for_each(|c| *c = choice);
                }
            }
        });
        ui.add_space(6.0);
        egui::Frame::new().fill(theme::FIELD).corner_radius(6).inner_margin(8).show(ui, |ui| {
            ScrollArea::vertical().max_height((screen.height() * 0.5).max(160.0)).show(ui, |ui| {
                ui.set_width(ui.available_width());
                for (index, conflict) in conflicts.iter().enumerate() {
                    conflict_row(ui, conflict, &mut choices[index]);
                    if index + 1 < conflicts.len() {
                        ui.separator();
                    }
                }
            });
        });
        ui.add_space(12.0);
        ui.horizontal(|ui| {
            if widgets::button(ui, "Продолжить", true).clicked() {
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
    let Some(confirmed) = result else { return };
    let Some(Dialog::Conflicts { transfer, conflicts, choices, followup }) = app.dialog.take()
    else {
        return;
    };
    if confirmed {
        let decisions = conflicts.into_iter().zip(choices).collect();
        app.resolve_conflicts(transfer, decisions, followup);
    }
}

/// Строка конфликта: имя, обе стороны (новее — акцентом) и выбор.
fn conflict_row(ui: &mut egui::Ui, conflict: &Conflict, choice: &mut OnConflict) {
    let name =
        conflict.target.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    ui.horizontal(|ui| {
        ui.vertical(|ui| {
            ui.label(RichText::new(name).font(theme::bold(14.5)));
            let newer = match (conflict.incoming.modified, conflict.existing.modified) {
                (Some(a), Some(b)) => Some(a > b),
                _ => None,
            };
            side(ui, "Новый", &conflict.incoming, newer == Some(true));
            side(ui, "Есть", &conflict.existing, newer == Some(false));
        });
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            for (value, title) in CHOICES.iter().rev() {
                ui.selectable_value(choice, *value, *title);
            }
        });
    });
}

fn side(ui: &mut egui::Ui, label: &str, side: &mh_files_fs::transfer::Side, newer: bool) {
    let size = if side.is_dir { "папка".to_string() } else { format::size(side.size) };
    let date = side.modified.map(format::date_full).unwrap_or_default();
    let color: Color32 = if newer { theme::accent() } else { theme::TEXT_SECONDARY };
    let mark = if newer { " · новее" } else { "" };
    ui.label(
        RichText::new(format!("{label}: {size}, {date}{mark}"))
            .font(theme::regular(13.0))
            .color(color),
    );
}
