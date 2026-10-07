//! Где находится вкладка: «Этот компьютер», папка или результаты поиска.

use std::path::{Component, Path, PathBuf};

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Location {
    /// Список дисков.
    Computer,
    Dir(PathBuf),
    /// Рекурсивный поиск по `root`: по имени или (`content`) по тексту внутри файлов.
    Search {
        root: PathBuf,
        query: String,
        #[serde(default)]
        content: bool,
    },
    /// Поиск по индексу всех дисков.
    Index {
        query: String,
    },
    /// Папка внутри архива: `inner` — путь в архиве через `/`, пусто — корень архива.
    Archive {
        archive: PathBuf,
        inner: String,
    },
    /// Найденные дубликаты в папках `roots`.
    Duplicates {
        roots: Vec<PathBuf>,
    },
    /// Сортировщик: разложить содержимое `root` по папкам категорий.
    Sort {
        root: PathBuf,
    },
}

/// Звено строки пути.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Crumb {
    pub label: String,
    pub location: Location,
}

pub const COMPUTER_TITLE: &str = "Этот компьютер";
pub const INDEX_TITLE: &str = "Поиск по дискам";
pub const DUPLICATES_TITLE: &str = "Дубликаты";
pub const SORT_TITLE: &str = "Разложить";

impl Location {
    /// Папка, в которую можно вставлять и создавать. У поиска, архива и «Этого компьютера»
    /// её нет.
    pub fn dir(&self) -> Option<&Path> {
        match self {
            Location::Dir(path) => Some(path),
            _ => None,
        }
    }

    /// Результаты поиска (по папке, по дискам, дубликаты): у записей разные папки.
    pub fn is_search(&self) -> bool {
        matches!(
            self,
            Location::Search { .. } | Location::Index { .. } | Location::Duplicates { .. }
        )
    }

    /// Папка внутри архива как путь: `C:\a.zip\docs`. Такого пути на диске нет — по нему
    /// записи архива узнают своё место.
    pub fn archive_path(archive: &Path, inner: &str) -> PathBuf {
        let mut path = archive.to_path_buf();
        for part in inner.split('/').filter(|p| !p.is_empty()) {
            path.push(part);
        }
        path
    }

    /// Куда ведёт папка с путём `path` из этого места: внутри архива — глубже в архив,
    /// иначе — обычная папка.
    pub fn enter(&self, path: &Path) -> Location {
        match self {
            Location::Archive { archive, inner } => {
                let name = path.file_name().map(|n| n.to_string_lossy()).unwrap_or_default();
                let inner = inner.trim_matches('/');
                let inner =
                    if inner.is_empty() { name.into_owned() } else { format!("{inner}/{name}") };
                Location::Archive { archive: archive.clone(), inner }
            }
            _ => Location::Dir(path.to_path_buf()),
        }
    }

    /// Путь этого места так, как его видят записи родителя: у папки — сама папка, в архиве —
    /// путь через архив. По нему в колонке родителя подсвечивается дорога сюда.
    pub fn own_path(&self) -> Option<PathBuf> {
        match self {
            Location::Dir(path) => Some(path.clone()),
            Location::Archive { archive, inner } => Some(Location::archive_path(archive, inner)),
            _ => None,
        }
    }

    /// Заголовок вкладки.
    pub fn title(&self) -> String {
        match self {
            Location::Computer => COMPUTER_TITLE.to_string(),
            Location::Dir(path) => path_label(path),
            Location::Search { query, content: true, .. } => format!("Текст: {query}"),
            Location::Search { query, .. } => format!("Поиск: {query}"),
            Location::Index { query } if query.is_empty() => INDEX_TITLE.to_string(),
            Location::Index { query } => format!("Везде: {query}"),
            Location::Archive { archive, inner } => match inner.rsplit('/').find(|p| !p.is_empty())
            {
                Some(name) => name.to_string(),
                None => path_label(archive),
            },
            Location::Duplicates { roots } => match roots.as_slice() {
                [root] => format!("{DUPLICATES_TITLE}: {}", path_label(root)),
                _ => DUPLICATES_TITLE.to_string(),
            },
            Location::Sort { root } => format!("{SORT_TITLE}: {}", path_label(root)),
        }
    }

    /// На уровень выше. У корня диска — «Этот компьютер», у корня архива — папка архива.
    pub fn parent(&self) -> Option<Location> {
        match self {
            Location::Computer => None,
            Location::Dir(path) => {
                Some(path.parent().map_or(Location::Computer, |p| Location::Dir(p.to_path_buf())))
            }
            Location::Search { root, .. } => Some(Location::Dir(root.clone())),
            Location::Index { .. } => Some(Location::Computer),
            Location::Archive { archive, inner } => {
                let inner = inner.trim_matches('/');
                if inner.is_empty() {
                    return Some(
                        archive
                            .parent()
                            .map_or(Location::Computer, |p| Location::Dir(p.to_path_buf())),
                    );
                }
                let parent = inner.rsplit_once('/').map_or("", |(parent, _)| parent);
                Some(Location::Archive { archive: archive.clone(), inner: parent.to_string() })
            }
            Location::Duplicates { roots } => match roots.as_slice() {
                [root] => Some(Location::Dir(root.clone())),
                _ => Some(Location::Computer),
            },
            Location::Sort { root } => Some(Location::Dir(root.clone())),
        }
    }

