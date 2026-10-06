//! Выделение во вкладке. Хранится путями: пересортировка, фильтр и правки наблюдателя его не
//! сбивают. Курсор — строка с фокусом клавиатуры; якорь — начало диапазона для Shift.

use std::collections::HashSet;
use std::path::PathBuf;

use crate::listing::Listing;

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Selection {
    selected: HashSet<PathBuf>,
    cursor: Option<PathBuf>,
    anchor: Option<PathBuf>,
}

/// Модификаторы щелчка или клавиши.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Modifiers {
    pub ctrl: bool,
    pub shift: bool,
}

impl Selection {
    pub fn clear(&mut self) {
        self.selected.clear();
    }

    /// Полный сброс: при переходе в другую папку.
    pub fn reset(&mut self) {
        *self = Selection::default();
    }

    pub fn is_selected(&self, path: &std::path::Path) -> bool {
        self.selected.contains(path)
    }

    pub fn len(&self) -> usize {
        self.selected.len()
    }

    pub fn is_empty(&self) -> bool {
        self.selected.is_empty()
    }

    pub fn cursor(&self) -> Option<&PathBuf> {
        self.cursor.as_ref()
    }

    pub fn cursor_row(&self, listing: &Listing) -> Option<usize> {
        self.cursor.as_deref().and_then(|path| listing.row_of(path))
    }

    /// Выделить один объект и поставить на него курсор.
    pub fn select_only(&mut self, path: PathBuf) {
        self.selected.clear();
        self.selected.insert(path.clone());
        self.anchor = Some(path.clone());
        self.cursor = Some(path);
    }

    /// Поставить курсор, не трогая выделение (например, на только что созданную папку —
    /// а потом выделить её отдельно).
    pub fn set_cursor(&mut self, path: Option<PathBuf>) {
        self.cursor = path;
    }

    /// Щелчок мышью по строке `row`.
    pub fn click(&mut self, listing: &Listing, row: usize, modifiers: Modifiers) {
        let Some(entry) = listing.get(row) else { return };
        let path = entry.path();
        match (modifiers.ctrl, modifiers.shift) {
            (_, true) => {
                let anchor_row =
                    self.anchor.as_deref().and_then(|a| listing.row_of(a)).unwrap_or(row);
                if !modifiers.ctrl {
                    self.selected.clear();
                }
                self.add_range(listing, anchor_row, row);
                self.cursor = Some(path);
            }
            (true, false) => {
                if !self.selected.remove(&path) {
                    self.selected.insert(path.clone());
                }
                self.anchor = Some(path.clone());
                self.cursor = Some(path);
            }
            (false, false) => self.select_only(path),
        }
    }

    /// Курсор на строку `row` с клавиатуры: Shift расширяет от якоря, Ctrl только двигает.
    pub fn move_to(&mut self, listing: &Listing, row: usize, modifiers: Modifiers) {
        let Some(entry) = listing.get(row) else { return };
        let path = entry.path();
        if modifiers.shift {
            let anchor_row = self.anchor.as_deref().and_then(|a| listing.row_of(a)).unwrap_or(row);
            self.selected.clear();
            self.add_range(listing, anchor_row, row);
            self.cursor = Some(path);
        } else if modifiers.ctrl {
            self.cursor = Some(path);
        } else {
            self.select_only(path);
        }
    }

    /// Ctrl+Space: переключить строку под курсором.
    pub fn toggle_cursor(&mut self) {
        if let Some(path) = self.cursor.clone()
            && !self.selected.remove(&path)
        {
            self.selected.insert(path);
        }
    }

    pub fn select_all(&mut self, listing: &Listing) {
        self.selected = listing.iter().map(|entry| entry.path()).collect();
    }

    pub fn invert(&mut self, listing: &Listing) {
        self.selected = listing
            .iter()
            .map(|entry| entry.path())
            .filter(|path| !self.selected.contains(path))
            .collect();
    }

    /// После перечитывания: убрать то, чего больше нет или что скрыто фильтром.
    pub fn retain_visible(&mut self, listing: &Listing) {
        if self.selected.is_empty() && self.cursor.is_none() {
            return;
        }
        let visible: HashSet<PathBuf> = listing.iter().map(|entry| entry.path()).collect();
        self.selected.retain(|path| visible.contains(path));
        if self.cursor.as_ref().is_some_and(|c| !visible.contains(c)) {
            self.cursor = None;
        }
    }

    /// Выделенные пути в порядке показа. Пусто, но есть курсор — объект под курсором
    /// (так F2 и Delete работают сразу после перехода стрелками).
    pub fn targets(&self, listing: &Listing) -> Vec<PathBuf> {
        if self.selected.is_empty() {
            return self.cursor.iter().cloned().collect();
        }
        listing.iter().map(|entry| entry.path()).filter(|p| self.selected.contains(p)).collect()
    }

    /// Только выделенные пути в порядке показа, без подстановки курсора.
    pub fn selected_paths(&self, listing: &Listing) -> Vec<PathBuf> {
        listing.iter().map(|entry| entry.path()).filter(|p| self.selected.contains(p)).collect()
    }

    fn add_range(&mut self, listing: &Listing, a: usize, b: usize) {
        let (from, to) = if a <= b { (a, b) } else { (b, a) };
        for row in from..=to {
            if let Some(entry) = listing.get(row) {
                self.selected.insert(entry.path());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::entry::{EntryKind, test_entry};
    use crate::listing::ViewOptions;

    fn listing() -> Listing {
        let mut l = Listing::new(ViewOptions::default());
        l.extend(["a", "b", "c", "d", "e"].map(|n| test_entry(n, EntryKind::File, 1)));
        l.finish(Ok(()));
        l
    }

    fn names(selection: &Selection, l: &Listing) -> Vec<String> {
        selection
            .targets(l)
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
            .collect()
    }

    #[test]
    fn click_ctrl_shift() {
        let l = listing();
        let mut s = Selection::default();
        s.click(&l, 1, Modifiers::default());
        s.click(&l, 3, Modifiers { shift: true, ctrl: false });
        assert_eq!(names(&s, &l), ["b", "c", "d"]);
        s.click(&l, 2, Modifiers { ctrl: true, shift: false });
        assert_eq!(names(&s, &l), ["b", "d"]);
        // Ctrl+Shift добавляет диапазон от нового якоря, не сбрасывая выделенное.
        s.click(&l, 4, Modifiers { ctrl: true, shift: true });
        assert_eq!(names(&s, &l), ["b", "c", "d", "e"]);
    }

    #[test]
    fn keyboard_extend_and_cursor_fallback() {
        let l = listing();
        let mut s = Selection::default();
        s.move_to(&l, 0, Modifiers::default());
        s.move_to(&l, 2, Modifiers { shift: true, ctrl: false });
        assert_eq!(names(&s, &l), ["a", "b", "c"]);
        s.move_to(&l, 1, Modifiers { shift: true, ctrl: false });
        assert_eq!(names(&s, &l), ["a", "b"]);
        s.clear();
        assert_eq!(names(&s, &l), ["b"], "без выделения цель — курсор");
        s.invert(&l);
        assert_eq!(s.len(), 5);
    }

    #[test]
    fn survives_filtering() {
        let mut l = listing();
        let mut s = Selection::default();
        s.select_all(&l);
        l.set_filter("a");
        s.retain_visible(&l);
        assert_eq!(names(&s, &l), ["a"]);
    }
}
