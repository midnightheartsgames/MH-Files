//! Палитра команд (Ctrl+Shift+P), GoTo (Ctrl+G) и поиск во вложенных папках (Ctrl+Shift+F) —
//! одно всплывающее окно с полем ввода и списком.

use std::path::{Path, PathBuf};

use eframe::egui::{
    self, Align2, Color32, CornerRadius, Id, Key, RichText, Sense, Stroke, text::LayoutJob, vec2,
};
use mh_files_core::fuzzy::Fuzzy;
use mh_files_core::goto::{self, Candidate, Source};
use mh_files_core::location::{Location, path_label};
use mh_files_fs::Ticket;

use crate::app::{Action, FilesApp, OWNER_PALETTE, Target, drive_title};
use crate::commands::CommandId;
use crate::theme;

const ROWS: usize = 12;

pub enum Mode {
    Commands,
    GoTo { candidates: Vec<Candidate> },
    Search { root: PathBuf },
}

pub struct State {
    mode: Mode,
    query: String,
    selected: usize,
    fresh: bool,
    /// Дописывание пути: для какой папки и что в ней нашлось.
    completion: Option<(PathBuf, Vec<String>)>,
    completion_generation: u64,
    completion_asked: Option<PathBuf>,
}

enum Row {
    Command { command: CommandId, enabled: bool, matched: Vec<usize> },
    Place(Candidate),
    SearchHere,
}

impl State {
    fn new(mode: Mode) -> State {
        State {
            mode,
            query: String::new(),
            selected: 0,
            fresh: true,
            completion: None,
            completion_generation: 0,
            completion_asked: None,
        }
    }

    pub fn commands() -> State {
        State::new(Mode::Commands)
    }

    pub fn search(root: PathBuf) -> State {
        State::new(Mode::Search { root })
    }

    /// GoTo: кандидаты собираются один раз при открытии из того, что уже известно.
    pub fn goto(app: &FilesApp) -> State {
        let mut candidates = Vec::new();
        let mut push = |label: String, path: PathBuf, source| {
            candidates.push(Candidate { label, path, source })
        };
        for pane in &app.panes {
            for tab in &pane.tabs {
                if let Location::Dir(path) = &tab.location {
                    push(path_label(path), path.clone(), Source::Tab);
                }
            }
        }
        for group in &app.settings.groups {
            for item in &group.items {
                push(item.name.clone(), item.path.clone(), Source::Favorite);
            }
        }
        for (folder, path) in &app.places {
            push(folder.title().to_string(), path.clone(), Source::Place);
        }
        for drive in &app.drives {
            push(drive_title(drive), drive.root.clone(), Source::Drive);
        }
        for path in &app.recent {
            push(path_label(path), path.clone(), Source::Recent);
        }
        State::new(Mode::GoTo { candidates })
    }

    pub fn on_completion(&mut self, ticket: Ticket, dir: PathBuf, names: Vec<String>) {
        if ticket.generation == self.completion_generation {
            self.completion = Some((dir, names));
        }
    }

    fn rows(&mut self, app: &FilesApp) -> Vec<Row> {
        match &self.mode {
            Mode::Commands => {
                let mut fuzzy = Fuzzy::new(&self.query);
                let mut scored: Vec<(u32, Row)> = CommandId::ALL
                    .iter()
                    .filter_map(|&command| {
                        let by_name = fuzzy.score(command.name()).map(|s| s + 10);
                        let by_alias =
                            command.aliases().iter().filter_map(|a| fuzzy.score(a)).max();
                        let by_key = fuzzy.score(&app.keymap.text(command));
                        let score = by_name.max(by_alias).max(by_key)?;
                        let matched = fuzzy.indices(command.name());
                        let enabled = app.available_cached(command);
                        // Недоступные — в конце списка.
                        let score = if enabled { score + 1000 } else { score };
                        Some((score, Row::Command { command, enabled, matched }))
                    })
                    .collect();
                if !fuzzy.is_empty() {
                    scored.sort_by_key(|(score, _)| std::cmp::Reverse(*score));
                } else {
                    scored.sort_by_key(|(score, _)| std::cmp::Reverse(*score >= 1000));
                }
                scored.into_iter().map(|(_, row)| row).collect()
            }
            Mode::GoTo { candidates } => {
                let mut all = candidates.clone();
                let typed = goto::expand(&self.query);
                if goto::looks_like_path(&self.query) && !typed.is_empty() {
                    let path = mh_files_core::location::normalize(Path::new(&typed));
                    all.push(Candidate {
                        label: path.display().to_string(),
                        path,
                        source: Source::Typed,
                    });
                    if let Some((dir, names)) = &self.completion
                        && let Some((typed_dir, prefix)) = goto::split_for_completion(&typed)
                        && *dir == typed_dir
                    {
                        let prefix = prefix.to_lowercase();
                        for name in names.iter().filter(|n| n.to_lowercase().starts_with(&prefix)) {
                            all.push(Candidate {
                                label: name.clone(),
                                path: dir.join(name),
                                source: Source::Completion,
                            });
                        }
                    }
                    return goto::rank("", all, 40).into_iter().map(Row::Place).collect();
                }
                goto::rank(&self.query, all, 40).into_iter().map(Row::Place).collect()
            }
            Mode::Search { .. } => vec![Row::SearchHere],
        }
    }

