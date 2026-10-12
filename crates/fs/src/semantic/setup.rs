//! Скачивание модели и llama.cpp — один раз, по согласию пользователя (включил поиск по
//! смыслу). Что скачано, записано в `installed.json`: при следующем запуске сеть не нужна.
//!
//! ```text
//! <папка>\models\<модель>.gguf, mmproj-….gguf
//! <папка>\llama\b11530\llama-server.exe, *.dll
//! <папка>\installed.json
//! ```

use std::path::{Path, PathBuf};

use mh_files_core::semantic::model::{self, Download};
use mh_files_core::settings::SemanticSettings;
use serde::{Deserialize, Serialize};

/// Что уже скачано.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Installed {
    pub repo: String,
    pub quant: String,
    /// Имена файлов в `models`.
    pub model: String,
    pub mmproj: Option<String>,
    /// Сборка llama.cpp — папка в `llama`.
    pub runtime: Option<String>,
}

/// Готовые к работе пути.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Paths {
    pub exe: PathBuf,
    pub model: PathBuf,
    pub mmproj: Option<PathBuf>,
}

impl Installed {
    pub fn load(dir: &Path) -> Installed {
        std::fs::read_to_string(dir.join("installed.json"))
            .ok()
            .and_then(|text| serde_json::from_str(&text).ok())
            .unwrap_or_default()
    }

    fn save(&self, dir: &Path) -> Result<(), String> {
        let text = serde_json::to_string_pretty(self).map_err(|e| e.to_string())?;
        let path = dir.join("installed.json");
        let temp = dir.join("installed.json.tmp");
        std::fs::write(&temp, text)
            .and_then(|_| std::fs::rename(&temp, &path))
            .map_err(|e| format!("installed.json не записан: {e}"))
    }

    /// Модель с этими настройками уже на диске.
    pub fn model_ready(&self, dir: &Path, settings: &SemanticSettings) -> bool {
        self.repo == settings.model_repo
            && self.quant.eq_ignore_ascii_case(&settings.quant)
            && !self.model.is_empty()
            && dir.join("models").join(&self.model).is_file()
            && self.mmproj.as_ref().is_none_or(|m| dir.join("models").join(m).is_file())
    }

    /// Скачанный `llama-server`.
    pub fn runtime_exe(&self, dir: &Path) -> Option<PathBuf> {
        let name = if cfg!(windows) { "llama-server.exe" } else { "llama-server" };
        self.runtime.as_ref().map(|tag| dir.join("llama").join(tag).join(name))
    }

    /// Пути, если всё на месте. Без мультимодальной части (`mmproj`) — только текст.
    pub fn paths(&self, dir: &Path, settings: &SemanticSettings) -> Option<Paths> {
        if !self.model_ready(dir, settings) {
            return None;
        }
        let exe = super::server::find_exe(&settings.server, self.runtime_exe(dir))?;
        let want_media = settings.images || settings.audio || settings.documents;
        Some(Paths {
            exe,
            model: dir.join("models").join(&self.model),
            mmproj: self.mmproj.as_ref().filter(|_| want_media).map(|m| dir.join("models").join(m)),
        })
    }
}

/// Ход скачивания: что, сколько из скольких; вернуть `false` — прервать.
pub type Progress<'a> = &'a mut dyn FnMut(&str, u64, u64) -> bool;

