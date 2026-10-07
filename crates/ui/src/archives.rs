//! Архивы и дубликаты в окне: извлечение (команды, перетаскивание, открытие файла из
//! архива, Ctrl+C) и поиск дубликатов с отметкой лишних копий.

use std::path::{Path, PathBuf};

use mh_files_core::duplicates::Keep;
use mh_files_core::location::Location;
use mh_files_fs::archive::{is_archive_name, unique_path};
use mh_files_fs::{AfterExtract, CancelToken, Extraction, ShellJob};

use crate::app::{FilesApp, Level, Target};

/// Идущее извлечение — для строки состояния.
pub struct ExtractView {
    pub id: u64,
    pub label: String,
    pub done: u64,
    pub total: u64,
    pub cancel: CancelToken,
}

/// Папка для файлов, извлечённых, чтобы открыть их или положить в буфер обмена.
fn temp_dir(id: u64) -> PathBuf {
    let name = if cfg!(windows) { "MH Files" } else { "mh-files" };
    std::env::temp_dir().join(name).join("archives").join(id.to_string())
}

/// Путь внутри архива для записи из вкладки архива.
fn inner_of(archive: &Path, path: &Path) -> Option<String> {
    let rest = path.strip_prefix(archive).ok()?;
    let parts: Vec<String> = rest.iter().map(|p| p.to_string_lossy().into_owned()).collect();
    Some(parts.join("/"))
}

impl FilesApp {
    /// Архив текущей вкладки и пути внутри него для выделенного (без выделения — вся
    /// открытая папка архива).
    pub fn archive_selection(&self) -> Option<(PathBuf, Vec<String>)> {
        let Location::Archive { archive, inner } = &self.tab().location else { return None };
        let targets = self.tab().targets();
        let inners = if targets.is_empty() {
            vec![inner.clone()]
        } else {
            targets.iter().filter_map(|p| inner_of(archive, p)).collect()
        };
        Some((archive.clone(), inners))
    }

    pub fn start_extract(
        &mut self,
        archive: PathBuf,
        inners: Vec<String>,
        dest: PathBuf,
        then: AfterExtract,
    ) {
        self.next_extract += 1;
        let id = self.next_extract;
        let dest = if then == AfterExtract::Report { dest } else { temp_dir(id) };
        let label = format!("Извлечение из {}", mh_files_core::location::path_label(&archive));
        let cancel = self.workers.extract(Extraction { id, archive, inners, dest, then });
        self.extractions.push(ExtractView { id, label, done: 0, total: 0, cancel });
    }

    pub fn on_extract_progress(&mut self, id: u64, done: u64, total: u64) {
        if let Some(view) = self.extractions.iter_mut().find(|v| v.id == id) {
            view.done = done;
            view.total = total;
        }
    }

    pub fn on_extracted(&mut self, extraction: Extraction, result: Result<Vec<PathBuf>, String>) {
        self.extractions.retain(|v| v.id != extraction.id);
        let paths = match result {
            Ok(paths) => paths,
            Err(error) => {
                self.set_status(format!("не извлечено: {error}"), Level::Error);
                return;
            }
        };
        match extraction.then {
            AfterExtract::Report => {
                self.set_status(
                    format!(
                        "извлечено: {} в «{}»",
                        mh_files_core::format::items(paths.len()),
                        mh_files_core::location::path_label(&extraction.dest)
                    ),
                    Level::Info,
                );
                let mut dirs = vec![extraction.dest.clone()];
                dirs.extend(extraction.dest.parent().map(PathBuf::from));
                self.reload_dirs(&dirs);
            }
            AfterExtract::Open => {
                for path in paths {
                    self.workers.shell(ShellJob::Open(path));
                }
            }
            AfterExtract::Clipboard => {
                let count = paths.len();
                self.workers.shell(ShellJob::SetClipboard { paths, cut: false });
                self.set_status(
                    format!("скопировано из архива: {}", mh_files_core::format::items(count)),
                    Level::Info,
                );
            }
        }
    }

