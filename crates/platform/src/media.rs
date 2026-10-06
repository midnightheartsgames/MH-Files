//! Свойства медиафайлов, фотографий и документов из Windows Property System — для Инспектора.
//!
//! Значения читают обработчики свойств Windows (те же, что у колонок Проводника), поэтому
//! функция блокирующая и может ждать чужой код — только из фоновых потоков.

use std::path::Path;

/// Свойства файла из Windows Property System для Инспектора: длительность, кадр, частота,
/// исполнитель, альбом, битрейт, камера, страницы, автор… Подписи — по-русски (свои),
/// значения — как их форматирует Windows (PSFormatForDisplay). Пустые свойства пропускаются.
/// Блокирующая: только из фонового потока с `thumbs::init_worker_thread()`.
pub fn media_info(path: &Path) -> Result<Vec<(String, String)>, String> {
    #[cfg(windows)]
    {
        crate::win::media::media_info(path)
    }
    #[cfg(not(windows))]
    {
        let _ = path;
        Err("свойства медиафайлов есть только в Windows".into())
    }
}
