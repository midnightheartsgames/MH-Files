//! Дописать файлы и папки в zip. Свой код, а не `IFileOperation`: копирование Shell в zip
//! (файл архива как папка назначения) отвечает «Неверно задано имя папки».
//!
//! Архив не правится на месте: копия рядом дописывается и подменяет его — оборвалось на
//! середине, архив остался прежним. Записи архива переносятся байт в байт, с прежними
//! именами (и в кодировке DOS у архивов Проводника). Занятое имя не перезаписывается:
//! новый файл ложится рядом с номером — «отчёт (2).docx»; папка с тем же именем дополняется.

use std::collections::HashSet;
use std::fs::{self, File};
use std::io::{self, BufReader};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, DateTime, ZipWriter};

use crate::CancelToken;

/// Что получилось: сколько файлов дописано и какие легли под другим именем.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Added {
    pub files: usize,
    /// `(было, стало)` — имена в архиве.
    pub renamed: Vec<(String, String)>,
}

/// Дописать `sources` в папку `inner` (через `/`, пусто — корень) архива `archive`.
pub fn add(
    archive: &Path,
    inner: &str,
    sources: &[PathBuf],
    cancel: &CancelToken,
) -> Result<Added, String> {
    let name = archive.file_name().unwrap_or_default().to_string_lossy().into_owned();
    let temp = archive.with_file_name(format!(".{name}.mh-files-tmp"));
    let result = write(archive, &temp, inner, sources, cancel);
    match result {
        Ok(added) if !cancel.is_cancelled() => {
            fs::rename(&temp, archive).map_err(|e| {
                let _ = fs::remove_file(&temp);
                format!("{}: архив занят другой программой? ({e})", archive.display())
            })?;
            Ok(added)
        }
        Ok(_) => {
            let _ = fs::remove_file(&temp);
            Ok(Added::default())
        }
        Err(error) => {
            let _ = fs::remove_file(&temp);
            Err(error)
        }
    }
}

fn write(
    archive: &Path,
    temp: &Path,
    inner: &str,
    sources: &[PathBuf],
    cancel: &CancelToken,
) -> Result<Added, String> {
    // Имена — как их показывает MH Files (у архивов Проводника — в кодировке DOS).
    let existing = crate::archive::zip_names(archive)?;
    fs::copy(archive, temp).map_err(|e| format!("{}: {e}", archive.display()))?;
    let file = File::options()
        .read(true)
        .write(true)
        .open(temp)
        .map_err(|e| format!("{}: {e}", temp.display()))?;
    let mut writer = ZipWriter::new_append(file)
        .map_err(|e| format!("{name}: {e}", name = archive.display()))?;
    // Занятые имена — без учёта регистра, как в Проводнике; папки — и неявные (`a/` из `a/b.txt`).
    let mut taken: HashSet<String> = HashSet::new();
    for name in &existing {
        let mut path = String::new();
        for part in name.split('/') {
            if !path.is_empty() {
                path.push('/');
            }
            path.push_str(part);
            taken.insert(path.to_lowercase());
        }
    }
    let base = inner.trim_matches('/');
    let mut added = Added::default();
    for source in sources {
        let Some(name) = source.file_name().map(|n| n.to_string_lossy().into_owned()) else {
            continue;
        };
        let wanted = join(base, &name);
        let meta =
            fs::symlink_metadata(source).map_err(|e| format!("{}: {e}", source.display()))?;
        if meta.is_dir() {
            // Папка с тем же именем дополняется, как в Проводнике.
            add_dir(&mut writer, source, &wanted, &mut taken, &mut added, cancel)?;
        } else {
            let target = free_name(&wanted, &taken);
            if target != wanted {
                added.renamed.push((wanted, target.clone()));
            }
            add_file(&mut writer, source, &target, &mut taken, cancel)?;
            added.files += 1;
        }
        if cancel.is_cancelled() {
            return Ok(added);
        }
    }
    writer.finish().map_err(|e| format!("{}: {e}", archive.display()))?;
    Ok(added)
}

