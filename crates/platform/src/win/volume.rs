//! Права администратора и журнал USN тома NTFS.
//!
//! Журнал читается через описатель самого тома (`\\.\C:`), а открыть том на чтение может только
//! администратор. Записи журнала знают не пути, а номера файлов (FRN) — пути родительских папок
//! восстанавливаются по номерам через `OpenFileById` уже после чтения, по разу на папку.

use std::collections::HashSet;
use std::ffi::OsString;
use std::os::windows::ffi::OsStringExt;
use std::path::{Component, Path, PathBuf, Prefix};

use windows::Win32::Foundation::{
    ERROR_HANDLE_EOF, ERROR_JOURNAL_DELETE_IN_PROGRESS, ERROR_JOURNAL_ENTRY_DELETED,
    ERROR_JOURNAL_NOT_ACTIVE, GENERIC_READ, HANDLE,
};
use windows::Win32::Security::{GetTokenInformation, TOKEN_ELEVATION, TOKEN_QUERY, TokenElevation};
use windows::Win32::Storage::FileSystem::{
    CreateFileW, FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAGS_AND_ATTRIBUTES, FILE_ID_DESCRIPTOR,
    FILE_ID_DESCRIPTOR_0, FILE_NAME_NORMALIZED, FILE_READ_ATTRIBUTES, FILE_SHARE_DELETE,
    FILE_SHARE_READ, FILE_SHARE_WRITE, FileIdType, GETFINALPATHNAMEBYHANDLE_FLAGS,
    GetFinalPathNameByHandleW, OPEN_EXISTING, OpenFileById, VOLUME_NAME_DOS,
};
use windows::Win32::System::IO::DeviceIoControl;
use windows::Win32::System::Ioctl::{
    FSCTL_ENUM_USN_DATA, FSCTL_QUERY_USN_JOURNAL, FSCTL_READ_USN_JOURNAL, MFT_ENUM_DATA_V0,
    READ_USN_JOURNAL_DATA_V0, USN_JOURNAL_DATA_V0, USN_REASON_BASIC_INFO_CHANGE,
    USN_REASON_DATA_EXTEND, USN_REASON_DATA_OVERWRITE, USN_REASON_DATA_TRUNCATION,
    USN_REASON_FILE_CREATE, USN_REASON_FILE_DELETE, USN_REASON_RENAME_NEW_NAME,
    USN_REASON_RENAME_OLD_NAME,
};
use windows::Win32::System::Threading::{
    GetCurrentProcess, GetCurrentThread, OpenProcessToken, SetThreadPriority,
    THREAD_MODE_BACKGROUND_BEGIN,
};
use windows::core::{HRESULT, Owned, PCWSTR};

use super::com::{describe, wide};
use crate::volume::{Journal, JournalChanges, MFT_ROOT, MftRecord};

/// Буфер чтения журнала. Из `u64`, чтобы записи (в них есть 64-битные поля) были выровнены.
const READ_WORDS: usize = 64 * 1024 / 8;

/// Больше папок — дешевле обойти диск заново, чем открывать каждую по номеру.
const MAX_DIRS: usize = 200_000;

/// Изменения, после которых содержимое родительской папки в индексе устаревает. `CLOSE` не
/// нужен: те же причины уже пришли отдельными записями раньше.
const REASONS: u32 = USN_REASON_FILE_CREATE
    | USN_REASON_FILE_DELETE
    | USN_REASON_RENAME_OLD_NAME
    | USN_REASON_RENAME_NEW_NAME
    | USN_REASON_DATA_OVERWRITE
    | USN_REASON_DATA_EXTEND
    | USN_REASON_DATA_TRUNCATION
    | USN_REASON_BASIC_INFO_CHANGE;

pub fn background_thread() {
    // SAFETY: псевдодескриптор текущего потока, закрывать его не нужно.
    unsafe {
        let _ = SetThreadPriority(GetCurrentThread(), THREAD_MODE_BACKGROUND_BEGIN);
    }
}

pub fn is_elevated() -> bool {
    let mut token = HANDLE::default();
    // SAFETY: псевдодескриптор процесса закрывать не нужно; токен закрывает Owned.
    if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) }.is_err() {
        return false;
    }
    // SAFETY: токен открыт успешно и больше нигде не закрывается.
    let token = unsafe { Owned::new(token) };
    let mut elevation = TOKEN_ELEVATION::default();
    let mut returned = 0u32;
    // SAFETY: буфер — TOKEN_ELEVATION ровно того размера, что передан.
    let ok = unsafe {
        GetTokenInformation(
            *token,
            TokenElevation,
            Some(&mut elevation as *mut _ as *mut _),
            size_of::<TOKEN_ELEVATION>() as u32,
            &mut returned,
        )
    };
    ok.is_ok() && elevation.TokenIsElevated != 0
}

