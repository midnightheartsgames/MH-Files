//! Вид папки, как в OneCommander: выбранные «Таблица» или «Плитки» запоминаются для папки
//! и возвращаются, когда её открывают снова. Хранятся в своём файле `folder_views.json`,
//! чтобы не зависеть от восстановления сеанса.

use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::location::Location;
use crate::session::ViewMode;

/// Сколько папок помнить; давно не открывавшиеся забываются первыми.
pub const LIMIT: usize = 2000;

const VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct Remembered {
    /// Ключ места (см. [`key`]).
    place: String,
    view: ViewMode,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct FolderViews {
    #[serde(default)]
    version: u32,
    /// Недавно выбранные — первыми.
    #[serde(default)]
    views: Vec<Remembered>,
}

fn path_key(path: &Path) -> String {
    let text = path.to_string_lossy();
    if cfg!(windows) { text.to_lowercase() } else { text.into_owned() }
}

/// Ключ места, вид которого запоминается: папка или папка в архиве. Поиск, дубликаты,
/// корзина и прочие особые места своего вида не помнят.
fn key(location: &Location) -> Option<String> {
    match location {
        Location::Dir(path) => Some(path_key(path)),
        Location::Archive { archive, inner } => {
            let inner = if cfg!(windows) { inner.to_lowercase() } else { inner.clone() };
            Some(format!("{}::{inner}", path_key(archive)))
        }
        _ => None,
    }
}

impl FolderViews {
    /// Разобрать `folder_views.json`. `None` — файл испорчен.
    pub fn parse(json: &str) -> Option<FolderViews> {
        let mut views: FolderViews = serde_json::from_str(json).ok()?;
        views.views.retain(|r| r.view != ViewMode::Columns);
        views.views.truncate(LIMIT);
        views.version = VERSION;
        Some(views)
    }

    pub fn to_json(&self) -> String {
        let views = FolderViews { version: VERSION, views: self.views.clone() };
        serde_json::to_string_pretty(&views).unwrap_or_default()
    }

    /// Помнит ли место свой вид вообще (папка или архив).
    pub fn applies(location: &Location) -> bool {
        key(location).is_some()
    }

    /// Запомненный вид места.
    pub fn get(&self, location: &Location) -> Option<ViewMode> {
        let key = key(location)?;
        self.views.iter().find(|r| r.place == key).map(|r| r.view)
    }

    /// Запомнить вид места. «Колонки» — способ ходить по папкам, а не вид одной папки:
    /// они не запоминаются. `true` — что-то изменилось.
    pub fn set(&mut self, location: &Location, view: ViewMode) -> bool {
        let Some(place) = key(location) else { return false };
        if view == ViewMode::Columns {
            return false;
        }
        if self.views.first().is_some_and(|r| r.place == place && r.view == view) {
            return false;
        }
        self.views.retain(|r| r.place != place);
        self.views.insert(0, Remembered { place, view });
        self.views.truncate(LIMIT);
        true
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    fn dir(path: &str) -> Location {
        Location::Dir(PathBuf::from(path))
    }

    #[test]
    fn remembers_per_folder() {
        let mut views = FolderViews::default();
        assert!(views.set(&dir("/photos"), ViewMode::Grid));
        assert!(!views.set(&dir("/photos"), ViewMode::Grid), "то же самое — без изменений");
        assert!(views.set(&dir("/docs"), ViewMode::Details));
        assert_eq!(views.get(&dir("/photos")), Some(ViewMode::Grid));
        assert_eq!(views.get(&dir("/docs")), Some(ViewMode::Details));
        assert_eq!(views.get(&dir("/other")), None);
        assert!(views.set(&dir("/photos"), ViewMode::Details));
        assert_eq!(views.get(&dir("/photos")), Some(ViewMode::Details));
    }

    #[test]
    fn columns_and_special_places_are_not_remembered() {
        let mut views = FolderViews::default();
        assert!(!views.set(&dir("/a"), ViewMode::Columns));
        assert!(!views.set(&Location::Computer, ViewMode::Grid));
        assert!(!views.set(&Location::Index { query: "x".into() }, ViewMode::Grid));
        assert_eq!(views, FolderViews::default());
    }

    #[test]
    fn archives_are_remembered_by_inner_folder() {
        let mut views = FolderViews::default();
        let root = Location::Archive { archive: PathBuf::from("/a.zip"), inner: String::new() };
        let inner = Location::Archive { archive: PathBuf::from("/a.zip"), inner: "img".into() };
        views.set(&inner, ViewMode::Grid);
        assert_eq!(views.get(&inner), Some(ViewMode::Grid));
        assert_eq!(views.get(&root), None);
    }

    #[test]
    fn oldest_are_forgotten_and_json_round_trips() {
        let mut views = FolderViews::default();
        for i in 0..LIMIT + 5 {
            views.set(&dir(&format!("/d{i}")), ViewMode::Grid);
        }
        assert_eq!(views.get(&dir("/d0")), None, "самая старая забыта");
        assert_eq!(views.get(&dir(&format!("/d{}", LIMIT + 4))), Some(ViewMode::Grid));
        let parsed = FolderViews::parse(&views.to_json()).unwrap();
        assert_eq!(parsed.to_json(), views.to_json());
        assert!(FolderViews::parse("garbage").is_none());
    }
}
