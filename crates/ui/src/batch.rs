//! Пакетное переименование: правила слева, предпросмотр «было → стало» справа. Применить
//! можно, только когда ни в одной строке нет ошибки.

use std::collections::HashMap;
use std::path::PathBuf;

use eframe::egui::{
    self, Align, Color32, CornerRadius, DragValue, Id, Key, Layout, RichText, ScrollArea, Stroke,
    Ui, vec2,
};
use mh_files_core::rename::{
    self, CaseMode, ExtensionRule, InsertAt, Item, Numbering, Rule, Status,
};
use mh_files_fs::Ticket;

use crate::app::{FilesApp, OWNER_BATCH};
use crate::tabs::Tab;
use crate::{icons, theme, widgets};

pub struct State {
    items: Vec<Item>,
    siblings: HashMap<PathBuf, Vec<String>>,
    rules: Vec<Rule>,
    numbering: Numbering,
    names: Result<Vec<String>, String>,
    statuses: Vec<Status>,
    dirty: bool,
}

/// Имена всех объектов в папках вкладки — для проверки столкновений.
pub fn siblings(tab: &Tab) -> HashMap<PathBuf, Vec<String>> {
    let mut map: HashMap<PathBuf, Vec<String>> = HashMap::new();
    for entry in tab.listing.all() {
        map.entry(entry.parent.to_path_buf()).or_default().push(entry.name.clone());
    }
    map
}

impl State {
    pub fn new(items: Vec<Item>, siblings: HashMap<PathBuf, Vec<String>>) -> State {
        State {
            items,
            siblings,
            rules: vec![Rule::Replace {
                find: String::new(),
                replace: String::new(),
                regex: false,
                case_sensitive: false,
            }],
            numbering: Numbering::default(),
            names: Ok(Vec::new()),
            statuses: Vec::new(),
            dirty: true,
        }
    }

    fn recompute(&mut self) {
        self.names = rename::preview(&self.items, &self.rules, self.numbering);
        self.statuses = match &self.names {
            Ok(names) => rename::validate(&self.items, names, &self.siblings),
            Err(_) => Vec::new(),
        };
        self.dirty = false;
    }
}

fn new_rule(kind: usize) -> Rule {
    match kind {
        0 => Rule::Replace {
            find: String::new(),
            replace: String::new(),
            regex: false,
            case_sensitive: false,
        },
        1 => Rule::Replace {
            find: String::new(),
            replace: String::new(),
            regex: true,
            case_sensitive: false,
        },
        2 => Rule::Insert { text: String::new(), at: InsertAt::Prefix },
        3 => Rule::Insert { text: String::new(), at: InsertAt::Suffix },
        4 => Rule::Template { pattern: "{name}_{n:3}".into() },
        5 => Rule::Case { mode: CaseMode::Lower },
        6 => Rule::Extension { rule: ExtensionRule::Lower },
        7 => Rule::RemoveRange { from: 0, count: 1, from_end: false },
        _ => Rule::Trim,
    }
}

const RULE_KINDS: [&str; 9] = [
    "Найти и заменить",
    "Регулярное выражение",
    "Префикс",
    "Суффикс",
    "Шаблон имени",
    "Регистр",
    "Расширение",
    "Удалить символы",
    "Убрать лишние пробелы",
];

