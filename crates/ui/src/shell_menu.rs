//! Пункты меню Windows внутри своего контекстного меню — как в OneCommander: сверху команды
//! MH Files, ниже — пункты Проводника и сторонних программ (7-Zip, Git, PowerToys, VS Code…)
//! с их значками. Своё меню открывается сразу; меню Shell тем временем собирается в фоновом
//! потоке (расширения грузятся и читают диск) и появляется, как только готово.
//!
//! Поток меню живёт, пока открыто своё меню: выбранный пункт выполняется в нём же, объектом
//! того меню, из которого его прочитали. Своё меню закрылось — отправитель команд
//! отпускается, канал закрывается, поток заканчивается.
//!
//! Чтобы пункты появлялись сразу, меню заказывается заранее — при нажатии правой кнопки
//! (своё меню откроется при отпускании) — и приходит в два приёма: сначала пункты
//! расширений, затем подменю, которые Windows заполняет при открытии («Отправить» перебирает
//! диски). Пока ждём — место под пункты держится по прошлому такому же меню, без скачка.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use crossbeam_channel::Sender;
use eframe::egui::{self, ColorImage, Image, RichText, TextureHandle, TextureOptions, Ui, vec2};
use mh_files_fs::ShellJob;
use mh_files_platform::shell::{MenuIcon, MenuTarget, ShellMenuItem, tidy_menu};

use crate::app::FilesApp;
use crate::theme;

/// Команды объектов, которые в своём меню уже есть: Windows их не дублирует.
const OWN_ITEM_VERBS: &[&str] = &[
    "open",
    "opennewwindow",
    "opennewprocess",
    "opennewtab",
    "explore",
    "openas",
    "cut",
    "copy",
    "delete",
    "rename",
    "properties",
    "copyaspath",
];

/// То же для пустого места папки.
const OWN_BACKGROUND_VERBS: &[&str] =
    &["paste", "properties", "newfolder", "undo", "redo", "refresh"];

/// Дольше этого меню Windows обычно не собирается; дальше — честно сказать, что ждём
/// зависшее расширение.
const SLOW: Duration = Duration::from_secs(6);

const ICON_SIZE: f32 = 16.0;

/// Сколько ждёт заказанное заранее меню: дальше состояние объектов (Git, OneDrive) могло
/// устареть, а поток меню держать незачем.
const PREFETCH_TTL: Duration = Duration::from_secs(8);

/// Высота раздела Windows у последнего собранного меню того же вида (`kind`): её держим, пока
/// пункты собираются.
pub type Heights = HashMap<String, f32>;

pub struct ShellMenu {
    target: MenuTarget,
    /// Shift при щелчке: расширенные команды.
    extended: bool,
    generation: u64,
    started: Instant,
    /// Меню рисовалось в этом кадре. Не рисовалось — закрыто: поток меню отпускается.
    seen: bool,
    state: State,
}

enum State {
    Loading,
    Ready {
        /// Подменю уже заполнены; без этого — первая часть, «Отправить» и прочие ещё пустые.
        complete: bool,
        items: Vec<Item>,
        /// Есть ли значок хоть у одного пункта: тогда и у остальных место под него, чтобы
        /// подписи шли ровной колонкой.
        icons: bool,
        blank: TextureHandle,
        commands: Sender<u32>,
    },
    Failed(String),
}

enum Item {
    Separator,
    Command {
        id: u32,
        label: String,
        icon: Option<TextureHandle>,
        enabled: bool,
    },
    Submenu {
        label: String,
        icon: Option<TextureHandle>,
        enabled: bool,
        items: Vec<Item>,
        icons: bool,
    },
}

/// Пункты Windows встраиваются в свои меню: есть Windows и не выключено в настройках.
pub fn enabled(app: &FilesApp) -> bool {
    cfg!(windows) && app.settings.files.windows_menu_items
}

/// Заказать меню Shell для `target` у фонового потока.
fn request(app: &mut FilesApp, target: MenuTarget, extended: bool) -> ShellMenu {
    app.shell_menu_generation += 1;
    let generation = app.shell_menu_generation;
    app.workers.shell(ShellJob::LiveMenu { generation, target: target.clone(), extended });
    ShellMenu {
        target,
        extended,
        generation,
        started: Instant::now(),
        seen: true,
        state: State::Loading,
    }
}

/// Вид меню для запоминания высоты: пустое место папки или объекты с таким расширением.
fn kind(target: &MenuTarget) -> String {
    match target {
        MenuTarget::Background(_) => "background".into(),
        MenuTarget::Items(paths) => {
            let ext = paths
                .first()
                .and_then(|p| p.extension())
                .map(|e| e.to_string_lossy().to_lowercase())
                .unwrap_or_default();
            format!("items:{}:{ext}", paths.len().min(2))
        }
    }
}