pub fn journal_state(root: &Path) -> Result<Journal, String> {
    let volume = open_volume(root)?;
    query(&volume, root).map(|data| Journal { id: data.UsnJournalID, next_usn: data.NextUsn })
}

pub fn changed_dirs(root: &Path, since: Journal) -> Result<JournalChanges, String> {
    let volume = open_volume(root)?;
    let data = query(&volume, root)?;
    let current = Journal { id: data.UsnJournalID, next_usn: data.NextUsn };
    let reset = JournalChanges { dirs: Vec::new(), journal: current, reset: true };
    // Другой журнал (пересоздан) или нужные записи уже затёрты новыми — пропуски неизвестны.
    if current.id != since.id || since.next_usn < data.FirstUsn {
        return Ok(reset);
    }
    if since.next_usn >= current.next_usn {
        return Ok(JournalChanges { dirs: Vec::new(), journal: since, reset: false });
    }

    let Some((parents, next_usn)) = read_parents(&volume, root, since.next_usn, current)? else {
        return Ok(reset);
    };
    let dirs = parents.into_iter().filter_map(|frn| path_by_id(&volume, frn)).collect();
    Ok(JournalChanges { dirs, journal: Journal { id: current.id, next_usn }, reset: false })
}

pub fn mft_records(root: &Path) -> Result<Vec<MftRecord>, String> {
    let volume = open_volume(root)?;
    let data = query(&volume, root)?;
    let mut buffer = vec![0u64; READ_WORDS * 4];
    let mut records = Vec::new();
    let mut request =
        MFT_ENUM_DATA_V0 { StartFileReferenceNumber: 0, LowUsn: 0, HighUsn: data.NextUsn };
    loop {
        let mut bytes = 0u32;
        // SAFETY: входная структура и буфер живут до конца синхронного вызова, размеры верные.
        let read = unsafe {
            DeviceIoControl(
                *volume,
                FSCTL_ENUM_USN_DATA,
                Some(&request as *const _ as *const _),
                size_of::<MFT_ENUM_DATA_V0>() as u32,
                Some(buffer.as_mut_ptr() as *mut _),
                (buffer.len() * 8) as u32,
                Some(&mut bytes),
                None,
            )
        };
        match read {
            Ok(()) => {}
            // Записи кончились.
            Err(error) if is_win32(&error, ERROR_HANDLE_EOF.0) => break,
            Err(error) => {
                return Err(describe(
                    &format!("не удалось прочитать MFT диска {}", root.display()),
                    &error,
                ));
            }
        }
        if bytes <= 8 {
            break;
        }
        // SAFETY: Vec<u64> можно читать как байты; длина не больше выделенной.
        let raw = unsafe {
            std::slice::from_raw_parts(
                buffer.as_ptr() as *const u8,
                (bytes as usize).min(buffer.len() * 8),
            )
        };
        // Первые 8 байт — номер, с которого продолжать.
        let next = buffer[0];
        collect_records(&raw[8..], &mut records);
        if next <= request.StartFileReferenceNumber {
            break;
        }
        request.StartFileReferenceNumber = next;
    }
    Ok(records)
}

/// Номер записи без номера последовательности (старшие 16 бит).
const FRN_MASK: u64 = (1 << 48) - 1;

/// Разбирает `USN_RECORD_V2` с проверкой границ. Служебные записи NTFS (номера до 16, кроме
/// корня) отбрасываются: от корня до их детей (`$Extend\…`) тогда тоже не дойти.
fn collect_records(raw: &[u8], records: &mut Vec<MftRecord>) {
    let u16_at = |at: usize| raw.get(at..at + 2).map(|b| u16::from_le_bytes([b[0], b[1]]));
    let u32_at =
        |at: usize| raw.get(at..at + 4).map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]));
    let u64_at =
        |at: usize| raw.get(at..at + 8)?.first_chunk::<8>().map(|b| u64::from_le_bytes(*b));
    let mut offset = 0usize;
    while let (Some(length), Some(major)) = (u32_at(offset), u16_at(offset + 4)) {
        if length == 0 {
            break;
        }
        let record = (|| {
            if major != 2 {
                return None;
            }
            let id = u64_at(offset + 8)? & FRN_MASK;
            let parent = u64_at(offset + 16)? & FRN_MASK;
            let attributes = u32_at(offset + 52)?;
            let name_len = u16_at(offset + 56)? as usize;
            let name_at = offset + u16_at(offset + 58)? as usize;
            if id < 16 && id != MFT_ROOT {
                return None;
            }
            let units: Vec<u16> = raw
                .get(name_at..name_at + name_len)?
                .as_chunks::<2>()
                .0
                .iter()
                .map(|&pair| u16::from_le_bytes(pair))
                .collect();
            Some(MftRecord {
                id,
                parent,
                name: String::from_utf16_lossy(&units),
                is_dir: attributes & 0x10 != 0,
                hidden: attributes & 0x2 != 0,
                system: attributes & 0x4 != 0,
            })
        })();
        records.extend(record);
        offset += length as usize;
    }
}

