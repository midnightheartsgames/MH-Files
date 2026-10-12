//! `llama-server` с моделью: запуск, ожидание готовности, запросы векторов.
//!
//! Сервер слушает только `127.0.0.1` на свободном порту. HTTP здесь свой, на `TcpStream`:
//! запрос — JSON, ответ — JSON, соединение на запрос (`Connection: close`); тянуть ради
//! этого HTTP-библиотеку незачем.

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use mh_files_core::semantic::model::{self, Input, ServerConfig};
use mh_files_platform::process::Background;

/// Сколько ждать загрузки модели: первый запуск с диска HDD бывает небыстрым.
const START_TIMEOUT: Duration = Duration::from_secs(180);
/// Сколько ждать ответа на запрос векторов: пачка картинок на слабом процессоре — секунды.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(300);
/// Ответ больше — что-то не так.
const RESPONSE_LIMIT: usize = 64 << 20;

/// Работающий сервер. Уничтожение объекта завершает процесс.
pub struct Server {
    /// `None` — только в тестах: сервер поддельный и живёт сам.
    process: Option<Background>,
    pub port: u16,
    /// Запущен на процессоре, хотя просили видеокарту: с ней не запустился.
    pub gpu_fallback: bool,
}

impl Server {
    /// Запустить и дождаться готовности. Не запустился с видеокартой — ещё раз на
    /// процессоре: драйвер Vulkan бывает капризным, а поиск важнее скорости.
    pub fn start(exe: &Path, config: &ServerConfig, log: &Path) -> Result<Server, String> {
        #[cfg(test)]
        if let Some(port) = *TEST_PORT.lock() {
            return Ok(Server { process: None, port, gpu_fallback: false });
        }
        match Server::start_once(exe, config, log) {
            Ok(server) => Ok(server),
            Err(error) if config.gpu => {
                let cpu = ServerConfig { gpu: false, ..config.clone() };
                let mut server = Server::start_once(exe, &cpu, log)
                    .map_err(|second| format!("{error}; на процессоре тоже: {second}"))?;
                server.gpu_fallback = true;
                Ok(server)
            }
            Err(error) => Err(error),
        }
    }

    fn start_once(exe: &Path, config: &ServerConfig, log: &Path) -> Result<Server, String> {
        let port = free_port()?;
        let config = ServerConfig { port, ..config.clone() };
        let mut process = Background::spawn(exe, &model::server_args(&config), Some(log))?;
        let started = Instant::now();
        loop {
            if let Some(status) = process.exited() {
                return Err(format!("llama-server завершился ({status}){}", log_tail(log)));
            }
            // 503 — модель грузится; нет соединения — сервер ещё не открыл порт.
            if let Ok((200, _)) = request(port, "GET", "/health", None, Duration::from_secs(2)) {
                return Ok(Server { process: Some(process), port, gpu_fallback: false });
            }
            if started.elapsed() > START_TIMEOUT {
                return Err(format!(
                    "модель не загрузилась за {} с{}",
                    START_TIMEOUT.as_secs(),
                    log_tail(log)
                ));
            }
            std::thread::sleep(Duration::from_millis(150));
        }
    }

    /// Процесс жив.
    pub fn alive(&mut self) -> bool {
        self.process.as_mut().is_none_or(|p| p.exited().is_none())
    }

    /// Векторы для входов (обрезанные и нормированные), в их порядке.
    pub fn embed(&self, inputs: &[Input]) -> Result<Vec<Vec<f32>>, EmbedError> {
        embed(self.port, inputs)
    }
}

/// Порт поддельного сервера вместо запуска `llama-server` — для тестов службы.
#[cfg(test)]
static TEST_PORT: parking_lot::Mutex<Option<u16>> = parking_lot::Mutex::new(None);

#[cfg(test)]
pub fn set_test_port(port: Option<u16>) {
    *TEST_PORT.lock() = port;
}

