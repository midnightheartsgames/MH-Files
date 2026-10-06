//! Свой заголовок окна в стиле MH: значок и название, где мы сейчас, строка поиска по дискам и
//! кнопки окна. Пустое место тянет окно, двойной щелчок разворачивает его, края окна меняют
//! размер. Выключается в настройках — тогда рамка системная.

use eframe::egui::{
    self, Align2, Color32, CornerRadius, Id, Painter, Rect, ResizeDirection, Sense, Stroke, Ui,
    ViewportCommand, pos2, vec2,
};

use crate::app::{Action, FilesApp};
use crate::commands::CommandId;
use crate::{icons, theme};

pub const HEIGHT: f32 = 36.0;
const BUTTON_WIDTH: f32 = 46.0;
/// Ширина полосы у края окна, за которую меняется размер.
const BORDER: f32 = 5.0;
const CLOSE_HOVER: Color32 = Color32::from_rgb(0xC4, 0x2B, 0x1C);

pub fn show(ui: &mut Ui, app: &mut FilesApp) {
    let rect = ui.max_rect();
    let ctx = ui.ctx().clone();
    let maximized = ctx.input(|i| i.viewport().maximized.unwrap_or(false));
    let painter = ui.painter().clone();
    painter.rect_filled(rect, CornerRadius::ZERO, theme::PANEL);

    // Фон — первым: кнопки и поле поиска лежат поверх и забирают свои щелчки.
    let background = ui.interact(rect, Id::new("titlebar"), Sense::click_and_drag());
    if background.double_clicked() {
        ctx.send_viewport_cmd(ViewportCommand::Maximized(!maximized));
    } else if background.drag_started_by(egui::PointerButton::Primary) {
        ctx.send_viewport_cmd(ViewportCommand::StartDrag);
    }

    // Значок и название.
    let logo =
        Rect::from_min_size(pos2(rect.left() + 14.0, rect.center().y - 8.0), vec2(16.0, 16.0));
    for (i, h) in [7.0, 16.0, 11.0].into_iter().enumerate() {
        painter.rect_filled(
            Rect::from_min_size(
                pos2(logo.left() + i as f32 * 6.0, logo.bottom() - h),
                vec2(4.0, h),
            ),
            1,
            theme::accent(),
        );
    }
    let name = painter.text(
        pos2(logo.right() + 10.0, rect.center().y),
        Align2::LEFT_CENTER,
        "MH FILES",
        theme::bold(14.0),
        theme::TEXT_SECONDARY,
    );

    // Кнопки окна справа.
    let mut x = rect.right();
    let mut button = |kind: Button| {
        x -= BUTTON_WIDTH;
        let area = Rect::from_min_size(pos2(x, rect.top()), vec2(BUTTON_WIDTH, rect.height()));
        window_button(ui, &painter, area, kind, maximized)
    };
    if button(Button::Close) {
        ctx.send_viewport_cmd(ViewportCommand::Close);
    }
    if button(Button::Maximize) {
        ctx.send_viewport_cmd(ViewportCommand::Maximized(!maximized));
    }
    if button(Button::Minimize) {
        ctx.send_viewport_cmd(ViewportCommand::Minimized(true));
    }
    let buttons_left = x;

    // Строка поиска по дискам посередине.
    let width = (rect.width() * 0.36).clamp(220.0, 520.0);
    let center_x = rect.center().x.max(name.right() + 24.0 + width / 2.0);
    let field = Rect::from_center_size(pos2(center_x, rect.center().y), vec2(width, 26.0));
    if field.right() < buttons_left - 12.0 {
        let response = ui.interact(field, Id::new("titlebar-search"), Sense::click());
        let fill = if response.hovered() { theme::CARD } else { theme::BACKGROUND };
        painter.rect_filled(field, CornerRadius::same(5), fill);
        painter.rect_stroke(
            field,
            CornerRadius::same(5),
            Stroke::new(1.0, theme::CARD_STROKE),
            egui::StrokeKind::Inside,
        );
        let icon =
            Rect::from_center_size(pos2(field.left() + 15.0, field.center().y), vec2(13.0, 13.0));
        icons::search(&painter, icon, theme::TEXT_DISABLED);
        let query = match &app.tab().location {
            mh_files_core::location::Location::Index { query } if !query.is_empty() => {
                Some(query.clone())
            }
            _ => None,
        };
        let (text, color) = match query {
            Some(query) => (query, theme::TEXT_SECONDARY),
            None => ("Поиск по дискам".to_string(), theme::TEXT_DISABLED),
        };
        let galley =
            crate::pane_view::elided(ui, &text, theme::regular(13.5), color, field.width() - 90.0);
        painter.galley(
            pos2(field.left() + 30.0, field.center().y - galley.size().y / 2.0),
            galley,
            color,
        );
        painter.text(
            pos2(field.right() - 10.0, field.center().y),
            Align2::RIGHT_CENTER,
            app.keymap.text(CommandId::SearchEverywhere),
            theme::regular(12.0),
            theme::TEXT_DISABLED,
        );
        if response.clicked() {
            app.actions.push(Action::Run(CommandId::SearchEverywhere));
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Button {
    Minimize,
    Maximize,
    Close,
}

/// Кнопка окна. `true` — нажата.
fn window_button(
    ui: &mut Ui,
    painter: &Painter,
    area: Rect,
    kind: Button,
    maximized: bool,
) -> bool {
    let response = ui.interact(area, Id::new(("window-button", kind as u8)), Sense::click());
    let hovered = response.hovered();
    if hovered {
        let fill = if kind == Button::Close { CLOSE_HOVER } else { theme::CARD };
        painter.rect_filled(area, CornerRadius::ZERO, fill);
    }
    let color = if hovered { theme::TEXT_PRIMARY } else { theme::TEXT_SECONDARY };
    let stroke = Stroke::new(1.0, color);
    let c = area.center();
    match kind {
        Button::Minimize => {
            painter.hline((c.x - 5.0)..=(c.x + 5.0), c.y, stroke);
        }
        Button::Maximize if maximized => {
            let back = Rect::from_center_size(c + vec2(1.5, -1.5), vec2(8.0, 8.0));
            let front = Rect::from_center_size(c + vec2(-1.0, 1.0), vec2(8.0, 8.0));
            painter.rect_stroke(back, CornerRadius::same(1), stroke, egui::StrokeKind::Middle);
            painter.rect_filled(
                front,
                CornerRadius::same(1),
                if hovered { theme::CARD } else { theme::PANEL },
            );
            painter.rect_stroke(front, CornerRadius::same(1), stroke, egui::StrokeKind::Middle);
        }
        Button::Maximize => {
            let square = Rect::from_center_size(c, vec2(10.0, 10.0));
            painter.rect_stroke(square, CornerRadius::same(1), stroke, egui::StrokeKind::Middle);
        }
        Button::Close => icons::close(painter, Rect::from_center_size(c, vec2(11.0, 11.0)), color),
    }
    let tip = match kind {
        Button::Minimize => "Свернуть",
        Button::Maximize if maximized => "Восстановить",
        Button::Maximize => "Развернуть",
        Button::Close => "Закрыть",
    };
    response.on_hover_text(tip).clicked()
}

/// Края окна без системной рамки: курсор и изменение размера. Тонкая рамка вокруг окна —
/// чтобы оно не сливалось с соседними.
pub fn borders(ctx: &egui::Context) {
    let (maximized, fullscreen) = ctx.input(|i| {
        let viewport = i.viewport();
        (viewport.maximized.unwrap_or(false), viewport.fullscreen.unwrap_or(false))
    });
    if maximized || fullscreen {
        return;
    }
    let window = ctx.content_rect();
    ctx.layer_painter(egui::LayerId::new(egui::Order::Foreground, Id::new("window-border")))
        .rect_stroke(
            window,
            CornerRadius::ZERO,
            Stroke::new(1.0, theme::CARD_STROKE),
            egui::StrokeKind::Inside,
        );
    let Some(pos) = ctx.input(|i| i.pointer.hover_pos()) else { return };
    let left = pos.x - window.left() < BORDER;
    let right = window.right() - pos.x < BORDER;
    let top = pos.y - window.top() < BORDER;
    let bottom = window.bottom() - pos.y < BORDER;
    let direction = match (left, right, top, bottom) {
        (true, _, true, _) => ResizeDirection::NorthWest,
        (_, true, true, _) => ResizeDirection::NorthEast,
        (true, _, _, true) => ResizeDirection::SouthWest,
        (_, true, _, true) => ResizeDirection::SouthEast,
        (true, ..) => ResizeDirection::West,
        (_, true, ..) => ResizeDirection::East,
        (_, _, true, _) => ResizeDirection::North,
        (_, _, _, true) => ResizeDirection::South,
        _ => return,
    };
    ctx.set_cursor_icon(match direction {
        ResizeDirection::North | ResizeDirection::South => egui::CursorIcon::ResizeVertical,
        ResizeDirection::East | ResizeDirection::West => egui::CursorIcon::ResizeHorizontal,
        ResizeDirection::NorthWest | ResizeDirection::SouthEast => egui::CursorIcon::ResizeNwSe,
        ResizeDirection::NorthEast | ResizeDirection::SouthWest => egui::CursorIcon::ResizeNeSw,
    });
    if ctx.input(|i| i.pointer.primary_pressed()) {
        ctx.send_viewport_cmd(ViewportCommand::BeginResize(direction));
    }
}
