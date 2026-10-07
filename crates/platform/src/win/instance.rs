//! Единственная копия через именованный канал `\\.\pipe\<key>-<пользователь>-<сеанс>`.
//!
//! Первая копия создаёт канал с `FILE_FLAG_FIRST_PIPE_INSTANCE`: второй такой вызов (из другого
//! процесса) получает `ERROR_ACCESS_DENIED` или `ERROR_PIPE_BUSY` и становится клиентом.
//! Экземпляр канала один: сервер принимает клиентов по очереди, остальные ждут в
//! `WaitNamedPipeW`. Чтение с обеих сторон — опросом `PeekNamedPipe` со сроком: зависший
//! клиент не держит сервер, зависший сервер не держит запуск.
//!
//! Разрешения канала по умолчанию: писать в него может только сам пользователь (и
//! администраторы), удалённые клиенты отклоняются.

use std::fs::{File, OpenOptions};
use std::io::{self, Read, Write};
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use windows::Win32::Foundation::{
    ERROR_ACCESS_DENIED, ERROR_BROKEN_PIPE, ERROR_FILE_NOT_FOUND, ERROR_PIPE_BUSY,
    ERROR_PIPE_CONNECTED, HANDLE,
};
use windows::Win32::Storage::FileSystem::{FILE_FLAG_FIRST_PIPE_INSTANCE, PIPE_ACCESS_DUPLEX};
use windows::Win32::System::IO::CancelSynchronousIo;
use windows::Win32::System::Pipes::{
    ConnectNamedPipe, CreateNamedPipeW, DisconnectNamedPipe, PIPE_READMODE_BYTE,
    PIPE_REJECT_REMOTE_CLIENTS, PIPE_TYPE_BYTE, PIPE_WAIT, PeekNamedPipe, WaitNamedPipeW,
};
use windows::Win32::System::RemoteDesktop::ProcessIdToSessionId;
use windows::Win32::System::Threading::GetCurrentProcessId;
use windows::Win32::System::WindowsProgramming::GetUserNameW;
use windows::Win32::UI::WindowsAndMessaging::{ASFW_ANY, AllowSetForegroundWindow};
use windows::core::{PCWSTR, PWSTR};

use super::com::wide;
use crate::instance::{
    ACK, CONNECT_PATIENCE, EXCHANGE_TIMEOUT, OnMessage, decode, deliver, sanitize, stop_thread,
};

/// Буфер канала на приём: обычное сообщение (несколько путей) влезает целиком.
const IN_BUFFER: u32 = 64 * 1024;

/// Шаг опроса канала.
const POLL: Duration = Duration::from_millis(5);

pub struct Server {
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl Drop for Server {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        let thread = self.thread.take();
        let raw = thread.as_ref().map(|thread| HANDLE(thread.as_raw_handle()));
        stop_thread(thread, || {
            if let Some(raw) = raw {
                // SAFETY: описатель потока жив, пока жив JoinHandle, а тот живёт внутри
                // stop_thread до конца её работы. Отмена только прерывает блокирующий
                // ConnectNamedPipe; повторяется, если поток ещё не успел в него войти.
                let _ = unsafe { CancelSynchronousIo(raw) };
            }
        });
    }
}

