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

/// Векторные картинки, которые MH Files рисует сам.
pub fn vector(ext: &str) -> bool {
    ext == "svg"
}

/// Больше SVG не разбирается: дерево XML в памяти занимает во много раз больше файла.
const SVG_LIMIT: u64 = 16 << 20;

/// SVG-файл картинкой: длинная сторона — `max_side` (вектор и увеличивается, и уменьшается).
/// Вторым — размер из самого файла. Файлы больше `limit` байт не читаются.
pub fn render_svg_file(
    path: &Path,
    max_side: u32,
    limit: u64,
) -> Result<(Bitmap, (u32, u32)), String> {
    let size = std::fs::metadata(path).map_err(|e| e.to_string())?.len();
    if size > limit.min(SVG_LIMIT) {
        return Err("слишком большой файл для предпросмотра".into());
    }
    render_svg(&std::fs::read(path).map_err(|e| e.to_string())?, max_side)
}

/// То же для SVG в памяти (из архива).
pub fn render_svg(data: &[u8], max_side: u32) -> Result<(Bitmap, (u32, u32)), String> {
    use resvg::usvg;
    let mut options = usvg::Options { fontdb: svg_fonts(), ..Default::default() };
    // Только встроенные (data:) картинки. Ссылка на файл, тем более на \\сервер\папку, не
    // открывается: просмотр чужого SVG не должен читать диск и ходить в сеть.
    options.image_href_resolver = usvg::ImageHrefResolver {
        resolve_data: usvg::ImageHrefResolver::default_data_resolver(),
        resolve_string: Box::new(|_, _| None),
    };
    let tree = usvg::Tree::from_data(data, &options).map_err(|e| format!("SVG: {e}"))?;
    let (width, height) = (tree.size().width(), tree.size().height());
    let max_side = max_side.max(1);
    let scale = max_side as f32 / width.max(height);
    let pixels_w = ((width * scale).round() as u32).clamp(1, max_side);
    let pixels_h = ((height * scale).round() as u32).clamp(1, max_side);
    let bitmap = rasterize(&tree, pixels_w, pixels_h)?;
    Ok((bitmap, (width.round() as u32, height.round() as u32)))
}

/// Нарисовать разобранный SVG в картинку `width`×`height` (растягивая под неё).
pub(crate) fn rasterize(
    tree: &resvg::usvg::Tree,
    width: u32,
    height: u32,
) -> Result<Bitmap, String> {
    use resvg::tiny_skia;
    let mut pixmap = tiny_skia::Pixmap::new(width, height).ok_or("SVG: пустая картинка")?;
    let transform = tiny_skia::Transform::from_scale(
        width as f32 / tree.size().width(),
        height as f32 / tree.size().height(),
    );
    resvg::render(tree, transform, &mut pixmap.as_mut());
    // tiny-skia отдаёт premultiplied alpha, а картинки здесь — с обычной.
    let mut rgba = pixmap.take();
    for pixel in rgba.as_chunks_mut::<4>().0 {
        let alpha = u16::from(pixel[3]);
        if alpha != 0 && alpha != 255 {
            for channel in &mut pixel[..3] {
                *channel = ((u16::from(*channel) * 255 + alpha / 2) / alpha).min(255) as u8;
            }
        }
    }
    Ok(Bitmap { width, height, rgba })
}

/// Системные шрифты для текста в SVG — один раз на процесс: поток предпросмотра каждый раз
/// новый, а перечислить шрифты Windows — сотни миллисекунд.
fn svg_fonts() -> Arc<resvg::usvg::fontdb::Database> {
    static FONTS: std::sync::OnceLock<Arc<resvg::usvg::fontdb::Database>> =
        std::sync::OnceLock::new();
    FONTS
        .get_or_init(|| {
            let mut fonts = resvg::usvg::fontdb::Database::new();
            fonts.load_system_fonts();
            Arc::new(fonts)
        })
        .clone()
}

fn load(key: &ImageKey) -> Result<Bitmap, String> {
    match &key.kind {
        ImageKind::TypeIcon { ext, is_dir } => thumbs::type_icon(ext, *is_dir, key.size),
        ImageKind::FileIcon => thumbs::shell_image(&key.path, key.size, ImageMode::Icon),
        ImageKind::Thumbnail => {
            let ext = mh_files_core::entry::extension_of(
                &key.path.file_name().unwrap_or_default().to_string_lossy(),
            );
            // SVG рисуем сами: у Windows эскизов SVG нет без PowerToys. Большие файлы держали
            // бы пул эскизов — им значок типа.
            if vector(&ext) {
                return render_svg_file(&key.path, key.size, 2 << 20).map(|(bitmap, _)| bitmap);
            }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn svg_renders_to_fit_and_keeps_its_size() {
        let svg = br##"<svg xmlns="http://www.w3.org/2000/svg" width="20" height="10">
            <rect width="20" height="10" fill="#ff0000" fill-opacity="0.5"/></svg>"##;
        let (bitmap, size) = render_svg(svg, 256).unwrap();
        assert_eq!(size, (20, 10));
        assert_eq!((bitmap.width, bitmap.height), (256, 128));
        // Полупрозрачный красный — с обычной альфой, не premultiplied.
        let pixel = &bitmap.rgba[..4];
        assert!(pixel[0] >= 250 && pixel[1] == 0 && (120..=135).contains(&pixel[3]), "{pixel:?}");
    }

    #[test]
    fn broken_svg_is_an_error() {
        assert!(render_svg(b"<svg", 64).is_err());
        assert!(render_svg(b"plain text", 64).is_err());
    }

    #[test]
    fn svg_does_not_open_linked_files() {
        // Красная картинка рядом — по ссылке из SVG она не открывается.
        let png = std::env::temp_dir().join(format!("mh-files-svg-{}.png", std::process::id()));
        image::RgbaImage::from_pixel(10, 10, image::Rgba([255, 0, 0, 255])).save(&png).unwrap();
        let svg = format!(
            r#"<svg xmlns="http://www.w3.org/2000/svg" width="10" height="10">
            <image width="10" height="10" href="{}"/></svg>"#,
            png.display()
        );
        let (bitmap, _) = render_svg(svg.as_bytes(), 10).unwrap();
        std::fs::remove_file(&png).unwrap();
        assert!(bitmap.rgba.iter().all(|&b| b == 0));
    }
}
