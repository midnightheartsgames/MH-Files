//! Проверка автотегов с настоящей моделью — без окна MH Files.
//!
//! ```text
//! cargo run -p mh-files-fs --release --example tags_probe -- \
//!     <папка данных> <библиотека ONNX Runtime> <файл>…
//! ```
//!
//! В папке данных — как у MH Files: `models\<репозиторий>\model.onnx` и `selected_tags.csv`
//! (модель — `MH_TAGS_MODEL`, по умолчанию EVA02-Large; недостающее скачивается с
//! Hugging Face). Помощник с моделью — этот же пример, запущенный с `--tagger-host`, как
//! `MH-Files.exe` в программе. На каждый файл печатаются время и теги.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use mh_files_core::settings::TagSettings;
use mh_files_fs::tags::{self, HOST_FLAG, HostCommand};
use mh_files_fs::{Lookup, TagState, Tagger, Want, Workers};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().map(String::as_str) == Some(HOST_FLAG) {
        std::process::exit(tags::host::main(&args[1..]));
    }
    if args.len() < 3 {
        eprintln!("tags_probe <папка данных> <библиотека ONNX Runtime> <файл>…");
        std::process::exit(2);
    }
    let settings = TagSettings {
        enabled: true,
        runtime: PathBuf::from(&args[1]),
        model: std::env::var("MH_TAGS_MODEL").unwrap_or_else(|_| TagSettings::default().model),
        gpu: std::env::var("MH_TAGS_GPU").is_ok_and(|v| v == "1"),
        ..TagSettings::default()
    };
    let (workers, _events) = Workers::new(mh_files_platform::no_waker());
    let launcher = tags::host_launcher(HostCommand::current().expect("путь к примеру"));
    let tagger = Tagger::new(workers, PathBuf::from(&args[0]), launcher);
    let started = Instant::now();
    tagger.configure(&settings);
    let mut last = String::new();
    loop {
        let status = tagger.status();
        let mut line = status.describe();
        if status.loaded {
            line.push_str(if status.gpu {
                " · видеокарта"
            } else {
                " · процессор"
            });
        }
        if line != last {
            eprintln!("[{:>5.1} с] {line}", started.elapsed().as_secs_f32());
            last = line;
        }
        match status.state {
            TagState::Ready => break,
            TagState::Failed(error) => {
                eprintln!("ошибка: {error}");
                std::process::exit(1);
            }
            _ => std::thread::sleep(Duration::from_millis(200)),
        }
    }
    for file in &args[2..] {
        let path = std::fs::canonicalize(file).unwrap_or_else(|_| PathBuf::from(file));
        let Ok(meta) = std::fs::metadata(&path) else {
            eprintln!("{file}: не найден");
            continue;
        };
        let want = Want {
            path: path.clone(),
            size: meta.len(),
            modified: meta.modified().map_or(0, mh_files_core::index::unix),
        };
        let asked = Instant::now();
        // Как интерфейс: спрашивать каждый «кадр», пока теги не появятся.
        let lookup = loop {
            let lookup = tagger.lookup(&want.path, want.size, want.modified);
            tagger.end_frame();
            if lookup != Lookup::Pending {
                break lookup;
            }
            if let TagState::Failed(error) = tagger.status().state {
                eprintln!("ошибка: {error}");
                std::process::exit(1);
            }
            std::thread::sleep(Duration::from_millis(20));
        };
        match lookup {
            Lookup::Ready(shown) => {
                let rating = shown
                    .rating
                    .as_ref()
                    .map(|t| format!(" [{} {:.0}%]", t.name, t.score * 100.0))
                    .unwrap_or_default();
                println!("{file} ({:.1} с){rating}", asked.elapsed().as_secs_f32());
                println!("    {}", shown.line);
            }
            _ => println!("{file}: тегов не бывает"),
        }
    }
    tagger.shutdown(Duration::from_secs(10));
}
