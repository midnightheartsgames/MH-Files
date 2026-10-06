//! Инспектор справа: предпросмотр и сведения об объекте под курсором.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use eframe::egui::{self, RichText, ScrollArea, TextureHandle, Ui};
use mh_files_core::Entry;
use mh_files_core::format;
use mh_files_core::location::{Location, path_label};
use mh_files_fs::{CancelToken, Preview, PreviewRequest, Ticket};

use crate::app::{Action, FilesApp, OWNER_INSPECTOR};
use crate::commands::CommandId;
use crate::{preview_ui, theme, widgets};

/// Пока курсор бежит по списку стрелками, предпросмотр не запрашивается.
const SETTLE: Duration = Duration::from_millis(110);

#[derive(Default)]
pub struct State {
    path: Option<PathBuf>,
    generation: u64,
    cancel: Option<CancelToken>,
    preview: Option<Preview>,
    texture: Option<TextureHandle>,
    wanted: Option<(PathBuf, Instant)>,
}

impl State {
    pub fn on_preview(
        &mut self,
        ctx: &egui::Context,
        ticket: Ticket,
        path: PathBuf,
        preview: Preview,
    ) {
        if ticket.generation != self.generation || self.path.as_ref() != Some(&path) {
            return;
        }
        self.texture = preview_ui::texture_of(ctx, &preview, "inspector");
        self.preview = Some(preview);
        self.cancel = None;
    }
}

/// Что показывать: объект под курсором или текущая папка.
fn subject(app: &FilesApp) -> Option<(PathBuf, bool)> {
    let tab = app.tab();
    let targets = tab.targets();
    if let Some(path) = targets.first().filter(|_| targets.len() == 1) {
        let is_dir = tab.entry(path).is_some_and(Entry::is_dir);
        return Some((path.clone(), is_dir));
    }
    if !targets.is_empty() {
        return None;
    }
    tab.dir().map(|dir| (dir, true))
}

/// Следит за курсором и просит предпросмотр, когда курсор остановился.
pub fn follow(app: &mut FilesApp) {
    if !app.settings.panes.show_inspector {
        return;
    }
    let subject = subject(app);
    let state = &mut app.inspector;
    let Some((path, is_dir)) = subject else {
        state.path = None;
        state.wanted = None;
        return;
    };
    if state.path.as_ref() == Some(&path) {
        return;
    }
    match &state.wanted {
        Some((wanted, at)) if *wanted == path => {
            if at.elapsed() < SETTLE {
                return;
            }
        }
        _ => {
            state.wanted = Some((path, Instant::now()));
            return;
        }
    }
    state.wanted = None;
    if let Some(cancel) = state.cancel.take() {
        cancel.cancel();
    }
    state.generation += 1;
    state.path = Some(path.clone());
    state.preview = None;
    state.texture = None;
    let request = PreviewRequest {
        path,
        is_dir,
        max_side: 512,
        text_limit: app.settings.preview.text_limit_kb as usize * 1024,
        image_limit: u64::from(app.settings.preview.image_limit_mb) * 1024 * 1024,
    };
    let ticket = Ticket { owner: OWNER_INSPECTOR, generation: state.generation };
    state.cancel = Some(app.workers.preview(ticket, request));
}

