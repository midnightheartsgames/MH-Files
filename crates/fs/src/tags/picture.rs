//! Картинка файла для теггера: квадрат `side`×`side`, сама картинка — по центру на белом,
//! как у SmilingWolf/wd-tagger (прозрачное — тоже на белом).

use std::path::Path;

use mh_files_core::category::Category;
use mh_files_platform::thumbs::{Bitmap, ImageMode};

/// Картинки меньше этого по большей стороне — значки: теги у них бессмысленные.
pub const MIN_SIDE: u32 = 96;

/// Можно ли что-то сказать о файле с таким расширением: картинки и видео (кадр эскиза
/// Windows).
pub fn taggable(ext: &str) -> bool {
    Category::Images.extensions().contains(&ext) || Category::Video.extensions().contains(&ext)
}

pub fn is_video(ext: &str) -> bool {
    Category::Video.extensions().contains(&ext)
}

/// Кадры видео для модели — RGB `side`×`side`: каждые `step` секунд, не больше `max`
/// ([`mh_files_core::tags::frame_times`]). Media Foundation не открыла файл (нет кодека) —
/// один кадр эскиза Windows, как до 1.3.3.
pub fn video_frames(path: &Path, side: usize, step: f64, max: usize) -> Vec<Vec<u8>> {
    let mut frames = Vec::new();
    if let Ok(mut reader) = mh_files_platform::decode::VideoReader::open(path) {
        let duration = reader.duration().unwrap_or(0.0);
        for at in mh_files_core::tags::frame_times(duration, step, max) {
            if let Some(rgb) = reader.frame(at).ok().as_ref().and_then(|b| square(b, side)) {
                frames.push(rgb);
            }
        }
    }
    if frames.is_empty()
        && let Ok(bitmap) =
            mh_files_platform::thumbs::shell_image(path, side as u32, ImageMode::Thumbnail)
        && let Some(rgb) = square(&bitmap, side)
    {
        frames.push(rgb);
    }
    frames
}

/// Картинка файла в RGB для модели. `None` — не картинка, не читается или значок.
pub fn load(path: &Path, ext: &str, side: usize) -> Option<Vec<u8>> {
    let side32 = side as u32;
    // Размер — по заголовку: уменьшенная картинка о значке уже не скажет.
    if crate::images::dimensions(path).is_some_and(|(w, h)| w.max(h) < MIN_SIDE) {
        return None;
    }
    let bitmap = if Category::Video.extensions().contains(&ext) {
        // Кадр видео — эскиз Windows: там, где для типа есть обработчик эскизов.
        mh_files_platform::thumbs::shell_image(path, side32, ImageMode::Thumbnail).ok()
    } else if crate::images::vector(ext) {
        crate::images::render_svg_file(path, side32, 4 << 20).ok().map(|(bitmap, _)| bitmap)
    } else if crate::images::decodable(ext) {
        crate::images::decode_scaled(path, side32, u64::MAX).ok()
    } else {
        None
    };
    // HEIC, RAW, PSD — через эскизы Windows, если кодеки стоят.
    let bitmap = bitmap.or_else(|| {
        mh_files_platform::thumbs::shell_image(path, side32, ImageMode::Thumbnail).ok()
    })?;
    if bitmap.width.max(bitmap.height) < MIN_SIDE.min(side32) {
        return None;
    }
    square(&bitmap, side)
}

/// По центру на белом квадрате и к стороне `side` (бикубически, как PIL у WD Tagger).
pub fn square(bitmap: &Bitmap, side: usize) -> Option<Vec<u8>> {
    let (width, height) = (bitmap.width, bitmap.height);
    let rgba = image::RgbaImage::from_raw(width, height, bitmap.rgba.clone())?;
    let edge = width.max(height);
    if edge == 0 || side == 0 {
        return None;
    }
    let mut canvas = image::RgbaImage::from_pixel(edge, edge, image::Rgba([255, 255, 255, 255]));
    image::imageops::overlay(
        &mut canvas,
        &rgba,
        i64::from((edge - width) / 2),
        i64::from((edge - height) / 2),
    );
    let canvas = if edge as usize == side {
        canvas
    } else {
        image::imageops::resize(
            &canvas,
            side as u32,
            side as u32,
            image::imageops::FilterType::CatmullRom,
        )
    };
    Some(canvas.pixels().flat_map(|p| [p[0], p[1], p[2]]).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pads_to_a_white_square() {
        // Красная полоса 4×2, верхняя строка полупрозрачная.
        let mut rgba = Vec::new();
        for y in 0..2 {
            for _ in 0..4 {
                rgba.extend_from_slice(&[200, 0, 0, if y == 0 { 0 } else { 255 }]);
            }
        }
        let rgb = square(&Bitmap { width: 4, height: 2, rgba }, 4).unwrap();
        assert_eq!(rgb.len(), 4 * 4 * 3);
        let pixel = |x: usize, y: usize| &rgb[(y * 4 + x) * 3..(y * 4 + x) * 3 + 3];
        assert_eq!(pixel(0, 0), &[255, 255, 255], "поле сверху");
        assert_eq!(pixel(0, 1), &[255, 255, 255], "прозрачное — белое");
        assert_eq!(pixel(2, 2), &[200, 0, 0]);
        assert_eq!(pixel(3, 3), &[255, 255, 255], "поле снизу");
        let scaled = square(&Bitmap { width: 1, height: 1, rgba: vec![0, 0, 0, 255] }, 8).unwrap();
        assert_eq!(scaled.len(), 8 * 8 * 3);
        assert!(square(&Bitmap { width: 2, height: 2, rgba: vec![0; 3] }, 4).is_none());
    }

    #[test]
    fn reads_pictures_and_skips_icons() {
        let dir = tempfile::tempdir().unwrap();
        let photo = dir.path().join("кот.png");
        image::RgbImage::from_pixel(300, 150, image::Rgb([10, 200, 30])).save(&photo).unwrap();
        let rgb = load(&photo, "png", 64).unwrap();
        assert_eq!(rgb.len(), 64 * 64 * 3);
        assert_eq!(&rgb[(32 * 64 + 32) * 3..(32 * 64 + 32) * 3 + 3], &[10, 200, 30]);
        let icon = dir.path().join("значок.png");
        image::RgbImage::from_pixel(32, 32, image::Rgb([0, 0, 0])).save(&icon).unwrap();
        assert!(load(&icon, "png", 64).is_none());
        assert!(taggable("jpg") && taggable("mp4") && !taggable("txt"));
    }
}