    /// «Извлечь…»: из открытого архива — в соседнюю панель (или рядом с архивом); архивы,
    /// выделенные в папке, — каждый в свою папку рядом (или в соседнюю панель).
    pub fn extract_command(&mut self, beside: bool) {
        let other = if beside {
            None
        } else {
            self.other_pane().and_then(|id| self.pane(id)).and_then(|p| p.tab().dir())
        };
        if let Some((archive, inners)) = self.archive_selection() {
            let dest = other.unwrap_or_else(|| {
                let parent = archive.parent().unwrap_or(Path::new("")).to_path_buf();
                unique_path(&parent.join(stem(&archive)))
            });
            self.start_extract(archive, inners, dest, AfterExtract::Report);
            return;
        }
        let archives: Vec<PathBuf> = self
            .tab()
            .targets()
            .into_iter()
            .filter(|p| p.file_name().is_some_and(|n| is_archive_name(&n.to_string_lossy())))
            .collect();
        if archives.is_empty() {
            self.set_status("выделите архив zip или 7z", Level::Info);
            return;
        }
        for archive in archives {
            let base = other
                .clone()
                .unwrap_or_else(|| archive.parent().unwrap_or(Path::new("")).to_path_buf());
            let dest = unique_path(&base.join(stem(&archive)));
            self.start_extract(archive, vec![String::new()], dest, AfterExtract::Report);
        }
    }

    /// Файлы, брошенные из архива в папку, — извлечь туда.
    pub fn extract_drop(&mut self, archive: PathBuf, paths: Vec<PathBuf>, dest: PathBuf) {
        let inners: Vec<String> = paths.iter().filter_map(|p| inner_of(&archive, p)).collect();
        if !inners.is_empty() {
            self.start_extract(archive, inners, dest, AfterExtract::Report);
        }
    }

    /// Открыть объекты внутри архива: папки — переходом, файлы — через временную папку.
    pub fn open_in_archive(&mut self, targets: &[PathBuf], target: Target) -> bool {
        let Location::Archive { archive, .. } = self.tab().location.clone() else { return false };
        let mut files = Vec::new();
        for path in targets {
            match self.tab().entry(path) {
                Some(entry) if entry.is_dir() => {
                    let location = self.tab().location.enter(path);
                    self.open_location(location, target);
                }
                // Архив в архиве открывается как папка: путь — через внешний архив.
                Some(entry) if mh_files_fs::archive::is_archive_name(&entry.name) => {
                    let location =
                        Location::Archive { archive: path.clone(), inner: String::new() };
                    self.open_location(location, target);
                }
                Some(_) => files.extend(inner_of(&archive, path)),
                None => {}
            }
        }
        if !files.is_empty() {
            self.start_extract(archive, files, PathBuf::new(), AfterExtract::Open);
        }
        true
    }

    /// Ctrl+C в архиве: извлечь во временную папку и положить в буфер обмена.
    pub fn copy_from_archive(&mut self) {
        if let Some((archive, inners)) = self.archive_selection() {
            self.start_extract(archive, inners, PathBuf::new(), AfterExtract::Clipboard);
        }
    }

    /// Найти дубликаты в выделенных папках или в текущей — в новой вкладке.
    pub fn find_duplicates(&mut self) {
        let tab = self.tab();
        let mut roots: Vec<PathBuf> = tab
            .targets()
            .into_iter()
            .filter(|p| tab.entry(p).is_some_and(|e| e.is_dir()))
            .collect();
        if roots.is_empty() {
            roots.extend(tab.dir());
        }
        if roots.is_empty() {
            self.set_status("откройте папку, в которой искать дубликаты", Level::Info);
            return;
        }
        self.open_location(Location::Duplicates { roots }, Target::NewTab);
    }

    /// Выделить все копии, кроме одной в каждой группе.
    pub fn select_extra_copies(&mut self, keep: Keep) {
        let tab = self.tab_mut();
        let Some(view) = &tab.duplicates else { return };
        let extra: std::collections::HashSet<PathBuf> =
            view.groups.iter().flat_map(|g| g.extra(keep).cloned()).collect();
        let count = extra.len();
        tab.selection.set_rows(&tab.listing, [], &extra);
        self.set_status(
            format!(
                "отмечено лишних копий: {count} ({}) — Delete отправит их в корзину",
                keep.title()
            ),
            Level::Info,
        );
    }
}

fn stem(path: &Path) -> String {
    path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_else(|| "архив".into())
}
