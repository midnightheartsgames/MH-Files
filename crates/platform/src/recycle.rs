//! Корзина Windows: сколько в ней лежит, открыть её окно, очистить.
//!
//! Опрос корзины заглядывает в `$Recycle.Bin` каждого диска — только из фонового потока.

/// Что лежит в корзине всех дисков.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct BinInfo {
    pub items: u64,
    pub bytes: u64,
}

/// Сколько в корзине; `None` — узнать не вышло (или корзины нет — не Windows).
pub fn query() -> Option<BinInfo> {
    #[cfg(windows)]
    {
        crate::win::recycle::query()
    }
    #[cfg(not(windows))]
    {
        None
    }
}

/// Папки корзины текущего пользователя на дисках: `X:\$Recycle.Bin\<SID>`. Каждый диск
/// спрашивается, есть ли она там, — только из фонового потока.
pub fn bin_dirs() -> Vec<std::path::PathBuf> {
    #[cfg(windows)]
    {
        crate::win::recycle::bin_dirs()
    }
    #[cfg(not(windows))]
    {
        Vec::new()
    }
}

/// Окно корзины в Проводнике: там её содержимое и «Восстановить».
pub fn open() -> Result<(), String> {
    #[cfg(windows)]
    {
        crate::win::recycle::open()
    }
    #[cfg(not(windows))]
    {
        Err("корзина есть только в Windows".into())
    }
}

/// Очистить корзину всех дисков. Подтверждение и ход показывает сама Windows; отказ в
/// подтверждении — не ошибка.
pub fn empty() -> Result<(), String> {
    #[cfg(windows)]
    {
        crate::win::recycle::empty()
    }
    #[cfg(not(windows))]
    {
        Err("корзина есть только в Windows".into())
    }
}