/// Номера родительских папок всех записей от `start` до `current.next_usn` и положение, с
/// которого читать в следующий раз. `None` — читать бессмысленно, нужен полный обход.
fn read_parents(
    volume: &Owned<HANDLE>,
    root: &Path,
    start: i64,
    current: Journal,
) -> Result<Option<(HashSet<u64>, i64)>, String> {
    let mut buffer = vec![0u64; READ_WORDS];
    let mut parents = HashSet::new();
    let mut usn = start;
    while usn < current.next_usn {
        let request = READ_USN_JOURNAL_DATA_V0 {
            StartUsn: usn,
            ReasonMask: REASONS,
            ReturnOnlyOnClose: 0,
            Timeout: 0,
            BytesToWaitFor: 0,
            UsnJournalID: current.id,
        };
        let mut bytes = 0u32;
        // SAFETY: входная структура и буфер живут до конца синхронного вызова, размеры верные.
        let read = unsafe {
            DeviceIoControl(
                **volume,
                FSCTL_READ_USN_JOURNAL,
                Some(&request as *const _ as *const _),
                size_of::<READ_USN_JOURNAL_DATA_V0>() as u32,
                Some(buffer.as_mut_ptr() as *mut _),
                (buffer.len() * 8) as u32,
                Some(&mut bytes),
                None,
            )
        };
        if let Err(error) = read {
            // Записи вытеснили, пока читали, или журнал удаляют прямо сейчас.
            if is_win32(&error, ERROR_JOURNAL_ENTRY_DELETED.0)
                || is_win32(&error, ERROR_JOURNAL_DELETE_IN_PROGRESS.0)
            {
                return Ok(None);
            }
            return Err(journal_error(root, &error));
        }
        if bytes < 8 {
            break;
        }
        // SAFETY: Vec<u64> можно читать как байты; длина не больше выделенной.
        let raw = unsafe {
            std::slice::from_raw_parts(
                buffer.as_ptr() as *const u8,
                (bytes as usize).min(buffer.len() * 8),
            )
        };
        // Первые 8 байт — откуда читать дальше; записи, не прошедшие фильтр, уже пропущены.
        let next = buffer[0] as i64;
        collect_parents(&raw[8..], &mut parents);
        if parents.len() > MAX_DIRS {
            return Ok(None);
        }
        if next <= usn {
            break;
        }
        usn = next;
    }
    Ok(Some((parents, usn)))
}

/// Разбирает цепочку `USN_RECORD_V2`/`V3` с проверкой границ, без приведения указателей.
fn collect_parents(raw: &[u8], parents: &mut HashSet<u64>) {
    let u16_at = |at: usize| raw.get(at..at + 2).map(|b| u16::from_le_bytes([b[0], b[1]]));
    let u32_at =
        |at: usize| raw.get(at..at + 4).map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]));
    let u64_at =
        |at: usize| raw.get(at..at + 8)?.first_chunk::<8>().map(|b| u64::from_le_bytes(*b));
    let mut offset = 0usize;
    while let (Some(length), Some(major)) = (u32_at(offset), u16_at(offset + 4)) {
        if length == 0 {
            break;
        }
        let parent = match major {
            // V2: FileReferenceNumber (8 байт) по смещению 8, родитель — по 16.
            2 => u64_at(offset + 16),
            // V3: 128-битные номера, родитель по смещению 24. Номер NTFS умещается в младшие 64
            // бита; если старшие заняты (ReFS), OpenFileById с FileIdType его не откроет.
            3 => match (u64_at(offset + 24), u64_at(offset + 32)) {
                (Some(low), Some(0)) => Some(low),
                _ => None,
            },
            _ => None,
        };
        if let Some(parent) = parent {
            parents.insert(parent);
        }
        offset += length as usize;
    }
}

