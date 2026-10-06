//! Правила имён файлов Windows. Проверяются до операции, чтобы ошибка была понятной, а не
//! «Параметр задан неверно».

use std::collections::HashSet;

/// Символы, запрещённые в именах Windows.
pub const INVALID_CHARS: &[char] = &['<', '>', ':', '"', '/', '\\', '|', '?', '*'];

/// Предел длины одного имени в NTFS.
pub const MAX_NAME_CHARS: usize = 255;

const RESERVED: &[&str] = &[
    "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8",
    "COM9", "COM¹", "COM²", "COM³", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8",
    "LPT9", "LPT¹", "LPT²", "LPT³",
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NameError {
    Empty,
    InvalidChar(char),
    Reserved(String),
    TrailingDotOrSpace,
    TooLong,
}

impl std::fmt::Display for NameError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            NameError::Empty => write!(f, "пустое имя"),
            NameError::InvalidChar(c) if c.is_control() => {
                write!(f, "управляющий символ в имени")
            }
            NameError::InvalidChar(c) => write!(f, "символ «{c}» нельзя использовать в имени"),
            NameError::Reserved(name) => write!(f, "«{name}» — зарезервированное имя Windows"),
            NameError::TrailingDotOrSpace => {
                write!(f, "имя не может кончаться точкой или пробелом")
            }
            NameError::TooLong => write!(f, "имя длиннее {MAX_NAME_CHARS} символов"),
        }
    }
}

/// Проверяет одно имя (без пути).
pub fn validate(name: &str) -> Result<(), NameError> {
    if name.is_empty() || name == "." || name == ".." {
        return Err(NameError::Empty);
    }
    if let Some(c) = name.chars().find(|c| INVALID_CHARS.contains(c) || c.is_control()) {
        return Err(NameError::InvalidChar(c));
    }
    if name.ends_with(['.', ' ']) {
        return Err(NameError::TrailingDotOrSpace);
    }
    if name.encode_utf16().count() > MAX_NAME_CHARS {
        return Err(NameError::TooLong);
    }
    // `CON.txt` тоже зарезервировано: Windows смотрит на часть до первой точки.
    let base = name.split('.').next().unwrap_or(name).trim_end();
    if RESERVED.iter().any(|reserved| reserved.eq_ignore_ascii_case(base)) {
        return Err(NameError::Reserved(base.to_string()));
    }
    Ok(())
}

/// Свободное имя на основе `base`: `Новая папка`, `Новая папка (2)`… Сравнение без учёта
/// регистра, как в NTFS.
pub fn unique_name(base: &str, existing: &[&str]) -> String {
    let taken: HashSet<String> = existing.iter().map(|name| name.to_lowercase()).collect();
    if !taken.contains(&base.to_lowercase()) {
        return base.to_string();
    }
    (2..)
        .map(|n| format!("{base} ({n})"))
        .find(|name| !taken.contains(&name.to_lowercase()))
        .expect("бесконечная последовательность")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validation() {
        assert_eq!(validate("ok.txt"), Ok(()));
        assert_eq!(validate("Новый документ"), Ok(()));
        assert_eq!(validate(""), Err(NameError::Empty));
        assert_eq!(validate("a:b"), Err(NameError::InvalidChar(':')));
        assert_eq!(validate("con"), Err(NameError::Reserved("con".into())));
        assert_eq!(validate("NUL.tar.gz"), Err(NameError::Reserved("NUL".into())));
        assert_eq!(validate("console"), Ok(()));
        assert_eq!(validate("dot."), Err(NameError::TrailingDotOrSpace));
        assert_eq!(validate("space "), Err(NameError::TrailingDotOrSpace));
        assert_eq!(validate(&"x".repeat(256)), Err(NameError::TooLong));
        assert_eq!(validate("tab\there"), Err(NameError::InvalidChar('\t')));
    }

    #[test]
    fn unique_names() {
        assert_eq!(unique_name("Новая папка", &["a"]), "Новая папка");
        assert_eq!(
            unique_name("Новая папка", &["новая папка", "Новая папка (2)"]),
            "Новая папка (3)"
        );
    }
}
