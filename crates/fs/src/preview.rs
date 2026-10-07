//! Предпросмотр для Инспектора и быстрого просмотра (пробел).

use std::io::Read;
use std::path::{Path, PathBuf};

use mh_files_platform::thumbs::{self, Bitmap, ImageMode};
use mh_files_platform::{media, pdf};

use crate::images::{decodable, decode_bytes, decode_scaled, dimensions};
use crate::{CancelToken, archive};

#[derive(Debug, Clone)]
pub struct PreviewRequest {
    pub path: PathBuf,
    pub is_dir: bool,
    /// Сторона картинки, точки.
    pub max_side: u32,
    /// Сколько байт текста читать.
    pub text_limit: usize,
    /// Картинки больше — не декодировать.
    pub image_limit: u64,
    /// Страница многостраничного документа (PDF), с нуля.
    pub page: u32,
}

#[derive(Debug, Clone)]
pub enum Preview {
    Image {
        bitmap: Bitmap,
        /// Исходные размеры, если известны.
        dimensions: Option<(u32, u32)>,
        /// Документ со страницами: показанная страница (с нуля) и сколько всего.
        pages: Option<(u32, u32)>,
        /// Свойства из Windows: длительность, кадр, исполнитель, камера…
        info: Vec<(String, String)>,
    },
    /// Картинки нет, но есть свойства (аудио без обложки, документ).
    Info(Vec<(String, String)>),
    /// Архив: сколько внутри и сколько займёт распакованным.
    Archive {
        dirs: usize,
        files: usize,
        bytes: u64,
    },
    Text {
        text: String,
        truncated: bool,
        encoding: &'static str,
    },
    Folder {
        dirs: usize,
        files: usize,
        /// Сумма размеров файлов первого уровня.
        bytes: u64,
        /// Папок больше, чем просмотрено, — числа неполные.
        truncated: bool,
    },
    /// Показать нечего, но есть пояснение.
    None(String),
    Error(String),
}

/// Сколько записей папки считать для сводки.
const FOLDER_LIMIT: usize = 50_000;

pub fn load(request: &PreviewRequest, cancel: &CancelToken) -> Preview {
    // Записи архива: такого пути на диске нет.
    if std::fs::symlink_metadata(&request.path).is_err()
        && let Some((archive, inner)) = archive::split(&request.path)
    {
        return in_archive(request, &archive, &inner);
    }
    if request.is_dir {
        return folder(&request.path, cancel);
    }
    let name = request.path.file_name().unwrap_or_default().to_string_lossy();
    let ext = mh_files_core::entry::extension_of(&name);
    if archive::is_archive_ext(&ext) {
        return match archive::folder_totals(&request.path, "") {
            Ok((dirs, files, bytes)) => Preview::Archive { dirs, files, bytes },
            Err(error) => Preview::Error(error),
        };
    }
    if decodable(&ext) {
        return match decode_scaled(&request.path, request.max_side, request.image_limit) {
            Ok(bitmap) => Preview::Image {
                bitmap,
                dimensions: dimensions(&request.path),
                pages: None,
                info: info(&request.path, &ext),
            },
            Err(error) => Preview::Error(error),
        };
    }
    if ext == "pdf"
        && let Ok(preview) = pdf_page(request)
    {
        return preview;
    }
    if text_like(&ext) || sniff_text(&request.path) {
        return text(&request.path, request.text_limit);
    }
    // Видео, аудио, документы — эскиз Shell (кадр, обложка, первая страница) и свойства.
    let info = info(&request.path, &ext);
    match thumbs::shell_image(&request.path, request.max_side, ImageMode::Thumbnail) {
        Ok(bitmap) => Preview::Image { bitmap, dimensions: None, pages: None, info },
        Err(_) if !info.is_empty() => Preview::Info(info),
        Err(_) => Preview::None("предпросмотр для этого типа не поддерживается".into()),
    }
}

/// Свойства из Windows — для медиа, фото и документов; у прочего их не спрашиваем: на
/// каждом файле это лишнее обращение к обработчикам свойств.
fn info(path: &Path, ext: &str) -> Vec<(String, String)> {
    if !has_properties(ext) {
        return Vec::new();
    }
    media::media_info(path).unwrap_or_default()
}

