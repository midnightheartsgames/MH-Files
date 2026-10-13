//! Скачивание модели теггера и ONNX Runtime — один раз, по согласию пользователя (включил
//! автотеги). Файл скачивается в `.part` и переименовывается в конце, так что файл на месте —
//! значит, скачан целиком.
//!
//! ```text
//! <папка>\models\SmilingWolf--wd-eva02-large-tagger-v3\model.onnx, selected_tags.csv
//! <папка>\runtime\1.31.0\onnxruntime.dll, onnxruntime_providers_shared.dll
//! ```

use std::path::{Path, PathBuf};

use mh_files_core::settings::TagSettings;
use mh_files_core::tags::{
    self, MODEL_FILE, RUNTIME_FILES, RUNTIME_HOST, RUNTIME_VERSION, TAGS_FILE,
};

/// Готовые к работе пути.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Paths {
    /// Библиотека ONNX Runtime.
    pub runtime: PathBuf,
    pub model: PathBuf,
    pub tags: PathBuf,
}

/// Ход скачивания: что, сколько из скольких; вернуть `false` — прервать.
pub type Progress<'a> = &'a mut dyn FnMut(&str, u64, u64) -> bool;

fn model_dir(dir: &Path, settings: &TagSettings) -> PathBuf {
    dir.join("models").join(tags::model_folder(&settings.model))
}

fn runtime_dir(dir: &Path) -> PathBuf {
    dir.join("runtime").join(RUNTIME_VERSION)
}

/// Скачанная библиотека ONNX Runtime или своя из настроек.
fn runtime_lib(dir: &Path, settings: &TagSettings) -> PathBuf {
    if settings.runtime.as_os_str().is_empty() {
        runtime_dir(dir).join(RUNTIME_FILES[0])
    } else {
        settings.runtime.clone()
    }
}

/// Пути, если всё на месте.
pub fn ready(dir: &Path, settings: &TagSettings) -> Option<Paths> {
    let models = model_dir(dir, settings);
    let paths = Paths {
        runtime: runtime_lib(dir, settings),
        model: models.join(MODEL_FILE),
        tags: models.join(TAGS_FILE),
    };
    (paths.runtime.is_file() && paths.model.is_file() && paths.tags.is_file()).then_some(paths)
}

/// Скачать недостающее. Итог — пути для запуска.
pub fn ensure(dir: &Path, settings: &TagSettings, progress: Progress) -> Result<Paths, String> {
    if let Some(paths) = ready(dir, settings) {
        return Ok(paths);
    }
    let runtime = runtime_lib(dir, settings);
    if !runtime.is_file() {
        if !settings.runtime.as_os_str().is_empty() {
            return Err(format!("нет файла {}", settings.runtime.display()));
        }
        if !cfg!(windows) {
            return Err(
                "ONNX Runtime не найден: укажите путь к libonnxruntime в настройках тегов".into()
            );
        }
        let zip = dir.join("runtime").join(format!("onnxruntime-{RUNTIME_VERSION}.zip"));
        mh_files_platform::net::download(
            RUNTIME_HOST,
            &tags::runtime_path(),
            &zip,
            &mut |done, total| progress("ONNX Runtime", done, total.unwrap_or(0)),
        )
        .map_err(|e| format!("ONNX Runtime не скачан: {e}"))?;
        let unpacked = unpack(&zip, &runtime_dir(dir));
        let _ = std::fs::remove_file(&zip);
        unpacked?;
        // Прежние сборки — долой.
        if let Ok(entries) = std::fs::read_dir(dir.join("runtime")) {
            for entry in entries.flatten() {
                if entry.file_name() != RUNTIME_VERSION && entry.path().is_dir() {
                    let _ = std::fs::remove_dir_all(entry.path());
                }
            }
        }
    }
    let models = model_dir(dir, settings);
    // Сначала маленький список тегов: не тот репозиторий — ошибка сразу, а не после гигабайта.
    for (file, what) in [(TAGS_FILE, "список тегов"), (MODEL_FILE, "модель")] {
        let dest = models.join(file);
        if dest.is_file() {
            continue;
        }
        let path = format!("/{}/resolve/main/{file}", settings.model);
        mh_files_platform::net::download("huggingface.co", &path, &dest, &mut |done, total| {
            progress(what, done, total.unwrap_or(0))
        })
        .map_err(|e| format!("{file} из {} не скачан: {e}", settings.model))?;
    }
    // Прежние модели больше не нужны: гигабайт на диске.
    let keep = tags::model_folder(&settings.model);
    if let Ok(entries) = std::fs::read_dir(dir.join("models")) {
        for entry in entries.flatten() {
            if entry.file_name().to_string_lossy() != keep && entry.path().is_dir() {
                let _ = std::fs::remove_dir_all(entry.path());
            }
        }
    }
    ready(dir, settings).ok_or_else(|| "файлы модели не найдены после скачивания".to_string())
}