pub fn show(ui: &mut Ui, app: &mut FilesApp) {
    if app.inspector.wanted.is_some() {
        ui.ctx().request_repaint_after(SETTLE);
    }
    widgets::section_label(ui, "Инспектор");
    ui.add_space(8.0);
    let tab = app.tab();
    let selected = tab.selection.selected_paths(&tab.listing);
    if selected.len() > 1 {
        let entries: Vec<&Entry> = selected.iter().filter_map(|p| tab.entry(p)).collect();
        let dirs = entries.iter().filter(|e| e.is_dir()).count();
        let bytes: u64 = entries.iter().filter(|e| !e.is_dir()).map(|e| e.size).sum();
        ui.label(
            RichText::new(format!("Выделено: {}", format::items(selected.len())))
                .font(theme::bold(17.0)),
        );
        ui.add_space(6.0);
        field(ui, "Папок", &dirs.to_string());
        field(ui, "Файлов", &(entries.len() - dirs).to_string());
        field(ui, "Размер файлов", &format::size(bytes));
        ui.add_space(10.0);
        if widgets::button(ui, "Пакетное переименование", false).clicked() {
            app.actions.push(Action::Run(CommandId::BatchRename));
        }
        return;
    }
    let Some(path) = app.inspector.path.clone() else {
        if matches!(tab.location, Location::Computer) {
            ui.label(RichText::new("Выберите диск или папку").color(theme::TEXT_DISABLED));
        }
        return;
    };
    let entry = tab.entry(&path).cloned();
    ScrollArea::vertical().id_salt("inspector").auto_shrink([false, false]).show(ui, |ui| {
        let max = (ui.available_height() * 0.5).clamp(160.0, 420.0);
        preview_ui::show(
            ui,
            app.inspector.preview.as_ref(),
            app.inspector.texture.as_ref(),
            max,
            "inspector-text",
        );
        ui.add_space(10.0);
        ui.add(
            egui::Label::new(
                RichText::new(path_label(&path)).font(theme::bold(17.0)).color(theme::TEXT_PRIMARY),
            )
            .wrap(),
        );
        ui.add_space(6.0);
        if let Some(entry) = &entry {
            field(ui, "Тип", &format::kind(&entry.extension(), entry.is_dir()));
            if !entry.is_dir() {
                field(
                    ui,
                    "Размер",
                    &format!("{} ({} байт)", format::size(entry.size), group_digits(entry.size)),
                );
            }
            if let Some(time) = entry.modified {
                field(ui, "Изменён", &format::date_full(time));
            }
            if let Some(time) = entry.created {
                field(ui, "Создан", &format::date_full(time));
            }
            let mut attributes = Vec::new();
            if entry.attributes.readonly() {
                attributes.push("только чтение");
            }
            if entry.attributes.hidden() {
                attributes.push("скрытый");
            }
            if entry.attributes.system() {
                attributes.push("системный");
            }
            if entry.attributes.reparse() {
                attributes.push("ссылка");
            }
            if !attributes.is_empty() {
                field(ui, "Атрибуты", &attributes.join(", "));
            }
        }
        // Папка: размер целиком — по запросу, обход может быть долгим.
        if entry.as_ref().is_none_or(Entry::is_dir) {
            folder_size(ui, app, &path);
        }
        ui.add_space(4.0);
        ui.label(RichText::new("Путь").font(theme::regular(12.5)).color(theme::TEXT_DISABLED));
        let text = path.display().to_string();
        let response = ui.add(
            egui::Label::new(
                RichText::new(&text).font(theme::regular(13.0)).color(theme::TEXT_SECONDARY),
            )
            .wrap()
            .sense(egui::Sense::click()),
        );
        if response.on_hover_text("Щёлкните, чтобы скопировать").clicked() {
            ui.ctx().copy_text(text);
            app.actions.push(Action::Status("путь скопирован".into(), crate::app::Level::Info));
        }
    });
}

fn field(ui: &mut Ui, label: &str, value: &str) {
    ui.label(RichText::new(label).font(theme::regular(12.5)).color(theme::TEXT_DISABLED));
    ui.add(
        egui::Label::new(
            RichText::new(value).font(theme::regular(14.0)).color(theme::TEXT_PRIMARY),
        )
        .wrap(),
    );
    ui.add_space(4.0);
}

/// `8 589 934 592` — пробелы между разрядами.
fn group_digits(value: u64) -> String {
    let digits = value.to_string();
    let mut result = String::new();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            result.push('\u{202F}');
        }
        result.push(c);
    }
    result
}

fn folder_size(ui: &mut Ui, app: &mut FilesApp, path: &std::path::Path) {
    if let Some(size) = app.folder_sizes.get(path) {
        let partial = if size.partial { " (без недоступных папок)" } else { "" };
        let text = format!(
            "{} · файлов: {}, папок: {}{partial}",
            format::size(size.bytes),
            size.files,
            size.dirs
        );
        field(ui, "Размер целиком", &text);
    } else if app.sizes_pending.contains(path) {
        field(ui, "Размер целиком", "считается…");
    } else if ui.button("Посчитать размер").clicked() {
        app.actions.push(Action::FolderSizes(vec![path.to_path_buf()]));
    }
    ui.add_space(4.0);
}
