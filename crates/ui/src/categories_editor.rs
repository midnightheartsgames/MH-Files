//! Правка категорий сортировщика в «Настройки → Сортировка»: список категорий (имя,
//! расширения, порядок), особые папки, проверка имени файла. Черновик живёт в окне настроек
//! и применяется вместе с остальным кнопкой «Применить» (модель — `core::sorting::edit`).

use eframe::egui::{self, Align, CornerRadius, Layout, RichText, Sense, Ui, vec2};
use mh_files_core::sorting::edit::{Check, Draft, parse_extensions};
use mh_files_core::sorting::{Classifier, Config};

use crate::sorter::{look_for, paint_chip};
use crate::widgets::{card, row};
use crate::{icons, theme, widgets};

/// Состояние редактора в окне настроек.
#[derive(Default)]
pub struct Editor {
    pub draft: Draft,
    /// Какие категории были при открытии — чтобы понять, есть ли изменения.
    original: Config,
    /// Раскрытая категория.
    open: Option<usize>,
    /// Поле «Проверить имя файла».
    test_name: String,
}

impl Editor {
    pub fn new(config: &Config) -> Editor {
        Editor { draft: Draft::from_config(config), original: config.clone(), ..Editor::default() }
    }

    /// Изменённые категории для сохранения: `Ok(None)` — менять нечего, `Err` — ошибка
    /// в черновике (первая).
    pub fn result(&self) -> Result<Option<Config>, String> {
        if !self.draft.differs_from(&self.original) {
            return Ok(None);
        }
        match self.draft.check().errors.into_iter().next() {
            Some(error) => Err(format!("Категории: {error}")),
            None => Ok(Some(self.draft.to_config())),
        }
    }

    /// Сохранено: теперь это исходное состояние.
    pub fn saved(&mut self, config: &Config) {
        self.original = config.clone();
        self.draft = Draft::from_config(config);
    }

    pub fn changed(&self) -> bool {
        self.draft.differs_from(&self.original)
    }
}

/// Карточки редактора. `type_folders` — галочка «Папки по типам» для проверки имени.
pub fn show(ui: &mut Ui, editor: &mut Editor, type_folders: bool) {
    let check = editor.draft.check();
    card(ui, "Категории", |ui| {
        widgets::hint(
            ui,
            "Порядок важен: если расширение есть в двух категориях, файл попадёт в верхнюю. Переименование не трогает уже созданные папки.",
        );
        ui.add_space(4.0);
        list(ui, editor);
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            if ui.button("Добавить категорию").clicked() {
                editor.open = Some(editor.draft.add());
            }
            if ui
                .button("Стандартные категории")
                .on_hover_text(
                    "Вернуть категории MH Sort по умолчанию — до «Применить» можно отменить",
                )
                .clicked()
            {
                editor.draft = Draft::from_config(&Config::default());
                editor.open = None;
            }
            if editor.changed() && ui.button("Отменить правку").clicked() {
                editor.draft = Draft::from_config(&editor.original);
                editor.open = None;
            }
        });
        problems(ui, &check);
        if editor.changed() {
            ui.label(
                RichText::new("Есть изменения — сохранятся кнопкой «Применить» или «ОК».")
                    .color(theme::accent()),
            );
        }
    });
    card(ui, "Особые папки", |ui| {
        let draft = &mut editor.draft;
        row(ui, "Неопознанные файлы", |ui| {
            ui.add(egui::TextEdit::singleline(&mut draft.unknown_category).desired_width(220.0));
        });
        row(ui, "Файлы без расширения", |ui| {
            ui.add(egui::TextEdit::singleline(&mut draft.no_extension_folder).desired_width(220.0))
                .on_hover_text("Папка типа внутри неопознанных");
        });
        row(ui, "Не трогать", |ui| {
            ui.add(
                egui::TextEdit::singleline(&mut draft.ignore_extensions)
                    .desired_width(ui.available_width()),
            )
            .on_hover_text("Недокачанные и временные файлы: crdownload, part, tmp…");
        });
    });
    card(ui, "Правила по дате и размеру", |ui| {
        widgets::hint(
            ui,
            "Проверяются раньше категорий: подошедший файл ляжет в «Папка\\Категория». Пустое поле — без этого условия; категории через запятую, пусто — все.",
        );
        rules(ui, &mut editor.draft);
        if ui.button("Добавить правило").clicked() {
            editor.draft.add_rule();
        }
    });
    card(ui, "Проверить имя файла", |ui| {
        ui.add(
            egui::TextEdit::singleline(&mut editor.test_name)
                .hint_text("например, отпуск.tar.gz")
                .desired_width(320.0),
        );
        let name = editor.test_name.trim();
        if name.is_empty() {
            widgets::hint(
                ui,
                "Покажет, куда попадёт файл с такими категориями (без чтения содержимого).",
            );
        } else if check.is_ok() {
            let classifier = Classifier::new(&editor.draft.to_config());
            let text = if classifier.is_ignored(name) {
                "не трогается — в списке «Не трогать»".to_string()
            } else {
                let class = classifier.classify(name, || None);
                if type_folders {
                    format!("в папку «{}\\{}»", class.category, class.type_folder)
                } else {
                    format!("в папку «{}»", class.category)
                }
            };
            ui.label(RichText::new(text).color(theme::TEXT_PRIMARY));
        } else {
            widgets::hint(ui, "Сначала исправьте ошибки в категориях.");
        }
    });
}

