//! Встраивание в Проводник: пункт «Открыть в MH Files» в контекстном меню папок, дисков и
//! пустого места окна. Только для текущего пользователя (`HKCU\Software\Classes`), без прав
//! администратора.

use std::path::Path;

/// Пункт «Открыть в MH Files» в меню папок, дисков и пустого места Проводника.
pub fn explorer_menu_installed() -> bool {
    #[cfg(windows)]
    {
        crate::win::integration::explorer_menu_installed()
    }
    #[cfg(not(windows))]
    {
        false
    }
}

/// Добавить пункт для `exe` (путь к MH-Files.exe). Только для текущего пользователя.
pub fn install_explorer_menu(exe: &Path) -> Result<(), String> {
    #[cfg(windows)]
    {
        crate::win::integration::install_explorer_menu(exe)
    }
    #[cfg(not(windows))]
    {
        let _ = exe;
        Err(UNSUPPORTED.into())
    }
}

/// Убрать пункт. Если его нет — не ошибка.
pub fn uninstall_explorer_menu() -> Result<(), String> {
    #[cfg(windows)]
    {
        crate::win::integration::uninstall_explorer_menu()
    }
    #[cfg(not(windows))]
    {
        Err(UNSUPPORTED.into())
    }
}

#[cfg(not(windows))]
const UNSUPPORTED: &str = "пункт в меню Проводника есть только в Windows";
