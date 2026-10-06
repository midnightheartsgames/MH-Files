//! Действия Shell и буфер обмена — в фоне: расширение Shell может думать секунды.

use std::path::PathBuf;

use mh_files_platform::{clipboard, shell};

use crate::{Event, Workers};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ShellJob {
    Open(PathBuf),
    OpenWith(PathBuf),
    Properties(Vec<PathBuf>),
    Terminal {
        dir: PathBuf,
        command: String,
    },
    Reveal(PathBuf),
    /// Положить файлы в буфер обмена Windows.
    SetClipboard {
        paths: Vec<PathBuf>,
        cut: bool,
    },
    /// Прочитать буфер обмена для вставки в `dest`.
    ReadClipboard {
        dest: PathBuf,
    },
    /// Очистить буфер после вставки вырезанного.
    ClearClipboard,
}

pub(crate) fn run(workers: &Workers, job: ShellJob) {
    let _com = mh_files_platform::thumbs::init_worker_thread();
    let (what, result) = match job {
        ShellJob::Open(path) => ("Открыть", shell::open(&path)),
        ShellJob::OpenWith(path) => ("Открыть с помощью", shell::open_with(&path)),
        ShellJob::Properties(paths) => ("Свойства", shell::properties(&paths)),
        ShellJob::Terminal { dir, command } => ("Терминал", shell::open_terminal(&dir, &command)),
        ShellJob::Reveal(path) => ("Показать в Проводнике", shell::reveal_in_explorer(&path)),
        ShellJob::SetClipboard { paths, cut } => {
            ("Буфер обмена", clipboard::set_files(&paths, cut))
        }
        ShellJob::ReadClipboard { dest } => {
            workers.send(Event::Paste { dest, files: clipboard::get_files() });
            return;
        }
        ShellJob::ClearClipboard => {
            clipboard::clear();
            return;
        }
    };
    workers.send(Event::Shell { what: what.to_string(), result });
}
