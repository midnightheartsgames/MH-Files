//! Чтение папки пачками.

use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use mh_files_core::sort::natural_cmp;

use crate::{CancelToken, Event, Ticket, Workers, read};

/// Пачка уходит, когда набралось столько записей или прошло столько времени: первая строка
/// появляется сразу, а сто тысяч записей не превращаются в сто тысяч сообщений.
const BATCH: usize = 2000;
const BATCH_TIME: Duration = Duration::from_millis(60);

pub(crate) fn run(workers: &Workers, ticket: Ticket, dir: &Path, cancel: &CancelToken) {
    let result = read_batched(dir, cancel, |batch| {
        workers.send(Event::Listing { ticket, batch });
    });
    if !cancel.is_cancelled() {
        workers.send(Event::ListingDone { ticket, result });
    }
}

/// Читает папку и отдаёт записи пачками. Нечитаемая запись пропускается, нечитаемая папка —
/// ошибка с понятным текстом.
pub fn read_batched(
    dir: &Path,
    cancel: &CancelToken,
    mut emit: impl FnMut(Vec<mh_files_core::Entry>),
) -> Result<(), String> {
    let entries = std::fs::read_dir(dir).map_err(|error| describe(dir, &error))?;
    let parent: Arc<Path> = Arc::from(dir);
    let mut batch = Vec::new();
    let mut last = Instant::now();
    for entry in entries {
        if cancel.is_cancelled() {
            return Ok(());
        }
        let Ok(entry) = entry else { continue };
        if let Some(mut record) = read::from_dir_entry(&parent, &entry) {
            read::apply_dot_hidden(&mut record);
            batch.push(record);
        }
        if batch.len() >= BATCH || (last.elapsed() >= BATCH_TIME && !batch.is_empty()) {
            emit(std::mem::take(&mut batch));
            last = Instant::now();
        }
    }
    if !batch.is_empty() {
        emit(batch);
    }
    Ok(())
}

/// Понятный текст ошибки чтения папки.
pub fn describe(path: &Path, error: &std::io::Error) -> String {
    use std::io::ErrorKind;
    let what = match error.kind() {
        ErrorKind::NotFound => "папка не найдена".to_string(),
        ErrorKind::PermissionDenied => "нет доступа".to_string(),
        _ => error.to_string(),
    };
    format!("{what}: {}", path.display())
}

/// Подпапки, чьи имена начинаются с `prefix` (без учёта регистра), по алфавиту.
pub fn subdirs(dir: &Path, prefix: &str, limit: usize) -> Vec<String> {
    let prefix = prefix.to_lowercase();
    let Ok(entries) = std::fs::read_dir(dir) else { return Vec::new() };
    let mut names: Vec<String> = entries
        .flatten()
        .filter(|entry| entry.file_type().is_ok_and(|t| t.is_dir() || t.is_symlink()))
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .filter(|name| name.to_lowercase().starts_with(&prefix))
        .collect();
    names.sort_by(|a, b| natural_cmp(a, b));
    names.truncate(limit);
    names
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_in_batches_and_reports_errors() {
        let dir = std::env::temp_dir().join(format!("mh-files-list-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("Sub")).unwrap();
        std::fs::create_dir_all(dir.join("sub2")).unwrap();
        for i in 0..2500 {
            std::fs::write(dir.join(format!("f{i}.txt")), "x").unwrap();
        }
        let mut total = 0;
        let mut batches = 0;
        read_batched(&dir, &CancelToken::default(), |batch| {
            total += batch.len();
            batches += 1;
        })
        .unwrap();
        assert_eq!(total, 2502);
        assert!(batches >= 2);
        assert_eq!(subdirs(&dir, "SU", 10), ["Sub", "sub2"]);
        let missing = read_batched(&dir.join("nope"), &CancelToken::default(), |_| {});
        assert!(missing.unwrap_err().contains("не найдена"));
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
