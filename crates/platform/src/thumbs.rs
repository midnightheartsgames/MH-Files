//! Эскизы и значки из Windows Shell (`IShellItemImageFactory`, `SHGetFileInfo`).
//!
//! Функции блокирующие и могут ждать сторонний обработчик эскизов — только из фоновых потоков,
//! в которых вызван [`init_worker_thread`].

use std::path::Path;

/// Картинка в RGBA без премультипликации, строки сверху вниз.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Bitmap {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ImageMode {
    /// Только эскиз содержимого. Нет обработчика — ошибка, а не значок.
    Thumbnail,
    /// Только значок объекта (у exe, lnk, ico он свой).
    Icon,
}

/// Зарегистрирован ли в Windows обработчик эскизов для расширения (без точки) — так их
/// регистрируют программы: Blender для `.blend`, Affinity, CAD. Читает реестр — только из
/// фоновых потоков, результат кэшировать.
pub fn has_thumbnail_provider(ext: &str) -> bool {
    #[cfg(windows)]
    {
        crate::win::thumbs::has_thumbnail_provider(ext)
    }
    #[cfg(not(windows))]
    {
        let _ = ext;
        false
    }
}

/// Подготовка потока к вызовам Shell (COM STA). Держать, пока поток работает.
pub struct ThreadGuard {
    #[cfg(windows)]
    _apartment: crate::win::com::Apartment,
}

pub fn init_worker_thread() -> ThreadGuard {
    ThreadGuard {
        #[cfg(windows)]
        _apartment: crate::win::com::Apartment::sta(),
    }
}

/// Эскиз или значок конкретного файла, стороной до `size` точек.
pub fn shell_image(path: &Path, size: u32, mode: ImageMode) -> Result<Bitmap, String> {
    #[cfg(windows)]
    {
        crate::win::thumbs::shell_image(path, size, mode)
    }
    #[cfg(not(windows))]
    {
        let _ = (path, size, mode);
        Err("эскизы Shell есть только в Windows".into())
    }
}

/// Значок типа: папки или файла с расширением `ext` (без точки). Файл не читается, поэтому
/// значок можно кэшировать по расширению.
pub fn type_icon(ext: &str, is_dir: bool, size: u32) -> Result<Bitmap, String> {
    #[cfg(windows)]
    {
        crate::win::thumbs::type_icon(ext, is_dir, size)
    }
    #[cfg(not(windows))]
    {
        let _ = (ext, is_dir, size);
        Err("значки Shell есть только в Windows".into())
    }
}

/// Расширения, у которых значок свой у каждого файла: его нельзя брать из кэша по типу.
pub fn has_own_icon(ext: &str) -> bool {
    matches!(ext, "exe" | "lnk" | "ico" | "url" | "cur" | "ani" | "msc" | "scr" | "appref-ms")
}
