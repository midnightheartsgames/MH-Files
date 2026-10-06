//! Сортировка: натуральная по имени (`file2` раньше `file10`), папки первыми.

use std::cmp::Ordering;

use serde::{Deserialize, Serialize};

use crate::entry::Entry;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
pub enum SortColumn {
    #[default]
    Name,
    Modified,
    Type,
    Size,
    Created,
    /// Порядок выдачи поиска по дискам: лучшие совпадения первыми.
    Relevance,
}

impl SortColumn {
    pub fn title(self) -> &'static str {
        match self {
            SortColumn::Name => "Имя",
            SortColumn::Modified => "Изменён",
            SortColumn::Type => "Тип",
            SortColumn::Size => "Размер",
            SortColumn::Created => "Создан",
            SortColumn::Relevance => "Совпадение",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct SortOrder {
    pub column: SortColumn,
    pub descending: bool,
}

impl Default for SortOrder {
    fn default() -> SortOrder {
        SortOrder { column: SortColumn::Name, descending: false }
    }
}

impl SortOrder {
    /// Щелчок по заголовку столбца: тот же столбец — обратный порядок, другой — с начала.
    /// Даты и размеры по умолчанию от больших к меньшим, как в Проводнике.
    pub fn toggled(self, column: SortColumn) -> SortOrder {
        if self.column == column {
            SortOrder { column, descending: !self.descending }
        } else {
            let descending =
                matches!(column, SortColumn::Modified | SortColumn::Created | SortColumn::Size);
            SortOrder { column, descending }
        }
    }
}

/// Сравнение двух записей. Папки первыми при `folders_first` — независимо от направления.
pub fn compare(a: &Entry, b: &Entry, order: SortOrder, folders_first: bool) -> Ordering {
    if order.column == SortColumn::Relevance {
        // Порядок уже задан выдачей; сортировка устойчивая и его не меняет.
        return Ordering::Equal;
    }

    if folders_first && a.is_dir() != b.is_dir() {
        return if a.is_dir() { Ordering::Less } else { Ordering::Greater };
    }
    let primary = match order.column {
        SortColumn::Name => Ordering::Equal,
        SortColumn::Modified => a.modified.cmp(&b.modified),
        SortColumn::Created => a.created.cmp(&b.created),
        SortColumn::Size => a.size.cmp(&b.size),
        SortColumn::Type => a.extension().cmp(&b.extension()),
        SortColumn::Relevance => Ordering::Equal,
    };
    let ordering = primary.then_with(|| natural_cmp(&a.name, &b.name));
    if order.descending { ordering.reverse() } else { ordering }
}

/// Натуральное сравнение без учёта регистра: числа сравниваются как числа.
pub fn natural_cmp(a: &str, b: &str) -> Ordering {
    let mut left = a.chars().peekable();
    let mut right = b.chars().peekable();
    loop {
        match (left.peek().copied(), right.peek().copied()) {
            (None, None) => break,
            (None, Some(_)) => return Ordering::Less,
            (Some(_), None) => return Ordering::Greater,
            (Some(x), Some(y)) if x.is_ascii_digit() && y.is_ascii_digit() => {
                let take = |chars: &mut std::iter::Peekable<std::str::Chars>| {
                    let mut digits = String::new();
                    while let Some(&c) = chars.peek() {
                        if !c.is_ascii_digit() {
                            break;
                        }
                        digits.push(c);
                        chars.next();
                    }
                    digits
                };
                let (dx, dy) = (take(&mut left), take(&mut right));
                let (tx, ty) = (dx.trim_start_matches('0'), dy.trim_start_matches('0'));
                let ordering = tx.len().cmp(&ty.len()).then_with(|| tx.cmp(ty));
                if ordering != Ordering::Equal {
                    return ordering;
                }
            }
            (Some(x), Some(y)) => {
                let ordering = fold(x).cmp(&fold(y));
                if ordering != Ordering::Equal {
                    return ordering;
                }
                left.next();
                right.next();
            }
        }
    }
    // Равны без регистра — порядок всё равно должен быть полным и устойчивым.
    a.cmp(b)
}

fn fold(c: char) -> char {
    if c.is_ascii() { c.to_ascii_lowercase() } else { c.to_lowercase().next().unwrap_or(c) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::entry::{EntryKind, test_entry};

    #[test]
    fn natural_order() {
        let mut names = vec!["file10", "File2", "file1", "file02b", "a", "Б", "б2", "z"];
        names.sort_by(|a, b| natural_cmp(a, b));
        assert_eq!(names, vec!["a", "file1", "File2", "file02b", "file10", "z", "Б", "б2"]);
        assert_eq!(natural_cmp("x", "X"), "x".cmp("X"));
    }

    #[test]
    fn folders_first_regardless_of_direction() {
        let dir = test_entry("zz", EntryKind::Dir, 0);
        let file = test_entry("aa", EntryKind::File, 10);
        let order = SortOrder { column: SortColumn::Name, descending: true };
        assert_eq!(compare(&dir, &file, order, true), Ordering::Less);
        assert_eq!(compare(&dir, &file, order, false), Ordering::Less, "zz > aa, по убыванию");
        let ascending = SortOrder { descending: false, ..order };
        assert_eq!(compare(&dir, &file, ascending, false), Ordering::Greater);
    }

    #[test]
    fn size_sort_falls_back_to_name() {
        let a = test_entry("a", EntryKind::File, 5);
        let b = test_entry("b", EntryKind::File, 5);
        let c = test_entry("c", EntryKind::File, 9);
        let order = SortOrder { column: SortColumn::Size, descending: false };
        let mut items = [c.clone(), b.clone(), a.clone()];
        items.sort_by(|x, y| compare(x, y, order, true));
        assert_eq!(items.map(|e| e.name), ["a", "b", "c"]);
        assert!(SortOrder::default().toggled(SortColumn::Size).descending);
    }
}
