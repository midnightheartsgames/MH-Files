//! Звук и кадры видео кусками: отрезки звука для автотегов, кадры видео для автотегов и
//! превью над полоской быстрого просмотра. В Windows — Source Reader Media Foundation (те же
//! кодеки, что у проигрывателя); вне Windows — ошибка (звук WAV читает `fs`).
//!
//! Читатели блокирующие — только из фоновых потоков, где COM уже готов
//! (`thumbs::init_worker_thread`).

use std::path::Path;

use crate::thumbs::Bitmap;

/// Звук: моно, `rate` отсчётов в секунду, −1..1.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Sound {
    pub rate: u32,
    pub samples: Vec<f32>,
}

/// Звук файла: открыть один раз, читать отрезки.
pub struct AudioReader {
    #[cfg(windows)]
    inner: crate::win::decode::AudioReader,
}

impl AudioReader {
    pub fn open(path: &Path) -> Result<AudioReader, String> {
        #[cfg(windows)]
        {
            crate::win::decode::AudioReader::open(path).map(|inner| AudioReader { inner })
        }
        #[cfg(not(windows))]
        {
            let _ = path;
            Err("звук читается только в Windows".into())
        }
    }

    /// Длительность, секунды; `None` — неизвестна.
    pub fn duration(&self) -> Option<f64> {
        #[cfg(windows)]
        {
            self.inner.duration()
        }
        #[cfg(not(windows))]
        {
            None
        }
    }

    /// `seconds` секунд звука с секунды `start` (меньше — файл кончился).
    pub fn clip(&mut self, start: f64, seconds: f64) -> Result<Sound, String> {
        #[cfg(windows)]
        {
            self.inner.clip(start, seconds)
        }
        #[cfg(not(windows))]
        {
            let _ = (start, seconds);
            Err("звук читается только в Windows".into())
        }
    }
}

/// Кадры видео: открыть один раз, брать кадры по времени.
pub struct VideoReader {
    #[cfg(windows)]
    inner: crate::win::decode::VideoReader,
}

impl VideoReader {
    pub fn open(path: &Path) -> Result<VideoReader, String> {
        #[cfg(windows)]
        {
            crate::win::decode::VideoReader::open(path).map(|inner| VideoReader { inner })
        }
        #[cfg(not(windows))]
        {
            let _ = path;
            Err("кадры видео читаются только в Windows".into())
        }
    }

    /// Длительность, секунды; `None` — неизвестна.
    pub fn duration(&self) -> Option<f64> {
        #[cfg(windows)]
        {
            self.inner.duration()
        }
        #[cfg(not(windows))]
        {
            None
        }
    }

    /// Кадр около секунды `at` в полном размере (уменьшает вызывающий), уже повёрнутый, как
    /// его показывают.
    pub fn frame(&mut self, at: f64) -> Result<Bitmap, String> {
        #[cfg(windows)]
        {
            self.inner.frame(at)
        }
        #[cfg(not(windows))]
        {
            let _ = at;
            Err("кадры видео читаются только в Windows".into())
        }
    }
}

/// Повернуть по часовой на 90, 180 или 270 градусов.
#[cfg_attr(not(windows), allow(dead_code))]
pub(crate) fn rotate(bitmap: Bitmap, degrees: u32) -> Bitmap {
    let (w, h) = (bitmap.width as usize, bitmap.height as usize);
    let pixel = |x: usize, y: usize| &bitmap.rgba[(y * w + x) * 4..(y * w + x) * 4 + 4];
    match degrees {
        90 | 270 => {
            let mut rgba = Vec::with_capacity(bitmap.rgba.len());
            for y in 0..w {
                for x in 0..h {
                    // По часовой: новая (x, y) — старая (y, h−1−x); против — (w−1−y, x).
                    let source =
                        if degrees == 90 { pixel(y, h - 1 - x) } else { pixel(w - 1 - y, x) };
                    rgba.extend_from_slice(source);
                }
            }
            Bitmap { width: bitmap.height, height: bitmap.width, rgba }
        }
        180 => {
            let mut rgba = Vec::with_capacity(bitmap.rgba.len());
            for y in (0..h).rev() {
                for x in (0..w).rev() {
                    rgba.extend_from_slice(pixel(x, y));
                }
            }
            Bitmap { width: bitmap.width, height: bitmap.height, rgba }
        }
        _ => bitmap,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rotations() {
        // 2×1: красный, зелёный.
        let bitmap = Bitmap { width: 2, height: 1, rgba: vec![255, 0, 0, 255, 0, 255, 0, 255] };
        let cw = rotate(bitmap.clone(), 90);
        assert_eq!((cw.width, cw.height), (1, 2));
        assert_eq!(&cw.rgba[..4], &[255, 0, 0, 255], "по часовой: левый — наверху");
        let ccw = rotate(bitmap.clone(), 270);
        assert_eq!(&ccw.rgba[..4], &[0, 255, 0, 255]);
        let flip = rotate(bitmap.clone(), 180);
        assert_eq!(&flip.rgba[..4], &[0, 255, 0, 255]);
        assert_eq!(rotate(bitmap.clone(), 0), bitmap);
    }
}
