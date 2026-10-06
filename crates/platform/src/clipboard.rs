//! Буфер обмена Windows с файлами: `CF_HDROP` + `Preferred DropEffect`. Тот же формат, что у
//! Проводника, поэтому копирование работает в обе стороны и с любыми программами.

use std::path::PathBuf;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClipboardFiles {
    pub paths: Vec<PathBuf>,
    /// «Вырезать»: после вставки исходники удаляются.
    pub cut: bool,
}

/// Кладёт файлы в буфер обмена.
pub fn set_files(paths: &[PathBuf], cut: bool) -> Result<(), String> {
    #[cfg(windows)]
    {
        crate::win::clipboard::set_files(paths, cut)
    }
    #[cfg(not(windows))]
    {
        *portable::CLIPBOARD.lock().unwrap() = Some(ClipboardFiles { paths: paths.to_vec(), cut });
        Ok(())
    }
}

/// Файлы из буфера обмена; `None` — в буфере не файлы.
pub fn get_files() -> Option<ClipboardFiles> {
    #[cfg(windows)]
    {
        crate::win::clipboard::get_files()
    }
    #[cfg(not(windows))]
    {
        portable::CLIPBOARD.lock().unwrap().clone()
    }
}

/// После вставки вырезанного буфер очищается, как в Проводнике.
pub fn clear() {
    #[cfg(windows)]
    {
        crate::win::clipboard::clear();
    }
    #[cfg(not(windows))]
    {
        *portable::CLIPBOARD.lock().unwrap() = None;
    }
}

#[cfg(not(windows))]
mod portable {
    use std::sync::Mutex;

    /// Вне Windows буфер живёт внутри процесса — для проверки логики этого достаточно.
    pub static CLIPBOARD: Mutex<Option<super::ClipboardFiles>> = Mutex::new(None);
}
