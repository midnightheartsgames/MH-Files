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

/// Для чего меню Windows: объекты одной папки или пустое место папки.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MenuTarget {
    Items(Vec<PathBuf>),
    Background(PathBuf),
}

impl MenuTarget {
    /// Пути, которые могла затронуть команда меню: объекты или сама папка.
    pub fn paths(&self) -> Vec<PathBuf> {
        match self {
            MenuTarget::Items(paths) => paths.clone(),
            MenuTarget::Background(dir) => vec![dir.clone()],
        }
    }
}

/// Значок пункта меню: RGBA с premultiplied alpha (так их хранит Windows), строка за строкой.
#[derive(Clone, PartialEq, Eq)]
pub struct MenuIcon {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

impl std::fmt::Debug for MenuIcon {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "MenuIcon({}×{})", self.width, self.height)
    }
}

/// Пункт меню Windows, прочитанный для показа в своём меню MH Files.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ShellMenuItem {
    Separator,
    /// Команда: `id` передаётся обратно потоку меню, чтобы её выполнить.
    Command {
        id: u32,
        label: String,
        /// Имя команды для Shell («delete», «7-Zip.Extract»…), если расширение его сообщает.
        verb: Option<String>,
        icon: Option<MenuIcon>,
        enabled: bool,
    },
    Submenu {
        label: String,
        icon: Option<MenuIcon>,
        enabled: bool,
        items: Vec<ShellMenuItem>,
    },
}

/// Подпись пункта меню Win32 без служебного: `&` перед буквой-мнемоникой (`&&` — сам
/// амперсанд) и сочетание клавиш после табуляции.
pub fn menu_label(raw: &str) -> String {
    let text = raw.split('\t').next().unwrap_or_default();
    let mut label = String::with_capacity(text.len());
    let mut chars = text.chars();
    while let Some(c) = chars.next() {
        if c == '&' {
            if let Some(next) = chars.next() {
                label.push(next);
            }
        } else {
            label.push(c);
        }
    }
    label.trim().to_string()
}

/// Пункты Windows для вставки в своё меню: без команд, которые у MH Files уже есть (`hidden` —
/// их имена для Shell), без повторов одной подписи (Windows 11 показывает пункт PowerToys и
/// как современную команду, и как старое расширение), без пустых подменю и лишних
/// разделителей.
pub fn tidy_menu(items: Vec<ShellMenuItem>, hidden: &[&str]) -> Vec<ShellMenuItem> {
    let mut tidy: Vec<ShellMenuItem> = Vec::with_capacity(items.len());
    let mut labels = std::collections::HashSet::new();
    for item in items {
        let item = match item {
            ShellMenuItem::Separator => {
                if !matches!(tidy.last(), None | Some(ShellMenuItem::Separator)) {
                    tidy.push(ShellMenuItem::Separator);
                }
                continue;
            }
            ShellMenuItem::Command { ref verb, ref label, .. } => {
                let own = verb
                    .as_deref()
                    .is_some_and(|verb| hidden.iter().any(|h| h.eq_ignore_ascii_case(verb)));
                if own || label.is_empty() || !labels.insert(label.to_lowercase()) {
                    continue;
                }
                item
            }
            ShellMenuItem::Submenu { label, icon, enabled, items } => {
                let items = tidy_menu(items, hidden);
                if items.is_empty() || label.is_empty() || !labels.insert(label.to_lowercase()) {
                    continue;
                }
                ShellMenuItem::Submenu { label, icon, enabled, items }
            }
        };
        tidy.push(item);
    }
    if matches!(tidy.last(), Some(ShellMenuItem::Separator)) {
        tidy.pop();
    }
    tidy
}

/// Меню Windows для встраивания в своё меню. Собирает меню Shell для `target` (с пунктами
/// сторонних расширений, подменю «Отправить», «Создать», «Открыть с помощью» заполняются
/// сразу) и отдаёт пункты в `ready`; затем ждёт номер выбранной команды из `commands` и
/// выполняет её. Канал закрыт — меню закрыли, ничего не выбрав.
///
/// Блокирует, пока ждёт; вызывать из фонового потока: объекты меню живут в нём (STA).
/// Ошибка сборки меню уходит в `ready`, ошибка выполнения команды — в результат.
pub fn live_menu(
    target: &MenuTarget,
    commands: &crossbeam_channel::Receiver<u32>,
    ready: impl FnOnce(Result<Vec<ShellMenuItem>, String>),
) -> Result<MenuChoice, String> {
    #[cfg(windows)]
    {
        crate::win::menu::live_menu(target, commands, ready)
    }
    #[cfg(not(windows))]
    {
        let _ = (target, commands);
        ready(Err("меню Windows есть только в Windows".into()));
        Ok(MenuChoice::Dismissed)
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

    #[test]
    fn menu_labels_lose_mnemonics_and_shortcuts() {
        assert_eq!(menu_label("&Открыть"), "Открыть");
        assert_eq!(menu_label("Tom && Jerry"), "Tom & Jerry");
        assert_eq!(menu_label("Вы&резать\tCtrl+X"), "Вырезать");
        assert_eq!(menu_label("7-Zip"), "7-Zip");
    }

    fn command(id: u32, label: &str, verb: Option<&str>) -> ShellMenuItem {
        ShellMenuItem::Command {
            id,
            label: label.into(),
            verb: verb.map(Into::into),
            icon: None,
            enabled: true,
        }
    }

    #[test]
    fn tidy_menu_drops_own_commands_repeats_and_extra_separators() {
        let items = vec![
            ShellMenuItem::Separator,
            command(1, "Открыть", Some("open")),
            command(2, "Разблокировать", None),
            ShellMenuItem::Separator,
            ShellMenuItem::Separator,
            command(3, "Разблокировать", Some("Locksmith")),
            command(4, "Вырезать", Some("CUT")),
            ShellMenuItem::Submenu {
                label: "Пусто".into(),
                icon: None,
                enabled: true,
                items: vec![ShellMenuItem::Separator, command(5, "Удалить", Some("delete"))],
            },
            ShellMenuItem::Submenu {
                label: "7-Zip".into(),
                icon: None,
                enabled: true,
                items: vec![command(6, "Распаковать", Some("7-Zip.Extract"))],
            },
            ShellMenuItem::Separator,
        ];
        let tidy = tidy_menu(items, &["open", "cut", "delete"]);
        assert_eq!(
            tidy,
            vec![
                command(2, "Разблокировать", None),
                ShellMenuItem::Separator,
                ShellMenuItem::Submenu {
                    label: "7-Zip".into(),
                    icon: None,
                    enabled: true,
                    items: vec![command(6, "Распаковать", Some("7-Zip.Extract"))],
                },
            ]
        );
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
