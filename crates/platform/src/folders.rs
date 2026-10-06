//! Известные папки Windows: рабочий стол, документы, загрузки…

use std::path::PathBuf;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum KnownFolder {
    Home,
    Desktop,
    Documents,
    Downloads,
    Pictures,
    Videos,
    Music,
}

impl KnownFolder {
    pub const ALL: [KnownFolder; 7] = [
        KnownFolder::Home,
        KnownFolder::Desktop,
        KnownFolder::Documents,
        KnownFolder::Downloads,
        KnownFolder::Pictures,
        KnownFolder::Videos,
        KnownFolder::Music,
    ];

    pub fn title(self) -> &'static str {
        match self {
            KnownFolder::Home => "Домашняя папка",
            KnownFolder::Desktop => "Рабочий стол",
            KnownFolder::Documents => "Документы",
            KnownFolder::Downloads => "Загрузки",
            KnownFolder::Pictures => "Изображения",
            KnownFolder::Videos => "Видео",
            KnownFolder::Music => "Музыка",
        }
    }
}

/// Путь известной папки. `None` — папки нет или Windows её не назвала.
pub fn known_folder(folder: KnownFolder) -> Option<PathBuf> {
    #[cfg(windows)]
    {
        crate::win::folders::known_folder(folder)
    }
    #[cfg(not(windows))]
    {
        let home = PathBuf::from(std::env::var_os("HOME")?);
        let sub = match folder {
            KnownFolder::Home => return Some(home),
            KnownFolder::Desktop => "Desktop",
            KnownFolder::Documents => "Documents",
            KnownFolder::Downloads => "Downloads",
            KnownFolder::Pictures => "Pictures",
            KnownFolder::Videos => "Videos",
            KnownFolder::Music => "Music",
        };
        Some(home.join(sub))
    }
}

/// Все известные папки, которые существуют. Обращается к диску — из фонового потока.
pub fn known_folders() -> Vec<(KnownFolder, PathBuf)> {
    KnownFolder::ALL
        .iter()
        .filter_map(|&folder| known_folder(folder).map(|path| (folder, path)))
        .filter(|(_, path)| path.is_dir())
        .collect()
}
