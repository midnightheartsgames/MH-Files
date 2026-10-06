//! Сортировщик из MH Sort: обход папки и план, перемещение или копирование без перезаписи,
//! журнал и отмена по нему. Поведение то же, что у MH Sort; модель — в
//! `mh_files_core::sorting`.
//!
//! Здесь все функции блокирующие — их зовут воркеры ([`crate::Workers::sort_scan`] и
//! соседние), а не поток UI.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use mh_files_core::sorting::names::{is_within, path_key, primary_ext, unique_path};
use mh_files_core::sorting::{
    Action, CONFIG_FILE, Classifier, Config, Entry, Journal, Mode, Plan, PlannedMove, Report,
    ScanOptions,
};
use walkdir::{DirEntry, WalkDir};

/// Сколько журналов хранить.
pub const JOURNAL_KEEP: usize = 100;

/// Существует ли что-нибудь по этому пути (включая битые ссылки).
pub fn exists(path: &Path) -> bool {
    path.symlink_metadata().is_ok()
}

/// Путь относительно `base` для показа пользователю.
fn rel(path: &Path, base: &Path) -> String {
    path.strip_prefix(base).unwrap_or(path).display().to_string()
}

fn home_dir() -> Option<PathBuf> {
    std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" }).map(PathBuf::from)
}

// ── Категории ───────────────────────────────────────────────────────────────────────

/// Читает categories.json. Нет файла — берёт его у MH Sort (`%APPDATA%\\MH Sort`), если тот
/// установлен, иначе создаёт стандартный. Испорченный файл не перезаписывается: категории
/// стандартные и текст ошибки.
pub fn load_categories(path: &Path) -> (Config, Option<String>) {
    match fs::read_to_string(path) {
        Ok(text) => match Config::parse(&text) {
            Ok(config) => (config, None),
            Err(e) => (
                Config::default(),
                Some(format!(
                    "Ошибка в {CONFIG_FILE}: {e}. Пока используются стандартные категории."
                )),
            ),
        },
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            let imported = mh_sort_categories()
                .and_then(|from| fs::read_to_string(from).ok())
                .and_then(|text| Config::parse(&text).ok());
            let config = imported.unwrap_or_default();
            let error = save_categories(&config, path)
                .err()
                .map(|e| format!("Не удалось создать {}: {e}", path.display()));
            (config, error)
        }
        Err(e) => {
            (Config::default(), Some(format!("Не удалось прочитать {}: {e}", path.display())))
        }
    }
}

/// categories.json установленного MH Sort.
fn mh_sort_categories() -> Option<PathBuf> {
    let base = if cfg!(windows) {
        std::env::var_os("APPDATA").map(PathBuf::from)
    } else {
        std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .or_else(|| home_dir().map(|h| h.join(".config")))
    }?;
    let path = base.join("MH Sort").join(CONFIG_FILE);
    path.is_file().then_some(path)
}

pub fn save_categories(config: &Config, path: &Path) -> io::Result<()> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    fs::write(path, config.to_json())
}

/// Почему эту папку сортировать нельзя (или `None`, если можно).
pub fn danger_reason(root: &Path, recursive: bool) -> Option<&'static str> {
    for var in ["SystemRoot", "ProgramFiles", "ProgramFiles(x86)", "ProgramData"] {
        if let Some(dir) = std::env::var_os(var)
            && is_within(root, Path::new(&dir))
        {
            return Some("Это системная папка — сортировать её нельзя.");
        }
    }
    if recursive {
        if root.parent().is_none() {
            return Some("Корень диска нельзя сортировать вместе с подпапками.");
        }
        if let Some(home) = home_dir() {
            let key = path_key(root);
            if key == path_key(&home) || home.parent().is_some_and(|p| key == path_key(p)) {
                return Some("Папку пользователя нельзя сортировать вместе с подпапками.");
            }
        }
    }
    None
}

// ── План ────────────────────────────────────────────────────────────────────────────

