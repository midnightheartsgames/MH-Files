//! COM-апартамент потока и мелкие общие помощники для вызовов Win32.

use std::ffi::OsStr;
use std::os::windows::ffi::OsStrExt;

use windows::Win32::Foundation::HWND;
use windows::Win32::System::Com::{
    COINIT_APARTMENTTHREADED, COINIT_DISABLE_OLE1DDE, CoInitializeEx, CoUninitialize,
};

/// COM STA текущего потока. Shell требует STA: диалоги `IFileOperation`, «Открыть с помощью» и
/// обработчики эскизов сторонних программ без него не работают или работают с ошибками.
pub struct Apartment {
    /// `CoUninitialize` вызывается только в паре с успешным `CoInitializeEx`.
    initialized: bool,
}

impl Apartment {
    /// Входит в STA. Если поток уже в MTA (`RPC_E_CHANGED_MODE`), остаётся в нём: ничего не
    /// поделать, а выходить из чужого апартамента нельзя.
    pub fn sta() -> Apartment {
        // SAFETY: CoInitializeEx можно вызывать в любом потоке; парный вызов — в Drop.
        let hr = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE) };
        // S_OK и S_FALSE (уже инициализирован) оба успешны и оба требуют CoUninitialize.
        Apartment { initialized: hr.is_ok() }
    }
}

impl Drop for Apartment {
    fn drop(&mut self) {
        if self.initialized {
            // SAFETY: парный вызов к успешному CoInitializeEx в этом же потоке.
            unsafe { CoUninitialize() };
        }
    }
}

/// Строка для Win32: UTF-16 с завершающим нулём.
pub fn wide(s: impl AsRef<OsStr>) -> Vec<u16> {
    s.as_ref().encode_wide().chain(std::iter::once(0)).collect()
}

/// Окно-владелец для системных диалогов; `None`, если главное окно ещё не задано.
pub fn owner_hwnd() -> Option<HWND> {
    match crate::window::owner_window() {
        0 => None,
        hwnd => Some(HWND(hwnd as *mut core::ffi::c_void)),
    }
}

/// Текст ошибки Windows для пользователя: что делали и что ответила система.
pub fn describe(what: &str, error: &windows::core::Error) -> String {
    format!("{what}: {error}")
}
