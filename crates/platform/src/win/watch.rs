//! Наблюдатель за папкой: перекрытый `ReadDirectoryChangesW` в своём потоке.
//!
//! Буфер, `OVERLAPPED` и описатель папки принадлежат потоку наблюдателя: пока запрос к ядру не
//! завершён, их нельзя освобождать, а дожидаться этого должен тот, кто их держит. Поэтому
//! `Drop` только подаёт сигнал остановки и не ждёт поток — уничтожение не задерживает UI и не
//! может зависнуть, даже если его вызвали из самого обработчика событий.

use std::ffi::OsString;
use std::os::windows::ffi::OsStringExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use windows::Win32::Foundation::{CloseHandle, ERROR_NOTIFY_ENUM_DIR, HANDLE, WAIT_OBJECT_0};
use windows::Win32::Storage::FileSystem::{
    CreateFileW, FILE_ACTION_ADDED, FILE_ACTION_MODIFIED, FILE_ACTION_REMOVED,
    FILE_ACTION_RENAMED_NEW_NAME, FILE_ACTION_RENAMED_OLD_NAME, FILE_FLAG_BACKUP_SEMANTICS,
    FILE_FLAG_OVERLAPPED, FILE_LIST_DIRECTORY, FILE_NOTIFY_CHANGE_ATTRIBUTES,
    FILE_NOTIFY_CHANGE_CREATION, FILE_NOTIFY_CHANGE_DIR_NAME, FILE_NOTIFY_CHANGE_FILE_NAME,
    FILE_NOTIFY_CHANGE_LAST_WRITE, FILE_NOTIFY_CHANGE_SIZE, FILE_SHARE_DELETE, FILE_SHARE_READ,
    FILE_SHARE_WRITE, OPEN_EXISTING, ReadDirectoryChangesW,
};
use windows::Win32::System::IO::{CancelIoEx, GetOverlappedResult, OVERLAPPED};
use windows::Win32::System::Threading::{
    CreateEventW, INFINITE, ResetEvent, SetEvent, WaitForMultipleObjects,
};
use windows::core::{HRESULT, PCWSTR};

use super::com::{describe, wide};
use crate::watch::WatchEvent;

/// 64 КиБ — предел для сетевых папок. `u32`, чтобы буфер был выровнен по DWORD, как требует
/// `ReadDirectoryChangesW`.
const BUFFER_WORDS: usize = 64 * 1024 / 4;

type Callback = Box<dyn Fn(Vec<WatchEvent>) + Send + 'static>;

pub struct DirWatcher {
    stop: Arc<Handle>,
}

impl DirWatcher {
    pub fn new(dir: PathBuf, on_events: Callback) -> Result<DirWatcher, String> {
        let dir_w = wide(&dir);
        // SAFETY: строка живёт до конца вызова; описатель закрывает Handle.
        let handle = unsafe {
            CreateFileW(
                PCWSTR(dir_w.as_ptr()),
                FILE_LIST_DIRECTORY.0,
                FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
                None,
                OPEN_EXISTING,
                FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OVERLAPPED,
                None,
            )
        }
        .map(Handle)
        .map_err(|error| describe(&format!("не удалось следить за {}", dir.display()), &error))?;
        let io_event = new_event()?;
        let stop = Arc::new(new_event()?);

        let thread_stop = stop.clone();
        std::thread::Builder::new()
            .name("dir-watch".into())
            .spawn(move || run(&dir, &handle, &io_event, &thread_stop, on_events))
            .map_err(|error| format!("поток наблюдателя не запущен: {error}"))?;
        Ok(DirWatcher { stop })
    }
}

impl Drop for DirWatcher {
    fn drop(&mut self) {
        // SAFETY: событие живо, пока жив хотя бы один Arc.
        let _ = unsafe { SetEvent(self.stop.0) };
    }
}

