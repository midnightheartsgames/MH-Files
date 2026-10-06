//! Запись о файле из каталога или по пути.

use std::fs::{DirEntry, Metadata};
use std::path::Path;
use std::sync::Arc;

use mh_files_core::{Attributes, Entry, EntryKind};

/// Запись из элемента каталога. В Windows метаданные приходят вместе с перечислением
/// (`FindNextFile`), отдельного обращения к диску нет.
pub fn from_dir_entry(parent: &Arc<Path>, entry: &DirEntry) -> Option<Entry> {
    let name = entry.file_name().to_string_lossy().into_owned();
    let meta = entry.metadata().ok();
    Some(build(parent.clone(), name, meta.as_ref(), entry.file_type().ok()))
}

/// Запись по пути (для правок наблюдателя). Ссылка не разыменовывается.
pub fn stat(path: &Path) -> std::io::Result<Entry> {
    let meta = std::fs::symlink_metadata(path)?;
    let parent: Arc<Path> = Arc::from(path.parent().unwrap_or(path));
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    Ok(build(parent, name, Some(&meta), Some(meta.file_type())))
}

fn build(
    parent: Arc<Path>,
    name: String,
    meta: Option<&Metadata>,
    file_type: Option<std::fs::FileType>,
) -> Entry {
    let symlink = file_type.is_some_and(|t| t.is_symlink());
    let mut is_dir = file_type.is_some_and(|t| t.is_dir());
    if symlink {
        // Ссылка на папку ведёт себя как папка: в неё можно войти.
        is_dir = std::fs::metadata(parent.join(&name)).is_ok_and(|m| m.is_dir());
    }
    let mut attributes = meta.map(attributes_of).unwrap_or_default();
    if symlink {
        attributes.0 |= Attributes::REPARSE;
    }
    Entry {
        name,
        parent,
        kind: if is_dir { EntryKind::Dir } else { EntryKind::File },
        size: if is_dir { 0 } else { meta.map_or(0, Metadata::len) },
        modified: meta.and_then(|m| m.modified().ok()),
        created: meta.and_then(|m| m.created().ok()),
        attributes,
    }
}

#[cfg(windows)]
fn attributes_of(meta: &Metadata) -> Attributes {
    use std::os::windows::fs::MetadataExt;
    Attributes(meta.file_attributes())
}

#[cfg(not(windows))]
fn attributes_of(_meta: &Metadata) -> Attributes {
    Attributes::default()
}

/// Вне Windows атрибута «скрытый» нет; для проверки интерфейса скрытыми считаются имена с точкой.
pub fn apply_dot_hidden(entry: &mut Entry) {
    if cfg!(not(windows)) && entry.name.starts_with('.') {
        entry.attributes.0 |= Attributes::HIDDEN;
    }
}
