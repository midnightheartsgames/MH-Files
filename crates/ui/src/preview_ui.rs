//! Показ результата предпросмотра — общий для Инспектора и быстрого просмотра.

use eframe::egui::{
    self, Color32, CornerRadius, RichText, ScrollArea, TextureHandle, TextureOptions, Ui, vec2,
};
use mh_files_core::format;
use mh_files_fs::Preview;

use crate::theme;

/// Картинка предпросмотра — в текстуру сразу по приходу, чтобы не пересоздавать каждый кадр.
pub fn texture_of(ctx: &egui::Context, preview: &Preview, name: &str) -> Option<TextureHandle> {
    let Preview::Image { bitmap, .. } = preview else { return None };
    let size = [bitmap.width as usize, bitmap.height as usize];
    if bitmap.rgba.len() != size[0] * size[1] * 4 {
        return None;
    }
    let image = egui::ColorImage::from_rgba_unmultiplied(size, &bitmap.rgba);
    Some(ctx.load_texture(name, image, TextureOptions::LINEAR))
}

/// Рисует предпросмотр в доступной области. `max_height` ограничивает картинку и текст.
pub fn show(
    ui: &mut Ui,
    preview: Option<&Preview>,
    texture: Option<&TextureHandle>,
    max_height: f32,
    id: &str,
) {
    let width = ui.available_width();
    match preview {
        None => {
            ui.allocate_ui(vec2(width, 60.0), |ui| {
                ui.centered_and_justified(|ui| ui.add(egui::Spinner::new().color(theme::accent())));
            });
        }
        Some(Preview::Image { dimensions, .. }) => {
            if let Some(texture) = texture {
                let size = texture.size_vec2();
                let scale = (width / size.x).min(max_height / size.y).min(4.0);
                let shown = size * scale;
                ui.vertical_centered(|ui| {
                    let (rect, _) = ui.allocate_exact_size(shown, egui::Sense::hover());
                    checkerboard(ui, rect);
                    ui.painter().image(
                        texture.id(),
                        rect,
                        egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                        Color32::WHITE,
                    );
                });
            }
            if let Some((w, h)) = dimensions {
                ui.add_space(4.0);
                ui.label(RichText::new(format!("{w} × {h}")).color(theme::TEXT_SECONDARY));
            }
        }
        Some(Preview::Text { text, truncated, encoding }) => {
            egui::Frame::new()
                .fill(theme::FIELD)
                .corner_radius(CornerRadius::same(4))
                .inner_margin(egui::Margin::same(8))
                .show(ui, |ui| {
                    ScrollArea::both()
                        .id_salt(id)
                        .max_height(max_height)
                        .auto_shrink([false, true])
                        .show(ui, |ui| {
                            // Очень длинный текст режется: предпросмотр, а не редактор.
                            let shown: String = text.chars().take(200_000).collect();
                            ui.add(
                                egui::Label::new(
                                    RichText::new(shown)
                                        .font(theme::mono(12.5))
                                        .color(theme::TEXT_PRIMARY),
                                )
                                .extend(),
                            );
                        });
                });
            let note = if *truncated {
                format!("{encoding} · показано начало файла")
            } else {
                encoding.to_string()
            };
            ui.label(RichText::new(note).font(theme::regular(12.0)).color(theme::TEXT_DISABLED));
        }
        Some(Preview::Folder { dirs, files, bytes, truncated }) => {
            let more = if *truncated { "больше " } else { "" };
            ui.label(RichText::new(format!("Папок: {more}{dirs}")).color(theme::TEXT_SECONDARY));
            ui.label(RichText::new(format!("Файлов: {more}{files}")).color(theme::TEXT_SECONDARY));
            ui.label(
                RichText::new(format!("Файлы первого уровня: {}", format::size(*bytes)))
                    .color(theme::TEXT_SECONDARY),
            );
        }
        Some(Preview::None(note)) => {
            ui.label(RichText::new(note).color(theme::TEXT_DISABLED));
        }
        Some(Preview::Error(error)) => {
            ui.add(egui::Label::new(RichText::new(error).color(theme::WARN)).wrap());
        }
    }
}

/// Шахматка под картинками с прозрачностью.
fn checkerboard(ui: &Ui, rect: egui::Rect) {
    let painter = ui.painter_at(rect);
    let cell = 8.0;
    let dark = Color32::from_rgb(0x1A, 0x1F, 0x27);
    let light = Color32::from_rgb(0x22, 0x28, 0x32);
    painter.rect_filled(rect, CornerRadius::ZERO, dark);
    let (cols, rows) = ((rect.width() / cell).ceil() as i32, (rect.height() / cell).ceil() as i32);
    if cols * rows > 40_000 {
        return;
    }
    for y in 0..rows {
        for x in 0..cols {
            if (x + y) % 2 == 0 {
                let min = rect.min + vec2(x as f32 * cell, y as f32 * cell);
                painter.rect_filled(
                    egui::Rect::from_min_size(min, vec2(cell, cell)),
                    CornerRadius::ZERO,
                    light,
                );
            }
        }
    }
}
