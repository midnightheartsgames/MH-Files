//! История переходов вкладки: назад, вперёд.

use serde::{Deserialize, Serialize};

use crate::location::Location;

/// Сколько шагов помнить в каждую сторону.
const LIMIT: usize = 64;

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct History {
    back: Vec<Location>,
    forward: Vec<Location>,
}

impl History {
    /// Переход из `from` куда-то новое: «вперёд» больше нет.
    pub fn visit(&mut self, from: Location) {
        if self.back.last() != Some(&from) {
            self.back.push(from);
        }
        if self.back.len() > LIMIT {
            self.back.remove(0);
        }
        self.forward.clear();
    }

    pub fn back(&mut self, current: Location) -> Option<Location> {
        let target = self.back.pop()?;
        self.forward.push(current);
        Some(target)
    }

    pub fn forward(&mut self, current: Location) -> Option<Location> {
        let target = self.forward.pop()?;
        self.back.push(current);
        Some(target)
    }

    pub fn can_back(&self) -> bool {
        !self.back.is_empty()
    }

    pub fn can_forward(&self) -> bool {
        !self.forward.is_empty()
    }

    /// Последние места «назад», от ближнего к дальнему — для выпадающего списка.
    pub fn back_list(&self) -> impl Iterator<Item = &Location> {
        self.back.iter().rev()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn dir(name: &str) -> Location {
        Location::Dir(PathBuf::from(name))
    }

    #[test]
    fn back_and_forward() {
        let mut h = History::default();
        h.visit(dir("/a"));
        h.visit(dir("/b"));
        assert_eq!(h.back(dir("/c")), Some(dir("/b")));
        assert_eq!(h.back(dir("/b")), Some(dir("/a")));
        assert!(!h.can_back());
        assert_eq!(h.forward(dir("/a")), Some(dir("/b")));
        h.visit(dir("/b"));
        assert!(!h.can_forward(), "новый переход стирает «вперёд»");
    }
}
