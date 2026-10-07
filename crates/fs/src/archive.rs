//! Архивы как папки: zip и 7z — своим кодом, rar — через 7-Zip или `tar.exe` Windows
//! ([`crate::archive_tool`]); только чтение.
//!
//! Внутри архива у записи путь вида `C:\загрузки\a.zip\docs\x.txt` — такого пути на диске нет,
//! [`split`] находит в нём файл архива и путь внутри. Оглавление архива читается один раз и
//! держится в маленьком кэше: переходы по папкам архива не перечитывают его заново.
//!
//! Архив в архиве (`a.zip\b.7z\x.txt`) открывается так же: вложенный архив копируется во
//! временную папку один раз (по размеру и дате внешнего), дальше всё как с обычным.
//!
//! Имена при извлечении проверяются: `..`, абсолютные пути и символы, запрещённые в Windows,
//! не дают записать файл мимо папки назначения.

use std::collections::HashSet;
use std::fs::File;
use std::io::{BufWriter, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::SystemTime;

use mh_files_core::location::Location;
use mh_files_core::{Attributes, Entry, EntryKind};
use parking_lot::Mutex;

use crate::CancelToken;

/// Сколько оглавлений помнить.
const CACHE_SIZE: usize = 4;

/// Можно ли открыть файл с таким расширением как папку.
pub fn is_archive_ext(ext: &str) -> bool {
    mh_files_core::entry::ARCHIVE_EXTENSIONS.contains(&ext)
}

pub fn is_archive_name(name: &str) -> bool {
    mh_files_core::entry::is_archive_name(name)
}

/// Запись оглавления.
#[derive(Debug, Clone)]
struct Item {
    /// Путь в архиве через `/`, без `/` по краям.
    inner: String,
    /// Имя, как оно записано в архиве, — по нему запись читается. У папок, которых в
    /// оглавлении нет (только в путях файлов), — пусто.
    raw: String,
    /// Номер в zip.
    index: usize,
    is_dir: bool,
    size: u64,
    modified: Option<SystemTime>,
}

/// Чем читать архив.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Kind {
    Zip,
    Seven,
    /// Внешняя программа (rar).
    Tool(crate::archive_tool::Tool),
}

#[derive(Debug)]
struct Contents {
    kind: Kind,
    /// Файл архива на диске; у вложенного — его копия во временной папке.
    file: PathBuf,
    items: Vec<Item>,
}

type Key = (PathBuf, u64, Option<SystemTime>);

static CACHE: Mutex<Vec<(Key, Arc<Contents>)>> = Mutex::new(Vec::new());

/// Файл архива и путь внутри для пути, проходящего через архив. `None` — путь обычный.
/// Если путь проходит через вложенный архив, «файл архива» — путь вложенного через внешний
/// (`a.zip\b.7z`), а путь внутри — уже в нём.
pub fn split(path: &Path) -> Option<(PathBuf, String)> {
    let (mut archive, inner) = real_split(path)?;
    let parts: Vec<&str> = inner.split('/').filter(|p| !p.is_empty()).collect();
    let mut start = 0;
    // Последнее звено — сама запись: вложенным архивом могут быть только звенья до неё.
    for end in 0..parts.len().saturating_sub(1) {
        if !is_archive_name(parts[end]) {
            continue;
        }
        let candidate = parts[start..=end].join("/");
        let is_file = contents(&archive)
            .is_ok_and(|c| c.items.iter().any(|i| !i.is_dir && i.inner == candidate));
        if is_file {
            for part in &parts[start..=end] {
                archive.push(part);
            }
            start = end + 1;
        }
    }
    Some((archive, parts[start..].join("/")))
}

/// Ближайший настоящий файл архива на диске среди предков пути.
fn real_split(path: &Path) -> Option<(PathBuf, String)> {
    for ancestor in path.ancestors().skip(1) {
        match std::fs::metadata(ancestor) {
            Ok(meta) if meta.is_file() => {
                let name = ancestor.file_name()?.to_string_lossy();
                if !is_archive_name(&name) {
                    return None;
                }
                let inner = path.strip_prefix(ancestor).ok()?;
                let inner: Vec<String> =
                    inner.iter().map(|part| part.to_string_lossy().into_owned()).collect();
                return Some((ancestor.to_path_buf(), inner.join("/")));
            }
            // Настоящая папка: дальше вверх архива уже не будет.
            Ok(_) => return None,
            Err(_) => continue,
        }
    }
    None
}

