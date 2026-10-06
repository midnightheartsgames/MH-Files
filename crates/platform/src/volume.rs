//! Тома NTFS: права администратора и журнал изменений USN.
//!
//! Журнал USN — список всех изменений тома, который ведёт сама NTFS. По нему индекс после
//! перезапуска узнаёт, какие папки менялись, пока программа была закрыта, и перечитывает
//! только их. Читать журнал можно только с правами администратора; без них индекс
//! пересканирует диск в фоне.

use std::path::{Path, PathBuf};

/// Положение в журнале: его номер (меняется при пересоздании журнала) и следующая запись.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Journal {
    pub id: u64,
    pub next_usn: i64,
}

/// Что изменилось с прошлого положения.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JournalChanges {
    /// Папки, в которых что-то создали, удалили, переименовали или изменили.
    pub dirs: Vec<PathBuf>,
    /// Новое положение — сохранить в снимок индекса.
    pub journal: Journal,
    /// Журнал пересоздан или нужные записи уже вытеснены: доверять нельзя, нужен полный обход.
    pub reset: bool,
}

/// Перевести текущий поток в фоновый режим: низкий приоритет процессора и диска. Обход
/// дисков для индекса не должен тормозить ни программу, ни остальную систему.
pub fn background_thread() {
    #[cfg(windows)]
    crate::win::volume::background_thread();
}

/// Запущена ли программа с правами администратора.
pub fn is_elevated() -> bool {
    #[cfg(windows)]
    {
        crate::win::volume::is_elevated()
    }
    #[cfg(not(windows))]
    {
        false
    }
}

/// Текущее положение журнала тома `root` (`C:\`). Нужны права администратора.
pub fn journal_state(root: &Path) -> Result<Journal, String> {
    #[cfg(windows)]
    {
        crate::win::volume::journal_state(root)
    }
    #[cfg(not(windows))]
    {
        let _ = root;
        Err("журнал USN есть только в Windows".into())
    }
}

/// Папки тома, изменённые после `since`. Может занять секунды — из фонового потока.
pub fn changed_dirs(root: &Path, since: Journal) -> Result<JournalChanges, String> {
    #[cfg(windows)]
    {
        crate::win::volume::changed_dirs(root, since)
    }
    #[cfg(not(windows))]
    {
        let _ = (root, since);
        Err("журнал USN есть только в Windows".into())
    }
}
