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
    /// Размер выбранной папки считается сам: какой папки и как отменить.
    sizing: Option<(PathBuf, CancelToken)>,
    /// Кадр анимации (GIF) и когда следующий.
    playback: preview_ui::Playback,
    /// Все теги раскрыты кнопкой, хотя в настройках — только первые; до другого объекта.
    all_tags: bool,
}

impl State {
    /// Размер папки посчитан.
    pub fn size_done(&mut self, path: &std::path::Path) {
        if self.sizing.as_ref().is_some_and(|(p, _)| p == path) {
            self.sizing = None;
        }
    }

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
        self.playback = preview_ui::Playback::default();
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
    state.all_tags = false;
    request(app);
    request_size(app);
}

/// Размер выбранной папки — сразу: из индекса поиска мгновенно, иначе обходом на своих
/// дисках. На сетевых дисках и приводах обхода нет — там остаётся кнопка.
fn request_size(app: &mut FilesApp) {
    if let Some((_, cancel)) = app.inspector.sizing.take() {
        cancel.cancel();
    }
    let Some(path) = app.inspector.path.clone() else { return };
    if !app.inspector.is_dir
        || !app.settings.preview.inspector_folder_size
        || matches!(app.tab().location, Location::Archive { .. } | Location::RecycleBin)
        // Корень диска — это весь диск: его занятое место видно и так.
        || path.parent().is_none()
    {
        return;
    }
    let network = path.components().next().is_some_and(|c| {
        matches!(
            c,
            std::path::Component::Prefix(p)
                if matches!(p.kind(), std::path::Prefix::UNC(..) | std::path::Prefix::VerbatimUNC(..))
        )
    });
    let local = !network
        && app
            .drives
            .iter()
            .filter(|d| path.starts_with(&d.root))
            .max_by_key(|d| d.root.as_os_str().len())
            .is_none_or(|d| {
                d.ready
                    && matches!(
                        d.kind,
                        mh_files_platform::drives::DriveKind::Fixed
                            | mh_files_platform::drives::DriveKind::Removable
                            | mh_files_platform::drives::DriveKind::Ram
                    )
            });
    let ticket = Ticket { owner: crate::app::OWNER_INSPECTOR_SIZE, generation: 0 };
    let cancel = app.indexer.folder_sizes(ticket, vec![path.clone()], local);
    // Без обхода ответа может и не быть (тома нет в индексе) — тогда остаётся кнопка.
    if local {
        app.inspector.sizing = Some((path, cancel));
    }
}