    /// Для вводимого пути — попросить подпапки у воркера (один запрос на папку).
    fn ask_completion(&mut self, app: &FilesApp) {
        if !matches!(self.mode, Mode::GoTo { .. }) || !goto::looks_like_path(&self.query) {
            return;
        }
        let typed = goto::expand(&self.query);
        let Some((dir, _)) = goto::split_for_completion(&typed) else { return };
        if self.completion_asked.as_ref() == Some(&dir) {
            return;
        }
        self.completion_generation += 1;
        self.completion_asked = Some(dir.clone());
        let ticket = Ticket { owner: OWNER_PALETTE, generation: self.completion_generation };
        app.workers.complete(ticket, dir, String::new());
    }
}

/// Рисует палитру. `false` — закрыть.
pub fn show(ctx: &egui::Context, app: &mut FilesApp, state: &mut State) -> bool {
    state.ask_completion(app);
    let rows = state.rows(app);
    if state.selected >= rows.len() {
        state.selected = rows.len().saturating_sub(1);
    }
    let (up, down, enter, escape, tab_key, ctrl) = ctx.input_mut(|i| {
        (
            i.consume_key(egui::Modifiers::NONE, Key::ArrowUp),
            i.consume_key(egui::Modifiers::NONE, Key::ArrowDown),
            i.key_pressed(Key::Enter),
            i.consume_key(egui::Modifiers::NONE, Key::Escape),
            i.consume_key(egui::Modifiers::NONE, Key::Tab),
            i.modifiers.ctrl,
        )
    });
    if escape {
        return false;
    }
    if up {
        state.selected = state.selected.saturating_sub(1);
    }
    if down && state.selected + 1 < rows.len() {
        state.selected += 1;
    }
    // Tab в GoTo — дописать путь выбранного кандидата.
    if tab_key && let Some(Row::Place(candidate)) = rows.get(state.selected) {
        state.query = format!("{}{}", candidate.path.display(), std::path::MAIN_SEPARATOR);
        state.fresh = true;
    }

    let (title, hint) = match &state.mode {
        Mode::Commands => ("Команды", "Имя команды…"),
        Mode::GoTo { .. } => ("Перейти", "Папка, диск, путь или %переменная%…"),
        Mode::Search { .. } => ("Поиск во вложенных папках", "Слова или маска: qwen gguf, *.png…"),
    };
    let screen = ctx.content_rect();
    let width = (screen.width() - 40.0).min(640.0);
    let mut chosen: Option<usize> = if enter { Some(state.selected) } else { None };
    let mut keep = true;
    let area = egui::Area::new(Id::new("palette"))
        .order(egui::Order::Foreground)
        .anchor(Align2::CENTER_TOP, vec2(0.0, 70.0))
        .show(ctx, |ui| {
            egui::Frame::new()
                .fill(theme::PANEL)
                .stroke(Stroke::new(1.0, theme::CARD_STROKE))
                .corner_radius(CornerRadius::same(8))
                .shadow(egui::Shadow {
                    offset: [0, 8],
                    blur: 24,
                    spread: 0,
                    color: Color32::from_black_alpha(140),
                })
                .inner_margin(egui::Margin::same(10))
                .show(ui, |ui| {
                    ui.set_width(width);
                    ui.label(
                        RichText::new(title.to_uppercase())
                            .font(theme::bold(11.5))
                            .color(theme::TEXT_DISABLED)
                            .extra_letter_spacing(1.2),
                    );
                    ui.add_space(4.0);
                    let id = Id::new("palette-input");
                    let response = ui.add(
                        egui::TextEdit::singleline(&mut state.query)
                            .id(id)
                            .hint_text(hint)
                            .font(theme::regular(16.0))
                            .desired_width(f32::INFINITY)
                            .margin(egui::Margin::symmetric(8, 6)),
                    );
                    if std::mem::take(&mut state.fresh) {
                        response.request_focus();
                        let len = state.query.chars().count();
                        if let Some(mut text_state) = egui::TextEdit::load_state(ui.ctx(), id) {
                            let cursor = egui::text::CCursor::new(len);
                            text_state
                                .cursor
                                .set_char_range(Some(egui::text::CCursorRange::one(cursor)));
                            text_state.store(ui.ctx(), id);
                        }
                    } else if !response.has_focus() && !enter {
                        response.request_focus();
                    }
                    if response.changed() {
                        state.selected = 0;
                    }
                    ui.add_space(6.0);
                    let first = state.selected.saturating_sub(ROWS - 1);
                    for (index, row) in rows.iter().enumerate().skip(first).take(ROWS) {
                        if row_ui(ui, app, state, row, index == state.selected).clicked() {
                            chosen = Some(index);
                        }
                    }
                    if rows.is_empty() {
                        ui.label(RichText::new("Ничего не найдено").color(theme::TEXT_DISABLED));
                    }
                });
        });
    if ctx.input(|i| i.pointer.any_pressed())
        && let Some(pos) = ctx.input(|i| i.pointer.interact_pos())
        && !area.response.rect.contains(pos)
    {
        keep = false;
    }
    if let Some(index) = chosen
        && let Some(row) = rows.get(index)
    {
        keep = false;
        match row {
            Row::Command { command, enabled: true, .. } => app.actions.push(Action::Run(*command)),
            Row::Command { .. } => keep = true,
            Row::Place(candidate) => {
                let target = if ctrl { Target::NewTab } else { Target::Current };
                app.actions
                    .push(Action::Open { location: Location::Dir(candidate.path.clone()), target });
            }
            Row::SearchHere => {
                if let Mode::Search { root } = &state.mode
                    && !state.query.trim().is_empty()
                {
                    let location = Location::Search {
                        root: root.clone(),
                        query: state.query.trim().to_string(),
                    };
                    app.actions.push(Action::Open { location, target: Target::Current });
                } else {
                    keep = true;
                }
            }
        }
    }
    keep
}