/// Строит план. Возвращает `None`, если сканирование отменили.
pub fn scan(
    opts: &ScanOptions,
    classifier: &Classifier,
    cancel: &AtomicBool,
    progress: &mut dyn FnMut(usize),
) -> Option<Plan> {
    let same_output = path_key(&opts.root) == path_key(&opts.output);
    let output_key = path_key(&opts.output);
    let protected: HashSet<String> = opts.protected.iter().map(|p| path_key(p)).collect();
    let excluded: HashSet<String> = opts
        .excluded
        .iter()
        .map(|name| name.trim().to_lowercase())
        .filter(|name| !name.is_empty())
        .collect();

    let walker = WalkDir::new(&opts.root)
        .min_depth(1)
        .max_depth(if opts.recursive { usize::MAX } else { 1 })
        .follow_links(false)
        .sort_by_file_name()
        .into_iter()
        .filter_entry(|entry| {
            if !entry.file_type().is_dir() {
                return true;
            }
            let key = path_key(entry.path());
            let name = entry.file_name().to_string_lossy().to_lowercase();
            let sorted_folder =
                same_output && entry.depth() == 1 && classifier.is_category_folder(&name);
            !(protected.contains(&key)
                || (!same_output && key == output_key)
                || excluded.contains(&name)
                || (opts.skip_hidden && is_hidden(entry))
                || (opts.skip_sorted && sorted_folder))
        });

    let mut plan = Plan { root: opts.root.clone(), output: opts.output.clone(), ..Plan::default() };
    let mut reserved = HashSet::new();
    let mut seen = 0;

    for entry in walker {
        if cancel.load(Ordering::Relaxed) {
            return None;
        }
        let entry = match entry {
            Ok(entry) => entry,
            Err(e) => {
                plan.errors.push(e.to_string());
                continue;
            }
        };
        // Папки и ссылки (в т.ч. junction) не трогаем
        if !entry.file_type().is_file() {
            continue;
        }
        seen += 1;
        if seen % 256 == 0 {
            progress(seen);
        }

        let path = entry.path();
        let Some(name) = entry.file_name().to_str() else {
            plan.ignored += 1;
            plan.errors.push(format!("Имя файла не в Юникоде, пропущен: {}", path.display()));
            continue;
        };
        if protected.contains(&path_key(path))
            || classifier.is_ignored(name)
            || (opts.skip_hidden && is_hidden(&entry))
        {
            plan.ignored += 1;
            continue;
        }

        let class = classifier.classify(name, || {
            if opts.detect_content {
                infer::get_from_path(path).ok().flatten().map(|kind| kind.extension())
            } else {
                None
            }
        });
        let mut dir = opts.output.join(&class.category);
        if opts.type_folders {
            dir.push(&class.type_folder);
        }
        if path.parent().is_some_and(|parent| path_key(parent) == path_key(&dir))
            || (opts.copy && is_same_copy(&entry, &dir.join(name)))
        {
            plan.in_place += 1;
            continue;
        }

        let dst =
            unique_path(&dir, name, &class.ext, |p| reserved.contains(&path_key(p)) || exists(p));
        reserved.insert(path_key(&dst));
        plan.moves.push(PlannedMove {
            src: path.to_path_buf(),
            dst,
            category: class.category,
            ext: class.ext,
            size: entry.metadata().map(|m| m.len()).unwrap_or(0),
            by_content: class.by_content,
            enabled: true,
        });
    }
    progress(seen);
    Some(plan)
}

/// Копия уже лежит в папке назначения: тот же размер и время изменения.
fn is_same_copy(entry: &DirEntry, target: &Path) -> bool {
    let (Ok(src), Ok(dst)) = (entry.metadata(), fs::metadata(target)) else {
        return false;
    };
    let same_time = match (src.modified(), dst.modified()) {
        (Ok(a), Ok(b)) => {
            a.duration_since(b).or_else(|_| b.duration_since(a)).is_ok_and(|d| d.as_secs() < 2)
        }
        _ => false,
    };
    dst.is_file() && src.len() == dst.len() && same_time
}

fn is_hidden(entry: &DirEntry) -> bool {
    if entry.file_name().to_string_lossy().starts_with('.') {
        return true;
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        const HIDDEN: u32 = 0x2;
        const SYSTEM: u32 = 0x4;
        if let Ok(meta) = entry.metadata() {
            return meta.file_attributes() & (HIDDEN | SYSTEM) != 0;
        }
    }
    false
}

// ── Сортировка и отмена ─────────────────────────────────────────────────────────────

pub type Progress<'a> = &'a mut dyn FnMut(usize, usize, &str);

