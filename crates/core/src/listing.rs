//! Содержимое открытой папки и то, как оно показано: фильтр, сортировка, скрытые файлы.
//!
//! Воркер приносит записи пачками; список сразу пригоден для показа и досортировывается по
//! мере загрузки. Наблюдатель за папкой правит отдельные записи — выделение и прокрутка при
//! этом остаются, потому что выделение хранится путями, а не номерами строк.

use std::cell::OnceCell;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::entry::Entry;
use crate::filter::Filter;
use crate::sort::{SortOrder, compare};

/// Как показывать список. Меняется настройками и щелчками по заголовкам.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ViewOptions {
    pub sort: SortOrder,
    pub folders_first: bool,
    pub show_hidden: bool,
    pub show_system: bool,
}

impl Default for ViewOptions {
    fn default() -> ViewOptions {
        ViewOptions {
            sort: SortOrder::default(),
            folders_first: true,
            show_hidden: false,
            show_system: false,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum LoadState {
    #[default]
    Loading,
    Done,
    Failed(String),
}

#[derive(Debug, Clone, Default)]
pub struct Listing {
    entries: Vec<Entry>,
    /// Номера записей в `entries`, прошедших фильтр, в порядке показа.
    view: Vec<usize>,
    /// Строка каждой записи в `view`; `usize::MAX` — скрыта.
    rows: Vec<usize>,
    /// Путь → номер записи. Строится лениво при первом поиске после изменения списка:
    /// выделение и курсор хранятся путями, и поиск строки должен быть O(1) даже на 100 000.
    index: OnceCell<HashMap<PathBuf, usize>>,
    options: ViewOptions,
    filter: Filter,
    pub state: LoadState,
}

/// Итоги для строки состояния.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Totals {
    pub dirs: usize,
    pub files: usize,
    pub bytes: u64,
}

impl Listing {
    pub fn new(options: ViewOptions) -> Listing {
        Listing { options, ..Listing::default() }
    }

    /// Начать заново: новая папка или перечитывание.
    pub fn reset(&mut self) {
        self.entries.clear();
        self.view.clear();
        self.rows.clear();
        self.index = OnceCell::new();
        self.state = LoadState::Loading;
    }

    /// Пачка записей от воркера. Сортировка — отдельно, через [`Listing::refresh`]: при
    /// потоковой загрузке её делают не на каждой пачке.
    pub fn extend(&mut self, batch: impl IntoIterator<Item = Entry>) {
        self.entries.extend(batch);
    }

    /// Заменяет все записи разом (перечитывание без мигания пустым списком).
    pub fn replace(&mut self, entries: Vec<Entry>) {
        self.entries = entries;
        self.refresh();
    }

    pub fn finish(&mut self, result: Result<(), String>) {
        self.state = match result {
            Ok(()) => LoadState::Done,
            Err(error) => LoadState::Failed(error),
        };
        self.refresh();
    }

    pub fn is_loading(&self) -> bool {
        self.state == LoadState::Loading
    }

    pub fn options(&self) -> ViewOptions {
        self.options
    }

    pub fn set_options(&mut self, options: ViewOptions) {
        if self.options != options {
            self.options = options;
            self.refresh();
        }
    }

    pub fn set_filter(&mut self, query: &str) {
        let filter = Filter::new(query);
        if self.filter != filter {
            self.filter = filter;
            self.refresh();
        }
    }

    pub fn has_filter(&self) -> bool {
        !self.filter.is_empty()
    }

    /// Пересчитывает видимые строки: фильтр, скрытые, сортировка.
    pub fn refresh(&mut self) {
        let options = self.options;
        let filter = &self.filter;
        let entries = &self.entries;
        self.view = (0..entries.len())
            .filter(|&i| {
                let entry = &entries[i];
                (options.show_hidden || !entry.hidden())
                    && (options.show_system || !(entry.attributes.system() && entry.hidden()))
                    && filter.matches(&entry.name)
            })
            .collect();
        self.view.sort_by(|&a, &b| {
            compare(&entries[a], &entries[b], options.sort, options.folders_first)
        });
        self.rows = vec![usize::MAX; self.entries.len()];
        for (row, &index) in self.view.iter().enumerate() {
            self.rows[index] = row;
        }
        self.index = OnceCell::new();
    }

    fn index(&self) -> &HashMap<PathBuf, usize> {
        self.index.get_or_init(|| {
            self.entries.iter().enumerate().map(|(i, entry)| (entry.path(), i)).collect()
        })
    }

    /// Видимых строк.
    pub fn len(&self) -> usize {
        self.view.len()
    }

    pub fn is_empty(&self) -> bool {
        self.view.is_empty()
    }

    /// Всего прочитано записей, включая скрытые и отфильтрованные.
    pub fn total_len(&self) -> usize {
        self.entries.len()
    }

    /// Запись в видимой строке `row`.
    pub fn get(&self, row: usize) -> Option<&Entry> {
        self.view.get(row).map(|&i| &self.entries[i])
    }

    pub fn iter(&self) -> impl Iterator<Item = &Entry> {
        self.view.iter().map(|&i| &self.entries[i])
    }

    /// Все прочитанные записи, без фильтра.
    pub fn all(&self) -> &[Entry] {
        &self.entries
    }

    /// Видимая строка записи с этим путём.
    pub fn row_of(&self, path: &Path) -> Option<usize> {
        let &index = self.index().get(path)?;
        self.rows.get(index).copied().filter(|&row| row != usize::MAX)
    }

    /// Запись с этим путём среди всех прочитанных, даже скрытая фильтром.
    pub fn find(&self, path: &Path) -> Option<&Entry> {
        self.index().get(path).map(|&index| &self.entries[index])
    }

    /// Первая видимая строка, чьё имя начинается с `prefix` (набор с клавиатуры), начиная
    /// с `from` по кругу.
    pub fn find_prefix(&self, prefix: &str, from: usize) -> Option<usize> {
        let prefix = prefix.to_lowercase();
        let n = self.view.len();
        (0..n).map(|k| (from + k) % n).find(|&row| {
            self.get(row).is_some_and(|entry| entry.name.to_lowercase().starts_with(&prefix))
        })
    }

    /// Добавляет или обновляет запись (событие наблюдателя). Строки пересчитывает
    /// [`Listing::refresh`].
    pub fn upsert(&mut self, entry: Entry) {
        match self.entries.iter_mut().find(|e| e.name == entry.name && e.parent == entry.parent) {
            Some(existing) => *existing = entry,
            None => self.entries.push(entry),
        }
        self.index = OnceCell::new();
    }

    /// Убирает запись по пути. `true`, если она была.
    pub fn remove(&mut self, path: &Path) -> bool {
        let (Some(name), Some(parent)) = (path.file_name().and_then(|n| n.to_str()), path.parent())
        else {
            return false;
        };
        let before = self.entries.len();
        self.entries.retain(|e| !(e.name == name && *e.parent == *parent));
        self.index = OnceCell::new();
        before != self.entries.len()
    }

    /// Посчитанный размер папки: попадает в столбец «Размер» и в сортировку по размеру.
    /// `true` — такая папка есть в списке (нужен [`Listing::refresh`]).
    pub fn set_dir_size(&mut self, path: &Path, bytes: u64) -> bool {
        let Some(&index) = self.index().get(path) else { return false };
        let entry = &mut self.entries[index];
        if !entry.is_dir() {
            return false;
        }
        entry.size = bytes;
        true
    }

    pub fn totals(&self) -> Totals {
        let mut totals = Totals::default();
        for entry in self.iter() {
            if entry.is_dir() {
                totals.dirs += 1;
            } else {
                totals.files += 1;
                totals.bytes += entry.size;
            }
        }
        totals
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::entry::{Attributes, EntryKind, test_entry};
    use crate::sort::SortColumn;

    fn listing(names: &[(&str, EntryKind)]) -> Listing {
        let mut listing = Listing::new(ViewOptions::default());
        listing.extend(names.iter().map(|&(name, kind)| test_entry(name, kind, 1)));
        listing.finish(Ok(()));
        listing
    }

    fn names(listing: &Listing) -> Vec<&str> {
        listing.iter().map(|e| e.name.as_str()).collect()
    }

    #[test]
    fn sorted_with_folders_first_and_filtered() {
        let mut l = listing(&[
            ("b.txt", EntryKind::File),
            ("src", EntryKind::Dir),
            ("a10.txt", EntryKind::File),
            ("a2.txt", EntryKind::File),
        ]);
        assert_eq!(names(&l), ["src", "a2.txt", "a10.txt", "b.txt"]);
        l.set_filter("a");
        assert_eq!(names(&l), ["a2.txt", "a10.txt"]);
        l.set_filter("");
        let mut options = l.options();
        options.sort = SortOrder { column: SortColumn::Name, descending: true };
        l.set_options(options);
        assert_eq!(names(&l), ["src", "b.txt", "a10.txt", "a2.txt"]);
        assert_eq!(l.totals(), Totals { dirs: 1, files: 3, bytes: 3 });
    }

    #[test]
    fn hidden_files_follow_the_option() {
        let mut l = Listing::new(ViewOptions::default());
        let mut hidden = test_entry("secret", EntryKind::File, 1);
        hidden.attributes = Attributes(Attributes::HIDDEN);
        l.extend([hidden, test_entry("open", EntryKind::File, 1)]);
        l.finish(Ok(()));
        assert_eq!(names(&l), ["open"]);
        l.set_options(ViewOptions { show_hidden: true, ..l.options() });
        assert_eq!(names(&l), ["open", "secret"]);
        assert_eq!(l.total_len(), 2);
    }

    #[test]
    fn watcher_patches_keep_order() {
        let mut l = listing(&[("a", EntryKind::File), ("c", EntryKind::File)]);
        l.upsert(test_entry("b", EntryKind::File, 7));
        l.upsert(test_entry("a", EntryKind::File, 9));
        assert!(l.remove(Path::new("/test/c")));
        assert!(!l.remove(Path::new("/test/zzz")));
        l.refresh();
        assert_eq!(names(&l), ["a", "b"]);
        assert_eq!(l.get(0).unwrap().size, 9);
        assert_eq!(l.row_of(Path::new("/test/b")), Some(1));
        assert_eq!(l.find_prefix("B", 0), Some(1));
        assert_eq!(l.find_prefix("a", 1), Some(0));
    }

    #[test]
    fn folder_sizes_sort_by_size() {
        let mut l =
            listing(&[("small", EntryKind::Dir), ("big", EntryKind::Dir), ("f", EntryKind::File)]);
        assert!(l.set_dir_size(Path::new("/test/big"), 500));
        assert!(!l.set_dir_size(Path::new("/test/f"), 500), "только папки");
        let options =
            ViewOptions { sort: SortOrder::default().toggled(SortColumn::Size), ..l.options() };
        l.set_options(options);
        assert_eq!(names(&l), ["big", "small", "f"]);
    }
}
