//! Номер файла NTFS: `GetFileInformationByHandle`.

use std::path::Path;

use windows::Win32::Storage::FileSystem::{
    BY_HANDLE_FILE_INFORMATION, CreateFileW, FILE_FLAG_BACKUP_SEMANTICS, FILE_READ_ATTRIBUTES,
    FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE, GetFileInformationByHandle,
    OPEN_EXISTING,
};
use windows::core::{Owned, PCWSTR};

use super::com::wide;

pub fn identity(path: &Path) -> Option<(u64, u64)> {
    let path = wide(path);
    // SAFETY: строка живёт до конца вызова; описатель закрывает Owned.
    let file = unsafe {
        Owned::new(
            CreateFileW(
                PCWSTR(path.as_ptr()),
                FILE_READ_ATTRIBUTES.0,
                FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
                None,
                OPEN_EXISTING,
                FILE_FLAG_BACKUP_SEMANTICS,
                None,
            )
            .ok()?,
        )
    };
    let mut info = BY_HANDLE_FILE_INFORMATION::default();
    // SAFETY: описатель открыт, структура — локальная.
    unsafe { GetFileInformationByHandle(*file, &mut info) }.ok()?;
    let index = (u64::from(info.nFileIndexHigh) << 32) | u64::from(info.nFileIndexLow);
    Some((u64::from(info.dwVolumeSerialNumber), index))
}
