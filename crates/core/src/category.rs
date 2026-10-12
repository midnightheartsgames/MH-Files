//! Категории поиска: папки, изображения, видео, звук, документы, архивы, программы.
//!
//! Тип узнаётся по расширению имени — так быстро и в обходе папок, и в индексе дисков.

use serde::{Deserialize, Serialize};

use crate::entry::extension_of;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub enum Category {
    #[default]
    All,
    Folders,
    Images,
    Video,
    Audio,
    Documents,
    Archives,
    Programs,
}

const IMAGES: &[&str] = &[
    "jpg", "jpeg", "jfif", "png", "webp", "gif", "bmp", "tif", "tiff", "svg", "ico", "heic",
    "heif", "avif", "raw", "cr2", "cr3", "nef", "arw", "dng", "tga", "exr", "hdr", "dds", "psd",
];
const VIDEO: &[&str] = &[
    "mp4", "m4v", "mov", "mkv", "avi", "wmv", "webm", "mpg", "mpeg", "ts", "mts", "m2ts", "3gp",
    "asf", "flv", "vob",
];
const AUDIO: &[&str] = &[
    "mp3", "m4a", "aac", "wav", "flac", "ogg", "opus", "wma", "aiff", "aif", "ac3", "amr", "ape",
    "mid", "midi",
];
const DOCUMENTS: &[&str] = &[
    "txt", "pdf", "doc", "docx", "odt", "rtf", "md", "xps", "djvu", "epub", "fb2", "mobi", "azw3",
    "xls", "xlsx", "ods", "csv", "tsv", "ppt", "pptx", "odp", "mht", "mhtml",
];
const ARCHIVES: &[&str] = &[
    "zip", "rar", "7z", "tar", "gz", "tgz", "bz2", "xz", "zst", "cab", "iso", "img", "vhd", "vhdx",
];
const PROGRAMS: &[&str] =
    &["exe", "msi", "bat", "cmd", "ps1", "com", "lnk", "appx", "msix", "appxbundle", "msixbundle"];

impl Category {
    pub const ALL: [Category; 8] = [
        Category::All,
        Category::Folders,
        Category::Images,
        Category::Video,
        Category::Audio,
        Category::Documents,
        Category::Archives,
        Category::Programs,
    ];

    pub fn title(self) -> &'static str {
        match self {
            Category::All => "Всё",
            Category::Folders => "Папки",
            Category::Images => "Изображения",
            Category::Video => "Видео",
            Category::Audio => "Аудио",
            Category::Documents => "Документы",
            Category::Archives => "Архивы",
            Category::Programs => "Программы",
        }
    }

    /// Слово для фильтра `kind:` в поиске по дискам.
    pub fn key(self) -> &'static str {
        match self {
            Category::All => "всё",
            Category::Folders => "папки",
            Category::Images => "изображения",
            Category::Video => "видео",
            Category::Audio => "аудио",
            Category::Documents => "документы",
            Category::Archives => "архивы",
            Category::Programs => "программы",
        }
    }

    /// Категория по слову фильтра: русское или английское, полное или короткое.
    pub fn parse(word: &str) -> Option<Category> {
        let category = match word.trim().to_lowercase().as_str() {
            "" | "всё" | "все" | "all" => Category::All,
            "папки" | "папка" | "folders" | "folder" | "dir" => Category::Folders,
            "изображения" | "картинки" | "фото" | "images" | "image" | "pictures" | "photo" => {
                Category::Images
            }
            "видео" | "video" | "videos" => Category::Video,
            "аудио" | "музыка" | "звук" | "audio" | "music" => Category::Audio,
            "документы" | "документ" | "docs" | "documents" | "document" => {
                Category::Documents
            }
            "архивы" | "архив" | "archives" | "archive" => Category::Archives,
            "программы" | "программа" | "programs" | "apps" | "exe" => {
                Category::Programs
            }
            _ => return None,
        };
        Some(category)
    }

    /// Расширения файлов категории; у «Всё» и «Папки» — пусто.
    pub fn extensions(self) -> &'static [&'static str] {
        match self {
            Category::All | Category::Folders => &[],
            Category::Images => IMAGES,
            Category::Video => VIDEO,
            Category::Audio => AUDIO,
            Category::Documents => DOCUMENTS,
            Category::Archives => ARCHIVES,
            Category::Programs => PROGRAMS,
        }
    }

    pub fn matches(self, name: &str, is_dir: bool) -> bool {
        match self {
            Category::All => true,
            Category::Folders => is_dir,
            _ => !is_dir && self.extensions().contains(&extension_of(name).as_str()),
        }
    }
}

