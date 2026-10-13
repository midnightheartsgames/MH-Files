//! Кадр видео под указателем над полоской перемотки быстрого просмотра: свой поток держит
//! открытым видео (Media Foundation) и отвечает на последний вопрос — старые, пока поток
//! занят, пропускаются. Вне Windows кадров нет.

use std::path::Path;

use crossbeam_channel::{Receiver, Sender, unbounded};
use mh_files_platform::Waker;
use mh_files_platform::thumbs::Bitmap;

/// Кадры одного видео по времени. Поток кончается вместе с ним.
pub struct FramePeek {
    requests: Sender<f64>,
    results: Receiver<(f64, Bitmap)>,
}

impl FramePeek {
    /// `max_side` — сторона кадра в пикселях; `waker` — когда готов новый кадр.
    pub fn open(path: &Path, max_side: u32, waker: Waker) -> FramePeek {
        let (requests, asked) = unbounded::<f64>();
        let (answers, results) = unbounded();
        let path = path.to_path_buf();
        let _ = std::thread::Builder::new()
            .name("frame-peek".into())
            .spawn(move || serve(&path, max_side, &asked, &answers, &waker));
        FramePeek { requests, results }
    }

    /// Нужен кадр около секунды `at`.
    pub fn request(&self, at: f64) {
        let _ = self.requests.send(at);
    }

    /// Готовые кадры: (секунда, о которой спрашивали; кадр).
    pub fn take(&self) -> Vec<(f64, Bitmap)> {
        self.results.try_iter().collect()
    }
}

fn serve(
    path: &Path,
    max_side: u32,
    asked: &Receiver<f64>,
    answers: &Sender<(f64, Bitmap)>,
    waker: &Waker,
) {
    let _com = mh_files_platform::thumbs::init_worker_thread();
    let mut reader = None;
    while let Ok(mut at) = asked.recv() {
        // Указатель ушёл дальше, пока считался прошлый кадр: нужен только последний.
        while let Ok(later) = asked.try_recv() {
            at = later;
        }
        if reader.is_none() {
            match mh_files_platform::decode::VideoReader::open(path) {
                Ok(opened) => reader = Some(opened),
                Err(_) => return,
            }
        }
        let Some(reader) = reader.as_mut() else { return };
        let Ok(frame) = reader.frame(at) else { continue };
        if answers.send((at, shrink(&frame, max_side))).is_err() {
            return;
        }
        waker();
    }
}

/// Уменьшить, чтобы большая сторона была не больше `max_side`.
fn shrink(bitmap: &Bitmap, max_side: u32) -> Bitmap {
    let edge = bitmap.width.max(bitmap.height);
    if edge <= max_side || edge == 0 {
        return bitmap.clone();
    }
    let Some(image) = image::RgbaImage::from_raw(bitmap.width, bitmap.height, bitmap.rgba.clone())
    else {
        return bitmap.clone();
    };
    let scale = f64::from(max_side) / f64::from(edge);
    let width = ((f64::from(bitmap.width) * scale).round() as u32).max(1);
    let height = ((f64::from(bitmap.height) * scale).round() as u32).max(1);
    let small =
        image::imageops::resize(&image, width, height, image::imageops::FilterType::Triangle);
    Bitmap { width, height, rgba: small.into_raw() }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shrinks_frames() {
        let big = Bitmap { width: 400, height: 200, rgba: vec![128; 400 * 200 * 4] };
        let small = shrink(&big, 100);
        assert_eq!((small.width, small.height, small.rgba.len()), (100, 50, 100 * 50 * 4));
        assert_eq!(shrink(&small, 100), small);
    }

    #[test]
    fn no_video_no_frames() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("не видео.mp4");
        std::fs::write(&path, "текст").unwrap();
        let peek = FramePeek::open(&path, 160, mh_files_platform::no_waker());
        peek.request(1.0);
        std::thread::sleep(std::time::Duration::from_millis(200));
        assert!(peek.take().is_empty());
    }
}
