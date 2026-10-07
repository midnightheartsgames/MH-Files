//! Окно-владелец для системных диалогов (прогресс копирования, «Открыть с помощью», свойства).

use std::sync::atomic::{AtomicBool, AtomicIsize, Ordering};

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

/// Сочетания, которые egui-winit забирает раньше программы: Ctrl+V без текста в буфере не
/// даёт вообще никакого события (а файлы в буфере — не текст), Shift+Delete превращается в
/// «Вырезать». В Windows их замечает перехватчик сообщений [`message_hook`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Intercepted {
    Paste,
    DeletePermanent,
}

static PASTE: AtomicBool = AtomicBool::new(false);
static DELETE_PERMANENT: AtomicBool = AtomicBool::new(false);

#[cfg_attr(not(windows), allow(dead_code))]
pub(crate) fn note(shortcut: Intercepted) {
    match shortcut {
        Intercepted::Paste => PASTE.store(true, Ordering::Relaxed),
        Intercepted::DeletePermanent => DELETE_PERMANENT.store(true, Ordering::Relaxed),
    }
}

/// Перехваченные с прошлого кадра сочетания; флаги сбрасываются.
pub fn take_intercepted() -> Vec<Intercepted> {
    let mut taken = Vec::new();
    if PASTE.swap(false, Ordering::Relaxed) {
        taken.push(Intercepted::Paste);
    }
    if DELETE_PERMANENT.swap(false, Ordering::Relaxed) {
        taken.push(Intercepted::DeletePermanent);
    }
    taken
}

/// Перехватчик для `EventLoopBuilderExtWindows::with_msg_hook`. Ничего не съедает.
#[cfg(windows)]
pub fn message_hook(msg: *const std::ffi::c_void) -> bool {
    crate::win::window::message_hook(msg)
}

/// Вывести главное окно на передний план: развернуть, если свёрнуто, и сделать активным.
/// Не ждёт поток окна, поэтому можно звать из любого потока. Windows может отказать (фокус у
/// другой программы, а права вывести окно у нас нет) — тогда кнопка на панели задач мигает.
pub fn bring_to_front() {
    #[cfg(windows)]
    crate::win::window::bring_to_front();
}

/// Кнопка «Развернуть» своего заголовка окна в пикселях экрана относительно клиентской
/// области: `[левый, верхний, правый, нижний]`; `None` — заголовок системный.
///
/// В Windows 11 по этой области окно отвечает системе `HTMAXBUTTON`: наведение показывает
/// Snap Layouts, а щелчок разворачивает окно как обычная кнопка. Звать из потока окна (там
/// же при первом вызове подменяется обработчик сообщений окна).
pub fn set_maximize_button(rect: Option<[i32; 4]>) {
    #[cfg(windows)]
    crate::win::window::set_maximize_button(rect);
    #[cfg(not(windows))]
    let _ = rect;
}

/// Мышь над кнопкой «Развернуть», а события ей шлёт система, а не окно (Snap Layouts):
/// подсветку рисует заголовок по этому признаку.
pub fn maximize_button_hovered() -> bool {
    #[cfg(windows)]
    {
        crate::win::window::maximize_button_hovered()
    }
    #[cfg(not(windows))]
    {
        false
    }
}
