//! Запись о файле или папке — то, что воркер прочитал из каталога.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::SystemTime;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum EntryKind {
    Dir,
    File,
}

/// Атрибуты Windows, которые влияют на показ.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Hash)]
pub struct Attributes(pub u32);

impl Attributes {
    pub const READONLY: u32 = 0x1;
    pub const HIDDEN: u32 = 0x2;
    pub const SYSTEM: u32 = 0x4;
    /// Ссылка, junction и прочие точки повторной обработки. Рекурсивно не обходятся.
    pub const REPARSE: u32 = 0x400;

    pub fn hidden(self) -> bool {
        self.0 & Self::HIDDEN != 0
    }
    pub fn system(self) -> bool {
        self.0 & Self::SYSTEM != 0
    }
    pub fn readonly(self) -> bool {
        self.0 & Self::READONLY != 0
    }
    pub fn reparse(self) -> bool {
        self.0 & Self::REPARSE != 0
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Entry {
    pub name: String,
    /// Папка, в которой лежит запись. У всех записей одного каталога — один общий `Arc`;
    /// у результатов поиска — разные.
    pub parent: Arc<Path>,
    pub kind: EntryKind,
    /// Байты; у папок ноль.
    pub size: u64,
    pub modified: Option<SystemTime>,
    pub created: Option<SystemTime>,
    pub attributes: Attributes,
}

impl Entry {
    pub fn path(&self) -> PathBuf {
        self.parent.join(&self.name)
    }

    pub fn is_dir(&self) -> bool {
        self.kind == EntryKind::Dir
    }

    /// Расширение в нижнем регистре без точки; у папок и файлов вида `.gitignore` — пустое.
    pub fn extension(&self) -> String {
        if self.is_dir() {
            return String::new();
        }
        extension_of(&self.name)
    }

    /// Скрытый в смысле Проводника: атрибут «скрытый». Имена с точкой в Windows не скрыты.
    pub fn hidden(&self) -> bool {
        self.attributes.hidden()
    }
}

/// Расширение имени файла в нижнем регистре без точки.
pub fn extension_of(name: &str) -> String {
    match name.rfind('.') {
        Some(dot) if dot > 0 && dot + 1 < name.len() => name[dot + 1..].to_lowercase(),
        _ => String::new(),
    }
}

/// Расширения архивов, которые открываются как папки.
pub const ARCHIVE_EXTENSIONS: [&str; 3] = ["zip", "7z", "rar"];

/// Можно ли открыть файл с таким именем как папку (zip, 7z, rar).
pub fn is_archive_name(name: &str) -> bool {
    ARCHIVE_EXTENSIONS.contains(&extension_of(name).as_str())
}

/// Видео или звук — то, что играет быстрый просмотр.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MediaKind {
    Video,
    Audio,
}

/// Вид медиафайла по имени; `None` — не медиа.
pub fn media_kind(name: &str) -> Option<MediaKind> {
    match extension_of(name).as_str() {
        "mp4" | "m4v" | "mov" | "mkv" | "avi" | "wmv" | "webm" | "mpg" | "mpeg" | "ts" | "mts"
        | "m2ts" | "3gp" | "asf" => Some(MediaKind::Video),
        "mp3" | "m4a" | "aac" | "wav" | "flac" | "ogg" | "opus" | "wma" | "aiff" | "aif"
        | "ac3" | "amr" => Some(MediaKind::Audio),
        _ => None,
    }
}

/// Имя без расширения (у папок — всё имя).
pub fn stem_of(name: &str, is_dir: bool) -> &str {
    if is_dir {
        return name;
    }
    match name.rfind('.') {
        Some(dot) if dot > 0 && dot + 1 < name.len() => &name[..dot],
        _ => name,
    }
}

#[cfg(test)]
pub(crate) fn test_entry(name: &str, kind: EntryKind, size: u64) -> Entry {
    Entry {
        name: name.to_string(),
        parent: Arc::from(Path::new("/test")),
        kind,
        size,
        modified: None,
        created: None,
        attributes: Attributes::default(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extensions() {
        assert_eq!(extension_of("Photo.JPG"), "jpg");
        assert_eq!(extension_of("archive.tar.gz"), "gz");
        assert_eq!(extension_of(".gitignore"), "");
        assert_eq!(extension_of("README"), "");
        assert_eq!(extension_of("dot."), "");
        assert_eq!(stem_of("a.b.c", false), "a.b");
        assert_eq!(stem_of("a.b.c", true), "a.b.c");
        assert_eq!(stem_of(".env", false), ".env");
    }
}
