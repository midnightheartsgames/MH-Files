//! Командная строка: `MH-Files.exe [ключи] [путь…]`.
//!
//! * путь к папке — открыть её во вкладке; путь к файлу — открыть его папку и встать на него;
//! * `--new-window` — отдельная копия программы, даже если одна уже открыта;
//! * `--portable` — хранить настройки рядом с exe (как если бы рядом лежал файл `portable`);
//! * `--version` — номер версии.
//!
//! Относительные пути разрешает вызывающий: здесь нет ввода-вывода.

use std::path::PathBuf;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CommandLine {
    pub paths: Vec<PathBuf>,
    pub new_window: bool,
    pub portable: bool,
    pub version: bool,
    /// Незнакомые ключи — для сообщения в строке состояния.
    pub unknown: Vec<String>,
}

impl CommandLine {
    /// Разбор аргументов без имени программы.
    pub fn parse(args: impl IntoIterator<Item = String>) -> CommandLine {
        let mut line = CommandLine::default();
        let mut only_paths = false;
        for arg in args {
            if only_paths {
                line.push_path(arg);
                continue;
            }
            match arg.as_str() {
                "--" => only_paths = true,
                "--new-window" | "-n" | "/n" => line.new_window = true,
                "--portable" => line.portable = true,
                "--version" | "-V" => line.version = true,
                flag if flag.starts_with("--") => line.unknown.push(flag.to_string()),
                _ => line.push_path(arg),
            }
        }
        line
    }

    fn push_path(&mut self, arg: String) {
        // Проводник и ярлыки иногда передают путь в кавычках или с хвостовым пробелом.
        let trimmed = arg.trim().trim_matches('"');
        if !trimmed.is_empty() {
            self.paths.push(PathBuf::from(trimmed));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> CommandLine {
        CommandLine::parse(args.iter().map(|s| s.to_string()))
    }

    #[test]
    fn paths_and_flags() {
        let line = parse(&["--new-window", "D:\\Загрузки", "\"C:\\a b\\\"", "--odd"]);
        assert!(line.new_window);
        assert_eq!(line.paths, [PathBuf::from("D:\\Загрузки"), PathBuf::from("C:\\a b\\")]);
        assert_eq!(line.unknown, ["--odd"]);
        let line = parse(&["--", "--new-window"]);
        assert!(!line.new_window, "после -- всё — пути");
        assert_eq!(line.paths, [PathBuf::from("--new-window")]);
        assert_eq!(parse(&[]), CommandLine::default());
        assert!(parse(&["--portable", "--version"]).portable);
    }
}
