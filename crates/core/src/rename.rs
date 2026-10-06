//! Пакетное переименование: правила → предпросмотр → проверка → план из двух проходов.
//!
//! Правила применяются по очереди к паре «имя без расширения + расширение». Переменные в
//! тексте: `{name}` `{ext}` `{n}` `{n:3}` `{date}` `{modified}` `{created}` `{parent}` `{uuid}`.
//!
//! До применения проверяется всё: запрещённые символы и имена Windows, повторы среди новых
//! имён, столкновения с файлами, которые не переименовываются, длина пути. Применение идёт
//! через временные имена, поэтому обмен `A → B`, `B → A` работает.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use chrono::{DateTime, Local};
use regex::RegexBuilder;
use serde::{Deserialize, Serialize};

use crate::entry::{extension_of, stem_of};
use crate::names;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum CaseMode {
    Lower,
    Upper,
    /// Каждое Слово С Большой.
    Title,
    /// Первая буква большая, остальные как есть.
    Sentence,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum InsertAt {
    Prefix,
    Suffix,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ExtensionRule {
    Lower,
    Upper,
    Set(String),
    Remove,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Rule {
    /// Найти и заменить (в замене для регулярки работают `$1`, `${name}`).
    Replace {
        find: String,
        replace: String,
        regex: bool,
        case_sensitive: bool,
    },
    /// Добавить текст в начало или конец имени.
    Insert {
        text: String,
        at: InsertAt,
    },
    /// Имя целиком по шаблону, например `trip_{n:3}`.
    Template {
        pattern: String,
    },
    Case {
        mode: CaseMode,
    },
    Extension {
        rule: ExtensionRule,
    },
    /// Убрать `count` символов начиная с `from` (с конца, если `from_end`).
    RemoveRange {
        from: usize,
        count: usize,
        from_end: bool,
    },
    /// Убрать пробелы по краям и сжать повторяющиеся.
    Trim,
}

impl Rule {
    pub fn title(&self) -> &'static str {
        match self {
            Rule::Replace { regex: true, .. } => "Регулярное выражение",
            Rule::Replace { .. } => "Найти и заменить",
            Rule::Insert { at: InsertAt::Prefix, .. } => "Префикс",
            Rule::Insert { at: InsertAt::Suffix, .. } => "Суффикс",
            Rule::Template { .. } => "Шаблон имени",
            Rule::Case { .. } => "Регистр",
            Rule::Extension { .. } => "Расширение",
            Rule::RemoveRange { .. } => "Удалить символы",
            Rule::Trim => "Убрать лишние пробелы",
        }
    }
}

/// Счётчик `{n}`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Numbering {
    pub start: u64,
    pub step: u64,
    /// Минимум цифр для `{n}` без явной ширины.
    pub width: usize,
}

impl Default for Numbering {
    fn default() -> Numbering {
        Numbering { start: 1, step: 1, width: 1 }
    }
}

/// Что известно о переименовываемом объекте.
#[derive(Debug, Clone, PartialEq)]
pub struct Item {
    pub path: PathBuf,
    pub is_dir: bool,
    pub modified: Option<SystemTime>,
    pub created: Option<SystemTime>,
}

impl Item {
    pub fn name(&self) -> String {
        self.path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
    }
}

/// Новое имя для каждого объекта по порядку. Ошибка правила (плохая регулярка) — у всех.
pub fn preview(
    items: &[Item],
    rules: &[Rule],
    numbering: Numbering,
) -> Result<Vec<String>, String> {
    let today = Local::now().format("%Y-%m-%d").to_string();
    let mut compiled = Vec::with_capacity(rules.len());
    for rule in rules {
        compiled.push(Compiled::new(rule)?);
    }
    Ok(items
        .iter()
        .enumerate()
        .map(|(index, item)| {
            let name = item.name();
            let mut stem = stem_of(&name, item.is_dir).to_string();
            let mut ext = if item.is_dir { String::new() } else { original_ext(&name) };
            let counter = numbering.start + numbering.step * index as u64;
            let vars = Vars { item, original: &name, counter, numbering, today: &today };
            for rule in &compiled {
                rule.apply(&mut stem, &mut ext, &vars);
            }
            if ext.is_empty() { stem } else { format!("{stem}.{ext}") }
        })
        .collect())
}

/// Расширение с исходным регистром.
fn original_ext(name: &str) -> String {
    let lower = extension_of(name);
    if lower.is_empty() { lower } else { name[name.len() - lower.len()..].to_string() }
}

struct Vars<'a> {
    item: &'a Item,
    original: &'a str,
    counter: u64,
    numbering: Numbering,
    today: &'a str,
}