pub struct SortJob {
    pub source: PathBuf,
    pub output: PathBuf,
    pub mode: Mode,
    pub remove_empty: bool,
    pub moves: Vec<PlannedMove>,
    pub journal_path: PathBuf,
}

pub fn run_sort(job: SortJob, cancel: &AtomicBool, progress: Progress<'_>) -> Report {
    let total = job.moves.len();
    let mut report = Report::new(Action::Sort(job.mode), total);
    let mut journal = Journal::new(&job.source, &job.output, job.mode);
    let mut unsaved = 0;

    for (i, planned) in job.moves.iter().enumerate() {
        if cancel.load(Ordering::Relaxed) {
            report.cancelled = true;
            break;
        }
        let name = planned.src.file_name().unwrap_or_default().to_string_lossy();
        progress(i, total, &name);
        match sort_one(planned, job.mode, &mut journal.created_dirs) {
            Ok(to) => {
                report.ok(format!(
                    "{}  ›  {}",
                    rel(&planned.src, &job.source),
                    rel(&to, &job.output)
                ));
                journal.entries.push(Entry { from: planned.src.clone(), to, size: planned.size });
                unsaved += 1;
                // Журнал пишется по ходу дела: даже при сбое останется, что отменять
                if unsaved >= 200 && save_journal(&journal, &job.journal_path).is_ok() {
                    unsaved = 0;
                }
            }
            Err(e) => report.fail(format!("{}: {e}", rel(&planned.src, &job.source))),
        }
    }
    progress(total, total, "");

    if job.remove_empty && job.mode == Mode::Move {
        let sources = journal.entries.iter().map(|e| e.from.as_path());
        journal.removed_dirs = remove_empty_dirs(sources, &job.source);
    }
    if !journal.entries.is_empty()
        && let Err(e) = save_journal(&journal, &job.journal_path)
    {
        report.note =
            Some(format!("Журнал не сохранился ({e}) — отменить эту операцию не получится."));
    }
    report
}

fn sort_one(
    planned: &PlannedMove,
    mode: Mode,
    created_dirs: &mut Vec<PathBuf>,
) -> io::Result<PathBuf> {
    if !exists(&planned.src) {
        return Err(io::Error::new(io::ErrorKind::NotFound, "файла больше нет"));
    }
    let dir = planned.dst.parent().ok_or_else(|| io::Error::other("неверный путь назначения"))?;
    let name = planned
        .src
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| io::Error::other("неверное имя файла"))?;
    ensure_dir(dir, created_dirs)?;
    // Имя подбирается заново: за время предпросмотра в папке могли появиться файлы
    let to = unique_path(dir, name, &planned.ext, exists);
    match mode {
        Mode::Move => move_file(&planned.src, &to)?,
        Mode::Copy => copy_file(&planned.src, &to)?,
    }
    Ok(to)
}

/// Создаёт папку и запоминает, каких папок раньше не было.
fn ensure_dir(dir: &Path, created: &mut Vec<PathBuf>) -> io::Result<()> {
    if dir.is_dir() {
        return Ok(());
    }
    let missing: Vec<PathBuf> =
        dir.ancestors().take_while(|d| !exists(d)).map(Path::to_path_buf).collect();
    fs::create_dir_all(dir)?;
    created.extend(missing.into_iter().rev());
    Ok(())
}

fn already_exists() -> io::Error {
    io::Error::new(io::ErrorKind::AlreadyExists, "файл назначения уже существует")
}

/// Перемещение без перезаписи. Между дисками — копия и удаление оригинала.
pub fn move_file(from: &Path, to: &Path) -> io::Result<()> {
    if exists(to) {
        return Err(already_exists());
    }
    match fs::rename(from, to) {
        Ok(()) => Ok(()),
        Err(e) if is_cross_device(&e) => {
            copy_file(from, to)?;
            if let Err(e) = fs::remove_file(from) {
                let _ = fs::remove_file(to);
                return Err(e);
            }
            Ok(())
        }
        Err(e) => Err(e),
    }
}

/// Копирование без перезаписи.
pub fn copy_file(from: &Path, to: &Path) -> io::Result<()> {
    if exists(to) {
        return Err(already_exists());
    }
    if let Err(e) = fs::copy(from, to) {
        let _ = fs::remove_file(to);
        return Err(e);
    }
    // Дата изменения как у оригинала (на Windows fs::copy сохраняет её сам)
    if let Ok(modified) = fs::metadata(from).and_then(|m| m.modified()) {
        let _ = fs::File::options().write(true).open(to).and_then(|f| f.set_modified(modified));
    }
    Ok(())
}

