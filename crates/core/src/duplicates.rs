//! Группы одинаковых файлов и выбор «лишних» копий. Поиск с чтением файлов — в `mh-files-fs`;
//! здесь только то, что проверяется без диска.

use std::path::{Path, PathBuf};
use std::time::SystemTime;

/// Файлы с одинаковым содержимым.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Group {
    /// Размер одного файла.
    pub size: u64,
    pub files: Vec<Member>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Member {
    pub path: PathBuf,
    pub modified: Option<SystemTime>,
}

/// Какую копию в группе оставить; остальные — «лишние».
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Keep {
    /// Самую старую: обычно это оригинал, а остальное — скачанное или скопированное позже.
    #[default]
    Oldest,
    Newest,
    /// С самым коротким путём: ближе к корню — скорее «настоящее» место.
    ShortestPath,
}

impl Keep {
    pub const ALL: [Keep; 3] = [Keep::Oldest, Keep::Newest, Keep::ShortestPath];

    pub fn title(self) -> &'static str {
        match self {
            Keep::Oldest => "оставить самую старую",
            Keep::Newest => "оставить самую новую",
            Keep::ShortestPath => "оставить с самым коротким путём",
        }
    }
}

impl Group {
    /// Сколько места занимают копии сверх одной.
    pub fn wasted(&self) -> u64 {
        self.size * self.files.len().saturating_sub(1) as u64
    }

    /// Номер копии, которая остаётся.
    pub fn keeper(&self, keep: Keep) -> usize {
        let index = |best: Option<(usize, &Member)>| best.map_or(0, |(i, _)| i);
        match keep {
            Keep::Oldest => index(self.files.iter().enumerate().min_by(|a, b| {
                a.1.modified.cmp(&b.1.modified).then_with(|| a.1.path.cmp(&b.1.path))
            })),
            Keep::Newest => index(self.files.iter().enumerate().max_by(|a, b| {
                a.1.modified.cmp(&b.1.modified).then_with(|| b.1.path.cmp(&a.1.path))
            })),
            Keep::ShortestPath => index(self.files.iter().enumerate().min_by(|a, b| {
                let depth = |c: &Member| c.path.components().count();
                depth(a.1)
                    .cmp(&depth(b.1))
                    .then_with(|| a.1.path.as_os_str().len().cmp(&b.1.path.as_os_str().len()))
                    .then_with(|| a.1.path.cmp(&b.1.path))
            })),
        }
    }

    /// Все копии, кроме оставляемой.
    pub fn extra(&self, keep: Keep) -> impl Iterator<Item = &PathBuf> + '_ {
        let keeper = self.keeper(keep);
        self.files.iter().enumerate().filter(move |(i, _)| *i != keeper).map(|(_, c)| &c.path)
    }
}

/// Пропустить ли папку или файл при поиске дубликатов. Правило — полный путь папки (есть
/// `\\`, `/` или `:`; всё внутри неё), маска с `*` и `?` или имя целиком; регистр не важен.
pub fn excluded(path: &Path, rules: &[String]) -> bool {
    let name = path.file_name().map(|n| n.to_string_lossy().to_lowercase()).unwrap_or_default();
    let lower_path = path.to_string_lossy().to_lowercase().replace('\\', "/");
    rules.iter().map(|rule| rule.trim()).filter(|rule| !rule.is_empty()).any(|rule| {
        let rule = rule.to_lowercase();
        if rule.contains(['\\', '/', ':']) {
            let rule = rule.replace('\\', "/");
            let rule = rule.trim_end_matches('/');
            lower_path == rule || lower_path.starts_with(&format!("{rule}/"))
        } else if rule.contains(['*', '?']) {
            let pattern: Vec<char> = rule.chars().collect();
            let name: Vec<char> = name.chars().collect();
            crate::filter::glob_match(&pattern, &name)
        } else {
            name == rule
        }
    })
}

impl Group {
    /// Пары «оставляемая копия → лишняя» — для замены лишних жёсткими ссылками.
    pub fn link_plan(&self, keep: Keep) -> Vec<(PathBuf, Member)> {
        let keeper = self.keeper(keep);
        let target = self.files[keeper].path.clone();
        self.files
            .iter()
            .enumerate()
            .filter(|(i, _)| *i != keeper)
            .map(|(_, member)| (target.clone(), member.clone()))
            .collect()
    }
}

/// Порядок показа: сначала группы, которые освободят больше места.
pub fn sort_groups(groups: &mut [Group]) {
    for group in groups.iter_mut() {
        group.files.sort_by(|a, b| a.path.cmp(&b.path));
    }
    groups.sort_by(|a, b| {
        b.wasted().cmp(&a.wasted()).then_with(|| a.files[0].path.cmp(&b.files[0].path))
    });
}

/// Итог: групп, лишних копий, сколько можно освободить.
pub fn totals(groups: &[Group]) -> (usize, usize, u64) {
    let extra = groups.iter().map(|g| g.files.len().saturating_sub(1)).sum();
    (groups.len(), extra, groups.iter().map(Group::wasted).sum())
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    fn copy(path: &str, age: u64) -> Member {
        Member {
            path: PathBuf::from(path),
            modified: Some(SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000 - age)),
        }
    }

    #[test]
    fn exclusions() {
        let rules: Vec<String> =
            ["node_modules", "*.TMP", "D:\\Резерв\\", " "].into_iter().map(String::from).collect();
        assert!(excluded(Path::new("C:/p/Node_Modules"), &rules));
        assert!(excluded(Path::new("C:/p/a.tmp"), &rules));
        assert!(excluded(Path::new("D:\\резерв"), &rules));
        assert!(excluded(Path::new("D:\\Резерв\\2024"), &rules));
        assert!(!excluded(Path::new("D:\\Резервы"), &rules));
        assert!(!excluded(Path::new("C:/p/a.txt"), &rules));
    }

    #[test]
    fn link_plan_points_to_keeper() {
        let group = Group { size: 5, files: vec![copy("/a", 1), copy("/b", 9), copy("/c", 3)] };
        let plan = group.link_plan(Keep::Oldest);
        assert_eq!(plan.len(), 2);
        assert!(plan.iter().all(|(target, _)| target == &PathBuf::from("/b")));
        assert_eq!(plan[0].1.path, PathBuf::from("/a"));
    }

    #[test]
    fn keepers_and_totals() {
        let group = Group {
            size: 100,
            files: vec![copy("/a/b/c/x.jpg", 10), copy("/a/x.jpg", 5), copy("/z/x (2).jpg", 50)],
        };
        assert_eq!(group.wasted(), 200);
        assert_eq!(group.keeper(Keep::Oldest), 2);
        assert_eq!(group.keeper(Keep::Newest), 1);
        assert_eq!(group.keeper(Keep::ShortestPath), 1);
        let extra: Vec<&PathBuf> = group.extra(Keep::Oldest).collect();
        assert_eq!(extra, [&PathBuf::from("/a/b/c/x.jpg"), &PathBuf::from("/a/x.jpg")]);

        let small = Group { size: 1, files: vec![copy("/q", 1), copy("/r", 1)] };
        let mut groups = vec![small, group];
        sort_groups(&mut groups);
        assert_eq!(groups[0].size, 100, "больше освобождает — выше");
        assert_eq!(groups[0].files[0].path, PathBuf::from("/a/b/c/x.jpg"));
        assert_eq!(totals(&groups), (2, 3, 201));
    }
}
