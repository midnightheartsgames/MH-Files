//! Окно настроек — как у MH Monitoring: разделы слева, карточки в середине, внизу «Применить»
//! и «Отмена». Всё меняется в черновике; рабочие настройки — только кнопкой.

use std::collections::BTreeMap;

use eframe::egui::{
    self, Align, Color32, CornerRadius, Layout, Margin, RichText, ScrollArea, Sense, Ui,
    ViewportBuilder, ViewportClass, ViewportCommand, ViewportId, vec2,
};
use mh_files_core::settings::{
    MAX_FONT_SCALE, MAX_GRID, MAX_LIST_SCALE, MIN_FONT_SCALE, MIN_GRID, MIN_LIST_SCALE,
    NewTabLocation, Settings,
};

use crate::app::FilesApp;
use crate::commands::{CommandId, Keymap, format_chord, parse_chord};
use crate::widgets::{card, page_title, row, switch_row};
use crate::{storage, theme, widgets};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum Page {
    #[default]
    General,
    Appearance,
    Files,
    Preview,
    Index,
    Semantic,
    Tags,
    Sorting,
    System,
    Keys,
    About,
}

impl Page {
    const ALL: [Page; 11] = [
        Page::General,
        Page::Appearance,
        Page::Files,
        Page::Preview,
        Page::Index,
        Page::Semantic,
        Page::Tags,
        Page::Sorting,
        Page::System,
        Page::Keys,
        Page::About,
    ];

    fn title(self) -> &'static str {
        match self {
            Page::General => "Общие",
            Page::Appearance => "Внешний вид",
            Page::Files => "Файлы и папки",
            Page::Preview => "Предпросмотр",
            Page::Index => "Поиск по дискам",
            Page::Semantic => "Поиск по смыслу",
            Page::Tags => "Автотеги",
            Page::Sorting => "Сортировка",
            Page::System => "Система",
            Page::Keys => "Горячие клавиши",
            Page::About => "О программе",
        }
    }
}

#[derive(Default)]
pub struct State {
    open: bool,
    focus: bool,
    page: Page,
    draft: Settings,
    /// Сочетания как текст: «Ctrl+C, Ctrl+Insert».
    keys: BTreeMap<CommandId, String>,
    status: Option<(String, bool)>,
    /// Правка категорий сортировщика.
    categories: crate::categories_editor::Editor,
    /// Поля «добавить папку» на странице индекса.
    new_root: String,
    new_exclude: String,
    /// Поле «добавить папку» поиска по смыслу.
    new_semantic: String,
    /// Исключения поиска по смыслу текстом, по одному на строку.
    semantic_exclude: String,
}

impl State {
    pub fn open(&mut self, settings: &Settings, categories: &mh_files_core::sorting::Config) {
        if !self.open {
            self.draft = settings.clone();
            self.categories = crate::categories_editor::Editor::new(categories);
            let (keymap, _) = Keymap::new(&settings.keys);
            self.keys = CommandId::ALL.iter().map(|&c| (c, keymap.text(c))).collect();
            self.status = None;
            self.semantic_exclude = settings.semantic.exclude.join("\n");
        }
        self.open = true;
        self.focus = true;
    }

    /// Открыть сразу на странице сортировщика (кнопка во вкладке «Разложить»).
    pub fn open_sorting(
        &mut self,
        settings: &Settings,
        categories: &mh_files_core::sorting::Config,
    ) {
        self.open(settings, categories);
        self.page = Page::Sorting;
    }

    /// Открыть сразу на странице поиска по смыслу (кнопка «Настроить…» во вкладке поиска).
    pub fn open_semantic(
        &mut self,
        settings: &Settings,
        categories: &mh_files_core::sorting::Config,
    ) {
        self.open(settings, categories);
        self.page = Page::Semantic;
    }

    /// Открыть сразу на странице автотегов (кнопка в Инспекторе).
    pub fn open_tags(&mut self, settings: &Settings, categories: &mh_files_core::sorting::Config) {
        self.open(settings, categories);
        self.page = Page::Tags;
    }

    /// Набранный, но не добавленный кнопкой путь индекса тоже идёт в список: иначе
    /// «Применить» молча его теряет.
    fn commit_pending_paths(&mut self) {
        for (list, text) in [
            (&mut self.draft.index.roots, &mut self.new_root),
            (&mut self.draft.index.exclude, &mut self.new_exclude),
            (&mut self.draft.semantic.folders, &mut self.new_semantic),
        ] {
            add_path(list, text);
        }
    }

    /// Черновик с разобранными сочетаниями и изменённые категории (если менялись).
    /// Ошибка — текст.
    fn result(&self) -> Result<(Settings, Option<mh_files_core::sorting::Config>), String> {
        let categories = self.categories.result()?;
        Ok((self.settings()?, categories))
    }

    fn settings(&self) -> Result<Settings, String> {
        let mut settings = self.draft.clone().sanitized();
        let mut keys = BTreeMap::new();
        let mut seen: BTreeMap<String, CommandId> = BTreeMap::new();
        for (&command, text) in &self.keys {
            let mut parsed = Vec::new();
            for part in crate::commands::split_chords(text) {
                let chord = parse_chord(part).ok_or_else(|| {
                    format!("«{part}» у команды «{}» не разобрано", command.name())
                })?;
                let label = format_chord(&chord);
                if let Some(other) = seen.insert(label.clone(), command) {
                    return Err(format!(
                        "{label} — и у «{}», и у «{}»",
                        other.name(),
                        command.name()
                    ));
                }
                parsed.push(label);
            }
            let defaults: Vec<String> = command
                .default_keys()
                .iter()
                .filter_map(|k| parse_chord(k).map(|c| format_chord(&c)))
                .collect();
            if parsed != defaults {
                keys.insert(command.key().to_string(), parsed);
            }
        }
        settings.keys = keys;
        Ok(settings)
    }
}

