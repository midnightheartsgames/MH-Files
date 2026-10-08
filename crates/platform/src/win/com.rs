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

/// CLSID расширения Shell `iid` (обработчик предпросмотра, эскизов…) для расширения файла
/// (без точки). `AssocQueryStringW` сама смотрит `.ext\ShellEx`, ProgID и
/// `SystemFileAssociations`. Читает реестр — результат кэшировать у вызывающего.
pub fn shell_extension(ext: &str, iid: &str) -> Option<windows::core::GUID> {
    use windows::Win32::UI::Shell::{
        ASSOCF_INIT_IGNOREUNKNOWN, ASSOCF_NOTRUNCATE, ASSOCSTR_SHELLEXTENSION, AssocQueryStringW,
    };
    use windows::core::{GUID, PCWSTR, PWSTR};

    let ext = ext.trim_start_matches('.');
    if ext.is_empty() {
        return None;
    }
    let assoc = wide(format!(".{ext}"));
    let iid = wide(iid);
    let mut buffer = [0u16; 64];
    let mut len = buffer.len() as u32;
    // SAFETY: строки и буфер живут до конца вызова; len — размер буфера в символах.
    let found = unsafe {
        AssocQueryStringW(
            ASSOCF_INIT_IGNOREUNKNOWN | ASSOCF_NOTRUNCATE,
            ASSOCSTR_SHELLEXTENSION,
            PCWSTR(assoc.as_ptr()),
            PCWSTR(iid.as_ptr()),
            Some(PWSTR(buffer.as_mut_ptr())),
            &mut len,
        )
    };
    if found.is_err() {
        return None;
    }
    let end = buffer.iter().position(|&unit| unit == 0).unwrap_or(buffer.len());
    let text = String::from_utf16_lossy(&buffer[..end]);
    GUID::try_from(text.trim().trim_start_matches('{').trim_end_matches('}')).ok()
}
