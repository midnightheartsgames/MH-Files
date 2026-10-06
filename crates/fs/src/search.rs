//! Рекурсивный поиск по имени: обход в ширину, результаты идут потоком, отмена в любой момент.
//! Ссылки и junction не обходятся — иначе петля `AppData\Local\Application Data` бесконечна.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use mh_files_core::filter::Filter;

use crate::{CancelToken, Event, Ticket, Workers, read};

/// Больше результатов не собирается: дальше запрос стоит уточнить.
pub const RESULT_LIMIT: usize = 50_000;
const BATCH_TIME: Duration = Duration::from_millis(80);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchQuery {
    /// Слова или маска, как у фильтра папки.
    pub text: String,
    pub include_hidden: bool,
}

pub(crate) fn run(
    workers: &Workers,
    ticket: Ticket,
    root: &Path,
    query: &SearchQuery,
    cancel: &CancelToken,
) {
    let mut last_scanned = 0;
    let result = walk(root, query, cancel, |batch, scanned| {
        last_scanned = scanned;
        workers.send(Event::Search { ticket, batch, scanned });
    });
    if !cancel.is_cancelled() {
        let scanned = result.as_ref().map_or(last_scanned, |&s| s);
        workers.send(Event::SearchDone { ticket, result: result.map(|_| ()), scanned });
    }
}

/// Обходит дерево и отдаёт совпадения пачками вместе с числом просмотренных папок.
pub fn walk(
    root: &Path,
    query: &SearchQuery,
    cancel: &CancelToken,
    mut emit: impl FnMut(Vec<mh_files_core::Entry>, usize),
) -> Result<usize, String> {
    let filter = Filter::new(&query.text);
    if filter.is_empty() {
        return Err("пустой запрос".into());
    }
    std::fs::read_dir(root).map_err(|error| crate::listing::describe(root, &error))?;
    let mut queue: VecDeque<PathBuf> = VecDeque::from([root.to_path_buf()]);
    let (mut scanned, mut found) = (0usize, 0usize);
    let mut batch = Vec::new();
    let mut last = Instant::now();
    while let Some(dir) = queue.pop_front() {
        if cancel.is_cancelled() {
            return Ok(scanned);
        }
        scanned += 1;
        let Ok(entries) = std::fs::read_dir(&dir) else { continue };
        let parent: Arc<Path> = Arc::from(dir.as_path());
        for entry in entries.flatten() {
            let Some(mut record) = read::from_dir_entry(&parent, &entry) else { continue };
            read::apply_dot_hidden(&mut record);
            if record.hidden() && !query.include_hidden {
                continue;
            }
            let link =
                entry.file_type().is_ok_and(|t| t.is_symlink()) || record.attributes.reparse();
            if record.is_dir() && !link {
                queue.push_back(record.path());
            }
            if filter.matches(&record.name) {
                batch.push(record);
                found += 1;
                if found >= RESULT_LIMIT {
                    emit(batch, scanned);
                    return Err(format!("найдено больше {RESULT_LIMIT} — уточните запрос"));
                }
            }
        }
        if last.elapsed() >= BATCH_TIME {
            emit(std::mem::take(&mut batch), scanned);
            last = Instant::now();
        }
    }
    emit(batch, scanned);
    Ok(scanned)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_nested_matches() {
        let dir = std::env::temp_dir().join(format!("mh-files-search-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("a/b/models")).unwrap();
        std::fs::write(dir.join("a/b/models/qwen-7b.gguf"), "").unwrap();
        std::fs::write(dir.join("a/llama.gguf"), "").unwrap();
        std::fs::write(dir.join("a/.hidden.gguf"), "").unwrap();
        let query = SearchQuery { text: "*.gguf".into(), include_hidden: false };
        let mut names = Vec::new();
        let scanned = walk(&dir, &query, &CancelToken::default(), |batch, _| {
            names.extend(batch.into_iter().map(|e| e.name));
        })
        .unwrap();
        names.sort();
        assert_eq!(names, ["llama.gguf", "qwen-7b.gguf"]);
        assert_eq!(scanned, 4);
        let words = SearchQuery { text: "QWEN gguf".into(), include_hidden: true };
        let mut count = 0;
        walk(&dir, &words, &CancelToken::default(), |batch, _| count += batch.len()).unwrap();
        assert_eq!(count, 1);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
