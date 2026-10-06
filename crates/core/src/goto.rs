//! GoTo (Ctrl+G): переход к папке по нескольким буквам. Здесь — разбор ввода и ранжирование;
//! кандидатов собирает интерфейс, дописывание путей читает воркер.

use std::path::{Path, PathBuf};

use crate::fuzzy::Fuzzy;
use crate::location::normalize;

/// Откуда кандидат. Порядок — приоритет при пустом запросе и при равной оценке.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Source {
    /// Введён путь целиком.
    Typed,
    /// Дописывание введённого пути.
    Completion,
    Tab,
    Favorite,
    Place,
    Drive,
    Recent,
}

impl Source {
    pub fn title(self) -> &'static str {
        match self {
            Source::Typed => "путь",
            Source::Completion => "папка",
            Source::Tab => "вкладка",
            Source::Favorite => "избранное",
            Source::Place => "место",
            Source::Drive => "диск",
            Source::Recent => "недавнее",
        }
    }

    fn bonus(self) -> u32 {
        match self {
            Source::Typed | Source::Completion => 60,
            Source::Tab | Source::Favorite => 30,
            Source::Place | Source::Drive => 20,
            Source::Recent => 0,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    pub label: String,
    pub path: PathBuf,
    pub source: Source,
}

/// Подставляет переменные окружения: `%appdata%\Code`, `~\Downloads`.
pub fn expand(input: &str) -> String {
    expand_with(input, |name| std::env::var(name).ok())
}

pub fn expand_with(input: &str, lookup: impl Fn(&str) -> Option<String>) -> String {
    let mut text = input.trim().trim_matches('"').to_string();
    if (text == "~" || text.starts_with("~\\") || text.starts_with("~/"))
        && let Some(home) = lookup("USERPROFILE").or_else(|| lookup("HOME"))
    {
        text.replace_range(..1, &home);
    }
    let mut result = String::with_capacity(text.len());
    let mut rest = text.as_str();
    while let Some(start) = rest.find('%') {
        result.push_str(&rest[..start]);
        let after = &rest[start + 1..];
        match after.find('%') {
            Some(end) if end > 0 => {
                let name = &after[..end];
                match lookup(name) {
                    Some(value) => result.push_str(&value),
                    None => {
                        result.push('%');
                        result.push_str(name);
                        result.push('%');
                    }
                }
                rest = &after[end + 1..];
            }
            _ => {
                result.push('%');
                rest = after;
            }
        }
    }
    result.push_str(rest);
    result
}

/// Похож ли ввод на путь (а не на слова для поиска по кандидатам).
pub fn looks_like_path(text: &str) -> bool {
    let bytes = text.as_bytes();
    let drive = bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':';
    drive
        || text.starts_with("\\\\")
        || text.starts_with('/')
        || text.starts_with('%')
        || text.starts_with('~')
}

/// Делит введённый путь на папку и начало имени для дописывания:
/// `C:\Us` → (`C:\`, `Us`), `C:\Users\` → (`C:\Users`, ``).
pub fn split_for_completion(text: &str) -> Option<(PathBuf, String)> {
    let cut = text.rfind(['\\', '/'])?;
    let (dir, prefix) = (&text[..=cut], &text[cut + 1..]);
    Some((normalize(Path::new(dir)), prefix.to_string()))
}

/// Кандидаты по запросу, лучшие первыми, без повторов путей.
pub fn rank(query: &str, candidates: Vec<Candidate>, limit: usize) -> Vec<Candidate> {
    let mut fuzzy = Fuzzy::for_paths(query);
    let mut scored: Vec<(u32, Candidate)> = candidates
        .into_iter()
        .filter_map(|candidate| {
            if matches!(candidate.source, Source::Typed | Source::Completion) {
                return Some((u32::MAX - candidate.source as u32, candidate));
            }
            let by_label = fuzzy.score(&candidate.label).map(|s| s + 20);
            let by_path = fuzzy.score(&candidate.path.to_string_lossy());
            let score = by_label.max(by_path)?;
            Some((score + candidate.source.bonus(), candidate))
        })
        .collect();
    scored.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.source.cmp(&b.1.source)));
    let mut seen = std::collections::HashSet::new();
    scored
        .into_iter()
        .map(|(_, candidate)| candidate)
        .filter(|candidate| seen.insert(candidate.path.to_string_lossy().to_lowercase()))
        .take(limit)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env(name: &str) -> Option<String> {
        match name.to_lowercase().as_str() {
            "appdata" => Some("C:\\Users\\me\\AppData\\Roaming".into()),
            "userprofile" => Some("C:\\Users\\me".into()),
            _ => None,
        }
    }

    #[test]
    fn expands_variables() {
        assert_eq!(expand_with("%appdata%\\Code", env), "C:\\Users\\me\\AppData\\Roaming\\Code");
        assert_eq!(expand_with("~\\Downloads", env), "C:\\Users\\me\\Downloads");
        assert_eq!(expand_with("%nope%\\x", env), "%nope%\\x");
        assert_eq!(expand_with("100% sure", env), "100% sure");
        assert_eq!(expand_with("\"C:\\a b\"", env), "C:\\a b");
    }

    #[test]
    fn path_detection_and_split() {
        assert!(looks_like_path("C:\\Users"));
        assert!(looks_like_path("\\\\NAS\\Media"));
        assert!(!looks_like_path("proj"));
        let (dir, prefix) = split_for_completion("/home/us").unwrap();
        assert_eq!((dir, prefix.as_str()), (PathBuf::from("/home"), "us"));
        assert_eq!(split_for_completion("proj"), None);
    }

    #[test]
    fn ranking_dedupes_and_prefers_better_sources() {
        let c = |label: &str, path: &str, source| Candidate {
            label: label.into(),
            path: PathBuf::from(path),
            source,
        };
        let ranked = rank(
            "proj",
            vec![
                c("Projects", "/f/Projects", Source::Recent),
                c("IdeaProjects", "/k/IdeaProjects", Source::Favorite),
                c("Projects", "/F/projects", Source::Recent),
                c("Music", "/music", Source::Place),
            ],
            10,
        );
        let labels: Vec<&str> = ranked.iter().map(|c| c.label.as_str()).collect();
        assert_eq!(labels.len(), 2);
        assert!(!labels.contains(&"Music"));
    }
}