/// `Ok(Some)` — мы первые, `Ok(None)` — сообщение передано.
pub fn claim(key: &str, message: &[u8], on_message: OnMessage) -> Result<Option<Server>, String> {
    let name = pipe_name(key);
    let deadline = Instant::now() + CONNECT_PATIENCE;
    let mut on_message = Some(on_message);
    loop {
        match create_first(&name) {
            Ok(pipe) => {
                let Some(on_message) = on_message.take() else {
                    return Err("сервер копии уже запускался".into());
                };
                return serve(pipe, on_message).map(Some);
            }
            // Канал уже есть — значит, есть и копия.
            Err(code) if code == ERROR_ACCESS_DENIED.0 || code == ERROR_PIPE_BUSY.0 => {}
            Err(code) => {
                return Err(format!(
                    "не удалось создать канал копии: {}",
                    io::Error::from_raw_os_error(code as i32)
                ));
            }
        }
        match connect(&name) {
            Ok(Some(pipe)) => return send(pipe, message).map(|()| None),
            // Канал занят или копия только что закрылась — попробовать ещё раз.
            Ok(None) => {}
            Err(error) => return Err(error),
        }
        if Instant::now() >= deadline {
            return Err("копия программы не отвечает".into());
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// Первый экземпляр канала. Ошибка — код Win32.
fn create_first(name: &str) -> Result<File, u32> {
    let name = wide(name);
    // SAFETY: имя живёт до конца вызова; описатель сразу переходит во владение File.
    let handle = unsafe {
        CreateNamedPipeW(
            PCWSTR(name.as_ptr()),
            PIPE_ACCESS_DUPLEX | FILE_FLAG_FIRST_PIPE_INSTANCE,
            PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS,
            1,
            4096,
            IN_BUFFER,
            0,
            None,
        )
    };
    if handle.is_invalid() {
        return Err(io::Error::last_os_error().raw_os_error().unwrap_or(0) as u32);
    }
    // SAFETY: описатель только что создан, действителен и больше никому не принадлежит.
    Ok(File::from(unsafe { OwnedHandle::from_raw_handle(handle.0) }))
}

/// Клиентский конец канала. `Ok(None)` — канал занят или исчез, стоит попробовать снова.
fn connect(name: &str) -> Result<Option<File>, String> {
    match OpenOptions::new().read(true).write(true).open(name) {
        Ok(pipe) => Ok(Some(pipe)),
        Err(error) => match error.raw_os_error().map(|code| code as u32) {
            Some(code) if code == ERROR_PIPE_BUSY.0 => {
                let name = wide(name);
                // SAFETY: имя живёт до конца вызова. Ждёт свободный экземпляр не дольше 200 мс.
                let _ = unsafe { WaitNamedPipeW(PCWSTR(name.as_ptr()), 200) };
                Ok(None)
            }
            Some(code) if code == ERROR_FILE_NOT_FOUND.0 => Ok(None),
            // ERROR_ACCESS_DENIED здесь — копия запущена от имени администратора, а мы нет
            // (или наоборот): передать ей ничего нельзя.
            _ => Err(format!("не удалось подключиться к копии: {error}")),
        },
    }
}

fn send(pipe: File, message: &[u8]) -> Result<(), String> {
    // Работающая копия получит право вывести своё окно на передний план: это право есть у
    // только что запущенного процесса, но не у фоновой копии.
    // SAFETY: функция без указателей; ошибка (права нет и у нас) не мешает передаче.
    let _ = unsafe { AllowSetForegroundWindow(ASFW_ANY) };
    (&pipe)
        .write_all(message)
        .map_err(|error| format!("не удалось передать аргументы копии: {error}"))?;
    let mut ack = [0u8; 1];
    let mut reader = PipeReader::new(&pipe, None);
    match reader.read_exact(&mut ack) {
        Ok(()) if ack[0] == ACK => Ok(()),
        Ok(()) => Err("копия ответила непонятно".into()),
        Err(error) => Err(format!("копия не подтвердила получение: {error}")),
    }
}

fn serve(pipe: File, on_message: OnMessage) -> Result<Server, String> {
    let stop = Arc::new(AtomicBool::new(false));
    let flag = stop.clone();
    let thread = std::thread::Builder::new()
        .name("instance".into())
        .spawn(move || listen(pipe, &flag, on_message))
        .map_err(|error| format!("не удалось запустить поток копии: {error}"))?;
    Ok(Server { stop, thread: Some(thread) })
}

/// Цикл сервера: ждать клиента, прочитать сообщение, подтвердить, дождаться, пока клиент
/// закроет свой конец, отдать обработчику, отключить. Канал закрывается вместе с потоком.
fn listen(pipe: File, stop: &AtomicBool, on_message: OnMessage) {
    let handle = HANDLE(pipe.as_raw_handle());
    while !stop.load(Ordering::SeqCst) {
        // SAFETY: описатель канала принадлежит `pipe` и жив весь цикл. Вызов блокирующий;
        // Drop сервера прерывает его через CancelSynchronousIo.
        let connected = match unsafe { ConnectNamedPipe(handle, None) } {
            Ok(()) => true,
            // Клиент мог подключиться между созданием экземпляра и ConnectNamedPipe.
            Err(error) => error.code() == ERROR_PIPE_CONNECTED.to_hresult(),
        };
        if stop.load(Ordering::SeqCst) {
            break;
        }
        if connected {
            let mut reader = PipeReader::new(&pipe, Some(stop));
            if let Ok(args) = decode(&mut reader)
                && (&pipe).write_all(&[ACK]).is_ok()
            {
                // DisconnectNamedPipe выбрасывает непрочитанное: сначала клиент забирает ответ
                // и закрывает свой конец, потом — обработчик (он может быть небыстрым).
                reader.wait_closed();
                deliver(&on_message, args);
            }
        } else {
            // Клиент пришёл и сразу ушёл или вызов прерван: не крутиться впустую.
            std::thread::sleep(Duration::from_millis(20));
        }
        // SAFETY: описатель жив; отключение готовит экземпляр к следующему клиенту.
        let _ = unsafe { DisconnectNamedPipe(handle) };
    }
}

/// Чтение из канала со сроком: `ReadFile` зовётся, только когда данные уже есть, иначе
/// `PeekNamedPipe` раз в [`POLL`] до срока или флага остановки.
struct PipeReader<'a> {
    pipe: &'a File,
    stop: Option<&'a AtomicBool>,
    deadline: Instant,
}

impl<'a> PipeReader<'a> {
    fn new(pipe: &'a File, stop: Option<&'a AtomicBool>) -> PipeReader<'a> {
        PipeReader { pipe, stop, deadline: Instant::now() + EXCHANGE_TIMEOUT }
    }

    /// Байт в канале; `None` — другой конец закрыт или канал сломан.
    fn available(&self) -> Option<u32> {
        let mut total = 0u32;
        // SAFETY: описатель жив, пока жив `pipe`; буфер не передаётся, только счётчик.
        let peek = unsafe {
            PeekNamedPipe(HANDLE(self.pipe.as_raw_handle()), None, 0, None, Some(&mut total), None)
        };
        peek.ok().map(|()| total)
    }

    fn stopped(&self) -> bool {
        self.stop.is_some_and(|stop| stop.load(Ordering::SeqCst)) || Instant::now() >= self.deadline
    }

    /// Ждёт, пока клиент закроет свой конец (до срока).
    fn wait_closed(&self) {
        while self.available().is_some() && !self.stopped() {
            std::thread::sleep(POLL);
        }
    }
}

impl Read for PipeReader<'_> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        loop {
            match self.available() {
                // Закрытый конец — конец данных.
                None => return Ok(0),
                Some(0) => {}
                Some(total) => {
                    let len = buf.len().min(total as usize);
                    let mut pipe = self.pipe;
                    return match pipe.read(&mut buf[..len]) {
                        Err(error) if error.raw_os_error() == Some(ERROR_BROKEN_PIPE.0 as i32) => {
                            Ok(0)
                        }
                        other => other,
                    };
                }
            }
            if self.stopped() {
                return Err(io::Error::new(io::ErrorKind::TimedOut, "истёк срок ожидания"));
            }
            std::thread::sleep(POLL);
        }
    }
}

/// `\\.\pipe\<key>-<пользователь>-<сеанс>`. Каналы общие для всех сеансов
/// машины, поэтому в имени и пользователь, и сеанс.
fn pipe_name(key: &str) -> String {
    format!(r"\\.\pipe\{}-{}-{}", sanitize(key), sanitize(&user_name()), session_id())
}

fn user_name() -> String {
    let mut buffer = [0u16; 257];
    let mut len = buffer.len() as u32;
    // SAFETY: буфер живёт до конца вызова, len — его размер в символах.
    let got = unsafe { GetUserNameW(Some(PWSTR(buffer.as_mut_ptr())), &mut len) };
    match got {
        // len — с завершающим нулём.
        Ok(()) => String::from_utf16_lossy(&buffer[..(len as usize).saturating_sub(1)]),
        Err(_) => std::env::var("USERNAME").unwrap_or_else(|_| "user".into()),
    }
}

fn session_id() -> u32 {
    let mut session = 0u32;
    // SAFETY: функции без входных указателей; результат пишется в локальную переменную.
    let _ = unsafe { ProcessIdToSessionId(GetCurrentProcessId(), &mut session) };
    session
}