pub fn show(ctx: &egui::Context, app: &mut FilesApp) {
    if !app.settings_window.open {
        return;
    }
    let builder = ViewportBuilder::default()
        .with_title("MH Files — настройки")
        .with_icon(crate::settings_icon())
        .with_inner_size([900.0, 620.0])
        .with_min_inner_size([760.0, 480.0]);
    let mut applied: Option<(Settings, Option<mh_files_core::sorting::Config>)> = None;
    let mut close = false;
    let index_status = app.indexer.status();
    let elevated = app.indexer.elevated();
    let mut rescan = false;
    let semantic_status = app.semantic.status();
    let mut semantic_action = None;
    let tags_status = app.tagger.status();
    let mut tags_action = None;
    let sort_info = SortInfo {
        path: app.sorter.path.clone(),
        history: app.sorter.history.clone(),
        error: app.sorter.error.clone(),
        categories: app.sorter.classifier.category_count(),
        extensions: app.sorter.classifier.extension_count,
    };
    let mut sort_action = None;
    let dirs = storage::dirs();
    let system_info = SystemInfo {
        config: dirs.config.clone(),
        index: dirs.index.clone(),
        semantic: dirs.semantic.clone(),
        tags: dirs.tags.clone(),
        portable: dirs.portable,
        integration: app.integration,
        crashed: app.previous.crashed,
        report: app.previous.report.clone(),
        first_frame: app.diag.first_frame,
        listings: app.diag.listings.iter().take(12).cloned().collect(),
        text: app.diag.report(),
    };
    let mut system_action = None;
    let update_info = (app.update.checking, app.update.result.clone());
    let mut about_action = None;
    ctx.show_viewport_immediate(ViewportId::from_hash_of("settings"), builder, |ui, class| {
        let state = &mut app.settings_window;
        if class == ViewportClass::EmbeddedWindow {
            ui.label("окно настроек недоступно");
            return;
        }
        if std::mem::take(&mut state.focus) {
            ui.ctx().send_viewport_cmd(ViewportCommand::Focus);
        }
        if ui.ctx().input(|i| i.viewport().close_requested()) {
            close = true;
        }
        egui::Panel::bottom("settings-footer")
            .exact_size(56.0)
            .frame(egui::Frame::new().fill(theme::PANEL).inner_margin(Margin::symmetric(20, 12)))
            .show(ui, |ui| {
                ui.horizontal_centered(|ui| {
                    if let Some((text, error)) = &state.status {
                        let color = if *error { theme::CRITICAL } else { theme::TEXT_SECONDARY };
                        ui.label(RichText::new(text).color(color));
                    }
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        let ok = widgets::button(ui, "ОК", true).clicked();
                        let apply = widgets::button(ui, "Применить", false).clicked();
                        if widgets::button(ui, "Отмена", false).clicked() {
                            close = true;
                        }
                        if ok || apply {
                            state.commit_pending_paths();
                            match state.result() {
                                Ok(settings) => {
                                    applied = Some(settings);
                                    state.status = Some(("применено".into(), false));
                                    if ok {
                                        close = true;
                                    }
                                }
                                Err(error) => state.status = Some((error, true)),
                            }
                        }
                    });
                });
            });
        egui::Panel::left("settings-pages")
            .exact_size(200.0)
            .frame(egui::Frame::new().fill(theme::PANEL).inner_margin(Margin::symmetric(12, 14)))
            .show(ui, |ui| {
                for page in Page::ALL {
                    if nav_item(ui, page.title(), state.page == page).clicked() {
                        state.page = page;
                    }
                }
                ui.with_layout(Layout::bottom_up(Align::Min), |ui| {
                    if ui
                        .add_sized(
                            [ui.available_width(), 28.0],
                            egui::Button::new("Сбросить настройки"),
                        )
                        .clicked()
                    {
                        let groups = state.draft.groups.clone();
                        state.draft = Settings { groups, ..Settings::default() };
                        state.semantic_exclude = state.draft.semantic.exclude.join("\n");
                        let (keymap, _) = Keymap::new(&BTreeMap::new());
                        state.keys = CommandId::ALL.iter().map(|&c| (c, keymap.text(c))).collect();
                        state.status =
                            Some(("значения по умолчанию — нажмите «Применить»".into(), false));
                    }
                });
            });
        egui::CentralPanel::default()
            .frame(
                egui::Frame::new()
                    .fill(theme::WINDOW_BACKGROUND)
                    .inner_margin(Margin::symmetric(20, 14)),
            )
            .show(ui, |ui| {
                ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
                    ui.set_max_width(ui.available_width() - 12.0);
                    page_title(ui, state.page.title());
                    match state.page {
                        Page::General => general(ui, &mut state.draft),
                        Page::Appearance => appearance(ui, &mut state.draft),
                        Page::Files => files(ui, &mut state.draft),
                        Page::Preview => preview(ui, &mut state.draft),
                        Page::Index => {
                            let info = IndexInfo { status: &index_status, elevated };
                            rescan |= index(ui, state, &info);
                        }
                        Page::Semantic => {
                            let info =
                                SemanticInfo { status: &semantic_status, dir: &dirs.semantic };
                            semantic_action = semantic(ui, state, &info);
                        }
                        Page::Tags => {
                            let info = TagsInfo { status: &tags_status, dir: &dirs.tags };
                            tags_action = tags(ui, &mut state.draft, &info);
                        }
                        Page::Sorting => {
                            sort_action =
                                sorting(ui, &mut state.draft, &mut state.categories, &sort_info)
                        }
                        Page::System => system_action = system(ui, &mut state.draft, &system_info),
                        Page::Keys => keys(ui, &mut state.keys),
                        Page::About => about_action = about(ui, &update_info),
                    }
                });
            });
    });
    if let Some((settings, categories)) = applied {
        app.apply_settings(ctx, settings);
        if let Some(config) = categories {
            app.settings_window.categories.saved(&config);
            app.sorter.set_config(config.clone());
            app.workers.save_categories(config, app.sorter.path.clone());
        }
    }
    if rescan {
        app.indexer.rescan();
    }
    match semantic_action {
        Some(SemanticAction::Rescan) => app.semantic.rescan(),
        Some(SemanticAction::OpenFile(path)) => {
            app.workers.shell(mh_files_fs::ShellJob::Open(path))
        }
        Some(SemanticAction::Open(path)) => {
            let _ = std::fs::create_dir_all(&path);
            app.actions.push(crate::app::Action::Open {
                location: mh_files_core::location::Location::Dir(path),
                target: crate::app::Target::NewTab,
            });
        }
        None => {}
    }
    match tags_action {
        Some(TagsAction::Forget) => {
            app.tagger.forget();
            app.settings_window.status = Some(("теги посчитаются заново".into(), false));
        }
        Some(TagsAction::OpenFile(path)) => app.workers.shell(mh_files_fs::ShellJob::Open(path)),
        Some(TagsAction::Open(path)) => {
            let _ = std::fs::create_dir_all(&path);
            app.actions.push(crate::app::Action::Open {
                location: mh_files_core::location::Location::Dir(path),
                target: crate::app::Target::NewTab,
            });
        }
        None => {}
    }
    match system_action {
        Some(SystemAction::Integration(feature, on)) => {
            app.integration = None;
            app.workers.integration(Some((feature, on)));
        }
        Some(SystemAction::Open(path)) => {
            let _ = std::fs::create_dir_all(&path);
            app.actions.push(crate::app::Action::Open {
                location: mh_files_core::location::Location::Dir(path),
                target: crate::app::Target::NewTab,
            });
        }
        Some(SystemAction::OpenFile(path)) => app.workers.shell(mh_files_fs::ShellJob::Open(path)),
        Some(SystemAction::Copy) => {
            ctx.copy_text(system_info.text.clone());
            app.settings_window.status = Some(("отчёт скопирован".into(), false));
        }
        None => {}
    }
    match about_action {
        Some(AboutAction::Check) => app.check_updates(),
        Some(AboutAction::Open(url)) => {
            app.workers.shell(mh_files_fs::ShellJob::Open(std::path::PathBuf::from(url)))
        }
        None => {}
    }
    match sort_action {
        Some(SortAction::Reload) => {
            app.sorter.reload();
            app.settings_window.categories =
                crate::categories_editor::Editor::new(&app.sorter.config);
            app.settings_window.status = Some(("категории перечитаны из файла".into(), false));
        }
        Some(SortAction::OpenFile) => {
            app.workers.shell(mh_files_fs::ShellJob::Open(sort_info.path.clone()));
        }
        Some(SortAction::OpenHistory) => {
            let _ = std::fs::create_dir_all(&sort_info.history);
            app.actions.push(crate::app::Action::Open {
                location: mh_files_core::location::Location::Dir(sort_info.history.clone()),
                target: crate::app::Target::NewTab,
            });
        }
        None => {}
    }
    if app.settings_window.open
        && (index_status.busy() || semantic_status.busy() || tags_status.busy())
    {
        ctx.request_repaint_after(std::time::Duration::from_millis(500));
    }
    if close {
        app.settings_window.open = false;
    }
}

