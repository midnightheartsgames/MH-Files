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
    GoComputer => "Этот компьютер", ["drives", "диски"], ["Alt+G C"];
    GoHome => "Домашняя папка", ["home"], ["Alt+G H"];
    GoDesktop => "Рабочий стол", ["desktop"], ["Alt+G E"];
    GoDocuments => "Документы", ["documents"], ["Alt+G O"];
    GoDownloads => "Загрузки", ["downloads"], ["Alt+G D"];
    GoPictures => "Изображения", ["pictures"], ["Alt+G P"];
    Refresh => "Обновить", ["reload", "refresh"], ["F5", "Ctrl+R"];
    EditAddress => "Ввести путь", ["address", "path", "адрес"], ["Ctrl+L", "Alt+D"];
    GoTo => "Перейти к папке (GoTo)", ["goto", "jump"], ["Ctrl+G", "Ctrl+P"];
    CommandPalette => "Палитра команд", ["commands", "palette"], ["Ctrl+Shift+P", "F1"];
    Filter => "Фильтр папки", ["filter", "find"], ["Ctrl+F"];
        Search => "Поиск во вложенных папках", ["search", "найти"], ["Ctrl+Shift+F"];
    SearchEverywhere => "Поиск по дискам", ["everything", "везде", "index", "индекс"], ["Ctrl+E"];
    SaveSearch => "Сохранить поиск", ["save search", "запомнить"], [];
    Reindex => "Переиндексировать диски", ["reindex", "rescan", "индекс"], [];
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
    WindowsMenu => "Меню Windows", ["context menu", "shell menu", "7-zip", "git"], ["Shift+F10"];
    Undo => "Отменить", ["undo"], ["Ctrl+Z"];
    FolderSizes => "Посчитать размеры папок", ["folder size", "размер"], ["Ctrl+Shift+S"];
    OpenTerminal => "Открыть терминал здесь", ["terminal", "console", "powershell"], ["Ctrl+Shift+T"];
    RevealInExplorer => "Показать в Проводнике", ["explorer", "reveal"], [];
    AddFavorite => "Добавить в избранное", ["favorite", "bookmark"], ["Ctrl+D"];
    SelectAll => "Выделить всё", ["select all"], ["Ctrl+A"];
    InvertSelection => "Обратить выделение", ["invert"], ["Ctrl+I"];
    ClearSelection => "Снять выделение", ["deselect"], [];
    QuickLook => "Быстрый просмотр", ["preview", "quick look"], ["Space"];
    ViewDetails => "Вид: таблица", ["details", "list"], ["Ctrl+1"];
        ViewGrid => "Вид: плитки", ["grid", "thumbnails"], ["Ctrl+2"];
    ViewColumns => "Вид: колонки", ["columns", "miller", "колонки миллера"], ["Ctrl+3"];
    Extract => "Извлечь…", ["extract", "unzip", "распаковать"], ["Ctrl+Shift+E"];
    ExtractHere => "Извлечь в папку рядом", ["extract here", "unzip here", "распаковать сюда"], [];
        FindDuplicates => "Найти дубликаты", ["duplicates", "дубли", "одинаковые"], [];
    SortFolder => "Разложить по папкам", ["sort", "mh sort", "сортировщик", "разобрать", "organize"], ["Ctrl+Shift+O"];
    SelectExtraCopies => "Отметить лишние копии", ["select duplicates", "лишние"], [];
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
    // «Ctrl++» — клавиша «плюс»: так её пишет `format_shortcut`.
    let text = text.trim();
    let (text, plus) = match text.strip_suffix("++") {
        Some(rest) => (rest, true),
        None => (text, false),
    };
    if plus {
        key = Some(Key::Plus);
    }
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
                key = symbol_key(part)
                    .or_else(|| Key::from_name(part))
                    .or_else(|| Key::from_name(&capitalize(part)));
                key?;
            }
        }
    }
    // На Windows Ctrl — это и есть «command» egui.
    modifiers.command = modifiers.ctrl;
    Some(KeyboardShortcut::new(modifiers, key?))
}

