//! Страницы PDF картинкой — для просмотра в Инспекторе и по пробелу.

use std::path::Path;

/// Сторона картинки не меньше и не больше этого: крошечная бесполезна, огромная съест память.
const SIDE_RANGE: (u32, u32) = (16, 8192);

/// Страница PDF как PNG (стороной до `max_side` пикселей) и число страниц. Встроенный в
/// Windows 10/11 движок `Windows.Data.Pdf` — без сторонних библиотек.
/// Блокирующая, из любого фонового потока.
///
/// `page` — номер с нуля; номер за последней страницей даёт последнюю.
pub fn render_page(path: &Path, page: u32, max_side: u32) -> Result<(Vec<u8>, u32), String> {
    #[cfg(windows)]
    {
        crate::win::pdf::render_page(path, page, max_side)
    }
    #[cfg(not(windows))]
    {
        let _ = (path, page, max_side);
        Err("просмотр PDF есть только в Windows".into())
    }
}

/// Размер картинки для страницы `width × height` (в любых единицах): пропорции сохраняются,
/// длинная сторона равна `max_side` (в пределах [`SIDE_RANGE`]), короткая — не меньше точки.
#[cfg_attr(not(windows), allow(dead_code))]
pub(crate) fn fit(width: f32, height: f32, max_side: u32) -> (u32, u32) {
    let side = max_side.clamp(SIDE_RANGE.0, SIDE_RANGE.1) as f32;
    // Битый размер страницы (ноль, NaN) — считаем её квадратной.
    let sane = |v: f32| if v.is_finite() && v > 0.0 { v } else { 1.0 };
    let (width, height) = (sane(width), sane(height));
    let scale = side / width.max(height);
    let px = |v: f32| ((v * scale).round() as u32).max(1);
    (px(width), px(height))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fit_keeps_aspect_and_long_side() {
        // A4 в точках PDF.
        assert_eq!(fit(595.0, 842.0, 1000), (707, 1000));
        assert_eq!(fit(842.0, 595.0, 1000), (1000, 707));
        assert_eq!(fit(100.0, 100.0, 512), (512, 512));
    }

    #[test]
    fn fit_survives_bad_sizes() {
        assert_eq!(fit(0.0, f32::NAN, 300), (300, 300));
        assert_eq!(fit(10_000.0, 1.0, 0), (16, 1));
        assert_eq!(fit(1.0, 1.0, u32::MAX), (8192, 8192));
    }
}
