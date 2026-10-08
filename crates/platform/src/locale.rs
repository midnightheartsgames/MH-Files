//! Языки пользователя: раскладки клавиатуры Windows.

/// Языки раскладок клавиатуры (BCP 47: «en-US», «ru-RU»), по порядку, без повторов. Вне
/// Windows — язык из `LANG`. Вызов Win32 — звать из фонового потока (в UI нет Win32).
pub fn keyboard_languages() -> Vec<String> {
    #[cfg(windows)]
    {
        crate::win::locale::keyboard_languages()
    }
    #[cfg(not(windows))]
    {
        // «ru_RU.UTF-8» → «ru-RU».
        std::env::var("LANG")
            .ok()
            .and_then(|lang| lang.split('.').next().map(|l| l.replace('_', "-")))
            .filter(|lang| !lang.is_empty() && lang != "C" && lang != "POSIX")
            .into_iter()
            .collect()
    }
}
