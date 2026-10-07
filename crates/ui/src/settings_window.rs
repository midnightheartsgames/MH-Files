//! Окно настроек — как у MH Monitoring: разделы слева, карточки в середине, внизу «Применить»
//! и «Отмена». Всё меняется в черновике; рабочие настройки — только кнопкой.

use std::collections::BTreeMap;

use eframe::egui::{
    self, Align, Color32, CornerRadius, Layout, Margin, RichText, ScrollArea, Sense, Ui,
    ViewportBuilder, ViewportClass, ViewportCommand, ViewportId, vec2,
};
use mh_files_core::settings::{
    MAX_FONT_SCALE, MAX_GRID, MIN_FONT_SCALE, MIN_GRID, NewTabLocation, Settings,
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
    Sorting,
    System,
    Keys,
    About,
}

impl Page {
    const ALL: [Page; 9] = [
        Page::General,
        Page::Appearance,
        Page::Files,
        Page::Preview,
        Page::Index,
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
}

impl State {
    pub fn open(&mut self, settings: &Settings, categories: &mh_files_core::sorting::Config) {
        if !self.open {
            self.draft = settings.clone();
            self.categories = crate::categories_editor::Editor::new(categories);
            let (keymap, _) = Keymap::new(&settings.keys);
            self.keys = CommandId::ALL.iter().map(|&c| (c, keymap.text(c))).collect();
            self.status = None;
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
        .with_inner_size([900.0, 620.0])
        .with_min_inner_size([760.0, 480.0]);
    let mut applied: Option<(Settings, Option<mh_files_core::sorting::Config>)> = None;
    let mut close = false;
    let index_status = app.indexer.status();
    let elevated = app.indexer.elevated();
    let mut rescan = false;
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
    if app.settings_window.open && index_status.busy() {
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
                    "MH Files появится в «Открыть с помощью» для архивов: архив откроется как папка. Сделать его программой по умолчанию Windows разрешает только вам — «Открыть с помощью → Выбрать другое приложение → Всегда».",
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
        ui.add(egui::TextEdit::singleline(new).hint_text(hint).desired_width(320.0));
        let path = std::path::PathBuf::from(new.trim());
        if ui.add_enabled(path.is_absolute(), egui::Button::new("Добавить")).clicked() {
            let path = mh_files_core::location::normalize(&path);
            if !paths.contains(&path) {
                paths.push(path);
            }
            new.clear();
        }
    });
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
