//! Обработчики предпросмотра Windows (`IPreviewHandler`): документы Office, PDF, письма и всё,
//! для чего установленные программы зарегистрировали предпросмотр, — как в области
//! просмотра Проводника.
//!
//! Обработчик рисует сам, в своё окно, поэтому показывается отдельным окном поверх
//! Инспектора, а не текстурой egui.

use std::path::Path;

/// Есть ли для расширения (без точки) обработчик предпросмотра Windows. Читает реестр
/// (`HKCR\.ext\ShellEx\{8895b1c6-b41f-4c1c-a562-0d564250836f}`, также через ProgID и
/// SystemFileAssociations). Быстрая, но трогает реестр — результат кэшировать у вызывающего.
pub fn has_handler(ext: &str) -> bool {
    #[cfg(windows)]
    {
        crate::win::preview_handler::has_handler(ext)
    }
    #[cfg(not(windows))]
    {
        let _ = ext;
        false
    }
}

/// Окно с обработчиком предпросмотра поверх Инспектора. Живёт в своём STA-потоке со своим
/// циклом сообщений: зависший обработчик не останавливает интерфейс. Методы не ждут —
/// команды уходят в поток окна.
///
/// COM-объектов в потоке UI нет: здесь только канал команд и описание окна.
pub struct PreviewHost {
    #[cfg(windows)]
    inner: crate::win::preview_handler::Host,
}

impl PreviewHost {
    /// Поток и окно создаются при первом `show`.
    pub fn new() -> PreviewHost {
        PreviewHost {
            #[cfg(windows)]
            inner: crate::win::preview_handler::Host::new(),
        }
    }

    /// Показать файл в прямоугольнике `rect` — клиентская область главного окна
    /// (`window::owner_window()`), физические пиксели: (x, y, ширина, высота).
    /// Тот же путь — только сдвинуть окно.
    ///
    /// Вызывать каждый кадр, пока предпросмотр виден: так окно следует за главным, когда то
    /// двигают. Одинаковые вызовы ничего не стоят.
    pub fn show(&mut self, path: &Path, rect: (i32, i32, i32, i32)) {
        #[cfg(windows)]
        self.inner.show(path, rect);
        #[cfg(not(windows))]
        let _ = (path, rect);
    }

    /// Спрятать окно и выгрузить обработчик.
    pub fn hide(&mut self) {
        #[cfg(windows)]
        self.inner.hide();
    }

    /// Последняя ошибка обработчика (не загрузился, файл не открылся) — для подписи в Инспекторе.
    pub fn error(&self) -> Option<String> {
        #[cfg(windows)]
        {
            self.inner.error()
        }
        #[cfg(not(windows))]
        {
            None
        }
    }
}

impl Default for PreviewHost {
    fn default() -> PreviewHost {
        PreviewHost::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_without_show_is_quiet() {
        let mut host = PreviewHost::new();
        host.hide();
        assert_eq!(host.error(), None);
    }
}
