//! Инспектор справа: предпросмотр и сведения об объекте под курсором.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use std::collections::HashMap;

use eframe::egui::{self, RichText, ScrollArea, TextureHandle, Ui};
use mh_files_core::Entry;
use mh_files_core::format;
use mh_files_core::location::{Location, path_label};
use mh_files_fs::{CancelToken, Preview, PreviewRequest, Ticket};
use mh_files_platform::preview_handler::{PreviewHost, has_handler};

use crate::app::{Action, FilesApp, OWNER_INSPECTOR};
use crate::commands::CommandId;
use crate::{preview_ui, theme, widgets};

/// Пока курсор бежит по списку стрелками, предпросмотр не запрашивается.
const SETTLE: Duration = Duration::from_millis(110);

#[derive(Default)]
pub struct State {
    path: Option<PathBuf>,
    is_dir: bool,
    generation: u64,
    cancel: Option<CancelToken>,
    preview: Option<Preview>,
    texture: Option<TextureHandle>,
    wanted: Option<(PathBuf, Instant)>,
    /// Страница PDF.
    page: u32,
    /// Есть ли обработчик предпросмотра Windows у расширения — реестр читается один раз.
    handlers: HashMap<String, bool>,
    host: PreviewHost,
    /// Где в этом кадре должен стоять обработчик: путь и место в точках.
    handler_rect: Option<(PathBuf, egui::Rect)>,
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
    state.path = Some(path);
    state.is_dir = is_dir;
    state.page = 0;
    request(app);
}

/// Попросить предпросмотр того, что выбрано, с текущей страницей.
fn request(app: &mut FilesApp) {
    let state = &mut app.inspector;
    let Some(path) = state.path.clone() else { return };
    if let Some(cancel) = state.cancel.take() {
        cancel.cancel();
    }
    state.generation += 1;
    state.preview = None;
    state.texture = None;
    let request = PreviewRequest {
        path,
        is_dir: state.is_dir,
        max_side: 512,
        text_limit: app.settings.preview.text_limit_kb as usize * 1024,
        image_limit: u64::from(app.settings.preview.image_limit_mb) * 1024 * 1024,
        page: state.page,
    };
    let ticket = Ticket { owner: OWNER_INSPECTOR, generation: state.generation };
    state.cancel = Some(app.workers.preview(ticket, request));
}

/// Показать документ обработчиком Windows вместо своего предпросмотра? Свои — картинки,
/// текст, PDF, архивы, папки; остальное — если обработчик зарегистрирован.
fn wants_handler(app: &mut FilesApp, path: &std::path::Path, is_dir: bool) -> bool {
    if !cfg!(windows) || !app.settings.preview.handlers || is_dir {
        return false;
    }
    let name = path.file_name().unwrap_or_default().to_string_lossy();
    let ext = mh_files_core::entry::extension_of(&name);
    let own = ext.is_empty()
        || ext == "pdf"
        || mh_files_fs::images::decodable(&ext)
        || mh_files_fs::preview::text_like(&ext)
        || mh_files_fs::archive::is_archive_ext(&ext);
    // Внутри архива файла на диске нет — обработчику нечего открыть.
    if own || matches!(app.tab().location, Location::Archive { .. }) {
        return false;
    }
    *app.inspector.handlers.entry(ext.clone()).or_insert_with(|| has_handler(&ext))
}

/// После кадра: поставить окно обработчика туда, где Инспектор оставил ему место, или
/// спрятать. Поверх окна обработчика egui рисовать не может — при открытых меню, палитре и
/// диалогах оно прячется.
pub fn sync_host(ctx: &egui::Context, app: &mut FilesApp) {
    let overlay = app.palette.is_some()
        || app.dialog.is_some()
        || app.batch.is_some()
        || app.quick.is_some()
        || app.crumb_menu.is_some()
        || egui::Popup::is_any_open(ctx)
        || egui::DragAndDrop::has_any_payload(ctx);
    let state = &mut app.inspector;
    match state.handler_rect.take() {
        Some((path, rect)) if !overlay && app.settings.panes.show_inspector => {
            let scale = ctx.pixels_per_point();
            let physical = (
                (rect.left() * scale).round() as i32,
                (rect.top() * scale).round() as i32,
                (rect.width() * scale).round() as i32,
                (rect.height() * scale).round() as i32,
            );
            state.host.show(&path, physical);
        }
        _ => state.host.hide(),
    }
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
    let is_dir = app.inspector.is_dir;
    let handler = wants_handler(app, &path, is_dir);
    ScrollArea::vertical().id_salt("inspector").auto_shrink([false, false]).show(ui, |ui| {
        let max = (ui.available_height() * 0.5).clamp(160.0, 420.0);
        if handler {
            // Место под окно обработчика: оно встанет сюда после кадра.
            let (rect, _) =
                ui.allocate_exact_size(egui::vec2(ui.available_width(), max), egui::Sense::hover());
            ui.painter().rect_filled(rect, egui::CornerRadius::same(4), theme::FIELD);
            let note = app
                .inspector
                .host
                .error()
                .unwrap_or_else(|| "обработчик предпросмотра Windows…".to_string());
            ui.painter().text(
                rect.center(),
                egui::Align2::CENTER_CENTER,
                note,
                theme::regular(12.5),
                theme::TEXT_DISABLED,
            );
            // Окно обработчика не прокручивается вместе с Инспектором: только если видно всё.
            if ui.clip_rect().contains_rect(rect) {
                app.inspector.handler_rect = Some((path.clone(), rect));
            }
        } else if let Some(page) = preview_ui::show(
            ui,
            app.inspector.preview.as_ref(),
            app.inspector.texture.as_ref(),
            max,
            "inspector-text",
        ) {
            app.inspector.page = page;
            request(app);
        }
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
        let in_archive = matches!(app.tab().location, Location::Archive { .. });
        if entry.as_ref().is_none_or(Entry::is_dir) && !in_archive {
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