/// Файл архива на диске: сам `archive` или, у вложенного, его копия во временной папке.
fn resolve(archive: &Path) -> Result<PathBuf, String> {
    if std::fs::metadata(archive).is_ok_and(|meta| meta.is_file()) {
        return Ok(archive.to_path_buf());
    }
    let (outer, inner) =
        split(archive).ok_or_else(|| format!("{}: архив не найден", archive.display()))?;
    nested_copy(&outer, &inner)
}

/// Временная папка вложенных архивов.
pub fn nested_dir() -> PathBuf {
    std::env::temp_dir().join("MH Files").join("nested")
}

/// Скопировать вложенный архив `inner` из `outer` во временную папку (если копии ещё нет).
/// Имя копии зависит от внешнего архива, его размера и даты — изменился внешний, будет новая.
fn nested_copy(outer: &Path, inner: &str) -> Result<PathBuf, String> {
    let contents = contents(outer)?;
    let item = contents
        .items
        .iter()
        .find(|i| !i.is_dir && i.inner == inner)
        .ok_or_else(|| format!("в архиве нет файла «{inner}»"))?;
    let meta = std::fs::metadata(&contents.file).map_err(|e| e.to_string())?;
    let stamp = meta.modified().ok().and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok());
    let key = format!("{}|{}|{:?}|{inner}", contents.file.display(), meta.len(), stamp);
    let hash = key.bytes().fold(0xcbf2_9ce4_8422_2325u64, |h, b| {
        (h ^ u64::from(b)).wrapping_mul(0x0000_0100_0000_01b3)
    });
    let dir = nested_dir();
    let name = safe_component(name_of(inner)).unwrap_or_else(|| "archive".into());
    let path = dir.join(format!("{hash:016x}-{name}"));
    if std::fs::metadata(&path).is_ok_and(|m| m.len() == item.size) {
        return Ok(path);
    }
    std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    forget_old_copies(&dir);
    let temp = path.with_extension("part");
    let written = (|| {
        let mut out = BufWriter::new(File::create(&temp).map_err(|e| e.to_string())?);
        with_entry(&contents, item, |reader| {
            std::io::copy(reader, &mut out).map(|_| ()).map_err(|e| format!("{inner}: {e}"))
        })?;
        out.into_inner().map_err(|e| e.to_string())?;
        std::fs::rename(&temp, &path).map_err(|e| e.to_string())
    })();
    if let Err(error) = written {
        let _ = std::fs::remove_file(&temp);
        return Err(error);
    }
    Ok(path)
}