pub fn show(ctx: &egui::Context, app: &mut FilesApp) {
    let Some(state) = &mut app.batch else { return };
    if state.dirty {
        state.recompute();
    }
    let screen = ctx.content_rect();
    let size = vec2((screen.width() - 60.0).min(1040.0), (screen.height() - 80.0).min(660.0));
    let mut close = false;
    let mut apply = false;
    let modal = egui::Modal::new(Id::new("batch-rename")).show(ctx, |ui| {
        ui.set_width(size.x);
        ui.set_height(size.y);
        ui.label(
            RichText::new(format!(
                "Пакетное переименование · {}",
                mh_files_core::format::items(state.items.len())
            ))
            .font(theme::bold(20.0)),
        );
        ui.add_space(8.0);
        let footer = 46.0;
        let body = (ui.available_height() - footer).max(120.0);
        ui.horizontal_top(|ui| {
            let column = Layout::top_down(Align::Min);
            ui.allocate_ui_with_layout(vec2(340.0, body), column, |ui| {
                ScrollArea::vertical().id_salt("batch-rules").auto_shrink([false, false]).show(
                    ui,
                    |ui| {
                        ui.set_width(326.0);
                        rules_ui(ui, state);
                    },
                );
            });
            ui.add_space(8.0);
            ui.allocate_ui_with_layout(vec2(ui.available_width(), body), column, |ui| {
                preview_table(ui, state)
            });
        });
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            let errors = state.statuses.iter().filter(|s| s.is_error()).count();
            let changed = state.statuses.iter().filter(|s| !matches!(s, Status::Unchanged)).count();
            let summary = match &state.names {
                Err(error) => RichText::new(error).color(theme::CRITICAL),
                Ok(_) if errors > 0 => {
                    RichText::new(format!("ошибок: {errors} — исправьте правила"))
                        .color(theme::CRITICAL)
                }
                Ok(_) => {
                    RichText::new(format!("изменится: {changed}")).color(theme::TEXT_SECONDARY)
                }
            };
            ui.label(summary);
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                let can_apply = state.names.is_ok() && errors == 0 && changed > 0;
                if ui
                    .add_enabled_ui(can_apply, |ui| widgets::button(ui, "Переименовать", true))
                    .inner
                    .clicked()
                {
                    apply = true;
                }
                if widgets::button(ui, "Отмена", false).clicked() {
                    close = true;
                }
            });
        });
    });
    if modal.should_close() || ctx.input(|i| i.key_pressed(Key::Escape)) {
        close = true;
    }
    if apply {
        let state = app.batch.take().expect("окно открыто");
        if let Ok(names) = &state.names {
            match rename::plan(&state.items, names, &state.statuses) {
                Ok(plan) if !plan.is_empty() => {
                    app.workers.batch_rename(Ticket { owner: OWNER_BATCH, generation: 0 }, plan);
                }
                Ok(_) => {}
                Err(error) => app.set_status(error, crate::app::Level::Error),
            }
        }
    } else if close {
        app.batch = None;
    }
}

fn rules_ui(ui: &mut Ui, state: &mut State) {
    let mut remove = None;
    let mut swap = None;
    let count = state.rules.len();
    for (index, rule) in state.rules.iter_mut().enumerate() {
        egui::Frame::new()
            .fill(theme::CARD)
            .stroke(Stroke::new(1.0, theme::CARD_STROKE))
            .corner_radius(CornerRadius::same(6))
            .inner_margin(egui::Margin::same(10))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal(|ui| {
                    ui.label(
                        RichText::new(format!("{}. {}", index + 1, rule.title()))
                            .font(theme::bold(14.0)),
                    );
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if widgets::icon_button(ui, true, "Убрать правило", icons::close).clicked()
                        {
                            remove = Some(index);
                        }
                        if widgets::icon_button(ui, index + 1 < count, "Ниже", icons::chevron_down)
                            .clicked()
                        {
                            swap = Some((index, index + 1));
                        }
                        if widgets::icon_button(ui, index > 0, "Выше", icons::arrow_up).clicked()
                        {
                            swap = Some((index, index - 1));
                        }
                    });
                });
                if rule_fields(ui, rule, index) {
                    state.dirty = true;
                }
            });
        ui.add_space(6.0);
    }
    if let Some(index) = remove {
        state.rules.remove(index);
        state.dirty = true;
    }
    if let Some((a, b)) = swap {
        state.rules.swap(a, b);
        state.dirty = true;
    }
    ui.menu_button("+ Добавить правило", |ui| {
        for (kind, title) in RULE_KINDS.iter().enumerate() {
            if ui.button(*title).clicked() {
                state.rules.push(new_rule(kind));
                state.dirty = true;
                ui.close();
            }
        }
    });
    ui.add_space(10.0);
    ui.label(RichText::new("Счётчик {n}").font(theme::bold(14.0)));
    ui.horizontal(|ui| {
        let n = &mut state.numbering;
        let mut changed = false;
        ui.label("с");
        changed |= ui.add(DragValue::new(&mut n.start).range(0..=1_000_000)).changed();
        ui.label("шаг");
        changed |= ui.add(DragValue::new(&mut n.step).range(1..=1000)).changed();
        ui.label("цифр");
        changed |= ui.add(DragValue::new(&mut n.width).range(1..=12)).changed();
        if changed {
            state.dirty = true;
        }
    });
    ui.add_space(8.0);
    widgets::hint(
        ui,
        "Переменные: {name} — имя, {ext} — расширение, {n} или {n:3} — номер, {date} — сегодня, \
         {modified} и {created} — даты файла, {parent} — папка, {uuid}.",
    );
}

