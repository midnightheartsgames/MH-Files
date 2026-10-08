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
pub const MIN_LIST_SCALE: f32 = 0.8;
pub const MAX_LIST_SCALE: f32 = 2.0;
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
    pub system: SystemSettings,
    pub duplicates: DuplicateSettings,
    /// Папки, которые сортировщик раскладывает сам по расписанию (пока программа открыта).
    pub sort_schedules: Vec<SortSchedule>,
}

/// Сортировка папки по расписанию.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SortSchedule {
    pub folder: std::path::PathBuf,
    /// Раз в столько часов.
    pub every_hours: u32,
    /// Последний запуск, секунды Unix; 0 — ещё не было.
    #[serde(default)]
    pub last_run: i64,
    /// Галочки сортировщика на момент, когда расписание включили.
    #[serde(default)]
    pub options: SortSettings,
}

impl SortSchedule {
    /// Варианты периода: часы и подпись.
    pub const PERIODS: [(u32, &'static str); 4] =
        [(1, "каждый час"), (6, "каждые 6 часов"), (24, "раз в день"), (168, "раз в неделю")];

    /// Пора ли запускать в момент `now` (секунды Unix).
    pub fn due(&self, now: i64) -> bool {
        self.every_hours > 0 && now - self.last_run >= i64::from(self.every_hours) * 3600
    }

    pub fn period_label(&self) -> String {
        Self::PERIODS.iter().find(|(hours, _)| *hours == self.every_hours).map_or_else(
            || format!("каждые {} ч", self.every_hours),
            |(_, label)| label.to_string(),
        )
    }
}

/// Поиск дубликатов.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct DuplicateSettings {
    /// Файлы меньше, байт, не сравниваются.
    pub min_size: u64,
    /// Что пропускать: имя папки или файла (`node_modules`), маска (`*.tmp`) или полный
    /// путь папки (`D:\Резерв`). См. `duplicates::excluded`.
    pub exclude: Vec<String>,
}

impl Default for DuplicateSettings {
    fn default() -> DuplicateSettings {
        DuplicateSettings { min_size: 1, exclude: vec![".git".into(), "node_modules".into()] }
    }
}

/// Как программа живёт в системе.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct SystemSettings {
    /// Одна копия: повторный запуск открывает пути в уже открытом окне.
    pub single_instance: bool,
}

impl Default for SystemSettings {
    fn default() -> SystemSettings {
        SystemSettings { single_instance: true }
    }
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
    /// Когда `roots` пусто: индексировать и съёмные диски (флешки, внешние диски) — пока
    /// они подключены.
    pub removable: bool,
    /// То же для сетевых дисков с буквой. Изменения в сети видны не всегда сразу: сервер
    /// может не сообщать о них.
    pub network: bool,
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
            removable: false,
            network: false,
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
    /// Размер строк таблицы и колонок: высота, значки и шрифт вместе (1 — обычный).
    pub list_scale: f32,
    pub accent: [u8; 3],
    pub animations: bool,
    /// Значки файлов из Windows; выключено — свои значки в стиле MH.
    pub system_icons: bool,
    /// Свой заголовок окна со вкладками вместо системного.
    pub custom_title_bar: bool,
    /// Цветной кружок у даты изменения: давность от «только что» до «давно».
    pub age_dots: bool,
    /// Цвет только что изменённого.
    pub age_new: [u8; 3],
    /// Цвет изменённого давно (год и больше).
    pub age_old: [u8; 3],
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
    /// Пункты меню Проводника (7-Zip, Git, «Отправить»…) прямо в своём контекстном меню.
    pub windows_menu_items: bool,
    /// Размеры вложенных папок в списке — сразу, из индекса поиска по дискам.
    pub auto_folder_sizes: bool,
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
    /// Видео и звук играют прямо в быстром просмотре (Media Foundation).
    pub media: bool,
    /// Размер выбранной папки в Инспекторе — сразу, без кнопки.
    pub inspector_folder_size: bool,
    /// Громкость быстрого просмотра 0..1 — какой её оставили в прошлый раз.
    pub volume: f32,
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
/// Давность даты по умолчанию: только что — красный, давно — зелёный.
pub const AGE_NEW: [u8; 3] = [0xE8, 0x5C, 0x5C];
pub const AGE_OLD: [u8; 3] = [0x4C, 0xC3, 0x8A];

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
            system: SystemSettings::default(),
            duplicates: DuplicateSettings::default(),
            sort_schedules: Vec::new(),
        }
    }
}

