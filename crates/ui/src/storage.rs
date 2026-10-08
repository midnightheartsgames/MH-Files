//! Где лежат настройки и сеанс, и как их записывать, не теряя при сбое.
//!
//! Обычно — профиль пользователя. Переносной режим (флешка, папка с программой): если рядом
//! с exe лежит файл `portable` или папка `data`, либо запуск с `--portable`, всё хранится в
//! `data` рядом с exe — настройки, сеанс, категории, журналы и индекс.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use mh_files_core::session::Session;
use mh_files_core::settings::Settings;

/// Папки данных программы.
#[derive(Debug, Clone)]
pub struct DataDirs {
    pub config: PathBuf,
    pub index: PathBuf,
    pub portable: bool,
}

static DIRS: OnceLock<DataDirs> = OnceLock::new();

/// Выбрать папки данных — один раз, до создания окна.
pub fn init(force_portable: bool) -> &'static DataDirs {
    DIRS.get_or_init(|| detect(force_portable))
}

pub fn dirs() -> &'static DataDirs {
    init(false)
}

fn detect(force_portable: bool) -> DataDirs {
    let exe_dir = std::env::current_exe().ok().and_then(|p| p.parent().map(PathBuf::from));
    if let Some(exe_dir) = exe_dir {
        let data = exe_dir.join("data");
        let marked = exe_dir.join("portable").is_file() || data.is_dir();
        if force_portable || marked {
            return DataDirs { index: data.join("index"), config: data, portable: true };
        }
    }
    DataDirs { config: profile_config(), index: profile_index(), portable: false }
}

/// `%APPDATA%\MH Files` в Windows, `~/.config/mh-files` в остальных системах.
fn profile_config() -> PathBuf {
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
fn profile_index() -> PathBuf {
    #[cfg(windows)]
    let base = std::env::var_os("LOCALAPPDATA").map(PathBuf::from);
    #[cfg(not(windows))]
    let base = std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".cache")));
    let name = if cfg!(windows) { "MH Files" } else { "mh-files" };
    base.unwrap_or_else(std::env::temp_dir).join(name).join("index")
}

pub fn config_dir() -> PathBuf {
    dirs().config.clone()
}

pub fn index_dir() -> PathBuf {
    dirs().index.clone()
}

/// Новая версия программы запускается первый раз — сохранить настройки и сеанс прошлой
/// версии в `backup\<версия>`: если новая что-то поймёт не так, к старым можно вернуться.
/// Возвращает прошлую версию, если она была другой.
pub fn backup_on_upgrade() -> Option<String> {
    let dir = config_dir();
    let marker = dir.join("version.txt");
    let current = env!("CARGO_PKG_VERSION");
    let previous = std::fs::read_to_string(&marker).ok().map(|v| v.trim().to_string());
    if previous.as_deref() == Some(current) {
        return None;
    }
    if let Some(old) = &previous {
        let backup = dir.join("backup").join(old);
        if std::fs::create_dir_all(&backup).is_ok() {
            for name in ["settings.json", "session.json", "categories.json"] {
                let _ = std::fs::copy(dir.join(name), backup.join(name));
            }
        }
    }
    let _ = write_atomic(&marker, current);
    previous.filter(|_| dir.join("settings.json").exists())
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

pub fn labels_path() -> PathBuf {
    config_dir().join("labels.json")
}

/// Цветные метки файлов. Испорченный файл сохраняется рядом как `.broken`, меток нет.
pub fn load_labels() -> mh_files_core::labels::Labels {
    let path = labels_path();
    let Ok(text) = std::fs::read_to_string(&path) else { return Default::default() };
    mh_files_core::labels::Labels::parse(&text).unwrap_or_else(|| {
        let _ = std::fs::copy(&path, path.with_extension("json.broken"));
        Default::default()
    })
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
