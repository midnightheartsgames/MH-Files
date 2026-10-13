//! Скачивание модели теггера и ONNX Runtime — один раз, по согласию пользователя (включил
//! автотеги). Модель скачивается в `.part` и переименовывается в конце, библиотеки пишутся во
//! временную папку и переносятся целиком, так что файл на месте — значит, скачан целиком.
//!
//! ```text
//! <папка>\models\SmilingWolf--wd-eva02-large-tagger-v3\model.onnx, selected_tags.csv
//! <папка>\models\Xenova--ast-finetuned-audioset-10-10-0.4593\config.json, onnx\model.onnx
//! <папка>\runtime\1.24.4-directml\onnxruntime.dll, onnxruntime_providers_shared.dll, DirectML.dll
//! ```

use std::io::Read;
use std::path::{Path, PathBuf};

use mh_files_core::settings::TagSettings;
use mh_files_core::sound::{SOUND_CONFIG, SOUND_MODELS, SOUND_REPO};
use mh_files_core::tags::{self, MODEL_FILE, NUGET_HOST, RUNTIME_FILES, RUNTIME_FOLDER, TAGS_FILE};

/// Готовые к работе пути.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Paths {
    /// Библиотека ONNX Runtime.
    pub runtime: PathBuf,
    pub model: PathBuf,
    pub tags: PathBuf,
    /// Модель звука и её классы — если звуковые теги включены.
    pub sound: Option<SoundPaths>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SoundPaths {
    pub model: PathBuf,
    pub config: PathBuf,
}

/// Ход скачивания: что, сколько из скольких; вернуть `false` — прервать.
pub type Progress<'a> = &'a mut dyn FnMut(&str, u64, u64) -> bool;

fn model_dir(dir: &Path, settings: &TagSettings) -> PathBuf {
    dir.join("models").join(tags::model_folder(&settings.model))
}

fn sound_dir(dir: &Path) -> PathBuf {
    dir.join("models").join(tags::model_folder(SOUND_REPO))
}

/// Файл модели звука из настроек (неизвестный — первый из готовых).
fn sound_file(settings: &TagSettings) -> &str {
    mh_files_core::sound::sound_model(&settings.sound_model)
        .map_or(SOUND_MODELS[0].file, |m| m.file)
}

fn sound_paths(dir: &Path, settings: &TagSettings) -> SoundPaths {
    let folder = sound_dir(dir);
    SoundPaths { model: folder.join(sound_file(settings)), config: folder.join(SOUND_CONFIG) }
}