impl Vars<'_> {
    fn expand(&self, text: &str) -> String {
        let mut result = String::with_capacity(text.len());
        let mut rest = text;
        while let Some(open) = rest.find('{') {
            result.push_str(&rest[..open]);
            let after = &rest[open + 1..];
            let Some(close) = after.find('}') else {
                result.push_str(&rest[open..]);
                return result;
            };
            let token = &after[..close];
            match self.value(token) {
                Some(value) => result.push_str(&value),
                None => {
                    result.push('{');
                    result.push_str(token);
                    result.push('}');
                }
            }
            rest = &after[close + 1..];
        }
        result.push_str(rest);
        result
    }

    fn value(&self, token: &str) -> Option<String> {
        let date = |time: Option<SystemTime>| {
            time.map(|t| DateTime::<Local>::from(t).format("%Y-%m-%d").to_string())
                .unwrap_or_default()
        };
        Some(match token {
            "name" => stem_of(self.original, self.item.is_dir).to_string(),
            "ext" => {
                if self.item.is_dir {
                    String::new()
                } else {
                    original_ext(self.original)
                }
            }
            "n" => format!("{:0width$}", self.counter, width = self.numbering.width),
            "date" => self.today.to_string(),
            "modified" => date(self.item.modified),
            "created" => date(self.item.created),
            "parent" => self
                .item
                .path
                .parent()
                .and_then(Path::file_name)
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default(),
            "uuid" => uuid::Uuid::new_v4().to_string(),
            _ => {
                let width: usize = token.strip_prefix("n:")?.parse().ok()?;
                format!("{:0width$}", self.counter, width = width.min(12))
            }
        })
    }
}