/// Почему не вышло посчитать векторы.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EmbedError {
    /// Сервер не отвечает: упал или его выключили.
    Unreachable(String),
    /// Сервер ответил ошибкой на этот вход (битая картинка, слишком длинный текст).
    Refused(String),
}

impl std::fmt::Display for EmbedError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            EmbedError::Unreachable(text) | EmbedError::Refused(text) => f.write_str(text),
        }
    }
}

pub fn embed(port: u16, inputs: &[Input]) -> Result<Vec<Vec<f32>>, EmbedError> {
    if inputs.is_empty() {
        return Ok(Vec::new());
    }
    let body = model::embeddings_request(inputs);
    let (status, text) = request(port, "POST", "/v1/embeddings", Some(&body), REQUEST_TIMEOUT)
        .map_err(EmbedError::Unreachable)?;
    match status {
        200 => model::parse_embeddings(&text, inputs.len()).map_err(EmbedError::Refused),
        // «Модель грузится» после перезапуска — как недоступный сервер.
        503 => Err(EmbedError::Unreachable("модель ещё загружается".into())),
        _ => Err(EmbedError::Refused(
            model::parse_embeddings(&text, inputs.len())
                .err()
                .unwrap_or_else(|| format!("сервер ответил кодом {status}")),
        )),
    }
}

/// Свободный порт: занять нулевой и сразу отпустить. Гонка с другой программой возможна,
/// но тогда сервер не запустится и будет второй заход.
fn free_port() -> Result<u16, String> {
    let listener = std::net::TcpListener::bind("127.0.0.1:0")
        .map_err(|e| format!("нет свободного порта: {e}"))?;
    listener.local_addr().map(|a| a.port()).map_err(|e| e.to_string())
}

/// Последние строки журнала сервера с ошибкой — в текст для пользователя.
fn log_tail(log: &Path) -> String {
    let Ok(text) = std::fs::read_to_string(log) else { return String::new() };
    let lines: Vec<&str> = text
        .lines()
        .filter(|l| {
            let l = l.to_lowercase();
            l.contains("error") || l.contains("failed") || l.contains("unknown")
        })
        .collect();
    match lines.last() {
        Some(line) => format!(": {}", line.trim().chars().take(200).collect::<String>()),
        None => String::new(),
    }
}

/// HTTP/1.1 к `127.0.0.1:port`: код ответа и тело.
pub fn request(
    port: u16,
    method: &str,
    path: &str,
    body: Option<&str>,
    timeout: Duration,
) -> Result<(u16, String), String> {
    let address = SocketAddr::from(([127, 0, 0, 1], port));
    let mut stream = TcpStream::connect_timeout(&address, Duration::from_secs(2))
        .map_err(|e| format!("сервер модели недоступен: {e}"))?;
    let _ = stream.set_read_timeout(Some(timeout));
    let _ = stream.set_write_timeout(Some(timeout));
    let body = body.unwrap_or("");
    let head = format!(
        "{method} {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    stream
        .write_all(head.as_bytes())
        .and_then(|_| stream.write_all(body.as_bytes()))
        .map_err(|e| format!("запрос к модели не отправлен: {e}"))?;
    let mut response = Vec::new();
    stream
        .take(RESPONSE_LIMIT as u64 + 1)
        .read_to_end(&mut response)
        .map_err(|e| format!("модель не ответила: {e}"))?;
    if response.len() > RESPONSE_LIMIT {
        return Err("ответ модели слишком большой".into());
    }
    parse_response(&response)
}

/// Разобрать ответ HTTP/1.1: код, заголовки, тело (с `Content-Length` или кусками).
fn parse_response(bytes: &[u8]) -> Result<(u16, String), String> {
    let broken = || "непонятный ответ сервера модели".to_string();
    let split = bytes.windows(4).position(|w| w == b"\r\n\r\n").ok_or_else(broken)?;
    let head = std::str::from_utf8(&bytes[..split]).map_err(|_| broken())?;
    let mut lines = head.split("\r\n");
    let status: u16 = lines
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|code| code.parse().ok())
        .ok_or_else(broken)?;
    let mut chunked = false;
    let mut length = None;
    for line in lines {
        let Some((name, value)) = line.split_once(':') else { continue };
        let (name, value) = (name.trim().to_ascii_lowercase(), value.trim());
        if name == "transfer-encoding" && value.to_ascii_lowercase().contains("chunked") {
            chunked = true;
        } else if name == "content-length" {
            length = value.parse::<usize>().ok();
        }
    }
    let rest = &bytes[split + 4..];
    let body = if chunked {
        let mut body = Vec::new();
        let mut at = 0;
        loop {
            let line_end = rest[at..].windows(2).position(|w| w == b"\r\n").ok_or_else(broken)?;
            let size_text = std::str::from_utf8(&rest[at..at + line_end]).map_err(|_| broken())?;
            let size_text = size_text.split(';').next().unwrap_or("").trim();
            let size = usize::from_str_radix(size_text, 16).map_err(|_| broken())?;
            at += line_end + 2;
            if size == 0 {
                break;
            }
            body.extend_from_slice(rest.get(at..at + size).ok_or_else(broken)?);
            at += size + 2;
        }
        body
    } else {
        let end = length.map_or(rest.len(), |n| n.min(rest.len()));
        rest[..end].to_vec()
    };
    Ok((status, String::from_utf8_lossy(&body).into_owned()))
}