/// Правая кнопка нажата над `response`: меню откроется при отпускании, а пункты Windows для
/// `target` можно собирать уже сейчас. `target` — ровно то, что получит [`section`].
pub fn prefetch_on_press(
    app: &mut FilesApp,
    response: &egui::Response,
    target: impl FnOnce() -> MenuTarget,
) {
    if !enabled(app)
        || !response.contains_pointer()
        || !response.ctx.input(|i| i.pointer.button_pressed(egui::PointerButton::Secondary))
    {
        return;
    }
    let target = target();
    let extended = response.ctx.input(|i| i.modifiers.shift);
    let same = |menu: &ShellMenu| menu.target == target && menu.extended == extended;
    if app.shell_menu.as_ref().is_some_and(same) || app.shell_prefetch.as_ref().is_some_and(same) {
        return;
    }
    // Прежний заказ отпускается вместе с отправителем: его поток заканчивается.
    app.shell_prefetch = Some(request(app, target, extended));
    response.ctx.request_repaint_after(PREFETCH_TTL);
}

/// Через пару секунд после запуска собрать и выбросить меню папки и пустого места: первое
/// настоящее меню тогда не ждёт загрузки расширений с диска.
pub fn warm_up(ctx: &egui::Context, app: &mut FilesApp) {
    if app.shell_warmed || !enabled(app) {
        return;
    }
    let since_start = ctx.input(|i| i.time);
    if since_start < 2.0 {
        ctx.request_repaint_after(Duration::from_secs_f64(2.0 - since_start));
        return;
    }
    app.shell_warmed = true;
    // Переменная окружения, не Shell: в потоке UI нет ввода-вывода.
    let Some(home) = std::env::var_os("USERPROFILE").map(std::path::PathBuf::from) else {
        return;
    };
    // Поколение 0 не выдаётся никогда: ответ отбросится, и поток закончится.
    for target in [MenuTarget::Items(vec![home.clone()]), MenuTarget::Background(home)] {
        app.workers.shell(ShellJob::LiveMenu { generation: 0, target, extended: false });
    }
}

/// Раздел пунктов Windows (с разделителем сверху) в открытом сейчас меню для `target`.
/// Рисуется каждый кадр, пока меню открыто; первый раз — берёт заказанное при нажатии меню
/// или заказывает его. `false` — раздела нет (выключен в настройках или не Windows).
pub fn section(ui: &mut Ui, app: &mut FilesApp, target: MenuTarget) -> bool {
    if !enabled(app) {
        return false;
    }
    let current = app.shell_menu.as_ref().is_some_and(|menu| menu.target == target);
    if !current {
        let extended = ui.input(|i| i.modifiers.shift);
        let prefetched = app.shell_prefetch.take().filter(|menu| {
            menu.target == target
                && menu.extended == extended
                && menu.started.elapsed() < PREFETCH_TTL
        });
        app.shell_menu = Some(match prefetched {
            Some(menu) => menu,
            None => request(app, target, extended),
        });
    }
    let key = app.shell_menu.as_ref().map(|menu| kind(&menu.target)).unwrap_or_default();
    let reserved = app.shell_menu_heights.get(&key).copied().unwrap_or(0.0);
    let Some(menu) = app.shell_menu.as_mut() else { return false };
    menu.seen = true;
    let mut chosen = None;
    let top = ui.cursor().top();
    let mut done = false;
    match &menu.state {
        State::Loading => {
            ui.separator();
            ui.horizontal(|ui| {
                ui.add(egui::Spinner::new().size(12.0));
                let text = if menu.started.elapsed() < SLOW {
                    "Пункты Windows…"
                } else {
                    "Пункты Windows: расширение Shell не отвечает"
                };
                ui.label(RichText::new(text).color(theme::TEXT_DISABLED));
            });
            ui.ctx().request_repaint_after(Duration::from_millis(250));
        }
        State::Failed(error) => {
            ui.separator();
            ui.label(RichText::new("Пункты Windows недоступны").color(theme::TEXT_DISABLED))
                .on_hover_text(error);
            done = true;
        }
        State::Ready { items, icons, blank, complete, .. } => {
            if !items.is_empty() {
                ui.separator();
                show_items(ui, items, *icons, blank, *complete, &mut chosen);
            }
            done = *complete;
        }
    }
    let height = ui.cursor().top() - top;
    if done {
        app.shell_menu_heights.insert(key, height);
    } else if reserved > height {
        // Место под пункты, которые вот-вот придут: меню не растёт и не прыгает у края экрана.
        ui.add_space(reserved - height);
    }
    if let Some(id) = chosen {
        if let Some(ShellMenu { state: State::Ready { commands, .. }, .. }) = &app.shell_menu {
            let _ = commands.send(id);
        }
        // Команда отправлена; отпустить отправитель — поток выполнит её и закончит.
        app.shell_menu = None;
        ui.close();
    }
    true
}

