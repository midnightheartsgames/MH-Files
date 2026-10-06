//! Файловые операции: копирование, перемещение, удаление, переименование, новая папка.
//!
//! В Windows всё идёт через `IFileOperation` в отдельном STA-потоке: так получаются обычные для
//! Windows корзина, диалоги конфликтов имён, запрос прав администратора, прогресс и отмена.
//! Операции выполняются по очереди в порядке поступления.
//!
//! Правило PLAN.md §4/10: операция получает **точный** список путей; никаких масок и
//! «всё в папке».

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use crossbeam_channel::{Receiver, Sender, unbounded};

use crate::Waker;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FileOp {
    /// Копировать в папку `dest`. Если папка та же, что у источника, — копия рядом с новым именем.
    Copy {
        sources: Vec<PathBuf>,
        dest: PathBuf,
    },
    /// Переместить в папку `dest`.
    Move {
        sources: Vec<PathBuf>,
        dest: PathBuf,
    },
    /// `permanent: false` — в корзину.
    Delete {
        paths: Vec<PathBuf>,
        permanent: bool,
    },
    /// Новое имя без пути.
    Rename {
        path: PathBuf,
        new_name: String,
    },
    NewFolder {
        parent: PathBuf,
        name: String,
    },
}

impl FileOp {
    /// Короткое описание для строки состояния.
    pub fn describe(&self) -> String {
        let count = |n: usize| match n {
            1 => "1 объект".to_string(),
            n => format!("{n} об."),
        };
        match self {
            FileOp::Copy { sources, .. } => format!("Копирование: {}", count(sources.len())),
            FileOp::Move { sources, .. } => format!("Перемещение: {}", count(sources.len())),
            FileOp::Delete { paths, permanent: false } => {
                format!("В корзину: {}", count(paths.len()))
            }
            FileOp::Delete { paths, permanent: true } => {
                format!("Удаление: {}", count(paths.len()))
            }
            FileOp::Rename { new_name, .. } => format!("Переименование в «{new_name}»"),
            FileOp::NewFolder { name, .. } => format!("Новая папка «{name}»"),
        }
    }

