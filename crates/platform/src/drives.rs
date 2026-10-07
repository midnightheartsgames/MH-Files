//! Диски. Список корней берётся без обращения к самим дискам, сведения о каждом — отдельно:
//! уснувший HDD или недоступный сетевой диск не должен задерживать остальные.

use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DriveKind {
    Fixed,
    Removable,
    Network,
    Optical,
    Ram,
    Unknown,
}

#[derive(Debug, Clone, PartialEq)]
pub struct DriveInfo {
    /// `C:\` и т. п.
    pub root: PathBuf,
    /// Метка тома; пустая, если её нет.
    pub label: String,
    /// NTFS, exFAT…; пустая, если неизвестна.
    pub file_system: String,
    pub kind: DriveKind,
    /// Байты; ноль, если неизвестно (диск не готов).
    pub total: u64,
    pub free: u64,
    /// Диск отвечает: в приводе есть носитель, сетевой путь доступен.
    pub ready: bool,
}

impl DriveInfo {
    /// Запись «ещё не опрошен»: показывается сразу, пока идёт опрос.
    pub fn pending(root: PathBuf, kind: DriveKind) -> DriveInfo {
        DriveInfo {
            root,
            label: String::new(),
            file_system: String::new(),
            kind,
            total: 0,
            free: 0,
            ready: false,
        }
    }

    pub fn used(&self) -> u64 {
        self.total.saturating_sub(self.free)
    }

    /// Доля занятого места, 0..=1.
    pub fn used_fraction(&self) -> f32 {
        if self.total == 0 { 0.0 } else { (self.used() as f64 / self.total as f64) as f32 }
    }
}

/// Корни дисков и их тип. Быстро: сами диски не опрашиваются.
pub fn drive_roots() -> Vec<(PathBuf, DriveKind)> {
    #[cfg(windows)]
    {
        crate::win::drives::drive_roots()
    }
    #[cfg(not(windows))]
    {
        vec![(PathBuf::from("/"), DriveKind::Fixed)]
    }
}

/// Буквы дисков битовой маской: меняется, когда диск подключили или отключили. Мгновенно —
/// к дискам не обращается, можно опрашивать часто.
pub fn drive_mask() -> u32 {
    #[cfg(windows)]
    {
        crate::win::drives::drive_mask()
    }
    #[cfg(not(windows))]
    {
        1
    }
}

/// Серийный номер тома (другая флешка на той же букве — другой номер). `None` — носителя нет
/// или номер неизвестен. Обращается к диску — только из фонового потока.
pub fn volume_serial(root: &Path) -> Option<u32> {
    #[cfg(windows)]
    {
        crate::win::drives::volume_serial(root)
    }
    #[cfg(not(windows))]
    {
        let _ = root;
        None
    }
}

/// Сведения о диске. Может ждать секунды (сеть, раскрутка HDD) — только из фонового потока.
pub fn drive_info(root: &Path, kind: DriveKind) -> DriveInfo {
    #[cfg(windows)]
    {
        crate::win::drives::drive_info(root, kind)
    }
    #[cfg(not(windows))]
    {
        DriveInfo {
            label: "Корень".into(),
            file_system: String::new(),
            ready: root.exists(),
            ..DriveInfo::pending(root.to_path_buf(), kind)
        }
    }
}
