//! Буфер обмена напрямую через Win32: `CF_HDROP` и `Preferred DropEffect`, как у Проводника.

use std::ffi::OsString;
use std::os::windows::ffi::OsStringExt;
use std::path::PathBuf;
use std::time::Duration;

use windows::Win32::Foundation::{GlobalFree, HANDLE, HGLOBAL, POINT};
use windows::Win32::System::DataExchange::{
    CloseClipboard, EmptyClipboard, GetClipboardData, IsClipboardFormatAvailable, OpenClipboard,
    RegisterClipboardFormatW, SetClipboardData,
};
use windows::Win32::System::Memory::{
    GMEM_MOVEABLE, GMEM_ZEROINIT, GlobalAlloc, GlobalLock, GlobalSize, GlobalUnlock,
};
use windows::Win32::System::Ole::{CF_HDROP, DROPEFFECT_COPY, DROPEFFECT_LINK, DROPEFFECT_MOVE};
use windows::Win32::UI::Shell::{DROPFILES, DragQueryFileW, HDROP};
use windows::core::{BOOL, w};

use super::com::owner_hwnd;
use crate::clipboard::ClipboardFiles;

pub fn set_files(paths: &[PathBuf], cut: bool) -> Result<(), String> {
    let drop_files = hdrop_bytes(paths);
    let effect = if cut { DROPEFFECT_MOVE.0 } else { (DROPEFFECT_COPY | DROPEFFECT_LINK).0 };
    let _open = Open::new()?;
    // SAFETY: буфер открыт нами; память после успешного SetClipboardData принадлежит системе.
    unsafe {
        EmptyClipboard().map_err(|error| format!("буфер обмена не очищен: {error}"))?;
        put(CF_HDROP.0 as u32, &drop_files)?;
        // Без этого формата вставка в Проводнике не отличит «вырезать» от «копировать»; список
        // файлов уже лежит, поэтому ошибку здесь не считаем фатальной.
        let _ = put(preferred_drop_effect(), &effect.to_le_bytes());
    }
    Ok(())
}

pub fn get_files() -> Option<ClipboardFiles> {
    // SAFETY: проверка формата не требует открытого буфера.
    unsafe { IsClipboardFormatAvailable(CF_HDROP.0 as u32) }.ok()?;
    let _open = Open::new().ok()?;
    // SAFETY: буфер открыт; данные принадлежат системе, мы их только читаем.
    unsafe {
        let handle = GetClipboardData(CF_HDROP.0 as u32).ok()?;
        let hdrop = HDROP(handle.0);
        let count = DragQueryFileW(hdrop, u32::MAX, None);
        let mut paths = Vec::with_capacity(count as usize);
        for i in 0..count {
            let len = DragQueryFileW(hdrop, i, None) as usize;
            let mut buf = vec![0u16; len + 1];
            let copied = DragQueryFileW(hdrop, i, Some(&mut buf)) as usize;
            if copied > 0 {
                paths.push(PathBuf::from(OsString::from_wide(&buf[..copied.min(len)])));
            }
        }
        if paths.is_empty() {
            return None;
        }
        // DROPEFFECT_MOVE без COPY — «вырезать». Нет формата — «копировать».
        let effect = read_effect().unwrap_or(DROPEFFECT_COPY.0);
        let cut = effect & DROPEFFECT_MOVE.0 != 0 && effect & DROPEFFECT_COPY.0 == 0;
        Some(ClipboardFiles { paths, cut })
    }
}

pub fn clear() {
    if let Ok(_open) = Open::new() {
        // SAFETY: буфер открыт нами.
        let _ = unsafe { EmptyClipboard() };
    }
}

/// `DROPFILES` и следом список путей UTF-16, каждый с нулём, плюс ещё один ноль в конце.
fn hdrop_bytes(paths: &[PathBuf]) -> Vec<u8> {
    let header = DROPFILES {
        pFiles: size_of::<DROPFILES>() as u32,
        pt: POINT::default(),
        fNC: BOOL(0),
        fWide: BOOL(1),
    };
    let mut bytes = Vec::new();
    // SAFETY: DROPFILES — простая C-структура без указателей и дыр.
    bytes.extend_from_slice(unsafe {
        std::slice::from_raw_parts(&header as *const DROPFILES as *const u8, size_of::<DROPFILES>())
    });
    for path in paths {
        for unit in super::com::wide(path) {
            bytes.extend_from_slice(&unit.to_le_bytes());
        }
    }
    bytes.extend_from_slice(&0u16.to_le_bytes());
    bytes
}

fn preferred_drop_effect() -> u32 {
    // SAFETY: регистрация формата по имени; повторный вызов вернёт тот же номер.
    unsafe { RegisterClipboardFormatW(w!("Preferred DropEffect")) }
}

/// Кладёт копию `data` в буфер обмена под форматом `format`. Буфер должен быть открыт.
unsafe fn put(format: u32, data: &[u8]) -> Result<(), String> {
    let error = |error: windows::core::Error| format!("буфер обмена недоступен: {error}");
    // SAFETY: выделенная память заполняется в своих границах; при ошибке освобождается нами,
    // при успехе ей владеет система.
    unsafe {
        let memory = GlobalAlloc(GMEM_MOVEABLE | GMEM_ZEROINIT, data.len()).map_err(error)?;
        let target = GlobalLock(memory) as *mut u8;
        if target.is_null() {
            let _ = GlobalFree(Some(memory));
            return Err("буфер обмена: нет памяти".into());
        }
        std::ptr::copy_nonoverlapping(data.as_ptr(), target, data.len());
        let _ = GlobalUnlock(memory);
        if let Err(e) = SetClipboardData(format, Some(HANDLE(memory.0))) {
            let _ = GlobalFree(Some(memory));
            return Err(error(e));
        }
    }
    Ok(())
}

/// Значение `Preferred DropEffect`. Буфер должен быть открыт.
unsafe fn read_effect() -> Option<u32> {
    // SAFETY: память принадлежит системе; читаем не больше её размера и только под замком.
    unsafe {
        let handle = GetClipboardData(preferred_drop_effect()).ok()?;
        let memory = HGLOBAL(handle.0);
        if GlobalSize(memory) < 4 {
            return None;
        }
        let ptr = GlobalLock(memory) as *const u8;
        if ptr.is_null() {
            return None;
        }
        let value = u32::from_le_bytes(std::ptr::read_unaligned(ptr as *const [u8; 4]));
        let _ = GlobalUnlock(memory);
        Some(value)
    }
}

/// Открытый буфер обмена; закрывается при уничтожении. Буфер — общий ресурс системы, и другая
/// программа может держать его открытым, поэтому несколько попыток.
struct Open;

impl Open {
    fn new() -> Result<Open, String> {
        let mut last = None;
        for attempt in 0..10 {
            if attempt > 0 {
                std::thread::sleep(Duration::from_millis(20));
            }
            // SAFETY: открытие буфера; закрытие — в Drop.
            match unsafe { OpenClipboard(owner_hwnd()) } {
                Ok(()) => return Ok(Open),
                Err(error) => last = Some(error),
            }
        }
        Err(match last {
            Some(error) => format!("буфер обмена занят другой программой: {error}"),
            None => "буфер обмена занят другой программой".into(),
        })
    }
}

impl Drop for Open {
    fn drop(&mut self) {
        // SAFETY: буфер открыт в Open::new.
        let _ = unsafe { CloseClipboard() };
    }
}
