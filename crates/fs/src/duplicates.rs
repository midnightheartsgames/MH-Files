//! Поиск дубликатов: обход папок, затем отсев по размеру, по хэшу начала файла и по хэшу
//! целиком (BLAKE3). Файл читается полностью только тогда, когда у него есть двойник того же
//! размера с тем же началом, — на обычной папке это доли процента данных.

use std::collections::{HashMap, HashSet, VecDeque};
use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

use mh_files_core::duplicates::{Group, Member, excluded, sort_groups};

use crate::{CancelToken, read};

/// Сколько читать для первого отсева.
const HEAD: u64 = 64 * 1024;
const PROGRESS_EVERY: Duration = Duration::from_millis(100);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DuplicateOptions {
    /// Файлы меньше — не сравниваются (пустые не сравниваются никогда).
    pub min_size: u64,
    pub include_hidden: bool,
    /// Правила пропуска: `duplicates::excluded`.
    pub exclude: Vec<String>,
}

impl Default for DuplicateOptions {
    fn default() -> DuplicateOptions {
        DuplicateOptions { min_size: 1, include_hidden: false, exclude: Vec::new() }
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
    let files = scan(roots, &options, cancel, &mut progress)?;
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
    // Жёсткие ссылки на один файл — не копии: места они не занимают. В группе остаётся по
    // одному пути на физический файл.
    let groups: Vec<Vec<usize>> = groups
        .into_iter()
        .map(|members| {
            let mut seen = HashSet::new();
            members
                .into_iter()
                .filter(|&i| match mh_files_platform::files::identity(&files[i].path) {
                    Some(id) => seen.insert(id),
                    None => true,
                })
                .collect::<Vec<usize>>()
        })
        .filter(|members| members.len() > 1)
        .collect();
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

/// Чем кончилась замена копий жёсткими ссылками.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LinkReport {
    pub linked: usize,
    pub freed: u64,
    /// Что не вышло — по строке на файл.
    pub failed: Vec<String>,
}

/// Заменить каждую лишнюю копию жёсткой ссылкой на оставляемую: `(оставляемая, лишняя,
/// размер)`. Лишняя заменяется, только если она и оставляемая всё ещё того же размера, а
/// лишняя не менялась с поиска; ссылка сначала создаётся рядом под временным именем и
/// только потом встаёт на место копии — сбой посередине копию не теряет. Ссылки возможны
/// только в пределах одного тома NTFS.
pub fn replace_with_links(pairs: &[(PathBuf, Member, u64)], cancel: &CancelToken) -> LinkReport {
    let mut report = LinkReport::default();
    for (keeper, extra, size) in pairs {
        if cancel.is_cancelled() {
            break;
        }
        let path = &extra.path;
        let fail = |report: &mut LinkReport, why: String| {
            report.failed.push(format!("{}: {why}", path.display()));
        };
        let (Ok(keeper_meta), Ok(extra_meta)) =
            (std::fs::metadata(keeper), std::fs::metadata(path))
        else {
            fail(&mut report, "файла уже нет".into());
            continue;
        };
        if keeper_meta.len() != *size
            || extra_meta.len() != *size
            || extra.modified.is_some_and(|m| extra_meta.modified().ok() != Some(m))
        {
            fail(&mut report, "изменился после поиска — пропущен".into());
            continue;
        }
        let same = mh_files_platform::files::identity(keeper);
        if same.is_some() && same == mh_files_platform::files::identity(path) {
            continue; // Уже ссылка на тот же файл.
        }
        let name = path.file_name().unwrap_or_default().to_string_lossy();
        let temp = path.with_file_name(format!(".{name}.mh-link"));
        if let Err(error) = std::fs::hard_link(keeper, &temp) {
            fail(&mut report, format!("ссылка не создана ({error}) — другой диск или не NTFS?"));
            continue;
        }
        match std::fs::rename(&temp, path) {
            Ok(()) => {
                report.linked += 1;
                report.freed += size;
            }
            Err(error) => {
                let _ = std::fs::remove_file(&temp);
                fail(&mut report, format!("не заменён: {error}"));
            }
        }
    }
    report
}

/// Обход в ширину без ссылок и junction. Пересекающиеся корни не дают файл дважды.
fn scan(
    roots: &[PathBuf],
    options: &DuplicateOptions,
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
            if link || excluded(&record.path(), &options.exclude) {
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

        // Исключения и порог размера.
        let options = DuplicateOptions {
            exclude: vec!["b".into(), "*.txt".into()],
            min_size: 10,
            ..DuplicateOptions::default()
        };
        let groups = find(&roots, options, &CancelToken::default(), |_| {}).unwrap();
        assert_eq!(groups.len(), 1, "txt пропущены по маске");
        assert_eq!(groups[0].size, 200_000);
        let options = DuplicateOptions { min_size: 1 << 20, ..DuplicateOptions::default() };
        assert!(find(&roots, options, &CancelToken::default(), |_| {}).unwrap().is_empty());

        // Лишняя копия становится жёсткой ссылкой — и больше не дубликат.
        let groups =
            find(&roots, DuplicateOptions::default(), &CancelToken::default(), |_| {}).unwrap();
        let group = &groups[0];
        let pairs: Vec<(PathBuf, Member, u64)> = group
            .link_plan(mh_files_core::duplicates::Keep::ShortestPath)
            .into_iter()
            .map(|(keeper, extra)| (keeper, extra, group.size))
            .collect();
        let report = replace_with_links(&pairs, &CancelToken::default());
        assert_eq!(report, LinkReport { linked: 1, freed: 200_000, failed: Vec::new() });
        assert_eq!(std::fs::read(dir.join("a/big2.bin")).unwrap(), big);
        let groups =
            find(&roots, DuplicateOptions::default(), &CancelToken::default(), |_| {}).unwrap();
        assert_eq!(groups.len(), 1, "ссылки на один файл — не дубликаты");
        // Повторно — уже ссылка, ничего не делается; изменённый файл не трогается.
        assert_eq!(replace_with_links(&pairs, &CancelToken::default()).linked, 0);
        std::fs::write(dir.join("a/y.txt"), "changed!").unwrap();
        let stale = [(dir.join("x.txt"), Member { path: dir.join("a/y.txt"), modified: None }, 4)];
        let report = replace_with_links(&stale, &CancelToken::default());
        assert_eq!(report.linked, 0);
        assert_eq!(report.failed.len(), 1);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
