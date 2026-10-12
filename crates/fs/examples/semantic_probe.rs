//! Проверка поиска по смыслу с настоящей моделью — без окна MH Files.
//!
//! ```text
//! cargo run -p mh-files-fs --release --example semantic_probe -- \
//!     <папка данных> <папка с файлами> "кошка на диване" "договор аренды" …
//! ```
//!
//! В папке данных — как у MH Files: `installed.json`, `models\…`, `llama\<сборка>\llama-server`
//! (папку можно подготовить и вручную: модель с Hugging Face, llama-server из выпусков
//! llama.cpp). Служба индексирует папку с файлами, затем на каждый запрос печатает выдачу:
//! место, сходство, чем подошло, файл.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use mh_files_core::settings::SemanticSettings;
use mh_files_fs::{Event, Semantic, SemanticState, Ticket, Workers};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.len() < 3 {
        eprintln!("semantic_probe <папка данных> <папка с файлами> <запрос>…");
        std::process::exit(2);
    }
    let data = PathBuf::from(&args[0]);
    let corpus = PathBuf::from(&args[1]);
    let installed: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(data.join("installed.json")).unwrap())
            .unwrap();
    let strictness = std::env::var("MH_STRICTNESS").ok().and_then(|s| s.parse().ok());
    let settings = SemanticSettings {
        enabled: true,
        folders: vec![corpus],
        gpu: false,
        model_repo: installed["repo"].as_str().unwrap_or_default().into(),
        quant: installed["quant"].as_str().unwrap_or_default().into(),
        strictness: strictness.unwrap_or(SemanticSettings::default().strictness),
        ..SemanticSettings::default()
    };
    let (workers, events) = Workers::new(mh_files_platform::no_waker());
    let semantic = Semantic::new(workers, data);
    let started = Instant::now();
    semantic.configure(&settings);
    let mut last = String::new();
    loop {
        let status = semantic.status();
        let line = status.describe();
        if line != last {
            eprintln!("[{:>5.1} с] {line}", started.elapsed().as_secs_f32());
            last = line;
        }
        match status.state {
            SemanticState::Ready => break,
            SemanticState::Failed(error) => {
                eprintln!("ошибка: {error}");
                std::process::exit(1);
            }
            _ => std::thread::sleep(Duration::from_millis(200)),
        }
    }
    let status = semantic.status();
    eprintln!(
        "файлов {}, векторов {}, без картинок: {}, пропущено: {}",
        status.files, status.parts, status.media_fallback, status.skipped
    );
    for (n, query) in args[2..].iter().enumerate() {
        let ticket = Ticket { owner: 1, generation: n as u64 };
        semantic.search(ticket, query.clone());
        let result = loop {
            match events.recv_timeout(Duration::from_secs(120)).expect("нет ответа") {
                Event::SemanticResults { ticket: t, result } if t == ticket => break result,
                _ => continue,
            }
        };
        println!("\n«{query}»");
        match result {
            Ok(found) => {
                println!("  найдено {} за {} мс", found.entries.len(), found.elapsed.as_millis());
                for (place, entry) in found.entries.iter().take(12).enumerate() {
                    let (score, reason) = &found.reasons[&entry.path()];
                    println!("  {:>2}. {:.3}  {:<16} {}", place + 1, score, reason, entry.name);
                }
            }
            Err(error) => println!("  ошибка: {error}"),
        }
    }
    semantic.shutdown(Duration::from_secs(5));
}
