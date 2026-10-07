//! План сортировки (какой файл куда поедет), итог операции и журнал для отмены. Формат журнала
//! тот же, что у MH Sort.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::config::Mode;

/// Что и как сканировать.
#[derive(Clone, Debug, PartialEq)]
pub struct ScanOptions {
    pub root: PathBuf,
    /// Куда складывать категории; обычно совпадает с `root`.
    pub output: PathBuf,
    pub recursive: bool,
    pub type_folders: bool,
    pub skip_sorted: bool,
    pub skip_hidden: bool,
    pub detect_content: bool,
    /// Режим копирования: файлы, чья копия уже лежит на месте, пропускаются.
    pub copy: bool,
    /// Имена папок, в которые не заходим.
    pub excluded: Vec<String>,
    /// Файлы и папки самой программы — их не трогаем никогда.
    pub protected: Vec<PathBuf>,
}

#[derive(Clone, Debug)]
pub struct PlannedMove {
    pub src: PathBuf,
    /// Ожидаемый путь назначения (с учётом совпадающих имён).
    pub dst: PathBuf,
    pub category: String,
    pub ext: String,
    pub size: u64,
    pub by_content: bool,
    /// Галочка в предпросмотре.
    pub enabled: bool,
}

impl PlannedMove {
    /// Папка типа внутри категории для показа: «Видео › MP4».
    pub fn destination_label(&self, output: &Path) -> String {
        let parent = self.dst.parent().unwrap_or(&self.dst);
        let relative = parent.strip_prefix(output).unwrap_or(parent);
        relative
            .iter()
            .map(|part| part.to_string_lossy().into_owned())
            .collect::<Vec<_>>()
            .join(" › ")
    }
}

#[derive(Debug, Default, Clone)]
pub struct Plan {
    pub root: PathBuf,
    pub output: PathBuf,
    pub moves: Vec<PlannedMove>,
    /// Файлы, которые уже лежат в своей папке.
    pub in_place: usize,
    /// Скрытые, системные, недокачанные и прочие пропущенные файлы.
    pub ignored: usize,
    /// Папки, которые не удалось прочитать.
    pub errors: Vec<String>,
}

/// Сводка по категории для списка слева и полосы состава.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CategoryStat {
    pub name: String,
    pub files: usize,
    pub bytes: u64,
    /// Сколько файлов категории отмечено.
    pub selected: usize,
}

impl Plan {
    /// Категории в порядке `order` (как в categories.json), незнакомые — в конце.
    pub fn stats(&self, order: &[String]) -> Vec<CategoryStat> {
        let mut stats: Vec<CategoryStat> = Vec::new();
        for planned in &self.moves {
            let index = match stats.iter().position(|s| s.name == planned.category) {
                Some(index) => index,
                None => {
                    stats.push(CategoryStat {
                        name: planned.category.clone(),
                        files: 0,
                        bytes: 0,
                        selected: 0,
                    });
                    stats.len() - 1
                }
            };
            let stat = &mut stats[index];
            stat.files += 1;
            stat.bytes += planned.size;
            stat.selected += usize::from(planned.enabled);
        }
        let rank = |name: &str| order.iter().position(|o| o == name).unwrap_or(usize::MAX);
        stats.sort_by(|a, b| rank(&a.name).cmp(&rank(&b.name)).then_with(|| a.name.cmp(&b.name)));
        stats
    }

    /// Отмечено файлов, их объём и сколько разных папок назначения.
    pub fn selection(&self) -> (usize, u64, usize) {
        let mut dirs = std::collections::HashSet::new();
        let (mut count, mut bytes) = (0, 0);
        for planned in self.moves.iter().filter(|m| m.enabled) {
            count += 1;
            bytes += planned.size;
            dirs.insert(planned.dst.parent().map(Path::to_path_buf));
        }
        (count, bytes, dirs.len())
    }
}

/// Строка журнала в окне.
#[derive(Debug, Clone)]
pub struct LogLine {
    pub ok: bool,
    pub text: String,
}

/// Что было сделано.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    Sort(Mode),
    Undo(Mode),
}

/// Итог операции для окна.
#[derive(Debug, Clone)]
pub struct Report {
    pub action: Action,
    /// Сколько файлов собирались обработать.
    pub total: usize,
    pub done: usize,
    pub failed: usize,
    pub cancelled: bool,
    pub lines: Vec<LogLine>,
    /// Важное замечание, например «журнал не сохранился».
    pub note: Option<String>,
}

