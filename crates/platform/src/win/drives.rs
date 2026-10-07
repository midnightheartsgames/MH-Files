//! Диски: `GetLogicalDrives` для списка, `GetVolumeInformationW` и `GetDiskFreeSpaceExW` для
//! сведений.

use std::path::{Path, PathBuf};

use windows::Win32::Foundation::{HANDLE, NO_ERROR};
use windows::Win32::NetworkManagement::WNet::WNetGetConnectionW;
use windows::Win32::Storage::FileSystem::{
    BusType1394, BusTypeMmc, BusTypeSd, BusTypeUsb, CreateFileW, FILE_FLAGS_AND_ATTRIBUTES,
    FILE_SHARE_READ, FILE_SHARE_WRITE, GetDiskFreeSpaceExW, GetDriveTypeW, GetLogicalDrives,
    GetVolumeInformationW, OPEN_EXISTING,
};
use windows::Win32::System::Diagnostics::Debug::{
    SEM_FAILCRITICALERRORS, SetThreadErrorMode, THREAD_ERROR_MODE,
};
use windows::Win32::System::IO::DeviceIoControl;
use windows::Win32::System::Ioctl::{
    IOCTL_STORAGE_QUERY_PROPERTY, PropertyStandardQuery, STORAGE_DEVICE_DESCRIPTOR,
    STORAGE_PROPERTY_QUERY, StorageDeviceProperty,
};
use windows::core::{Owned, PCWSTR, PWSTR};

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

/// Буквы дисков битовой маской (бит 0 — `A:`). Мгновенно: к дискам не обращается.
pub fn drive_mask() -> u32 {
    // SAFETY: только читает список букв.
    unsafe { GetLogicalDrives() }
}

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
                // Диск на USB или карта памяти: Windows зовёт его «несъёмным», но он приходит
                // и уходит, как флешка.
                DRIVE_FIXED if is_external(bit) => DriveKind::Removable,
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

/// Описатель устройства тома `\\.\X:` без прав на чтение: хватает для запросов о самом
/// устройстве, носитель не трогается, администратор не нужен.
fn open_device(bit: u8) -> Option<Owned<HANDLE>> {
    let device = wide(format!(r"\\.\{}:", (b'A' + bit) as char));
    // SAFETY: строка живёт до конца вызова; описатель закрывает Owned.
    unsafe {
        CreateFileW(
            PCWSTR(device.as_ptr()),
            0,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            None,
            OPEN_EXISTING,
            FILE_FLAGS_AND_ATTRIBUTES(0),
            None,
        )
        .ok()
        .map(|handle| Owned::new(handle))
    }
}

/// Диск на внешней шине (USB, FireWire, SD/MMC) или со съёмным носителем.
fn is_external(bit: u8) -> bool {
    let Some(device) = open_device(bit) else { return false };
    let query = STORAGE_PROPERTY_QUERY {
        PropertyId: StorageDeviceProperty,
        QueryType: PropertyStandardQuery,
        ..Default::default()
    };
    // Хватает на заголовок; строки производителя, которые идут следом, не нужны.
    let mut descriptor = STORAGE_DEVICE_DESCRIPTOR::default();
    let mut bytes = 0u32;
    // SAFETY: обе структуры живут до конца синхронного вызова, размеры верные.
    let ok = unsafe {
        DeviceIoControl(
            *device,
            IOCTL_STORAGE_QUERY_PROPERTY,
            Some(&query as *const _ as *const _),
            size_of::<STORAGE_PROPERTY_QUERY>() as u32,
            Some(&mut descriptor as *mut _ as *mut _),
            size_of::<STORAGE_DEVICE_DESCRIPTOR>() as u32,
            Some(&mut bytes),
            None,
        )
        .is_ok()
    };
    ok && bytes as usize >= std::mem::offset_of!(STORAGE_DEVICE_DESCRIPTOR, RawPropertiesLength)
        && (descriptor.RemovableMedia
            || [BusTypeUsb, BusType1394, BusTypeSd, BusTypeMmc].contains(&descriptor.BusType))
}

/// Серийный номер тома: у другой флешки на той же букве он другой. `None` — носителя нет.
pub fn volume_serial(root: &Path) -> Option<u32> {
    let _quiet = QuietErrors::new();
    let root_w = wide(root);
    let mut serial = 0u32;
    // SAFETY: строка и выходное число живут до конца вызова.
    unsafe {
        GetVolumeInformationW(PCWSTR(root_w.as_ptr()), None, Some(&mut serial), None, None, None)
    }
    .ok()
    .map(|()| serial)
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