/// Клавиши, которые `format_shortcut` пишет знаком.
fn symbol_key(text: &str) -> Option<Key> {
    Some(match text {
        "+" => Key::Plus,
        "=" => Key::Equals,
        "-" => Key::Minus,
        "," => Key::Comma,
        "\\" => Key::Backslash,
        _ => return None,
    })
}

/// Список сочетаний через запятую: «Ctrl+C, Ctrl+Insert». Запятая сразу после «+» — это
/// клавиша («Ctrl+,»), а не разделитель; после «++» (клавиша «плюс») — снова разделитель.
pub fn split_chords(text: &str) -> Vec<&str> {
    let mut parts = Vec::new();
    let mut start = 0;
    // Сколько «+» подряд стоит перед текущим знаком (пробелы не считаются).
    let mut pluses = 0;
    for (index, c) in text.char_indices() {
        if c == ',' && pluses % 2 == 0 {
            parts.push(text[start..index].trim());
            start = index + 1;
        }
        if c == '+' {
            pluses += 1;
        } else if !c.is_whitespace() {
            pluses = 0;
        }
    }
    parts.push(text[start..].trim());
    parts.retain(|p| !p.is_empty());
    parts
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
        Key::Plus => "+".into(),
        Key::Equals => "=".into(),
        Key::Minus => "-".into(),
        Key::Comma => ",".into(),
        Key::Backslash => "\\".into(),
        key => key.name().to_string(),
    };
    parts.push(key);
    parts.join("+")
}

/// Последовательность: одно сочетание или два подряд (`Alt+G D`).
pub type Chord = Vec<KeyboardShortcut>;

/// `Alt+G D` → два шага. Шаги разделяются пробелом.
pub fn parse_chord(text: &str) -> Option<Chord> {
    let steps: Option<Chord> = text.split_whitespace().map(parse_shortcut).collect();
    steps.filter(|steps| (1..=2).contains(&steps.len()))
}

pub fn format_chord(chord: &[KeyboardShortcut]) -> String {
    chord.iter().map(format_shortcut).collect::<Vec<_>>().join(" ")
}

/// Итог нажатия.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lookup {
    None,
    Command(CommandId),
    /// Первый шаг последовательности: ждать второй.
    Prefix(KeyboardShortcut),
}

fn matches(shortcut: &KeyboardShortcut, key: Key, modifiers: Modifiers) -> bool {
    let ctrl = modifiers.ctrl || modifiers.command;
    shortcut.logical_key == key
        && (shortcut.modifiers.ctrl || shortcut.modifiers.command) == ctrl
        && shortcut.modifiers.shift == modifiers.shift
        && shortcut.modifiers.alt == modifiers.alt
}

