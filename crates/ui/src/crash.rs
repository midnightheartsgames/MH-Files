//! Восстановление после сбоя.
//!
//! * Паника пишет отчёт в `crashes\crash-<время>.txt` (версия, поток, место, стек).
//! * Пока программа работает, в папке данных лежит файл `running-<pid>`, запертый ею
//!   (`File::lock`); при обычном выходе он удаляется. Замок снимает система, когда процесс
//!   кончается как угодно, поэтому незапертый чужой файл — прошлый запуск закончился сбоем
//!   (паника, снятие процесса, выключение питания), и при запуске об этом говорится. Запертый —
//!   это вторая открытая копия (`--new-window`), а не сбой. Сеанс к тому времени сохранён:
//!   он пишется каждые 5 секунд, если изменился.

use std::fs::File;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// Сколько отчётов хранить.
const KEEP: usize = 10;

/// Свой файл-отметка: держится открытым и запертым до выхода.
static MARKER: Mutex<Option<(File, PathBuf)>> = Mutex::new(None);

/// Что известно о прошлом запуске.
#[derive(Debug, Clone, Default)]
pub struct Previous {
    /// Прошлый запуск не дошёл до обычного выхода.
    pub crashed: bool,
    /// Последний отчёт о сбое, если он есть и новее прошлого запуска.
    pub report: Option<PathBuf>,
}

pub fn crashes_dir(data: &Path) -> PathBuf {
    data.join("crashes")
}

/// Отметить начало работы и узнать, чем кончился прошлый запуск.
pub fn start(data: &Path) -> Previous {
    // Отметки запусков, которые кончились, не дойдя до выхода, — их никто не держит.
    let mut started: Option<std::time::SystemTime> = None;
    let mut crashed = false;
    for path in markers(data) {
        let Ok(file) = File::options().read(true).write(true).open(&path) else { continue };
        if file.try_lock().is_err() {
            continue; // другая копия работает
        }
        crashed = true;
        let modified = file.metadata().and_then(|m| m.modified()).ok();
        started = match (started, modified) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        };
        drop(file);
        let _ = std::fs::remove_file(&path);
    }
    let report = latest_report(data).filter(|path| {
        // Отчёт относится к прошлому запуску, а не к давнему.
        let modified = std::fs::metadata(path).and_then(|m| m.modified()).ok();
        crashed && started.zip(modified).is_none_or(|(s, m)| m >= s)
    });
    let _ = std::fs::create_dir_all(data);
    let path = data.join(format!("running-{}", std::process::id()));
    if let Ok(file) = File::create(&path)
        && file.try_lock().is_ok()
    {
        *MARKER.lock().unwrap_or_else(|e| e.into_inner()) = Some((file, path));
    }
    Previous { crashed, report }
}

/// Обычный выход.
pub fn finish() {
    if let Some((file, path)) = MARKER.lock().unwrap_or_else(|e| e.into_inner()).take() {
        drop(file);
        let _ = std::fs::remove_file(path);
    }
}

fn markers(data: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(data) else { return Vec::new() };
    entries
        .flatten()
        .filter(|e| e.file_name().to_string_lossy().starts_with("running"))
        .map(|e| e.path())
        .collect()
}

fn latest_report(data: &Path) -> Option<PathBuf> {
    let mut reports: Vec<PathBuf> = std::fs::read_dir(crashes_dir(data))
        .ok()?
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e == "txt"))
        .collect();
    reports.sort();
    reports.pop()
}

/// Перехват паники: отчёт в файл, затем обычная обработка (сообщение в консоль).
pub fn install_hook(data: &Path) {
    let dir = crashes_dir(data);
    let default = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let stamp = chrono::Local::now();
        let thread = std::thread::current().name().unwrap_or("без имени").to_string();
        let message = info
            .payload()
            .downcast_ref::<&str>()
            .map(|s| s.to_string())
            .or_else(|| info.payload().downcast_ref::<String>().cloned())
            .unwrap_or_else(|| "паника без текста".to_string());
        let place =
            info.location().map(|l| format!("{}:{}", l.file(), l.line())).unwrap_or_default();
        let text = format!(
            "MH Files {}\n{}\nПоток: {thread}\nМесто: {place}\n{message}\n\n{}\n",
            env!("CARGO_PKG_VERSION"),
            stamp.format("%d.%m.%Y %H:%M:%S"),
            std::backtrace::Backtrace::force_capture()
        );
        if std::fs::create_dir_all(&dir).is_ok() {
            let path = dir.join(format!("crash-{}.txt", stamp.format("%Y-%m-%d_%H-%M-%S")));
            let _ = std::fs::write(path, text);
            prune(&dir);
        }
        default(info);
    }));
}

fn prune(dir: &Path) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    let mut reports: Vec<PathBuf> = entries.flatten().map(|e| e.path()).collect();
    reports.sort();
    let extra = reports.len().saturating_sub(KEEP);
    for path in &reports[..extra] {
        let _ = std::fs::remove_file(path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn marker_tells_about_previous_run() {
        let dir = std::env::temp_dir().join(format!("mh-files-crash-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        assert!(!start(&dir).crashed, "первый запуск");
        finish();
        assert!(!start(&dir).crashed, "прошлый вышел как обычно");
        // Запертая отметка — работающая копия, а не сбой.
        assert!(!start(&dir).crashed, "своя запертая отметка");
        finish();
        // Незапертая чужая отметка — как после сбоя (и старое имя `running` из rc-сборок).
        std::fs::write(dir.join("running-1"), "").unwrap();
        std::fs::create_dir_all(crashes_dir(&dir)).unwrap();
        std::fs::write(crashes_dir(&dir).join("crash-2026-10-07_10-00-00.txt"), "x").unwrap();
        let previous = start(&dir);
        assert!(previous.crashed);
        assert!(previous.report.is_some());
        assert!(!dir.join("running-1").exists());
        finish();
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn panic_writes_report() {
        let dir = std::env::temp_dir().join(format!("mh-files-panic-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        install_hook(&dir);
        let result = std::thread::Builder::new()
            .name("проверка".into())
            .spawn(|| panic!("нарочно"))
            .unwrap()
            .join();
        let _ = std::panic::take_hook();
        assert!(result.is_err());
        let report = latest_report(&dir).expect("отчёт");
        let text = std::fs::read_to_string(report).unwrap();
        assert!(text.contains("нарочно") && text.contains("Поток: проверка"), "{text}");
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
