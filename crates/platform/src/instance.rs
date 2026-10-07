//! Единственная копия программы: второй запуск передаёт свои аргументы (пути) уже
//! работающей копии и выходит.
//!
//! В Windows — именованный канал `\\.\pipe\<key>-<пользователь>-<сеанс>`, вне Windows — сокет
//! Unix в `$XDG_RUNTIME_DIR`. Первая копия держит сервер в фоновом потоке, следующие
//! подключаются к нему клиентом.
//!
//! Сообщение — один запуск: `u32` LE число аргументов, затем для каждого `u32` LE длина в
//! байтах и сам аргумент в UTF-8. Всё сообщение не больше [`MAX_MESSAGE`]. Сервер отвечает
//! одним байтом [`ACK`], как только прочитал сообщение целиком, и только потом зовёт
//! `on_message`: подтверждение значит «получено», медленный обработчик клиента не держит.

use std::io::{self, Read};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

/// Предел сообщения: столько путей за один запуск не передают, больше — мусор или атака.
pub(crate) const MAX_MESSAGE: usize = 1 << 20;

/// Ответ сервера «сообщение получено».
pub(crate) const ACK: u8 = 0x06;

/// Сколько клиент пытается достучаться до копии, которая только что запустилась.
pub(crate) const CONNECT_PATIENCE: Duration = Duration::from_millis(1500);

/// Сколько ждать сообщения от клиента и подтверждения от сервера.
pub(crate) const EXCHANGE_TIMEOUT: Duration = Duration::from_secs(2);

/// Сколько `Drop` сервера готов ждать его поток.
const STOP_PATIENCE: Duration = Duration::from_millis(200);

/// Обработчик аргументов очередного запуска.
pub type OnMessage = Box<dyn Fn(Vec<String>) + Send + 'static>;

/// Сервер единственной копии: пока жив, другие запуски передают ему свои аргументы.
pub struct InstanceServer {
    #[cfg(windows)]
    _inner: crate::win::instance::Server,
    #[cfg(all(unix, not(windows)))]
    _inner: unix::Server,
}

impl std::fmt::Debug for InstanceServer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("InstanceServer").finish_non_exhaustive()
    }
}

/// Итог [`claim`].
#[derive(Debug)]
pub enum Claim {
    /// Мы первая копия. Аргументы других запусков приходят в `on_message`.
    First(InstanceServer),
    /// Копия уже запущена: аргументы ей переданы, этому процессу надо выйти.
    Forwarded,
    /// Не получилось ни то, ни другое — работать как отдельная копия (текст — для журнала).
    Failed(String),
}

/// `key` — имя канала (вызывающий передаёт, например, "MH-Files"); к нему добавляется
/// имя пользователя и номер сеанса Windows, чтобы копии разных пользователей не мешали.
/// `args` — что передать уже запущенной копии (пути). `on_message` вызывается из фонового
/// потока сервера с аргументами очередного запуска.
///
/// Блокирует до ~1,5 с, если копия есть, но ещё не слушает (два запуска одновременно), и до
/// ~2 с в ожидании её подтверждения. Вызывать до создания окна.
pub fn claim(key: &str, args: &[String], on_message: OnMessage) -> Claim {
    let message = match encode(args) {
        Ok(message) => message,
        Err(error) => return Claim::Failed(error),
    };
    #[cfg(windows)]
    {
        match crate::win::instance::claim(key, &message, on_message) {
            Ok(Some(server)) => Claim::First(InstanceServer { _inner: server }),
            Ok(None) => Claim::Forwarded,
            Err(error) => Claim::Failed(error),
        }
    }
    #[cfg(all(unix, not(windows)))]
    {
        match unix::claim(key, &message, on_message) {
            Ok(Some(server)) => Claim::First(InstanceServer { _inner: server }),
            Ok(None) => Claim::Forwarded,
            Err(error) => Claim::Failed(error),
        }
    }
    #[cfg(not(any(windows, unix)))]
    {
        let _ = (key, message, on_message);
        Claim::Failed("единственная копия на этой системе не поддерживается".into())
    }
}

/// Имя для канала или файла: буквы, цифры, `-`, `_` и `.`; остальное — `_`. Пустое — `_`.
pub(crate) fn sanitize(part: &str) -> String {
    let clean: String = part
        .chars()
        .take(64)
        .map(|c| if c.is_alphanumeric() || matches!(c, '-' | '_' | '.') { c } else { '_' })
        .collect();
    if clean.is_empty() { "_".into() } else { clean }
}

/// Сообщение запуска; формат — в описании модуля.
pub(crate) fn encode(args: &[String]) -> Result<Vec<u8>, String> {
    let size = 4 + args.iter().map(|arg| 4 + arg.len()).sum::<usize>();
    if size > MAX_MESSAGE {
        return Err("аргументы запуска слишком длинные для передачи копии".into());
    }
    let mut message = Vec::with_capacity(size);
    message.extend_from_slice(&(args.len() as u32).to_le_bytes());
    for arg in args {
        message.extend_from_slice(&(arg.len() as u32).to_le_bytes());
        message.extend_from_slice(arg.as_bytes());
    }
    Ok(message)
}