/// Путь папки по её номеру. `None`, если её уже нет или к ней нет доступа.
fn path_by_id(volume: &Owned<HANDLE>, frn: u64) -> Option<PathBuf> {
    let id = FILE_ID_DESCRIPTOR {
        dwSize: size_of::<FILE_ID_DESCRIPTOR>() as u32,
        Type: FileIdType,
        Anonymous: FILE_ID_DESCRIPTOR_0 { FileId: frn as i64 },
    };
    // SAFETY: описатель тома жив; дескриптор живёт до конца вызова; открытый файл закрывает
    // Owned.
    let file = unsafe {
        Owned::new(
            OpenFileById(
                **volume,
                &id,
                FILE_READ_ATTRIBUTES.0,
                FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
                None,
                FILE_FLAG_BACKUP_SEMANTICS,
            )
            .ok()?,
        )
    };
    let mut buf = vec![0u16; 512];
    loop {
        // SAFETY: буфер — срез с верной длиной.
        let len = unsafe {
            // Оба флага нулевые: нормализованный путь с буквой диска.
            GetFinalPathNameByHandleW(
                *file,
                &mut buf,
                GETFINALPATHNAMEBYHANDLE_FLAGS(FILE_NAME_NORMALIZED.0 | VOLUME_NAME_DOS.0),
            )
        } as usize;
        match len {
            0 => return None,
            // Не влезло: вернулся нужный размер с завершающим нулём.
            len if len >= buf.len() => buf.resize(len, 0),
            len => return Some(strip_verbatim(&buf[..len])),
        }
    }
}

/// `\\?\C:\dir` → `C:\dir`, `\\?\UNC\server\share` → `\\server\share`: в индексе пути
/// обычного вида.
fn strip_verbatim(units: &[u16]) -> PathBuf {
    let unc: Vec<u16> = r"\\?\UNC\".encode_utf16().collect();
    let verbatim: Vec<u16> = r"\\?\".encode_utf16().collect();
    let path = if let Some(rest) = units.strip_prefix(unc.as_slice()) {
        let mut out: Vec<u16> = r"\\".encode_utf16().collect();
        out.extend_from_slice(rest);
        out
    } else {
        units.strip_prefix(verbatim.as_slice()).unwrap_or(units).to_vec()
    };
    PathBuf::from(OsString::from_wide(&path))
}

/// Описатель тома `\\.\C:` по корню `C:\`.
fn open_volume(root: &Path) -> Result<Owned<HANDLE>, String> {
    let letter = match root.components().next().map(|first| match first {
        Component::Prefix(prefix) => Some(prefix.kind()),
        _ => None,
    }) {
        Some(Some(Prefix::Disk(letter) | Prefix::VerbatimDisk(letter))) => letter,
        _ => return Err("журнал есть только у локальных дисков NTFS".into()),
    };
    let device = wide(format!(r"\\.\{}:", letter as char));
    // SAFETY: строка живёт до конца вызова; описатель закрывает Owned.
    unsafe {
        CreateFileW(
            PCWSTR(device.as_ptr()),
            GENERIC_READ.0,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            None,
            OPEN_EXISTING,
            FILE_FLAGS_AND_ATTRIBUTES(0),
            None,
        )
        .map(|handle| Owned::new(handle))
    }
    .map_err(|error| describe(&format!("не удалось открыть том {}", root.display()), &error))
}

fn query(volume: &Owned<HANDLE>, root: &Path) -> Result<USN_JOURNAL_DATA_V0, String> {
    // V0 — начало и V1, и V2: нужные поля одинаковы, а размер V0 принимают все версии NTFS.
    let mut data = USN_JOURNAL_DATA_V0::default();
    let mut bytes = 0u32;
    // SAFETY: выходной буфер — структура ровно того размера, что передан.
    unsafe {
        DeviceIoControl(
            **volume,
            FSCTL_QUERY_USN_JOURNAL,
            None,
            0,
            Some(&mut data as *mut _ as *mut _),
            size_of::<USN_JOURNAL_DATA_V0>() as u32,
            Some(&mut bytes),
            None,
        )
    }
    .map_err(|error| journal_error(root, &error))?;
    Ok(data)
}

fn journal_error(root: &Path, error: &windows::core::Error) -> String {
    if is_win32(error, ERROR_JOURNAL_NOT_ACTIVE.0) {
        return format!("журнал USN на диске {} выключен", root.display());
    }
    describe(&format!("не удалось прочитать журнал USN диска {}", root.display()), error)
}

fn is_win32(error: &windows::core::Error, code: u32) -> bool {
    error.code() == HRESULT::from_win32(code)
}
