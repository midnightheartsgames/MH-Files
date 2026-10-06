//! Поиск дубликатов: обход папок, затем отсев по размеру, по хэшу начала файла и по хэшу
//! целиком (BLAKE3). Файл читается полностью только тогда, когда у него есть двойник того же
//! размера с тем же началом, — на обычной папке это доли процента данных.

use std::collections::{HashMap, HashSet, VecDeque};
use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

use mh_files_core::duplicates::{Group, Member, sort_groups};

use crate::{CancelToken, read};

/// Сколько читать для первого отсева.
const HEAD: u64 = 64 * 1024;
const PROGRESS_EVERY: Duration = Duration::from_millis(100);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DuplicateOptions {
    /// Файлы меньше — не сравниваются (пустые не сравниваются никогда).
    pub min_size: u64,
    pub include_hidden: bool,
}

impl Default for DuplicateOptions {
    fn default() -> DuplicateOptions {
        DuplicateOptions { min_size: 1, include_hidden: false }
    }
}

/// Ход поиска.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DuplicateProgress {
    /// Обход: сколько файлов найдено.
    Scanning { files: usize },
    /// Сравнение содержимого: сколько байт прочитано из скольких.
    Hashing { done: u64, total: u64 },
}

struct Candidate {
    path: PathBuf,
    size: u64,
    modified: Option<SystemTime>,
}

/// Группы одинаковых файлов в `roots`, лучшие (больше освобождают) первыми. Отмена — `Ok`
/// с тем, что успели найти, не бывает: возвращается ошибка «отменено».
pub fn find(
    roots: &[PathBuf],
    options: DuplicateOptions,
    cancel: &CancelToken,
    mut progress: impl FnMut(DuplicateProgress),
) -> Result<Vec<Group>, String> {
    let files = scan(roots, options, cancel, &mut progress)?;
    let mut by_size: HashMap<u64, Vec<usize>> = HashMap::new();
    for (i, file) in files.iter().enumerate() {
        by_size.entry(file.size).or_default().push(i);
    }
    let candidates: Vec<Vec<usize>> = by_size.into_values().filter(|g| g.len() > 1).collect();
    let mut total: u64 = candidates.iter().flatten().map(|&i| files[i].size.min(HEAD)).sum();
    let mut done = 0u64;
    let mut last = Instant::now();
    let mut report = |done: u64, total: u64, force: bool| {
        if force || last.elapsed() >= PROGRESS_EVERY {
            last = Instant::now();
            progress(DuplicateProgress::Hashing { done, total });
        }
    };
    // Первый отсев: начало файла. Для маленьких файлов это и есть весь файл.
    let mut full_candidates = Vec::new();
    let mut groups = Vec::new();
    for group in candidates {
        let mut by_head: HashMap<[u8; 32], Vec<usize>> = HashMap::new();
        for i in group {
            if cancel.is_cancelled() {
                return Err("отменено".into());
            }
            let file = &files[i];
            let read = file.size.min(HEAD);
            if let Ok(hash) = hash_file(&file.path, Some(HEAD), cancel) {
                by_head.entry(hash).or_default().push(i);
            }
            done += read;
            report(done, total, false);
        }
        for same in by_head.into_values().filter(|g| g.len() > 1) {
            if files[same[0]].size <= HEAD {
                groups.push(same);
            } else {
                total += same.iter().map(|&i| files[i].size).sum::<u64>();
                full_candidates.push(same);
            }
        }
    }
    // Второй: файл целиком.
    for group in full_candidates {
        let mut by_hash: HashMap<[u8; 32], Vec<usize>> = HashMap::new();
        for i in group {
            if cancel.is_cancelled() {
                return Err("отменено".into());
            }
            if let Ok(hash) = hash_file(&files[i].path, None, cancel) {
                by_hash.entry(hash).or_default().push(i);
            }
            done += files[i].size;
            report(done, total, false);
        }
        groups.extend(by_hash.into_values().filter(|g| g.len() > 1));
    }
    report(total, total, true);
    let mut groups: Vec<Group> = groups
        .into_iter()
        .map(|members| Group {
            size: files[members[0]].size,
            files: members
                .into_iter()
                .map(|i| Member { path: files[i].path.clone(), modified: files[i].modified })
                .collect(),
        })
        .collect();
    sort_groups(&mut groups);
    Ok(groups)
}

