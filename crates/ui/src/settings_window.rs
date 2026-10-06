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
    Keys,
    About,
}

impl Page {
    const ALL: [Page; 6] =
        [Page::General, Page::Appearance, Page::Files, Page::Preview, Page::Keys, Page::About];

    fn title(self) -> &'static str {
        match self {
            Page::General => "Общие",
            Page::Appearance => "Внешний вид",
            Page::Files => "Файлы и папки",
            Page::Preview => "Предпросмотр",
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
}

impl State {
    pub fn open(&mut self, settings: &Settings) {
        if !self.open {
            self.draft = settings.clone();
            let (keymap, _) = Keymap::new(&settings.keys);
            self.keys = CommandId::ALL.iter().map(|&c| (c, keymap.text(c))).collect();
            self.status = None;
        }
        self.open = true;
        self.focus = true;
    }

    /// Черновик с разобранными сочетаниями. Ошибка — текст.
    fn result(&self) -> Result<Settings, String> {
        let mut settings = self.draft.clone().sanitized();
        let mut keys = BTreeMap::new();
        let mut seen: BTreeMap<String, CommandId> = BTreeMap::new();
        for (&command, text) in &self.keys {
            let mut parsed = Vec::new();
            for part in text.split(',').map(str::trim).filter(|p| !p.is_empty()) {
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
    let mut applied: Option<Settings> = None;
    let mut close = false;
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
                        Page::Keys => keys(ui, &mut state.keys),
                        Page::About => about(ui),
                    }
                });
            });
    });
    if let Some(settings) = applied {
        app.apply_settings(ctx, settings);
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
    card(ui, "Инспектор и быстрый просмотр", |ui| {
        row(ui, "Читать текста, КБ", |ui| {
            ui.add(egui::DragValue::new(&mut s.preview.text_limit_kb).range(4..=4096));
        });
        row(ui, "Картинки до, МБ", |ui| {
            ui.add(egui::DragValue::new(&mut s.preview.image_limit_mb).range(1..=1024));
        });
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

fn about(ui: &mut Ui) {
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
    card(ui, "Сторонние компоненты", |ui| {
        ui.label(
            RichText::new(
                "Шрифт Cuprum — SIL Open Font License 1.1. Интерфейс — egui (MIT/Apache-2.0). Нечёткий поиск — nucleo-matcher (MPL-2.0).",
            )
            .color(theme::TEXT_SECONDARY),
        );
    });
    let _ = Color32::TRANSPARENT;
}
