//! Запрос по HTTPS — только для проверки обновлений по кнопке. В Windows — WinHTTP
//! (системные прокси и сертификаты, без своих TLS-библиотек), вне Windows — `curl`.

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