/// Где лежит `llama-server`: свой путь из настроек, скачанный MH Files или найденный в
/// `PATH` (вне Windows — для разработки).
pub fn find_exe(custom: &Path, downloaded: Option<PathBuf>) -> Option<PathBuf> {
    if !custom.as_os_str().is_empty() {
        return custom.is_file().then(|| custom.to_path_buf());
    }
    if let Some(path) = downloaded.filter(|p| p.is_file()) {
        return Some(path);
    }
    let name = if cfg!(windows) { "llama-server.exe" } else { "llama-server" };
    std::env::var_os("PATH")
        .map(|paths| std::env::split_paths(&paths).map(|dir| dir.join(name)).collect::<Vec<_>>())
        .unwrap_or_default()
        .into_iter()
        .find(|p| p.is_file())
}

#[cfg(test)]
pub mod tests {
    use std::io::BufRead;
    use std::net::TcpListener;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;

    #[test]
    fn responses_parse() {
        let plain = b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 7\r\n\r\n{\"a\":1}extra";
        assert_eq!(parse_response(plain).unwrap(), (200, "{\"a\":1}".to_string()));
        let chunked = b"HTTP/1.1 503 Service Unavailable\r\nTransfer-Encoding: chunked\r\n\r\n4\r\n{\"b\"\r\n3;x=1\r\n:2}\r\n0\r\n\r\n";
        assert_eq!(parse_response(chunked).unwrap(), (503, "{\"b\":2}".to_string()));
        let until_close = "HTTP/1.0 200 OK\r\n\r\nтело".as_bytes();
        assert_eq!(parse_response(until_close).unwrap().1, "тело");
        assert!(parse_response(b"garbage").is_err());
        assert!(
            parse_response(b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\nzz\r\n").is_err()
        );
    }

    /// Поддельный `llama-server`: отвечает на `/health` и `/v1/embeddings`; вектор входа —
    /// по первому слову текста (или «картинка» для картинок), чтобы в тестах были
    /// предсказуемые близости. Возвращает порт и счётчик запросов векторов.
    pub fn fake_server() -> (u16, Arc<AtomicUsize>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let calls = Arc::new(AtomicUsize::new(0));
        let counter = calls.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(stream) = stream else { continue };
                let counter = counter.clone();
                std::thread::spawn(move || serve(stream, &counter));
            }
        });
        (port, calls)
    }

    fn serve(stream: TcpStream, calls: &AtomicUsize) {
        let mut reader = std::io::BufReader::new(stream.try_clone().unwrap());
        let mut first = String::new();
        reader.read_line(&mut first).unwrap();
        let mut length = 0;
        loop {
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            if line.trim().is_empty() {
                break;
            }
            if let Some(value) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                length = value.trim().parse().unwrap();
            }
        }
        let mut body = vec![0; length];
        reader.read_exact(&mut body).unwrap();
        let (status, reply) = if first.starts_with("GET /health") {
            (200, r#"{"status":"ok"}"#.to_string())
        } else {
            calls.fetch_add(1, Ordering::SeqCst);
            let request: serde_json::Value = serde_json::from_slice(&body).unwrap();
            let mut data = Vec::new();
            for (index, input) in request["input"].as_array().unwrap().iter().enumerate() {
                let text = match input.as_str() {
                    Some(text) => text.rsplit(": ").next().unwrap_or("").to_string(),
                    None if input.to_string().contains("image_url") => "картинка".into(),
                    None => "звук".into(),
                };
                if text.contains("СЛОМАННЫЙ") {
                    let error = r#"{"error":{"code":500,"message":"Failed to load image","type":"server_error"}}"#;
                    reply_with(&stream, 500, error);
                    return;
                }
                data.push(serde_json::json!({"index": index, "embedding": word_vector(&text)}));
            }
            (200, serde_json::json!({"data": data}).to_string())
        };
        reply_with(&stream, status, &reply);
    }

    fn reply_with(mut stream: &TcpStream, status: u16, body: &str) {
        let head = format!(
            "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n",
            body.len()
        );
        let _ = stream.write_all(head.as_bytes());
        let _ = stream.write_all(body.as_bytes());
    }

    /// Вектор текста: ось — по первой букве первого слова, плюс общий фон.
    pub fn word_vector(text: &str) -> Vec<f32> {
        let mut v = vec![0.05f32; 768];
        let first = text.split_whitespace().next().unwrap_or("").to_lowercase();
        let axis = first.chars().next().map_or(0, |c| c as usize % 500);
        v[axis] = 1.0;
        v
    }

    #[test]
    fn embeddings_from_a_server() {
        let (port, calls) = fake_server();
        assert_eq!(request(port, "GET", "/health", None, Duration::from_secs(5)).unwrap().0, 200);
        let vectors = embed(
            port,
            &[
                Input::Text("task: search result | query: кот".into()),
                Input::Image { mime: "image/png", base64: "AA==".into() },
            ],
        )
        .unwrap();
        assert_eq!(vectors.len(), 2);
        assert_eq!(vectors[0].len(), mh_files_core::semantic::DIMS);
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        let refused = embed(port, &[Input::Text("СЛОМАННЫЙ".into())]).unwrap_err();
        assert_eq!(refused, EmbedError::Refused("модель: Failed to load image".into()));
        let closed = free_port().unwrap();
        assert!(matches!(
            embed(closed, &[Input::Text("x".into())]),
            Err(EmbedError::Unreachable(_))
        ));
    }

    #[test]
    fn server_that_dies_is_reported() {
        if !cfg!(unix) {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("log.txt");
        // «Сервер», который пишет ошибку и выходит.
        let exe = dir.path().join("llama-server");
        std::fs::write(&exe, "#!/bin/sh\necho 'error: failed to load model' >&2\nexit 1\n")
            .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let config = ServerConfig {
            model: dir.path().join("m.gguf"),
            mmproj: None,
            port: 0,
            gpu: true,
            threads: 0,
        };
        let error = Server::start(&exe, &config, &log).err().unwrap();
        assert!(error.contains("failed to load model"), "{error}");
        assert!(error.contains("на процессоре тоже"), "вторая попытка — без видеокарты: {error}");
        assert_eq!(find_exe(&exe, None), Some(exe.clone()));
        assert_eq!(find_exe(&dir.path().join("нет"), Some(exe.clone())), None, "свой путь важнее");
        assert_eq!(find_exe(Path::new(""), Some(exe.clone())), Some(exe));
    }
}