/// Читает одно сообщение запуска. Длины проверяются до выделения памяти: чужой поток байт
/// не заставит выделить больше [`MAX_MESSAGE`].
pub(crate) fn decode(reader: &mut impl Read) -> Result<Vec<String>, String> {
    let too_long = || "сообщение копии слишком длинное".to_string();
    let mut budget = MAX_MESSAGE - 4;
    let count = read_u32(reader)? as usize;
    if count.saturating_mul(4) > budget {
        return Err(too_long());
    }
    let mut args = Vec::with_capacity(count);
    for _ in 0..count {
        budget = budget.checked_sub(4).ok_or_else(too_long)?;
        let len = read_u32(reader)? as usize;
        budget = budget.checked_sub(len).ok_or_else(too_long)?;
        let mut bytes = vec![0u8; len];
        reader.read_exact(&mut bytes).map_err(read_error)?;
        args.push(String::from_utf8(bytes).map_err(|_| "сообщение копии не в UTF-8".to_string())?);
    }
    Ok(args)
}

fn read_u32(reader: &mut impl Read) -> Result<u32, String> {
    let mut bytes = [0u8; 4];
    reader.read_exact(&mut bytes).map_err(read_error)?;
    Ok(u32::from_le_bytes(bytes))
}

fn read_error(error: io::Error) -> String {
    format!("не удалось прочитать сообщение копии: {error}")
}

/// Зовёт обработчик так, чтобы его паника не убила поток сервера.
pub(crate) fn deliver(on_message: &OnMessage, args: Vec<String>) {
    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| on_message(args)));
}