fn runtime_dir(dir: &Path) -> PathBuf {
    dir.join("runtime").join(RUNTIME_FOLDER)
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
    let sound = sound_paths(dir, settings);
    let sound_ready = sound.model.is_file() && sound.config.is_file();
    if settings.sound && !sound_ready {
        return None;
    }
    let paths = Paths {
        runtime: runtime_lib(dir, settings),
        model: models.join(MODEL_FILE),
        tags: models.join(TAGS_FILE),
        sound: settings.sound.then_some(sound),
    };
    // Скачанная сборка — только вся: без DirectML.dll видеокарта не заработает.
    let runtime = if settings.runtime.as_os_str().is_empty() {
        RUNTIME_FILES.iter().all(|f| runtime_dir(dir).join(f).is_file())
    } else {
        paths.runtime.is_file()
    };
    (runtime && paths.model.is_file() && paths.tags.is_file()).then_some(paths)
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
        let target = runtime_dir(dir);
        let temp = target.with_extension("part");
        let _ = std::fs::remove_dir_all(&temp);
        std::fs::create_dir_all(&temp).map_err(|e| format!("папка не создана: {e}"))?;
        // Ход — скачанными байтами: сколько всего, заранее не известно (куски архивов).
        let mut loaded = 0u64;
        for (package, files) in &tags::runtime_packages() {
            let mut fetch = |range: &str| {
                if !progress("ONNX Runtime", loaded, 0) {
                    return Err("отменено".to_string());
                }
                let bytes =
                    mh_files_platform::net::get_range(NUGET_HOST, package, range, 64 << 20)?;
                loaded += bytes.len() as u64;
                Ok(bytes)
            };
            for (entry, name) in files.iter() {
                let bytes = zip_entry(&mut fetch, entry)
                    .map_err(|e| format!("ONNX Runtime не скачан ({entry}): {e}"))?;
                std::fs::write(temp.join(name), bytes).map_err(|e| format!("запись: {e}"))?;
            }
        }
        let _ = std::fs::remove_dir_all(&target);
        std::fs::rename(&temp, &target).map_err(|e| e.to_string())?;
        // Прежние сборки — долой.
        if let Ok(entries) = std::fs::read_dir(dir.join("runtime")) {
            for entry in entries.flatten() {
                if entry.file_name() != RUNTIME_FOLDER && entry.path().is_dir() {
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
    if settings.sound {
        let sound = sound_paths(dir, settings);
        let file = sound_file(settings);
        for (dest, remote, what) in
            [(&sound.config, SOUND_CONFIG, "классы звука"), (&sound.model, file, "модель звука")]
        {
            if dest.is_file() {
                continue;
            }
            let path = format!("/{SOUND_REPO}/resolve/main/{remote}");
            mh_files_platform::net::download("huggingface.co", &path, dest, &mut |done, total| {
                progress(what, done, total.unwrap_or(0))
            })
            .map_err(|e| format!("{remote} из {SOUND_REPO} не скачан: {e}"))?;
        }
        // Другой вариант модели звука больше не нужен.
        for other in SOUND_MODELS.iter().filter(|m| m.file != file) {
            let _ = std::fs::remove_file(sound_dir(dir).join(other.file));
        }
    }
    // Прежние модели больше не нужны: гигабайт на диске. Модель звука остаётся, даже если
    // звук выключен: включить его снова — без скачивания.
    let keep = [tags::model_folder(&settings.model), tags::model_folder(SOUND_REPO)];
    if let Ok(entries) = std::fs::read_dir(dir.join("models")) {
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if !keep.contains(&name) && entry.path().is_dir() {
                let _ = std::fs::remove_dir_all(entry.path());
            }
        }
    }
    ready(dir, settings).ok_or_else(|| "файлы модели не найдены после скачивания".to_string())
}

/// Достать файл `name` из zip на сервере, скачивая только нужное: `fetch` отдаёт кусок по
/// диапазону HTTP Range (`-65536` — последние байты, `100-199`). Сначала — конец архива с
/// оглавлением, потом — сам файл. Zip64 (архивы больше 4 ГБ) не нужен и не разбирается.
pub fn zip_entry(
    fetch: &mut dyn FnMut(&str) -> Result<Vec<u8>, String>,
    name: &str,
) -> Result<Vec<u8>, String> {
    let u16_at = |b: &[u8], at: usize| b.get(at..at + 2).map(|s| u16::from_le_bytes([s[0], s[1]]));
    let u32_at = |b: &[u8], at: usize| {
        b.get(at..at + 4).map(|s| u32::from_le_bytes([s[0], s[1], s[2], s[3]]))
    };
    let broken = || "архив не читается".to_string();
    let tail = fetch("-65536")?;
    let end = (0..tail.len().saturating_sub(21))
        .rev()
        .find(|&at| u32_at(&tail, at) == Some(0x0605_4b50))
        .ok_or("нет оглавления архива")?;
    let cd_size = u32_at(&tail, end + 12).ok_or_else(broken)? as usize;
    let cd_offset = u32_at(&tail, end + 16).ok_or_else(broken)? as usize;
    if cd_offset == u32::MAX as usize {
        return Err("архив zip64 не поддерживается".into());
    }
    // Оглавление обычно в том же конце; нет — отдельным куском.
    let base = (cd_offset + cd_size).checked_sub(end).ok_or_else(broken)?;
    let directory = if cd_offset >= base {
        tail.get(cd_offset - base..cd_offset - base + cd_size).ok_or_else(broken)?.to_vec()
    } else {
        fetch(&format!("{cd_offset}-{}", cd_offset + cd_size - 1))?
    };
    let mut at = 0;
    while u32_at(&directory, at) == Some(0x0201_4b50) {
        let method = u16_at(&directory, at + 10).ok_or_else(broken)?;
        let crc = u32_at(&directory, at + 16).ok_or_else(broken)?;
        let packed = u32_at(&directory, at + 20).ok_or_else(broken)? as usize;
        let size = u32_at(&directory, at + 24).ok_or_else(broken)? as usize;
        let name_len = u16_at(&directory, at + 28).ok_or_else(broken)? as usize;
        let extra = u16_at(&directory, at + 30).ok_or_else(broken)? as usize;
        let comment = u16_at(&directory, at + 32).ok_or_else(broken)? as usize;
        let local = u32_at(&directory, at + 42).ok_or_else(broken)? as usize;
        let entry = directory.get(at + 46..at + 46 + name_len).ok_or_else(broken)?;
        at += 46 + name_len + extra + comment;
        if entry != name.as_bytes() {
            continue;
        }
        // Локальный заголовок: его поле extra бывает другим, чем в оглавлении, — берётся с
        // запасом и читается по месту.
        let chunk = fetch(&format!("{local}-{}", local + 30 + 1024 + name_len + packed))?;
        if u32_at(&chunk, 0) != Some(0x0403_4b50) {
            return Err(broken());
        }
        let skip = 30
            + u16_at(&chunk, 26).ok_or_else(broken)? as usize
            + u16_at(&chunk, 28).ok_or_else(broken)? as usize;
        let data = chunk.get(skip..skip + packed).ok_or_else(broken)?;
        let bytes = match method {
            0 => data.to_vec(),
            8 => {
                let mut out = Vec::with_capacity(size);
                flate2::read::DeflateDecoder::new(data)
                    .read_to_end(&mut out)
                    .map_err(|e| format!("распаковка: {e}"))?;
                out
            }
            other => return Err(format!("сжатие {other} не поддерживается")),
        };
        if bytes.len() != size || crc32fast::hash(&bytes) != crc {
            return Err("файл из архива повреждён".into());
        }
        return Ok(bytes);
    }
    Err(format!("в архиве нет {name}"))
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    use super::*;

    /// Сервер с кусками: диапазоны HTTP Range по байтам архива; запросы записываются.
    fn server<'a>(
        bytes: &'a [u8],
        asked: &'a mut Vec<String>,
    ) -> impl FnMut(&str) -> Result<Vec<u8>, String> + 'a {
        move |range: &str| {
            asked.push(range.to_string());
            let len = bytes.len();
            let (start, end) = match range.strip_prefix('-') {
                Some(last) => (len.saturating_sub(last.parse::<usize>().unwrap()), len - 1),
                None => {
                    let (a, b) = range.split_once('-').unwrap();
                    (a.parse().unwrap(), b.parse::<usize>().unwrap().min(len - 1))
                }
            };
            Ok(bytes[start..=end].to_vec())
        }
    }

    #[test]
    fn takes_one_file_from_a_remote_zip() {
        let mut archive = std::io::Cursor::new(Vec::new());
        {
            let mut zip = zip::ZipWriter::new(&mut archive);
            let deflated = zip::write::SimpleFileOptions::default();
            let stored = deflated.compression_method(zip::CompressionMethod::Stored);
            // Большой «шум» спереди: файл и оглавление — в разных кусках.
            zip.start_file("bin/x64-xbox/huge.dll", stored).unwrap();
            let noise: Vec<u8> = (0..200_000u32).map(|i| (i * 7919 % 251) as u8).collect();
            zip.write_all(&noise).unwrap();
            zip.start_file("bin/x64-win/DirectML.dll", deflated).unwrap();
            zip.write_all(&b"MZ DirectML ".repeat(5000)).unwrap();
            zip.start_file("runtimes/win-x64/native/onnxruntime.dll", stored).unwrap();
            zip.write_all(b"MZ onnxruntime").unwrap();
            zip.finish().unwrap();
        }
        let bytes = archive.into_inner();
        let mut asked = Vec::new();
        let mut fetch = server(&bytes, &mut asked);
        let dll = zip_entry(&mut fetch, "bin/x64-win/DirectML.dll").unwrap();
        assert_eq!(dll, b"MZ DirectML ".repeat(5000));
        let ort = zip_entry(&mut fetch, "runtimes/win-x64/native/onnxruntime.dll").unwrap();
        assert_eq!(ort, b"MZ onnxruntime");
        assert!(zip_entry(&mut fetch, "нет/такого.dll").unwrap_err().contains("нет"));
        drop(fetch);
        assert_eq!(asked[0], "-65536", "сначала — конец архива");
        assert!(asked.len() <= 6, "скачано только нужное: {asked:?}");
        // Битый файл в архиве — ошибка, а не мусор на диске.
        let mut broken = bytes.clone();
        let at = broken.windows(5).position(|w| w == b"MZ on").unwrap();
        broken[at + 3] ^= 1;
        let mut asked = Vec::new();
        let mut fetch = server(&broken, &mut asked);
        assert!(zip_entry(&mut fetch, "runtimes/win-x64/native/onnxruntime.dll").is_err());
        let mut asked = Vec::new();
        assert!(zip_entry(&mut server(b"not a zip at all", &mut asked), "x").is_err());
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
        let quiet = TagSettings { sound: false, ..settings.clone() };
        assert!(ready(dir.path(), &settings).is_none(), "модели звука ещё нет");
        let paths =
            ensure(dir.path(), &quiet, &mut |_, _, _| panic!("ничего не качается")).unwrap();
        assert_eq!(paths.runtime, runtime);
        assert_eq!(paths.model, models.join(MODEL_FILE));
        assert_eq!(paths.sound, None);
        let sound = sound_paths(dir.path(), &settings);
        std::fs::create_dir_all(sound.model.parent().unwrap()).unwrap();
        std::fs::write(&sound.model, "m").unwrap();
        std::fs::write(&sound.config, "{}").unwrap();
        let paths =
            ensure(dir.path(), &settings, &mut |_, _, _| panic!("ничего не качается")).unwrap();
        assert!(paths.sound.unwrap().model.ends_with("onnx/model.onnx"));
        let missing = TagSettings { runtime: dir.path().join("нет"), ..settings };
        assert!(ensure(dir.path(), &missing, &mut |_, _, _| true).unwrap_err().contains("нет"));
    }
}