fn nav_item(ui: &mut Ui, title: &str, selected: bool) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(vec2(ui.available_width(), 34.0), Sense::click());
    let painter = ui.painter();
    if selected {
        painter.rect_filled(rect, CornerRadius::same(4), theme::CARD);
        let bar = egui::Rect::from_min_size(rect.min, vec2(3.0, rect.height()));
        painter.rect_filled(bar, CornerRadius::same(1), theme::accent());
    } else if response.hovered() {
        painter.rect_filled(rect, CornerRadius::same(4), theme::CARD.gamma_multiply(0.6));
    }
    let color = if selected { theme::TEXT_PRIMARY } else { theme::TEXT_SECONDARY };
    painter.text(
        egui::pos2(rect.left() + 14.0, rect.center().y),
        egui::Align2::LEFT_CENTER,
        title,
        theme::regular(15.0),
        color,
    );
    ui.add_space(2.0);
    response
}

fn general(ui: &mut Ui, s: &mut Settings) {
    card(ui, "Запуск", |ui| {
        switch_row(
            ui,
            "Восстанавливать вкладки и панели",
            Some("После перезапуска открываются те же папки, вкладки и разбиение."),
            &mut s.panes.restore_session,
        );
    });
    card(ui, "Новая вкладка", |ui| {
        row(ui, "Открывается в", |ui| {
            let titles = [
                (NewTabLocation::Same, "той же папке"),
                (NewTabLocation::Home, "домашней папке"),
                (NewTabLocation::Computer, "«Этот компьютер»"),
            ];
            for (value, title) in titles {
                ui.radio_value(&mut s.panes.new_tab, value, title);
            }
        });
    });
    card(ui, "Терминал", |ui| {
        row(ui, "Команда", |ui| {
            ui.add(
                egui::TextEdit::singleline(&mut s.terminal)
                    .hint_text("пусто — Windows Terminal или PowerShell")
                    .desired_width(320.0),
            );
        });
        widgets::hint(
            ui,
            "{dir} заменяется папкой. Пример: wt.exe -d \"{dir}\" или cmd /k cd /d \"{dir}\".",
        );
    });
}

fn appearance(ui: &mut Ui, s: &mut Settings) {
    card(ui, "Масштаб и плотность", |ui| {
        row(ui, "Масштаб интерфейса", |ui| {
            ui.add(
                egui::Slider::new(&mut s.appearance.font_scale, MIN_FONT_SCALE..=MAX_FONT_SCALE)
                    .step_by(0.05)
                    .custom_formatter(|v, _| format!("{:.0}%", v * 100.0)),
            );
        });
        switch_row(
            ui,
            "Плотный список",
            Some("Строки ниже — больше файлов на экране."),
            &mut s.appearance.compact,
        );
        row(ui, "Размер плиток", |ui| {
            ui.add(
                egui::Slider::new(&mut s.appearance.grid_size, MIN_GRID..=MAX_GRID)
                    .step_by(8.0)
                    .suffix(" точек"),
            );
        });
        row(ui, "Размер строк", |ui| {
            ui.add(
                egui::Slider::new(&mut s.appearance.list_scale, MIN_LIST_SCALE..=MAX_LIST_SCALE)
                    .step_by(0.1)
                    .custom_formatter(|v, _| format!("{:.0}%", v * 100.0)),
            );
        });
    });
    card(ui, "Цвет и значки", |ui| {
        row(ui, "Акцент", |ui| {
            ui.color_edit_button_srgb(&mut s.appearance.accent);
            if s.appearance.accent != mh_files_core::settings::ACCENT
                && ui.button("Цвет MH").clicked()
            {
                s.appearance.accent = mh_files_core::settings::ACCENT;
            }
        });
        switch_row(
            ui,
            "Системные значки файлов",
            Some("Значки из Windows. Выключено — свои значки в стиле MH."),
            &mut s.appearance.system_icons,
        );
        switch_row(ui, "Анимации", None, &mut s.appearance.animations);
    });
    card(ui, "Давность дат", |ui| {
        switch_row(
            ui,
            "Кружок у даты изменения",
            Some(
                "В списке и Инспекторе: цвет от только что изменённых до изменённых год назад и раньше.",
            ),
            &mut s.appearance.age_dots,
        );
        ui.add_enabled_ui(s.appearance.age_dots, |ui| {
            row(ui, "Только что", |ui| {
                ui.color_edit_button_srgb(&mut s.appearance.age_new)
            });
            row(ui, "Давно", |ui| ui.color_edit_button_srgb(&mut s.appearance.age_old));
            row(ui, "Шкала", |ui| {
                let (rect, _) =
                    ui.allocate_exact_size(egui::vec2(220.0, 12.0), egui::Sense::hover());
                let painter = ui.painter();
                let steps = 44;
                let width = rect.width() / steps as f32;
                for i in 0..steps {
                    let t = i as f32 / (steps - 1) as f32;
                    let color = theme::age_color(s.appearance.age_new, s.appearance.age_old, t);
                    let x = rect.left() + i as f32 * width;
                    painter.rect_filled(
                        egui::Rect::from_min_size(
                            egui::pos2(x, rect.top()),
                            egui::vec2(width + 0.5, rect.height()),
                        ),
                        CornerRadius::ZERO,
                        color,
                    );
                }
                let defaults = (mh_files_core::settings::AGE_NEW, mh_files_core::settings::AGE_OLD);
                if (s.appearance.age_new, s.appearance.age_old) != defaults
                    && ui.button("По умолчанию").clicked()
                {
                    (s.appearance.age_new, s.appearance.age_old) = defaults;
                }
            });
        });
    });
    card(ui, "Окно", |ui| {
        switch_row(
            ui,
            "Свой заголовок окна",
            Some(
                "Заголовок в стиле MH со строкой поиска по дискам. Выключено — системная рамка Windows.",
            ),
            &mut s.appearance.custom_title_bar,
        );
    });
}

fn files(ui: &mut Ui, s: &mut Settings) {
    card(ui, "Показ", |ui| {
        switch_row(
            ui,
            "Скрытые файлы",
            Some("Ctrl+H переключает быстро."),
            &mut s.files.show_hidden,
        );
        switch_row(
            ui,
            "Системные файлы",
            Some("Скрытые файлы с атрибутом «системный»: pagefile.sys, desktop.ini…"),
            &mut s.files.show_system,
        );
        switch_row(ui, "Расширения имён", None, &mut s.files.show_extensions);
        switch_row(ui, "Папки первыми", None, &mut s.files.folders_first);
        switch_row(ui, "Даты «Сегодня», «Вчера»", None, &mut s.files.relative_dates);
    });
    card(ui, "Размеры папок", |ui| {
        switch_row(
            ui,
            "Размеры папок в списке сразу",
            Some(
                "Из индекса поиска по дискам — мгновенно и без обращения к диску. Где индекса \
                 нет — по Ctrl+Shift+S.",
            ),
            &mut s.files.auto_folder_sizes,
        );
    });
    card(ui, "Контекстное меню", |ui| {
        switch_row(
            ui,
            "Пункты меню Windows",
            Some(
                "7-Zip, Git, «Отправить», «Открыть с помощью» и другие пункты Проводника — \
                 под командами MH Files. Полное меню Windows — Shift+F10.",
            ),
            &mut s.files.windows_menu_items,
        );
    });
    card(ui, "Удаление", |ui| {
        switch_row(
            ui,
            "Спрашивать перед корзиной",
            Some("Удаление насовсем (Shift+Delete) спрашивает всегда."),
            &mut s.files.confirm_recycle,
        );
    });
}

