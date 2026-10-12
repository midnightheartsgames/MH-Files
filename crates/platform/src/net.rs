//! Запросы по HTTPS: проверка обновлений по кнопке и скачивание модели поиска по смыслу,
//! когда пользователь его включил. В Windows — WinHTTP (системные прокси и сертификаты, без
//! своих TLS-библиотек), вне Windows — `curl`.

use std::path::Path;
use std::time::Duration;

/// Сколько ждать соединения и ответа.
pub const TIMEOUT: Duration = Duration::from_secs(15);

/// `GET https://<host><path>`: тело ответа, если код 200. `limit` — сколько байт принять.
pub fn get(
    host: &str,
    path: &str,
    headers: &[(&str, &str)],
    limit: usize,
) -> Result<Vec<u8>, String> {
    #[cfg(windows)]
    {
        crate::win::net::get(host, path, headers, limit)
    }
    #[cfg(not(windows))]
    {
        let mut command = std::process::Command::new("curl");
        command
            .args(["-sS", "-f", "-L", "--max-time"])
            .arg(TIMEOUT.as_secs().to_string())
            .arg("--max-filesize")
            .arg(limit.to_string());
        for (name, value) in headers {
            command.arg("-H").arg(format!("{name}: {value}"));
        }
        let output = command
            .arg(format!("https://{host}{path}"))
            .output()
            .map_err(|error| format!("curl не запущен: {error}"))?;
        if output.status.success() {
            Ok(output.stdout)
        } else {
            Err(String::from_utf8_lossy(&output.stderr).trim().to_string())
        }
    }
}

/// Скачать `https://<host><path>` в файл `dest`: сначала в `dest.part`, по окончании —
/// переименованием, так что недокачанный файл никогда не выглядит готовым. `progress`
/// получает (скачано, всего) и может прервать загрузку, вернув `false`. Возвращает размер.
pub fn download(
    host: &str,
    path: &str,
    dest: &Path,
    progress: &mut dyn FnMut(u64, Option<u64>) -> bool,
) -> Result<u64, String> {
    let part = dest.with_extension(format!(
        "{}part",
        dest.extension().map(|e| format!("{}.", e.to_string_lossy())).unwrap_or_default()
    ));
    if let Some(dir) = dest.parent() {
        std::fs::create_dir_all(dir)
            .map_err(|e| format!("папка {} не создана: {e}", dir.display()))?;
    }
    let result = download_to(host, path, &part, progress);
    match result {
        Ok(size) => {
            std::fs::rename(&part, dest).map_err(|e| format!("файл не сохранён: {e}"))?;
            Ok(size)
        }
        Err(error) => {
            let _ = std::fs::remove_file(&part);
            Err(error)
        }
    }
}

#[cfg(windows)]
fn download_to(
    host: &str,
    path: &str,
    part: &Path,
    progress: &mut dyn FnMut(u64, Option<u64>) -> bool,
) -> Result<u64, String> {
    use std::io::Write;
    let file = std::fs::File::create(part).map_err(|e| format!("файл не создан: {e}"))?;
    let mut file = std::io::BufWriter::with_capacity(1 << 20, file);
    // Обе половины зовут `progress` и знают длину: общие ячейки вместо двух `&mut`.
    let progress = std::cell::RefCell::new(progress);
    let total = std::cell::Cell::new(None);
    let mut done = 0u64;
    let mut last = std::time::Instant::now();
    crate::win::net::fetch(
        host,
        path,
        &[],
        &mut |length| {
            total.set(length);
            (progress.borrow_mut())(0, length)
        },
        &mut |chunk| {
            file.write_all(chunk).map_err(|e| format!("запись не удалась: {e}"))?;
            done += chunk.len() as u64;
            if last.elapsed() >= Duration::from_millis(100) {
                last = std::time::Instant::now();
                if !(progress.borrow_mut())(done, total.get()) {
                    return Err("отменено".into());
                }
            }
            Ok(())
        },
    )?;
    file.flush().map_err(|e| format!("запись не удалась: {e}"))?;
    let total = total.get();
    if let Some(total) = total.filter(|&total| total != done) {
        return Err(format!("скачано {done} байт из {total}"));
    }
    (progress.into_inner())(done, Some(done));
    Ok(done)
}

#[cfg(not(windows))]
fn download_to(
    host: &str,
    path: &str,
    part: &Path,
    progress: &mut dyn FnMut(u64, Option<u64>) -> bool,
) -> Result<u64, String> {
    let mut child = std::process::Command::new("curl")
        .args(["-sS", "-f", "-L", "--connect-timeout"])
        .arg(TIMEOUT.as_secs().to_string())
        .arg("-o")
        .arg(part)
        .arg(format!("https://{host}{path}"))
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|error| format!("curl не запущен: {error}"))?;
    loop {
        let done = std::fs::metadata(part).map(|m| m.len()).unwrap_or(0);
        if let Some(status) = child.try_wait().map_err(|e| e.to_string())? {
            if status.success() {
                progress(done, Some(done));
                return Ok(done);
            }
            let mut message = String::new();
            if let Some(mut stderr) = child.stderr.take() {
                use std::io::Read;
                let _ = stderr.read_to_string(&mut message);
            }
            return Err(message.trim().to_string());
        }
        if !progress(done, None) {
            let _ = child.kill();
            let _ = child.wait();
            return Err("отменено".into());
        }
        std::thread::sleep(Duration::from_millis(200));
    }
}
