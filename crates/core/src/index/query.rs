//! Язык запросов поиска по дискам.
//!
//! * слова — подстроки имени в любом порядке: `qwen gguf`;
//! * `*` и `?` — маска имени: `*.png`;
//! * `-слово` — исключить;
//! * слово с `\` или `/` ищется во всём пути: `models\qwen`;
//! * `"в кавычках"` — фраза с пробелами;
//! * фильтры: `ext:png,jpg` · `size:>100mb`, `size:<1kb`, `size:10mb..1gb` ·
//!   `dm:2026`, `dm:2026-09`, `dm:>2026-09-01`, `dm:сегодня|вчера|неделя|месяц` ·
//!   `type:папка|файл` (или `folder:`, `file:`) · `in:"K:\Models"` · `hidden:да`.

use std::path::PathBuf;

use chrono::{Datelike, Local, NaiveDate, TimeZone};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Term {
    /// Подстрока имени в нижнем регистре.
    Word { needle: String, negate: bool },
    /// Маска имени в нижнем регистре.
    Glob { pattern: Vec<char>, negate: bool },
    /// Подстрока полного пути в нижнем регистре, разделители — `\`.
    Path { needle: String, negate: bool },
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Query {
    pub terms: Vec<Term>,
    /// Расширения в нижнем регистре без точки.
    pub exts: Vec<String>,
    /// Размер файла в байтах, включительно.
    pub size: Option<(u64, u64)>,
    /// Время изменения, секунды Unix, включительно.
    pub modified: Option<(i64, i64)>,
    /// `Some(true)` — только папки, `Some(false)` — только файлы.
    pub dirs: Option<bool>,
    /// Только внутри этой папки.
    pub within: Option<PathBuf>,
    /// Показывать скрытые и системные.
    pub hidden: bool,
}

impl Query {
    /// Пустой запрос ничего не ищет: выдать весь диск — не поиск.
    pub fn is_empty(&self) -> bool {
        self.terms.is_empty()
            && self.exts.is_empty()
            && self.size.is_none()
            && self.modified.is_none()
            && self.within.is_none()
    }
}

/// Разбирает запрос. `now` — текущее время Unix (для «сегодня», «неделя»).
pub fn parse(text: &str, now: i64) -> Result<Query, String> {
    let mut query = Query::default();
    for token in tokenize(text) {
        let (negate, token) = match token.strip_prefix('-') {
            Some(rest) if !rest.is_empty() && !rest.contains(':') => (true, rest.to_string()),
            _ => (false, token),
        };
        if let Some((key, value)) = filter_parts(&token) {
            apply_filter(&mut query, &key, &value, now)?;
            continue;
        }
        let lower = token.to_lowercase();
        let term = if lower.contains(['\\', '/']) {
            Term::Path { needle: lower.replace('/', "\\"), negate }
        } else if lower.contains(['*', '?']) {
            Term::Glob { pattern: lower.chars().collect(), negate }
        } else {
            Term::Word { needle: lower, negate }
        };
        query.terms.push(term);
    }
    Ok(query)
}

/// Слова с учётом кавычек: `in:"C:\My Files" отчёт` → два слова.
fn tokenize(text: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    let mut quoted = false;
    for c in text.chars() {
        match c {
            '"' => quoted = !quoted,
            c if c.is_whitespace() && !quoted => {
                if !current.is_empty() {
                    tokens.push(std::mem::take(&mut current));
                }
            }
            c => current.push(c),
        }
    }
    if !current.is_empty() {
        tokens.push(current);
    }
    tokens
}

const KEYS: &[&str] = &["ext", "size", "dm", "type", "folder", "file", "in", "hidden"];

/// `ключ:значение`, если ключ известен. `C:\path` — не фильтр.
fn filter_parts(token: &str) -> Option<(String, String)> {
    let (key, value) = token.split_once(':')?;
    let key = key.to_lowercase();
    KEYS.contains(&key.as_str()).then(|| (key, value.to_string()))
}

fn apply_filter(query: &mut Query, key: &str, value: &str, now: i64) -> Result<(), String> {
    match key {
        "ext" => {
            query.exts.extend(
                value
                    .split([',', ';', '|'])
                    .map(|e| e.trim().trim_start_matches('.').to_lowercase())
                    .filter(|e| !e.is_empty()),
            );
        }
        "size" => {
            query.size = Some(parse_range(value, parse_size).ok_or_else(|| bad("size", value))?)
        }
        "dm" => query.modified = Some(parse_dates(value, now).ok_or_else(|| bad("dm", value))?),
        "type" => {
            query.dirs = match value.to_lowercase().as_str() {
                "folder" | "dir" | "папка" | "папки" => Some(true),
                "file" | "файл" | "файлы" => Some(false),
                _ => return Err(bad("type", value)),
            }
        }
        "folder" => query.dirs = Some(true),
        "file" => query.dirs = Some(false),
        "in" => {
            if value.is_empty() {
                return Err(bad("in", value));
            }
            query.within = Some(PathBuf::from(value));
        }
        "hidden" => {
            query.hidden = matches!(value.to_lowercase().as_str(), "" | "yes" | "да" | "1" | "true")
        }
        _ => {}
    }
    Ok(())
}

fn bad(key: &str, value: &str) -> String {
    format!("не понял «{key}:{value}»")
}

/// `>x`, `<x`, `x..y`, `x-y` или `x` (ровно — для дат «внутри периода»).
fn parse_range<T: Copy + Ord + Bounded>(
    value: &str,
    parse: impl Fn(&str) -> Option<(T, T)>,
) -> Option<(T, T)> {
    if let Some(rest) = value.strip_prefix(">=").or_else(|| value.strip_prefix('>')) {
        return Some((parse(rest)?.0, T::MAX));
    }
    if let Some(rest) = value.strip_prefix("<=").or_else(|| value.strip_prefix('<')) {
        return Some((T::MIN, parse(rest)?.1));
    }
    let split =
        value.split_once("..").or_else(|| value.split_once('-').filter(|(a, _)| !a.is_empty()));
    if let Some((from, to)) = split {
        // `2026-09-01` — это дата, а не диапазон: пробуем целиком, прежде чем делить.
        if let Some(whole) = parse(value) {
            return Some(whole);
        }
        let (from, to) = (parse(from)?, parse(to)?);
        return (from.0 <= to.1).then_some((from.0, to.1));
    }
    parse(value)
}

trait Bounded {
    const MIN: Self;
    const MAX: Self;
}

impl Bounded for u64 {
    const MIN: u64 = 0;
    const MAX: u64 = u64::MAX;
}

impl Bounded for i64 {
    const MIN: i64 = i64::MIN;
    const MAX: i64 = i64::MAX;
}

/// `100mb` → байты; один размер — это точка, `(n, n)`.
fn parse_size(text: &str) -> Option<(u64, u64)> {
    let text = text.trim().to_lowercase();
    let split =
        text.find(|c: char| !(c.is_ascii_digit() || c == '.' || c == ',')).unwrap_or(text.len());
    let (number, unit) = text.split_at(split);
    let number: f64 = number.replace(',', ".").parse().ok()?;
    let multiplier: f64 = match unit.trim() {
        "" | "b" | "б" | "байт" => 1.0,
        "k" | "kb" | "кб" => 1024.0,
        "m" | "mb" | "мб" => 1024.0 * 1024.0,
        "g" | "gb" | "гб" => 1024.0 * 1024.0 * 1024.0,
        "t" | "tb" | "тб" => 1024f64.powi(4),
        _ => return None,
    };
    let bytes = (number * multiplier).round() as u64;
    Some((bytes, bytes))
}

/// Период: год, месяц, день или слово; результат — начало и конец в секундах Unix.
fn parse_dates(value: &str, now: i64) -> Option<(i64, i64)> {
    let today = Local.timestamp_opt(now, 0).single()?.date_naive();
    let day_start = |date: NaiveDate| {
        Local.from_local_datetime(&date.and_hms_opt(0, 0, 0)?).earliest().map(|t| t.timestamp())
    };
    let period = |from: NaiveDate, to_exclusive: NaiveDate| {
        Some((day_start(from)?, day_start(to_exclusive)? - 1))
    };
    let word = match value.to_lowercase().as_str() {
        "today" | "сегодня" => period(today, today.succ_opt()?),
        "yesterday" | "вчера" => period(today.pred_opt()?, today),
        "week" | "неделя" => Some((day_start(today - chrono::Duration::days(6))?, now)),
        "month" | "месяц" => Some((day_start(today - chrono::Duration::days(29))?, now)),
        "year" | "год" => Some((day_start(today - chrono::Duration::days(364))?, now)),
        _ => None,
    };
    if word.is_some() {
        return word;
    }
    parse_range(value, |text: &str| {
        let parts: Vec<&str> = text.split(['-', '.']).collect();
        let numbers: Option<Vec<u32>> = parts.iter().map(|p| p.parse().ok()).collect();
        let numbers = numbers?;
        // Год-месяц-день или день.месяц.год — как пишут у нас.
        let (year, month, day) = match numbers.as_slice() {
            [y] if *y > 1900 => (*y as i32, None, None),
            [y, m] if *y > 1900 => (*y as i32, Some(*m), None),
            [y, m, d] if *y > 1900 => (*y as i32, Some(*m), Some(*d)),
            [d, m, y] if *y > 1900 => (*y as i32, Some(*m), Some(*d)),
            _ => return None,
        };
        match (month, day) {
            (None, _) => period(
                NaiveDate::from_ymd_opt(year, 1, 1)?,
                NaiveDate::from_ymd_opt(year + 1, 1, 1)?,
            ),
            (Some(m), None) => {
                let start = NaiveDate::from_ymd_opt(year, m, 1)?;
                let next = if m == 12 {
                    NaiveDate::from_ymd_opt(year + 1, 1, 1)?
                } else {
                    NaiveDate::from_ymd_opt(year, m + 1, 1)?
                };
                period(start, next)
            }
            (Some(m), Some(d)) => {
                let date = NaiveDate::from_ymd_opt(year, m, d)?;
                period(date, date.succ_opt()?)
            }
        }
    })
    .filter(|(from, to)| from <= to && today.year() > 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn now() -> i64 {
        Local.with_ymd_and_hms(2026, 9, 19, 12, 0, 0).unwrap().timestamp()
    }

    fn q(text: &str) -> Query {
        parse(text, now()).unwrap()
    }

    #[test]
    fn words_globs_paths_and_negation() {
        let query = q("Qwen *.GGUF -q8 models/7b \"my file\"");
        assert_eq!(
            query.terms,
            vec![
                Term::Word { needle: "qwen".into(), negate: false },
                Term::Glob { pattern: "*.gguf".chars().collect(), negate: false },
                Term::Word { needle: "q8".into(), negate: true },
                Term::Path { needle: "models\\7b".into(), negate: false },
                Term::Word { needle: "my file".into(), negate: false },
            ]
        );
        assert!(q("   ").is_empty());
        assert!(q("C:\\Users").terms.len() == 1, "путь с диском — не фильтр");
    }

    #[test]
    fn filters() {
        let query = q("ext:PNG,.jpg size:>100mb type:файл in:\"K:\\My Models\" hidden:да");
        assert_eq!(query.exts, ["png", "jpg"]);
        assert_eq!(query.size, Some((100 << 20, u64::MAX)));
        assert_eq!(query.dirs, Some(false));
        assert_eq!(query.within, Some(PathBuf::from("K:\\My Models")));
        assert!(query.hidden);
        assert_eq!(q("size:10kb..1mb").size, Some((10 << 10, 1 << 20)));
        assert_eq!(q("size:<1,5кб").size, Some((0, 1536)));
        assert!(parse("size:много", now()).is_err());
        assert!(parse("type:кот", now()).is_err());
        assert_eq!(q("folder:").dirs, Some(true));
    }

    #[test]
    fn dates() {
        let day = |y, m, d| Local.with_ymd_and_hms(y, m, d, 0, 0, 0).unwrap().timestamp();
        assert_eq!(q("dm:2026").modified, Some((day(2026, 1, 1), day(2027, 1, 1) - 1)));
        assert_eq!(q("dm:2026-09").modified, Some((day(2026, 9, 1), day(2026, 10, 1) - 1)));
        assert_eq!(q("dm:19.09.2026").modified, Some((day(2026, 9, 19), day(2026, 9, 20) - 1)));
        assert_eq!(q("dm:>2026-09-01").modified, Some((day(2026, 9, 1), i64::MAX)));
        assert_eq!(q("dm:сегодня").modified, Some((day(2026, 9, 19), day(2026, 9, 20) - 1)));
        assert_eq!(q("dm:вчера").modified, Some((day(2026, 9, 18), day(2026, 9, 19) - 1)));
        assert_eq!(q("dm:неделя").modified, Some((day(2026, 9, 13), now())));
        assert_eq!(q("dm:2026-01..2026-03").modified, Some((day(2026, 1, 1), day(2026, 4, 1) - 1)));
        assert!(parse("dm:потом", now()).is_err());
    }
}