impl Report {
    pub fn new(action: Action, total: usize) -> Self {
        Self { action, total, done: 0, failed: 0, cancelled: false, lines: Vec::new(), note: None }
    }

    pub fn ok(&mut self, text: String) {
        self.done += 1;
        self.lines.push(LogLine { ok: true, text });
    }

    pub fn fail(&mut self, text: String) {
        self.failed += 1;
        self.lines.push(LogLine { ok: false, text });
    }

    /// Заголовок итога: «Разложено 12 файлов», «Отменено: 3 из 5».
    pub fn title(&self) -> String {
        let files =
            |n: usize| format!("{n} {}", crate::format::plural(n, "файл", "файла", "файлов"));
        let verb = match self.action {
            Action::Sort(Mode::Move) => "Разложено",
            Action::Sort(Mode::Copy) => "Скопировано",
            Action::Undo(_) => "Возвращено",
        };
        if self.cancelled {
            format!("Остановлено: {verb} {} из {}", self.done, self.total)
        } else if self.failed > 0 {
            format!("{verb} {}, ошибок: {}", files(self.done), self.failed)
        } else {
            format!("{verb} {}", files(self.done))
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Entry {
    pub from: PathBuf,
    pub to: PathBuf,
    pub size: u64,
}

/// Журнал операции: по нему работает отмена.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Journal {
    /// Время операции для показа, например «26.09.2026 14:03:12».
    pub created: String,
    pub source: PathBuf,
    pub output: PathBuf,
    pub mode: Mode,
    #[serde(default)]
    pub undone: bool,
    pub entries: Vec<Entry>,
    /// Папки, которых не было до сортировки (при отмене удаляются, если пустые).
    #[serde(default)]
    pub created_dirs: Vec<PathBuf>,
    /// Опустевшие папки, удалённые после сортировки (при отмене создаются снова).
    #[serde(default)]
    pub removed_dirs: Vec<PathBuf>,
}

impl Journal {
    pub fn new(source: &Path, output: &Path, mode: Mode) -> Self {
        Self {
            created: chrono::Local::now().format("%d.%m.%Y %H:%M:%S").to_string(),
            source: source.to_path_buf(),
            output: output.to_path_buf(),
            mode,
            undone: false,
            entries: Vec::new(),
            created_dirs: Vec::new(),
            removed_dirs: Vec::new(),
        }
    }

    /// Что отменит этот журнал — для подписи кнопки.
    pub fn describe(&self) -> String {
        let n = self.entries.len();
        let what =
            if self.mode == Mode::Move { "сортировку" } else { "копирование" };
        let files = crate::format::plural(n, "файла", "файлов", "файлов");
        format!("{what} {n} {files} ({})", self.created)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn planned(category: &str, dst: &str, size: u64, enabled: bool) -> PlannedMove {
        PlannedMove {
            src: PathBuf::from("/d/x"),
            dst: PathBuf::from(dst),
            category: category.into(),
            ext: String::new(),
            size,
            by_content: false,
            enabled,
        }
    }

    #[test]
    fn stats_and_selection() {
        let plan = Plan {
            root: "/d".into(),
            output: "/d".into(),
            moves: vec![
                planned("Видео", "/d/Видео/MP4/a.mp4", 10, true),
                planned("Аудио", "/d/Аудио/MP3/b.mp3", 5, false),
                planned("Видео", "/d/Видео/MKV/c.mkv", 20, true),
            ],
            ..Plan::default()
        };
        let stats = plan.stats(&["Аудио".into(), "Видео".into()]);
        assert_eq!(stats[0].name, "Аудио", "порядок из categories.json");
        let stats = plan.stats(&[]);
        assert_eq!(stats[1].name, "Видео");
        assert_eq!((stats[1].files, stats[1].bytes, stats[1].selected), (2, 30, 2));
        assert_eq!(plan.selection(), (2, 30, 2));
        assert_eq!(plan.moves[0].destination_label(Path::new("/d")), "Видео › MP4");
    }

    #[test]
    fn report_titles() {
        let mut report = Report::new(Action::Sort(Mode::Move), 3);
        report.ok("a".into());
        report.ok("b".into());
        assert_eq!(report.title(), "Разложено 2 файла");
        report.fail("c".into());
        assert_eq!(report.title(), "Разложено 2 файла, ошибок: 1");
        let journal: Journal = serde_json::from_str(
            r#"{"created":"1","source":"/a","output":"/a","mode":"copy","entries":[]}"#,
        )
        .unwrap();
        assert_eq!(journal.mode, Mode::Copy);
        assert!(!journal.undone);
    }
}
