//! Быстрый фильтр папки (Ctrl+F): мгновенно, по уже загруженному списку.
//!
//! Правила простые и предсказуемые:
//! * `*` и `?` — маска по всему имени (`*.png`, `IMG_????.jpg`);
//! * иначе каждое слово запроса должно встретиться в имени, без учёта регистра и порядка
//!   (`qwen gguf` находит `Qwen2.5-7B.Q4.gguf`).

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Filter {
    kind: Kind,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
enum Kind {
    #[default]
    All,
    Words(Vec<String>),
    Glob(Vec<char>),
}

impl Filter {
    pub fn new(query: &str) -> Filter {
        let query = query.trim();
        let kind = if query.is_empty() {
            Kind::All
        } else if query.contains(['*', '?']) {
            Kind::Glob(query.to_lowercase().chars().collect())
        } else {
            Kind::Words(query.to_lowercase().split_whitespace().map(str::to_string).collect())
        };
        Filter { kind }
    }

    pub fn is_empty(&self) -> bool {
        self.kind == Kind::All
    }

    pub fn matches(&self, name: &str) -> bool {
        match &self.kind {
            Kind::All => true,
            Kind::Words(words) => {
                let name = name.to_lowercase();
                words.iter().all(|word| name.contains(word.as_str()))
            }
            Kind::Glob(pattern) => {
                let name: Vec<char> = name.to_lowercase().chars().collect();
                glob(pattern, &name)
            }
        }
    }
}

/// Маска с `*` и `?` без рекурсии: жадный проход с откатом к последней звёздочке.
fn glob(pattern: &[char], text: &[char]) -> bool {
    let (mut p, mut t) = (0, 0);
    let mut star: Option<(usize, usize)> = None;
    while t < text.len() {
        if p < pattern.len() && (pattern[p] == '?' || pattern[p] == text[t]) {
            p += 1;
            t += 1;
        } else if p < pattern.len() && pattern[p] == '*' {
            star = Some((p, t));
            p += 1;
        } else if let Some((sp, st)) = star {
            p = sp + 1;
            t = st + 1;
            star = Some((sp, st + 1));
        } else {
            return false;
        }
    }
    pattern[p..].iter().all(|&c| c == '*')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn words_in_any_order() {
        let filter = Filter::new("gguf  qwen");
        assert!(filter.matches("Qwen2.5-7B.Q4.gguf"));
        assert!(!filter.matches("llama.gguf"));
        assert!(Filter::new("  ").is_empty());
        assert!(Filter::new("фото").matches("Мои ФОТО 2024"));
    }

    #[test]
    fn globs() {
        assert!(Filter::new("*.png").matches("Shot.PNG"));
        assert!(!Filter::new("*.png").matches("shot.png.txt"));
        assert!(Filter::new("img_????.jpg").matches("IMG_0042.jpg"));
        assert!(!Filter::new("img_????.jpg").matches("IMG_042.jpg"));
        assert!(Filter::new("*a*b*").matches("xxaxxbxx"));
        assert!(Filter::new("*").matches(""));
    }
}
