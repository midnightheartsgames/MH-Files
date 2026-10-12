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
    /// Запущен без кодировщика картинок и звука (`mmproj`): с ним не запустился.
    pub media_fallback: bool,
}

/// Попытка запуска: настройки и чем она уступает просьбе.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Attempt {
    config: ServerConfig,
    gpu_fallback: bool,
    media_fallback: bool,
}

/// Порядок попыток: как просили; без `mmproj` (битый или чужой кодировщик не должен
/// лишать поиска по тексту); на процессоре (капризный драйвер Vulkan); на процессоре
/// без `mmproj`.
fn attempts(config: &ServerConfig) -> Vec<Attempt> {
    let mut out =
        vec![Attempt { config: config.clone(), gpu_fallback: false, media_fallback: false }];
    let text_only = ServerConfig { mmproj: None, ..config.clone() };
    if config.mmproj.is_some() {
        out.push(Attempt { config: text_only.clone(), gpu_fallback: false, media_fallback: true });
    }
    if config.gpu {
        let cpu = ServerConfig { gpu: false, ..config.clone() };
        out.push(Attempt { config: cpu, gpu_fallback: true, media_fallback: false });
        if config.mmproj.is_some() {
            let cpu_text = ServerConfig { gpu: false, ..text_only };
            out.push(Attempt { config: cpu_text, gpu_fallback: true, media_fallback: true });
        }
    }
    out
}

impl Server {
    /// Запустить и дождаться готовности. Не вышло — следующие попытки по [`attempts`];
    /// журнал всех попыток — в `log`, ошибка — с причиной из журнала первой и последней.
    pub fn start(exe: &Path, config: &ServerConfig, log: &Path) -> Result<Server, String> {
        #[cfg(test)]
        if let Some((_, port)) = TEST_PORT.lock().as_ref().filter(|(path, _)| path == exe) {
            let port = *port;
            return Ok(Server { process: None, port, gpu_fallback: false, media_fallback: false });
        }
        let _ = std::fs::write(log, "");
        let mut reasons: Vec<String> = Vec::new();
        for attempt in attempts(config) {
            match Server::start_once(exe, &attempt.config, log) {
                Ok(mut server) => {
                    server.gpu_fallback = attempt.gpu_fallback;
                    server.media_fallback = attempt.media_fallback;
                    return Ok(server);
                }
                // Сборка не знает модель — остальные попытки не помогут.
                Err(reason) if reason.contains("unknown model architecture") => {
                    return Err(format!("{} ({reason})", model::too_old(exe_build(exe))));
                }
                Err(reason) => {
                    if !reasons.contains(&reason) {
                        reasons.push(reason);
                    }
                }
            }
        }
        Err(format!("модель не запустилась: {} (журнал: {})", reasons.join("; "), log.display()))
    }