fn show_items(
    ui: &mut Ui,
    items: &[Item],
    icons: bool,
    blank: &TextureHandle,
    complete: bool,
    chosen: &mut Option<u32>,
) {
    for item in items {
        match item {
            Item::Separator => {
                ui.separator();
            }
            Item::Command { id, label, icon, enabled } => {
                let button = match image(icon.as_ref(), icons, blank) {
                    Some(image) => egui::Button::image_and_text(image, label.as_str()),
                    None => egui::Button::new(label.as_str()),
                };
                if ui.add_enabled(*enabled, button).clicked() {
                    *chosen = Some(*id);
                }
            }
            Item::Submenu { label, icon, enabled, items, icons: inner } => {
                ui.add_enabled_ui(*enabled, |ui| {
                    let add = |ui: &mut Ui| {
                        if items.is_empty() && !complete {
                            // Подменю ещё заполняется («Отправить» перебирает диски).
                            ui.horizontal(|ui| {
                                ui.add(egui::Spinner::new().size(12.0));
                                ui.label(RichText::new("Загрузка…").color(theme::TEXT_DISABLED));
                            });
                        } else {
                            show_items(ui, items, *inner, blank, complete, chosen);
                        }
                    };
                    match image(icon.as_ref(), icons, blank) {
                        Some(image) => ui.menu_image_text_button(image, label.as_str(), add),
                        None => ui.menu_button(label.as_str(), add),
                    }
                });
            }
        }
    }
}

/// Значок пункта, прозрачная заглушка того же размера (если значки есть у соседей) или ничего.
fn image<'a>(
    icon: Option<&'a TextureHandle>,
    icons: bool,
    blank: &'a TextureHandle,
) -> Option<Image<'a>> {
    let texture = icon.or(icons.then_some(blank))?;
    Some(Image::new(texture).fit_to_exact_size(vec2(ICON_SIZE, ICON_SIZE)))
}

/// Пункты собраны (или не собрались); без `complete` — первая часть. Ответ на устаревший
/// запрос отбрасывается — вместе с отправителем, и его поток заканчивается.
pub fn on_ready(
    app: &mut FilesApp,
    ctx: &egui::Context,
    generation: u64,
    items: Result<Vec<ShellMenuItem>, String>,
    complete: bool,
    commands: Sender<u32>,
) {
    let Some(menu) = [app.shell_menu.as_mut(), app.shell_prefetch.as_mut()]
        .into_iter()
        .flatten()
        .find(|menu| menu.generation == generation)
    else {
        return;
    };
    menu.state = match items {
        Ok(items) => {
            let hidden = match menu.target {
                MenuTarget::Items(_) => OWN_ITEM_VERBS,
                MenuTarget::Background(_) => OWN_BACKGROUND_VERBS,
            };
            let items = convert(ctx, tidy_menu(items, hidden, !complete));
            let blank = ctx.load_texture(
                "shell-menu-blank",
                ColorImage::new([1, 1], vec![egui::Color32::TRANSPARENT]),
                TextureOptions::LINEAR,
            );
            State::Ready { complete, icons: has_icons(&items), items, blank, commands }
        }
        Err(error) => State::Failed(error),
    };
}

fn convert(ctx: &egui::Context, items: Vec<ShellMenuItem>) -> Vec<Item> {
    items
        .into_iter()
        .map(|item| match item {
            ShellMenuItem::Separator => Item::Separator,
            ShellMenuItem::Command { id, label, icon, enabled, .. } => {
                Item::Command { id, label, icon: texture(ctx, icon), enabled }
            }
            ShellMenuItem::Submenu { label, icon, enabled, items } => {
                let items = convert(ctx, items);
                let icons = has_icons(&items);
                Item::Submenu { label, icon: texture(ctx, icon), enabled, items, icons }
            }
        })
        .collect()
}

fn texture(ctx: &egui::Context, icon: Option<MenuIcon>) -> Option<TextureHandle> {
    let icon = icon?;
    let size = [icon.width as usize, icon.height as usize];
    let image = ColorImage::from_rgba_premultiplied(size, &icon.rgba);
    Some(ctx.load_texture("shell-menu-icon", image, TextureOptions::LINEAR))
}

fn has_icons(items: &[Item]) -> bool {
    items.iter().any(|item| match item {
        Item::Command { icon, .. } | Item::Submenu { icon, .. } => icon.is_some(),
        Item::Separator => false,
    })
}

/// Конец кадра: меню, которое в этом кадре не рисовалось, закрыто — отпустить его поток; так
/// же и с заказанным заранее, которое так и не открыли.
pub fn end_frame(app: &mut FilesApp) {
    match &mut app.shell_menu {
        Some(menu) if menu.seen => menu.seen = false,
        Some(_) => app.shell_menu = None,
        None => {}
    }
    if app.shell_prefetch.as_ref().is_some_and(|menu| menu.started.elapsed() >= PREFETCH_TTL) {
        app.shell_prefetch = None;
    }
}
