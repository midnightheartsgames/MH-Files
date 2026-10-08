//! Содержимое корзины для её вкладки: сведения `$I…` каждой папки корзины разбираются в
//! записи с прежними именем и папкой, рядом — где лежит сам объект (`$R…`) для
//! «Восстановить» и «Удалить насовсем».

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use mh_files_core::entry::{Attributes, Entry, EntryKind};
use mh_files_core::recycle::{data_name, parse_info};

use crate::CancelToken;

/// Удалённый объект: запись для списка и его файлы в корзине.
#[derive(Debug, Clone)]
pub struct Recycled {
    /// Прежние имя и папка; дата изменения — время удаления.
    pub entry: Entry,
    /// Сам объект (`$R…`).
    pub data: PathBuf,
    /// Сведения (`$I…`).
    pub info: PathBuf,
    /// Откуда удалён — туда и вернётся (имя в списке может быть с номером).
    pub original: PathBuf,
}

/// Сведения больше этого — не сведения корзины.
const INFO_LIMIT: u64 = 64 * 1024;

/// Удалённое из папок корзины `dirs`. Объекты, удалённые из одного места с одним именем,
/// различаются в списке приставкой « (2)», « (3)»… к имени — путь записи должен быть
/// единственным.
pub fn scan(dirs: &[PathBuf], cancel: &CancelToken) -> Vec<Recycled> {
    let mut found = Vec::new();
    let mut seen: HashMap<PathBuf, usize> = HashMap::new();
    for dir in dirs {
        let Ok(read) = std::fs::read_dir(dir) else { continue };
        for item in read.flatten() {
            if cancel.is_cancelled() {
                return found;
            }
            let name = item.file_name().to_string_lossy().into_owned();
            let Some(data_name) = data_name(&name) else { continue };
            let info = item.path();
            let Some(deleted) = read_info(&info) else { continue };
            let data = dir.join(data_name);
            // Без самого объекта сведения ничего не значат (его удалили мимо корзины).
            let Ok(meta) = std::fs::metadata(&data) else { continue };
            let Some(parent) = deleted.original.parent().map(Path::to_path_buf) else { continue };
            let Some(original_name) = deleted.original.file_name() else { continue };
            let original_name = original_name.to_string_lossy().into_owned();
            let count = seen.entry(deleted.original.clone()).or_insert(0);
            *count += 1;
            let shown = if *count == 1 { original_name } else { numbered(&original_name, *count) };
            let entry = Entry {
                name: shown,
                parent: Arc::from(parent.as_path()),
                kind: if meta.is_dir() { EntryKind::Dir } else { EntryKind::File },
                size: deleted.size,
                modified: deleted.deleted,
                created: None,
                attributes: Attributes::default(),
            };
            found.push(Recycled { entry, data, info, original: deleted.original });
        }
    }
    found
}

fn read_info(path: &Path) -> Option<mh_files_core::recycle::Deleted> {
    if std::fs::metadata(path).ok()?.len() > INFO_LIMIT {
        return None;
    }
    parse_info(&std::fs::read(path).ok()?)
}

/// `отчёт.docx` → `отчёт (2).docx`.
fn numbered(name: &str, n: usize) -> String {
    match name.rfind('.').filter(|&dot| dot > 0) {
        Some(dot) => format!("{} ({n}){}", &name[..dot], &name[dot..]),
        None => format!("{name} ({n})"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn info(path: &str, size: u64) -> Vec<u8> {
        let mut data: Vec<u8> =
            [2u64, size, 133_000_000_000_000_000].iter().flat_map(|v| v.to_le_bytes()).collect();
        data.extend((path.encode_utf16().count() as u32 + 1).to_le_bytes());
        data.extend(path.encode_utf16().chain([0]).flat_map(u16::to_le_bytes));
        data
    }

    #[test]
    fn lists_deleted_items_with_unique_paths() {
        let dir = std::env::temp_dir().join(format!("mh-recycle-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("$RDIR1")).unwrap();
        std::fs::write(dir.join("$IDIR1"), info("/home/a/photos", 2048)).unwrap();
        std::fs::write(dir.join("$RAB1.txt"), "one").unwrap();
        std::fs::write(dir.join("$IAB1.txt"), info("/home/a/notes.txt", 3)).unwrap();
        std::fs::write(dir.join("$RAB2.txt"), "two").unwrap();
        std::fs::write(dir.join("$IAB2.txt"), info("/home/a/notes.txt", 3)).unwrap();
        // Сведения без объекта и мусор — мимо.
        std::fs::write(dir.join("$IGONE.txt"), info("/home/a/gone.txt", 1)).unwrap();
        std::fs::write(dir.join("desktop.ini"), "x").unwrap();
        let mut found = scan(std::slice::from_ref(&dir), &CancelToken::default());
        found.sort_by(|a, b| a.entry.name.cmp(&b.entry.name));
        let names: Vec<&str> = found.iter().map(|r| r.entry.name.as_str()).collect();
        assert_eq!(names, ["notes (2).txt", "notes.txt", "photos"]);
        let photos = &found[2];
        assert!(photos.entry.is_dir());
        assert_eq!(photos.entry.size, 2048);
        assert_eq!(photos.data, dir.join("$RDIR1"));
        assert_eq!(photos.original, Path::new("/home/a/photos"));
        assert_eq!(
            found[0].original,
            Path::new("/home/a/notes.txt"),
            "вернётся под прежним именем"
        );
        assert_eq!(numbered(".bashrc", 2), ".bashrc (2)");
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
