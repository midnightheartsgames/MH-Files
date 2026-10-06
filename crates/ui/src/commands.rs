//! Реестр команд. Кнопки, контекстное меню, палитра команд и горячие клавиши не содержат своей
//! логики: все они называют [`CommandId`], а выполняет его одно место — `actions.rs`.

use std::collections::BTreeMap;

use eframe::egui::{Key, KeyboardShortcut, Modifiers};

macro_rules! commands {
    ($( $id:ident => $name:literal, [$($alias:literal),*], [$($key:literal),*]; )*) => {
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
        pub enum CommandId { $($id),* }

        impl CommandId {
            pub const ALL: &'static [CommandId] = &[$(CommandId::$id),*];

            /// Имя для людей.
            pub fn name(self) -> &'static str {
                match self { $(CommandId::$id => $name),* }
            }

            /// Устойчивое имя для файла настроек.
            pub fn key(self) -> &'static str {
                match self { $(CommandId::$id => stringify!($id)),* }
            }

            pub fn from_key(key: &str) -> Option<CommandId> {
                match key { $(stringify!($id) => Some(CommandId::$id),)* _ => None }
            }

            /// Другие слова, по которым команду находит палитра.
            pub fn aliases(self) -> &'static [&'static str] {
                match self { $(CommandId::$id => &[$($alias),*]),* }
            }

            pub fn default_keys(self) -> &'static [&'static str] {
                match self { $(CommandId::$id => &[$($key),*]),* }
            }
        }
    };
}

commands! {
    Open => "Открыть", ["open", "запустить"], ["Enter"];
    OpenWith => "Открыть с помощью…", ["open with"], [];
    OpenInNewTab => "Открыть в новой вкладке", ["new tab"], ["Ctrl+Enter"];
    OpenInOtherPane => "Открыть в соседней панели", ["other pane"], ["Shift+Enter"];
    GoBack => "Назад", ["back"], ["Alt+Left", "Backspace"];
    GoForward => "Вперёд", ["forward"], ["Alt+Right"];
    GoUp => "Вверх на уровень", ["up", "parent"], ["Alt+Up"];
    GoComputer => "Этот компьютер", ["drives", "диски"], [];
    GoHome => "Домашняя папка", ["home"], [];
    Refresh => "Обновить", ["reload", "refresh"], ["F5", "Ctrl+R"];
    EditAddress => "Ввести путь", ["address", "path", "адрес"], ["Ctrl+L", "Alt+D"];
    GoTo => "Перейти к папке (GoTo)", ["goto", "jump"], ["Ctrl+G", "Ctrl+P"];
    CommandPalette => "Палитра команд", ["commands", "palette"], ["Ctrl+Shift+P", "F1"];
    Filter => "Фильтр папки", ["filter", "find"], ["Ctrl+F"];
    Search => "Поиск во вложенных папках", ["search", "найти"], ["Ctrl+Shift+F"];
    Copy => "Копировать", ["copy"], ["Ctrl+C"];
    Cut => "Вырезать", ["cut"], ["Ctrl+X"];
    Paste => "Вставить", ["paste"], ["Ctrl+V"];
    CopyToOtherPane => "Копировать в соседнюю панель", ["copy to"], [];
    MoveToOtherPane => "Переместить в соседнюю панель", ["move to"], [];
    CopyPath => "Копировать путь", ["copy path"], ["Ctrl+Shift+C"];
    CopyName => "Копировать имя", ["copy name"], [];
    Rename => "Переименовать", ["rename"], ["F2"];
    BatchRename => "Пакетное переименование", ["batch rename", "mass rename"], ["Ctrl+Shift+R"];
    Delete => "Удалить в корзину", ["delete", "trash", "recycle"], ["Delete"];
    DeletePermanent => "Удалить насовсем", ["delete permanently", "shred"], ["Shift+Delete"];
    NewFolder => "Новая папка", ["new folder", "mkdir"], ["Ctrl+Shift+N"];
    Properties => "Свойства", ["properties"], ["Alt+Enter"];
    OpenTerminal => "Открыть терминал здесь", ["terminal", "console", "powershell"], ["Ctrl+Shift+T"];
    RevealInExplorer => "Показать в Проводнике", ["explorer", "reveal"], [];
    AddFavorite => "Добавить в избранное", ["favorite", "bookmark"], ["Ctrl+D"];
    SelectAll => "Выделить всё", ["select all"], ["Ctrl+A"];
    InvertSelection => "Обратить выделение", ["invert"], ["Ctrl+I"];
    ClearSelection => "Снять выделение", ["deselect"], [];
    QuickLook => "Быстрый просмотр", ["preview", "quick look"], ["Space"];
    ViewDetails => "Вид: таблица", ["details", "list"], ["Ctrl+1"];
    ViewGrid => "Вид: плитки", ["grid", "thumbnails"], ["Ctrl+2"];
    ToggleHidden => "Показывать скрытые", ["hidden"], ["Ctrl+H"];
    ToggleInspector => "Инспектор", ["inspector", "preview pane"], ["Alt+P"];
    ToggleSidebar => "Боковая панель", ["sidebar"], ["Ctrl+B"];
    ZoomIn => "Крупнее", ["zoom in"], ["Ctrl+Plus", "Ctrl+Equals"];
    ZoomOut => "Мельче", ["zoom out"], ["Ctrl+Minus"];
    ZoomReset => "Обычный масштаб", ["zoom reset"], ["Ctrl+0"];
    NewTab => "Новая вкладка", ["new tab"], ["Ctrl+T"];
    CloseTab => "Закрыть вкладку", ["close tab"], ["Ctrl+W", "Ctrl+F4"];
    ReopenTab => "Вернуть закрытую вкладку", ["reopen"], ["Ctrl+Alt+T"];
    DuplicateTab => "Дублировать вкладку", ["duplicate tab"], [];
    NextTab => "Следующая вкладка", ["next tab"], ["Ctrl+Tab", "Ctrl+PageDown"];
    PrevTab => "Предыдущая вкладка", ["previous tab"], ["Ctrl+Shift+Tab", "Ctrl+PageUp"];
    SplitRight => "Разделить: панель справа", ["split vertical", "dual pane"], ["Ctrl+Backslash"];
    SplitDown => "Разделить: панель снизу", ["split horizontal"], ["Ctrl+Shift+Backslash"];
    ClosePane => "Закрыть панель", ["close pane"], ["Ctrl+Shift+W"];
    NextPane => "Следующая панель", ["switch pane"], ["Tab", "F6"];
    Settings => "Настройки", ["settings", "options", "preferences"], ["Ctrl+Comma"];
}

