//! Окно-владелец для системных диалогов (прогресс копирования, «Открыть с помощью», свойства).

use std::sync::atomic::{AtomicIsize, Ordering};

static OWNER: AtomicIsize = AtomicIsize::new(0);

/// Запоминает HWND главного окна. Ноль — без владельца.
pub fn set_owner_window(hwnd: isize) {
    OWNER.store(hwnd, Ordering::Relaxed);
}

/// HWND главного окна или ноль.
pub fn owner_window() -> isize {
    OWNER.load(Ordering::Relaxed)
}

/// Видна ли точка экрана хоть на одном мониторе: сохранённое положение окна могло остаться на
/// отключённом мониторе.
pub fn point_on_screen(x: i32, y: i32) -> bool {
    #[cfg(windows)]
    {
        crate::win::window::point_on_screen(x, y)
    }
    #[cfg(not(windows))]
    {
        let _ = (x, y);
        true
    }
}
