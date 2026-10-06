//! Проверка перед копированием и перемещением: какие имена уже заняты в папке назначения.
//! По ней MH Files показывает свой диалог конфликтов, а не системный.

use std::path::{Path, PathBuf};
use std::time::SystemTime;

/// Копирование или перемещение, которое ждёт решения по конфликтам.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Transfer {
    pub sources: Vec<PathBuf>,
    pub dest: PathBuf,
    pub copy: bool,
}

/// Что известно о каждой стороне конфликта — для сравнения в диалоге.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Side {
    pub is_dir: bool,
    pub size: u64,
    pub modified: Option<SystemTime>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Conflict {
    pub source: PathBuf,
    pub target: PathBuf,
    pub incoming: Side,
    pub existing: Side,
}

fn side(path: &Path) -> Option<Side> {
    let meta = std::fs::symlink_metadata(path).ok()?;
    Some(Side { is_dir: meta.is_dir(), size: meta.len(), modified: meta.modified().ok() })
}

/// Объекты, чьё имя уже занято в `dest`. Копия в ту же папку — не конфликт: она получит
/// имя «… - копия».
pub fn conflicts(transfer: &Transfer) -> Vec<Conflict> {
    transfer
        .sources
        .iter()
        .filter(|source| source.parent() != Some(transfer.dest.as_path()))
        .filter_map(|source| {
            let target = transfer.dest.join(source.file_name()?);
            let existing = side(&target)?;
            let incoming = side(source)?;
            Some(Conflict { source: source.clone(), target, incoming, existing })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_taken_names_only() {
        let dir = std::env::temp_dir().join(format!("mh-files-transfer-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("src")).unwrap();
        std::fs::create_dir_all(dir.join("dst")).unwrap();
        for name in ["a", "b"] {
            std::fs::write(dir.join("src").join(name), "new!").unwrap();
        }
        std::fs::write(dir.join("dst/a"), "o").unwrap();
        let transfer = Transfer {
            sources: vec![dir.join("src/a"), dir.join("src/b")],
            dest: dir.join("dst"),
            copy: true,
        };
        let found = conflicts(&transfer);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].target, dir.join("dst/a"));
        assert_eq!((found[0].incoming.size, found[0].existing.size), (4, 1));
        let same = Transfer { sources: vec![dir.join("src/a")], dest: dir.join("src"), copy: true };
        assert!(conflicts(&same).is_empty(), "копия рядом с собой — не конфликт");
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