impl CommandId {
    /// Нужен ли выделенный объект.
    pub fn needs_targets(self) -> bool {
        use CommandId::*;
        matches!(
            self,
            Open | OpenWith
                | OpenInNewTab
                | OpenInOtherPane
                | Copy
                | Cut
                | CopyToOtherPane
                | MoveToOtherPane
                | CopyPath
                | CopyName
                | Rename
                | BatchRename
                | Delete
                | DeletePermanent
                | Properties
                | RevealInExplorer
                | QuickLook
        )
    }
}

/// Разбирает `Ctrl+Shift+P`. Регистр и пробелы не важны.
pub fn parse_shortcut(text: &str) -> Option<KeyboardShortcut> {
    let mut modifiers = Modifiers::NONE;
    let mut key = None;
    for part in text.split('+').map(str::trim) {
        match part.to_ascii_lowercase().as_str() {
            "ctrl" | "control" => modifiers.ctrl = true,
            "shift" => modifiers.shift = true,
            "alt" => modifiers.alt = true,
            "" => return None,
            _ => {
                if key.is_some() {
                    return None;
                }
                key = Key::from_name(part).or_else(|| Key::from_name(&capitalize(part)));
                key?;
            }
        }
    }
    // На Windows Ctrl — это и есть «command» egui.
    modifiers.command = modifiers.ctrl;
    Some(KeyboardShortcut::new(modifiers, key?))
}

fn capitalize(text: &str) -> String {
    let mut chars = text.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars.flat_map(char::to_lowercase)).collect(),
        None => String::new(),
    }
}

