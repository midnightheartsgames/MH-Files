//! Текстуры значков и эскизов. Ограниченный кэш; всё, чего нет, запрашивается у пула воркеров
//! и рисуется заглушкой, пока не придёт.

use std::collections::HashSet;
use std::num::NonZeroUsize;

use eframe::egui::{self, ColorImage, TextureHandle, TextureOptions};
use lru::LruCache;
use mh_files_core::Entry;
use mh_files_fs::{ImageKey, ImageKind, ImageResult, Workers};
use mh_files_platform::thumbs::has_own_icon;

enum Slot {
    Pending,
    Ready(TextureHandle),
    Failed,
}

pub struct ImageCache {
    icons: LruCache<ImageKey, Slot>,
    thumbnails: LruCache<ImageKey, Slot>,
    wanted: HashSet<ImageKey>,
    pub system_icons: bool,
    pub thumbnails_enabled: bool,
}

/// Значков много и они маленькие; эскизов — до ~100 МБ видеопамяти при 256 точках.
const ICONS: usize = 2048;
const THUMBNAILS: usize = 400;

impl ImageCache {
    pub fn new() -> ImageCache {
        ImageCache {
            icons: LruCache::new(NonZeroUsize::new(ICONS).unwrap()),
            thumbnails: LruCache::new(NonZeroUsize::new(THUMBNAILS).unwrap()),
            wanted: HashSet::new(),
            system_icons: true,
            thumbnails_enabled: true,
        }
    }

    fn cache(&mut self, key: &ImageKey) -> &mut LruCache<ImageKey, Slot> {
        match key.kind {
            ImageKind::Thumbnail => &mut self.thumbnails,
            _ => &mut self.icons,
        }
    }

    /// Текстура, если готова. Нет — заявка воркерам.
    pub fn get(&mut self, workers: &Workers, key: ImageKey) -> Option<TextureHandle> {
        self.wanted.insert(key.clone());
        let cache = self.cache(&key);
        match cache.get(&key) {
            Some(Slot::Ready(texture)) => Some(texture.clone()),
            Some(_) => None,
            None => {
                cache.put(key.clone(), Slot::Pending);
                workers.image(key);
                None
            }
        }
    }

    pub fn on_result(&mut self, ctx: &egui::Context, key: ImageKey, result: ImageResult) {
        let slot = match result {
            ImageResult::Ready(bitmap) => {
                let size = [bitmap.width as usize, bitmap.height as usize];
                if bitmap.rgba.len() != size[0] * size[1] * 4 {
                    Slot::Failed
                } else {
                    let image = ColorImage::from_rgba_unmultiplied(size, &bitmap.rgba);
                    let name = format!("{:?}", key.kind);
                    Slot::Ready(ctx.load_texture(name, image, TextureOptions::LINEAR))
                }
            }
            ImageResult::Failed => Slot::Failed,
            ImageResult::Skipped => {
                self.cache(&key).pop(&key);
                return;
            }
        };
        self.cache(&key).put(key, slot);
    }

    /// Конец кадра: заявки, которые больше не видны, отменяются.
    pub fn end_frame(&mut self, workers: &Workers) {
        let wanted = std::mem::take(&mut self.wanted);
        for cache in [&mut self.icons, &mut self.thumbnails] {
            let stale: Vec<ImageKey> = cache
                .iter()
                .filter(|(key, slot)| matches!(slot, Slot::Pending) && !wanted.contains(*key))
                .map(|(key, _)| key.clone())
                .collect();
            for key in stale {
                cache.pop(&key);
            }
        }
        workers.retain_images(|key| wanted.contains(key));
    }

    /// Значок записи стороной `pixels`. `None` — рисовать свой.
    pub fn icon(&mut self, workers: &Workers, entry: &Entry, pixels: f32) -> Option<TextureHandle> {
        if !self.system_icons {
            return None;
        }
        let size = bucket(pixels);
        let ext = entry.extension();
        let key = if entry.is_dir() {
            ImageKey {
                kind: ImageKind::TypeIcon { ext: String::new(), is_dir: true },
                path: Default::default(),
                size,
                modified: None,
            }
        } else if has_own_icon(&ext) {
            ImageKey {
                kind: ImageKind::FileIcon,
                path: entry.path(),
                size,
                modified: entry.modified,
            }
        } else {
            ImageKey {
                kind: ImageKind::TypeIcon { ext, is_dir: false },
                path: Default::default(),
                size,
                modified: None,
            }
        };
        self.get(workers, key)
    }

    /// Эскиз содержимого, если для типа он бывает.
    pub fn thumbnail(
        &mut self,
        workers: &Workers,
        entry: &Entry,
        pixels: f32,
    ) -> Option<TextureHandle> {
        if !self.thumbnails_enabled || entry.is_dir() || !has_thumbnail(&entry.extension()) {
            return None;
        }
        let key = ImageKey {
            kind: ImageKind::Thumbnail,
            path: entry.path(),
            size: bucket(pixels),
            modified: entry.modified,
        };
        self.get(workers, key)
    }
}

/// Размеры запросов округляются вверх до ступеньки: меньше вариантов — больше попаданий.
fn bucket(pixels: f32) -> u32 {
    const STEPS: [u32; 8] = [16, 24, 32, 48, 64, 96, 128, 256];
    STEPS.into_iter().find(|&step| step as f32 >= pixels).unwrap_or(256)
}

/// Типы, у которых бывает эскиз. В Windows эскизы видео и документов даёт Shell.
fn has_thumbnail(ext: &str) -> bool {
    if mh_files_fs::images::decodable(ext) || mh_files_fs::images::vector(ext) {
        return true;
    }
    cfg!(windows)
        && matches!(
            ext,
            "mp4"
                | "mkv"
                | "avi"
                | "mov"
                | "webm"
                | "wmv"
                | "m4v"
                | "pdf"
                | "psd"
                | "heic"
                | "avif"
                | "jxl"
                | "raw"
                | "cr2"
                | "nef"
                | "arw"
                | "dng"
                | "mp3"
                | "flac"
                | "m4a"
                | "docx"
                | "xlsx"
                | "pptx"
                | "3mf"
                | "stl"
                | "glb"
        )
}

#[cfg(test)]
mod tests {
    #[test]
    fn buckets() {
        assert_eq!(super::bucket(15.0), 16);
        assert_eq!(super::bucket(20.0), 24);
        assert_eq!(super::bucket(1000.0), 256);
    }
}