impl Default for Appearance {
    fn default() -> Appearance {
        Appearance {
            font_scale: 1.0,
            compact: false,
            grid_size: 112.0,
            list_scale: 1.0,
            accent: ACCENT,
            animations: true,
            system_icons: true,
            custom_title_bar: true,
            age_dots: true,
            age_new: AGE_NEW,
            age_old: AGE_OLD,
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
            windows_menu_items: true,
            auto_folder_sizes: true,
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
        Preview {
            thumbnails: true,
            text_limit_kb: 256,
            image_limit_mb: 64,
            handlers: true,
            media: true,
            inspector_folder_size: true,
            volume: 1.0,
        }
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
        a.list_scale = clamp(a.list_scale, MIN_LIST_SCALE, MAX_LIST_SCALE, 1.0);
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
    fn schedules_are_due_by_period() {
        let mut schedule = SortSchedule {
            folder: "D:/Загрузки".into(),
            every_hours: 24,
            last_run: 0,
            options: SortSettings::default(),
        };
        assert!(schedule.due(1_760_000_000), "ещё не запускалось");
        schedule.last_run = 100_000;
        assert!(!schedule.due(100_000 + 23 * 3600));
        assert!(schedule.due(100_000 + 24 * 3600));
        assert_eq!(schedule.period_label(), "раз в день");
        schedule.every_hours = 0;
        assert!(!schedule.due(i64::MAX / 2), "период 0 — выключено");
        let json = r#"{"sort_schedules":[{"folder":"C:/x","every_hours":6}]}"#;
        let (parsed, error) = Settings::parse(json);
        assert!(error.is_none());
        assert_eq!(parsed.sort_schedules[0].every_hours, 6);
        assert_eq!(parsed.sort_schedules[0].options, SortSettings::default());
    }

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

    /// Файлы настроек всех прошлых версий читаются без потерь: чего в них нет — по
    /// умолчанию, что есть — как было.
    #[test]
    fn files_of_older_versions() {
        // 0.1: ещё нет индекса, сортировщика, своего заголовка окна.
        let v01 = r#"{
          "version": 1,
          "appearance": {"font_scale": 1.25, "compact": true, "grid_size": 128.0,
                         "accent": [255, 0, 0], "animations": false, "system_icons": false},
          "files": {"show_hidden": true, "show_system": false, "show_extensions": false,
                    "folders_first": false, "confirm_recycle": true, "relative_dates": false},
          "panes": {"show_sidebar": false, "sidebar_width": 300.0, "show_inspector": true,
                    "inspector_width": 320.0, "restore_session": false, "new_tab": "Home"},
          "preview": {"thumbnails": false, "text_limit_kb": 64, "image_limit_mb": 8},
          "groups": [{"name": "Проекты", "collapsed": true,
                      "items": [{"name": "MH", "path": "D:\\MH"}]}],
          "keys": {"Rename": ["Ctrl+R"]},
          "terminal": "wt.exe -d \"{dir}\""
        }"#;
        let (s, notice) = Settings::parse(v01);
        assert!(notice.is_none());
        assert_eq!(s.appearance.font_scale, 1.25);
        assert!(s.appearance.compact && !s.appearance.animations);
        assert!(s.appearance.custom_title_bar, "новое — по умолчанию");
        assert!(s.files.show_hidden && s.files.confirm_recycle);
        assert_eq!(s.panes.new_tab, NewTabLocation::Home);
        assert_eq!(s.groups[0].items[0].path, PathBuf::from("D:\\MH"));
        assert_eq!(s.keys["Rename"], ["Ctrl+R"]);
        assert!(s.index.enabled && s.saved_searches.is_empty());
        assert!(s.preview.handlers);
        assert_eq!(s.sorting, SortSettings::default());
        assert!(s.system.single_instance);

        // 0.3–0.5: индекс, поиски, сортировщик.
        let v05 = r#"{
          "version": 1,
          "index": {"enabled": false, "roots": ["D:\\"], "exclude": [], "rescan_on_start": false},
          "saved_searches": [{"name": "Видео", "query": "ext:mp4"}],
          "sorting": {"mode": "copy", "recursive": true, "excluded": ["node_modules"]},
          "preview": {"handlers": false}
        }"#;
        let (s, _) = Settings::parse(v05);
        assert!(!s.index.enabled && !s.index.rescan_on_start);
        assert_eq!(s.saved_searches[0].query, "ext:mp4");
        assert_eq!(s.sorting.mode, crate::sorting::Mode::Copy);
        assert!(s.sorting.recursive && s.sorting.type_folders);
        assert_eq!(s.sorting.excluded, ["node_modules"]);
        assert!(!s.preview.handlers);
        assert_eq!(Settings::parse(&s.to_json()).0, s, "запись и чтение без потерь");
    }
}