/// Сочетание для показа: `Ctrl+Shift+P`.
pub fn format_shortcut(shortcut: &KeyboardShortcut) -> String {
    let mut parts = Vec::new();
    if shortcut.modifiers.ctrl || shortcut.modifiers.command {
        parts.push("Ctrl".to_string());
    }
    if shortcut.modifiers.alt {
        parts.push("Alt".into());
    }
    if shortcut.modifiers.shift {
        parts.push("Shift".into());
    }
    let key = match shortcut.logical_key {
        Key::ArrowLeft => "Left".to_string(),
        Key::ArrowRight => "Right".into(),
        Key::ArrowUp => "Up".into(),
        Key::ArrowDown => "Down".into(),
        Key::Plus | Key::Equals => "+".into(),
        Key::Minus => "-".into(),
        Key::Comma => ",".into(),
        Key::Backslash => "\\".into(),
        key => key.name().to_string(),
    };
    parts.push(key);
    parts.join("+")
}

/// Действующие сочетания: по умолчанию плюс свои из настроек.
#[derive(Debug, Clone, Default)]
pub struct Keymap {
    bindings: Vec<(KeyboardShortcut, CommandId)>,
}

impl Keymap {
    /// Свои сочетания заменяют стандартные для своей команды. Неразобранные пропускаются и
    /// возвращаются текстом ошибки.
    pub fn new(overrides: &BTreeMap<String, Vec<String>>) -> (Keymap, Vec<String>) {
        let mut bindings = Vec::new();
        let mut errors: Vec<String> = overrides
            .keys()
            .filter(|key| CommandId::from_key(key).is_none())
            .map(|key| format!("неизвестная команда «{key}»"))
            .collect();
        for &command in CommandId::ALL {
            let texts: Vec<String> = match overrides.get(command.key()) {
                Some(custom) => custom.clone(),
                None => command.default_keys().iter().map(|s| s.to_string()).collect(),
            };
            for text in texts {
                match parse_shortcut(&text) {
                    Some(shortcut) => bindings.push((shortcut, command)),
                    None => errors.push(format!("«{text}» ({})", command.name())),
                }
            }
        }
        (Keymap { bindings }, errors)
    }

    pub fn shortcuts(&self, command: CommandId) -> impl Iterator<Item = &KeyboardShortcut> {
        self.bindings.iter().filter(move |(_, c)| *c == command).map(|(s, _)| s)
    }

    /// Первое сочетание команды для подписи в меню.
    pub fn label(&self, command: CommandId) -> String {
        self.shortcuts(command).next().map(format_shortcut).unwrap_or_default()
    }

    /// Все сочетания через запятую — для настроек.
    pub fn text(&self, command: CommandId) -> String {
        self.shortcuts(command).map(format_shortcut).collect::<Vec<_>>().join(", ")
    }

    /// Команда для нажатия с точным набором модификаторов.
    pub fn lookup(&self, key: Key, modifiers: Modifiers) -> Option<CommandId> {
        let ctrl = modifiers.ctrl || modifiers.command;
        self.bindings
            .iter()
            .find(|(s, _)| {
                s.logical_key == key
                    && (s.modifiers.ctrl || s.modifiers.command) == ctrl
                    && s.modifiers.shift == modifiers.shift
                    && s.modifiers.alt == modifiers.alt
            })
            .map(|(_, c)| *c)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_default_shortcut_parses_and_is_unique() {
        let (keymap, errors) = Keymap::new(&BTreeMap::new());
        assert!(errors.is_empty(), "{errors:?}");
        let mut seen = std::collections::HashSet::new();
        for (shortcut, command) in &keymap.bindings {
            assert!(seen.insert(format_shortcut(shortcut)), "повтор у {command:?}");
        }
    }

    #[test]
    fn parsing_and_lookup() {
        let shortcut = parse_shortcut("ctrl + shift + p").unwrap();
        assert_eq!(format_shortcut(&shortcut), "Ctrl+Shift+P");
        assert!(parse_shortcut("Ctrl+").is_none());
        assert!(parse_shortcut("Ctrl+P+Q").is_none());
        let mut overrides = BTreeMap::new();
        overrides.insert("Rename".to_string(), vec!["Ctrl+E".to_string()]);
        let (keymap, _) = Keymap::new(&overrides);
        let ctrl = Modifiers { ctrl: true, command: true, ..Modifiers::NONE };
        assert_eq!(keymap.lookup(Key::E, ctrl), Some(CommandId::Rename));
        assert_eq!(keymap.lookup(Key::F2, Modifiers::NONE), None, "свои заменяют стандартные");
        assert_eq!(CommandId::from_key("BatchRename"), Some(CommandId::BatchRename));
    }
}
