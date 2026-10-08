//! Цветные метки файлов и папок: пользователь отмечает объект одним из шести цветов, чтобы
//! тот сразу бросался в глаза в списке (рамка вокруг значка или эскиза). Хранятся по полному
//! пути в своём файле `labels.json` — отдельно от настроек, которые правит окно настроек.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// Цветов на выбор.
pub const COLORS: u8 = 6;

/// Названия цветов по порядку.
pub const NAMES: [&str; COLORS as usize] =
    ["Красный", "Оранжевый", "Жёлтый", "Зелёный", "Синий", "Фиолетовый"];

/// Цвета по порядку (RGB).
pub const RGB: [[u8; 3]; COLORS as usize] = [
    [0xE8, 0x5C, 0x5C],
    [0xF2, 0x9A, 0x3C],
    [0xE6, 0xCF, 0x4A],
    [0x4C, 0xC3, 0x8A],
    [0x4C, 0x9B, 0xE8],
    [0xA7, 0x7B, 0xE8],
];

const VERSION: u32 = 1;

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Labels {
    #[serde(default)]
    version: u32,
    /// Путь (в Windows — в нижнем регистре: регистр имён там не важен) → номер цвета.
    #[serde(default)]
    labels: BTreeMap<String, u8>,
}

/// Ключ пути: в Windows регистр не различается.
fn key(path: &Path) -> String {
    let text = path.to_string_lossy();
    if cfg!(windows) { text.to_lowercase() } else { text.into_owned() }
}

impl Labels {
    /// Разобрать `labels.json`. `None` — файл испорчен (его стоит сохранить как `.broken`).
    pub fn parse(json: &str) -> Option<Labels> {
        let mut labels: Labels = serde_json::from_str(json).ok()?;
        labels.labels.retain(|_, color| *color < COLORS);
        labels.version = VERSION;
        Some(labels)
    }

    pub fn to_json(&self) -> String {
        let labels = Labels { version: VERSION, labels: self.labels.clone() };
        serde_json::to_string_pretty(&labels).unwrap_or_default()
    }

    /// Номер цвета объекта.
    pub fn get(&self, path: &Path) -> Option<u8> {
        if self.labels.is_empty() {
            return None;
        }
        self.labels.get(&key(path)).copied()
    }

    /// Отметить объекты цветом (`None` — снять метку). `true` — что-то изменилось.
    pub fn set(&mut self, paths: &[PathBuf], color: Option<u8>) -> bool {
        if color.is_some_and(|c| c >= COLORS) {
            return false;
        }
        let mut changed = false;
        for path in paths {
            let key = key(path);
            changed |= match color {
                Some(color) => self.labels.insert(key, color) != Some(color),
                None => self.labels.remove(&key).is_some(),
            };
        }
        changed
    }

    /// Объект переименовали или переместили: метка (и метки внутри папки) — за ним.
    /// `true` — что-то изменилось.
    pub fn moved(&mut self, from: &Path, to: &Path) -> bool {
        let (from, to) = (key(from), key(to));
        let from = from.trim_end_matches(['/', '\\']).to_string();
        // Сам объект или что-то внутри: дальше — разделитель (в Windows бывают оба).
        let below = |path: &str| {
            path.strip_prefix(from.as_str())
                .is_some_and(|rest| rest.is_empty() || rest.starts_with(['/', '\\']))
        };
        let moved: Vec<(String, u8)> = self
            .labels
            .iter()
            .filter(|(path, _)| below(path))
            .map(|(path, color)| (path.clone(), *color))
            .collect();
        for (path, color) in &moved {
            self.labels.remove(path);
            let rest = &path[from.len()..];
            self.labels.insert(format!("{to}{rest}"), *color);
        }
        !moved.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn set_get_and_clear() {
        let mut labels = Labels::default();
        let a = PathBuf::from("/data/setup.exe");
        assert!(labels.set(std::slice::from_ref(&a), Some(2)));
        assert!(!labels.set(std::slice::from_ref(&a), Some(2)), "тот же цвет — без изменений");
        assert_eq!(labels.get(&a), Some(2));
        assert!(!labels.set(std::slice::from_ref(&a), Some(COLORS)), "нет такого цвета");
        assert!(labels.set(std::slice::from_ref(&a), None));
        assert_eq!(labels.get(&a), None);
    }

    #[test]
    fn follows_renames_including_contents() {
        let mut labels = Labels::default();
        labels.set(&[PathBuf::from("/d/old"), PathBuf::from("/d/old/x.exe")], Some(1));
        labels.set(&[PathBuf::from("/d/older.txt")], Some(4));
        assert!(labels.moved(Path::new("/d/old"), Path::new("/e/new")));
        assert_eq!(labels.get(Path::new("/e/new")), Some(1));
        assert_eq!(labels.get(Path::new("/e/new/x.exe")), Some(1));
        assert_eq!(labels.get(Path::new("/d/older.txt")), Some(4), "похожее имя не трогается");
        assert_eq!(labels.get(Path::new("/d/old")), None);
    }

    #[test]
    fn round_trips_and_rejects_garbage() {
        let mut labels = Labels::default();
        labels.set(&[PathBuf::from("/a")], Some(5));
        assert_eq!(Labels::parse(&labels.to_json()).unwrap().get(Path::new("/a")), Some(5));
        assert!(Labels::parse("{не json").is_none());
        let wrong = r#"{"version":1,"labels":{"/a":9,"/b":0}}"#;
        let parsed = Labels::parse(wrong).unwrap();
        assert_eq!((parsed.get(Path::new("/a")), parsed.get(Path::new("/b"))), (None, Some(0)));
    }
}
