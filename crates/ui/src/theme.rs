//! Палитра и шрифты MH: те же цвета и Cuprum, что в MH Monitoring и MH Sidebar.

use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

use eframe::egui::{
    self, Color32, CornerRadius, FontData, FontDefinitions, FontFamily, FontId, Stroke, Visuals,
};

pub const BACKGROUND: Color32 = Color32::from_rgb(0x0B, 0x0B, 0x0D);
pub const WINDOW_BACKGROUND: Color32 = Color32::from_rgb(0x0E, 0x11, 0x16);
pub const PANEL: Color32 = Color32::from_rgb(0x12, 0x16, 0x1D);
pub const CARD: Color32 = Color32::from_rgb(0x15, 0x1A, 0x22);
pub const CARD_STROKE: Color32 = Color32::from_rgb(0x24, 0x2C, 0x38);
pub const FIELD: Color32 = Color32::from_rgb(0x0F, 0x13, 0x19);
pub const SWITCH_OFF: Color32 = Color32::from_rgb(0x3A, 0x42, 0x50);

pub const TEXT_PRIMARY: Color32 = Color32::from_rgb(0xF1, 0xF1, 0xF1);
pub const TEXT_SECONDARY: Color32 = Color32::from_rgb(0xA9, 0xA9, 0xAD);
pub const TEXT_DISABLED: Color32 = Color32::from_rgb(0x6A, 0x6A, 0x70);

pub const WARN: Color32 = Color32::from_rgb(0xF2, 0xA3, 0x3C);
pub const CRITICAL: Color32 = Color32::from_rgb(0xE8, 0x5C, 0x5C);

/// Акцент из настроек. Атомарный, чтобы его читали все функции рисования без передачи.
static ACCENT: AtomicU32 = AtomicU32::new(0x3FD0D8);

pub fn set_accent(rgb: [u8; 3]) {
    ACCENT.store(u32::from_be_bytes([0, rgb[0], rgb[1], rgb[2]]), Ordering::Relaxed);
}

pub fn accent() -> Color32 {
    let [_, r, g, b] = ACCENT.load(Ordering::Relaxed).to_be_bytes();
    Color32::from_rgb(r, g, b)
}

/// Заливка выделенной строки.
pub fn selection_fill() -> Color32 {
    accent().gamma_multiply(0.22)
}

/// Цвет полосы диска по заполненности — те же пороги, что у нагрузки в MH Monitoring.
pub fn usage_color(fraction: f32) -> Color32 {
    if fraction >= 0.9 {
        CRITICAL
    } else if fraction >= 0.7 {
        WARN
    } else {
        accent()
    }
}

const BOLD: &str = "cuprum-bold";

pub fn regular(size: f32) -> FontId {
    FontId::new(size, FontFamily::Proportional)
}

pub fn bold(size: f32) -> FontId {
    FontId::new(size, FontFamily::Name(BOLD.into()))
}

pub fn mono(size: f32) -> FontId {
    FontId::new(size, FontFamily::Monospace)
}

/// Cuprum первым; шрифты egui остаются запасными для символов, которых в Cuprum нет.
fn fonts() -> FontDefinitions {
    let mut fonts = FontDefinitions::default();
    fonts.font_data.insert(
        "cuprum".into(),
        Arc::new(FontData::from_static(include_bytes!("../../../assets/fonts/Cuprum-Regular.ttf"))),
    );
    fonts.font_data.insert(
        BOLD.into(),
        Arc::new(FontData::from_static(include_bytes!("../../../assets/fonts/Cuprum-Bold.ttf"))),
    );
    let fallback = fonts.families.get(&FontFamily::Proportional).cloned().unwrap_or_default();
    let mut regular = vec!["cuprum".to_string()];
    regular.extend(fallback.iter().cloned());
    let mut bold = vec![BOLD.to_string()];
    bold.extend(fallback);
    fonts.families.insert(FontFamily::Proportional, regular);
    fonts.families.insert(FontFamily::Name(BOLD.into()), bold);
    crate::sorter::add_fonts(&mut fonts);
    fonts
}

/// Шрифты и стиль всего приложения.
pub fn install(ctx: &egui::Context) {
    ctx.set_fonts(fonts());
    ctx.set_theme(egui::Theme::Dark);
    let mut style = (*ctx.style_of(egui::Theme::Dark)).clone();
    let mut visuals = Visuals::dark();
    visuals.override_text_color = Some(TEXT_PRIMARY);
    visuals.panel_fill = WINDOW_BACKGROUND;
    visuals.window_fill = PANEL;
    visuals.window_stroke = Stroke::new(1.0, CARD_STROKE);
    visuals.window_corner_radius = CornerRadius::same(6);
    visuals.menu_corner_radius = CornerRadius::same(6);
    visuals.extreme_bg_color = FIELD;
    visuals.faint_bg_color = CARD;
    visuals.slider_trailing_fill = true;
    visuals.widgets.noninteractive.bg_stroke = Stroke::new(1.0, CARD_STROKE);
    visuals.widgets.noninteractive.fg_stroke = Stroke::new(1.0, TEXT_SECONDARY);
    for widget in [&mut visuals.widgets.inactive, &mut visuals.widgets.hovered] {
        widget.weak_bg_fill = FIELD;
        widget.bg_fill = FIELD;
        widget.bg_stroke = Stroke::new(1.0, CARD_STROKE);
        widget.corner_radius = CornerRadius::same(4);
    }
    visuals.widgets.hovered.weak_bg_fill = CARD;
    visuals.widgets.active.corner_radius = CornerRadius::same(4);
    visuals.widgets.open.corner_radius = CornerRadius::same(4);
    style.visuals = visuals;
    style.spacing.item_spacing = egui::vec2(6.0, 4.0);
    style.spacing.button_padding = egui::vec2(8.0, 4.0);
    style.spacing.menu_margin = egui::Margin::same(6);
    style.spacing.interact_size.y = 24.0;
    style.text_styles.insert(egui::TextStyle::Body, regular(14.0));
    style.text_styles.insert(egui::TextStyle::Button, regular(14.0));
    style.text_styles.insert(egui::TextStyle::Small, regular(12.0));
    style.text_styles.insert(egui::TextStyle::Heading, bold(18.0));
    style.text_styles.insert(egui::TextStyle::Monospace, mono(12.5));
    ctx.set_style_of(egui::Theme::Dark, style);
    refresh_accent(ctx);
}

/// Цвета, которые зависят от акцента: выделение текста, наведение.
pub fn refresh_accent(ctx: &egui::Context) {
    let accent = accent();
    ctx.style_mut_of(egui::Theme::Dark, |style| {
        style.visuals.selection.bg_fill = accent.gamma_multiply(0.45);
        style.visuals.selection.stroke = Stroke::new(1.0, accent);
        style.visuals.widgets.hovered.bg_stroke = Stroke::new(1.0, accent.gamma_multiply(0.6));
        style.visuals.widgets.active.bg_stroke = Stroke::new(1.0, accent);
        style.visuals.hyperlink_color = accent;
    });
}