fn row_ui(
    ui: &mut egui::Ui,
    app: &FilesApp,
    state: &State,
    row: &Row,
    selected: bool,
) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(vec2(ui.available_width(), 30.0), Sense::click());
    let painter = ui.painter();
    if selected {
        painter.rect_filled(rect, CornerRadius::same(4), theme::selection_fill());
    } else if response.hovered() {
        painter.rect_filled(rect, CornerRadius::same(4), theme::CARD);
    }
    let left = egui::pos2(rect.left() + 10.0, rect.center().y);
    let right = egui::pos2(rect.right() - 10.0, rect.center().y);
    match row {
        Row::Command { command, enabled, matched } => {
            let color = if *enabled { theme::TEXT_PRIMARY } else { theme::TEXT_DISABLED };
            let galley = ui.painter().layout_job(highlighted(command.name(), matched, color));
            painter.galley(left - vec2(0.0, galley.size().y / 2.0), galley, color);
            painter.text(
                right,
                Align2::RIGHT_CENTER,
                app.keymap.label(*command),
                theme::regular(13.0),
                theme::TEXT_DISABLED,
            );
        }
        Row::Place(candidate) => {
            let mut fuzzy = Fuzzy::for_paths(&state.query);
            let matched = fuzzy.indices(&candidate.label);
            let galley = ui.painter().layout_job(highlighted(
                &candidate.label,
                &matched,
                theme::TEXT_PRIMARY,
            ));
            let label_width = galley.size().x;
            painter.galley(left - vec2(0.0, galley.size().y / 2.0), galley, theme::TEXT_PRIMARY);
            let path = candidate.path.display().to_string();
            let path_width = rect.width() - label_width - 110.0;
            let path_galley = crate::pane_view::elided(
                ui,
                &path,
                theme::regular(12.5),
                theme::TEXT_DISABLED,
                path_width.max(40.0),
            );
            painter.galley(
                egui::pos2(
                    left.x + label_width + 12.0,
                    rect.center().y - path_galley.size().y / 2.0,
                ),
                path_galley,
                theme::TEXT_DISABLED,
            );
            painter.text(
                right,
                Align2::RIGHT_CENTER,
                candidate.source.title(),
                theme::regular(12.0),
                theme::accent().gamma_multiply(0.8),
            );
        }
        Row::SearchHere => {
            let root = match &state.mode {
                Mode::Search { root } => root.display().to_string(),
                _ => String::new(),
            };
            let text = if state.query.trim().is_empty() {
                format!("Введите запрос — поиск в «{root}» и всех вложенных папках")
            } else {
                format!("Искать «{}» в {root}", state.query.trim())
            };
            painter.text(
                left,
                Align2::LEFT_CENTER,
                text,
                theme::regular(14.0),
                theme::TEXT_SECONDARY,
            );
            painter.text(
                right,
                Align2::RIGHT_CENTER,
                "Enter",
                theme::regular(12.0),
                theme::TEXT_DISABLED,
            );
        }
    }
    response
}

/// Текст с подсвеченными совпавшими символами.
fn highlighted(text: &str, matched: &[usize], color: Color32) -> LayoutJob {
    let mut job = LayoutJob::default();
    for (index, c) in text.chars().enumerate() {
        let hit = matched.binary_search(&index).is_ok();
        let format = egui::TextFormat {
            font_id: if hit { theme::bold(14.5) } else { theme::regular(14.5) },
            color: if hit { theme::accent() } else { color },
            ..Default::default()
        };
        job.append(&c.to_string(), 0.0, format);
    }
    job
}
