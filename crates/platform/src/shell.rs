//! Действия Shell: открыть, «Открыть с помощью», свойства, терминал, показать в Проводнике.
//!
//! Вызовы могут ждать расширения Shell сторонних программ — только из фонового потока.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU8, Ordering};

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

/// Кодовая страница ANSI Windows (1251 у русской); вне Windows — UTF-8 (65001). Нужна, чтобы
/// прочитать вывод консольных программ, печатающих в ней.
pub fn ansi_code_page() -> u32 {
    #[cfg(windows)]
    {
        // SAFETY: функция без аргументов, только читает настройку системы.
        unsafe { windows::Win32::Globalization::GetACP() }
    }
    #[cfg(not(windows))]
    {
        65001
    }
}

/// Ярлыки на `targets` в папке `dest` («имя - ярлык.lnk», занятые имена не трогаются).
/// Вне Windows — символические ссылки. Возвращает пути созданных ярлыков.
pub fn create_shortcuts(targets: &[PathBuf], dest: &Path) -> Result<Vec<PathBuf>, String> {
    let mut made = Vec::new();
    for target in targets {
        let link = free_shortcut_path(target, dest);
        #[cfg(windows)]
        crate::win::shell::create_shortcut(target, &link)?;
        #[cfg(not(windows))]
        std::os::unix::fs::symlink(target, &link)
            .map_err(|error| format!("ярлык {} не создан: {error}", link.display()))?;
        made.push(link);
    }
    Ok(made)
}

/// Свободное имя ярлыка: «имя - ярлык», затем «имя - ярлык (2)»…
fn free_shortcut_path(target: &Path, dest: &Path) -> PathBuf {
    let name = match target.file_name() {
        Some(name) => name.to_string_lossy().into_owned(),
        // Корень диска: «C:\» → «Диск C».
        None => format!("Диск {}", target.to_string_lossy().trim_end_matches(['\\', '/', ':'])),
    };
    let ext = if cfg!(windows) { ".lnk" } else { "" };
    (1..)
        .map(|n| match n {
            1 => dest.join(format!("{name} - ярлык{ext}")),
            n => dest.join(format!("{name} - ярлык ({n}){ext}")),
        })
        .find(|path| std::fs::symlink_metadata(path).is_err())
        .unwrap_or_else(|| dest.join(format!("{name} - ярлык{ext}")))
}

#[cfg(not(windows))]
fn spawn(program: &str, args: &[&std::ffi::OsStr]) -> Result<(), String> {
    std::process::Command::new(program)
        .args(args)
        .spawn()
        .map(|_| ())
        .map_err(|error| format!("{program}: {error}"))
}

/// Что выбрали в меню Windows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MenuChoice {
    /// Меню закрыли, ничего не выбрав.
    Dismissed,
    /// Выполнена команда; `verb` — её имя для Shell («delete», «cut», «7-Zip.Extract»…), если
    /// расширение его сообщило. Нужен, чтобы MH Files сам обработал «Переименовать»,
    /// «Вырезать» и т. п. без расхождений со своим состоянием.
    Invoked { verb: Option<String> },
}

/// Кто первым: меню, собранное расширениями, или сторож, переставший его ждать. Меню,
/// которое собралось после того, как его бросили, не показывается: оно выскочило бы через
/// минуту, когда пользователь уже занят другим.
#[derive(Debug, Default)]
pub struct MenuGate(AtomicU8);

const GATE_BUILDING: u8 = 0;
const GATE_SHOWN: u8 = 1;
const GATE_ABANDONED: u8 = 2;

impl MenuGate {
    /// Меню собрано (или закрыто) — показать? `false`, если его уже бросили.
    pub fn try_show(&self) -> bool {
        self.settle(GATE_SHOWN)
    }

    /// Перестать ждать. `false`, если меню уже показано.
    pub fn try_abandon(&self) -> bool {
        self.settle(GATE_ABANDONED)
    }

    pub fn abandoned(&self) -> bool {
        self.0.load(Ordering::Acquire) == GATE_ABANDONED
    }

    /// Меню показано или брошено.
    pub fn settled(&self) -> bool {
        self.0.load(Ordering::Acquire) != GATE_BUILDING
    }

    fn settle(&self, to: u8) -> bool {
        match self.0.compare_exchange(GATE_BUILDING, to, Ordering::AcqRel, Ordering::Acquire) {
            Ok(_) => true,
            Err(now) => now == to,
        }
    }
}

/// Контекстное меню Shell для объектов одной папки — с пунктами сторонних расширений
/// (7-Zip, Git, VLC…), там, где сейчас курсор. Блокирует до закрытия меню; вызывать из
/// фонового потока: окно меню принадлежит этому потоку. Собранное меню показывается, только
/// если `gate` его ещё ждёт; иначе — `Dismissed`.
pub fn context_menu(paths: &[PathBuf], gate: &MenuGate) -> Result<MenuChoice, String> {
    #[cfg(windows)]
    {
        crate::win::menu::context_menu(paths, gate, true)
    }
    #[cfg(not(windows))]
    {
        let _ = (paths, gate);
        Err("меню Windows есть только в Windows".into())
    }
}

/// То же меню без сторонних расширений: только пункты самой Windows («Вырезать»,
/// «Копировать», «Удалить», «Свойства»…). Для случая, когда расширение зависло.
pub fn plain_context_menu(paths: &[PathBuf]) -> Result<MenuChoice, String> {
    #[cfg(windows)]
    {
        crate::win::menu::context_menu(paths, &MenuGate::default(), false)
    }
    #[cfg(not(windows))]
    {
        let _ = paths;
        Err("меню Windows есть только в Windows".into())
    }
}

/// Меню пустого места папки: «Создать ▸», «Вставить ярлык», пункты расширений.
pub fn background_menu(dir: &Path, gate: &MenuGate) -> Result<MenuChoice, String> {
    #[cfg(windows)]
    {
        crate::win::menu::background_menu(dir, gate)
    }
    #[cfg(not(windows))]
    {
        let _ = (dir, gate);
        Err("меню Windows есть только в Windows".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gate_settles_once() {
        let gate = MenuGate::default();
        assert!(!gate.settled());
        assert!(gate.try_show());
        assert!(gate.try_show(), "повторно — то же решение");
        assert!(!gate.try_abandon(), "показанное меню уже не бросить");
        let gate = MenuGate::default();
        assert!(gate.try_abandon());
        assert!(gate.abandoned());
        assert!(!gate.try_show(), "брошенное меню не показывается");
    }

    #[cfg(not(windows))]
    #[test]
    fn shortcuts_get_free_names() {
        let dir = std::env::temp_dir().join(format!("mh-files-links-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("dest")).unwrap();
        std::fs::write(dir.join("a.txt"), "").unwrap();
        let made =
            create_shortcuts(&[dir.join("a.txt"), dir.join("a.txt")], &dir.join("dest")).unwrap();
        assert_eq!(made, [dir.join("dest/a.txt - ярлык"), dir.join("dest/a.txt - ярлык (2)")]);
        assert!(made.iter().all(|link| link.exists()));
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