/// Список категорий: строка — значок, имя, расширения; щелчок раскрывает правку.
fn list(ui: &mut Ui, editor: &mut Editor) {
    let mut toggle = None;
    let mut moved = None;
    let mut remove = None;
    let count = editor.draft.categories.len();
    for index in 0..count {
        let open = editor.open == Some(index);
        let category = &mut editor.draft.categories[index];
        let exts = parse_extensions(&category.extensions);
        let look = look_for(category.name.trim(), &exts);
        let (rect, response) =
            ui.allocate_exact_size(vec2(ui.available_width(), 30.0), Sense::click());
        if ui.is_rect_visible(rect) {
            let painter = ui.painter();
            if open || response.hovered() {
                painter.rect_filled(rect, CornerRadius::same(4), theme::FIELD);
            }
            let chip = egui::Rect::from_min_size(rect.min + vec2(4.0, 4.0), vec2(22.0, 22.0));
            paint_chip(ui, chip, Some(look));
        }
        // Имя, расширения и кнопки — поверх строки.
        let mut content = ui.new_child(
            egui::UiBuilder::new()
                .max_rect(rect.with_min_x(rect.left() + 34.0).shrink2(vec2(0.0, 2.0)))
                .layout(Layout::left_to_right(Align::Center)),
        );
        let name = if category.name.trim().is_empty() {
            "без имени"
        } else {
            category.name.trim()
        };
        cell(&mut content, 150.0, RichText::new(name).color(theme::TEXT_PRIMARY));
        let buttons = 3.0 * 26.0 + 8.0;
        let preview = match exts.len() {
            0 => "нет расширений".to_string(),
            n => format!("{n} · {}", exts.join(" ")),
        };
        let width = (content.available_width() - buttons).max(40.0);
        cell(&mut content, width, RichText::new(preview).color(theme::TEXT_DISABLED));
        content.with_layout(Layout::right_to_left(Align::Center), |ui| {
            if widgets::icon_button(ui, true, "Удалить категорию", icons::close).clicked()
            {
                remove = Some(index);
            }
            if widgets::icon_button(ui, index + 1 < count, "Ниже", icons::arrow_down).clicked()
            {
                moved = Some((index, false));
            }
            if widgets::icon_button(ui, index > 0, "Выше", icons::arrow_up).clicked() {
                moved = Some((index, true));
            }
        });
        if response.clicked() {
            toggle = Some(index);
        }
        if open {
            egui::Frame::new()
                .fill(theme::FIELD)
                .corner_radius(CornerRadius::same(4))
                .inner_margin(egui::Margin { left: 34, right: 10, top: 2, bottom: 10 })
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.horizontal(|ui| {
                        ui.label(RichText::new("Имя папки").color(theme::TEXT_SECONDARY));
                        ui.add(egui::TextEdit::singleline(&mut category.name).desired_width(240.0));
                    });
                    ui.label(RichText::new("Расширения").color(theme::TEXT_SECONDARY));
                    ui.add(
                        egui::TextEdit::multiline(&mut category.extensions)
                            .desired_rows(2)
                            .desired_width(ui.available_width()),
                    );
                    widgets::hint(
                        ui,
                        "Через запятую или пробел: jpg, png, tar.gz. Точка и *. не нужны.",
                    );
                });
        }
    }
    if let Some(index) = toggle {
        editor.open = if editor.open == Some(index) { None } else { Some(index) };
    }
    if let Some((index, up)) = moved {
        let to = editor.draft.move_by(index, up);
        if editor.open == Some(index) {
            editor.open = Some(to);
        } else if editor.open == Some(to) {
            editor.open = Some(index);
        }
    }
    if let Some(index) = remove {
        editor.draft.remove(index);
        editor.open = match editor.open {
            Some(open) if open == index => None,
            Some(open) if open > index => Some(open - 1),
            other => other,
        };
    }
}