fn preview(ui: &mut Ui, s: &mut Settings) {
    card(ui, "Эскизы", |ui| {
        switch_row(
            ui,
            "Эскизы картинок и видео",
            Some("Из кэша эскизов Windows; не тормозят переход по папкам."),
            &mut s.preview.thumbnails,
        );
    });
    card(ui, "Инспектор", |ui| {
        switch_row(
            ui,
            "Размер выбранной папки сразу",
            Some(
                "Из индекса поиска, иначе подсчётом на своих дисках. На сетевых дисках — по \
                 кнопке.",
            ),
            &mut s.preview.inspector_folder_size,
        );
    });
    card(ui, "Обработчики Windows", |ui| {
        switch_row(
            ui,
            "Документы через обработчики предпросмотра",
            Some(
                "Word, Excel, PowerPoint, Visio и другие — тем же, что показывает Проводник. Обработчик работает в отдельном процессе и не задерживает окно.",
            ),
            &mut s.preview.handlers,
        );
    });
    card(ui, "Видео и звук", |ui| {
        switch_row(
            ui,
            "Играть в быстром просмотре",
            Some(
                "Пробел на видео или музыке — сразу воспроизведение: Enter — пауза, Shift+стрелки — перемотка. Кодеки — те же, что у Windows (HEVC и AV1 ставятся из Microsoft Store).",
            ),
            &mut s.preview.media,
        );
    });
    card(ui, "Инспектор и быстрый просмотр", |ui| {
        row(ui, "Читать текста, КБ", |ui| {
            ui.add(egui::DragValue::new(&mut s.preview.text_limit_kb).range(4..=4096));
        });
        row(ui, "Картинки до, МБ", |ui| {
            ui.add(egui::DragValue::new(&mut s.preview.image_limit_mb).range(1..=1024));
        });
    });
}

struct SystemInfo {
    config: std::path::PathBuf,
    index: std::path::PathBuf,
    semantic: std::path::PathBuf,
    tags: std::path::PathBuf,
    portable: bool,
    integration: Option<mh_files_platform::integration::Status>,
    crashed: bool,
    report: Option<std::path::PathBuf>,
    first_frame: Option<std::time::Duration>,
    listings: Vec<(std::path::PathBuf, usize, std::time::Duration)>,
    text: String,
}

enum SystemAction {
    Integration(mh_files_platform::integration::Feature, bool),
    Open(std::path::PathBuf),
    OpenFile(std::path::PathBuf),
    Copy,
}

/// Запуск, Проводник, где данные, сбои и замеры.
fn system(ui: &mut Ui, s: &mut Settings, info: &SystemInfo) -> Option<SystemAction> {
    let mut action = None;
    card(ui, "Запуск", |ui| {
        switch_row(
            ui,
            "Одна копия программы",
            Some(
                "Повторный запуск (ярлык, «Открыть в MH Files», путь в командной строке) открывает папку в уже открытом окне. Действует со следующего запуска; отдельное окно — ключ --new-window.",
            ),
            &mut s.system.single_instance,
        );
    });
    card(ui, "Проводник", |ui| {
        use mh_files_platform::integration::Feature;
        let busy = info.integration.is_none();
        let status = info.integration.unwrap_or_default();
        ui.add_enabled_ui(!busy, |ui| {
            let mut rows = [
                (
                    Feature::ExplorerMenu,
                    "Пункт «Открыть в MH Files»",
                    "В меню папок, дисков и пустого места окна. В Windows 11 — в «Показать дополнительные параметры».",
                    status.explorer_menu,
                ),
                (
                    Feature::DefaultFolders,
                    "Открывать папки в MH Files",
                    "Двойной щелчок по папке или диску на рабочем столе и в других программах открывает MH Files вместо Проводника. Win+E, «Этот компьютер» и «Панель управления» остаются за Проводником; «Показать в Проводнике» по-прежнему открывает Проводник.",
                    status.default_folders,
                ),
                (
                    Feature::Archives,
                    "Архивы zip, 7z, rar",
                    "MH Files появится в «Открыть с помощью» для архивов: архив откроется как папка. Сделать его программой по умолчанию Windows разрешает только вам — «Открыть с помощью › Выбрать другое приложение › Всегда».",
                    status.archives,
                ),
            ];
            for (feature, label, help, on) in &mut rows {
                let before = *on;
                switch_row(ui, label, Some(help), on);
                if *on != before {
                    action = Some(SystemAction::Integration(*feature, *on));
                }
            }
        });
        let text = if busy {
            "Проверяется…"
        } else {
            "Только для текущего пользователя, права администратора не нужны. Удаление программы убирает всё это."
        };
        widgets::hint(ui, text);
    });
    card(ui, "Данные программы", |ui| {
        let mode = if info.portable {
            "Переносной режим: всё хранится рядом с программой (папка data)."
        } else {
            "Настройки — в профиле пользователя. Переносной режим: положите рядом с MH-Files.exe пустой файл portable."
        };
        widgets::hint(ui, mode);
        widgets::hint(
            ui,
            &format!("Настройки, сеанс, категории, журналы: {}", info.config.display()),
        );
        widgets::hint(ui, &format!("Индекс дисков: {}", info.index.display()));
        widgets::hint(ui, &format!("Поиск по смыслу: {}", info.semantic.display()));
        widgets::hint(ui, &format!("Автотеги: {}", info.tags.display()));
        if ui.button("Открыть папку данных").clicked() {
            action = Some(SystemAction::Open(info.config.clone()));
        }
    });
    card(ui, "Сбои", |ui| {
        let (text, color) = match (info.crashed, &info.report) {
            (true, Some(_)) => (
                "Прошлый запуск закончился сбоем программы. Вкладки восстановлены из сеанса.",
                theme::WARN,
            ),
            (true, None) => (
                "Прошлый запуск не закрылся как обычно (выключение компьютера, снятие процесса). Вкладки восстановлены из сеанса.",
                theme::TEXT_SECONDARY,
            ),
            (false, _) => ("Прошлый запуск закончился как обычно.", theme::TEXT_SECONDARY),
        };
        ui.label(RichText::new(text).color(color));
        ui.horizontal(|ui| {
            if let Some(report) = &info.report
                && ui.button("Открыть отчёт о сбое").clicked()
            {
                action = Some(SystemAction::OpenFile(report.clone()));
            }
            if ui.button("Папка отчётов").clicked() {
                action = Some(SystemAction::Open(crate::crash::crashes_dir(&info.config)));
            }
        });
        widgets::hint(
            ui,
            "Отчёт пишется при внутренней ошибке программы: версия, место, стек вызовов. Его можно приложить к сообщению об ошибке.",
        );
    });
    card(ui, "Замеры", |ui| {
        let first = info.first_frame.map_or("—".into(), |d| format!("{} мс", d.as_millis()));
        ui.label(format!("От запуска до первого кадра: {first}"));
        if info.listings.is_empty() {
            widgets::hint(
                ui,
                "Откройте несколько папок — здесь появится, сколько читалась каждая.",
            );
        }
        // Длинные пути обрезаются по ширине карточки, целиком — в подсказке.
        let cell = |ui: &mut egui::Ui, width: f32, text: RichText| {
            ui.allocate_ui_with_layout(
                egui::vec2(width, 18.0),
                egui::Layout::left_to_right(egui::Align::Center),
                |ui| {
                    ui.set_min_width(width);
                    ui.label(text);
                },
            );
        };
        for (dir, entries, took) in &info.listings {
            ui.horizontal(|ui| {
                let took = format!("{} мс", took.as_millis());
                cell(ui, 64.0, RichText::new(took).color(theme::TEXT_PRIMARY));
                let entries = mh_files_core::format::items(*entries);
                cell(ui, 96.0, RichText::new(entries).color(theme::TEXT_SECONDARY));
                let path = dir.display().to_string();
                ui.add(
                    egui::Label::new(RichText::new(&path).color(theme::TEXT_DISABLED)).truncate(),
                )
                .on_hover_text(path);
            });
        }
        widgets::hint(
            ui,
            "Для PLAN.md §10: откройте папку на уснувшем HDD, в сети и на 100 000 файлов, затем скопируйте отчёт.",
        );
        if ui.button("Скопировать отчёт").clicked() {
            action = Some(SystemAction::Copy);
        }
    });
    action
}

