//! Где лежат настройки и сеанс, и как их записывать, не теряя при сбое.

use std::path::{Path, PathBuf};

use mh_files_core::session::Session;
use mh_files_core::settings::Settings;

/// `%APPDATA%\MH Files` в Windows, `~/.config/mh-files` в остальных системах.
pub fn config_dir() -> PathBuf {
    #[cfg(windows)]
    let base = std::env::var_os("APPDATA").map(PathBuf::from);
    #[cfg(not(windows))]
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")));
    let name = if cfg!(windows) { "MH Files" } else { "mh-files" };
    base.unwrap_or_else(std::env::temp_dir).join(name)
}

/// Снимки индекса дисков: `%LOCALAPPDATA%\MH Files\index` — данные машины, а не
/// пользователя, в перемещаемый профиль им незачем; вне Windows — `~/.cache/mh-files/index`.
pub fn index_dir() -> PathBuf {
    #[cfg(windows)]
    let base = std::env::var_os("LOCALAPPDATA").map(PathBuf::from);
    #[cfg(not(windows))]
    let base = std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".cache")));
    let name = if cfg!(windows) { "MH Files" } else { "mh-files" };
    base.unwrap_or_else(std::env::temp_dir).join(name).join("index")
}

pub fn settings_path() -> PathBuf {
    config_dir().join("settings.json")
}

pub fn session_path() -> PathBuf {
    config_dir().join("session.json")
}

/// Настройки и, если что-то не так, текст для строки состояния. Испорченный файл
/// сохраняется рядом с суффиксом `.broken`, чтобы его не затёрли.
pub fn load_settings() -> (Settings, Option<String>) {
    let path = settings_path();
    match std::fs::read_to_string(&path) {
        Ok(text) => {
            let (settings, notice) = Settings::parse(&text);
            if notice.is_some() {
                let _ = std::fs::copy(&path, path.with_extension("json.broken"));
            }
            (settings, notice)
        }
        Err(_) => (Settings::default(), None),
    }
}

pub fn load_session() -> Option<Session> {
    Session::parse(&std::fs::read_to_string(session_path()).ok()?)
}

/// Запись через временный файл и переименование: при сбое остаётся старый файл целиком.
pub fn write_atomic(path: &Path, contents: &str) -> Result<(), String> {
    let dir = path.parent().ok_or("нет папки")?;
    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    let temp = path.with_extension("tmp");
    std::fs::write(&temp, contents).map_err(|e| e.to_string())?;
    std::fs::rename(&temp, path).map_err(|e| e.to_string())
}