    /// Папки, содержимое которых меняется: их стоит перечитать, если наблюдатель не успел.
    pub fn affected_dirs(&self) -> Vec<PathBuf> {
        let parents = |paths: &[PathBuf]| -> Vec<PathBuf> {
            paths.iter().filter_map(|path| path.parent().map(PathBuf::from)).collect()
        };
        let mut dirs = match self {
            FileOp::Copy { dest, .. } => vec![dest.clone()],
            FileOp::Move { sources, dest } => {
                let mut dirs = parents(sources);
                dirs.push(dest.clone());
                dirs
            }
            FileOp::Delete { paths, .. } => parents(paths),
            FileOp::Rename { path, .. } => parents(std::slice::from_ref(path)),
            FileOp::NewFolder { parent, .. } => vec![parent.clone()],
        };
        dirs.sort();
        dirs.dedup();
        dirs
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct OpId(pub u64);

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OpOutcome {
    Done {
        /// Новые пути, если они известны: новая папка, переименованный объект.
        created: Vec<PathBuf>,
    },
    /// Пользователь отменил в системном диалоге.
    Aborted,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OpEvent {
    Started { id: OpId, op: FileOp },
    Finished { id: OpId, op: FileOp, result: Result<OpOutcome, String> },
}

/// Очередь операций со своим потоком. Закрывается вместе с последней копией.
pub struct Executor {
    jobs: Sender<(OpId, FileOp)>,
    events: Receiver<OpEvent>,
}

static NEXT_ID: AtomicU64 = AtomicU64::new(1);

impl Executor {
    pub fn new(waker: Waker) -> Executor {
        let (jobs, job_rx) = unbounded::<(OpId, FileOp)>();
        let (event_tx, events) = unbounded();
        std::thread::Builder::new()
            .name("file-ops".into())
            .spawn(move || run(job_rx, event_tx, waker))
            .expect("поток файловых операций");
        Executor { jobs, events }
    }

    pub fn submit(&self, op: FileOp) -> OpId {
        let id = OpId(NEXT_ID.fetch_add(1, Ordering::Relaxed));
        // Поток живёт, пока жив отправитель, — отправка не падает.
        let _ = self.jobs.send((id, op));
        id
    }

    pub fn events(&self) -> &Receiver<OpEvent> {
        &self.events
    }
}

fn run(jobs: Receiver<(OpId, FileOp)>, events: Sender<OpEvent>, waker: Waker) {
    #[cfg(windows)]
    let _com = crate::win::com::Apartment::sta();
    for (id, op) in jobs {
        let _ = events.send(OpEvent::Started { id, op: op.clone() });
        waker();
        let result = execute(&op);
        let _ = events.send(OpEvent::Finished { id, op, result });
        waker();
    }
}

/// Выполняет одну операцию в текущем потоке. В Windows поток должен быть STA.
pub fn execute(op: &FileOp) -> Result<OpOutcome, String> {
    #[cfg(windows)]
    {
        crate::win::ops::execute(op)
    }
    #[cfg(not(windows))]
    {
        portable::execute(op)
    }
}

#[cfg(not(windows))]
mod portable {
    use std::fs;
    use std::path::{Path, PathBuf};

    use super::{FileOp, OpOutcome};

    pub fn execute(op: &FileOp) -> Result<OpOutcome, String> {
        match op {
            FileOp::Copy { sources, dest } => {
                for source in sources {
                    let target = free_target(source, dest)?;
                    copy_recursive(source, &target).map_err(|e| describe(source, e))?;
                }
                Ok(OpOutcome::Done { created: Vec::new() })
            }
            FileOp::Move { sources, dest } => {
                for source in sources {
                    let target = dest.join(file_name(source)?);
                    if target.exists() {
                        return Err(format!("уже существует: {}", target.display()));
                    }
                    if fs::rename(source, &target).is_err() {
                        copy_recursive(source, &target).map_err(|e| describe(source, e))?;
                        remove(source).map_err(|e| describe(source, e))?;
                    }
                }
                Ok(OpOutcome::Done { created: Vec::new() })
            }
            FileOp::Delete { paths, permanent } => {
                if !permanent {
                    return Err(
                        "корзина есть только в Windows; удалите насовсем (Shift+Delete)".into()
                    );
                }
                for path in paths {
                    remove(path).map_err(|e| describe(path, e))?;
                }
                Ok(OpOutcome::Done { created: Vec::new() })
            }
            FileOp::Rename { path, new_name } => {
                let target = path.with_file_name(new_name);
                if target.exists() && !same_ignoring_case(path, &target) {
                    return Err(format!("уже существует: {}", target.display()));
                }
                fs::rename(path, &target).map_err(|e| describe(path, e))?;
                Ok(OpOutcome::Done { created: vec![target] })
            }
            FileOp::NewFolder { parent, name } => {
                let target = parent.join(name);
                fs::create_dir(&target).map_err(|e| describe(&target, e))?;
                Ok(OpOutcome::Done { created: vec![target] })
            }
        }
    }

    fn same_ignoring_case(a: &Path, b: &Path) -> bool {
        a.to_string_lossy().to_lowercase() == b.to_string_lossy().to_lowercase()
    }

    fn file_name(path: &Path) -> Result<&std::ffi::OsStr, String> {
        path.file_name().ok_or_else(|| format!("нет имени: {}", path.display()))
    }

    /// Свободное имя в папке назначения: «имя», «имя - копия», «имя - копия (2)»…
    fn free_target(source: &Path, dest: &Path) -> Result<PathBuf, String> {
        let name = file_name(source)?.to_string_lossy().into_owned();
        let target = dest.join(&name);
        if !target.exists() {
            return Ok(target);
        }
        if source.parent() != Some(dest) {
            return Err(format!("уже существует: {}", target.display()));
        }
        let (stem, ext) = match name.rfind('.') {
            Some(dot) if dot > 0 && !source.is_dir() => (&name[..dot], &name[dot..]),
            _ => (name.as_str(), ""),
        };
        for n in 1.. {
            let suffix =
                if n == 1 { " - копия".to_string() } else { format!(" - копия ({n})") };
            let candidate = dest.join(format!("{stem}{suffix}{ext}"));
            if !candidate.exists() {
                return Ok(candidate);
            }
        }
        unreachable!()
    }

    fn copy_recursive(source: &Path, target: &Path) -> std::io::Result<()> {
        let meta = fs::symlink_metadata(source)?;
        if meta.is_dir() {
            fs::create_dir(target)?;
            for entry in fs::read_dir(source)? {
                let entry = entry?;
                copy_recursive(&entry.path(), &target.join(entry.file_name()))?;
            }
            Ok(())
        } else {
            fs::copy(source, target).map(|_| ())
        }
    }

    fn remove(path: &Path) -> std::io::Result<()> {
        let meta = fs::symlink_metadata(path)?;
        if meta.is_dir() { fs::remove_dir_all(path) } else { fs::remove_file(path) }
    }

    fn describe(path: &Path, error: std::io::Error) -> String {
        format!("{}: {error}", path.display())
    }
}

#[cfg(all(test, not(windows)))]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("mh-files-ops-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn copy_into_same_folder_makes_a_copy() {
        let dir = temp_dir("copy");
        std::fs::write(dir.join("a.txt"), "x").unwrap();
        let op = FileOp::Copy { sources: vec![dir.join("a.txt")], dest: dir.clone() };
        execute(&op).unwrap();
        execute(&op).unwrap();
        assert!(dir.join("a - копия.txt").exists());
        assert!(dir.join("a - копия (2).txt").exists());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn move_rename_and_new_folder() {
        let dir = temp_dir("move");
        execute(&FileOp::NewFolder { parent: dir.clone(), name: "sub".into() }).unwrap();
        std::fs::write(dir.join("f"), "x").unwrap();
        execute(&FileOp::Move { sources: vec![dir.join("f")], dest: dir.join("sub") }).unwrap();
        assert!(dir.join("sub/f").exists());
        execute(&FileOp::Rename { path: dir.join("sub/f"), new_name: "g".into() }).unwrap();
        assert!(dir.join("sub/g").exists());
        let err = execute(&FileOp::Delete { paths: vec![dir.join("sub")], permanent: false });
        assert!(err.is_err(), "без корзины ничего не удаляется");
        execute(&FileOp::Delete { paths: vec![dir.join("sub")], permanent: true }).unwrap();
        assert!(!dir.join("sub").exists());
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