/// Скачать недостающее. Итог — пути для запуска.
pub fn ensure(
    dir: &Path,
    settings: &SemanticSettings,
    progress: Progress,
) -> Result<Paths, String> {
    let mut installed = Installed::load(dir);
    if let Some(paths) = installed.paths(dir, settings) {
        return Ok(paths);
    }
    std::fs::create_dir_all(dir.join("models"))
        .map_err(|e| format!("папка модели не создана: {e}"))?;
    if !installed.model_ready(dir, settings) {
        let api = mh_files_platform::net::get(
            "huggingface.co",
            &model::repo_api_path(&settings.model_repo),
            &[],
            8 << 20,
        )
        .map_err(|e| format!("Hugging Face недоступен: {e}"))?;
        let files = model::choose_model_files(
            &String::from_utf8_lossy(&api),
            &settings.model_repo,
            &settings.quant,
        )?;
        let total = files.model.size + files.mmproj.as_ref().map_or(0, |m| m.size);
        let mut before = 0;
        for file in std::iter::once(&files.model).chain(files.mmproj.as_ref()) {
            let dest = dir.join("models").join(&file.name);
            if dest.is_file()
                && (file.size == 0 || dest.metadata().map(|m| m.len()).ok() == Some(file.size))
            {
                before += file.size;
                continue;
            }
            let done = fetch(file, &dest, &mut |done, size| {
                let total = if total > 0 { total } else { size };
                progress("модель", before + done, total)
            })?;
            before += done;
        }
        // Прежние файлы другой модели больше не нужны.
        for old in [Some(&installed.model), installed.mmproj.as_ref()].into_iter().flatten() {
            if !old.is_empty()
                && *old != files.model.name
                && Some(old) != files.mmproj.as_ref().map(|m| &m.name)
            {
                let _ = std::fs::remove_file(dir.join("models").join(old));
            }
        }
        installed.repo = settings.model_repo.clone();
        installed.quant = settings.quant.clone();
        installed.model = files.model.name;
        installed.mmproj = files.mmproj.map(|m| m.name);
        installed.save(dir)?;
    }
    if super::server::find_exe(&settings.server, installed.runtime_exe(dir)).is_none() {
        if !settings.server.as_os_str().is_empty() {
            return Err(format!("нет файла {}", settings.server.display()));
        }
        if !cfg!(windows) {
            return Err(
                "llama-server не найден: поставьте llama.cpp или укажите путь в настройках".into(),
            );
        }
        let api = mh_files_platform::net::get(
            "api.github.com",
            &model::releases_api_path(),
            &[("Accept", "application/vnd.github+json"), ("X-GitHub-Api-Version", "2022-11-28")],
            16 << 20,
        )
        .map_err(|e| format!("GitHub недоступен: {e}"))?;
        let (tag, file) = model::choose_runtime(&String::from_utf8_lossy(&api))?;
        let zip = dir.join("llama").join(&file.name);
        fetch(&file, &zip, &mut |done, size| progress("llama.cpp", done, size))?;
        let target = dir.join("llama").join(&tag);
        let unpacked = unpack(&zip, &target);
        let _ = std::fs::remove_file(&zip);
        unpacked?;
        // Прежние сборки — долой.
        if let Some(old) = installed.runtime.as_ref().filter(|old| **old != tag) {
            let _ = std::fs::remove_dir_all(dir.join("llama").join(old));
        }
        installed.runtime = Some(tag);
        installed.save(dir)?;
    }
    installed.paths(dir, settings).ok_or_else(|| "файлы модели не найдены после скачивания".into())
}

fn fetch(
    file: &Download,
    dest: &Path,
    progress: &mut dyn FnMut(u64, u64) -> bool,
) -> Result<u64, String> {
    let (host, path) =
        model::split_url(&file.url).ok_or_else(|| format!("странный адрес {}", file.url))?;
    mh_files_platform::net::download(host, path, dest, &mut |done, total| {
        progress(done, total.unwrap_or(file.size).max(file.size))
    })
    .map_err(|e| format!("{} не скачан: {e}", file.name))
}

