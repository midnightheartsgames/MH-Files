//! Значки, нарисованные линиями: чёткие на любом масштабе и в одном стиле с MH Monitoring.
//! Каждая функция рисует в квадрате `rect` цветом `color`.

use eframe::egui::{Color32, CornerRadius, Painter, Pos2, Rect, Shape, Stroke, pos2, vec2};

use crate::theme;

fn stroke(rect: Rect, color: Color32) -> Stroke {
    Stroke::new((rect.width() / 9.0).clamp(1.2, 2.2), color)
}

fn p(rect: Rect, x: f32, y: f32) -> Pos2 {
    pos2(rect.left() + rect.width() * x, rect.top() + rect.height() * y)
}

fn polyline(painter: &Painter, rect: Rect, color: Color32, points: &[(f32, f32)]) {
    let points: Vec<Pos2> = points.iter().map(|&(x, y)| p(rect, x, y)).collect();
    painter.add(Shape::line(points, stroke(rect, color)));
}

pub fn arrow_left(painter: &Painter, rect: Rect, color: Color32) {
    polyline(painter, rect, color, &[(0.55, 0.15), (0.2, 0.5), (0.55, 0.85)]);
    polyline(painter, rect, color, &[(0.2, 0.5), (0.9, 0.5)]);
}

pub fn arrow_right(painter: &Painter, rect: Rect, color: Color32) {
    polyline(painter, rect, color, &[(0.45, 0.15), (0.8, 0.5), (0.45, 0.85)]);
    polyline(painter, rect, color, &[(0.1, 0.5), (0.8, 0.5)]);
}

pub fn arrow_up(painter: &Painter, rect: Rect, color: Color32) {
    polyline(painter, rect, color, &[(0.15, 0.55), (0.5, 0.2), (0.85, 0.55)]);
    polyline(painter, rect, color, &[(0.5, 0.2), (0.5, 0.9)]);
}

pub fn chevron_right(painter: &Painter, rect: Rect, color: Color32) {
    polyline(painter, rect, color, &[(0.35, 0.2), (0.65, 0.5), (0.35, 0.8)]);
}

pub fn chevron_down(painter: &Painter, rect: Rect, color: Color32) {
    polyline(painter, rect, color, &[(0.2, 0.35), (0.5, 0.65), (0.8, 0.35)]);
}

pub fn refresh(painter: &Painter, rect: Rect, color: Color32) {
    let center = rect.center();
    let radius = rect.width() * 0.36;
    let points: Vec<Pos2> = (0..=20)
        .map(|i| {
            let angle = -0.6 + i as f32 / 20.0 * 4.9;
            center + vec2(angle.cos(), angle.sin()) * radius
        })
        .collect();
    let end = *points.last().unwrap();
    painter.add(Shape::line(points, stroke(rect, color)));
    let s = rect.width() * 0.2;
    painter.add(Shape::line(
        vec![end + vec2(-s, -s * 0.2), end, end + vec2(s * 0.3, -s)],
        stroke(rect, color),
    ));
}

pub fn plus(painter: &Painter, rect: Rect, color: Color32) {
    polyline(painter, rect, color, &[(0.5, 0.15), (0.5, 0.85)]);
    polyline(painter, rect, color, &[(0.15, 0.5), (0.85, 0.5)]);
}

pub fn close(painter: &Painter, rect: Rect, color: Color32) {
    polyline(painter, rect, color, &[(0.2, 0.2), (0.8, 0.8)]);
    polyline(painter, rect, color, &[(0.8, 0.2), (0.2, 0.8)]);
}

pub fn search(painter: &Painter, rect: Rect, color: Color32) {
    painter.circle_stroke(p(rect, 0.42, 0.42), rect.width() * 0.28, stroke(rect, color));
    polyline(painter, rect, color, &[(0.63, 0.63), (0.9, 0.9)]);
}

pub fn dots(painter: &Painter, rect: Rect, color: Color32) {
    for x in [0.2, 0.5, 0.8] {
        painter.circle_filled(p(rect, x, 0.5), rect.width() * 0.08, color);
    }
}

