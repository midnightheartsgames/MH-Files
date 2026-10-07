//! Сведения о файлах, которых нет в `std`: какой это файл физически (для жёстких ссылок).

use std::path::Path;

/// Номер тома и номер файла на нём. У жёстких ссылок на один файл они совпадают: удаление
/// одной из них места не освобождает.
pub fn identity(path: &Path) -> Option<(u64, u64)> {
    #[cfg(windows)]
    {
        crate::win::files::identity(path)
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let meta = std::fs::metadata(path).ok()?;
        Some((meta.dev(), meta.ino()))
    }
    #[cfg(not(any(windows, unix)))]
    {
        let _ = path;
        None
    }
}
