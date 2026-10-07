//! Встраивание в Проводник. Только для текущего пользователя (`HKCU\Software\Classes`), без
//! прав администратора:
//! * пункт «Открыть в MH Files» в контекстном меню папок, дисков и пустого места окна;
//! * MH Files — действие по умолчанию для папок и дисков (двойной щелчок открывает папку в
//!   MH Files). `Folder\shell` не трогается: на нём держатся `Win+E`, «Панель управления» и
//!   библиотеки, — только `Directory` и `Drive`;
//! * архивы (zip, 7z…) — MH Files в списке «Открыть с помощью»; программой по умолчанию
//!   Windows 10/11 позволяет сделать её только самому пользователю.

use std::path::Path;

/// Что из встраивания сейчас есть.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Status {
    pub explorer_menu: bool,
    pub default_folders: bool,
    pub archives: bool,
}

/// Что включить или выключить.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Feature {
    ExplorerMenu,
    /// Требует пункта меню: включение добавляет и его.
    DefaultFolders,
    Archives,
}

/// Расширения архивов, которые MH Files открывает как папки.
pub const ARCHIVE_EXTENSIONS: [&str; 3] = ["zip", "7z", "rar"];

pub fn status() -> Status {
    #[cfg(windows)]
    {
        crate::win::integration::status()
    }
    #[cfg(not(windows))]
    {
        Status::default()
    }
}

/// Включить (`on`) или выключить возможность для `exe` (путь к MH-Files.exe).
pub fn set(feature: Feature, exe: &Path, on: bool) -> Result<(), String> {
    #[cfg(windows)]
    {
        crate::win::integration::set(feature, exe, on)
    }
    #[cfg(not(windows))]
    {
        let _ = (feature, exe, on);
        Err(UNSUPPORTED.into())
    }
}

#[cfg(not(windows))]
const UNSUPPORTED: &str = "встраивание в Проводник есть только в Windows";