/// Достать из архива ONNX Runtime только нужные библиотеки ([`RUNTIME_FILES`]), где бы в
/// архиве они ни лежали.
pub fn unpack(zip: &Path, target: &Path) -> Result<(), String> {
    let file = std::fs::File::open(zip).map_err(|e| e.to_string())?;
    let mut archive = zip::ZipArchive::new(std::io::BufReader::new(file))
        .map_err(|e| format!("архив ONNX Runtime не читается: {e}"))?;
    let temp = target.with_extension("unpacking");
    let _ = std::fs::remove_dir_all(&temp);
    std::fs::create_dir_all(&temp).map_err(|e| e.to_string())?;
    let mut found = 0;
    for index in 0..archive.len() {
        let mut entry = archive.by_index(index).map_err(|e| e.to_string())?;
        let Some(name) = entry.enclosed_name().and_then(|p| p.file_name().map(|n| n.to_owned()))
        else {
            continue;
        };
        if entry.is_dir() || !RUNTIME_FILES.iter().any(|f| name.eq_ignore_ascii_case(f)) {
            continue;
        }
        let mut out = std::fs::File::create(temp.join(&name)).map_err(|e| e.to_string())?;
        std::io::copy(&mut entry, &mut out).map_err(|e| format!("распаковка: {e}"))?;
        found += 1;
    }
    if !temp.join(RUNTIME_FILES[0]).is_file() || found < RUNTIME_FILES.len() {
        let _ = std::fs::remove_dir_all(&temp);
        return Err(format!("в архиве ONNX Runtime нет {}", RUNTIME_FILES.join(", ")));
    }
    let _ = std::fs::remove_dir_all(target);
    std::fs::rename(&temp, target).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    use super::*;

    #[test]
    fn unpacks_only_the_libraries() {
        let dir = tempfile::tempdir().unwrap();
        let zip_path = dir.path().join("ort.zip");
        let mut zip = zip::ZipWriter::new(std::fs::File::create(&zip_path).unwrap());
        for (name, body) in [
            ("onnxruntime-win-x64-1.31.0/lib/onnxruntime.dll", "dll"),
            ("onnxruntime-win-x64-1.31.0/lib/onnxruntime_providers_shared.dll", "shared"),
            ("onnxruntime-win-x64-1.31.0/lib/onnxruntime.pdb", "pdb"),
            ("../onnxruntime.dll", "evil"),
        ] {
            zip.start_file(name, zip::write::SimpleFileOptions::default()).unwrap();
            zip.write_all(body.as_bytes()).unwrap();
        }
        zip.finish().unwrap();
        let target = dir.path().join("runtime").join("1.31.0");
        unpack(&zip_path, &target).unwrap();
        assert_eq!(std::fs::read_to_string(target.join("onnxruntime.dll")).unwrap(), "dll");
        assert!(target.join("onnxruntime_providers_shared.dll").is_file());
        assert!(!target.join("onnxruntime.pdb").exists(), "отладочное не нужно");
        assert!(!dir.path().join("onnxruntime.dll").exists());
        let empty = dir.path().join("empty.zip");
        zip::ZipWriter::new(std::fs::File::create(&empty).unwrap()).finish().unwrap();
        assert!(unpack(&empty, &dir.path().join("x")).is_err());
        assert!(!dir.path().join("x.unpacking").exists());
    }

    #[test]
    fn ready_files_need_no_network() {
        let dir = tempfile::tempdir().unwrap();
        let runtime = dir.path().join("libonnxruntime.so");
        let settings = TagSettings { runtime: runtime.clone(), ..TagSettings::default() };
        assert!(ready(dir.path(), &settings).is_none());
        std::fs::write(&runtime, "").unwrap();
        let models = model_dir(dir.path(), &settings);
        std::fs::create_dir_all(&models).unwrap();
        std::fs::write(models.join(MODEL_FILE), "m").unwrap();
        std::fs::write(models.join(TAGS_FILE), "t").unwrap();
        let paths =
            ensure(dir.path(), &settings, &mut |_, _, _| panic!("ничего не качается")).unwrap();
        assert_eq!(paths.runtime, runtime);
        assert_eq!(paths.model, models.join(MODEL_FILE));
        let missing = TagSettings { runtime: dir.path().join("нет"), ..settings };
        assert!(ensure(dir.path(), &missing, &mut |_, _, _| true).unwrap_err().contains("нет"));
    }
}
