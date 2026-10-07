//! Действия Shell и буфер обмена — в фоне: расширение Shell может думать секунды.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use mh_files_platform::shell::MenuGate;
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
    /// Контекстное меню Windows для объектов.
    ContextMenu(Vec<PathBuf>),
    /// Меню Windows для пустого места папки.
    BackgroundMenu(PathBuf),
    /// Ярлыки на объекты в папке `dest`.
    CreateShortcuts {
        targets: Vec<PathBuf>,
        dest: PathBuf,
    },
}

pub(crate) fn run(workers: &Workers, job: ShellJob) {
    let _com = mh_files_platform::thumbs::init_worker_thread();
    let (what, result) = match job {
        ShellJob::Open(path) => ("Открыть", shell::open(&path)),
        ShellJob::OpenWith(path) => ("Открыть с помощью", shell::open_with(&path)),
        ShellJob::Properties(paths) => ("Свойства", shell::properties(&paths)),
        ShellJob::Terminal { dir, command } => ("Терминал", shell::open_terminal(&dir, &command)),
        ShellJob::Reveal(path) => ("Показать в Проводнике", shell::reveal_in_explorer(&path)),
        ShellJob::CreateShortcuts { targets, dest } => {
            ("Ярлыки", shell::create_shortcuts(&targets, &dest).map(|_| ()))
        }
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
        ShellJob::ContextMenu(paths) => {
            let gate = watch_menu(workers, Some(paths.clone()));
            let choice = shell::context_menu(&paths, &gate);
            // Меню закрыто или не собралось: сторожу больше нечего ждать.
            if gate.try_show() {
                workers.send(Event::Menu { paths, choice });
            }
            return;
        }
        ShellJob::BackgroundMenu(dir) => {
            let gate = watch_menu(workers, None);
            let choice = shell::background_menu(&dir, &gate);
            // Меню закрыто или не собралось: сторожу больше нечего ждать.
            if gate.try_show() {
                workers.send(Event::Menu { paths: vec![dir], choice });
            }
            return;
        }
    };
    workers.send(Event::Shell { what: what.to_string(), result });
}

/// Сколько ждать, пока расширения соберут меню Windows. Обычно это доли секунды; дольше —
/// значит, чья-то библиотека зависла (сетевой диск в «Отправить», облачный клиент…).
const MENU_TIMEOUT: Duration = Duration::from_secs(6);

/// Сторож меню: если за [`MENU_TIMEOUT`] меню не собралось, его перестают ждать (поток с
/// зависшим расширением остаётся висеть — убить его нельзя, но и показать меню он уже не
/// сможет) и говорят об этом. Для объектов сразу показывается меню без расширений.
fn watch_menu(workers: &Workers, paths: Option<Vec<PathBuf>>) -> Arc<MenuGate> {
    let gate = Arc::new(MenuGate::default());
    let watched = gate.clone();
    workers.spawn("shell-menu-watch", move |workers| {
        let started = Instant::now();
        while !watched.settled() && started.elapsed() < MENU_TIMEOUT {
            std::thread::sleep(Duration::from_millis(50));
        }
        if !watched.try_abandon() {
            return;
        }
        let seconds = MENU_TIMEOUT.as_secs();
        let Some(paths) = paths else {
            workers.send(Event::Shell {
                what: "Меню Windows".into(),
                result: Err(format!(
                    "меню Windows не собралось за {seconds} с: не отвечает расширение Shell"
                )),
            });
            return;
        };
        workers.send(Event::Shell {
            what: "Меню Windows".into(),
            result: Err(format!(
                "расширение Shell не отвечает {seconds} с — меню показано без сторонних пунктов"
            )),
        });
        let _com = mh_files_platform::thumbs::init_worker_thread();
        let choice = shell::plain_context_menu(&paths);
        workers.send(Event::Menu { paths, choice });
    });
    gate
}
