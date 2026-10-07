//! Рекурсивный поиск по имени или по тексту внутри файлов: обход в ширину, результаты идут
//! потоком, отмена в любой момент. Ссылки и junction не обходятся — иначе петля
//! `AppData\Local\Application Data` бесконечна.
//!
//! По содержимому ищется в текстовых файлах (UTF-8, UTF-16 с BOM, Windows-1251): файл с
//! нулевыми байтами в начале считается двоичным и пропускается, большие — тоже.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use std::io::Read;

use mh_files_core::filter::{ContentQuery, Filter};

use crate::{CancelToken, Event, Ticket, Workers, read};

/// Больше результатов не собирается: дальше запрос стоит уточнить.
pub const RESULT_LIMIT: usize = 50_000;
const BATCH_TIME: Duration = Duration::from_millis(80);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchQuery {
    /// Слова или маска, как у фильтра папки; для поиска по содержимому — слова текста и
    /// маски имён ([`ContentQuery`]).
    pub text: String,
    pub include_hidden: bool,
    /// Искать слова внутри файлов, а не в именах.
    pub content: bool,
}

/// Файлы больше этого по содержимому не просматриваются.
pub const CONTENT_FILE_LIMIT: u64 = 64 << 20;
/// Сколько байт начала смотреть, чтобы узнать двоичный файл.
const SNIFF: usize = 8 << 10;

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
    let content = query.content.then(|| ContentQuery::new(&query.text));
    if filter.is_empty() || content.as_ref().is_some_and(ContentQuery::is_empty) {
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
            let found_here = match &content {
                None => filter.matches(&record.name),
                Some(content) => {
                    !record.is_dir()
                        && record.size <= CONTENT_FILE_LIMIT
                        && content.name_matches(&record.name)
                        && file_contains(&record.path(), content, cancel)
                }
            };
            if found_here {
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

/// Есть ли в файле все слова запроса. Двоичный, нечитаемый или отменённый — нет.
fn file_contains(path: &Path, query: &ContentQuery, cancel: &CancelToken) -> bool {
    let Ok(file) = std::fs::File::open(path) else { return false };
    let mut bytes = Vec::new();
    let mut reader = file.take(CONTENT_FILE_LIMIT);
    let mut chunk = vec![0u8; SNIFF];
    let Ok(read) = reader.read(&mut chunk) else { return false };
    chunk.truncate(read);
    let utf16 = chunk.starts_with(&[0xFF, 0xFE]) || chunk.starts_with(&[0xFE, 0xFF]);
    if !utf16 && chunk.contains(&0) {
        return false;
    }
    bytes.extend_from_slice(&chunk);
    if cancel.is_cancelled() || reader.read_to_end(&mut bytes).is_err() {
        return false;
    }
    let (text, _) = crate::preview::decode_text(&bytes);
    query.text_matches(&text.to_lowercase())
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
        // В Windows точка в имени файл не скрывает — нужен атрибут, как у Проводника.
        #[cfg(windows)]
        {
            let status = std::process::Command::new("attrib")
                .arg("+h")
                .arg(dir.join("a").join(".hidden.gguf"))
                .status()
                .unwrap();
            assert!(status.success());
        }
        let query = SearchQuery { text: "*.gguf".into(), include_hidden: false, content: false };
        let mut names = Vec::new();
        let scanned = walk(&dir, &query, &CancelToken::default(), |batch, _| {
            names.extend(batch.into_iter().map(|e| e.name));
        })
        .unwrap();
        names.sort();
        assert_eq!(names, ["llama.gguf", "qwen-7b.gguf"]);
        assert_eq!(scanned, 4);
        let words = SearchQuery { text: "QWEN gguf".into(), include_hidden: true, content: false };
        let mut count = 0;
        walk(&dir, &words, &CancelToken::default(), |batch, _| count += batch.len()).unwrap();
        assert_eq!(count, 1);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn finds_text_inside_files() {
        let dir = std::env::temp_dir().join(format!("mh-files-content-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("docs")).unwrap();
        std::fs::write(dir.join("docs/договор.txt"), "Договор АРЕНДЫ квартиры").unwrap();
        let (cp1251, _, _) = encoding_rs::WINDOWS_1251.encode("старый договор аренды");
        std::fs::write(dir.join("docs/old.ini"), cp1251).unwrap();
        let mut utf16 = vec![0xFF, 0xFE];
        utf16.extend("договор аренды".encode_utf16().flat_map(u16::to_le_bytes));
        std::fs::write(dir.join("docs/wide.txt"), utf16).unwrap();
        std::fs::write(
            dir.join("docs/binary.bin"),
            [&[0u8, 0][..], "договор аренды".as_bytes()].concat(),
        )
        .unwrap();
        std::fs::write(dir.join("docs/other.txt"), "только договор").unwrap();
        let search = |text: &str| {
            let query = SearchQuery { text: text.into(), include_hidden: true, content: true };
            let mut names = Vec::new();
            walk(&dir, &query, &CancelToken::default(), |batch, _| {
                names.extend(batch.into_iter().map(|e| e.name));
            })
            .unwrap();
            names.sort();
            names
        };
        assert_eq!(search("аренды договор"), ["old.ini", "wide.txt", "договор.txt"]);
        assert_eq!(search("аренды *.txt"), ["wide.txt", "договор.txt"]);
        let empty = SearchQuery { text: "*.txt".into(), include_hidden: true, content: true };
        assert!(walk(&dir, &empty, &CancelToken::default(), |_, _| {}).is_err());
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