/// Распаковать zip llama.cpp в `target`: всё содержимое, включая вложенные папки. Имена с
/// `..` и абсолютные пути пропускаются.
pub fn unpack(zip: &Path, target: &Path) -> Result<(), String> {
    let file = std::fs::File::open(zip).map_err(|e| e.to_string())?;
    let mut archive = zip::ZipArchive::new(std::io::BufReader::new(file))
        .map_err(|e| format!("архив llama.cpp не читается: {e}"))?;
    let temp = target.with_extension("unpacking");
    let _ = std::fs::remove_dir_all(&temp);
    for index in 0..archive.len() {
        let mut entry = archive.by_index(index).map_err(|e| e.to_string())?;
        let Some(relative) = entry.enclosed_name() else { continue };
        let path = temp.join(relative);
        if entry.is_dir() {
            std::fs::create_dir_all(&path).map_err(|e| e.to_string())?;
            continue;
        }
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        let mut out = std::fs::File::create(&path).map_err(|e| e.to_string())?;
        std::io::copy(&mut entry, &mut out).map_err(|e| format!("распаковка: {e}"))?;
    }
    // Архив с одной папкой внутри — её содержимое и есть сборка.
    let name = if cfg!(windows) { "llama-server.exe" } else { "llama-server" };
    let root = if temp.join(name).is_file() {
        temp.clone()
    } else {
        std::fs::read_dir(&temp)
            .map_err(|e| e.to_string())?
            .flatten()
            .map(|e| e.path())
            .find(|p| p.join(name).is_file())
            .ok_or_else(|| {
                let _ = std::fs::remove_dir_all(&temp);
                format!("в архиве llama.cpp нет {name}")
            })?
    };
    let _ = std::fs::remove_dir_all(target);
    std::fs::rename(&root, target).map_err(|e| e.to_string())?;
    let _ = std::fs::remove_dir_all(&temp);
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    use super::*;

    fn zip_with(path: &Path, files: &[(&str, &str)]) {
        let mut zip = zip::ZipWriter::new(std::fs::File::create(path).unwrap());
        for (name, body) in files {
            zip.start_file(*name, zip::write::SimpleFileOptions::default()).unwrap();
            zip.write_all(body.as_bytes()).unwrap();
        }
        zip.finish().unwrap();
    }

    #[test]
    fn unpacks_flat_and_nested_archives() {
        let dir = tempfile::tempdir().unwrap();
        let exe = if cfg!(windows) { "llama-server.exe" } else { "llama-server" };
        let flat = dir.path().join("flat.zip");
        zip_with(&flat, &[(exe, "exe"), ("ggml-cpu-haswell.dll", "dll"), ("../evil.txt", "x")]);
        let target = dir.path().join("llama").join("b1");
        unpack(&flat, &target).unwrap();
        assert!(target.join(exe).is_file() && target.join("ggml-cpu-haswell.dll").is_file());
        assert!(!dir.path().join("llama").join("evil.txt").exists());
        let nested = dir.path().join("nested.zip");
        zip_with(&nested, &[(&format!("build/bin/{exe}"), "exe2")]);
        let target2 = dir.path().join("llama").join("b2");
        assert!(unpack(&nested, &target2).is_err(), "сервер глубже одной папки — не сборка");
        let one = dir.path().join("one.zip");
        zip_with(&one, &[(&format!("bin/{exe}"), "exe3")]);
        unpack(&one, &target2).unwrap();
        assert_eq!(std::fs::read_to_string(target2.join(exe)).unwrap(), "exe3");
        let empty = dir.path().join("empty.zip");
        zip_with(&empty, &[("readme.txt", "")]);
        assert!(unpack(&empty, &dir.path().join("llama").join("b3")).is_err());
        assert!(!dir.path().join("llama").join("b3.unpacking").exists());
    }

    #[test]
    fn installed_paths() {
        let dir = tempfile::tempdir().unwrap();
        let settings = SemanticSettings::default();
        let installed = Installed {
            repo: settings.model_repo.clone(),
            quant: "q8_0".into(),
            model: "m.gguf".into(),
            mmproj: Some("mmproj.gguf".into()),
            runtime: Some("b11530".into()),
        };
        assert!(!installed.model_ready(dir.path(), &settings), "файлов ещё нет");
        std::fs::create_dir_all(dir.path().join("models")).unwrap();
        std::fs::write(dir.path().join("models/m.gguf"), "m").unwrap();
        std::fs::write(dir.path().join("models/mmproj.gguf"), "p").unwrap();
        assert!(installed.model_ready(dir.path(), &settings));
        let other = SemanticSettings { quant: "Q4_K_M".into(), ..settings.clone() };
        assert!(!installed.model_ready(dir.path(), &other), "другое квантование — скачать");
        let exe = installed.runtime_exe(dir.path()).unwrap();
        std::fs::create_dir_all(exe.parent().unwrap()).unwrap();
        std::fs::write(&exe, "").unwrap();
        let paths = installed.paths(dir.path(), &settings).unwrap();
        assert_eq!(paths.exe, exe);
        assert_eq!(paths.mmproj, Some(dir.path().join("models/mmproj.gguf")));
        let text_only =
            SemanticSettings { images: false, audio: false, documents: false, ..settings.clone() };
        assert_eq!(installed.paths(dir.path(), &text_only).unwrap().mmproj, None);
        installed.save(dir.path()).unwrap();
        assert_eq!(Installed::load(dir.path()), installed);
        // `ensure` с готовыми файлами в сеть не ходит.
        let ready =
            ensure(dir.path(), &settings, &mut |_, _, _| panic!("ничего не качается")).unwrap();
        assert_eq!(ready, paths);
    }
}