fn add_dir(
    writer: &mut ZipWriter<File>,
    dir: &Path,
    name: &str,
    taken: &mut HashSet<String>,
    added: &mut Added,
    cancel: &CancelToken,
) -> Result<(), String> {
    if taken.insert(name.to_lowercase()) {
        let options = options(fs::metadata(dir).ok().and_then(|m| m.modified().ok()), 0);
        writer.add_directory(format!("{name}/"), options).map_err(|e| format!("{name}: {e}"))?;
    }
    let entries = fs::read_dir(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    for entry in entries.flatten() {
        if cancel.is_cancelled() {
            return Ok(());
        }
        let path = entry.path();
        let child = join(name, &entry.file_name().to_string_lossy());
        // Ссылки и junction не обходятся: за ними может быть что угодно, вплоть до цикла.
        let Ok(meta) = fs::symlink_metadata(&path) else { continue };
        if meta.is_dir() {
            add_dir(writer, &path, &child, taken, added, cancel)?;
        } else if meta.is_file() {
            let target = free_name(&child, taken);
            if target != child {
                added.renamed.push((child, target.clone()));
            }
            add_file(writer, &path, &target, taken, cancel)?;
            added.files += 1;
        }
    }
    Ok(())
}

fn add_file(
    writer: &mut ZipWriter<File>,
    path: &Path,
    name: &str,
    taken: &mut HashSet<String>,
    cancel: &CancelToken,
) -> Result<(), String> {
    let file = File::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let meta = file.metadata().map_err(|e| format!("{}: {e}", path.display()))?;
    writer
        .start_file(name, options(meta.modified().ok(), meta.len()))
        .map_err(|e| format!("{name}: {e}"))?;
    let mut reader = Cancellable { inner: BufReader::new(file), cancel };
    io::copy(&mut reader, writer).map_err(|e| format!("{}: {e}", path.display()))?;
    taken.insert(name.to_lowercase());
    Ok(())
}

/// Чтение, которое обрывается отменой.
struct Cancellable<'a, R> {
    inner: R,
    cancel: &'a CancelToken,
}

impl<R: io::Read> io::Read for Cancellable<'_, R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if self.cancel.is_cancelled() {
            return Err(io::Error::new(io::ErrorKind::Interrupted, "отменено"));
        }
        self.inner.read(buf)
    }
}

fn options(modified: Option<SystemTime>, size: u64) -> SimpleFileOptions {
    let mut options = SimpleFileOptions::default()
        .compression_method(CompressionMethod::Deflated)
        .large_file(size >= u64::from(u32::MAX));
    if let Some(time) = modified.and_then(zip_time) {
        options = options.last_modified_time(time);
    }
    options
}

/// Время в zip — местное, с точностью до двух секунд.
fn zip_time(time: SystemTime) -> Option<DateTime> {
    use chrono::{Datelike, Timelike};
    let local: chrono::DateTime<chrono::Local> = time.into();
    DateTime::from_date_and_time(
        u16::try_from(local.year()).ok()?,
        local.month() as u8,
        local.day() as u8,
        local.hour() as u8,
        local.minute() as u8,
        local.second() as u8,
    )
    .ok()
}

fn join(base: &str, name: &str) -> String {
    if base.is_empty() { name.to_string() } else { format!("{base}/{name}") }
}

/// Свободное имя: `a/отчёт.docx`, `a/отчёт (2).docx`…
fn free_name(wanted: &str, taken: &HashSet<String>) -> String {
    if !taken.contains(&wanted.to_lowercase()) {
        return wanted.to_string();
    }
    let (dir, name) = match wanted.rsplit_once('/') {
        Some((dir, name)) => (format!("{dir}/"), name),
        None => (String::new(), wanted),
    };
    let (stem, ext) = match name.rfind('.').filter(|&dot| dot > 0) {
        Some(dot) => (&name[..dot], &name[dot..]),
        None => (name, ""),
    };
    (2..)
        .map(|n| format!("{dir}{stem} ({n}){ext}"))
        .find(|candidate| !taken.contains(&candidate.to_lowercase()))
        .expect("бесконечная последовательность")
}