/// Попросить предпросмотр того, что выбрано, с текущей страницей. Предпросмотр скрыт в
/// настройках — не просить: его попросит показ, когда его включат.
fn request(app: &mut FilesApp) {
    let state = &mut app.inspector;
    let Some(path) = state.path.clone() else { return };
    if let Some(cancel) = state.cancel.take() {
        cancel.cancel();
    }
    state.generation += 1;
    state.preview = None;
    state.texture = None;
    if !app.settings.inspector.preview {
        return;
    }
    // В корзине объект лежит не по своему пути: показать нечего, кроме сведений.
    if app.tab().location == Location::RecycleBin {
        app.inspector.preview = Some(Preview::Info(Vec::new()));
        return;
    }
    let state = &mut app.inspector;
    let request = PreviewRequest {
        path,
        is_dir: state.is_dir,
        max_side: 512,
        text_limit: app.settings.preview.text_limit_kb as usize * 1024,
        image_limit: u64::from(app.settings.preview.image_limit_mb) * 1024 * 1024,
        page: state.page,
        animate: true,
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
        || mh_files_fs::font::is_font(&ext)
        || mh_files_fs::archive::is_archive_ext(&ext);
    // Внутри архива файла на диске нет — обработчику нечего открыть.
    if own || matches!(app.tab().location, Location::Archive { .. } | Location::RecycleBin) {
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
        || app.drop_menu.is_some()
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
        let gap = app.settings.inspector.spacing;
        ui.add_space(gap + 2.0);
        field(ui, gap, "Папок", &dirs.to_string());
        field(ui, gap, "Файлов", &(entries.len() - dirs).to_string());
        field(ui, gap, "Размер файлов", &format::size(bytes));
        ui.add_space(gap * 2.0 + 2.0);
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
    let view = app.settings.inspector.clone();
    let gap = view.spacing;
    let handler = view.preview && wants_handler(app, &path, is_dir);
    // Предпросмотр только что включили в настройках — попросить его.
    if view.preview && !handler && app.inspector.preview.is_none() && app.inspector.cancel.is_none()
    {
        request(app);
    }
    ScrollArea::vertical().id_salt("inspector").auto_shrink([false, false]).show(ui, |ui| {
        let max = (ui.available_height() * 0.5).clamp(160.0, 420.0);
        let state = &mut app.inspector;
        state.playback.tick(ui.ctx(), state.preview.as_ref(), state.texture.as_mut());
        if !view.preview {
            // Без предпросмотра — сразу сведения.
        } else if handler {
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
        if view.preview {
            ui.add_space(gap * 2.0 + 2.0);
        }
        ui.add(
            egui::Label::new(
                RichText::new(path_label(&path)).font(theme::bold(17.0)).color(theme::TEXT_PRIMARY),
            )
            .wrap(),
        );
        ui.add_space(gap + 2.0);
        if let Some(entry) = &entry {
            if view.kind {
                field(ui, gap, "Тип", &format::kind(&entry.extension(), entry.is_dir()));
            }
            if view.size && !entry.is_dir() {
                field(
                    ui,
                    gap,
                    "Размер",
                    &format!("{} ({} байт)", format::size(entry.size), group_digits(entry.size)),
                );
            }
            if let Some(time) = entry.modified.filter(|_| view.modified) {
                let dot = crate::pane_view::age_dot_color(app, time);
                let title = if app.tab().location == Location::RecycleBin {
                    "Удалён"
                } else {
                    "Изменён"
                };
                dated_field(ui, gap, title, &format::date_full(time), dot);
            }
            if let Some(time) = entry.created.filter(|_| view.created) {
                let dot = crate::pane_view::age_dot_color(app, time);
                dated_field(ui, gap, "Создан", &format::date_full(time), dot);
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
            if view.attributes && !attributes.is_empty() {
                field(ui, gap, "Атрибуты", &attributes.join(", "));
            }
            if view.tags && !entry.is_dir() && crate::pane_view::real_files(&app.tab().location) {
                tags(ui, app, entry);
            }
        }
        // Папка: размер целиком — по запросу, обход может быть долгим.
        let in_archive =
            matches!(app.tab().location, Location::Archive { .. } | Location::RecycleBin);
        if view.folder_size && entry.as_ref().is_none_or(Entry::is_dir) && !in_archive {
            folder_size(ui, app, &path);
        }
        if !view.path {
            return;
        }
        ui.add_space(gap);
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

/// Разделы «Теги» и «Звук»: что на картинке или кадрах видео и что слышно. Щелчок по тегу —
/// поиск по нему, «+» — добавить к поиску (или к фильтру папки), правый — ещё.
fn tags(ui: &mut Ui, app: &mut FilesApp, entry: &Entry) {
    let gap = app.settings.inspector.spacing;
    let name = entry.name.as_str();
    let ext = mh_files_core::entry::extension_of(name);
    let label = |ui: &mut Ui, text: &str| {
        ui.label(RichText::new(text).font(theme::regular(12.5)).color(theme::TEXT_DISABLED));
    };
    if !app.settings.tags.enabled {
        let media = mh_files_fs::tags::picture::taggable(&ext)
            || mh_files_core::category::Category::Audio.extensions().contains(&ext.as_str());
        if media {
            label(ui, "Теги");
            ui.horizontal_wrapped(|ui| {
                ui.label(RichText::new("автотеги выключены").color(theme::TEXT_SECONDARY));
                if ui.small_button("Включить…").clicked() {
                    app.settings_window.open_tags(&app.settings, &app.sorter.config);
                }
            });
            ui.add_space(gap);
        }
        return;
    }
    let path = entry.path();
    let parts = app.tagger.wanted_parts(&path, entry.size);
    if parts == 0 {
        return;
    }
    let modified = entry.modified.map_or(0, mh_files_core::index::unix);
    let shown = match app.tagger.lookup(&path, entry.size, modified) {
        mh_files_fs::Lookup::Ready(shown) => shown,
        mh_files_fs::Lookup::Pending => {
            field(ui, gap, "Теги", "определяются…");
            return;
        }
        mh_files_fs::Lookup::None => {
            if let mh_files_fs::TagState::Failed(error) = app.tagger.status().state {
                label(ui, "Теги");
                ui.add(egui::Label::new(RichText::new(error).color(theme::WARN)).wrap());
                ui.add_space(gap);
            }
            return;
        }
    };
    let image = parts & mh_files_core::tags::PART_IMAGE != 0;
    let sound = parts & mh_files_core::tags::PART_SOUND != 0;
    if image
        && app.settings.tags.rating
        && let Some(rating) = &shown.rating
    {
        let text = format!(
            "{} · {:.0}%",
            mh_files_core::tags::rating_label(&rating.name),
            rating.score * 100.0
        );
        label(ui, "Рейтинг");
        let response = ui
            .add(
                egui::Label::new(
                    RichText::new(text).font(theme::regular(14.0)).color(theme::TEXT_PRIMARY),
                )
                .sense(egui::Sense::click()),
            )
            .on_hover_text("Щелчок — все файлы с таким рейтингом; правый — скрыть такие в папке")
            .on_hover_cursor(egui::CursorIcon::PointingHand);
        let token = format!("rating:{}", rating.name);
        if response.clicked() {
            search_tags(app, token.clone());
        }
        response.context_menu(|ui| {
            if ui.button("Все файлы с таким рейтингом").clicked() {
                search_tags(app, token.clone());
                ui.close();
            }
            if ui.button("Скрыть такие в этой папке").clicked() {
                let text = app.tab().filter.clone();
                if !mh_files_core::tags::rating_hidden(&text, &rating.name) {
                    filter_folder(app, mh_files_core::tags::toggle_rating(&text, &rating.name));
                }
                ui.close();
            }
        });
        ui.add_space(gap);
    }
    if image {
        let empty = "модель ничего уверенно не узнала";
        chips(ui, app, "Теги", &shown.tags, &shown.line, empty);
        ui.add_space(gap);
    }
    if sound {
        let line = shown.sounds.iter().map(|t| t.name.as_str()).collect::<Vec<_>>().join(", ");
        let empty = if shown.sounds.is_empty() && !image && shown.tags.is_empty() {
            "ничего уверенно не слышно (или звука нет)"
        } else {
            "ничего уверенно не слышно"
        };
        chips(ui, app, "Звук", &shown.sounds, &line, empty);
        ui.add_space(gap);
    }
}

/// Заголовок с числом и кнопкой «Копировать», затем теги плашками: все или первые сколько
/// задано в настройках, остальные — кнопкой «Ещё».
fn chips(
    ui: &mut Ui,
    app: &mut FilesApp,
    title: &str,
    tags: &[mh_files_core::tags::Tag],
    line: &str,
    empty: &str,
) {
    ui.horizontal(|ui| {
        ui.label(
            RichText::new(format!("{title} · {}", tags.len()))
                .font(theme::regular(12.5))
                .color(theme::TEXT_DISABLED),
        );
        if !tags.is_empty()
            && ui.small_button("Копировать").on_hover_text("Все через запятую").clicked()
        {
            ui.ctx().copy_text(line.to_string());
            app.actions.push(Action::Status("теги скопированы".into(), crate::app::Level::Info));
        }
    });
    let gap = app.settings.inspector.spacing;
    if tags.is_empty() {
        ui.label(RichText::new(empty).color(theme::TEXT_SECONDARY));
        ui.add_space(gap);
        return;
    }
    let limit = app.settings.inspector.tag_limit as usize;
    let folded = limit > 0 && tags.len() > limit;
    let shown = if folded && !app.inspector.all_tags { &tags[..limit] } else { tags };
    let in_search = matches!(app.tab().location, Location::Tags { .. });
    let plus_hint = if in_search {
        "Добавить к этому поиску по тегам"
    } else {
        "Добавить к фильтру папки: только файлы и с этим тегом"
    };
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing = egui::vec2(4.0, 4.0);
        for tag in shown {
            // Персонажи — цветом акцента: их меньше, и они важнее.
            let color = if tag.kind == mh_files_core::tags::TagKind::Character {
                theme::accent()
            } else {
                theme::TEXT_PRIMARY
            };
            let kind = match tag.kind {
                mh_files_core::tags::TagKind::Character => "персонаж · ",
                mh_files_core::tags::TagKind::Sound => "звук · ",
                _ => "",
            };
            let token = mh_files_core::tags::query_token(&tag.name);
            let (response, plus) = tag_chip(ui, &tag.name, color);
            if plus {
                if response.clicked() {
                    add_to_search(app, &token);
                }
                response.clone().on_hover_text(plus_hint);
            } else {
                let hint = format!(
                    "{kind}уверенность {:.0}%\nщелчок — все файлы с этим тегом, «+» — добавить к поиску, правый — ещё",
                    tag.score * 100.0
                );
                if response.clicked() {
                    search_tags(app, token.clone());
                }
                response.clone().on_hover_text(hint);
            }
            response.context_menu(|ui| {
                if ui.button("Все файлы с этим тегом").clicked() {
                    search_tags(app, token.clone());
                    ui.close();
                }
                if in_search && ui.button("Добавить к поиску").clicked() {
                    add_to_search(app, &token);
                    ui.close();
                }
                if ui.button("Только с этим тегом в папке").clicked() {
                    add_to_filter(app, &format!("tag:{token}"));
                    ui.close();
                }
                if ui.button("Скрыть с этим тегом в папке").clicked() {
                    add_to_filter(app, &format!("-tag:{token}"));
                    ui.close();
                }
                ui.separator();
                if ui.button("Копировать").clicked() {
                    ui.ctx().copy_text(tag.name.clone());
                    app.actions.push(Action::Status(
                        format!("тег «{}» скопирован", tag.name),
                        crate::app::Level::Info,
                    ));
                    ui.close();
                }
            });
        }
        if folded {
            let (text, hint) = if app.inspector.all_tags {
                ("Свернуть".to_string(), format!("Только первые {limit}"))
            } else {
                (format!("Ещё {}", tags.len() - limit), "Показать все теги".to_string())
            };
            if more_chip(ui, &text).on_hover_text(hint).clicked() {
                app.inspector.all_tags = !app.inspector.all_tags;
            }
        }
    });
    ui.add_space(gap);
}

/// Маленькая кнопка в ряду тегов: «Ещё 12» или «Свернуть».
fn more_chip(ui: &mut Ui, text: &str) -> egui::Response {
    let font = theme::regular(12.5);
    let galley = ui.painter().layout_no_wrap(text.to_string(), font, theme::accent());
    let size = egui::vec2(galley.size().x + 12.0, galley.size().y + 6.0);
    let (rect, response) = ui.allocate_exact_size(size, egui::Sense::click());
    if ui.is_rect_visible(rect) {
        if response.hovered() {
            ui.painter().rect_filled(rect, egui::CornerRadius::same(4), theme::CARD);
        }
        let pos = rect.center() - galley.size() / 2.0;
        ui.painter().galley(pos, galley, theme::accent());
    }
    response.on_hover_cursor(egui::CursorIcon::PointingHand)
}

/// Плашка тега с «+» слева. Второе — указатель над «+» (щелчок по нему — добавить к поиску).
fn tag_chip(ui: &mut Ui, name: &str, color: egui::Color32) -> (egui::Response, bool) {
    let font = theme::regular(13.0);
    let galley = ui.painter().layout_no_wrap(name.to_string(), font.clone(), color);
    let plus_width = 18.0;
    let size = egui::vec2(plus_width + galley.size().x + 12.0, galley.size().y + 6.0);
    let (rect, response) = ui.allocate_exact_size(size, egui::Sense::click());
    let plus_rect = egui::Rect::from_min_size(rect.min, egui::vec2(plus_width, rect.height()));
    let on_plus = response.hover_pos().is_some_and(|p| plus_rect.contains(p))
        || (response.clicked()
            && response.interact_pointer_pos().is_some_and(|p| plus_rect.contains(p)));
    if ui.is_rect_visible(rect) {
        let painter = ui.painter();
        let radius = egui::CornerRadius::same(4);
        let fill = if response.hovered() && !on_plus { theme::CARD } else { theme::FIELD };
        painter.rect(
            rect,
            radius,
            fill,
            egui::Stroke::new(1.0, theme::CARD_STROKE),
            egui::StrokeKind::Inside,
        );
        if on_plus {
            let left = egui::CornerRadius { nw: 4, sw: 4, ne: 0, se: 0 };
            painter.rect_filled(plus_rect.shrink(1.0), left, theme::CARD);
        }
        painter.vline(
            plus_rect.right(),
            rect.y_range().shrink(4.0),
            egui::Stroke::new(1.0, theme::CARD_STROKE),
        );
        let plus_color = if on_plus { theme::accent() } else { theme::TEXT_SECONDARY };
        let c = plus_rect.center();
        let stroke = egui::Stroke::new(1.5, plus_color);
        painter.line_segment([c - egui::vec2(4.0, 0.0), c + egui::vec2(4.0, 0.0)], stroke);
        painter.line_segment([c - egui::vec2(0.0, 4.0), c + egui::vec2(0.0, 4.0)], stroke);
        let text_pos = egui::pos2(plus_rect.right() + 6.0, rect.center().y - galley.size().y / 2.0);
        painter.galley(text_pos, galley, color);
    }
    let response = response.on_hover_cursor(egui::CursorIcon::PointingHand);
    (response, on_plus)
}

/// «+» у тега: во вкладке поиска по тегам — к её запросу, иначе — к фильтру папки.
fn add_to_search(app: &mut FilesApp, token: &str) {
    let Location::Tags { query } = &app.tab().location else {
        add_to_filter(app, &format!("tag:{token}"));
        return;
    };
    let current = app.tab().index.as_ref().map_or_else(|| query.clone(), |view| view.text.clone());
    let (parsed, _) = mh_files_core::tags::TagQuery::parse(&current, true);
    let (wanted, _) = mh_files_core::tags::TagQuery::parse(token, true);
    if wanted.include.iter().all(|t| parsed.include.contains(t)) {
        return;
    }
    let text = if current.trim().is_empty() {
        token.to_string()
    } else {
        format!("{} {token}", current.trim_end())
    };
    let workers = app.workers.clone();
    app.tab_mut().set_index_query(text, &workers);
}

/// Поиск по тегам в новой вкладке.
fn search_tags(app: &mut FilesApp, query: String) {
    app.actions.push(Action::Open {
        location: Location::Tags { query },
        target: crate::app::Target::NewTab,
    });
}

/// Дописать слово в фильтр текущей папки (если его там нет) и показать строку фильтра.
fn add_to_filter(app: &mut FilesApp, word: &str) {
    let text = app.tab().filter.clone();
    if text.split_whitespace().any(|w| w.eq_ignore_ascii_case(word)) {
        return;
    }
    let text = if text.trim().is_empty() {
        word.to_string()
    } else {
        format!("{} {word}", text.trim_end())
    };
    filter_folder(app, text);
}

fn filter_folder(app: &mut FilesApp, text: String) {
    let tab = app.tab_mut();
    tab.filter_open = true;
    tab.set_filter(text);
}

fn field(ui: &mut Ui, gap: f32, label: &str, value: &str) {
    ui.label(RichText::new(label).font(theme::regular(12.5)).color(theme::TEXT_DISABLED));
    ui.add(
        egui::Label::new(
            RichText::new(value).font(theme::regular(14.0)).color(theme::TEXT_PRIMARY),
        )
        .wrap(),
    );
    ui.add_space(gap);
}

/// Поле с датой и кружком давности слева от неё.
fn dated_field(ui: &mut Ui, gap: f32, label: &str, value: &str, dot: Option<egui::Color32>) {
    let Some(color) = dot else {
        field(ui, gap, label, value);
        return;
    };
    ui.label(RichText::new(label).font(theme::regular(12.5)).color(theme::TEXT_DISABLED));
    ui.horizontal(|ui| {
        let (rect, _) = ui.allocate_exact_size(egui::vec2(10.0, 16.0), egui::Sense::hover());
        ui.painter().circle_filled(rect.center(), 4.0, color);
        ui.add_space(2.0);
        ui.add(
            egui::Label::new(
                RichText::new(value).font(theme::regular(14.0)).color(theme::TEXT_PRIMARY),
            )
            .wrap(),
        );
    });
    ui.add_space(gap);
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
    let gap = app.settings.inspector.spacing;
    if let Some(size) = app.folder_sizes.get(path) {
        let partial = if size.partial { " (без недоступных папок)" } else { "" };
        let text = format!(
            "{} · файлов: {}, папок: {}{partial}",
            format::size(size.bytes),
            size.files,
            size.dirs
        );
        field(ui, gap, "Размер целиком", &text);
    } else if app.sizes_pending.contains(path)
        || app.inspector.sizing.as_ref().is_some_and(|(p, _)| p == path)
    {
        field(ui, gap, "Размер целиком", "считается…");
    } else if ui.button("Посчитать размер").clicked() {
        app.actions.push(Action::FolderSizes(vec![path.to_path_buf()]));
    }
    ui.add_space(gap);
}