pub fn has_properties(ext: &str) -> bool {
    matches!(
        ext,
        "mp4"
            | "mkv"
            | "avi"
            | "mov"
            | "wmv"
            | "webm"
            | "m4v"
            | "mpg"
            | "mpeg"
            | "ts"
            | "flv"
            | "3gp"
            | "mp3"
            | "flac"
            | "wav"
            | "ogg"
            | "opus"
            | "m4a"
            | "aac"
            | "wma"
            | "aiff"
            | "jpg"
            | "jpeg"
            | "heic"
            | "tif"
            | "tiff"
            | "png"
            | "webp"
            | "cr2"
            | "nef"
            | "arw"
            | "dng"
            | "pdf"
            | "docx"
            | "doc"
            | "xlsx"
            | "xls"
            | "pptx"
            | "ppt"
            | "odt"
    )
}

/// Страница PDF встроенным в Windows движком.
fn pdf_page(request: &PreviewRequest) -> Result<Preview, String> {
    let (png, count) = pdf::render_page(&request.path, request.page, request.max_side)?;
    let bitmap = decode_bytes(&png, request.max_side)?;
    let page = request.page.min(count.saturating_sub(1));
    let info = info(&request.path, "pdf");
    Ok(Preview::Image { bitmap, dimensions: None, pages: Some((page, count)), info })
}

/// Предпросмотр записи архива: картинки и текст читаются в память, остальное — после
/// извлечения.
fn in_archive(request: &PreviewRequest, archive: &Path, inner: &str) -> Preview {
    if request.is_dir {
        return match archive::folder_totals(archive, inner) {
            Ok((dirs, files, bytes)) => Preview::Archive { dirs, files, bytes },
            Err(error) => Preview::Error(error),
        };
    }
    let name = request.path.file_name().unwrap_or_default().to_string_lossy();
    let ext = mh_files_core::entry::extension_of(&name);
    // Архив в архиве — сводка, как у обычного.
    if archive::is_archive_ext(&ext) {
        return match archive::folder_totals(&request.path, "") {
            Ok((dirs, files, bytes)) => Preview::Archive { dirs, files, bytes },
            Err(error) => Preview::Error(error),
        };
    }
    if decodable(&ext) {
        return match archive::read(archive, inner, request.image_limit)
            .and_then(|bytes| decode_bytes(&bytes, request.max_side))
        {
            Ok(bitmap) => {
                Preview::Image { bitmap, dimensions: None, pages: None, info: Vec::new() }
            }
            Err(error) => Preview::Error(error),
        };
    }
    if text_like(&ext) {
        let limit = request.text_limit as u64;
        return match archive::read(archive, inner, limit.saturating_mul(64)) {
            Ok(mut bytes) => {
                let truncated = bytes.len() > request.text_limit;
                bytes.truncate(request.text_limit);
                let (text, encoding) = decode_text(&bytes);
                Preview::Text { text, truncated, encoding }
            }
            Err(error) => Preview::Error(error),
        };
    }
    Preview::None("файл в архиве — извлеките его (Ctrl+Shift+E), чтобы посмотреть".into())
}

fn folder(path: &Path, cancel: &CancelToken) -> Preview {
    let entries = match std::fs::read_dir(path) {
        Ok(entries) => entries,
        Err(error) => return Preview::Error(crate::listing::describe(path, &error)),
    };
    let (mut dirs, mut files, mut bytes) = (0, 0, 0);
    for (i, entry) in entries.flatten().enumerate() {
        if i >= FOLDER_LIMIT || cancel.is_cancelled() {
            return Preview::Folder { dirs, files, bytes, truncated: true };
        }
        match entry.metadata() {
            Ok(meta) if meta.is_dir() => dirs += 1,
            Ok(meta) => {
                files += 1;
                bytes += meta.len();
            }
            Err(_) => files += 1,
        }
    }
    Preview::Folder { dirs, files, bytes, truncated: false }
}

/// Расширения, которые точно текст.
pub fn text_like(ext: &str) -> bool {
    matches!(
        ext,
        "txt"
            | "md"
            | "markdown"
            | "log"
            | "ini"
            | "cfg"
            | "conf"
            | "toml"
            | "yaml"
            | "yml"
            | "json"
            | "jsonc"
            | "xml"
            | "csv"
            | "tsv"
            | "rs"
            | "py"
            | "js"
            | "ts"
            | "tsx"
            | "jsx"
            | "kt"
            | "kts"
            | "java"
            | "c"
            | "h"
            | "cpp"
            | "hpp"
            | "cc"
            | "cs"
            | "go"
            | "rb"
            | "php"
            | "lua"
            | "sh"
            | "bat"
            | "cmd"
            | "ps1"
            | "psm1"
            | "sql"
            | "html"
            | "htm"
            | "css"
            | "scss"
            | "less"
            | "vue"
            | "svelte"
            | "gradle"
            | "properties"
            | "gitignore"
            | "gitattributes"
            | "editorconfig"
            | "env"
            | "lock"
            | "svg"
            | "srt"
            | "vtt"
            | "nfo"
            | "reg"
            | "glsl"
            | "hlsl"
            | "wgsl"
            | "swift"
            | "dart"
            | "zig"
    )
}

