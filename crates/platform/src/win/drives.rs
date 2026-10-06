//! Диски: `GetLogicalDrives` для списка, `GetVolumeInformationW` и `GetDiskFreeSpaceExW` для
//! сведений.

use std::path::{Path, PathBuf};

use windows::Win32::Foundation::NO_ERROR;
use windows::Win32::NetworkManagement::WNet::WNetGetConnectionW;
use windows::Win32::Storage::FileSystem::{
    GetDiskFreeSpaceExW, GetDriveTypeW, GetLogicalDrives, GetVolumeInformationW,
};
use windows::Win32::System::Diagnostics::Debug::{
    SEM_FAILCRITICALERRORS, SetThreadErrorMode, THREAD_ERROR_MODE,
};
use windows::core::{PCWSTR, PWSTR};

use super::com::wide;
use crate::drives::{DriveInfo, DriveKind};

// Значения GetDriveTypeW (winbase.h). Свои константы, чтобы не тянуть ради них целый модуль
// `Win32_System_WindowsProgramming`.
const DRIVE_NO_ROOT_DIR: u32 = 1;
const DRIVE_REMOVABLE: u32 = 2;
const DRIVE_FIXED: u32 = 3;
const DRIVE_REMOTE: u32 = 4;
const DRIVE_CDROM: u32 = 5;
const DRIVE_RAMDISK: u32 = 6;

pub fn drive_roots() -> Vec<(PathBuf, DriveKind)> {
    // SAFETY: функции только читают список дисков; к самим носителям не обращаются.
    let mask = unsafe { GetLogicalDrives() };
    (0..26u8)
        .filter(|bit| mask & (1 << bit) != 0)
        .filter_map(|bit| {
            let root = format!("{}:\\", (b'A' + bit) as char);
            let root_w = wide(&root);
            let kind = match unsafe { GetDriveTypeW(PCWSTR(root_w.as_ptr())) } {
                DRIVE_NO_ROOT_DIR => return None,
                DRIVE_REMOVABLE => DriveKind::Removable,
                DRIVE_FIXED => DriveKind::Fixed,
                DRIVE_REMOTE => DriveKind::Network,
                DRIVE_CDROM => DriveKind::Optical,
                DRIVE_RAMDISK => DriveKind::Ram,
                _ => DriveKind::Unknown,
            };
            Some((PathBuf::from(root), kind))
        })
        .collect()
}

pub fn drive_info(root: &Path, kind: DriveKind) -> DriveInfo {
    // Пустой привод или кардридер иначе показывает системное окно «Вставьте диск».
    let _quiet = QuietErrors::new();
    let root_w = wide(root);
    let root_p = PCWSTR(root_w.as_ptr());

    let mut label = [0u16; 261];
    let mut fs = [0u16; 261];
    // SAFETY: буферы живут до конца вызова, длины передаются срезами.
    let volume_ok = unsafe {
        GetVolumeInformationW(root_p, Some(&mut label), None, None, None, Some(&mut fs)).is_ok()
    };

    let (mut free, mut total) = (0u64, 0u64);
    // SAFETY: указатели на локальные переменные.
    let space_ok =
        unsafe { GetDiskFreeSpaceExW(root_p, Some(&mut free), Some(&mut total), None).is_ok() };

    let mut label = if volume_ok { from_wide_buf(&label) } else { String::new() };
    if label.is_empty() && kind == DriveKind::Network {
        label = remote_name(root).unwrap_or_default();
    }
    DriveInfo {
        root: root.to_path_buf(),
        label,
        file_system: if volume_ok { from_wide_buf(&fs) } else { String::new() },
        kind,
        total: if space_ok { total } else { 0 },
        free: if space_ok { free } else { 0 },
        ready: volume_ok || space_ok,
    }
}

/// `\\сервер\ресурс` для подключённого сетевого диска: у них часто нет метки тома.
fn remote_name(root: &Path) -> Option<String> {
    let letter = root.to_str()?.get(..2)?;
    let local = wide(letter);
    let mut buf = [0u16; 512];
    let mut len = buf.len() as u32;
    // SAFETY: длина буфера передаётся в `len`, Windows не пишет за его пределы.
    let status = unsafe {
        WNetGetConnectionW(PCWSTR(local.as_ptr()), Some(PWSTR(buf.as_mut_ptr())), &mut len)
    };
    (status == NO_ERROR).then(|| from_wide_buf(&buf)).filter(|name| !name.is_empty())
}

fn from_wide_buf(buf: &[u16]) -> String {
    let len = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    String::from_utf16_lossy(&buf[..len])
}

/// Отключает системные окна критических ошибок в текущем потоке и возвращает прежний режим.
struct QuietErrors(Option<THREAD_ERROR_MODE>);

impl QuietErrors {
    fn new() -> QuietErrors {
        let mut old = THREAD_ERROR_MODE(0);
        // SAFETY: меняется режим только текущего потока.
        let ok = unsafe { SetThreadErrorMode(SEM_FAILCRITICALERRORS, Some(&mut old)).is_ok() };
        QuietErrors(ok.then_some(old))
    }
}

impl Drop for QuietErrors {
    fn drop(&mut self) {
        if let Some(old) = self.0 {
            // SAFETY: возвращаем режим, сохранённый в new.
            let _ = unsafe { SetThreadErrorMode(old, None) };
        }
    }
}
