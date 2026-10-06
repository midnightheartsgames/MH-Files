//! Нечёткий поиск для палитры команд и GoTo (`nucleo-matcher`, как в Helix).

use nucleo_matcher::pattern::{CaseMatching, Normalization, Pattern};
use nucleo_matcher::{Config, Matcher, Utf32Str};

/// Запрос, разобранный один раз и применяемый ко многим строкам.
pub struct Fuzzy {
    pattern: Pattern,
    matcher: Matcher,
    buffer: Vec<char>,
    empty: bool,
}

impl Fuzzy {
    pub fn new(query: &str) -> Fuzzy {
        Fuzzy::with_config(query, Config::DEFAULT)
    }

    /// Для путей: разделители считаются границами слов.
    pub fn for_paths(query: &str) -> Fuzzy {
        let mut config = Config::DEFAULT;
        config.set_match_paths();
        Fuzzy::with_config(query, config)
    }

    fn with_config(query: &str, config: Config) -> Fuzzy {
        Fuzzy {
            pattern: Pattern::parse(query, CaseMatching::Ignore, Normalization::Smart),
            matcher: Matcher::new(config),
            buffer: Vec::new(),
            empty: query.trim().is_empty(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.empty
    }

    /// Оценка совпадения; `None` — не подходит. Пустой запрос подходит ко всему с нулём.
    pub fn score(&mut self, text: &str) -> Option<u32> {
        if self.empty {
            return Some(0);
        }
        let haystack = Utf32Str::new(text, &mut self.buffer);
        self.pattern.score(haystack, &mut self.matcher)
    }

    /// Номера совпавших символов (не байтов) — для подсветки.
    pub fn indices(&mut self, text: &str) -> Vec<usize> {
        if self.empty {
            return Vec::new();
        }
        let mut indices = Vec::new();
        let haystack = Utf32Str::new(text, &mut self.buffer);
        self.pattern.indices(haystack, &mut self.matcher, &mut indices);
        indices.sort_unstable();
        indices.dedup();
        indices.into_iter().map(|i| i as usize).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scores_and_indices() {
        let mut fuzzy = Fuzzy::new("bren");
        assert!(fuzzy.score("Batch Rename").is_some());
        assert!(fuzzy.score("Copy").is_none());
        assert_eq!(fuzzy.indices("Batch Rename").len(), 4);
        let mut empty = Fuzzy::new("  ");
        assert_eq!(empty.score("anything"), Some(0));
        let mut cyr = Fuzzy::new("переим");
        assert!(cyr.score("Переименовать").is_some());
    }

    #[test]
    fn prefers_tighter_matches() {
        let mut fuzzy = Fuzzy::for_paths("proj");
        let tight = fuzzy.score("IdeaProjects").unwrap();
        let loose = fuzzy.score("p-xx-r-xx-o-xx-j").unwrap_or(0);
        assert!(tight > loose);
    }
}