/// Похоже на текст: в первых килобайтах нет нулевых байтов (кроме UTF-16 с BOM).
fn sniff_text(path: &Path) -> bool {
    let Ok(mut file) = std::fs::File::open(path) else { return false };
    let mut head = [0u8; 4096];
    let Ok(n) = file.read(&mut head) else { return false };
    let head = &head[..n];
    if n == 0 {
        return true;
    }
    if head.starts_with(&[0xFF, 0xFE]) || head.starts_with(&[0xFE, 0xFF]) {
        return true;
    }
    !head.contains(&0)
}

fn text(path: &Path, limit: usize) -> Preview {
    let mut file = match std::fs::File::open(path) {
        Ok(file) => file,
        Err(error) => return Preview::Error(error.to_string()),
    };
    let mut bytes = Vec::with_capacity(limit.min(1 << 20));
    if let Err(error) = (&mut file).take(limit as u64 + 1).read_to_end(&mut bytes) {
        return Preview::Error(error.to_string());
    }
    let truncated = bytes.len() > limit;
    bytes.truncate(limit);
    let (text, encoding) = decode_text(&bytes);
    Preview::Text { text, truncated, encoding }
}

/// UTF-8/UTF-16 по BOM, корректный UTF-8 без BOM, иначе Windows-1251 — самая частая
/// однобайтовая кодировка у русскоязычных файлов.
pub fn decode_text(bytes: &[u8]) -> (String, &'static str) {
    if let Some((encoding, bom)) = encoding_rs::Encoding::for_bom(bytes) {
        let (text, _) = encoding.decode_without_bom_handling(&bytes[bom..]);
        return (text.into_owned(), encoding.name());
    }
    match std::str::from_utf8(bytes) {
        Ok(text) => (text.to_string(), "UTF-8"),
        // Обрезали посреди символа — остальное корректно.
        Err(error) if error.error_len().is_none() => {
            (String::from_utf8_lossy(&bytes[..error.valid_up_to()]).into_owned(), "UTF-8")
        }
        Err(_) => {
            let (text, _, _) = encoding_rs::WINDOWS_1251.decode(bytes);
            (text.into_owned(), "Windows-1251")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_common_encodings() {
        assert_eq!(decode_text("привет".as_bytes()), ("привет".to_string(), "UTF-8"));
        let cp1251 = [0xEF, 0xF0, 0xE8, 0xE2, 0xE5, 0xF2];
        assert_eq!(decode_text(&cp1251), ("привет".to_string(), "Windows-1251"));
        let utf16 = [0xFF, 0xFE, b'h', 0, b'i', 0];
        assert_eq!(decode_text(&utf16).0, "hi");
        let cut = &"жж".as_bytes()[..3];
        assert_eq!(decode_text(cut), ("ж".to_string(), "UTF-8"));
    }

    #[test]
    fn previews_folders_and_text() {
        let dir = std::env::temp_dir().join(format!("mh-files-preview-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("sub")).unwrap();
        std::fs::write(dir.join("a.txt"), "hello world").unwrap();
        std::fs::write(dir.join("noext"), "plain").unwrap();
        let request = |path: PathBuf, is_dir| PreviewRequest {
            path,
            is_dir,
            max_side: 256,
            text_limit: 5,
            image_limit: 1 << 20,
            page: 0,
        };
        let cancel = CancelToken::default();
        match load(&request(dir.clone(), true), &cancel) {
            Preview::Folder { dirs: 1, files: 2, bytes: 16, truncated: false } => {}
            other => panic!("{other:?}"),
        }
        match load(&request(dir.join("a.txt"), false), &cancel) {
            Preview::Text { text, truncated: true, .. } => assert_eq!(text, "hello"),
            other => panic!("{other:?}"),
        }
        assert!(matches!(load(&request(dir.join("noext"), false), &cancel), Preview::Text { .. }));
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
