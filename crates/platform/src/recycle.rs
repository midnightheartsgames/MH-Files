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