/// Запрос поиска по дискам с другой категорией: прежние `kind:` убираются, новая
/// дописывается в конец. Слова в кавычках не трогаются.
pub fn with_kind(text: &str, category: Category) -> String {
    let mut tokens: Vec<String> = raw_tokens(text)
        .into_iter()
        .filter(|token| !token.to_lowercase().starts_with("kind:"))
        .collect();
    if category != Category::All {
        tokens.push(format!("kind:{}", category.key()));
    }
    tokens.join(" ")
}

/// Категория из `kind:` запроса поиска по дискам (последняя, если их несколько).
pub fn kind_of(text: &str) -> Category {
    raw_tokens(text)
        .iter()
        .rev()
        .find_map(|token| {
            let (key, value) = token.split_once(':')?;
            key.eq_ignore_ascii_case("kind").then(|| Category::parse(value)).flatten()
        })
        .unwrap_or_default()
}

/// Слова как есть, с кавычками: пробел внутри кавычек слово не делит.
fn raw_tokens(text: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    let mut quoted = false;
    for c in text.chars() {
        if c == '"' {
            quoted = !quoted;
        }
        if c.is_whitespace() && !quoted {
            if !current.is_empty() {
                tokens.push(std::mem::take(&mut current));
            }
        } else {
            current.push(c);
        }
    }
    if !current.is_empty() {
        tokens.push(current);
    }
    tokens
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_by_extension() {
        assert!(Category::Images.matches("Photo.JPG", false));
        assert!(!Category::Images.matches("Photo.JPG", true));
        assert!(Category::Video.matches("film.mkv", false));
        assert!(Category::Audio.matches("song.flac", false));
        assert!(Category::Documents.matches("отчёт.docx", false));
        assert!(Category::Archives.matches("backup.7z", false));
        assert!(Category::Programs.matches("setup.exe", false));
        assert!(Category::Folders.matches("photos.png", true));
        assert!(!Category::Folders.matches("photos.png", false));
        assert!(!Category::Documents.matches("readme", false));
        assert!(Category::All.matches("anything", false));
    }

    #[test]
    fn parses_words() {
        for category in Category::ALL {
            assert_eq!(Category::parse(category.key()), Some(category));
        }
        assert_eq!(Category::parse("Фото"), Some(Category::Images));
        assert_eq!(Category::parse("music"), Some(Category::Audio));
        assert_eq!(Category::parse("кот"), None);
    }

    #[test]
    fn rewrites_kind_in_query() {
        assert_eq!(with_kind("qwen", Category::Video), "qwen kind:видео");
        assert_eq!(
            with_kind("qwen kind:видео size:>1mb", Category::Audio),
            "qwen size:>1mb kind:аудио"
        );
        assert_eq!(with_kind("KIND:фото отчёт", Category::All), "отчёт");
        assert_eq!(
            with_kind("in:\"C:\\My Files\" kind:фото", Category::Documents),
            "in:\"C:\\My Files\" kind:документы"
        );
        assert_eq!(kind_of("отчёт kind:фото"), Category::Images);
        assert_eq!(kind_of("отчёт"), Category::All);
        assert_eq!(kind_of("kind:кот"), Category::All);
    }
}
