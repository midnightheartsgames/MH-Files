//! Сортировщик из MH Sort: раскладывает содержимое папки по папкам категорий
//! (`Изображения`, `Видео`…) и типов (`Видео\MP4`). Здесь — то, что проверяется без диска:
//! категории, определение категории по имени, имена, план и журнал. Обход папки, перемещение
//! и отмена — в `mh-files-fs`.

pub mod classify;
pub mod config;
pub mod edit;
pub mod names;
pub mod plan;

pub use classify::{Class, Classifier};
pub use config::{CONFIG_FILE, Config, Mode, Rule};
pub use plan::{
    Action, CategoryStat, Entry, Journal, LogLine, Plan, PlannedMove, Report, ScanOptions,
};