/// Действующие сочетания: по умолчанию плюс свои из настроек.
#[derive(Debug, Clone, Default)]
pub struct Keymap {
    bindings: Vec<(Chord, CommandId)>,
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
                match parse_chord(&text) {
                    Some(chord) => bindings.push((chord, command)),
                    None => errors.push(format!("«{text}» ({})", command.name())),
                }
            }
        }
        (Keymap { bindings }, errors)
    }

    pub fn chords(&self, command: CommandId) -> impl Iterator<Item = &Chord> {
        self.bindings.iter().filter(move |(_, c)| *c == command).map(|(s, _)| s)
    }

    /// Первое сочетание команды для подписи в меню.
    pub fn label(&self, command: CommandId) -> String {
        self.chords(command).next().map(|c| format_chord(c)).unwrap_or_default()
    }

    /// Все сочетания через запятую — для настроек.
    pub fn text(&self, command: CommandId) -> String {
        self.chords(command).map(|c| format_chord(c)).collect::<Vec<_>>().join(", ")
    }

    /// Что значит нажатие. `pending` — первый шаг уже нажатой последовательности.
    pub fn lookup(
        &self,
        pending: Option<&KeyboardShortcut>,
        key: Key,
        modifiers: Modifiers,
    ) -> Lookup {
        if let Some(first) = pending {
            return self
                .bindings
                .iter()
                .find(|(chord, _)| {
                    chord.len() == 2 && chord[0] == *first && matches(&chord[1], key, modifiers)
                })
                .map_or(Lookup::None, |(_, command)| Lookup::Command(*command));
        }
        if let Some((_, command)) = self
            .bindings
            .iter()
            .find(|(chord, _)| chord.len() == 1 && matches(&chord[0], key, modifiers))
        {
            return Lookup::Command(*command);
        }
        self.bindings
            .iter()
            .find(|(chord, _)| chord.len() == 2 && matches(&chord[0], key, modifiers))
            .map_or(Lookup::None, |(chord, _)| Lookup::Prefix(chord[0]))
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
        for (chord, command) in &keymap.bindings {
            assert!(seen.insert(format_chord(chord)), "повтор у {command:?}");
        }
        // Первый шаг последовательности не может быть самостоятельным сочетанием.
        for (chord, command) in keymap.bindings.iter().filter(|(c, _)| c.len() == 2) {
            let first = format_shortcut(&chord[0]);
            assert!(!seen.contains(&first), "{first} у {command:?} — и команда, и начало");
        }
    }

    #[test]
    fn shown_shortcuts_parse_back() {
        // Окно настроек показывает сочетания текстом и разбирает его при «Применить»:
        // всё показанное должно разбираться обратно в то же самое.
        let (keymap, _) = Keymap::new(&BTreeMap::new());
        for &command in CommandId::ALL {
            let text = keymap.text(command);
            let parsed: Vec<String> = split_chords(&text)
                .into_iter()
                .map(|part| {
                    format_chord(
                        &parse_chord(part).unwrap_or_else(|| panic!("{part} у {command:?}")),
                    )
                })
                .collect();
            assert_eq!(parsed.join(", "), text, "{command:?}");
        }
        assert_eq!(split_chords("Ctrl+,, Ctrl+C,Alt+-"), ["Ctrl+,", "Ctrl+C", "Alt+-"]);
        assert_eq!(split_chords("Ctrl++, Ctrl+="), ["Ctrl++", "Ctrl+="]);
        assert_eq!(format_shortcut(&parse_shortcut("Ctrl+Shift++").unwrap()), "Ctrl+Shift++");
        assert!(parse_shortcut("Ctrl+").is_none());
    }

    #[test]
    fn parsing_and_lookup() {
        let shortcut = parse_shortcut("ctrl + shift + p").unwrap();
        assert_eq!(format_shortcut(&shortcut), "Ctrl+Shift+P");
        assert!(parse_shortcut("Ctrl+").is_none());
        assert!(parse_shortcut("Ctrl+P+Q").is_none());
        let mut overrides = BTreeMap::new();
        overrides.insert("Rename".to_string(), vec!["Ctrl+Y".to_string()]);
        let (keymap, _) = Keymap::new(&overrides);
        let ctrl = Modifiers { ctrl: true, command: true, ..Modifiers::NONE };
        assert_eq!(keymap.lookup(None, Key::Y, ctrl), Lookup::Command(CommandId::Rename));
        assert_eq!(
            keymap.lookup(None, Key::F2, Modifiers::NONE),
            Lookup::None,
            "свои заменяют стандартные"
        );
        let alt = Modifiers { alt: true, ..Modifiers::NONE };
        let Lookup::Prefix(first) = keymap.lookup(None, Key::G, alt) else { panic!() };
        assert_eq!(
            keymap.lookup(Some(&first), Key::D, Modifiers::NONE),
            Lookup::Command(CommandId::GoDownloads)
        );
        assert_eq!(keymap.lookup(Some(&first), Key::Z, Modifiers::NONE), Lookup::None);
        assert_eq!(format_chord(&parse_chord("alt+g  d").unwrap()), "Alt+G D");
        assert!(parse_chord("A B C").is_none());
        assert_eq!(CommandId::from_key("BatchRename"), Some(CommandId::BatchRename));
    }
}