/// Правила: папка, «старше, дней», «больше, МБ», категории, удалить.
fn rules(ui: &mut Ui, draft: &mut Draft) {
    let mut remove = None;
    for (index, rule) in draft.rules.iter_mut().enumerate() {
        egui::Frame::new()
            .fill(theme::FIELD)
            .corner_radius(CornerRadius::same(4))
            .inner_margin(egui::Margin::symmetric(10, 6))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal(|ui| {
                    ui.label(RichText::new("В папку").color(theme::TEXT_SECONDARY));
                    ui.add(egui::TextEdit::singleline(&mut rule.folder).desired_width(130.0));
                    ui.label(RichText::new("старше, дней").color(theme::TEXT_SECONDARY));
                    ui.add(
                        egui::TextEdit::singleline(&mut rule.older_than_days)
                            .hint_text("—")
                            .desired_width(50.0),
                    );
                    ui.label(RichText::new("больше, МБ").color(theme::TEXT_SECONDARY));
                    ui.add(
                        egui::TextEdit::singleline(&mut rule.larger_than_mb)
                            .hint_text("—")
                            .desired_width(60.0),
                    );
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if widgets::icon_button(ui, true, "Удалить правило", icons::close).clicked()
                        {
                            remove = Some(index);
                        }
                    });
                });
                ui.horizontal(|ui| {
                    ui.label(RichText::new("Категории").color(theme::TEXT_SECONDARY));
                    ui.add(
                        egui::TextEdit::singleline(&mut rule.categories)
                            .hint_text("все")
                            .desired_width(ui.available_width()),
                    );
                });
            });
        ui.add_space(4.0);
    }
    if let Some(index) = remove {
        draft.rules.remove(index);
    }
}

/// Подпись по левому краю в колонке заданной ширины; длинная обрезается многоточием.
fn cell(ui: &mut Ui, width: f32, text: RichText) {
    ui.allocate_ui_with_layout(vec2(width, 26.0), Layout::left_to_right(Align::Center), |ui| {
        ui.set_min_width(width);
        ui.add(egui::Label::new(text).truncate().selectable(false));
    });
}

fn problems(ui: &mut Ui, check: &Check) {
    for error in &check.errors {
        ui.label(RichText::new(error).color(theme::CRITICAL));
    }
    for warning in &check.warnings {
        ui.label(RichText::new(warning).color(theme::WARN));
    }
}