    fn start_once(exe: &Path, config: &ServerConfig, log: &Path) -> Result<Server, String> {
        let port = free_port()?;
        let config = ServerConfig { port, ..config.clone() };
        let args = model::server_args(&config);
        {
            use std::io::Write as _;
            if let Ok(mut file) = std::fs::OpenOptions::new().create(true).append(true).open(log) {
                let _ = writeln!(file, "{ATTEMPT_MARK} {} {}", exe.display(), args.join(" "));
            }
        }
        let mut process = Background::spawn(exe, &args, Some(log))?;
        let started = Instant::now();
        loop {
            if let Some(status) = process.exited() {
                let reason = log_reason(&std::fs::read_to_string(log).unwrap_or_default());
                return Err(match reason {
                    Some(reason) => reason,
                    None => format!("llama-server завершился ({status})"),
                });
            }
            // 503 — модель грузится; нет соединения — сервер ещё не открыл порт.
            if let Ok((200, _)) = request(port, "GET", "/health", None, Duration::from_secs(2)) {
                return Ok(Server {
                    process: Some(process),
                    port,
                    gpu_fallback: false,
                    media_fallback: false,
                });
            }
            if started.elapsed() > START_TIMEOUT {
                return Err(format!("модель не загрузилась за {} с", START_TIMEOUT.as_secs()));
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

/// Порт поддельного сервера вместо запуска этого `llama-server` — для тестов службы.
/// Подмена — только для своего пути: тесты идут параллельно.
#[cfg(test)]
static TEST_PORT: parking_lot::Mutex<Option<(PathBuf, u16)>> = parking_lot::Mutex::new(None);

#[cfg(test)]
pub fn set_test_port(fake: Option<(PathBuf, u16)>) {
    *TEST_PORT.lock() = fake;
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

/// Начало попытки запуска в журнале сервера.
const ATTEMPT_MARK: &str = "=== MH Files: запуск";

/// Причина отказа из журнала последней попытки: строки ошибок llama.cpp (уровень `E`,
/// «error», «failed»), кроме общего «exiting due to model loading error» — он лишь итог.
/// Не больше двух строк, первыми — самые говорящие.
fn log_reason(log: &str) -> Option<String> {
    let last = log.rsplit(ATTEMPT_MARK).next().unwrap_or(log);
    let mut lines: Vec<String> = Vec::new();
    for line in last.lines() {
        let line = strip_ansi(line);
        let lower = line.to_lowercase();
        let error = line.contains(" E ") || lower.contains("error") || lower.contains("failed");
        if !error || lower.contains("exiting due to model loading error") {
            continue;
        }
        // Метка времени и уровень llama.cpp («0.00.814.770 E ») — не для людей.
        let text = line
            .split_once(" E ")
            .filter(|(head, _)| head.chars().all(|c| c.is_ascii_digit() || c == '.' || c == ' '))
            .map_or(line.as_str(), |(_, rest)| rest)
            .trim()
            .to_string();
        if !text.is_empty() && !lines.contains(&text) {
            lines.push(text);
        }
    }
    let telling = |l: &String| {
        let l = l.to_lowercase();
        l.contains("error loading model")
            || l.contains("unknown")
            || l.contains("failed to load")
            || l.contains("invalid")
            || l.contains("out of memory")
    };
    lines.sort_by_key(|l| !telling(l));
    lines.truncate(2);
    (!lines.is_empty()).then(|| {
        lines
            .iter()
            .map(|l| l.chars().take(240).collect::<String>())
            .collect::<Vec<_>>()
            .join(" · ")
    })
}

/// Без цветовых кодов терминала (`ESC[…m`).
fn strip_ansi(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut chars = line.chars();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' {
            for c in chars.by_ref() {
                if c.is_ascii_alphabetic() {
                    break;
                }
            }
        } else {
            out.push(c);
        }
    }
    out
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

/// Где лежит `llama-server`: свой путь из настроек или скачанный MH Files. Вне Windows
/// (разработка) — ещё и найденный в `PATH`. В Windows `PATH` не смотрится: там часто
/// стоит свой llama.cpp (WinGet, Scoop), и старая сборка не знает EmbeddingGemma 2.
pub fn find_exe(custom: &Path, downloaded: Option<PathBuf>) -> Option<PathBuf> {
    if !custom.as_os_str().is_empty() {
        return custom.is_file().then(|| custom.to_path_buf());
    }
    if let Some(path) = downloaded.filter(|p| p.is_file()) {
        return Some(path);
    }
    if cfg!(windows) {
        return None;
    }
    std::env::var_os("PATH")
        .map(|paths| {
            std::env::split_paths(&paths).map(|dir| dir.join("llama-server")).collect::<Vec<_>>()
        })
        .unwrap_or_default()
        .into_iter()
        .find(|p| p.is_file())
}

/// Номер сборки llama.cpp у `exe` (`--version`); `None` — не узнать.
pub fn exe_build(exe: &Path) -> Option<u32> {
    let text =
        mh_files_platform::process::capture(exe, &["--version"], Duration::from_secs(15)).ok()?;
    model::parse_build(&text)
}

#[cfg(test)]
pub mod tests {
    use std::io::BufRead;
    use std::net::TcpListener;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;

    #[test]
    fn start_attempts_in_order() {
        let config = ServerConfig {
            model: "m.gguf".into(),
            mmproj: Some("mmproj.gguf".into()),
            port: 0,
            gpu: true,
            threads: 0,
        };
        let plan: Vec<(bool, bool, bool, bool)> = attempts(&config)
            .iter()
            .map(|a| (a.config.gpu, a.config.mmproj.is_some(), a.gpu_fallback, a.media_fallback))
            .collect();
        assert_eq!(
            plan,
            [
                (true, true, false, false),
                (true, false, false, true),
                (false, true, true, false),
                (false, false, true, true)
            ]
        );
        let cpu_text = ServerConfig { gpu: false, mmproj: None, ..config };
        assert_eq!(attempts(&cpu_text).len(), 1);
    }

    #[test]
    fn log_reason_skips_the_generic_line() {
        let log = "=== MH Files: запуск a\n0.00.100.000 E llama_model_load: error loading model: old\n=== MH Files: запуск b\n\
            0.00.500.000 I srv load_model: loading model\n\
            0.00.700.000 E clip_model_loader: \u{1b}[31mtensor 'v.patch_embd' not found\u{1b}[0m\n\
            0.00.800.000 E llama_model_load: error loading model: unknown model architecture: 'gemma-embedding2'\n\
            0.00.814.770 E srv  llama_server: exiting due to model loading error\n";
        assert_eq!(
            log_reason(log).unwrap(),
            "llama_model_load: error loading model: unknown model architecture: 'gemma-embedding2' · clip_model_loader: tensor 'v.patch_embd' not found"
        );
        assert_eq!(
            log_reason(
                "0.1 I всё хорошо\n0.2 E srv  llama_server: exiting due to model loading error"
            ),
            None
        );
    }

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
        assert!(error.contains(&log.display().to_string()), "путь к журналу: {error}");
        let text = std::fs::read_to_string(&log).unwrap();
        assert_eq!(text.matches(ATTEMPT_MARK).count(), 2, "как просили и на процессоре: {text}");
        // Старая сборка: не знает архитектуру — сразу понятный ответ, без лишних попыток.
        let old = dir.path().join("old-llama-server");
        std::fs::write(
            &old,
            "#!/bin/sh\nif [ \"$1\" = --version ]; then echo 'version: 6123 (b92761a51)' >&2; exit 0; fi\necho \"0.5 E llama_model_load: error loading model: unknown model architecture: 'gemma-embedding2'\" >&2\nexit 1\n",
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&old, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let with_mmproj =
            ServerConfig { mmproj: Some(dir.path().join("p.gguf")), ..config.clone() };
        let error = Server::start(&old, &with_mmproj, &log).err().unwrap();
        assert!(error.contains("b6123") && error.contains("b11452"), "{error}");
        let text = std::fs::read_to_string(&log).unwrap();
        assert_eq!(text.matches(ATTEMPT_MARK).count(), 1, "одна попытка: {text}");
        assert_eq!(exe_build(&old), Some(6123));
        assert_eq!(find_exe(&exe, None), Some(exe.clone()));
        assert_eq!(find_exe(&dir.path().join("нет"), Some(exe.clone())), None, "свой путь важнее");
        assert_eq!(find_exe(Path::new(""), Some(exe.clone())), Some(exe));
    }
}