/// Копии вложенных архивов старше суток удаляются: временная папка не растёт бесконечно.
fn forget_old_copies(dir: &Path) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    let day = std::time::Duration::from_secs(24 * 60 * 60);
    for entry in entries.flatten() {
        let old = entry
            .metadata()
            .and_then(|m| m.modified())
            .is_ok_and(|t| t.elapsed().is_ok_and(|age| age > day));
        if old {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

/// Прочитать одну запись: `use_reader` получает её содержимое потоком.
fn with_entry(
    contents: &Contents,
    item: &Item,
    use_reader: impl FnOnce(&mut dyn Read) -> Result<(), String>,
) -> Result<(), String> {
    match &contents.kind {
        Kind::Zip => {
            let mut zip = open_zip(&contents.file)?;
            let mut file = zip.by_index(item.index).map_err(zip_error)?;
            use_reader(&mut file)
        }
        Kind::Seven => {
            let mut reader = open_seven(&contents.file)?;
            let mut use_reader = Some(use_reader);
            let mut result = Err(format!("в архиве нет файла «{}»", item.inner));
            reader
                .for_each_entries(|entry, data| {
                    if entry.name() != item.raw {
                        return Ok(true);
                    }
                    if let Some(use_reader) = use_reader.take() {
                        result = use_reader(data);
                    }
                    Ok(false)
                })
                .map_err(seven_error)?;
            result
        }
        Kind::Tool(tool) => {
            let mut entry = crate::archive_tool::open(tool, &contents.file, &item.raw)?;
            use_reader(&mut entry)?;
            entry.finish()
        }
    }
}

/// Содержимое папки `inner` архива — записи для списка.
pub fn list(archive: &Path, inner: &str) -> Result<Vec<Entry>, String> {
    let contents = contents(archive)?;
    let inner = inner.trim_matches('/');
    if !inner.is_empty() && !contents.items.iter().any(|i| i.is_dir && i.inner == inner) {
        return Err(format!("в архиве нет папки «{inner}»"));
    }
    let parent: Arc<Path> = Arc::from(Location::archive_path(archive, inner));
    Ok(contents
        .items
        .iter()
        .filter(|item| parent_of(&item.inner) == inner)
        .map(|item| Entry {
            name: name_of(&item.inner).to_string(),
            parent: parent.clone(),
            kind: if item.is_dir { EntryKind::Dir } else { EntryKind::File },
            size: item.size,
            modified: item.modified,
            created: None,
            attributes: Attributes(Attributes::READONLY),
        })
        .collect())
}

/// Сколько файлов и байт в папке архива (со вложенными) — для Инспектора.
pub fn folder_totals(archive: &Path, inner: &str) -> Result<(usize, usize, u64), String> {
    let contents = contents(archive)?;
    let inner = inner.trim_matches('/');
    let (mut dirs, mut files, mut bytes) = (0, 0, 0);
    for item in contents.items.iter().filter(|i| i.inner != inner && inside(&i.inner, inner)) {
        if item.is_dir {
            dirs += 1;
        } else {
            files += 1;
            bytes += item.size;
        }
    }
    Ok((dirs, files, bytes))
}

/// Прочитать файл из архива целиком. Больше `limit` байт — ошибка.
pub fn read(archive: &Path, inner: &str, limit: u64) -> Result<Vec<u8>, String> {
    let contents = contents(archive)?;
    let inner = inner.trim_matches('/');
    let item = contents
        .items
        .iter()
        .find(|i| !i.is_dir && i.inner == inner)
        .ok_or_else(|| format!("в архиве нет файла «{inner}»"))?;
    if item.size > limit {
        return Err("слишком большой файл для предпросмотра".into());
    }
    if contents.kind == Kind::Seven {
        let mut reader = open_seven(&contents.file)?;
        return reader.read_file(&item.raw).map_err(seven_error);
    }
    let mut bytes = Vec::with_capacity(item.size as usize);
    with_entry(&contents, item, |reader| {
        reader
            .take(limit.saturating_add(1))
            .read_to_end(&mut bytes)
            .map(|_| ())
            .map_err(|e| e.to_string())
    })?;
    if bytes.len() as u64 > limit {
        return Err("слишком большой файл для предпросмотра".into());
    }
    Ok(bytes)
}

/// Извлечь записи `inners` (файлы и папки целиком; пустая строка — весь архив) в `dest`.
/// Занятые имена не перезаписываются: рядом появляется «имя (2)». Возвращает созданные пути
/// верхнего уровня. `progress` получает число записанных байт.
pub fn extract(
    archive: &Path,
    inners: &[String],
    dest: &Path,
    cancel: &CancelToken,
    mut progress: impl FnMut(u64),
) -> Result<Vec<PathBuf>, String> {
    let contents = contents(archive)?;
    std::fs::create_dir_all(dest).map_err(|e| format!("{}: {e}", dest.display()))?;
    // Каждой выбранной записи — своё место в папке назначения.
    let mut targets: Vec<(String, PathBuf)> = Vec::new();
    let mut expanded = Vec::new();
    for inner in inners {
        let inner = inner.trim_matches('/');
        if inner.is_empty() {
            let top = contents.items.iter().filter(|i| !i.inner.contains('/'));
            expanded.extend(top.map(|i| i.inner.clone()));
        } else {
            expanded.push(inner.to_string());
        }
    }
    for inner in expanded {
        let Some(name) = safe_component(name_of(&inner)) else { continue };
        targets.push((inner, unique_path(&dest.join(name))));
    }
    let target_of = |item: &Item| -> Option<PathBuf> {
        targets.iter().find_map(|(inner, top)| {
            let rest = if item.inner == *inner {
                ""
            } else {
                item.inner.strip_prefix(inner.as_str())?.strip_prefix('/')?
            };
            let mut path = top.clone();
            for part in rest.split('/').filter(|p| !p.is_empty()) {
                path.push(safe_component(part)?);
            }
            Some(path)
        })
    };
    // Папки — сразу, в том числе пустые.
    for item in contents.items.iter().filter(|i| i.is_dir) {
        if let Some(path) = target_of(item) {
            std::fs::create_dir_all(&path).map_err(|e| format!("{}: {e}", path.display()))?;
        }
    }
    let wanted: HashSet<usize> = contents
        .items
        .iter()
        .enumerate()
        .filter(|(_, i)| !i.is_dir && target_of(i).is_some())
        .map(|(n, _)| n)
        .collect();
    let mut write = |item: &Item, reader: &mut dyn Read| -> Result<(), String> {
        let Some(path) = target_of(item) else { return Ok(()) };
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
        }
        let file = File::create(&path).map_err(|e| format!("{}: {e}", path.display()))?;
        let mut out = BufWriter::new(file);
        let mut buffer = vec![0u8; 256 * 1024];
        loop {
            if cancel.is_cancelled() {
                drop(out);
                let _ = std::fs::remove_file(&path);
                return Err("отменено".into());
            }
            let read = reader.read(&mut buffer).map_err(|e| format!("{}: {e}", item.inner))?;
            if read == 0 {
                break;
            }
            out.write_all(&buffer[..read]).map_err(|e| format!("{}: {e}", path.display()))?;
            progress(read as u64);
        }
        let file = out.into_inner().map_err(|e| e.to_string())?;
        if let Some(modified) = item.modified {
            let _ = file.set_modified(modified);
        }
        Ok(())
    };
    if contents.kind == Kind::Seven {
        let by_raw: std::collections::HashMap<&str, &Item> =
            wanted.iter().map(|&n| (contents.items[n].raw.as_str(), &contents.items[n])).collect();
        let mut reader = open_seven(&contents.file)?;
        let mut failure = None;
        reader
            .for_each_entries(|entry, data| {
                if let Some(item) = by_raw.get(entry.name())
                    && let Err(error) = write(item, data)
                {
                    failure = Some(error);
                    return Ok(false);
                }
                Ok(!cancel.is_cancelled())
            })
            .map_err(seven_error)?;
        if let Some(error) = failure {
            return Err(error);
        }
    } else if contents.kind == Kind::Zip {
        let mut zip = open_zip(&contents.file)?;
        let mut indices: Vec<usize> = wanted.iter().copied().collect();
        indices.sort_unstable();
        for n in indices {
            let item = &contents.items[n];
            let mut file = zip.by_index(item.index).map_err(zip_error)?;
            write(item, &mut file)?;
        }
    } else {
        // Внешняя программа: процесс на запись.
        let mut indices: Vec<usize> = wanted.iter().copied().collect();
        indices.sort_unstable();
        for n in indices {
            let item = &contents.items[n];
            with_entry(&contents, item, |reader| write(item, reader))?;
        }
    }
    if cancel.is_cancelled() {
        return Err("отменено".into());
    }
    Ok(targets.into_iter().map(|(_, path)| path).collect())
}

/// Все пути файлов внутри выбранных записей — для подсчёта объёма перед извлечением.
pub fn total_size(archive: &Path, inners: &[String]) -> Result<u64, String> {
    let contents = contents(archive)?;
    Ok(contents
        .items
        .iter()
        .filter(|i| {
            !i.is_dir && inners.iter().any(|inner| inside(&i.inner, inner.trim_matches('/')))
        })
        .map(|i| i.size)
        .sum())
}

/// Свободное имя: `a.txt`, `a (2).txt`, `a (3).txt`…
pub fn unique_path(path: &Path) -> PathBuf {
    if std::fs::symlink_metadata(path).is_err() {
        return path.to_path_buf();
    }
    let name = path.file_name().unwrap_or_default().to_string_lossy().into_owned();
    let (stem, ext) = match name.rfind('.') {
        Some(dot) if dot > 0 => (&name[..dot], &name[dot..]),
        _ => (name.as_str(), ""),
    };
    (2..)
        .map(|n| path.with_file_name(format!("{stem} ({n}){ext}")))
        .find(|candidate| std::fs::symlink_metadata(candidate).is_err())
        .unwrap_or_else(|| path.to_path_buf())
}

fn parent_of(inner: &str) -> &str {
    inner.rsplit_once('/').map_or("", |(parent, _)| parent)
}

fn name_of(inner: &str) -> &str {
    inner.rsplit_once('/').map_or(inner, |(_, name)| name)
}

/// `inner` — это `dir` или лежит в нём.
fn inside(inner: &str, dir: &str) -> bool {
    dir.is_empty()
        || inner == dir
        || inner.strip_prefix(dir).is_some_and(|rest| rest.starts_with('/'))
}

/// Имя из архива, безопасное для записи на диск. `None` — пропустить запись целиком.
fn safe_component(part: &str) -> Option<String> {
    if part.is_empty() || part == "." || part == ".." {
        return None;
    }
    let cleaned: String = part
        .chars()
        .map(|c| if c.is_control() || "<>:\"/\\|?*".contains(c) { '_' } else { c })
        .collect();
    // Windows отбрасывает точки и пробелы в конце имени — «a.» и «a» были бы одним файлом.
    let cleaned = cleaned.trim_end_matches(['.', ' ']).to_string();
    (!cleaned.is_empty()).then_some(cleaned)
}

/// Путь записи: `/` вместо `\`, без `.` и пустых частей. Записи с `..` пропускаются.
fn normalize(name: &str) -> Option<String> {
    let mut parts = Vec::new();
    for part in name.split(['/', '\\']) {
        match part {
            "" | "." => {}
            ".." => return None,
            part => parts.push(part),
        }
    }
    (!parts.is_empty()).then(|| parts.join("/"))
}

fn contents(archive: &Path) -> Result<Arc<Contents>, String> {
    let file = resolve(archive)?;
    let meta = std::fs::metadata(&file).map_err(|e| format!("{}: {e}", archive.display()))?;
    let key: Key = (archive.to_path_buf(), meta.len(), meta.modified().ok());
    if let Some((_, contents)) = CACHE.lock().iter().find(|(k, _)| *k == key) {
        return Ok(contents.clone());
    }
    let name = archive.file_name().unwrap_or_default().to_string_lossy();
    let kind = match mh_files_core::entry::extension_of(&name).as_str() {
        "7z" => Kind::Seven,
        "rar" => {
            Kind::Tool(crate::archive_tool::tool().cloned().ok_or(crate::archive_tool::MISSING)?)
        }
        _ => Kind::Zip,
    };
    let mut items = match &kind {
        Kind::Zip => read_zip(&file)?,
        Kind::Seven => read_seven(&file)?,
        Kind::Tool(tool) => read_tool(tool, &file)?,
    };
    add_missing_dirs(&mut items);
    let contents = Arc::new(Contents { kind, file, items });
    let mut cache = CACHE.lock();
    cache.retain(|(k, _)| k.0 != key.0);
    cache.push((key, contents.clone()));
    if cache.len() > CACHE_SIZE {
        cache.remove(0);
    }
    Ok(contents)
}

/// Папки, которые есть только в путях файлов (`a/b/c.txt` без записи `a/`).
fn add_missing_dirs(items: &mut Vec<Item>) {
    let known: HashSet<String> =
        items.iter().filter(|i| i.is_dir).map(|i| i.inner.clone()).collect();
    let mut missing = HashSet::new();
    for item in items.iter() {
        let mut parent = parent_of(&item.inner);
        while !parent.is_empty() {
            if known.contains(parent) || !missing.insert(parent.to_string()) {
                break;
            }
            parent = parent_of(parent);
        }
    }
    items.extend(missing.into_iter().map(|inner| Item {
        inner,
        raw: String::new(),
        index: usize::MAX,
        is_dir: true,
        size: 0,
        modified: None,
    }));
}

fn open_zip(archive: &Path) -> Result<zip::ZipArchive<std::io::BufReader<File>>, String> {
    let file = File::open(archive).map_err(|e| format!("{}: {e}", archive.display()))?;
    zip::ZipArchive::new(std::io::BufReader::new(file)).map_err(zip_error)
}

fn read_zip(archive: &Path) -> Result<Vec<Item>, String> {
    let mut zip = open_zip(archive)?;
    let mut items = Vec::with_capacity(zip.len());
    let mut seen = HashSet::new();
    for index in 0..zip.len() {
        let file = zip.by_index_raw(index).map_err(zip_error)?;
        let raw = file.name().to_string();
        let Some(inner) = normalize(&zip_name(file.name_raw(), &raw)) else { continue };
        if !seen.insert(inner.clone()) {
            continue;
        }
        items.push(Item {
            inner,
            raw,
            index,
            is_dir: file.is_dir(),
            size: if file.is_dir() { 0 } else { file.size() },
            modified: file.last_modified().and_then(|t| {
                local_time(t.year(), t.month(), t.day(), t.hour(), t.minute(), t.second())
            }),
        });
    }
    Ok(items)
}

/// Имя в zip: UTF-8, а без него — кодировка DOS. Русский zip из Проводника — это CP866, а не
/// CP437, которую подставляет крейт.
fn zip_name(raw: &[u8], decoded: &str) -> String {
    match std::str::from_utf8(raw) {
        Ok(text) => text.to_string(),
        Err(_) if raw.iter().all(u8::is_ascii) => decoded.to_string(),
        Err(_) => encoding_rs::IBM866.decode_without_bom_handling(raw).0.into_owned(),
    }
}

fn local_time(
    year: u16,
    month: u8,
    day: u8,
    hour: u8,
    minute: u8,
    second: u8,
) -> Option<SystemTime> {
    use chrono::TimeZone;
    let date = chrono::NaiveDate::from_ymd_opt(year.into(), month.into(), day.into())?;
    let time = date.and_hms_opt(hour.into(), minute.into(), second.into())?;
    let local = chrono::Local.from_local_datetime(&time).earliest()?;
    Some(local.into())
}

fn open_seven(archive: &Path) -> Result<sevenz_rust2::ArchiveReader<File>, String> {
    sevenz_rust2::ArchiveReader::open(archive, sevenz_rust2::Password::empty()).map_err(seven_error)
}

fn read_seven(archive: &Path) -> Result<Vec<Item>, String> {
    let header = sevenz_rust2::Archive::open(archive).map_err(seven_error)?;
    let mut items = Vec::with_capacity(header.files.len());
    let mut seen = HashSet::new();
    for (index, entry) in header.files.iter().enumerate() {
        if entry.is_anti_item() {
            continue;
        }
        let Some(inner) = normalize(entry.name()) else { continue };
        if !seen.insert(inner.clone()) {
            continue;
        }
        items.push(Item {
            inner,
            raw: entry.name().to_string(),
            index,
            is_dir: entry.is_directory(),
            size: if entry.is_directory() { 0 } else { entry.size() },
            modified: entry
                .has_last_modified_date
                .then(|| SystemTime::from(entry.last_modified_date())),
        });
    }
    Ok(items)
}

fn read_tool(tool: &crate::archive_tool::Tool, archive: &Path) -> Result<Vec<Item>, String> {
    let mut items = Vec::new();
    let mut seen = HashSet::new();
    for (index, raw) in crate::archive_tool::list(tool, archive)?.into_iter().enumerate() {
        let Some(inner) = normalize(&raw.name) else { continue };
        if !seen.insert(inner.clone()) {
            continue;
        }
        items.push(Item {
            inner,
            raw: raw.name,
            index,
            is_dir: raw.is_dir,
            size: if raw.is_dir { 0 } else { raw.size },
            modified: raw.modified,
        });
    }
    Ok(items)
}

fn zip_error(error: zip::result::ZipError) -> String {
    match error {
        zip::result::ZipError::UnsupportedArchive(zip::result::ZipError::PASSWORD_REQUIRED) => {
            "архив зашифрован — откройте его в архиваторе".into()
        }
        zip::result::ZipError::UnsupportedArchive(what) => {
            format!("архив не поддерживается: {what}")
        }
        zip::result::ZipError::InvalidArchive(_) => "архив повреждён или это не zip".into(),
        other => format!("архив не прочитан: {other}"),
    }
}

fn seven_error(error: sevenz_rust2::Error) -> String {
    match error {
        sevenz_rust2::Error::PasswordRequired | sevenz_rust2::Error::MaybeBadPassword(_) => {
            "архив зашифрован — откройте его в архиваторе".into()
        }
        sevenz_rust2::Error::BadSignature(_) => "архив повреждён или это не 7z".into(),
        other => format!("архив не прочитан: {other}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("mh-files-archive-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn make_zip(path: &Path) {
        use zip::write::SimpleFileOptions;
        let mut writer = zip::ZipWriter::new(File::create(path).unwrap());
        let options = SimpleFileOptions::default();
        writer.add_directory("empty/", options).unwrap();
        writer.start_file("docs/readme.txt", options).unwrap();
        writer.write_all(b"hello").unwrap();
        writer.start_file("docs/deep/data.bin", options).unwrap();
        writer.write_all(&[7u8; 1000]).unwrap();
        writer.start_file("../evil.txt", options).unwrap();
        writer.write_all(b"no").unwrap();
        writer.start_file("top.txt", options).unwrap();
        writer.write_all(b"top").unwrap();
        writer.finish().unwrap();
    }

    #[test]
    fn zip_lists_reads_and_extracts() {
        let dir = temp("zip");
        let archive = dir.join("a.zip");
        make_zip(&archive);

        let mut names: Vec<String> =
            list(&archive, "").unwrap().into_iter().map(|e| e.name).collect();
        names.sort();
        assert_eq!(names, ["docs", "empty", "top.txt"], "«..» отброшено, папка docs досоздана");
        let docs = list(&archive, "docs").unwrap();
        assert!(docs.iter().any(|e| e.name == "deep" && e.is_dir()));
        let readme = docs.iter().find(|e| e.name == "readme.txt").unwrap();
        assert_eq!(readme.path(), archive.join("docs").join("readme.txt"));
        assert!(list(&archive, "nothing").is_err());

        assert_eq!(read(&archive, "docs/readme.txt", 100).unwrap(), b"hello");
        assert!(read(&archive, "docs/deep/data.bin", 10).is_err(), "предел размера");
        assert_eq!(folder_totals(&archive, "docs").unwrap(), (1, 2, 1005));
        assert_eq!(total_size(&archive, &["docs".into()]).unwrap(), 1005);

        let path = archive.join("docs").join("readme.txt");
        assert_eq!(split(&path), Some((archive.clone(), "docs/readme.txt".into())));
        assert_eq!(split(&dir.join("plain.txt")), None);

        let out = dir.join("out");
        let created = extract(
            &archive,
            &["docs".into(), "top.txt".into()],
            &out,
            &CancelToken::default(),
            |_| {},
        )
        .unwrap();
        assert_eq!(created, [out.join("docs"), out.join("top.txt")]);
        assert_eq!(std::fs::read(out.join("docs/deep/data.bin")).unwrap().len(), 1000);
        // Второй раз — рядом, ничего не перезаписано.
        let again =
            extract(&archive, &["top.txt".into()], &out, &CancelToken::default(), |_| {}).unwrap();
        assert_eq!(again, [out.join("top (2).txt")]);
        let whole = dir.join("whole");
        let mut all =
            extract(&archive, &[String::new()], &whole, &CancelToken::default(), |_| {}).unwrap();
        all.sort();
        assert_eq!(all, [whole.join("docs"), whole.join("empty"), whole.join("top.txt")]);
        assert!(!dir.join("evil.txt").exists());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn seven_zip_round_trip() {
        let dir = temp("7z");
        let source = dir.join("src");
        std::fs::create_dir_all(source.join("inner")).unwrap();
        std::fs::write(source.join("inner/note.txt"), "семь").unwrap();
        std::fs::write(source.join("root.txt"), "root").unwrap();
        let archive = dir.join("b.7z");
        sevenz_rust2::compress_to_path(&source, &archive).unwrap();

        let mut names: Vec<String> =
            list(&archive, "").unwrap().into_iter().map(|e| e.name).collect();
        names.sort();
        assert_eq!(names, ["inner", "root.txt"]);
        assert_eq!(read(&archive, "inner/note.txt", 100).unwrap(), "семь".as_bytes());
        let out = dir.join("out");
        extract(&archive, &["inner".into()], &out, &CancelToken::default(), |_| {}).unwrap();
        assert_eq!(std::fs::read_to_string(out.join("inner/note.txt")).unwrap(), "семь");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn nested_archives_open_like_folders() {
        use zip::write::SimpleFileOptions;
        let dir = temp("nested");
        let inner_zip = dir.join("inner.zip");
        make_zip(&inner_zip);
        let source = dir.join("src");
        std::fs::create_dir_all(&source).unwrap();
        std::fs::write(source.join("deep.txt"), "глубоко").unwrap();
        let inner_seven = dir.join("inner.7z");
        sevenz_rust2::compress_to_path(&source, &inner_seven).unwrap();
        let outer = dir.join("outer.zip");
        {
            let mut writer = zip::ZipWriter::new(File::create(&outer).unwrap());
            let options = SimpleFileOptions::default();
            writer.start_file("sub/inner.zip", options).unwrap();
            writer.write_all(&std::fs::read(&inner_zip).unwrap()).unwrap();
            writer.start_file("inner.7z", options).unwrap();
            writer.write_all(&std::fs::read(&inner_seven).unwrap()).unwrap();
            writer.finish().unwrap();
        }
        let nested = outer.join("sub").join("inner.zip");
        let mut names: Vec<String> =
            list(&nested, "").unwrap().into_iter().map(|e| e.name).collect();
        names.sort();
        assert_eq!(names, ["docs", "empty", "top.txt"]);
        assert_eq!(read(&nested, "docs/readme.txt", 100).unwrap(), b"hello");
        let through = nested.join("docs").join("readme.txt");
        assert_eq!(split(&through), Some((nested.clone(), "docs/readme.txt".into())));
        assert_eq!(split(&nested), Some((outer.clone(), "sub/inner.zip".into())));
        let seven = outer.join("inner.7z");
        assert_eq!(read(&seven, "deep.txt", 100).unwrap(), "глубоко".as_bytes());
        let out = dir.join("out");
        extract(&nested, &["top.txt".into()], &out, &CancelToken::default(), |_| {}).unwrap();
        assert_eq!(std::fs::read(out.join("top.txt")).unwrap(), b"top");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// rar читает внешняя программа; настоящего rar здесь не собрать, но 7-Zip и bsdtar
    /// узнают формат по содержимому — zip под именем .rar проверяет всю дорогу.
    #[test]
    fn rar_goes_through_external_tool() {
        if crate::archive_tool::tool().is_none() {
            return;
        }
        let dir = temp("rar");
        let archive = dir.join("a.rar");
        make_zip(&archive);
        let mut names: Vec<String> =
            list(&archive, "").unwrap().into_iter().map(|e| e.name).collect();
        names.sort();
        assert_eq!(names, ["docs", "empty", "top.txt"]);
        assert_eq!(read(&archive, "docs/readme.txt", 100).unwrap(), b"hello");
        assert!(read(&archive, "docs/deep/data.bin", 10).is_err(), "предел размера");
        let out = dir.join("out");
        extract(&archive, &["docs".into()], &out, &CancelToken::default(), |_| {}).unwrap();
        assert_eq!(std::fs::read(out.join("docs/deep/data.bin")).unwrap().len(), 1000);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn names_are_sanitized() {
        assert_eq!(normalize("a\\b/./c"), Some("a/b/c".into()));
        assert_eq!(normalize("a/../b"), None);
        assert_eq!(safe_component("con:x?"), Some("con_x_".into()));
        assert_eq!(safe_component("name. "), Some("name".into()));
        assert_eq!(safe_component(".."), None);
        assert_eq!(zip_name(&[0x8f, 0xe0, 0xa8, 0xa2, 0xa5, 0xe2], ""), "Привет");
    }
}