pub fn list(painter: &Painter, rect: Rect, color: Color32) {
    for y in [0.22, 0.5, 0.78] {
        painter.circle_filled(p(rect, 0.12, y), rect.width() * 0.07, color);
        polyline(painter, rect, color, &[(0.3, y), (0.92, y)]);
    }
}

pub fn grid(painter: &Painter, rect: Rect, color: Color32) {
    let s = rect.width() * 0.4;
    for (x, y) in [(0.05, 0.05), (0.55, 0.05), (0.05, 0.55), (0.55, 0.55)] {
        let cell = Rect::from_min_size(p(rect, x, y), vec2(s, s));
        painter.rect_stroke(cell, CornerRadius::same(2), stroke(rect, color), egui_inside());
    }
}

/// Колонки Миллера: три столбца, последний — со стрелкой вглубь.
pub fn columns(painter: &Painter, rect: Rect, color: Color32) {
    let frame = Rect::from_min_max(p(rect, 0.04, 0.12), p(rect, 0.96, 0.88));
    painter.rect_stroke(frame, CornerRadius::same(2), stroke(rect, color), egui_inside());
    for x in [0.35, 0.65] {
        polyline(painter, rect, color, &[(x, 0.14), (x, 0.86)]);
    }
    polyline(painter, rect, color, &[(0.76, 0.38), (0.86, 0.5), (0.76, 0.62)]);
}

/// Архив: коробка с «молнией».
pub fn archive(painter: &Painter, rect: Rect, color: Color32) {
    let body = Rect::from_min_max(p(rect, 0.14, 0.18), p(rect, 0.86, 0.9));
    painter.rect_stroke(body, CornerRadius::same(2), stroke(rect, color), egui_inside());
    for (i, y) in [0.28, 0.42, 0.56].into_iter().enumerate() {
        let x = if i % 2 == 0 { 0.44 } else { 0.56 };
        polyline(painter, rect, color, &[(x, y), (x, y + 0.08)]);
    }
    let lock = Rect::from_min_max(p(rect, 0.4, 0.66), p(rect, 0.6, 0.8));
    painter.rect_stroke(lock, CornerRadius::same(1), stroke(rect, color), egui_inside());
}

/// Сортировщик: три полки разной длины, как разложенные по папкам файлы.
pub fn sort(painter: &Painter, rect: Rect, color: Color32) {
    for (y, end) in [(0.25, 0.9), (0.5, 0.7), (0.75, 0.5)] {
        polyline(painter, rect, color, &[(0.1, y), (end, y)]);
    }
    polyline(painter, rect, color, &[(0.72, 0.62), (0.86, 0.76), (0.72, 0.9)]);
}

/// Дубликаты: два листа со сдвигом.
pub fn duplicates(painter: &Painter, rect: Rect, color: Color32) {
    let back = Rect::from_min_max(p(rect, 0.3, 0.08), p(rect, 0.9, 0.7));
    let front = Rect::from_min_max(p(rect, 0.1, 0.3), p(rect, 0.7, 0.92));
    painter.rect_stroke(back, CornerRadius::same(2), stroke(rect, color), egui_inside());
    painter.rect_filled(front, CornerRadius::same(2), theme::PANEL);
    painter.rect_stroke(front, CornerRadius::same(2), stroke(rect, color), egui_inside());
}

fn egui_inside() -> eframe::egui::StrokeKind {
    eframe::egui::StrokeKind::Inside
}

pub fn inspector(painter: &Painter, rect: Rect, color: Color32) {
    painter.rect_stroke(rect, CornerRadius::same(2), stroke(rect, color), egui_inside());
    polyline(painter, rect, color, &[(0.65, 0.05), (0.65, 0.95)]);
}

pub fn filter(painter: &Painter, rect: Rect, color: Color32) {
    polyline(
        painter,
        rect,
        color,
        &[
            (0.1, 0.15),
            (0.9, 0.15),
            (0.58, 0.52),
            (0.58, 0.85),
            (0.42, 0.75),
            (0.42, 0.52),
            (0.1, 0.15),
        ],
    );
}