fn is_cross_device(e: &io::Error) -> bool {
    // ERROR_NOT_SAME_DEVICE на Windows, EXDEV на Unix
    let code = if cfg!(windows) { 17 } else { 18 };
    e.raw_os_error() == Some(code)
}

/// Удаляет папки внутри `root`, которые опустели после перемещения. Сам `root` не трогает.
/// `remove_dir` удаляет только пустые папки, так что содержимое пострадать не может.
fn remove_empty_dirs<'a>(sources: impl Iterator<Item = &'a Path>, root: &Path) -> Vec<PathBuf> {
    let root_key = path_key(root);
    let mut dirs: HashMap<String, PathBuf> = HashMap::new();
    for src in sources {
        for dir in src.ancestors().skip(1) {
            let key = path_key(dir);
            if key == root_key || !is_within(dir, root) || dirs.contains_key(&key) {
                break;
            }
            dirs.insert(key, dir.to_path_buf());
        }
    }
    let mut list: Vec<PathBuf> = dirs.into_values().collect();
    // Сначала самые глубокие: родитель пустеет только после детей
    list.sort_by_key(|d| std::cmp::Reverse(d.components().count()));
    list.into_iter().filter(|d| fs::remove_dir(d).is_ok()).collect()
}

/// Отмена операции из журнала.
pub fn run_undo(
    path: &Path,
    mut journal: Journal,
    cancel: &AtomicBool,
    progress: Progress<'_>,
) -> Report {
    let mut report = Report::new(Action::Undo(journal.mode), journal.entries.len());
    for dir in journal.removed_dirs.iter().rev() {
        let _ = fs::create_dir_all(dir);
    }

    let total = journal.entries.len();
    let mut remaining = total;
    for (step, entry) in journal.entries.iter().rev().enumerate() {
        if cancel.load(Ordering::Relaxed) {
            report.cancelled = true;
            break;
        }
        progress(step, total, &entry.to.file_name().unwrap_or_default().to_string_lossy());
        let shown = rel(&entry.to, &journal.output);
        let result = match journal.mode {
            Mode::Move => undo_move(entry).map(|back| {
                let note = if back == entry.from {
                    ""
                } else {
                    "  (исходное имя занято)"
                };
                format!("{shown}  ›  {}{note}", rel(&back, &journal.source))
            }),
            Mode::Copy => undo_copy(entry).map(|removed| {
                let what =
                    if removed { "копия удалена" } else { "копии уже нет" };
                format!("{shown}: {what}")
            }),
        };
        match result {
            Ok(text) => report.ok(text),
            Err(e) => report.fail(format!("{shown}: {e}")),
        }
        remaining -= 1;
    }
    progress(total, total, "");

    let mut created = journal.created_dirs.clone();
    created.sort_by_key(|d| std::cmp::Reverse(d.components().count()));
    for dir in created {
        let _ = fs::remove_dir(dir);
    }

    // Прерванная отмена оставляет в журнале только то, что ещё не вернули
    journal.entries.truncate(remaining);
    journal.undone = journal.entries.is_empty();
    if let Err(e) = save_journal(&journal, path) {
        report.note = Some(format!("Не удалось обновить журнал: {e}"));
    }
    report
}

fn undo_move(entry: &Entry) -> io::Result<PathBuf> {
    if !exists(&entry.to) {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            "файл не найден — его переместили или удалили",
        ));
    }
    let dir = entry.from.parent().ok_or_else(|| io::Error::other("неверный исходный путь"))?;
    let name = entry
        .from
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| io::Error::other("неверное имя"))?;
    fs::create_dir_all(dir)?;
    let back = unique_path(dir, name, &primary_ext(name), exists);
    move_file(&entry.to, &back)?;
    Ok(back)
}

