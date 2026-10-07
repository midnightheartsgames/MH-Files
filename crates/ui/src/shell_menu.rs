//! Пункты меню Windows внутри своего контекстного меню — как в OneCommander: сверху команды
//! MH Files, ниже — пункты Проводника и сторонних программ (7-Zip, Git, PowerToys, VS Code…)
//! с их значками. Своё меню открывается сразу; меню Shell тем временем собирается в фоновом
//! потоке (расширения грузятся и читают диск) и появляется, как только готово.
//!
//! Поток меню живёт, пока открыто своё меню: выбранный пункт выполняется в нём же, объектом
//! того меню, из которого его прочитали. Своё меню закрылось — отправитель команд
//! отпускается, канал закрывается, поток заканчивается.

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

pub struct ShellMenu {
    target: MenuTarget,
    generation: u64,
    started: Instant,
    /// Меню рисовалось в этом кадре. Не рисовалось — закрыто: поток меню отпускается.
    seen: bool,
    state: State,
}

enum State {
    Loading,
    Ready {
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

/// Раздел пунктов Windows (с разделителем сверху) в открытом сейчас меню для `target`.
/// Рисуется каждый кадр, пока меню открыто; первый раз — заказывает меню Shell. `false` —
/// раздела нет (выключен в настройках или не Windows).
pub fn section(ui: &mut Ui, app: &mut FilesApp, target: MenuTarget) -> bool {
    if !enabled(app) {
        return false;
    }
    let current = app.shell_menu.as_ref().is_some_and(|menu| menu.target == target);
    if !current {
        app.shell_menu_generation += 1;
        let generation = app.shell_menu_generation;
        app.workers.shell(ShellJob::LiveMenu { generation, target: target.clone() });
        app.shell_menu = Some(ShellMenu {
            target,
            generation,
            started: Instant::now(),
            seen: true,
            state: State::Loading,
        });
    }
    let Some(menu) = app.shell_menu.as_mut() else { return false };
    menu.seen = true;
    let mut chosen = None;
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
        }
        State::Ready { items, icons, blank, .. } => {
            if !items.is_empty() {
                ui.separator();
                show_items(ui, items, *icons, blank, &mut chosen);
            }
        }
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
                    let add = |ui: &mut Ui| show_items(ui, items, *inner, blank, chosen);
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

/// Пункты собраны (или не собрались). Ответ на устаревший запрос отбрасывается — вместе с
/// отправителем, и его поток заканчивается.
pub fn on_ready(
    app: &mut FilesApp,
    ctx: &egui::Context,
    generation: u64,
    items: Result<Vec<ShellMenuItem>, String>,
    commands: Sender<u32>,
) {
    let Some(menu) = app.shell_menu.as_mut().filter(|menu| menu.generation == generation) else {
        return;
    };
    menu.state = match items {
        Ok(items) => {
            let hidden = match menu.target {
                MenuTarget::Items(_) => OWN_ITEM_VERBS,
                MenuTarget::Background(_) => OWN_BACKGROUND_VERBS,
            };
            let items = convert(ctx, tidy_menu(items, hidden));
            let blank = ctx.load_texture(
                "shell-menu-blank",
                ColorImage::new([1, 1], vec![egui::Color32::TRANSPARENT]),
                TextureOptions::LINEAR,
            );
            State::Ready { icons: has_icons(&items), items, blank, commands }
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

/// Конец кадра: меню, которое в этом кадре не рисовалось, закрыто — отпустить его поток.
pub fn end_frame(app: &mut FilesApp) {
    match &mut app.shell_menu {
        Some(menu) if menu.seen => menu.seen = false,
        Some(_) => app.shell_menu = None,
        None => {}
    }
}