struct SortInfo {
    path: std::path::PathBuf,
    history: std::path::PathBuf,
    error: Option<String>,
    categories: usize,
    extensions: usize,
}

enum SortAction {
    Reload,
    OpenFile,
    OpenHistory,
}

/// Страница сортировщика: галочки по умолчанию и файл категорий.
fn sorting(
    ui: &mut Ui,
    s: &mut Settings,
    editor: &mut crate::categories_editor::Editor,
    info: &SortInfo,
) -> Option<SortAction> {
    let mut action = None;
    widgets::hint(
        ui,
        "Сортировщик из MH Sort раскладывает папку по категориям: Изображения, Видео, Документы… (Ctrl+Shift+O или «Разложить по папкам» в меню папки).",
    );
    ui.add_space(6.0);
    card(ui, "По умолчанию", |ui| {
        let o = &mut s.sorting;
        switch_row(
            ui,
            "Папки по типам внутри категорий",
            Some("Видео\\MP4, Видео\\MKV."),
            &mut o.type_folders,
        );
        switch_row(
            ui,
            "Узнавать тип по содержимому",
            Some("Для файлов без расширения или с незнакомым."),
            &mut o.detect_content,
        );
        switch_row(ui, "Пропускать скрытые и системные", None, &mut o.skip_hidden);
        switch_row(ui, "Пропускать уже разложенные папки", None, &mut o.skip_sorted);
        switch_row(
            ui,
            "Удалять опустевшие папки",
            Some("После перемещения с подпапками."),
            &mut o.remove_empty,
        );
        widgets::hint(ui, "Галочки, изменённые во вкладке сортировщика, запоминаются сами.");
    });
    card(ui, "По расписанию", |ui| {
        if s.sort_schedules.is_empty() {
            widgets::hint(
                ui,
                "Папки можно раскладывать сами по себе: «Разложить» › «Параметры» › «По расписанию». Работает, пока MH Files открыт; пропущенное — при следующем запуске.",
            );
        }
        let mut remove = None;
        for (index, schedule) in s.sort_schedules.iter().enumerate() {
            // Путь бывает длинным: обрезается по ширине карточки, целиком — в подсказке.
            let path = schedule.folder.display().to_string();
            ui.add(egui::Label::new(RichText::new(&path).color(theme::TEXT_PRIMARY)).truncate())
                .on_hover_text(path);
            ui.horizontal(|ui| {
                ui.label(RichText::new(schedule.period_label()).color(theme::TEXT_SECONDARY));
                if schedule.last_run > 0 {
                    let when = chrono::DateTime::from_timestamp(schedule.last_run, 0)
                        .map(|t| {
                            t.with_timezone(&chrono::Local).format("%d.%m.%Y %H:%M").to_string()
                        })
                        .unwrap_or_default();
                    ui.label(RichText::new(format!("отсчёт с {when}")).color(theme::TEXT_DISABLED));
                }
                if ui.small_button("Убрать").clicked() {
                    remove = Some(index);
                }
            });
            ui.add_space(4.0);
        }
        if let Some(index) = remove {
            s.sort_schedules.remove(index);
        }
    });
    crate::categories_editor::show(ui, editor, s.sorting.type_folders);
    card(ui, "Файл categories.json", |ui| {
        ui.label(format!("Категорий: {}, расширений: {}", info.categories, info.extensions));
        if let Some(error) = &info.error {
            ui.label(RichText::new(error).color(theme::CRITICAL));
            widgets::hint(
                ui,
                "Если сохранить категории отсюда, файл с ошибкой останется рядом как categories.json.broken.",
            );
        }
        widgets::hint(ui, &info.path.display().to_string());
        widgets::hint(
            ui,
            "Тот же формат, что у MH Sort: файл можно перенести. При первом запуске категории берутся у установленного MH Sort.",
        );
        ui.horizontal(|ui| {
            if ui.button("Открыть categories.json").clicked() {
                action = Some(SortAction::OpenFile);
            }
            if ui
                .button("Перечитать")
                .on_hover_text(
                    "Взять категории из файла, если его правили в редакторе; правка здесь пропадёт",
                )
                .clicked()
            {
                action = Some(SortAction::Reload);
            }
        });
    });
    card(ui, "Журналы", |ui| {
        widgets::hint(
            ui,
            "Каждая сортировка пишется в журнал — по нему работает отмена. Хранятся последние 100.",
        );
        if ui.button("Открыть папку журналов").clicked() {
            action = Some(SortAction::OpenHistory);
        }
    });
    action
}

struct IndexInfo<'a> {
    status: &'a mh_files_fs::IndexStatus,
    elevated: bool,
}

/// Страница индекса. `true` — нажали «Пересканировать».
fn index(ui: &mut Ui, state: &mut State, info: &IndexInfo) -> bool {
    let s = &mut state.draft;
    let mut rescan = false;
    card(ui, "Индекс", |ui| {
        switch_row(
            ui,
            "Поиск по дискам",
            Some(
                "Имена всех файлов в памяти: поиск за миллисекунды (Ctrl+E). Около 50 МБ на миллион файлов.",
            ),
            &mut s.index.enabled,
        );
        switch_row(
            ui,
            "Досматривать при запуске",
            Some(
                "Найти изменённое, пока программа была закрыта. С правами администратора — по журналу NTFS за секунды, без них — обходом в фоне.",
            ),
            &mut s.index.rescan_on_start,
        );
        row(ui, "Состояние", |ui| {
            let text = if info.status.volumes.is_empty() {
                "не запущен".to_string()
            } else {
                format!(
                    "{} · {}",
                    mh_files_core::format::items(info.status.entries()),
                    crate::pane_view::index_state(info.status)
                )
            };
            ui.label(RichText::new(text).color(theme::TEXT_SECONDARY));
            if ui.button("Пересканировать").clicked() {
                rescan = true;
            }
        });
        for volume in &info.status.volumes {
            let mut notes = Vec::new();
            notes.push(if volume.live {
                "изменения видны сразу"
            } else {
                "обновляется при просмотре папок"
            });
            if volume.journal {
                notes.push("журнал NTFS");
            }
            widgets::hint(
                ui,
                &format!(
                    "{} — {}, {}",
                    volume.root.display(),
                    mh_files_core::format::items(volume.entries),
                    notes.join(", ")
                ),
            );
        }
        if cfg!(windows) && !info.elevated {
            widgets::hint(
                ui,
                "Без прав администратора журнал NTFS недоступен: после запуска диск досматривается обходом в фоне. Поиск при этом работает сразу.",
            );
        }
    });
    card(ui, "Что индексировать", |ui| {
        widgets::hint(ui, "Пусто — все локальные диски, а по выбору ещё съёмные и сетевые.");
        path_list(ui, &mut s.index.roots, &mut state.new_root, "D:\\ или папка");
        ui.add_enabled_ui(s.index.roots.is_empty(), |ui| {
            switch_row(
                ui,
                "Съёмные диски",
                Some(
                    "Флешки и внешние диски — пока подключены. Отключённый диск пропадает из поиска, а его индекс остаётся до следующего раза.",
                ),
                &mut s.index.removable,
            );
            switch_row(
                ui,
                "Сетевые диски",
                Some(
                    "Подключённые сетевые диски с буквой. Первый обход по сети долгий; изменения видны, если их сообщает сервер, иначе — при просмотре папок.",
                ),
                &mut s.index.network,
            );
        });
    });
    card(ui, "Исключения", |ui| {
        widgets::hint(ui, "Эти папки пропускаются вместе со всем содержимым.");
        path_list(ui, &mut s.index.exclude, &mut state.new_exclude, "C:\\Windows\\WinSxS");
    });
    rescan
}

