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

/// Маска с `*` и `?` (уже в нижнем регистре) против имени (тоже в нижнем).
pub fn glob_match(pattern: &[char], text: &[char]) -> bool {
    glob(pattern, text)
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

/// Запрос поиска по содержимому: слова — в тексте файла (все, в любом порядке, без учёта
/// регистра), маски `*.txt` — по имени файла (хотя бы одна, если они есть):
/// `договор аренды *.docx *.txt`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ContentQuery {
    masks: Vec<Vec<char>>,
    words: Vec<String>,
}

impl ContentQuery {
    pub fn new(query: &str) -> ContentQuery {
        let mut parsed = ContentQuery::default();
        for token in query.split_whitespace() {
            let token = token.to_lowercase();
            if token.contains(['*', '?']) {
                parsed.masks.push(token.chars().collect());
            } else {
                parsed.words.push(token);
            }
        }
        parsed
    }

    /// Искать нечего: без слов поиск по содержимому не имеет смысла.
    pub fn is_empty(&self) -> bool {
        self.words.is_empty()
    }

    /// Подходит ли файл по имени (маски).
    pub fn name_matches(&self, name: &str) -> bool {
        if self.masks.is_empty() {
            return true;
        }
        let name: Vec<char> = name.to_lowercase().chars().collect();
        self.masks.iter().any(|mask| glob(mask, &name))
    }

    /// Есть ли в тексте все слова. `text` — уже в нижнем регистре.
    pub fn text_matches(&self, lower_text: &str) -> bool {
        self.words.iter().all(|word| lower_text.contains(word.as_str()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn content_query_splits_masks_and_words() {
        let query = ContentQuery::new("Договор  аренды *.DOCX *.txt");
        assert!(!query.is_empty());
        assert!(query.name_matches("план.TXT") && query.name_matches("a.docx"));
        assert!(!query.name_matches("a.pdf"));
        assert!(query.text_matches("здесь аренды и договор квартиры"));
        assert!(!query.text_matches("только договор"));
        assert!(ContentQuery::new("*.txt").is_empty(), "одних масок мало");
        assert!(ContentQuery::new("слово").name_matches("любое.bin"));
    }

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