/// Поля одного правила. `true` — что-то поменялось.
fn rule_fields(ui: &mut Ui, rule: &mut Rule, index: usize) -> bool {
    let mut changed = false;
    let text = |ui: &mut Ui, value: &mut String, hint: &str| {
        ui.add(egui::TextEdit::singleline(value).hint_text(hint).desired_width(f32::INFINITY))
            .changed()
    };
    match rule {
        Rule::Replace { find, replace, regex, case_sensitive } => {
            changed |= text(ui, find, if *regex { "Регулярное выражение" } else { "Найти" });
            changed |= text(
                ui,
                replace,
                if *regex { "Замена ($1 — группа)" } else { "Заменить на" },
            );
            ui.horizontal(|ui| {
                changed |= ui.checkbox(regex, "регулярное").changed();
                changed |= ui.checkbox(case_sensitive, "с учётом регистра").changed();
            });
        }
        Rule::Insert { text: value, at } => {
            changed |= text(ui, value, "Текст, можно с {n}, {date}…");
            ui.horizontal(|ui| {
                changed |= ui.radio_value(at, InsertAt::Prefix, "в начало").changed();
                changed |= ui.radio_value(at, InsertAt::Suffix, "в конец").changed();
            });
        }
        Rule::Template { pattern } => changed |= text(ui, pattern, "Например trip_{n:3}"),
        Rule::Case { mode } => {
            egui::ComboBox::from_id_salt(("case", index)).selected_text(case_title(*mode)).show_ui(
                ui,
                |ui| {
                    for option in
                        [CaseMode::Lower, CaseMode::Upper, CaseMode::Title, CaseMode::Sentence]
                    {
                        changed |= ui.selectable_value(mode, option, case_title(option)).changed();
                    }
                },
            );
        }
        Rule::Extension { rule } => {
            let mut kind = match rule {
                ExtensionRule::Lower => 0,
                ExtensionRule::Upper => 1,
                ExtensionRule::Set(_) => 2,
                ExtensionRule::Remove => 3,
            };
            let titles = ["строчными", "ПРОПИСНЫМИ", "заменить на…", "убрать"];
            egui::ComboBox::from_id_salt(("ext", index)).selected_text(titles[kind]).show_ui(
                ui,
                |ui| {
                    for (i, title) in titles.iter().enumerate() {
                        if ui.selectable_value(&mut kind, i, *title).changed() {
                            *rule = match i {
                                0 => ExtensionRule::Lower,
                                1 => ExtensionRule::Upper,
                                2 => ExtensionRule::Set(String::new()),
                                _ => ExtensionRule::Remove,
                            };
                            changed = true;
                        }
                    }
                },
            );
            if let ExtensionRule::Set(value) = rule {
                changed |= text(ui, value, "Новое расширение");
            }
        }
        Rule::RemoveRange { from, count, from_end } => {
            ui.horizontal(|ui| {
                ui.label("с позиции");
                changed |= ui.add(DragValue::new(from).range(0..=255)).changed();
                ui.label("символов");
                changed |= ui.add(DragValue::new(count).range(0..=255)).changed();
            });
            changed |= ui.checkbox(from_end, "считать с конца").changed();
        }
        Rule::Trim => {
            ui.label(
                RichText::new("Пробелы по краям и двойные пробелы").color(theme::TEXT_SECONDARY),
            );
        }
    }
    changed
}