struct SemanticInfo<'a> {
    status: &'a mh_files_fs::SemanticStatus,
    dir: &'a std::path::Path,
}

struct TagsInfo<'a> {
    status: &'a mh_files_fs::TagStatus,
    dir: &'a std::path::Path,
}

enum TagsAction {
    Forget,
    Open(std::path::PathBuf),
    OpenFile(std::path::PathBuf),
}

fn tags(ui: &mut Ui, s: &mut Settings, info: &TagsInfo) -> Option<TagsAction> {
    use mh_files_core::tags::{MODELS, model_info};
    let t = &mut s.tags;
    let mut action = None;
    card(ui, "Автотеги", |ui| {
        widgets::hint(
            ui,
            "Модель WD Tagger v3 смотрит на картинки и кадры видео и подписывает, что на них: предметы, место, вид кадра, персонажей аниме и игр — словами Danbooru (около 10 800 тегов, по-английски). Теги видны в столбце «Теги» таблицы и в Инспекторе: сначала у видимых файлов, потом у остальных картинок открытой папки. Всё считается на этом компьютере: файлы никуда не отправляются.",
        );
        switch_row(
            ui,
            "Включить",
            Some(
                "При включении скачиваются модель с Hugging Face (0,4–1,3 ГБ) и ONNX Runtime с DirectML из NuGet (15 МБ). Нужны библиотеки Microsoft Visual C++ (x64) — обычно они уже стоят. Модель выгружается из памяти через пять минут без дела.",
            ),
            &mut t.enabled,
        );
        switch_row(
            ui,
            "Всю открытую папку",
            Some(
                "После видимых файлов — остальные картинки и видео папки, в фоне. Выключено — только то, что на экране.",
            ),
            &mut t.whole_folder,
        );
        switch_row(
            ui,
            "Видеокарта",
            Some(
                "Считать на видеокарте через DirectML — подходит любая с DirectX 12: NVIDIA, AMD, Intel. Обычно во много раз быстрее процессора. Не запустилась — модель сама переходит на процессор.",
            ),
            &mut t.gpu,
        );
        row(ui, "Состояние", |ui| {
            let status = info.status;
            let mut text = format!(
                "{} · с тегами {}",
                status.describe(),
                mh_files_core::format::items(status.tagged)
            );
            if status.loaded {
                text.push_str(if status.gpu {
                    " · модель на видеокарте"
                } else {
                    " · модель на процессоре"
                });
            }
            if let Some(last) = status.last {
                text.push_str(&format!(" · {:.1} с на файл", last.as_secs_f32()));
            }
            let color = match status.state {
                mh_files_fs::TagState::Failed(_) => theme::WARN,
                _ => theme::TEXT_SECONDARY,
            };
            ui.add(egui::Label::new(RichText::new(text).color(color)).truncate());
        });
        ui.horizontal(|ui| {
            if ui
                .button("Пересчитать все теги")
                .on_hover_text("Забыть посчитанное: файлы посмотрятся заново, когда их откроют")
                .clicked()
            {
                action = Some(TagsAction::Forget);
            }
            if ui.button("Открыть папку модели").clicked() {
                action = Some(TagsAction::Open(info.dir.to_path_buf()));
            }
            if ui
                .button("Открыть журнал")
                .on_hover_text("Вывод ONNX Runtime: почему модель не запустилась")
                .clicked()
            {
                action = Some(TagsAction::OpenFile(info.dir.join("tagger.log")));
            }
        });
        if let mh_files_fs::TagState::Failed(error) = &info.status.state {
            ui.add(egui::Label::new(RichText::new(error).color(theme::WARN)).wrap());
        }
        if info.status.gpu_fallback {
            widgets::hint(
                ui,
                "Видеокарта не подошла (нет DirectX 12 или драйвер отказал) — модель работает на процессоре. Причина — в журнале.",
            );
        }
        if info.status.skipped > 0 {
            widgets::hint(ui, &format!("Модель отказалась от файлов: {}.", info.status.skipped));
        }
    });
    card(ui, "Модель", |ui| {
        row(ui, "Модель", |ui| {
            let selected = model_info(&t.model)
                .map_or_else(|| t.model.clone(), |m| format!("{} ({})", m.title, m.size));
            egui::ComboBox::from_id_salt("tags-model")
                .selected_text(selected)
                .width(300.0)
                .show_ui(ui, |ui| {
                    for model in MODELS {
                        let label = format!("{} ({})", model.title, model.size);
                        if ui
                            .selectable_label(t.model.eq_ignore_ascii_case(model.repo), label)
                            .clicked()
                        {
                            t.model = model.repo.to_string();
                        }
                    }
                });
        });
        widgets::hint(
            ui,
            &format!(
                "SmilingWolf WD Tagger v3 с Hugging Face: {}. Другая модель — теги считаются заново, прежняя удаляется с диска.",
                t.model
            ),
        );
        row(ui, "Свой ONNX Runtime", |ui| {
            let mut text = t.runtime.display().to_string();
            let edit = ui.add(
                egui::TextEdit::singleline(&mut text)
                    .hint_text("пусто — скачанный MH Files")
                    .desired_width(300.0),
            );
            if edit.changed() {
                t.runtime = std::path::PathBuf::from(text.trim().trim_matches('"'));
            }
        });
        widgets::hint(ui, &format!("Файлы: {}", info.dir.display()));
    });
    card(ui, "Показ", |ui| {
        widgets::hint(
            ui,
            "Тег виден, если модель в нём уверена не меньше порога. Ниже — больше тегов, но и ошибок больше. Меняется без пересчёта.",
        );
        use mh_files_core::settings::TagSettings;
        row(ui, "Порог тегов", |ui| {
            ui.add(egui::Slider::new(&mut t.general, TagSettings::MIN_THRESHOLD..=95).suffix("%"));
            ui.label(RichText::new("по умолчанию 35%").color(theme::TEXT_SECONDARY));
        });
        row(ui, "Порог персонажей", |ui| {
            ui.add(egui::Slider::new(&mut t.character, 50..=99).suffix("%"));
            ui.label(RichText::new("по умолчанию 85%").color(theme::TEXT_SECONDARY));
        });
        switch_row(
            ui,
            "Рейтинг в Инспекторе",
            Some("Обычный, чувствительный, сомнительный или откровенный — как его видит модель."),
            &mut t.rating,
        );
        row(ui, "Файлы больше", |ui| {
            ui.add(egui::DragValue::new(&mut t.max_file_mb).range(1..=4096).suffix(" МБ"));
            ui.label(RichText::new("— без тегов").color(theme::TEXT_SECONDARY));
        });
    });
    action
}