/// Удаляет копию, только если оригинал на месте и копия не менялась.
fn undo_copy(entry: &Entry) -> io::Result<bool> {
    if !exists(&entry.to) {
        return Ok(false);
    }
    if !exists(&entry.from) {
        return Err(io::Error::other("оригинал пропал — копия оставлена"));
    }
    if fs::metadata(&entry.to)?.len() != entry.size {
        return Err(io::Error::other("копия изменилась — оставлена"));
    }
    fs::remove_file(&entry.to)?;
    Ok(true)
}

// ── Журналы ─────────────────────────────────────────────────────────────────────────

pub fn load_journal(path: &Path) -> io::Result<Journal> {
    Ok(serde_json::from_str(&fs::read_to_string(path)?)?)
}

/// Запись через временный файл, чтобы сбой не оставил полжурнала.
pub fn save_journal(journal: &Journal, path: &Path) -> io::Result<()> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("json.tmp");
    fs::write(&tmp, serde_json::to_string_pretty(journal)?)?;
    fs::rename(&tmp, path)
}

/// Имя файла для нового журнала: history/2026-09-26_14-03-12.json.
pub fn new_journal_path(dir: &Path) -> PathBuf {
    let stamp = chrono::Local::now().format("%Y-%m-%d_%H-%M-%S").to_string();
    let mut path = dir.join(format!("{stamp}.json"));
    let mut n = 1;
    while path.exists() {
        path = dir.join(format!("{stamp}_{n}.json"));
        n += 1;
    }
    path
}

fn journal_files(dir: &Path) -> Vec<PathBuf> {
    let mut files: Vec<PathBuf> = fs::read_dir(dir)
        .into_iter()
        .flatten()
        .filter_map(|entry| entry.ok().map(|e| e.path()))
        .filter(|path| path.extension().is_some_and(|ext| ext == "json"))
        .collect();
    // Имя — это время, поэтому сортировка по имени = по времени
    files.sort();
    files
}

/// Последняя операция, которую ещё можно отменить.
pub fn last_active(dir: &Path) -> Option<(PathBuf, Journal)> {
    journal_files(dir).into_iter().rev().find_map(|path| {
        let journal = load_journal(&path).ok()?;
        (!journal.undone && !journal.entries.is_empty()).then_some((path, journal))
    })
}