/// Останавливает поток сервера: `wake` выводит его из блокирующего вызова и повторяется,
/// пока поток не закончится, но не дольше [`STOP_PATIENCE`]. Не успел — поток бросается
/// (он выйдет сам, как только вернётся из вызова и увидит флаг остановки).
pub(crate) fn stop_thread(thread: Option<JoinHandle<()>>, mut wake: impl FnMut()) {
    let Some(thread) = thread else { return };
    let deadline = Instant::now() + STOP_PATIENCE;
    loop {
        if thread.is_finished() {
            let _ = thread.join();
            return;
        }
        if Instant::now() >= deadline {
            return;
        }
        wake();
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// Сокет Unix: только чтобы программу можно было запускать и проверять на Linux.
#[cfg(all(unix, not(windows)))]
mod unix {
    use std::io::{ErrorKind, Read, Write};
    use std::os::unix::net::{UnixListener, UnixStream};
    use std::path::PathBuf;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::thread::JoinHandle;
    use std::time::{Duration, Instant};

    use super::{ACK, CONNECT_PATIENCE, EXCHANGE_TIMEOUT, OnMessage, decode, deliver, sanitize};

    pub struct Server {
        path: PathBuf,
        stop: Arc<AtomicBool>,
        thread: Option<JoinHandle<()>>,
    }

    impl Drop for Server {
        fn drop(&mut self) {
            self.stop.store(true, Ordering::SeqCst);
            // Подключение будит `accept`; одного раза достаточно.
            let mut woken = false;
            super::stop_thread(self.thread.take(), || {
                if !woken {
                    woken = true;
                    let _ = UnixStream::connect(&self.path);
                }
            });
            let _ = std::fs::remove_file(&self.path);
        }
    }

    /// `Ok(Some)` — мы первые, `Ok(None)` — сообщение передано.
    pub fn claim(
        key: &str,
        message: &[u8],
        on_message: OnMessage,
    ) -> Result<Option<Server>, String> {
        let path = socket_path(key);
        let deadline = Instant::now() + CONNECT_PATIENCE;
        let mut on_message = Some(on_message);
        loop {
            match UnixStream::connect(&path) {
                Ok(stream) => return send(stream, message).map(|()| None),
                // Сокета нет или он остался от упавшей копии — занять место.
                Err(error)
                    if matches!(
                        error.kind(),
                        ErrorKind::NotFound | ErrorKind::ConnectionRefused
                    ) =>
                {
                    if error.kind() == ErrorKind::ConnectionRefused {
                        let _ = std::fs::remove_file(&path);
                    }
                    match UnixListener::bind(&path) {
                        Ok(listener) => {
                            let Some(on_message) = on_message.take() else {
                                return Err("сервер копии уже запускался".into());
                            };
                            return serve(listener, path, on_message).map(Some);
                        }
                        // Другой запуск занял место между connect и bind — к нему и подключиться.
                        Err(error) if error.kind() == ErrorKind::AddrInUse => {}
                        Err(error) => {
                            return Err(format!(
                                "не удалось создать сокет {}: {error}",
                                path.display()
                            ));
                        }
                    }
                }
                Err(error) => {
                    return Err(format!("не удалось подключиться к копии: {error}"));
                }
            }
            if Instant::now() >= deadline {
                return Err("копия программы не отвечает".into());
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    fn send(mut stream: UnixStream, message: &[u8]) -> Result<(), String> {
        let _ = stream.set_write_timeout(Some(EXCHANGE_TIMEOUT));
        let _ = stream.set_read_timeout(Some(EXCHANGE_TIMEOUT));
        stream
            .write_all(message)
            .map_err(|error| format!("не удалось передать аргументы копии: {error}"))?;
        let mut ack = [0u8; 1];
        match stream.read_exact(&mut ack) {
            Ok(()) if ack[0] == ACK => Ok(()),
            Ok(()) => Err("копия ответила непонятно".into()),
            Err(error) => Err(format!("копия не подтвердила получение: {error}")),
        }
    }

    fn serve(
        listener: UnixListener,
        path: PathBuf,
        on_message: OnMessage,
    ) -> Result<Server, String> {
        let stop = Arc::new(AtomicBool::new(false));
        let flag = stop.clone();
        let thread = std::thread::Builder::new().name("instance".into()).spawn(move || {
            for stream in listener.incoming() {
                if flag.load(Ordering::SeqCst) {
                    break;
                }
                let Ok(mut stream) = stream else { continue };
                let _ = stream.set_read_timeout(Some(EXCHANGE_TIMEOUT));
                let _ = stream.set_write_timeout(Some(EXCHANGE_TIMEOUT));
                let Ok(args) = decode(&mut stream) else { continue };
                let _ = stream.write_all(&[ACK]);
                drop(stream);
                deliver(&on_message, args);
            }
        });
        match thread {
            Ok(thread) => Ok(Server { path, stop, thread: Some(thread) }),
            Err(error) => {
                let _ = std::fs::remove_file(&path);
                Err(format!("не удалось запустить поток копии: {error}"))
            }
        }
    }

    /// `$XDG_RUNTIME_DIR/<key>.sock` (папка уже личная), иначе временная папка и имя
    /// пользователя в имени файла.
    fn socket_path(key: &str) -> PathBuf {
        let key = sanitize(key);
        match std::env::var_os("XDG_RUNTIME_DIR").filter(|dir| !dir.is_empty()) {
            Some(dir) => PathBuf::from(dir).join(format!("{key}.sock")),
            None => {
                let user = std::env::var("USER")
                    .or_else(|_| std::env::var("LOGNAME"))
                    .unwrap_or_else(|_| "user".into());
                std::env::temp_dir().join(format!("{key}-{}.sock", sanitize(&user)))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;

    fn key(name: &str) -> String {
        format!("mh-files-test-{name}-{}", std::process::id())
    }

    #[test]
    fn message_round_trip_keeps_non_ascii() {
        let args = vec![r"C:\Документы\отчёт.txt".to_string(), String::new(), "a b".to_string()];
        let message = encode(&args).unwrap();
        assert_eq!(decode(&mut message.as_slice()).unwrap(), args);
        assert_eq!(decode(&mut encode(&[]).unwrap().as_slice()).unwrap(), Vec::<String>::new());
    }

    #[test]
    fn oversized_or_truncated_messages_are_rejected() {
        let huge = vec!["x".repeat(MAX_MESSAGE)];
        assert!(encode(&huge).is_err());
        // Заявлено больше, чем можно: отказ до выделения памяти.
        let mut lying = Vec::new();
        lying.extend_from_slice(&1u32.to_le_bytes());
        lying.extend_from_slice(&u32::MAX.to_le_bytes());
        assert!(decode(&mut lying.as_slice()).is_err());
        let many = u32::MAX.to_le_bytes();
        assert!(decode(&mut many.as_slice()).is_err());
        let message = encode(&["путь".to_string()]).unwrap();
        assert!(decode(&mut &message[..message.len() - 1]).is_err());
    }

    #[test]
    fn sanitize_keeps_letters_and_digits() {
        assert_eq!(sanitize(r"MH Files\Пётр:1"), "MH_Files_Пётр_1");
        assert_eq!(sanitize(""), "_");
    }

    #[test]
    fn second_claim_forwards_args_to_first() {
        let key = key("forward");
        let (tx, rx) = mpsc::channel();
        let server = match claim(&key, &[], Box::new(move |args| tx.send(args).unwrap_or(()))) {
            Claim::First(server) => server,
            other => panic!("ожидалась первая копия: {other:?}"),
        };
        let args = vec![r"C:\Папка с пробелами".to_string(), "D:\\".to_string()];
        let second = claim(&key, &args, Box::new(|_| panic!("второй сервер не нужен")));
        assert!(matches!(second, Claim::Forwarded), "{second:?}");
        assert_eq!(rx.recv_timeout(Duration::from_secs(5)).unwrap(), args);
        drop(server);
    }

    #[test]
    fn claim_is_first_again_after_drop() {
        let key = key("again");
        let first = claim(&key, &[], Box::new(|_| {}));
        assert!(matches!(first, Claim::First(_)), "{first:?}");
        drop(first);
        let again = claim(&key, &["x".to_string()], Box::new(|_| {}));
        assert!(matches!(again, Claim::First(_)), "{again:?}");
    }
}
