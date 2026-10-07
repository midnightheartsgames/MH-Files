//! Настройки программы. Файл версионный: старые версии переводятся вперёд, неизвестные поля
//! игнорируются, недостающие берутся по умолчанию, значения вне границ поправляются.

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

pub const SETTINGS_VERSION: u32 = 1;

pub const MIN_FONT_SCALE: f32 = 0.8;
pub const MAX_FONT_SCALE: f32 = 1.6;
pub const MIN_GRID: f32 = 64.0;
pub const MAX_GRID: f32 = 256.0;
pub const MIN_SIDE_WIDTH: f32 = 150.0;
pub const MAX_SIDE_WIDTH: f32 = 600.0;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub version: u32,
    pub appearance: Appearance,
    pub files: Files,
    pub panes: Panes,
    pub preview: Preview,
    /// Группы боковой панели: «Избранное», «Проекты»…
    pub groups: Vec<Group>,
    /// Свои сочетания: имя команды → сочетания. Нет записи — по умолчанию.
    pub keys: BTreeMap<String, Vec<String>>,
    /// Команда терминала; `{dir}` — папка. Пусто — Windows Terminal или PowerShell.
    pub terminal: String,
    pub index: IndexSettings,
    /// Сохранённые поиски по дискам, показываются в боковой панели.
    pub saved_searches: Vec<SavedSearch>,
    pub sorting: SortSettings,
}

/// Сортировщик (из MH Sort): галочки, с которыми открывается вкладка «Разложить».
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct SortSettings {
    pub mode: crate::sorting::Mode,
    /// Разбирать и вложенные папки.
    pub recursive: bool,
    /// Внутри категории — папки типов: `Видео\MP4`.
    pub type_folders: bool,
    /// Не заходить в уже разложенные папки категорий.
    pub skip_sorted: bool,
    /// Удалять папки, опустевшие после перемещения.
    pub remove_empty: bool,
    pub skip_hidden: bool,
    /// Узнавать тип файла без расширения по его содержимому.
    pub detect_content: bool,
    /// Имена папок, в которые сортировщик не заходит.
    pub excluded: Vec<String>,
}

impl Default for SortSettings {
    fn default() -> SortSettings {
        SortSettings {
            mode: crate::sorting::Mode::Move,
            recursive: false,
            type_folders: true,
            skip_sorted: true,
            remove_empty: false,
            skip_hidden: true,
            detect_content: true,
            excluded: Vec::new(),
        }
    }
}