enum SemanticAction {
    Rescan,
    Open(std::path::PathBuf),
    OpenFile(std::path::PathBuf),
}

/// Квантования модели в `unsloth/embeddinggemma-2-GGUF`: подпись и что это значит.
const QUANTS: [(&str, &str); 5] = [
    ("Q8_0", "Q8_0 — почти без потерь, 0,3 + 0,55 ГБ (по умолчанию)"),
    ("UD-Q6_K_XL", "UD-Q6_K_XL — чуть меньше"),
    ("UD-Q4_K_XL", "UD-Q4_K_XL — меньше всех, чуть хуже"),
    ("BF16", "BF16 — без квантования, 0,55 + 1 ГБ"),
    ("F16", "F16 — без квантования, 0,55 + 1 ГБ"),
];

/// Страница поиска по смыслу.
fn semantic(ui: &mut Ui, state: &mut State, info: &SemanticInfo) -> Option<SemanticAction> {
    let s = &mut state.draft.semantic;
    let mut action = None;
    card(ui, "Поиск по смыслу", |ui| {
        widgets::hint(
            ui,
            "Модель EmbeddingGemma 2 от Google понимает текст, картинки и звук: «кошка на диване» найдёт фотографию кошки, «договор аренды» — скан договора, хоть файл и называется IMG_0412. Всё считается на этом компьютере: файлы никуда не отправляются.",
        );
        switch_row(
            ui,
            "Включить",
            Some(
                "При включении скачивается около гигабайта: модель с Hugging Face и llama.cpp с GitHub. Модель выгружается из памяти через пять минут без поиска.",
            ),
            &mut s.enabled,
        );
        row(ui, "Состояние", |ui| {
            let status = info.status;
            let mut text = format!(
                "{} · {} · векторов {}",
                status.describe(),
                mh_files_core::format::items(status.files),
                mh_files_core::format::count(status.parts)
            );
            if status.loaded {
                text.push_str(" · модель в памяти");
            }
            let color = match status.state {
                mh_files_fs::SemanticState::Failed(_) => theme::WARN,
                _ => theme::TEXT_SECONDARY,
            };
            ui.add(egui::Label::new(RichText::new(text).color(color)).truncate());
        });
        ui.horizontal(|ui| {
            if ui.button("Пересканировать").clicked() {
                action = Some(SemanticAction::Rescan);
            }
            if ui.button("Открыть папку модели").clicked() {
                action = Some(SemanticAction::Open(info.dir.to_path_buf()));
            }
            if ui
                .button("Открыть журнал")
                .on_hover_text("Вывод llama-server: почему модель не запустилась")
                .clicked()
            {
                action = Some(SemanticAction::OpenFile(info.dir.join("llama-server.log")));
            }
        });
        if let Some(model) = &info.status.model {
            widgets::hint(ui, &format!("Модель: {model}"));
        }
        if info.status.gpu_fallback {
            widgets::hint(
                ui,
                "Видеокарта не подошла (нет Vulkan или драйвер отказал) — модель работает на процессоре.",
            );
        }
        if let mh_files_fs::SemanticState::Failed(error) = &info.status.state {
            ui.add(egui::Label::new(RichText::new(error).color(theme::WARN)).wrap());
        }
        if info.status.media_fallback {
            widgets::hint(
                ui,
                "Кодировщик картинок и звука (mmproj) не загрузился или не различает картинки — ищется по тексту и именам. Подробности — в журнале.",
            );
        }
        if info.status.skipped > 0 {
            widgets::hint(
                ui,
                &format!(
                    "Не прочитано файлов: {} — они ищутся только по имени.",
                    info.status.skipped
                ),
            );
        }
    });
    card(ui, "Что понимать", |ui| {
        widgets::hint(
            ui,
            "Имена файлов ищутся всегда — по словам. Изменение — индекс строится заново.",
        );
        switch_row(
            ui,
            "Картинки и кадры видео",
            Some("Что изображено: фото, скриншоты, рисунки; у видео — кадр, как в эскизе Windows."),
            &mut s.images,
        );
        switch_row(
            ui,
            "Документы",
            Some("Текст и код, Word, Excel, PowerPoint, OpenDocument и первые страницы PDF."),
            &mut s.documents,
        );
        switch_row(
            ui,
            "Звук",
            Some("Первые 30 секунд записей MP3 и WAV: речь, музыка, звуки."),
            &mut s.audio,
        );
        switch_row(
            ui,
            "Видеокарта",
            Some(
                "Считать на видеокарте через Vulkan. Не запустилась — сама переключится на процессор.",
            ),
            &mut s.gpu,
        );
        row(ui, "Файлы больше", |ui| {
            ui.add(egui::DragValue::new(&mut s.max_file_mb).range(1..=4096).suffix(" МБ"));
            ui.label(RichText::new("— только по имени").color(theme::TEXT_SECONDARY));
        });
    });
    card(ui, "Выдача", |ui| {
        widgets::hint(
            ui,
            "Файл попадает в выдачу, если запрос подходит ему заметно лучше, чем обычные запросы: так скриншоты, текстуры и картинки с надписями не лезут в любой поиск. Мягче — больше находок, строже — только явное. Имя файла сравнивается со словами запроса: файл, в имени которого есть хотя бы половина слов, виден всегда. Меняется без перестройки индекса.",
        );
        row(ui, "Отбор", |ui| {
            use mh_files_core::settings::SemanticSettings;
            let selected = SemanticSettings::STRICTNESS
                .iter()
                .find(|(value, _)| *value == s.strictness)
                .map_or_else(|| format!("{:.1}", s.min_rank()), |(_, label)| label.to_string());
            egui::ComboBox::from_id_salt("semantic-strictness")
                .selected_text(selected)
                .width(240.0)
                .show_ui(ui, |ui| {
                    for (value, label) in SemanticSettings::STRICTNESS {
                        ui.selectable_value(&mut s.strictness, value, label);
                    }
                });
        });
        row(ui, "Показывать не больше", |ui| {
            ui.add(egui::DragValue::new(&mut s.max_results).range(10..=5000).suffix(" файлов"));
        });
    });
    card(ui, "Папки", |ui| {
        widgets::hint(
            ui,
            "Пусто — Рабочий стол, Документы, Загрузки, Изображения, Видео и Музыка. Скрытые и системные файлы пропускаются.",
        );
        path_list(ui, &mut s.folders, &mut state.new_semantic, "D:\\Фото");
        ui.add_space(6.0);
        widgets::hint(
            ui,
            "Исключения — по одному на строку: имя (node_modules), маска (*.tmp) или полный путь папки.",
        );
        let edit = ui.add(
            egui::TextEdit::multiline(&mut state.semantic_exclude)
                .desired_rows(4)
                .desired_width(360.0),
        );
        if edit.changed() {
            s.exclude = state
                .semantic_exclude
                .lines()
                .map(str::trim)
                .filter(|line| !line.is_empty())
                .map(String::from)
                .collect();
        }
    });
    card(ui, "Модель", |ui| {
        row(ui, "Репозиторий", |ui| {
            ui.add(egui::TextEdit::singleline(&mut s.model_repo).desired_width(280.0));
        });
        row(ui, "Квантование", |ui| {
            let selected = QUANTS
                .iter()
                .find(|(q, _)| q.eq_ignore_ascii_case(&s.quant))
                .map_or(s.quant.clone(), |(_, label)| label.to_string());
            egui::ComboBox::from_id_salt("semantic-quant")
                .selected_text(selected)
                .width(280.0)
                .show_ui(ui, |ui| {
                    for (quant, label) in QUANTS {
                        if ui.selectable_label(s.quant.eq_ignore_ascii_case(quant), label).clicked()
                        {
                            s.quant = quant.to_string();
                        }
                    }
                });
        });
        row(ui, "Свой llama-server", |ui| {
            let mut text = s.server.display().to_string();
            let edit = ui.add(
                egui::TextEdit::singleline(&mut text)
                    .hint_text("пусто — скачанный MH Files")
                    .desired_width(280.0),
            );
            if edit.changed() {
                s.server = std::path::PathBuf::from(text.trim().trim_matches('"'));
            }
        });
        widgets::hint(
            ui,
            &format!(
                "По умолчанию — {} с Hugging Face и сборка llama.cpp с Vulkan с GitHub. Файлы: {}",
                mh_files_core::semantic::model::DEFAULT_REPO,
                info.dir.display()
            ),
        );
    });
    action
}

