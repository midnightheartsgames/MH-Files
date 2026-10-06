//! Значки и эскизы: пул потоков с очередью «последний первым» — сначала то, что видно сейчас.
//!
//! UI никогда не ждёт картинку: просит её и рисует заглушку, пока не придёт ответ.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::SystemTime;

use crossbeam_channel::Sender;
use mh_files_platform::Waker;
use mh_files_platform::thumbs::{self, Bitmap, ImageMode};
use parking_lot::{Condvar, Mutex};

use crate::Event;

const THREADS: usize = 3;
/// Длиннее очередь не бывает: старые заявки выбрасываются, UI попросит снова, если нужно.
const QUEUE_LIMIT: usize = 512;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum ImageKind {
    /// Значок типа по расширению: один на все файлы этого типа.
    TypeIcon {
        ext: String,
        is_dir: bool,
    },
    /// Свой значок файла (exe, lnk, ico).
    FileIcon,
    Thumbnail,
}

/// Что просят. Время изменения в ключе: изменённый файл получит новый эскиз.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ImageKey {
    pub kind: ImageKind,
    /// Пусто у значков типа.
    pub path: PathBuf,
    pub size: u32,
    pub modified: Option<SystemTime>,
}

#[derive(Debug)]
pub enum ImageResult {
    Ready(Bitmap),
    /// Картинки нет и не будет (нет обработчика) — рисовать заглушку и не просить снова.
    Failed,
    /// Заявка выброшена из очереди — можно попросить снова.
    Skipped,
}

#[derive(Clone)]
pub(crate) struct Pool {
    shared: Arc<(Mutex<VecDeque<ImageKey>>, Condvar)>,
}

impl Pool {
    pub fn new(tx: Sender<Event>, waker: Waker) -> Pool {
        let shared = Arc::new((Mutex::new(VecDeque::new()), Condvar::new()));
        for i in 0..THREADS {
            let shared = shared.clone();
            let tx = tx.clone();
            let waker = waker.clone();
            let _ = std::thread::Builder::new().name(format!("images-{i}")).spawn(move || {
                let _com = thumbs::init_worker_thread();
                loop {
                    let key = {
                        let (queue, ready) = &*shared;
                        let mut queue = queue.lock();
                        loop {
                            if let Some(key) = queue.pop_back() {
                                break key;
                            }
                            ready.wait(&mut queue);
                        }
                    };
                    let result = match load(&key) {
                        Ok(bitmap) => ImageResult::Ready(bitmap),
                        Err(_) => ImageResult::Failed,
                    };
                    if tx.send(Event::Image { key, result }).is_err() {
                        return;
                    }
                    waker();
                }
            });
        }
        let _ = tx;
        Pool { shared }
    }

    pub fn request(&self, key: ImageKey) {
        let (queue, ready) = &*self.shared;
        let mut queue = queue.lock();
        if queue.contains(&key) {
            return;
        }
        queue.push_back(key);
        if queue.len() > QUEUE_LIMIT {
            // Самые старые заявки — то, что давно ушло с экрана.
            queue.pop_front();
        }
        ready.notify_one();
    }

    pub fn retain(&self, keep: impl Fn(&ImageKey) -> bool) {
        self.shared.0.lock().retain(|key| keep(key));
    }
}

/// Расширения, которые умеет декодировать сам MH Files.
pub fn decodable(ext: &str) -> bool {
    matches!(ext, "png" | "jpg" | "jpeg" | "jfif" | "gif" | "bmp" | "ico" | "webp" | "tif" | "tiff")
}

fn load(key: &ImageKey) -> Result<Bitmap, String> {
    match &key.kind {
        ImageKind::TypeIcon { ext, is_dir } => thumbs::type_icon(ext, *is_dir, key.size),
        ImageKind::FileIcon => thumbs::shell_image(&key.path, key.size, ImageMode::Icon),
        ImageKind::Thumbnail => {
            let ext = mh_files_core::entry::extension_of(
                &key.path.file_name().unwrap_or_default().to_string_lossy(),
            );
            // Shell знает больше форматов (видео, PDF, PSD с кодеками) и держит кэш эскизов.
            thumbs::shell_image(&key.path, key.size, ImageMode::Thumbnail).or_else(|error| {
                if decodable(&ext) {
                    decode_scaled(&key.path, key.size, u64::MAX)
                } else {
                    Err(error)
                }
            })
        }
    }
}

/// Декодирует картинку и уменьшает до `max_side`. Файлы больше `limit` байт не читаются.
pub fn decode_scaled(path: &Path, max_side: u32, limit: u64) -> Result<Bitmap, String> {
    let size = std::fs::metadata(path).map_err(|e| e.to_string())?.len();
    if size > limit {
        return Err("слишком большой файл для предпросмотра".into());
    }
    let reader = image::ImageReader::open(path)
        .and_then(|reader| reader.with_guessed_format())
        .map_err(|e| e.to_string())?;
    let image = reader.decode().map_err(|e| e.to_string())?;
    let image = if image.width() > max_side || image.height() > max_side {
        image.thumbnail(max_side, max_side)
    } else {
        image
    };
    let rgba = image.to_rgba8();
    Ok(Bitmap { width: rgba.width(), height: rgba.height(), rgba: rgba.into_raw() })
}

/// То же для картинки в памяти (из архива, страница PDF).
pub fn decode_bytes(bytes: &[u8], max_side: u32) -> Result<Bitmap, String> {
    let image = image::load_from_memory(bytes).map_err(|e| e.to_string())?;
    let image = if image.width() > max_side || image.height() > max_side {
        image.thumbnail(max_side, max_side)
    } else {
        image
    };
    let rgba = image.to_rgba8();
    Ok(Bitmap { width: rgba.width(), height: rgba.height(), rgba: rgba.into_raw() })
}

/// Размеры картинки по заголовку, без декодирования.
pub fn dimensions(path: &Path) -> Option<(u32, u32)> {
    image::ImageReader::open(path).ok()?.with_guessed_format().ok()?.into_dimensions().ok()
}