#[cfg(test)]
mod tests {
    use std::io::{Read, Write};

    use super::*;

    fn names(archive: &Path) -> Vec<String> {
        let mut zip = zip::ZipArchive::new(File::open(archive).unwrap()).unwrap();
        let mut names: Vec<String> =
            (0..zip.len()).map(|i| zip.by_index(i).unwrap().name().to_string()).collect();
        names.sort();
        names
    }

    #[test]
    fn appends_files_and_folders_without_overwriting() {
        let dir = std::env::temp_dir().join(format!("mh-zip-write-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("src/photos/2026")).unwrap();
        let archive = dir.join("a.zip");
        {
            let mut zip = ZipWriter::new(File::create(&archive).unwrap());
            zip.start_file("отчёт.txt", SimpleFileOptions::default()).unwrap();
            zip.write_all(b"old").unwrap();
            zip.start_file("docs/readme.md", SimpleFileOptions::default()).unwrap();
            zip.write_all(b"readme").unwrap();
            zip.finish().unwrap();
        }
        fs::write(dir.join("src/отчёт.txt"), "new").unwrap();
        fs::write(dir.join("src/photos/2026/cat.jpg"), "cat").unwrap();
        fs::write(dir.join("src/note.txt"), "note").unwrap();

        let sources = [dir.join("src/отчёт.txt"), dir.join("src/photos"), dir.join("src/note.txt")];
        let added = add(&archive, "", &sources, &CancelToken::default()).unwrap();
        assert_eq!(added.files, 3);
        assert_eq!(added.renamed, [("отчёт.txt".to_string(), "отчёт (2).txt".to_string())]);
        // В папку архива.
        let added =
            add(&archive, "docs", &[dir.join("src/note.txt")], &CancelToken::default()).unwrap();
        assert_eq!(added.files, 1);
        assert_eq!(
            names(&archive),
            [
                "docs/note.txt",
                "docs/readme.md",
                "note.txt",
                "photos/",
                "photos/2026/",
                "photos/2026/cat.jpg",
                "отчёт (2).txt",
                "отчёт.txt",
            ]
        );
        let mut zip = zip::ZipArchive::new(File::open(&archive).unwrap()).unwrap();
        let mut text = String::new();
        zip.by_name("отчёт.txt").unwrap().read_to_string(&mut text).unwrap();
        assert_eq!(text, "old", "прежняя запись не тронута");
        text.clear();
        zip.by_name("отчёт (2).txt").unwrap().read_to_string(&mut text).unwrap();
        assert_eq!(text, "new");
        assert!(!dir.join(".a.zip.mh-files-tmp").exists(), "копия убрана");
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn broken_archive_stays_as_it_was() {
        let dir = std::env::temp_dir().join(format!("mh-zip-broken-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let archive = dir.join("bad.zip");
        fs::write(&archive, "not a zip").unwrap();
        fs::write(dir.join("x.txt"), "x").unwrap();
        assert!(add(&archive, "", &[dir.join("x.txt")], &CancelToken::default()).is_err());
        assert_eq!(fs::read_to_string(&archive).unwrap(), "not a zip");
        assert!(!dir.join(".bad.zip.mh-files-tmp").exists());
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn free_names_keep_the_extension() {
        let taken: HashSet<String> = ["a/x.txt", "a/x (2).txt", ".bashrc"].map(String::from).into();
        assert_eq!(free_name("a/X.TXT", &taken), "a/X (3).TXT");
        assert_eq!(free_name("a/y.txt", &taken), "a/y.txt");
        assert_eq!(free_name(".bashrc", &taken), ".bashrc (2)");
    }
}