/// Список путей с кнопками «убрать» и полем «добавить».
fn path_list(ui: &mut Ui, paths: &mut Vec<std::path::PathBuf>, new: &mut String, hint: &str) {
    let mut remove = None;
    for (i, path) in paths.iter().enumerate() {
        ui.horizontal(|ui| {
            ui.label(path.display().to_string());
            if ui.small_button("Убрать").clicked() {
                remove = Some(i);
            }
        });
    }
    if let Some(i) = remove {
        paths.remove(i);
    }
    ui.horizontal(|ui| {
        let edit = ui.add(egui::TextEdit::singleline(new).hint_text(hint).desired_width(320.0));
        let enter = edit.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
        let valid = pending_path(new).is_some();
        if ui.add_enabled(valid, egui::Button::new("Добавить")).clicked() || enter {
            add_path(paths, new);
        }
    });
}

/// Путь из поля ввода: без пробелов по краям и кавычек «Копировать как путь» Проводника.
/// `None` — не полный путь.
fn pending_path(text: &str) -> Option<std::path::PathBuf> {
    let path = std::path::PathBuf::from(text.trim().trim_matches('"'));
    path.is_absolute().then(|| mh_files_core::location::normalize(&path))
}

/// Добавить набранный путь в список (без повторов) и очистить поле.
fn add_path(paths: &mut Vec<std::path::PathBuf>, text: &mut String) {
    if let Some(path) = pending_path(text) {
        if !paths.contains(&path) {
            paths.push(path);
        }
        text.clear();
    }
}

fn keys(ui: &mut Ui, keys: &mut BTreeMap<CommandId, String>) {
    widgets::hint(
        ui,
        "Несколько сочетаний — через запятую, два шага подряд — через пробел. Пусто — без сочетания. Пример: Ctrl+Shift+P, F1 или Alt+G D.",
    );
    ui.add_space(8.0);
    for &command in CommandId::ALL {
        let text = keys.entry(command).or_default();
        row(ui, command.name(), |ui| {
            ui.add(egui::TextEdit::singleline(text).desired_width(220.0));
            let defaults: Vec<String> = command
                .default_keys()
                .iter()
                .filter_map(|k| parse_chord(k).map(|c| format_chord(&c)))
                .collect();
            let default_text = defaults.join(", ");
            if *text != default_text
                && ui.button("По умолчанию").on_hover_text(default_text.clone()).clicked()
            {
                *text = default_text;
            }
        });
    }
}

enum AboutAction {
    Check,
    Open(String),
}

/// Версия, обновления, сторонние компоненты.
fn about(
    ui: &mut Ui,
    (checking, result): &(bool, Option<Result<mh_files_core::update::Check, String>>),
) -> Option<AboutAction> {
    use mh_files_core::update::Check;
    let mut action = None;
    card(ui, "MH Files", |ui| {
        ui.label(format!("Версия {}", env!("CARGO_PKG_VERSION")));
        ui.label(
            RichText::new("Файловый менеджер для Windows в стиле MH Monitoring.")
                .color(theme::TEXT_SECONDARY),
        );
        ui.add_space(6.0);
        ui.label(
            RichText::new(format!("Настройки: {}", storage::settings_path().display()))
                .color(theme::TEXT_DISABLED),
        );
        ui.label(
            RichText::new(format!("Сеанс: {}", storage::session_path().display()))
                .color(theme::TEXT_DISABLED),
        );
    });
    card(ui, "Обновления", |ui| {
        let (text, color) = match (checking, result) {
            (true, _) => ("Проверяю…".to_string(), theme::TEXT_DISABLED),
            (false, None) => (
                "Программа не проверяет обновления сама: только по этой кнопке.".to_string(),
                theme::TEXT_SECONDARY,
            ),
            (false, Some(Ok(Check::UpToDate))) => (
                format!("Установлена последняя версия — {}.", env!("CARGO_PKG_VERSION")),
                theme::TEXT_SECONDARY,
            ),
            (false, Some(Ok(Check::Available(release)))) => {
                let date =
                    release.published.as_deref().map(|d| format!(" от {d}")).unwrap_or_default();
                (format!("Доступна {}{date}.", release.name), theme::accent())
            }
            (false, Some(Err(error))) => {
                (format!("Не удалось проверить: {error}"), theme::CRITICAL)
            }
        };
        ui.label(RichText::new(text).color(color));
        ui.horizontal(|ui| {
            if ui.add_enabled(!checking, egui::Button::new("Проверить обновления")).clicked()
            {
                action = Some(AboutAction::Check);
            }
            if let Some(Ok(Check::Available(release))) = result
                && ui.button("Открыть страницу выпуска").clicked()
            {
                action = Some(AboutAction::Open(release.url.clone()));
            }
        });
        widgets::hint(
            ui,
            "Один запрос к GitHub Releases. Пред-выпуски (rc) предлагаются, только если установлен пред-выпуск. Установщик новой версии сохраняет настройки и вкладки.",
        );
    });
    card(ui, "Сторонние компоненты", |ui| {
        ui.label(
            RichText::new(
                "Шрифт Cuprum — SIL Open Font License 1.1. Интерфейс — egui (MIT/Apache-2.0). Нечёткий поиск — nucleo-matcher (MPL-2.0). Архивы — zip (MIT) и sevenz-rust2 (Apache-2.0), хэш дубликатов — BLAKE3 (Apache-2.0). Сортировщик — из MH Sort (MIT): значки Phosphor (MIT), walkdir и infer (MIT).",
            )
            .color(theme::TEXT_SECONDARY),
        );
    });
    let _ = Color32::TRANSPARENT;
    action
}