/// Обход в ширину без ссылок и junction. Пересекающиеся корни не дают файл дважды.
fn scan(
    roots: &[PathBuf],
    options: DuplicateOptions,
    cancel: &CancelToken,
    progress: &mut impl FnMut(DuplicateProgress),
) -> Result<Vec<Candidate>, String> {
    let mut files = Vec::new();
    let mut seen_dirs = HashSet::new();
    let mut queue: VecDeque<PathBuf> = roots.iter().cloned().collect();
    let mut last = Instant::now();
    while let Some(dir) = queue.pop_front() {
        if cancel.is_cancelled() {
            return Err("отменено".into());
        }
        // Пересекающиеся корни (папка и её подпапка) не обходятся дважды.
        if !seen_dirs.insert(dir.clone()) {
            continue;
        }
        let Ok(entries) = std::fs::read_dir(&dir) else { continue };
        let parent: std::sync::Arc<Path> = std::sync::Arc::from(dir.as_path());
        for entry in entries.flatten() {
            let Some(mut record) = read::from_dir_entry(&parent, &entry) else { continue };
            read::apply_dot_hidden(&mut record);
            if record.hidden() && !options.include_hidden {
                continue;
            }
            let link =
                entry.file_type().is_ok_and(|t| t.is_symlink()) || record.attributes.reparse();
            if link {
                continue;
            }
            if record.is_dir() {
                queue.push_back(record.path());
            } else if record.size >= options.min_size.max(1) {
                files.push(Candidate {
                    path: record.path(),
                    size: record.size,
                    modified: record.modified,
                });
            }
        }
        if last.elapsed() >= PROGRESS_EVERY {
            last = Instant::now();
            progress(DuplicateProgress::Scanning { files: files.len() });
        }
    }
    progress(DuplicateProgress::Scanning { files: files.len() });
    Ok(files)
}

/// BLAKE3 файла или его первых `limit` байт.
fn hash_file(path: &Path, limit: Option<u64>, cancel: &CancelToken) -> std::io::Result<[u8; 32]> {
    let file = File::open(path)?;
    let mut reader: Box<dyn Read> = match limit {
        Some(limit) => Box::new(file.take(limit)),
        None => Box::new(file),
    };
    let mut hasher = blake3::Hasher::new();
    let mut buffer = vec![0u8; 1024 * 1024];
    loop {
        if cancel.is_cancelled() {
            return Err(std::io::Error::other("отменено"));
        }
        let read = reader.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(*hasher.finalize().as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_groups_by_content() {
        let dir = std::env::temp_dir().join(format!("mh-files-dups-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("a/b")).unwrap();
        let big: Vec<u8> = (0..200_000u32).map(|i| (i % 251) as u8).collect();
        let mut other = big.clone();
        *other.last_mut().unwrap() ^= 1; // то же начало и размер, другой конец
        std::fs::write(dir.join("big1.bin"), &big).unwrap();
        std::fs::write(dir.join("a/big2.bin"), &big).unwrap();
        std::fs::write(dir.join("a/b/other.bin"), &other).unwrap();
        std::fs::write(dir.join("x.txt"), "same").unwrap();
        std::fs::write(dir.join("a/y.txt"), "same").unwrap();
        std::fs::write(dir.join("a/z.txt"), "diff").unwrap();
        std::fs::write(dir.join("empty1"), "").unwrap();
        std::fs::write(dir.join("empty2"), "").unwrap();

        let mut stages = Vec::new();
        // Корень и его подпапка — файлы не задваиваются.
        let roots = [dir.clone(), dir.join("a")];
        let groups =
            find(&roots, DuplicateOptions::default(), &CancelToken::default(), |p| stages.push(p))
                .unwrap();
        assert_eq!(groups.len(), 2);
        assert_eq!(groups[0].size, 200_000, "больше освобождает — первой");
        assert_eq!(groups[0].files.len(), 2);
        assert_eq!(groups[1].files.len(), 2);
        assert!(groups[1].files.iter().any(|m| m.path.ends_with("y.txt")));
        assert!(
            matches!(stages.last(), Some(DuplicateProgress::Hashing { done, total }) if done == total)
        );

        let cancel = CancelToken::default();
        cancel.cancel();
        assert!(find(&roots, DuplicateOptions::default(), &cancel, |_| {}).is_err());
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