/// Оставляет только `keep` последних журналов.
pub fn prune(dir: &Path, keep: usize) {
    let files = journal_files(dir);
    let extra = files.len().saturating_sub(keep);
    for path in &files[..extra] {
        let _ = fs::remove_file(path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    pub fn touch(path: &Path, content: &str) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, content).unwrap();
    }

    pub fn options(root: &Path) -> ScanOptions {
        ScanOptions {
            root: root.to_path_buf(),
            output: root.to_path_buf(),
            recursive: false,
            type_folders: true,
            skip_sorted: true,
            skip_hidden: true,
            detect_content: false,
            copy: false,
            excluded: Vec::new(),
            protected: Vec::new(),
        }
    }

    pub fn run(opts: &ScanOptions) -> Plan {
        let classifier = Classifier::new(&Config::default());
        scan(opts, &classifier, &AtomicBool::new(false), &mut |_| {}).unwrap()
    }

    fn rel_dst(plan: &Plan) -> Vec<String> {
        let mut list: Vec<String> = plan
            .moves
            .iter()
            .map(|m| m.dst.strip_prefix(&plan.output).unwrap().to_string_lossy().replace('\\', "/"))
            .collect();
        list.sort();
        list
    }

    #[test]
    fn root_only_plan() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        touch(&root.join("model.blend"), "");
        touch(&root.join("photo.JPG"), "");
        touch(&root.join("movie.mp4"), "");
        touch(&root.join("setup.exe"), "");
        touch(&root.join("video.mp4.crdownload"), "");
        touch(&root.join(".hidden.txt"), "");
        touch(&root.join("sub/deep.txt"), "");

        let plan = run(&options(root));
        assert_eq!(
            rel_dst(&plan),
            [
                "3D/BLEND/model.blend",
                "Видео/MP4/movie.mp4",
                "Изображения/JPG/photo.JPG",
                "Программы/EXE/setup.exe"
            ]
        );
        assert_eq!(plan.ignored, 2);
    }

    #[test]
    fn recursive_skips_sorted_and_excluded() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        touch(&root.join("Видео/MP4/old.mp4"), "");
        touch(&root.join("Видео/loose.mp4"), "");
        touch(&root.join("sub/notes.txt"), "");
        touch(&root.join("node_modules/lib.js"), "");

        let mut opts = options(root);
        opts.recursive = true;
        opts.excluded = vec!["NODE_MODULES".into()];
        assert_eq!(rel_dst(&run(&opts)), ["Документы/TXT/notes.txt"]);

        // Без «не трогать отсортированное» разбираются и папки категорий
        opts.skip_sorted = false;
        let plan = run(&opts);
        assert_eq!(rel_dst(&plan), ["Видео/MP4/loose.mp4", "Документы/TXT/notes.txt"]);
        assert_eq!(plan.in_place, 1);
    }

    #[test]
    fn duplicate_names_get_numbers() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        touch(&root.join("Изображения/JPG/image.jpg"), "old");
        touch(&root.join("image.jpg"), "a");
        touch(&root.join("a/image.jpg"), "b");
        touch(&root.join("b/IMAGE.jpg"), "c");

        let mut opts = options(root);
        opts.recursive = true;
        let names = rel_dst(&run(&opts));
        #[cfg(windows)]
        assert_eq!(
            names,
            [
                "Изображения/JPG/IMAGE (2).jpg",
                "Изображения/JPG/image (1).jpg",
                "Изображения/JPG/image (3).jpg"
            ]
        );
        #[cfg(not(windows))]
        assert_eq!(names.len(), 3);
    }

    #[test]
    fn protected_and_output_folders_are_skipped() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        touch(&root.join("categories.json"), "{}");
        touch(&root.join("Sorted/Документы/TXT/a.txt"), "");
        touch(&root.join("b.txt"), "");

        let mut opts = options(root);
        opts.recursive = true;
        opts.output = root.join("Sorted");
        opts.protected = vec![root.join("categories.json")];
        let plan = run(&opts);
        assert_eq!(rel_dst(&plan), ["Документы/TXT/b.txt"]);
        assert_eq!(plan.ignored, 1);
    }

    #[test]
    fn flat_mode_without_type_folders() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        touch(&root.join("a.png"), "");
        let mut opts = options(root);
        opts.type_folders = false;
        assert_eq!(rel_dst(&run(&opts)), ["Изображения/a.png"]);
    }

    fn sort(root: &Path, opts: &ScanOptions, mode: Mode, remove_empty: bool) -> (Report, PathBuf) {
        let plan = run(opts);
        let journal_path = root.join("journal.json");
        let job = SortJob {
            source: plan.root.clone(),
            output: plan.output.clone(),
            mode,
            remove_empty,
            moves: plan.moves,
            journal_path: journal_path.clone(),
        };
        (run_sort(job, &AtomicBool::new(false), &mut |_, _, _| {}), journal_path)
    }

    fn undo(journal_path: &Path) -> Report {
        let journal = load_journal(journal_path).unwrap();
        run_undo(journal_path, journal, &AtomicBool::new(false), &mut |_, _, _| {})
    }

    #[test]
    fn move_and_undo_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("Downloads");
        touch(&root.join("photo.jpg"), "photo");
        touch(&root.join("song.flac"), "song");
        touch(&root.join("inner/clip.mkv"), "clip");
        touch(&root.join("Изображения/JPG/photo.jpg"), "already there");

        let mut opts = options(&root);
        opts.recursive = true;
        let (report, journal_path) = sort(dir.path(), &opts, Mode::Move, true);
        assert_eq!((report.done, report.failed), (3, 0));
        assert_eq!(
            fs::read_to_string(root.join("Изображения/JPG/photo (1).jpg")).unwrap(),
            "photo"
        );
        assert_eq!(fs::read_to_string(root.join("Аудио/FLAC/song.flac")).unwrap(), "song");
        assert_eq!(fs::read_to_string(root.join("Видео/MKV/clip.mkv")).unwrap(), "clip");
        assert!(!root.join("inner").exists(), "опустевшая папка удалена");

        let report = undo(&journal_path);
        assert_eq!((report.done, report.failed), (3, 0));
        assert_eq!(fs::read_to_string(root.join("photo.jpg")).unwrap(), "photo");
        assert_eq!(fs::read_to_string(root.join("song.flac")).unwrap(), "song");
        assert_eq!(fs::read_to_string(root.join("inner/clip.mkv")).unwrap(), "clip");
        assert!(!root.join("Аудио").exists(), "созданные папки убраны");
        assert!(!root.join("Видео").exists());
        assert_eq!(
            fs::read_to_string(root.join("Изображения/JPG/photo.jpg")).unwrap(),
            "already there"
        );
        assert!(load_journal(&journal_path).unwrap().undone);
    }

    #[test]
    fn copy_and_undo_keeps_originals() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("src");
        let out = dir.path().join("out");
        touch(&root.join("a.txt"), "text");
        touch(&root.join("b.zip"), "zip");

        let mut opts = options(&root);
        opts.output = out.clone();
        let (report, journal_path) = sort(dir.path(), &opts, Mode::Copy, false);
        assert_eq!(report.done, 2);
        assert!(root.join("a.txt").exists());
        assert_eq!(fs::read_to_string(out.join("Документы/TXT/a.txt")).unwrap(), "text");

        // Изменённую копию отмена не удаляет
        fs::write(out.join("Архивы/ZIP/b.zip"), "changed!").unwrap();
        let report = undo(&journal_path);
        assert_eq!((report.done, report.failed), (1, 1));
        assert!(!out.join("Документы").exists());
        assert!(out.join("Архивы/ZIP/b.zip").exists());
        assert!(root.join("a.txt").exists() && root.join("b.zip").exists());
    }

    #[test]
    fn repeated_copy_skips_existing_copies() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("src");
        touch(&root.join("a.txt"), "text");
        touch(&root.join("b.txt"), "other");
        let mut opts = options(&root);
        opts.output = dir.path().join("out");
        opts.copy = true;
        sort(dir.path(), &opts, Mode::Copy, false);

        let plan = run(&opts);
        assert!(plan.moves.is_empty());
        assert_eq!(plan.in_place, 2);

        // Изменённый оригинал копируется снова, под новым именем
        fs::write(root.join("b.txt"), "changed text").unwrap();
        let plan = run(&opts);
        assert_eq!(plan.moves.len(), 1);
        assert!(plan.moves[0].dst.ends_with("b (1).txt"));
    }

    #[test]
    fn undo_does_not_overwrite_new_file() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("d");
        touch(&root.join("note.txt"), "old");
        let (_, journal_path) = sort(dir.path(), &options(&root), Mode::Move, false);
        touch(&root.join("note.txt"), "new");

        let report = undo(&journal_path);
        assert_eq!(report.failed, 0);
        assert_eq!(fs::read_to_string(root.join("note.txt")).unwrap(), "new");
        assert_eq!(fs::read_to_string(root.join("note (1).txt")).unwrap(), "old");
    }

    #[test]
    fn missing_source_is_reported() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("d");
        touch(&root.join("a.txt"), "");
        touch(&root.join("b.txt"), "");
        let plan = run(&options(&root));
        fs::remove_file(root.join("a.txt")).unwrap();
        let job = SortJob {
            source: root.clone(),
            output: root.clone(),
            mode: Mode::Move,
            remove_empty: false,
            moves: plan.moves,
            journal_path: dir.path().join("j.json"),
        };
        let report = run_sort(job, &AtomicBool::new(false), &mut |_, _, _| {});
        assert_eq!((report.done, report.failed), (1, 1));
    }

    #[test]
    fn categories_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(CONFIG_FILE);
        fs::write(&path, "{ broken").unwrap();
        let (config, error) = load_categories(&path);
        assert!(error.is_some());
        assert!(!config.categories.is_empty());
        assert_eq!(fs::read_to_string(&path).unwrap(), "{ broken", "испорченный не трогаем");

        let path = dir.path().join("sub").join(CONFIG_FILE);
        let (_, error) = load_categories(&path);
        assert!(error.is_none());
        assert!(path.is_file(), "нет файла — создаётся");
    }

    #[cfg(windows)]
    #[test]
    fn dangerous_folders_are_blocked() {
        let home = home_dir().unwrap();
        assert!(danger_reason(Path::new(r"C:\"), true).is_some());
        assert!(danger_reason(Path::new(r"C:\"), false).is_none());
        assert!(danger_reason(Path::new(r"C:\Windows\Temp"), false).is_some());
        assert!(danger_reason(&home, true).is_some());
        assert!(danger_reason(&home, false).is_none());
        assert!(danger_reason(&home.join("Downloads"), true).is_none());
    }
}