    /// Звенья строки пути от «Этого компьютера» до текущего места.
    pub fn crumbs(&self) -> Vec<Crumb> {
        let mut crumbs = vec![Crumb { label: COMPUTER_TITLE.into(), location: Location::Computer }];
        let path = match self {
            Location::Computer => return crumbs,
            Location::Dir(path) => path,
            Location::Search { root, .. } => root,
            Location::Archive { archive, .. } => archive.parent().unwrap_or(archive),
            Location::Sort { root } => root,
            Location::Duplicates { roots } => match roots.as_slice() {
                [root] => root,
                _ => {
                    crumbs.push(Crumb { label: DUPLICATES_TITLE.into(), location: self.clone() });
                    return crumbs;
                }
            },
            Location::Index { query } => {
                let label = if query.is_empty() {
                    INDEX_TITLE.to_string()
                } else {
                    format!("{INDEX_TITLE} «{query}»")
                };
                crumbs.push(Crumb { label, location: self.clone() });
                return crumbs;
            }
        };
        let mut ancestors: Vec<&Path> = path.ancestors().collect();
        ancestors.reverse();
        for ancestor in ancestors {
            crumbs.push(Crumb {
                label: path_label(ancestor),
                location: Location::Dir(ancestor.to_path_buf()),
            });
        }
        match self {
            Location::Search { query, content, .. } => {
                let what = if *content { "Текст" } else { "Поиск" };
                crumbs.push(Crumb { label: format!("{what} «{query}»"), location: self.clone() });
            }
            Location::Duplicates { .. } => {
                crumbs.push(Crumb { label: DUPLICATES_TITLE.into(), location: self.clone() });
            }
            Location::Sort { .. } => {
                crumbs.push(Crumb { label: SORT_TITLE.into(), location: self.clone() });
            }
            Location::Archive { archive, inner } => {
                let root = Location::Archive { archive: archive.clone(), inner: String::new() };
                crumbs.push(Crumb { label: path_label(archive), location: root });
                let mut current = String::new();
                for part in inner.split('/').filter(|p| !p.is_empty()) {
                    if !current.is_empty() {
                        current.push('/');
                    }
                    current.push_str(part);
                    let location =
                        Location::Archive { archive: archive.clone(), inner: current.clone() };
                    crumbs.push(Crumb { label: part.to_string(), location });
                }
            }
            _ => {}
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
        let search =
            Location::Search { root: PathBuf::from("/home"), query: "x".into(), content: false };
        assert_eq!(search.crumbs().last().unwrap().label, "Поиск «x»");
        assert_eq!(search.dir(), None);
        let index = Location::Index { query: "x".into() };
        let labels: Vec<String> = index.crumbs().into_iter().map(|c| c.label).collect();
        assert_eq!(labels, [COMPUTER_TITLE, "Поиск по дискам «x»"]);
        assert_eq!(index.parent(), Some(Location::Computer));
        assert_eq!(index.title(), "Везде: x");
    }

    #[test]
    fn archives() {
        let archive = PathBuf::from("/home/a.zip");
        let inside = Location::Archive { archive: archive.clone(), inner: "docs/old".into() };
        assert_eq!(inside.title(), "old");
        assert_eq!(
            inside.parent(),
            Some(Location::Archive { archive: archive.clone(), inner: "docs".into() })
        );
        let root = Location::Archive { archive: archive.clone(), inner: String::new() };
        assert_eq!(root.title(), "a.zip");
        assert_eq!(root.parent(), Some(Location::Dir(PathBuf::from("/home"))));
        let labels: Vec<String> = inside.crumbs().into_iter().map(|c| c.label).collect();
        assert_eq!(labels, [COMPUTER_TITLE, "/", "home", "a.zip", "docs", "old"]);
        assert_eq!(inside.dir(), None);
        assert_eq!(
            inside.enter(Path::new("/home/a.zip/docs/old/new")),
            Location::Archive { archive: archive.clone(), inner: "docs/old/new".into() }
        );
        assert_eq!(root.own_path(), Some(archive.clone()));
        assert_eq!(
            Location::Dir("/x".into()).enter(Path::new("/x/y")),
            Location::Dir("/x/y".into())
        );
        assert_eq!(
            Location::archive_path(&archive, "docs/old/"),
            PathBuf::from("/home/a.zip/docs/old")
        );
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