/// Индекс дисков для поиска «везде».
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct IndexSettings {
    pub enabled: bool,
    /// Что индексировать. Пусто — все локальные диски.
    pub roots: Vec<PathBuf>,
    /// Папки, которые пропускаются вместе с содержимым.
    pub exclude: Vec<PathBuf>,
    /// Досканировать при запуске папки, изменённые пока программа была закрыта. Без журнала
    /// USN (нужны права администратора) — полный обход в фоне.
    pub rescan_on_start: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SavedSearch {
    pub name: String,
    pub query: String,
}

impl Default for IndexSettings {
    fn default() -> IndexSettings {
        IndexSettings {
            enabled: true,
            roots: Vec::new(),
            exclude: Vec::new(),
            rescan_on_start: true,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Appearance {
    pub font_scale: f32,
    /// Плотный список: строки ниже.
    pub compact: bool,
    /// Сторона плитки в режиме «Плитки», точки.
    pub grid_size: f32,
    pub accent: [u8; 3],
    pub animations: bool,
    /// Значки файлов из Windows; выключено — свои значки в стиле MH.
    pub system_icons: bool,
    /// Свой заголовок окна со вкладками вместо системного.
    pub custom_title_bar: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Files {
    pub show_hidden: bool,
    pub show_system: bool,
    pub show_extensions: bool,
    pub folders_first: bool,
    /// Спрашивать перед отправкой в корзину. Удаление насовсем спрашивает всегда.
    pub confirm_recycle: bool,
    /// Даты «Сегодня 12:30» вместо полной даты.
    pub relative_dates: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum NewTabLocation {
    /// Там же, где текущая вкладка.
    #[default]
    Same,
    Home,
    Computer,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Panes {
    pub show_sidebar: bool,
    pub sidebar_width: f32,
    pub show_inspector: bool,
    pub inspector_width: f32,
    pub restore_session: bool,
    pub new_tab: NewTabLocation,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Preview {
    /// Эскизы в списке и плитках.
    pub thumbnails: bool,
    /// Сколько текста читать для предпросмотра, КБ.
    pub text_limit_kb: u32,
    /// Картинки больше этого не декодируются для предпросмотра, МБ.
    pub image_limit_mb: u32,
    /// Документы Office и прочее — обработчиками предпросмотра Windows в Инспекторе.
    pub handlers: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Group {
    pub name: String,
    #[serde(default)]
    pub collapsed: bool,
    #[serde(default)]
    pub items: Vec<Favorite>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Favorite {
    pub name: String,
    pub path: PathBuf,
}

pub const ACCENT: [u8; 3] = [0x3F, 0xD0, 0xD8];

impl Default for Settings {
    fn default() -> Settings {
        Settings {
            version: SETTINGS_VERSION,
            appearance: Appearance::default(),
            files: Files::default(),
            panes: Panes::default(),
            preview: Preview::default(),
            groups: vec![Group {
                name: "Избранное".into(), collapsed: false, items: Vec::new()
            }],
            keys: BTreeMap::new(),
            terminal: String::new(),
            index: IndexSettings::default(),
            saved_searches: Vec::new(),
            sorting: SortSettings::default(),
        }
    }
}

impl Default for Appearance {
    fn default() -> Appearance {
        Appearance {
            font_scale: 1.0,
            compact: false,
            grid_size: 112.0,
            accent: ACCENT,
            animations: true,
            system_icons: true,
            custom_title_bar: true,
        }
    }
}

impl Default for Files {
    fn default() -> Files {
        Files {
            show_hidden: false,
            show_system: false,
            show_extensions: true,
            folders_first: true,
            confirm_recycle: false,
            relative_dates: true,
        }
    }
}

impl Default for Panes {
    fn default() -> Panes {
        Panes {
            show_sidebar: true,
            sidebar_width: 230.0,
            show_inspector: true,
            inspector_width: 300.0,
            restore_session: true,
            new_tab: NewTabLocation::Same,
        }
    }
}

impl Default for Preview {
    fn default() -> Preview {
        Preview { thumbnails: true, text_limit_kb: 256, image_limit_mb: 64, handlers: true }
    }
}

impl Settings {
    /// Разбор файла настроек. Испорченный файл — настройки по умолчанию и текст для
    /// пользователя (файл при этом не перезаписывается молча: вызывающий сохранит копию).
    pub fn parse(json: &str) -> (Settings, Option<String>) {
        let value: serde_json::Value = match serde_json::from_str(json) {
            Ok(value) => value,
            Err(error) => {
                return (Settings::default(), Some(format!("настройки не прочитаны: {error}")));
            }
        };
        let version = value.get("version").and_then(|v| v.as_u64()).unwrap_or(0) as u32;
        let value = migrate(value, version);
        match serde_json::from_value::<Settings>(value) {
            Ok(settings) => {
                let notice = (version > SETTINGS_VERSION).then(|| {
                    "настройки от более новой версии MH Files: незнакомое пропущено".to_string()
                });
                (settings.sanitized(), notice)
            }
            Err(error) => (Settings::default(), Some(format!("настройки не прочитаны: {error}"))),
        }
    }

    pub fn to_json(&self) -> String {
        serde_json::to_string_pretty(self).expect("настройки сериализуются")
    }

    /// Значения в допустимых границах.
    pub fn sanitized(mut self) -> Settings {
        let clamp = |value: f32, min: f32, max: f32, default: f32| {
            if value.is_finite() { value.clamp(min, max) } else { default }
        };
        let a = &mut self.appearance;
        a.font_scale = clamp(a.font_scale, MIN_FONT_SCALE, MAX_FONT_SCALE, 1.0);
        a.grid_size = clamp(a.grid_size, MIN_GRID, MAX_GRID, 112.0);
        let p = &mut self.panes;
        p.sidebar_width = clamp(p.sidebar_width, MIN_SIDE_WIDTH, MAX_SIDE_WIDTH, 230.0);
        p.inspector_width = clamp(p.inspector_width, MIN_SIDE_WIDTH, MAX_SIDE_WIDTH, 300.0);
        self.preview.text_limit_kb = self.preview.text_limit_kb.clamp(4, 4096);
        self.preview.image_limit_mb = self.preview.image_limit_mb.clamp(1, 1024);
        self.version = SETTINGS_VERSION;
        self
    }
}

/// Перевод старых версий файла. Пока версия одна; место для будущих шагов.
fn migrate(value: serde_json::Value, version: u32) -> serde_json::Value {
    let _ = version;
    value
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_and_defaults() {
        let settings = Settings::default();
        let (parsed, notice) = Settings::parse(&settings.to_json());
        assert_eq!(parsed, settings);
        assert!(notice.is_none());
        let (partial, _) = Settings::parse(r#"{"version":1,"files":{"show_hidden":true}}"#);
        assert!(partial.files.show_hidden);
        assert!(partial.files.folders_first, "недостающее — по умолчанию");
    }

    #[test]
    fn broken_and_out_of_range() {
        let (settings, notice) = Settings::parse("{not json");
        assert_eq!(settings, Settings::default());
        assert!(notice.is_some());
        let (settings, _) =
            Settings::parse(r#"{"appearance":{"font_scale":9.0},"panes":{"sidebar_width":-5}}"#);
        assert_eq!(settings.appearance.font_scale, MAX_FONT_SCALE);
        assert_eq!(settings.panes.sidebar_width, MIN_SIDE_WIDTH);
        let (_, notice) = Settings::parse(r#"{"version":99}"#);
        assert!(notice.is_some());
    }
}