enum Compiled<'a> {
    Regex(regex::Regex, &'a str),
    Other(&'a Rule),
}

impl<'a> Compiled<'a> {
    fn new(rule: &'a Rule) -> Result<Compiled<'a>, String> {
        match rule {
            Rule::Replace { find, replace, regex, case_sensitive } if !find.is_empty() => {
                let source = if *regex { find.clone() } else { regex::escape(find) };
                let compiled = RegexBuilder::new(&source)
                    .case_insensitive(!case_sensitive)
                    .build()
                    .map_err(|error| format!("регулярное выражение: {error}"))?;
                Ok(Compiled::Regex(compiled, replace))
            }
            other => Ok(Compiled::Other(other)),
        }
    }

    fn apply(&self, stem: &mut String, ext: &mut String, vars: &Vars) {
        match self {
            Compiled::Regex(regex, replace) => {
                *stem = regex.replace_all(stem, *replace).into_owned();
            }
            Compiled::Other(rule) => match rule {
                Rule::Replace { .. } => {}
                Rule::Insert { text, at } => {
                    let text = vars.expand(text);
                    match at {
                        InsertAt::Prefix => stem.insert_str(0, &text),
                        InsertAt::Suffix => stem.push_str(&text),
                    }
                }
                Rule::Template { pattern } => {
                    if !pattern.trim().is_empty() {
                        *stem = vars.expand(pattern);
                    }
                }
                Rule::Case { mode } => *stem = change_case(stem, *mode),
                Rule::Extension { rule } => match rule {
                    ExtensionRule::Lower => *ext = ext.to_lowercase(),
                    ExtensionRule::Upper => *ext = ext.to_uppercase(),
                    ExtensionRule::Set(new) => *ext = new.trim().trim_start_matches('.').into(),
                    ExtensionRule::Remove => ext.clear(),
                },
                Rule::RemoveRange { from, count, from_end } => {
                    let chars: Vec<char> = stem.chars().collect();
                    let len = chars.len();
                    let start = if *from_end { len.saturating_sub(from + count) } else { *from };
                    let end = if *from_end {
                        len.saturating_sub(*from)
                    } else {
                        from.saturating_add(*count)
                    };
                    let (start, end) = (start.min(len), end.min(len));
                    *stem = chars[..start].iter().chain(&chars[end..]).collect();
                }
                Rule::Trim => {
                    *stem = stem.split_whitespace().collect::<Vec<_>>().join(" ");
                }
            },
        }
    }
}

fn change_case(text: &str, mode: CaseMode) -> String {
    match mode {
        CaseMode::Lower => text.to_lowercase(),
        CaseMode::Upper => text.to_uppercase(),
        CaseMode::Sentence => {
            let mut chars = text.chars();
            match chars.next() {
                Some(first) => first.to_uppercase().chain(chars).collect(),
                None => String::new(),
            }
        }
        CaseMode::Title => {
            let mut result = String::with_capacity(text.len());
            let mut boundary = true;
            for c in text.chars() {
                if boundary {
                    result.extend(c.to_uppercase());
                } else {
                    result.extend(c.to_lowercase());
                }
                boundary = !c.is_alphanumeric();
            }
            result
        }
    }
}

/// Итог проверки одной строки.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Status {
    Ok,
    /// Имя не меняется — объект пропускается.
    Unchanged,
    /// Применять можно, но стоит знать (длинный путь).
    Warning(String),
    Error(String),
}

impl Status {
    pub fn is_error(&self) -> bool {
        matches!(self, Status::Error(_))
    }
}

/// Предел, после которого многие программы не откроют файл (MAX_PATH без завершающего нуля).
pub const LEGACY_MAX_PATH: usize = 259;

/// Проверяет новые имена. `siblings` — имена всех объектов в папках переименования
/// (по папкам), чтобы найти столкновения с теми, что не переименовываются.
pub fn validate(
    items: &[Item],
    new_names: &[String],
    siblings: &HashMap<PathBuf, Vec<String>>,
) -> Vec<Status> {
    let key = |dir: &Path, name: &str| (dir.to_path_buf(), name.to_lowercase());
    // Старые имена переименовываемых: они освободятся, столкновением не считаются.
    let leaving: HashSet<(PathBuf, String)> =
        items.iter().filter_map(|item| Some(key(item.path.parent()?, &item.name()))).collect();
    let mut counts: HashMap<(PathBuf, String), usize> = HashMap::new();
    for (item, new) in items.iter().zip(new_names) {
        if let Some(dir) = item.path.parent() {
            *counts.entry(key(dir, new)).or_default() += 1;
        }
    }
    items
        .iter()
        .zip(new_names)
        .map(|(item, new)| {
            let Some(dir) = item.path.parent() else {
                return Status::Error("нет папки".into());
            };
            if *new == item.name() {
                return Status::Unchanged;
            }
            if let Err(error) = names::validate(new) {
                return Status::Error(error.to_string());
            }
            if counts.get(&key(dir, new)).copied().unwrap_or(0) > 1 {
                return Status::Error("такое же новое имя у другого объекта".into());
            }
            let lower = new.to_lowercase();
            let taken = siblings
                .get(dir)
                .is_some_and(|names| names.iter().any(|n| n.to_lowercase() == lower));
            let same_object = lower == item.name().to_lowercase();
            if taken && !same_object && !leaving.contains(&key(dir, new)) {
                return Status::Error("в папке уже есть объект с таким именем".into());
            }
            let length = dir.join(new).to_string_lossy().encode_utf16().count();
            if length > LEGACY_MAX_PATH {
                return Status::Warning(format!(
                    "путь {length} символов — не все программы его откроют"
                ));
            }
            Status::Ok
        })
        .collect()
}

/// Что переименовать: только строки без ошибок и с изменённым именем. Если хоть одна строка
/// с ошибкой — `Err`: частичного применения нет.
pub fn plan(items: &[Item], new_names: &[String], statuses: &[Status]) -> Result<Plan, String> {
    if let Some(index) = statuses.iter().position(Status::is_error) {
        return Err(format!("ошибка в строке {}: {}", index + 1, items[index].name()));
    }
    let moves = items
        .iter()
        .zip(new_names)
        .zip(statuses)
        .filter(|(_, status)| !matches!(status, Status::Unchanged))
        .map(|((item, new), _)| (item.path.clone(), item.path.with_file_name(new)))
        .collect();
    Ok(Plan { moves })
}

/// Переименования «откуда → куда» одного прохода.
pub type Moves = Vec<(PathBuf, PathBuf)>;

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Plan {
    /// Откуда → куда, полные пути.
    pub moves: Moves,
}

impl Plan {
    pub fn is_empty(&self) -> bool {
        self.moves.is_empty()
    }

    /// Два прохода: всё во временные имена, потом из них в новые. `token` делает временные
    /// имена уникальными для этого запуска.
    pub fn passes(&self, token: &str) -> (Moves, Moves) {
        let temp: Vec<PathBuf> = self
            .moves
            .iter()
            .enumerate()
            .map(|(i, (from, _))| from.with_file_name(format!(".mh-rename-{token}-{i}")))
            .collect();
        let first = self.moves.iter().zip(&temp).map(|((from, _), t)| (from.clone(), t.clone()));
        let second = self.moves.iter().zip(&temp).map(|((_, to), t)| (t.clone(), to.clone()));
        (first.collect(), second.collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(path: &str) -> Item {
        Item { path: PathBuf::from(path), is_dir: false, modified: None, created: None }
    }

    fn names(items: &[Item], rules: &[Rule]) -> Vec<String> {
        preview(items, rules, Numbering::default()).unwrap()
    }

    #[test]
    fn template_with_counter_keeps_extension() {
        let items = [item("/p/IMG_8421.png"), item("/p/IMG_8422.PNG"), item("/p/IMG_8423.png")];
        let rules = [Rule::Template { pattern: "trip_{n:3}".into() }];
        assert_eq!(names(&items, &rules), ["trip_001.png", "trip_002.PNG", "trip_003.png"]);
    }

    #[test]
    fn replace_regex_case_and_extension() {
        let items = [item("/p/My Photo (1).JPG")];
        let rules = [
            Rule::Replace {
                find: r"\s*\((\d+)\)".into(),
                replace: "_$1".into(),
                regex: true,
                case_sensitive: false,
            },
            Rule::Case { mode: CaseMode::Lower },
            Rule::Extension { rule: ExtensionRule::Lower },
            Rule::Insert { text: "{parent}-".into(), at: InsertAt::Prefix },
        ];
        assert_eq!(names(&items, &rules), ["p-my photo_1.jpg"]);
        let plain = [Rule::Replace {
            find: "photo".into(),
            replace: "Фото".into(),
            regex: false,
            case_sensitive: false,
        }];
        assert_eq!(names(&items, &plain), ["My Фото (1).JPG"]);
        let bad = [Rule::Replace {
            find: "(".into(),
            replace: "".into(),
            regex: true,
            case_sensitive: true,
        }];
        assert!(preview(&items, &bad, Numbering::default()).is_err());
    }

    #[test]
    fn title_case_remove_range_and_trim() {
        let items = [item("/p/hello  wORLD-again.txt")];
        assert_eq!(
            names(&items, &[Rule::Case { mode: CaseMode::Title }]),
            ["Hello  World-Again.txt"]
        );
        assert_eq!(names(&items, &[Rule::Trim]), ["hello wORLD-again.txt"]);
        let remove = Rule::RemoveRange { from: 0, count: 7, from_end: false };
        assert_eq!(names(&items, &[remove]), ["wORLD-again.txt"]);
        let remove_end = Rule::RemoveRange { from: 0, count: 6, from_end: true };
        assert_eq!(names(&items, &[remove_end]), ["hello  wORLD.txt"]);
        let too_far = Rule::RemoveRange { from: 50, count: 6, from_end: false };
        assert_eq!(names(&items, &[too_far]), ["hello  wORLD-again.txt"]);
    }

    #[test]
    fn validation_catches_problems_but_allows_swaps() {
        let items = [item("/p/a.txt"), item("/p/b.txt"), item("/p/c.txt"), item("/p/d.txt")];
        let new: Vec<String> =
            ["b.txt", "a.txt", "x?.txt", "keep.txt"].iter().map(|s| s.to_string()).collect();
        let mut siblings = HashMap::new();
        siblings.insert(
            PathBuf::from("/p"),
            ["a.txt", "b.txt", "c.txt", "d.txt", "KEEP.txt"]
                .iter()
                .map(|s| s.to_string())
                .collect(),
        );
        let statuses = validate(&items, &new, &siblings);
        assert_eq!(statuses[0], Status::Ok, "обмен a↔b разрешён");
        assert_eq!(statuses[1], Status::Ok);
        assert!(statuses[2].is_error(), "запрещённый символ");
        assert!(statuses[3].is_error(), "столкновение без учёта регистра");
        assert!(plan(&items, &new, &statuses).is_err());

        let dup: Vec<String> =
            ["z.txt", "Z.TXT", "c.txt", "d.txt"].iter().map(|s| s.to_string()).collect();
        let statuses = validate(&items, &dup, &siblings);
        assert!(statuses[0].is_error() && statuses[1].is_error());
        assert_eq!(statuses[2], Status::Unchanged);

        let case_only: Vec<String> =
            ["A.txt", "b.txt", "c.txt", "d.txt"].iter().map(|s| s.to_string()).collect();
        let statuses = validate(&items, &case_only, &siblings);
        assert_eq!(statuses[0], Status::Ok, "смена регистра своего имени — не столкновение");
    }

    #[test]
    fn two_passes_through_temporary_names() {
        let items = [item("/p/a"), item("/p/b")];
        let new = vec!["b".to_string(), "a".to_string()];
        let statuses = validate(&items, &new, &HashMap::new());
        let plan = plan(&items, &new, &statuses).unwrap();
        let (first, second) = plan.passes("t");
        assert_eq!(first[0], (PathBuf::from("/p/a"), PathBuf::from("/p/.mh-rename-t-0")));
        assert_eq!(second[1], (PathBuf::from("/p/.mh-rename-t-1"), PathBuf::from("/p/a")));
    }
}