fn case_title(mode: CaseMode) -> &'static str {
    match mode {
        CaseMode::Lower => "строчные",
        CaseMode::Upper => "ПРОПИСНЫЕ",
        CaseMode::Title => "Каждое Слово",
        CaseMode::Sentence => "Как в предложении",
    }
}

fn preview_table(ui: &mut Ui, state: &State) {
    egui::Frame::new()
        .fill(theme::FIELD)
        .corner_radius(CornerRadius::same(6))
        .inner_margin(egui::Margin::same(6))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            let half = ((ui.available_width() - 30.0) / 2.0).max(60.0);
            ui.horizontal(|ui| {
                ui.add_sized(
                    [half, 20.0],
                    egui::Label::new(RichText::new("Было").color(theme::TEXT_DISABLED)),
                );
                ui.add_space(20.0);
                ui.add_sized(
                    [half, 20.0],
                    egui::Label::new(RichText::new("Станет").color(theme::TEXT_DISABLED)),
                );
            });
            let names = state.names.as_ref().ok();
            ScrollArea::vertical().id_salt("batch-preview").auto_shrink([false, false]).show_rows(
                ui,
                24.0,
                state.items.len(),
                |ui, rows| {
                    for row in rows {
                        let item = &state.items[row];
                        let new = names.and_then(|n| n.get(row)).cloned().unwrap_or_default();
                        let status = state.statuses.get(row).cloned().unwrap_or(Status::Ok);
                        let (color, tip) = match &status {
                            Status::Ok => (theme::accent(), None),
                            Status::Unchanged => (theme::TEXT_DISABLED, None),
                            Status::Warning(text) => (theme::WARN, Some(text.clone())),
                            Status::Error(text) => (theme::CRITICAL, Some(text.clone())),
                        };
                        let response = ui.horizontal(|ui| {
                            let old = crate::pane_view::elided(
                                ui,
                                &item.name(),
                                theme::regular(13.5),
                                theme::TEXT_SECONDARY,
                                half,
                            );
                            let (rect, _) =
                                ui.allocate_exact_size(vec2(half, 22.0), egui::Sense::hover());
                            ui.painter().galley(
                                rect.left_center() - vec2(0.0, old.size().y / 2.0),
                                old,
                                theme::TEXT_SECONDARY,
                            );
                            let (arrow, _) =
                                ui.allocate_exact_size(vec2(20.0, 22.0), egui::Sense::hover());
                            icons::arrow_right(
                                ui.painter(),
                                arrow.shrink2(vec2(4.0, 6.0)),
                                theme::TEXT_DISABLED,
                            );
                            let text_color = if matches!(status, Status::Unchanged) {
                                theme::TEXT_DISABLED
                            } else {
                                color.lerp_to_gamma(Color32::WHITE, 0.35)
                            };
                            let galley = crate::pane_view::elided(
                                ui,
                                &new,
                                theme::regular(13.5),
                                text_color,
                                half,
                            );
                            let (rect, _) =
                                ui.allocate_exact_size(vec2(half, 22.0), egui::Sense::hover());
                            ui.painter().galley(
                                rect.left_center() - vec2(0.0, galley.size().y / 2.0),
                                galley,
                                text_color,
                            );
                        });
                        if let Some(tip) = tip {
                            response.response.on_hover_text(tip);
                        }
                    }
                },
            );
        });
}
