//! Наблюдение за содержимым папки: в Windows — `ReadDirectoryChangesW`, без опроса.
//!
//! События сырые; склеивает и применяет их вызывающий (с задержкой ~75 мс).

use std::path::PathBuf;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WatchEvent {
    Created(PathBuf),
    Removed(PathBuf),
    Modified(PathBuf),
    Renamed {
        from: PathBuf,
        to: PathBuf,
    },
    /// Событий было больше, чем помещается в буфер: папку нужно перечитать целиком.
    Overflow,
    /// Наблюдение прекратилось: папку удалили или диск отключили.
    Stopped,
}

/// Наблюдатель за одной папкой (без вложенных). Останавливается при уничтожении.
pub struct Watcher {
    #[cfg(windows)]
    _inner: crate::win::watch::DirWatcher,
    #[cfg(not(windows))]
    _inner: portable::PollWatcher,
}

impl Watcher {
    /// Наблюдатель за папкой и всеми вложенными — для индекса диска. Вне Windows его нет:
    /// индекс там обновляется пересканированием.
    pub fn recursive(
        dir: PathBuf,
        on_events: Box<dyn Fn(Vec<WatchEvent>) + Send + 'static>,
    ) -> Result<Watcher, String> {
        #[cfg(windows)]
        {
            Ok(Watcher { _inner: crate::win::watch::DirWatcher::recursive(dir, on_events)? })
        }
        #[cfg(not(windows))]
        {
            let _ = (dir, on_events);
            Err("наблюдение за деревом папок есть только в Windows".into())
        }
    }

    /// `on_events` вызывается из потока наблюдателя пачками событий.
    pub fn new(
        dir: PathBuf,
        on_events: Box<dyn Fn(Vec<WatchEvent>) + Send + 'static>,
    ) -> Result<Watcher, String> {
        #[cfg(windows)]
        {
            Ok(Watcher { _inner: crate::win::watch::DirWatcher::new(dir, on_events)? })
        }
        #[cfg(not(windows))]
        {
            Ok(Watcher { _inner: portable::PollWatcher::new(dir, on_events)? })
        }
    }
}

#[cfg(not(windows))]
mod portable {
    //! Вне Windows — опрос раз в секунду. Только для разработки.

    use std::collections::HashMap;
    use std::path::{Path, PathBuf};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::{Duration, SystemTime};

    use super::WatchEvent;

    pub struct PollWatcher {
        stop: Arc<AtomicBool>,
    }

    type Snapshot = HashMap<PathBuf, (u64, Option<SystemTime>)>;

    fn snapshot(dir: &Path) -> Option<Snapshot> {
        let entries = std::fs::read_dir(dir).ok()?;
        Some(
            entries
                .flatten()
                .map(|entry| {
                    let meta = entry.metadata().ok();
                    let key = meta.map(|m| (m.len(), m.modified().ok())).unwrap_or((0, None));
                    (entry.path(), key)
                })
                .collect(),
        )
    }

    impl PollWatcher {
        pub fn new(
            dir: PathBuf,
            on_events: Box<dyn Fn(Vec<WatchEvent>) + Send + 'static>,
        ) -> Result<PollWatcher, String> {
            let mut last = snapshot(&dir).ok_or_else(|| format!("нет папки {}", dir.display()))?;
            let stop = Arc::new(AtomicBool::new(false));
            let flag = stop.clone();
            std::thread::Builder::new()
                .name("watch-poll".into())
                .spawn(move || {
                    while !flag.load(Ordering::Relaxed) {
                        std::thread::sleep(Duration::from_millis(1000));
                        if flag.load(Ordering::Relaxed) {
                            break;
                        }
                        let Some(now) = snapshot(&dir) else {
                            on_events(vec![WatchEvent::Stopped]);
                            break;
                        };
                        let mut events = Vec::new();
                        for (path, key) in &now {
                            match last.get(path) {
                                None => events.push(WatchEvent::Created(path.clone())),
                                Some(old) if old != key => {
                                    events.push(WatchEvent::Modified(path.clone()))
                                }
                                _ => {}
                            }
                        }
                        for path in last.keys() {
                            if !now.contains_key(path) {
                                events.push(WatchEvent::Removed(path.clone()));
                            }
                        }
                        if !events.is_empty() {
                            on_events(events);
                        }
                        last = now;
                    }
                })
                .map_err(|error| error.to_string())?;
            Ok(PollWatcher { stop })
        }
    }

    impl Drop for PollWatcher {
        fn drop(&mut self) {
            self.stop.store(true, Ordering::Relaxed);
        }
    }
}
