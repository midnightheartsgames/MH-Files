//! Запуск: командная строка, одна копия программы, прошлый сбой, обновление версии.
//!
//! Всё здесь выполняется до создания окна (там ввод-вывод разрешён, PLAN.md §4) или в потоке
//! сервера единственной копии — не в потоке UI.

use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};
use std::time::Instant;

use crossbeam_channel::{Receiver, Sender, unbounded};
use eframe::egui;
use mh_files_core::location::normalize;

use crate::crash;

/// Что открыть: путь и папка ли это. Файл открывается своей папкой с выделением.
pub type Target = (PathBuf, bool);

/// Что окно получает при запуске.
pub struct Startup {
    pub started: Instant,
    /// Пути из командной строки.
    pub open: Vec<Target>,
    /// Пути, которых нет, — для сообщения.
    pub missing: Vec<PathBuf>,
    pub unknown_flags: Vec<String>,
    pub previous: crash::Previous,
    /// С какой версии обновились (настройки прошлой сохранены в backup).
    pub upgraded_from: Option<String>,
    /// Пути от следующих запусков (единственная копия).
    pub incoming: Option<Receiver<Vec<Target>>>,
    /// Кого будить, когда пришли пути: окно появится позже сервера.
    pub repaint: Arc<OnceLock<egui::Context>>,
    /// Сервер единственной копии: живёт, пока живёт окно.
    pub keepalive: Option<Box<dyn std::any::Any>>,
}

/// Абсолютные пути и признак «папка». Несуществующие — отдельно.
pub fn resolve(paths: &[PathBuf], cwd: &Path) -> (Vec<Target>, Vec<PathBuf>) {
    let mut found = Vec::new();
    let mut missing = Vec::new();
    for path in paths {
        let absolute = if path.is_absolute() { path.clone() } else { cwd.join(path) };
        let absolute = normalize(&absolute);
        match std::fs::metadata(&absolute) {
            Ok(meta) => found.push((absolute, meta.is_dir())),
            Err(_) => missing.push(absolute),
        }
    }
    (found, missing)
}

/// Канал для путей от следующих запусков и обработчик для сервера единственной копии:
/// пути разбираются в его потоке, окно только будится.
pub fn incoming_channel(
    repaint: Arc<OnceLock<egui::Context>>,
) -> (Receiver<Vec<Target>>, mh_files_platform::instance::OnMessage) {
    let (tx, rx): (Sender<Vec<Target>>, Receiver<Vec<Target>>) = unbounded();
    let handler = Box::new(move |args: Vec<String>| {
        // Пути приходят уже абсолютными: их разрешила запущенная следом копия.
        let paths: Vec<PathBuf> = args.into_iter().map(PathBuf::from).collect();
        let (found, _) = resolve(&paths, Path::new(""));
        let _ = tx.send(found);
        if let Some(ctx) = repaint.get() {
            ctx.request_repaint();
        }
    });
    (rx, handler)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_relative_and_missing() {
        let dir = std::env::temp_dir().join(format!("mh-files-startup-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("sub")).unwrap();
        std::fs::write(dir.join("a.txt"), "").unwrap();
        let (found, missing) =
            resolve(&[PathBuf::from("sub"), PathBuf::from("a.txt"), PathBuf::from("nope")], &dir);
        assert_eq!(found, [(dir.join("sub"), true), (dir.join("a.txt"), false)]);
        assert_eq!(missing, [dir.join("nope")]);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