/// Папка: залитая, цвета акцента, как в остальных программах MH.
pub fn folder(painter: &Painter, rect: Rect, color: Color32) {
    let h = rect.height();
    let body = Rect::from_min_max(p(rect, 0.05, 0.28), p(rect, 0.95, 0.85));
    let tab = Rect::from_min_max(p(rect, 0.05, 0.16), p(rect, 0.45, 0.34));
    let radius = CornerRadius::same((h * 0.08).max(1.0) as u8);
    painter.rect_filled(tab, radius, color.gamma_multiply(0.75));
    painter.rect_filled(body, radius, color);
}

/// Лист с загнутым углом и цветной меткой типа.
pub fn file(painter: &Painter, rect: Rect, ext: &str) {
    let color = category_color(ext);
    let left = rect.left() + rect.width() * 0.18;
    let right = rect.right() - rect.width() * 0.18;
    let top = rect.top() + rect.height() * 0.06;
    let bottom = rect.bottom() - rect.height() * 0.06;
    let fold = rect.width() * 0.22;
    let outline = vec![
        pos2(left, top),
        pos2(right - fold, top),
        pos2(right, top + fold),
        pos2(right, bottom),
        pos2(left, bottom),
    ];
    painter.add(Shape::convex_polygon(outline.clone(), theme::CARD, Stroke::NONE));
    painter.add(Shape::closed_line(outline, Stroke::new(1.0, theme::TEXT_SECONDARY)));
    painter.add(Shape::line(
        vec![pos2(right - fold, top), pos2(right - fold, top + fold), pos2(right, top + fold)],
        Stroke::new(1.0, theme::TEXT_SECONDARY),
    ));
    let band = Rect::from_min_max(
        pos2(left + 1.0, bottom - rect.height() * 0.3),
        pos2(right - 1.0, bottom - rect.height() * 0.12),
    );
    painter.rect_filled(band, CornerRadius::same(1), color);
}

pub fn drive(painter: &Painter, rect: Rect, color: Color32) {
    let body = Rect::from_min_max(p(rect, 0.05, 0.3), p(rect, 0.95, 0.75));
    painter.rect_stroke(body, CornerRadius::same(2), stroke(rect, color), egui_inside());
    painter.circle_filled(p(rect, 0.78, 0.525), rect.width() * 0.06, color);
}

pub fn computer(painter: &Painter, rect: Rect, color: Color32) {
    let screen = Rect::from_min_max(p(rect, 0.05, 0.12), p(rect, 0.95, 0.7));
    painter.rect_stroke(screen, CornerRadius::same(2), stroke(rect, color), egui_inside());
    polyline(painter, rect, color, &[(0.3, 0.9), (0.7, 0.9)]);
    polyline(painter, rect, color, &[(0.5, 0.7), (0.5, 0.9)]);
}

/// Цвет метки файла по типу.
pub fn category_color(ext: &str) -> Color32 {
    match ext {
        "png" | "jpg" | "jpeg" | "gif" | "bmp" | "webp" | "tif" | "tiff" | "ico" | "svg"
        | "psd" | "heic" | "avif" | "raw" | "cr2" | "nef" => Color32::from_rgb(0x5A, 0xA9, 0xE6),
        "mp4" | "mkv" | "avi" | "mov" | "webm" | "wmv" | "flv" | "m4v" => {
            Color32::from_rgb(0xB3, 0x6A, 0xE2)
        }
        "mp3" | "flac" | "wav" | "ogg" | "m4a" | "aac" | "opus" | "wma" => {
            Color32::from_rgb(0xE2, 0x6A, 0x9F)
        }
        "zip" | "rar" | "7z" | "tar" | "gz" | "xz" | "bz2" | "zst" | "iso" | "cab" => theme::WARN,
        "exe" | "msi" | "bat" | "cmd" | "ps1" | "com" | "lnk" | "appx" | "msix" => {
            Color32::from_rgb(0x6A, 0xD0, 0x7A)
        }
        "pdf" | "doc" | "docx" | "xls" | "xlsx" | "ppt" | "pptx" | "odt" | "rtf" | "epub" => {
            theme::CRITICAL
        }
        "safetensors" | "gguf" | "ckpt" | "pt" | "pth" | "onnx" | "bin" => {
            Color32::from_rgb(0xD8, 0xC2, 0x3F)
        }
        _ => theme::TEXT_DISABLED,
    }
}
