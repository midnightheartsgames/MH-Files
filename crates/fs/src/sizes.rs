//! Размер папки целиком: обход без перехода по ссылкам, с отменой.

use std::path::Path;

use crate::CancelToken;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct DirSize {
    pub bytes: u64,
    pub files: u64,
    pub dirs: u64,
    /// Часть папок прочитать не удалось (нет доступа) — числа неполные.
    pub partial: bool,
}

/// Сумма по дереву. `None` — отменено.
pub fn dir_size(root: &Path, cancel: &CancelToken) -> Option<DirSize> {
    let mut total = DirSize::default();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        if cancel.is_cancelled() {
            return None;
        }
        let Ok(entries) = std::fs::read_dir(&dir) else {
            total.partial = true;
            continue;
        };
        for entry in entries.flatten() {
            let Ok(kind) = entry.file_type() else {
                total.partial = true;
                continue;
            };
            if kind.is_symlink() {
                continue;
            }
            if kind.is_dir() {
                total.dirs += 1;
                stack.push(entry.path());
            } else {
                total.files += 1;
                total.bytes += entry.metadata().map_or(0, |m| m.len());
            }
        }
    }
    Some(total)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sums_the_tree_and_honours_cancel() {
        let dir = std::env::temp_dir().join(format!("mh-files-sizes-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("a/b")).unwrap();
        std::fs::write(dir.join("x"), [0u8; 10]).unwrap();
        std::fs::write(dir.join("a/b/y"), [0u8; 5]).unwrap();
        let size = dir_size(&dir, &CancelToken::default()).unwrap();
        assert_eq!(size, DirSize { bytes: 15, files: 2, dirs: 2, partial: false });
        let cancel = CancelToken::default();
        cancel.cancel();
        assert_eq!(dir_size(&dir, &cancel), None);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
