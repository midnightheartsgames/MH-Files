//! Где находится вкладка: «Этот компьютер», папка или результаты поиска.

use std::path::{Component, Path, PathBuf};

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Location {
    /// Список дисков.
    Computer,
    Dir(PathBuf),
    /// Рекурсивный поиск по `root`.
    Search {
        root: PathBuf,
        query: String,
    },
}

/// Звено строки пути.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Crumb {
    pub label: String,
    pub location: Location,
}

pub const COMPUTER_TITLE: &str = "Этот компьютер";

impl Location {
    /// Папка, в которую можно вставлять и создавать. У поиска и «Этого компьютера» её нет.
    pub fn dir(&self) -> Option<&Path> {
        match self {
            Location::Dir(path) => Some(path),
            _ => None,
        }
    }

    /// Заголовок вкладки.
    pub fn title(&self) -> String {
        match self {
            Location::Computer => COMPUTER_TITLE.to_string(),
            Location::Dir(path) => path_label(path),
            Location::Search { query, .. } => format!("Поиск: {query}"),
        }
    }

    /// На уровень выше. У корня диска — «Этот компьютер».
    pub fn parent(&self) -> Option<Location> {
        match self {
            Location::Computer => None,
            Location::Dir(path) => {
                Some(path.parent().map_or(Location::Computer, |p| Location::Dir(p.to_path_buf())))
            }
            Location::Search { root, .. } => Some(Location::Dir(root.clone())),
        }
    }

    /// Звенья строки пути от «Этого компьютера» до текущего места.
    pub fn crumbs(&self) -> Vec<Crumb> {
        let mut crumbs = vec![Crumb { label: COMPUTER_TITLE.into(), location: Location::Computer }];
        let path = match self {
            Location::Computer => return crumbs,
            Location::Dir(path) => path,
            Location::Search { root, .. } => root,
        };
        let mut ancestors: Vec<&Path> = path.ancestors().collect();
        ancestors.reverse();
        for ancestor in ancestors {
            crumbs.push(Crumb {
                label: path_label(ancestor),
                location: Location::Dir(ancestor.to_path_buf()),
            });
        }
        if let Location::Search { query, .. } = self {
            crumbs.push(Crumb { label: format!("Поиск «{query}»"), location: self.clone() });
        }
        crumbs
    }
}

/// Подпись папки: последнее имя, а у корня — `C:`, `\\сервер\ресурс` или `/`.
pub fn path_label(path: &Path) -> String {
    match path.file_name() {
        Some(name) => name.to_string_lossy().into_owned(),
        None => {
            let text = path.to_string_lossy();
            let trimmed = text.trim_end_matches(['\\', '/']);
            if trimmed.is_empty() { text.into_owned() } else { trimmed.to_string() }
        }
    }
}

/// Это корень диска или общего ресурса.
pub fn is_root(path: &Path) -> bool {
    path.parent().is_none()
}

/// Приводит введённый путь к обычному виду: `C:` → `C:\`, `C:\a\..\b` → `C:\b`, убирает
/// хвостовой разделитель. Диск не трогает.
pub fn normalize(path: &Path) -> PathBuf {
    let mut result = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Prefix(prefix) => {
                result.push(prefix.as_os_str());
                result.push(std::path::MAIN_SEPARATOR_STR);
            }
            Component::RootDir => result.push(std::path::MAIN_SEPARATOR_STR),
            Component::CurDir => {}
            Component::ParentDir => {
                if result.parent().is_some() {
                    result.pop();
                }
            }
            Component::Normal(name) => result.push(name),
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parent_of_root_is_computer() {
        let root = Location::Dir(PathBuf::from("/"));
        assert_eq!(root.parent(), Some(Location::Computer));
        assert_eq!(Location::Computer.parent(), None);
        let dir = Location::Dir(PathBuf::from("/home/user"));
        assert_eq!(dir.parent(), Some(Location::Dir(PathBuf::from("/home"))));
    }

    #[test]
    fn crumbs_and_titles() {
        let dir = Location::Dir(PathBuf::from("/home/user"));
        let labels: Vec<String> = dir.crumbs().into_iter().map(|c| c.label).collect();
        assert_eq!(labels, [COMPUTER_TITLE, "/", "home", "user"]);
        assert_eq!(dir.title(), "user");
        assert_eq!(Location::Dir(PathBuf::from("/")).title(), "/");
        let search = Location::Search { root: PathBuf::from("/home"), query: "x".into() };
        assert_eq!(search.crumbs().last().unwrap().label, "Поиск «x»");
        assert_eq!(search.dir(), None);
    }

    #[test]
    fn normalizes() {
        assert_eq!(normalize(Path::new("/a/./b/../c/")), PathBuf::from("/a/c"));
        assert_eq!(normalize(Path::new("/..")), PathBuf::from("/"));
    }

    #[cfg(windows)]
    #[test]
    fn windows_roots() {
        assert_eq!(normalize(Path::new("C:")), PathBuf::from("C:\\"));
        assert_eq!(path_label(Path::new("C:\\")), "C:");
        let labels: Vec<String> = Location::Dir(PathBuf::from("C:\\Users"))
            .crumbs()
            .into_iter()
            .map(|c| c.label)
            .collect();
        assert_eq!(labels, [COMPUTER_TITLE, "C:", "Users"]);
    }
}
