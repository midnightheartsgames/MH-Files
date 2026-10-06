//! Действия Shell: открыть, «Открыть с помощью», свойства, терминал, показать в Проводнике.
//!
//! Вызовы могут ждать расширения Shell сторонних программ — только из фонового потока.

use std::path::{Path, PathBuf};

/// Открыть файл программой по умолчанию (или запустить исполняемый файл).
pub fn open(path: &Path) -> Result<(), String> {
    #[cfg(windows)]
    {
        crate::win::shell::open(path)
    }
    #[cfg(not(windows))]
    {
        spawn("xdg-open", &[path.as_os_str()])
    }
}

/// Системный диалог «Открыть с помощью».
pub fn open_with(path: &Path) -> Result<(), String> {
    #[cfg(windows)]
    {
        crate::win::shell::open_with(path)
    }
    #[cfg(not(windows))]
    {
        let _ = path;
        Err("«Открыть с помощью» есть только в Windows".into())
    }
}

/// Системное окно «Свойства» для одного или нескольких объектов.
pub fn properties(paths: &[PathBuf]) -> Result<(), String> {
    #[cfg(windows)]
    {
        crate::win::shell::properties(paths)
    }
    #[cfg(not(windows))]
    {
        let _ = paths;
        Err("окно свойств есть только в Windows".into())
    }
}

/// Терминал в папке. `command` — своя команда из настроек, `{dir}` заменяется папкой;
/// пустая — Windows Terminal, а без него PowerShell.
pub fn open_terminal(dir: &Path, command: &str) -> Result<(), String> {
    #[cfg(windows)]
    {
        crate::win::shell::open_terminal(dir, command)
    }
    #[cfg(not(windows))]
    {
        let _ = command;
        std::process::Command::new("x-terminal-emulator")
            .current_dir(dir)
            .spawn()
            .map(|_| ())
            .map_err(|error| format!("терминал не запущен: {error}"))
    }
}

/// Открыть Проводник с выделенным объектом.
pub fn reveal_in_explorer(path: &Path) -> Result<(), String> {
    #[cfg(windows)]
    {
        crate::win::shell::reveal_in_explorer(path)
    }
    #[cfg(not(windows))]
    {
        let parent = path.parent().unwrap_or(path);
        spawn("xdg-open", &[parent.as_os_str()])
    }
}

#[cfg(not(windows))]
fn spawn(program: &str, args: &[&std::ffi::OsStr]) -> Result<(), String> {
    std::process::Command::new(program)
        .args(args)
        .spawn()
        .map(|_| ())
        .map_err(|error| format!("{program}: {error}"))
}