fn run(dir: &Path, handle: &Handle, io_event: &Handle, stop: &Handle, on_events: Callback) {
    let mut buffer = vec![0u32; BUFFER_WORDS];
    let filter = FILE_NOTIFY_CHANGE_FILE_NAME
        | FILE_NOTIFY_CHANGE_DIR_NAME
        | FILE_NOTIFY_CHANGE_SIZE
        | FILE_NOTIFY_CHANGE_LAST_WRITE
        | FILE_NOTIFY_CHANGE_ATTRIBUTES
        | FILE_NOTIFY_CHANGE_CREATION;

    loop {
        // Не перемещается, пока запрос в ядре: каждый проход дожидается его завершения.
        let mut overlapped = OVERLAPPED { hEvent: io_event.0, ..Default::default() };
        // SAFETY: буфер и OVERLAPPED живут до завершения запроса: из цикла выходим только после
        // GetOverlappedResult (или если запрос не был поставлен).
        let queued = unsafe {
            let _ = ResetEvent(io_event.0);
            ReadDirectoryChangesW(
                handle.0,
                buffer.as_mut_ptr() as *mut _,
                (buffer.len() * 4) as u32,
                false,
                filter,
                None,
                Some(&mut overlapped),
                None,
            )
        };
        if queued.is_err() {
            // Папку удалили, отключили диск или пропал доступ.
            on_events(vec![WatchEvent::Stopped]);
            return;
        }

        // SAFETY: оба события живы.
        let woke = unsafe { WaitForMultipleObjects(&[io_event.0, stop.0], false, INFINITE) };
        let mut bytes = 0u32;
        if woke != WAIT_OBJECT_0 {
            // Остановка (или сбой ожидания): отменяем запрос и ждём, пока ядро отпустит буфер.
            // SAFETY: тот самый OVERLAPPED, что передан в ReadDirectoryChangesW.
            unsafe {
                let _ = CancelIoEx(handle.0, Some(&overlapped));
                let _ = GetOverlappedResult(handle.0, &overlapped, &mut bytes, true);
            }
            return;
        }
        // SAFETY: запрос уже завершён (событие сработало).
        if let Err(error) = unsafe { GetOverlappedResult(handle.0, &overlapped, &mut bytes, false) }
        {
            if error.code() == HRESULT::from_win32(ERROR_NOTIFY_ENUM_DIR.0) {
                on_events(vec![WatchEvent::Overflow]);
                continue;
            }
            on_events(vec![WatchEvent::Stopped]);
            return;
        }
        if bytes == 0 {
            // Буфер переполнился — изменения потеряны.
            on_events(vec![WatchEvent::Overflow]);
            continue;
        }
        // SAFETY: Vec<u32> можно читать как байты; длина не больше выделенной.
        let raw = unsafe {
            std::slice::from_raw_parts(
                buffer.as_ptr() as *const u8,
                (bytes as usize).min(buffer.len() * 4),
            )
        };
        let events = parse(dir, raw);
        if !events.is_empty() {
            on_events(events);
        }
    }
}

/// Разбирает цепочку `FILE_NOTIFY_INFORMATION`. Байты читаются с проверкой границ, без
/// приведения указателей к структуре.
fn parse(dir: &Path, raw: &[u8]) -> Vec<WatchEvent> {
    let u32_at = |at: usize| -> Option<u32> {
        raw.get(at..at + 4).map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    };
    let mut events = Vec::new();
    let mut renamed_from: Option<PathBuf> = None;
    let mut offset = 0usize;
    while let (Some(next), Some(action), Some(name_len)) =
        (u32_at(offset), u32_at(offset + 4), u32_at(offset + 8))
    {
        let start = offset + 12;
        let Some(name) = raw.get(start..start + name_len as usize) else { break };
        let units: Vec<u16> =
            name.chunks_exact(2).map(|pair| u16::from_le_bytes([pair[0], pair[1]])).collect();
        let path = dir.join(OsString::from_wide(&units));

        match windows::Win32::Storage::FileSystem::FILE_ACTION(action) {
            FILE_ACTION_ADDED => events.push(WatchEvent::Created(path)),
            FILE_ACTION_REMOVED => events.push(WatchEvent::Removed(path)),
            FILE_ACTION_MODIFIED => events.push(WatchEvent::Modified(path)),
            FILE_ACTION_RENAMED_OLD_NAME => {
                if let Some(orphan) = renamed_from.replace(path) {
                    events.push(WatchEvent::Removed(orphan));
                }
            }
            FILE_ACTION_RENAMED_NEW_NAME => match renamed_from.take() {
                Some(from) => events.push(WatchEvent::Renamed { from, to: path }),
                None => events.push(WatchEvent::Created(path)),
            },
            _ => {}
        }

        if next == 0 {
            break;
        }
        offset += next as usize;
    }
    // Старое имя без нового: объект ушёл из папки.
    if let Some(orphan) = renamed_from {
        events.push(WatchEvent::Removed(orphan));
    }
    events
}

fn new_event() -> Result<Handle, String> {
    // SAFETY: безымянное событие с ручным сбросом; закрывает Handle.
    unsafe { CreateEventW(None, true, false, PCWSTR::null()) }
        .map(Handle)
        .map_err(|error| describe("не удалось создать событие", &error))
}

/// Описатель ядра; закрывается при уничтожении.
struct Handle(HANDLE);

// SAFETY: описатели ядра не привязаны к потоку.
unsafe impl Send for Handle {}
unsafe impl Sync for Handle {}

impl Drop for Handle {
    fn drop(&mut self) {
        // SAFETY: описатель наш и закрывается один раз.
        let _ = unsafe { CloseHandle(self.0) };
    }
}
