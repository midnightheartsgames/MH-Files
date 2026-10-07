//! Сортировщик из MH Sort во вкладке: раскладывает содержимое папки по папкам категорий и
//! типов (`Видео\MP4`). Экран сверху вниз повторяет MH Sort: что разбираем и с какими
//! галочками → сводка и полоса состава → категории и файлы (проверить и поправить) → подвал
//! (переместить или копировать, куда, главная кнопка). Отмена — кнопкой во вкладке и общим
//! Ctrl+Z: каждая операция пишется в журнал, как в MH Sort.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use eframe::egui::{
    self, Align, Align2, Color32, CornerRadius, FontId, Id, Layout, Rect, RichText, ScrollArea,
    Sense, Stroke, Ui, pos2, vec2,
};
use mh_files_core::format;
use mh_files_core::location::{Location, path_label};
use mh_files_core::settings::SortSettings;
use mh_files_core::sorting::{
    Action as SortAction, CategoryStat, Classifier, Config, Journal, Mode, Plan, Report,
    ScanOptions,
};
use mh_files_fs::sorting::{self, SortJob};
use mh_files_fs::{CancelToken, Ticket};

use crate::app::{FilesApp, Level};
use crate::tabs::Tab;
use crate::{theme, widgets};

/// Владелец событий отмены, запущенной Ctrl+Z, — не вкладка.
pub const OWNER_SORT: u64 = u64::MAX - 8;
/// Владелец сортировок по расписанию.
pub const OWNER_SCHEDULE: u64 = u64::MAX - 9;
/// Как часто проверять расписание.
const SCHEDULE_EVERY: std::time::Duration = std::time::Duration::from_secs(30);

egui_phosphor::subset! {
    /// В exe попадают только значки категорий, а не весь шрифт.
    pub mod ph {
        use fill::{
            APP_WINDOW, BOOKS, BRAIN, CODE, CUBE, DISC, FILE_TEXT, FILE_ZIP, FILM_STRIP,
            FOLDER_SIMPLE, GAME_CONTROLLER, IMAGE, LINK, MAGNET, MUSIC_NOTES, PAINT_BRUSH,
            PRESENTATION_CHART, QUESTION, TABLE, TEXT_AA,
        };
    }
}

/// Значок и цвет категории — как в MH Sort.
#[derive(Clone, Copy)]
pub struct Look {
    pub icon: &'static str,
    pub color: Color32,
}

/// Узнаваемые виды файлов: значок, цвет и характерные расширения.
const KINDS: &[(&str, [u8; 3], &[&str])] = &[
    (
        ph::fill::IMAGE,
        [0x3F, 0xD0, 0xC0],
        &["jpg", "jpeg", "png", "webp", "gif", "bmp", "heic", "tif", "svg"],
    ),
    (ph::fill::FILM_STRIP, [0xA6, 0x8C, 0xFF], &["mp4", "mkv", "avi", "mov", "webm", "wmv"]),
    (ph::fill::MUSIC_NOTES, [0xFF, 0x7C, 0xB5], &["mp3", "wav", "flac", "ogg", "m4a", "aac"]),
    (ph::fill::FILE_TEXT, [0x6F, 0xB4, 0xFF], &["txt", "pdf", "doc", "docx", "odt", "rtf", "md"]),
    (ph::fill::BOOKS, [0xD8, 0xA8, 0x74], &["epub", "fb2", "mobi", "djvu", "cbz"]),
    (ph::fill::TABLE, [0x5B, 0xD9, 0x8C], &["xls", "xlsx", "ods", "csv"]),
    (ph::fill::PRESENTATION_CHART, [0xFF, 0x9F, 0x5A], &["ppt", "pptx", "odp"]),
    (ph::fill::FILE_ZIP, [0xE3, 0xC3, 0x5A], &["zip", "rar", "7z", "tar", "gz"]),
    (ph::fill::DISC, [0x9F, 0xB0, 0xCC], &["iso", "img", "vhd", "vhdx"]),
    (ph::fill::CUBE, [0xFF, 0x82, 0x66], &["blend", "fbx", "obj", "gltf", "glb", "stl"]),
    (ph::fill::PAINT_BRUSH, [0xE5, 0x7B, 0xF0], &["psd", "kra", "clip", "xcf", "aep", "prproj"]),
    (ph::fill::CODE, [0xB4, 0xE0, 0x5A], &["py", "js", "ts", "rs", "cpp", "cs", "java", "html"]),
    (
        ph::fill::GAME_CONTROLLER,
        [0xFF, 0x64, 0x70],
        &["pak", "wad", "vpk", "unitypackage", "uasset"],
    ),
    (ph::fill::APP_WINDOW, [0x8C, 0x9C, 0xFF], &["exe", "msi", "apk", "appx", "msix"]),
    (ph::fill::BRAIN, [0x3C, 0xC8, 0xF0], &["safetensors", "gguf", "ckpt", "pt", "onnx"]),
    (ph::fill::MAGNET, [0x34, 0xC7, 0x9A], &["torrent"]),
    (ph::fill::TEXT_AA, [0xF0, 0xB3, 0xA0], &["ttf", "otf", "woff", "woff2"]),
    (ph::fill::LINK, [0xA7, 0xB8, 0xE8], &["lnk", "url"]),
];

/// Цвета для своих категорий, которые ни на что не похожи.
const SPARE_COLORS: [[u8; 3]; 6] = [
    [0x7F, 0xD1, 0xE8],
    [0xC7, 0xA1, 0xFF],
    [0xFF, 0xB3, 0x8A],
    [0x9B, 0xE3, 0x9B],
    [0xF2, 0xA2, 0xC8],
    [0xA0, 0xB4, 0xFF],
];

const UNKNOWN: Look = Look { icon: ph::fill::QUESTION, color: Color32::from_rgb(0x84, 0x90, 0xAB) };

/// Значок и цвет по расширениям категории: переименованная категория выглядит так же.
pub fn look_for(name: &str, extensions: &[String]) -> Look {
    let mut best: Option<(usize, &str, [u8; 3])> = None;
    for &(icon, rgb, known) in KINDS {
        let hits = extensions
            .iter()
            .filter(|ext| {
                known.contains(&ext.trim().trim_start_matches('.').to_lowercase().as_str())
            })
            .count();
        if hits > best.map_or(0, |(n, ..)| n) {
            best = Some((hits, icon, rgb));
        }
    }
    let (icon, [r, g, b]) = match best {
        Some((_, icon, rgb)) => (icon, rgb),
        None => {
            let hash = name.chars().fold(0u32, |h, c| h.wrapping_mul(31).wrapping_add(c as u32));
            (ph::fill::FOLDER_SIMPLE, SPARE_COLORS[hash as usize % SPARE_COLORS.len()])
        }
    };
    Look { icon, color: Color32::from_rgb(r, g, b) }
}

/// Значки категорий — отдельным семейством шрифта.
pub fn add_fonts(fonts: &mut egui::FontDefinitions) {
    ph::fill::add_as_family(fonts);
}

// ── Общее для всех вкладок: категории и журналы ─────────────────────────────────────

pub struct Shared {
    pub config: Config,
    pub classifier: Arc<Classifier>,
    /// Ошибка чтения categories.json.
    pub error: Option<String>,
    looks: HashMap<String, Look>,
    /// categories.json и папка журналов.
    pub path: PathBuf,
    pub history: PathBuf,
    /// Растёт при перечитывании категорий: открытые планы строятся заново.
    pub revision: u64,
}

impl Shared {
    /// Читает categories.json (маленький файл, при запуске — как и настройки).
    pub fn load(dir: &Path) -> Shared {
        let path = dir.join(mh_files_core::sorting::CONFIG_FILE);
        let (config, error) = sorting::load_categories(&path);
        let mut shared = Shared {
            classifier: Arc::new(Classifier::new(&config)),
            config,
            error,
            looks: HashMap::new(),
            path,
            history: dir.join("sort-history"),
            revision: 0,
        };
        shared.rebuild();
        shared
    }

    /// Категории из настроек: в памяти сразу, файл пишется в фоне.
    pub fn set_config(&mut self, config: Config) {
        self.classifier = Arc::new(Classifier::new(&config));
        self.config = config;
        self.error = None;
        self.revision += 1;
        self.rebuild();
    }

    pub fn reload(&mut self) {
        let (config, error) = sorting::load_categories(&self.path);
        self.classifier = Arc::new(Classifier::new(&config));
        self.config = config;
        self.error = error;
        self.revision += 1;
        self.rebuild();
    }

    fn rebuild(&mut self) {
        self.looks = self
            .config
            .categories
            .iter()
            .map(|(name, exts)| {
                (mh_files_core::sorting::names::sanitize_name(name), look_for(name, exts))
            })
            .collect();
    }

    pub fn look(&self, category: &str) -> Look {
        self.looks.get(category).copied().unwrap_or(UNKNOWN)
    }
}

// ── Вкладка ─────────────────────────────────────────────────────────────────────────

/// Куда складывать папки категорий.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Output {
    /// В саму разбираемую папку.
    #[default]
    Same,
    /// В папку соседней панели.
    OtherPane,
    /// В папку, введённую вручную.
    Custom,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Page {
    #[default]
    Preview,
    Log,
}

/// Идёт сортировка или отмена.
#[derive(Debug, Clone)]
pub struct Running {
    pub done: usize,
    pub total: usize,
    pub current: String,
    pub cancel: CancelToken,
}

#[derive(Default)]
pub struct SortView {
    generation: u64,
    /// Галочки ещё не взяты из настроек.
    fresh: bool,
    pub options: SortSettings,
    pub output: Output,
    pub custom: String,
    /// План нужно построить заново.
    pending: bool,
    /// Для каких параметров построен план: изменились — строим заново.
    built_for: Option<(ScanOptions, u64)>,
    scanning: Option<usize>,
    cancel: Option<CancelToken>,
    pub plan: Option<Plan>,
    problem: Option<String>,
    filter: Option<String>,
    search: String,
    page: Page,
    running: Option<Running>,
    report: Option<Report>,
    errors_only: bool,
    last: Option<(PathBuf, Journal)>,
    show_options: bool,
    excluded_text: String,
    /// Номера видимых строк плана и сводка по категориям — пересчитываются при изменениях.
    view: Vec<usize>,
    stats: Vec<CategoryStat>,
    selected: (usize, u64, usize),
    dirty: bool,
}

impl SortView {
    pub fn new() -> SortView {
        SortView { fresh: true, pending: true, dirty: true, ..SortView::default() }
    }

    /// Построить план заново (F5, изменения на диске).
    pub fn rescan(&mut self) {
        self.pending = true;
        self.built_for = None;
    }

    fn ticket(&self, tab: u64) -> Ticket {
        Ticket { owner: tab, generation: self.generation }
    }

    fn busy(&self) -> bool {
        self.running.is_some()
    }

    /// Пересчитать видимые строки и сводку. `order` — порядок категорий.
    fn refresh(&mut self, order: &[String]) {
        if !self.dirty {
            return;
        }
        self.dirty = false;
        let Some(plan) = &self.plan else {
            self.view.clear();
            self.stats.clear();
            self.selected = (0, 0, 0);
            return;
        };
        let search = self.search.trim().to_lowercase();
        self.view = plan
            .moves
            .iter()
            .enumerate()
            .filter(|(_, m)| self.filter.as_ref().is_none_or(|f| *f == m.category))
            .filter(|(_, m)| {
                search.is_empty()
                    || m.src
                        .file_name()
                        .is_some_and(|n| n.to_string_lossy().to_lowercase().contains(&search))
            })
            .map(|(i, _)| i)
            .collect();
        self.stats = plan.stats(order);
        self.selected = plan.selection();
    }
}

impl FilesApp {
    /// Открыть сортировщик для папки.
    pub fn open_sorter(&mut self) {
        let tab = self.tab();
        let targets = tab.targets();
        let root = match targets.as_slice() {
            [one] if tab.entry(one).is_some_and(|e| e.is_dir()) => Some(one.clone()),
            _ => tab.dir(),
        };
        match root {
            Some(root) => self.open_location(Location::Sort { root }, crate::app::Target::Current),
            None => self.set_status("откройте папку, которую нужно разложить", Level::Info),
        }
    }

    /// Каждый кадр: взять галочки из настроек, запустить нужные сканирования.
    pub fn drive_sorters(&mut self) {
        let other = self.other_pane().and_then(|id| self.pane(id)).and_then(|p| p.tab().dir());
        let revision = self.sorter.revision;
        let classifier = self.sorter.classifier.clone();
        let protected = vec![self.sorter.path.clone(), self.sorter.history.clone()];
        let history = self.sorter.history.clone();
        let workers = self.workers.clone();
        for tab in self.panes.iter_mut().flat_map(|pane| pane.tabs.iter_mut()) {
            let Location::Sort { root } = &tab.location else { continue };
            let root = root.clone();
            let id = tab.id;
            let Some(view) = &mut tab.sort else { continue };
            if view.fresh {
                view.fresh = false;
                view.options = self.settings.sorting.clone();
                view.excluded_text = view.options.excluded.join("\n");
                workers
                    .sort_last(Ticket { owner: id, generation: view.generation }, history.clone());
            }
            if view.busy() {
                continue;
            }
            let output = match view.output {
                Output::Same => Some(root.clone()),
                Output::OtherPane => other.clone().or(Some(root.clone())),
                Output::Custom => {
                    let path = PathBuf::from(view.custom.trim());
                    path.is_absolute().then_some(path)
                }
            };
            let Some(output) = output else {
                view.problem = Some("Введите полный путь папки, куда складывать".into());
                view.plan = None;
                view.dirty = true;
                continue;
            };
            let o = &view.options;
            let options = ScanOptions {
                root: root.clone(),
                output,
                recursive: o.recursive,
                type_folders: o.type_folders,
                skip_sorted: o.skip_sorted,
                skip_hidden: o.skip_hidden,
                detect_content: o.detect_content,
                copy: o.mode == Mode::Copy,
                excluded: o.excluded.clone(),
                protected: protected.clone(),
            };
            let key = (options.clone(), revision);
            if !view.pending && view.built_for.as_ref() == Some(&key) {
                continue;
            }
            view.pending = false;
            view.built_for = Some(key);
            if let Some(cancel) = view.cancel.take() {
                cancel.cancel();
            }
            view.generation += 1;
            if let Some(reason) = sorting::danger_reason(&root, o.recursive) {
                view.problem = Some(reason.to_string());
                view.plan = None;
                view.scanning = None;
                view.dirty = true;
                continue;
            }
            view.problem = None;
            view.scanning = Some(0);
            view.cancel = Some(workers.sort_scan(view.ticket(id), options, classifier.clone()));
        }
    }

    /// Раз в полминуты: разложить папки, которым по расписанию пора. Только пока программа
    /// открыта — фоновой службы нет; пропущенное за время простоя делается при запуске.
    pub fn run_schedules(&mut self, ctx: &egui::Context) {
        if self.settings.sort_schedules.is_empty() {
            return;
        }
        ctx.request_repaint_after(SCHEDULE_EVERY);
        if self.schedule_checked.is_some_and(|at| at.elapsed() < SCHEDULE_EVERY) {
            return;
        }
        self.schedule_checked = Some(std::time::Instant::now());
        let now = mh_files_core::index::unix(std::time::SystemTime::now());
        let protected = vec![self.sorter.path.clone(), self.sorter.history.clone()];
        let mut started = false;
        for (index, schedule) in self.settings.sort_schedules.iter_mut().enumerate() {
            if !schedule.due(now) {
                continue;
            }
            schedule.last_run = now;
            started = true;
            let o = &schedule.options;
            let options = ScanOptions {
                root: schedule.folder.clone(),
                output: schedule.folder.clone(),
                recursive: o.recursive,
                type_folders: o.type_folders,
                skip_sorted: o.skip_sorted,
                skip_hidden: o.skip_hidden,
                detect_content: o.detect_content,
                copy: o.mode == Mode::Copy,
                excluded: o.excluded.clone(),
                protected: protected.clone(),
            };
            let ticket = Ticket { owner: OWNER_SCHEDULE, generation: index as u64 };
            self.workers.sort_scheduled(
                ticket,
                options,
                o.mode,
                o.remove_empty,
                self.sorter.classifier.clone(),
                self.sorter.history.clone(),
            );
        }
        if started {
            self.save_settings();
        }
    }

    /// Расписание для папки: `Some(часы)` — включить с галочками `options`, `None` — убрать.
    pub fn set_schedule(
        &mut self,
        folder: &Path,
        every_hours: Option<u32>,
        options: &SortSettings,
    ) {
        let schedules = &mut self.settings.sort_schedules;
        let key = sorting_key(folder);
        let existing = schedules.iter().position(|s| sorting_key(&s.folder) == key);
        match (existing, every_hours) {
            (Some(index), None) => {
                schedules.remove(index);
            }
            (Some(index), Some(hours)) => {
                schedules[index].every_hours = hours;
                schedules[index].options = options.clone();
            }
            (None, Some(hours)) => schedules.push(mh_files_core::settings::SortSchedule {
                folder: folder.to_path_buf(),
                every_hours: hours,
                // Первый раз — через период, а не сразу: план можно сначала посмотреть.
                last_run: mh_files_core::index::unix(std::time::SystemTime::now()),
                options: options.clone(),
            }),
            (None, None) => return,
        }
        self.save_settings();
    }

    fn sort_view(&mut self, ticket: Ticket) -> Option<&mut SortView> {
        let tab = self.tab_by_id(ticket.owner)?;
        tab.sort.as_deref_mut().filter(|view| view.generation == ticket.generation)
    }

    pub fn on_sort_scan_progress(&mut self, ticket: Ticket, files: usize) {
        if let Some(view) = self.sort_view(ticket) {
            view.scanning = Some(files);
        }
    }

    pub fn on_sort_scanned(&mut self, ticket: Ticket, plan: Plan) {
        if let Some(view) = self.sort_view(ticket) {
            view.scanning = None;
            view.cancel = None;
            // Отмеченное раньше остаётся снятым и в новом плане.
            let off: std::collections::HashSet<PathBuf> = view
                .plan
                .iter()
                .flat_map(|p| p.moves.iter().filter(|m| !m.enabled).map(|m| m.src.clone()))
                .collect();
            let mut plan = plan;
            for planned in &mut plan.moves {
                planned.enabled = !off.contains(&planned.src);
            }
            if view.filter.as_ref().is_some_and(|f| !plan.moves.iter().any(|m| m.category == *f)) {
                view.filter = None;
            }
            view.plan = Some(plan);
            view.dirty = true;
        }
    }

    pub fn on_sort_progress(&mut self, ticket: Ticket, done: usize, total: usize, current: String) {
        if let Some(view) = self.sort_view(ticket)
            && let Some(running) = &mut view.running
        {
            running.done = done;
            running.total = total;
            running.current = current;
        }
    }

    /// Поколение не сверяется: ответ о журналах верен, даже если план с тех пор перестроили.
    pub fn on_sort_last(&mut self, ticket: Ticket, last: Option<(PathBuf, Journal)>) {
        if let Some(view) = self.tab_by_id(ticket.owner).and_then(|tab| tab.sort.as_deref_mut()) {
            view.last = last;
        }
    }

    pub fn on_sort_done(&mut self, ticket: Ticket, report: Report, journal: PathBuf) {
        // Папки, где что-то поменялось: перечитать вкладки и индекс.
        let dirs = match sorting::load_journal(&journal) {
            Ok(j) => vec![j.source, j.output],
            Err(_) => Vec::new(),
        };
        if !dirs.is_empty() {
            self.reload_dirs(&dirs);
        }
        let level = if report.failed > 0 { Level::Error } else { Level::Info };
        let mut text = report.title();
        if let Some(note) = &report.note {
            text.push_str(&format!(" — {note}"));
        }
        if ticket.owner == OWNER_SCHEDULE {
            let folder = self
                .settings
                .sort_schedules
                .get(ticket.generation as usize)
                .map(|s| path_label(&s.folder))
                .unwrap_or_default();
            text = format!("по расписанию «{folder}»: {text}");
        }
        // Расписанию нечего было делать — молчать.
        if ticket.owner != OWNER_SCHEDULE
            || report.done > 0
            || report.failed > 0
            || report.note.is_some()
        {
            self.set_status(text, level);
        }
        if let SortAction::Sort(mode) = report.action
            && report.done > 0
        {
            let what =
                if mode == Mode::Move { "сортировку" } else { "копирование" };
            let label = format!("{what} ({})", format::items(report.done));
            self.operations.record_sort(label, journal.clone());
        }
        let history = self.sorter.history.clone();
        let workers = self.workers.clone();
        if ticket.owner == OWNER_SCHEDULE {
            // По расписанию: открытые сортировщики этой папки строят план заново.
            for tab in self.panes.iter_mut().flat_map(|pane| pane.tabs.iter_mut()) {
                if let Some(view) = tab.sort.as_deref_mut() {
                    view.rescan();
                }
            }
            return;
        }
        if ticket.owner == OWNER_SORT {
            // Отмена из Ctrl+Z: открытые сортировщики перестраивают план и кнопку отмены.
            for tab in self.panes.iter_mut().flat_map(|pane| pane.tabs.iter_mut()) {
                if let Some(view) = tab.sort.as_deref_mut() {
                    view.rescan();
                    view.report = Some(report.clone());
                    view.last = None;
                    workers.sort_last(view.ticket(tab.id), history.clone());
                }
            }
            return;
        }
        if let Some(view) = self.sort_view(ticket) {
            view.running = None;
            view.page = if report.failed > 0 { Page::Log } else { view.page };
            view.errors_only = report.failed > 0;
            view.report = Some(report);
            view.rescan();
            view.generation += 1;
            workers.sort_last(Ticket { owner: ticket.owner, generation: view.generation }, history);
        }
    }

    /// Отмена из общего Ctrl+Z.
    pub fn undo_sort(&mut self, journal: PathBuf) {
        let ticket = Ticket { owner: OWNER_SORT, generation: 0 };
        self.workers.sort_undo(ticket, journal);
        self.set_status("отмена сортировки…", Level::Info);
    }

    /// Вкладка во время рисования вынута из панели — поэтому она передаётся сюда сама.
    fn start_sort(&mut self, tab: &mut Tab) {
        let history = self.sorter.history.clone();
        let workers = self.workers.clone();
        let id = tab.id;
        let Some(view) = tab.sort.as_deref_mut() else { return };
        let Some(plan) = &view.plan else { return };
        let moves: Vec<_> = plan.moves.iter().filter(|m| m.enabled).cloned().collect();
        if moves.is_empty() {
            return;
        }
        let job = SortJob {
            source: plan.root.clone(),
            output: plan.output.clone(),
            mode: view.options.mode,
            remove_empty: view.options.remove_empty && view.options.recursive,
            moves,
            journal_path: PathBuf::new(),
        };
        let total = job.moves.len();
        view.generation += 1;
        view.report = None;
        let cancel = workers.sort_run(view.ticket(id), job, history);
        view.running = Some(Running { done: 0, total, current: String::new(), cancel });
    }

    fn start_tab_undo(&mut self, tab: &mut Tab) {
        let workers = self.workers.clone();
        let id = tab.id;
        let Some(view) = tab.sort.as_deref_mut() else { return };
        let Some((path, journal)) = view.last.take() else { return };
        view.generation += 1;
        view.report = None;
        let cancel = workers.sort_undo(view.ticket(id), path);
        let total = journal.entries.len();
        view.running = Some(Running { done: 0, total, current: String::new(), cancel });
    }

    /// Галочки вкладки — в настройки: следующая вкладка откроется с ними же, как в MH Sort.
    fn remember_sort_options(&mut self, options: &SortSettings) {
        if self.settings.sorting != *options {
            self.settings.sorting = options.clone();
            self.save_settings();
        }
    }
}

// ── Рисование ───────────────────────────────────────────────────────────────────────

pub fn show(ui: &mut Ui, tab: &mut Tab, app: &mut FilesApp) {
    let Location::Sort { root } = tab.location.clone() else { return };
    let Some(view) = tab.sort.as_deref_mut() else { return };
    view.refresh(app.sorter.classifier.categories());
    let before = view.options.clone();
    let area = ui.available_rect_before_wrap();
    ui.painter().rect_filled(area, CornerRadius::ZERO, theme::BACKGROUND);
    let mut actions = Actions::default();
    let mut child = ui.new_child(
        egui::UiBuilder::new()
            .max_rect(area.shrink2(vec2(14.0, 10.0)))
            .layout(Layout::top_down(Align::Min)),
    );
    let ui = &mut child;
    let key = sorting_key(&root);
    let schedule = app
        .settings
        .sort_schedules
        .iter()
        .find(|s| sorting_key(&s.folder) == key)
        .map(|s| s.every_hours);
    header(ui, view, &root, schedule, &mut actions);
    ui.add_space(8.0);
    banner(ui, view, &mut actions);
    summary(ui, view, app);
    ui.add_space(10.0);
    // Подвал снизу, остальное — между.
    let footer_height = 44.0;
    let body = Rect::from_min_max(
        ui.cursor().min,
        pos2(ui.max_rect().right(), ui.max_rect().bottom() - footer_height - 8.0),
    );
    let mut body_ui =
        ui.new_child(egui::UiBuilder::new().max_rect(body).layout(Layout::top_down(Align::Min)));
    match (&view.problem, view.plan.is_some(), view.scanning) {
        (Some(problem), _, _) => note(&mut body_ui, problem, theme::WARN),
        (None, false, Some(files)) => note(
            &mut body_ui,
            &format!("Смотрю, что лежит в папке… {}", format::items(files)),
            theme::TEXT_SECONDARY,
        ),
        (None, false, None) => note(&mut body_ui, "", theme::TEXT_SECONDARY),
        (None, true, _) => {
            tabs_row(&mut body_ui, view);
            body_ui.add_space(6.0);
            match view.page {
                Page::Preview => preview(&mut body_ui, view, app),
                Page::Log => log(&mut body_ui, view),
            }
        }
    }
    let footer_rect = Rect::from_min_max(
        pos2(ui.max_rect().left(), ui.max_rect().bottom() - footer_height),
        ui.max_rect().max,
    );
    let mut footer_ui = ui.new_child(
        egui::UiBuilder::new().max_rect(footer_rect).layout(Layout::left_to_right(Align::Center)),
    );
    footer(&mut footer_ui, view, app, &mut actions);
    if view.options.excluded != before.excluded || view.options != before {
        view.dirty = true;
    }
    let options = view.options.clone();
    if options != before {
        app.remember_sort_options(&options);
    }
    if view.scanning.is_some() || view.running.is_some() {
        ui.ctx().request_repaint_after(std::time::Duration::from_millis(100));
    }
    if actions.sort {
        app.start_sort(tab);
    }
    if actions.undo {
        app.start_tab_undo(tab);
    }
    if actions.rescan
        && let Some(view) = tab.sort.as_deref_mut()
    {
        view.rescan();
    }
    if let Some(dir) = actions.open {
        app.actions.push(crate::app::Action::Open {
            location: Location::Dir(dir),
            target: crate::app::Target::NewTab,
        });
    }
    if actions.reload_categories {
        app.sorter.reload();
    }
    if actions.edit_categories {
        app.settings_window.open_sorting(&app.settings, &app.sorter.config);
    }
    if let Some(every_hours) = actions.schedule
        && let Some(view) = tab.sort.as_deref()
    {
        app.set_schedule(&root, every_hours, &view.options);
        let text = match every_hours {
            Some(_) => {
                format!("«{}» будет раскладываться сама, пока MH Files открыт", path_label(&root))
            }
            None => format!("расписание для «{}» выключено", path_label(&root)),
        };
        app.set_status(text, Level::Info);
    }
}

#[derive(Default)]
struct Actions {
    sort: bool,
    undo: bool,
    rescan: bool,
    reload_categories: bool,
    edit_categories: bool,
    open: Option<PathBuf>,
    /// Расписание: `Some(Some(часы))` — включить, `Some(None)` — выключить.
    schedule: Option<Option<u32>>,
}

fn note(ui: &mut Ui, text: &str, color: Color32) {
    ui.add_space(30.0);
    ui.vertical_centered(|ui| {
        if color == theme::TEXT_SECONDARY && !text.is_empty() {
            ui.add(egui::Spinner::new().color(theme::accent()));
            ui.add_space(6.0);
        }
        ui.label(RichText::new(text).font(theme::regular(14.5)).color(color));
    });
}

/// Шапка: что разбираем, «С подпапками», параметры, обновить, отменить последнюю.
fn header(
    ui: &mut Ui,
    view: &mut SortView,
    root: &Path,
    schedule: Option<u32>,
    actions: &mut Actions,
) {
    let enabled = !view.busy();
    ui.horizontal(|ui| {
        ui.label(RichText::new("Разложить").font(theme::bold(18.0)).color(theme::TEXT_PRIMARY));
        ui.label(
            RichText::new(format!("«{}»", path_label(root)))
                .font(theme::bold(18.0))
                .color(theme::accent()),
        )
        .on_hover_text(root.display().to_string());
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            let options =
                ui.add_enabled(enabled, egui::Button::selectable(view.show_options, "Параметры"));
            if options.clicked() {
                view.show_options = !view.show_options;
            }
            if ui
                .add_enabled(enabled, egui::Button::new("Обновить"))
                .on_hover_text("Проверить папку заново (F5)")
                .clicked()
            {
                actions.rescan = true;
            }
            ui.add_enabled_ui(enabled, |ui| {
                ui.checkbox(&mut view.options.recursive, "С подпапками")
                    .on_hover_text("Брать файлы и из вложенных папок");
            });
            if enabled
                && let Some((_, journal)) = &view.last
                && ui
                    .add(egui::Button::new("Отменить последнюю").frame(false))
                    .on_hover_text(format!("Вернуть {}", journal.describe()))
                    .clicked()
            {
                actions.undo = true;
            }
        });
    });
    if view.show_options {
        ui.add_space(6.0);
        egui::Frame::new()
            .fill(theme::CARD)
            .stroke(Stroke::new(1.0, theme::CARD_STROKE))
            .corner_radius(CornerRadius::same(6))
            .inner_margin(egui::Margin::symmetric(14, 10))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.add_enabled_ui(enabled, |ui| options_panel(ui, view, schedule, actions));
            });
    }
}

fn options_panel(ui: &mut Ui, view: &mut SortView, schedule: Option<u32>, actions: &mut Actions) {
    ui.columns(2, |columns| {
        let o = &mut view.options;
        widgets::switch_row(
            &mut columns[0],
            "Папки по типам внутри категорий",
            Some("Видео\\MP4, Видео\\MKV. Без этого файлы лягут прямо в папку категории."),
            &mut o.type_folders,
        );
        widgets::switch_row(
            &mut columns[0],
            "Узнавать тип по содержимому",
            Some("Для файлов без расширения или с незнакомым расширением."),
            &mut o.detect_content,
        );
        widgets::switch_row(
            &mut columns[0],
            "Пропускать скрытые и системные",
            None,
            &mut o.skip_hidden,
        );
        let nested = o.recursive && view.output == Output::Same;
        columns[0].add_enabled_ui(nested, |ui| {
            widgets::switch_row(
                ui,
                "Пропускать уже разложенные папки",
                Some("Папки категорий внутри исходной. Работает вместе с «С подпапками»."),
                &mut o.skip_sorted,
            );
        });
        let can_remove = o.recursive && o.mode == Mode::Move;
        columns[0].add_enabled_ui(can_remove, |ui| {
            widgets::switch_row(
                ui,
                "Удалять опустевшие папки",
                Some(
                    "Только те, что опустели после перемещения. Работает вместе с «С подпапками».",
                ),
                &mut o.remove_empty,
            );
        });
        let ui = &mut columns[1];
        ui.label(RichText::new("Исключённые папки").color(theme::TEXT_PRIMARY));
        widgets::hint(ui, "Имена папок, по одному в строке. Внутрь них сортировщик не заходит.");
        let edit = egui::TextEdit::multiline(&mut view.excluded_text)
            .desired_rows(3)
            .desired_width(f32::INFINITY)
            .hint_text("node_modules");
        if ui.add(edit).lost_focus() {
            view.options.excluded = view
                .excluded_text
                .lines()
                .map(str::trim)
                .filter(|l| !l.is_empty())
                .map(String::from)
                .collect();
        }
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            ui.label(RichText::new("По расписанию").color(theme::TEXT_PRIMARY));
            let label = |hours: Option<u32>| match hours {
                None => "выключено".to_string(),
                Some(hours) => mh_files_core::settings::SortSchedule::PERIODS
                    .iter()
                    .find(|(h, _)| *h == hours)
                    .map_or_else(|| format!("каждые {hours} ч"), |(_, l)| l.to_string()),
            };
            egui::ComboBox::from_id_salt("sort-schedule")
                .selected_text(label(schedule))
                .show_ui(ui, |ui| {
                    let choices = std::iter::once(None).chain(
                        mh_files_core::settings::SortSchedule::PERIODS.iter().map(|(h, _)| Some(*h)),
                    );
                    for choice in choices {
                        if ui.selectable_label(choice == schedule, label(choice)).clicked()
                            && choice != schedule
                        {
                            actions.schedule = Some(choice);
                        }
                    }
                });
        })
        .response
        .on_hover_text(
            "Раскладывать эту папку самой с этими галочками, пока MH Files открыт. Каждая такая сортировка пишется в журнал и отменяется, как обычная.",
        );
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            if ui.button("Изменить категории…").clicked() {
                actions.edit_categories = true;
            }
            if ui
                .button("Перечитать categories.json")
                .on_hover_text("Если файл правили в редакторе")
                .clicked()
            {
                actions.reload_categories = true;
            }
        });
    });
}

/// Итог последней операции: что сделано, открыть папку, отменить.
fn banner(ui: &mut Ui, view: &mut SortView, actions: &mut Actions) {
    let Some(report) = &view.report else { return };
    let warn = report.failed > 0 || report.cancelled;
    let color = if warn { theme::WARN } else { theme::accent() };
    let mut close = false;
    egui::Frame::new()
        .fill(color.gamma_multiply(0.12))
        .stroke(Stroke::new(1.0, color.gamma_multiply(0.5)))
        .corner_radius(CornerRadius::same(6))
        .inner_margin(egui::Margin::symmetric(12, 8))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                ui.label(RichText::new(report.title()).font(theme::bold(14.5)).color(color));
                if let Some(note) = &report.note {
                    ui.label(RichText::new(note).color(theme::WARN));
                }
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if ui.small_button("Скрыть").clicked() {
                        close = true;
                    }
                    if report.failed > 0 && ui.small_button("Журнал").clicked() {
                        view.page = Page::Log;
                    }
                    if matches!(report.action, SortAction::Sort(_))
                        && view.last.is_some()
                        && ui.small_button("Отменить").clicked()
                    {
                        actions.undo = true;
                    }
                    if let Some(plan) = &view.plan
                        && ui.small_button("Открыть папку").clicked()
                    {
                        actions.open = Some(plan.output.clone());
                    }
                });
            });
        });
    ui.add_space(8.0);
    if close {
        view.report = None;
    }
}

/// Сводка: сколько файлов и куда, полоса состава по категориям.
fn summary(ui: &mut Ui, view: &mut SortView, app: &FilesApp) {
    let Some(plan) = &view.plan else { return };
    let (count, bytes, dirs) = view.selected;
    ui.horizontal(|ui| {
        if count == 0 && !plan.moves.is_empty() {
            ui.label(RichText::new("Ничего не отмечено").font(theme::bold(17.0)));
            ui.label(
                RichText::new("отметьте файлы или категории ниже").color(theme::TEXT_SECONDARY),
            );
        } else if plan.moves.is_empty() {
            ui.label(RichText::new("Раскладывать нечего").font(theme::bold(17.0)));
        } else {
            ui.label(RichText::new(files(count)).font(theme::bold(17.0)));
            let folders = format!(
                "{} по {dirs} {}",
                if view.options.mode == Mode::Move {
                    "разложатся"
                } else {
                    "скопируются"
                },
                format::plural(dirs, "папке", "папкам", "папкам")
            );
            ui.label(RichText::new(folders).color(theme::TEXT_PRIMARY));
            ui.label(RichText::new(format::size(bytes)).color(theme::TEXT_SECONDARY));
        }
        if view.scanning.is_some() {
            ui.add(egui::Spinner::new().size(12.0).color(theme::accent()));
        }
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            let chip = |ui: &mut Ui, text: String, color: Color32| {
                ui.label(RichText::new(text).font(theme::regular(12.5)).color(color));
            };
            if !plan.errors.is_empty() {
                chip(ui, format!("Не прочитано: {}", plan.errors.len()), theme::WARN);
            }
            if plan.ignored > 0 {
                chip(
                    ui,
                    format!("Пропущено: {}", format::count(plan.ignored)),
                    theme::TEXT_SECONDARY,
                );
            }
            if plan.in_place > 0 {
                chip(
                    ui,
                    format!("На месте: {}", format::count(plan.in_place)),
                    theme::TEXT_SECONDARY,
                );
            }
        });
    });
    // Полоса состава: доля каждой категории по числу файлов; щелчок — фильтр.
    let total: usize = view.stats.iter().map(|s| s.files).sum();
    if total == 0 {
        return;
    }
    ui.add_space(6.0);
    let (bar, _) = ui.allocate_exact_size(vec2(ui.available_width(), 10.0), Sense::hover());
    let gap = 3.0;
    let usable = bar.width() - gap * (view.stats.len().saturating_sub(1)) as f32;
    let mut x = bar.left();
    let mut clicked = None;
    for stat in &view.stats {
        let width = (usable * stat.files as f32 / total as f32).max(4.0);
        let rect =
            Rect::from_min_size(pos2(x, bar.top()), vec2(width.min(bar.right() - x), bar.height()));
        let look = app.sorter.look(&stat.name);
        let dim = view.filter.as_ref().is_some_and(|f| *f != stat.name);
        let color = if dim { look.color.gamma_multiply(0.35) } else { look.color };
        ui.painter().rect_filled(rect, CornerRadius::same(5), color);
        let response = ui.interact(rect, Id::new(("sort-bar", &stat.name)), Sense::click());
        if response
            .on_hover_text(format!(
                "{}: {} · {}",
                stat.name,
                files(stat.files),
                format::size(stat.bytes)
            ))
            .clicked()
        {
            clicked = Some(stat.name.clone());
        }
        x += width + gap;
    }
    if let Some(name) = clicked {
        view.filter = if view.filter.as_ref() == Some(&name) { None } else { Some(name) };
        view.dirty = true;
    }
}

fn tabs_row(ui: &mut Ui, view: &mut SortView) {
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 14.0;
        for (page, title) in [(Page::Preview, "Предпросмотр"), (Page::Log, "Журнал")]
        {
            let selected = view.page == page;
            let text = RichText::new(title).font(theme::regular(14.5)).color(if selected {
                theme::TEXT_PRIMARY
            } else {
                theme::TEXT_SECONDARY
            });
            let response = ui.add(egui::Button::new(text).frame(false));
            if selected {
                let r = response.rect;
                ui.painter().hline(
                    r.x_range(),
                    r.bottom() + 2.0,
                    Stroke::new(2.0, theme::accent()),
                );
            }
            if response.clicked() {
                view.page = page;
            }
        }
        if view.page == Page::Preview {
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if ui.button("Снять все").clicked() {
                    set_visible(view, false);
                }
                if ui.button("Отметить все").clicked() {
                    set_visible(view, true);
                }
                let search = egui::TextEdit::singleline(&mut view.search)
                    .hint_text("Поиск по имени")
                    .desired_width(200.0);
                if ui.add(search).changed() {
                    view.dirty = true;
                }
            });
        }
    });
}

/// Отметить или снять все видимые строки.
fn set_visible(view: &mut SortView, on: bool) {
    if let Some(plan) = &mut view.plan {
        for &i in &view.view {
            plan.moves[i].enabled = on;
        }
        view.dirty = true;
    }
}

/// Категории слева, файлы справа.
fn preview(ui: &mut Ui, view: &mut SortView, app: &FilesApp) {
    let area = ui.available_rect_before_wrap();
    let left =
        Rect::from_min_size(area.min, vec2(230.0f32.min(area.width() * 0.35), area.height()));
    let right = Rect::from_min_max(pos2(left.right() + 12.0, area.top()), area.max);
    let mut left_ui =
        ui.new_child(egui::UiBuilder::new().max_rect(left).layout(Layout::top_down(Align::Min)));
    categories(&mut left_ui, view, app);
    ui.painter().vline(left.right() + 6.0, area.y_range(), Stroke::new(1.0, theme::CARD_STROKE));
    let mut right_ui =
        ui.new_child(egui::UiBuilder::new().max_rect(right).layout(Layout::top_down(Align::Min)));
    table(&mut right_ui, view, app);
}

/// Строку плана тащат на категорию.
struct SortRowDrag(usize);

fn categories(ui: &mut Ui, view: &mut SortView, app: &FilesApp) {
    let total: usize = view.stats.iter().map(|s| s.files).sum();
    let dragging = egui::DragAndDrop::has_payload_of_type::<SortRowDrag>(ui.ctx());
    let mut dropped: Option<(usize, String)> = None;
    if let Some(pos) = ui.ctx().pointer_hover_pos()
        && let Some(payload) = egui::DragAndDrop::payload::<SortRowDrag>(ui.ctx())
        && let Some(planned) = view.plan.as_ref().and_then(|p| p.moves.get(payload.0))
    {
        let name = planned.src.file_name().unwrap_or_default().to_string_lossy().into_owned();
        egui::Area::new(Id::new("sort-drag-label"))
            .order(egui::Order::Tooltip)
            .fixed_pos(pos + vec2(14.0, 10.0))
            .interactable(false)
            .show(ui.ctx(), |ui| {
                egui::Frame::new()
                    .fill(theme::CARD)
                    .stroke(Stroke::new(1.0, theme::CARD_STROKE))
                    .corner_radius(CornerRadius::same(4))
                    .inner_margin(egui::Margin::symmetric(8, 4))
                    .show(ui, |ui| ui.label(format!("{name} — в категорию…")));
            });
    }
    ScrollArea::vertical().id_salt("sort-categories").auto_shrink([false, false]).show(ui, |ui| {
        ui.spacing_mut().item_spacing.y = 2.0;
        let all = category_row(ui, None, "Все файлы", total, view.filter.is_none(), None);
        if all.row.clicked() {
            view.filter = None;
            view.dirty = true;
        }
        ui.add_space(6.0);
        let stats = view.stats.clone();
        for stat in stats {
            let state = if stat.selected == stat.files {
                Some(true)
            } else if stat.selected == 0 {
                Some(false)
            } else {
                None
            };
            let selected = view.filter.as_ref() == Some(&stat.name);
            let response = category_row(
                ui,
                Some(app.sorter.look(&stat.name)),
                &stat.name,
                stat.files,
                selected,
                Some(state),
            );
            if response.check {
                let on = state != Some(true);
                if let Some(plan) = &mut view.plan {
                    for planned in plan.moves.iter_mut().filter(|m| m.category == stat.name) {
                        planned.enabled = on;
                    }
                }
                view.dirty = true;
            } else if response.row.clicked() {
                view.filter = if selected { None } else { Some(stat.name.clone()) };
                view.dirty = true;
            }
            if let Some(payload) = response.row.dnd_release_payload::<SortRowDrag>() {
                dropped = Some((payload.0, stat.name.clone()));
            }
            drop_highlight(ui, &response.row, dragging);
            response.row.on_hover_text(format!(
                "{} · {}",
                files(stat.files),
                format::size(stat.bytes)
            ));
        }
        // Пока тащат — и те категории, в которые в плане пока ничего не идёт.
        if dragging {
            ui.add_space(6.0);
            let shown: Vec<String> = view.stats.iter().map(|s| s.name.clone()).collect();
            let others = app
                .sorter
                .classifier
                .categories()
                .iter()
                .chain(std::iter::once(&app.sorter.classifier.unknown().to_string()))
                .filter(|name| !shown.contains(name))
                .cloned()
                .collect::<Vec<String>>();
            for name in others {
                let response =
                    category_row(ui, Some(app.sorter.look(&name)), &name, 0, false, None);
                if let Some(payload) = response.row.dnd_release_payload::<SortRowDrag>() {
                    dropped = Some((payload.0, name.clone()));
                }
                drop_highlight(ui, &response.row, true);
            }
        }
    });
    if let Some((index, category)) = dropped
        && let Some(plan) = &mut view.plan
        && plan.reassign(index, &category)
    {
        view.dirty = true;
    }
}

/// Рамка вокруг категории, на которую сейчас бросят строку.
fn drop_highlight(ui: &Ui, row: &egui::Response, dragging: bool) {
    if dragging && row.contains_pointer() {
        ui.painter().rect_stroke(
            row.rect,
            CornerRadius::same(5),
            Stroke::new(1.5, theme::accent()),
            egui::StrokeKind::Inside,
        );
    }
}

struct RowResponse {
    row: egui::Response,
    check: bool,
}

/// Строка категории: галочка (у «Все файлы» нет), значок, имя, число.
fn category_row(
    ui: &mut Ui,
    look: Option<Look>,
    name: &str,
    count: usize,
    selected: bool,
    check: Option<Option<bool>>,
) -> RowResponse {
    let (rect, row) = ui.allocate_exact_size(vec2(ui.available_width(), 30.0), Sense::click());
    let painter = ui.painter();
    if selected {
        painter.rect_filled(rect, CornerRadius::same(5), theme::CARD);
        painter.rect_filled(
            Rect::from_min_size(rect.min, vec2(3.0, rect.height())),
            CornerRadius::same(1),
            theme::accent(),
        );
    } else if row.hovered() {
        painter.rect_filled(rect, CornerRadius::same(5), theme::CARD.gamma_multiply(0.6));
    }
    let mut x = rect.left() + 10.0;
    let mut clicked_check = false;
    if let Some(state) = check {
        let boxed = Rect::from_center_size(pos2(x + 8.0, rect.center().y), vec2(16.0, 16.0));
        let response = ui.interact(boxed, Id::new(("sort-check", name)), Sense::click());
        paint_check(ui, boxed, state, response.hovered());
        clicked_check = response.clicked();
        x += 26.0;
    }
    let chip = Rect::from_min_size(pos2(x, rect.center().y - 11.0), vec2(22.0, 22.0));
    paint_chip(ui, chip, look);
    let text_color =
        if selected { theme::TEXT_PRIMARY } else { theme::TEXT_SECONDARY.gamma_multiply(1.15) };
    let galley = crate::pane_view::elided(
        ui,
        name,
        theme::regular(14.0),
        text_color,
        rect.right() - chip.right() - 50.0,
    );
    ui.painter().galley(
        pos2(chip.right() + 8.0, rect.center().y - galley.size().y / 2.0),
        galley,
        text_color,
    );
    ui.painter().text(
        pos2(rect.right() - 8.0, rect.center().y),
        Align2::RIGHT_CENTER,
        format::count(count),
        theme::regular(12.5),
        theme::TEXT_DISABLED,
    );
    RowResponse { row, check: clicked_check }
}

/// Галочка: да, нет или частично (`None`).
fn paint_check(ui: &Ui, rect: Rect, state: Option<bool>, hovered: bool) {
    let painter = ui.painter();
    let on = state != Some(false);
    let fill = if on {
        theme::accent()
    } else if hovered {
        theme::CARD
    } else {
        theme::FIELD
    };
    painter.rect_filled(rect, CornerRadius::same(4), fill);
    painter.rect_stroke(
        rect,
        CornerRadius::same(4),
        Stroke::new(1.0, theme::CARD_STROKE),
        egui::StrokeKind::Inside,
    );
    let stroke = Stroke::new(2.0, theme::BACKGROUND);
    match state {
        Some(true) => {
            let points = vec![
                rect.left_center() + vec2(3.5, 0.5),
                rect.center_bottom() + vec2(-1.5, -4.0),
                rect.right_top() + vec2(-3.5, 4.0),
            ];
            painter.add(egui::Shape::line(points, stroke));
        }
        None => {
            painter.hline((rect.left() + 4.0)..=(rect.right() - 4.0), rect.center().y, stroke);
        }
        Some(false) => {}
    }
}

/// Цветной значок категории.
pub fn paint_chip(ui: &Ui, rect: Rect, look: Option<Look>) {
    let painter = ui.painter();
    match look {
        Some(look) => {
            painter.rect_filled(rect, CornerRadius::same(5), look.color.gamma_multiply(0.18));
            painter.text(
                rect.center(),
                Align2::CENTER_CENTER,
                look.icon,
                FontId::new(rect.height() * 0.62, ph::fill::family()),
                look.color,
            );
        }
        None => {
            painter.rect_filled(rect, CornerRadius::same(5), theme::CARD);
            crate::icons::folder(painter, rect.shrink(5.0), theme::accent());
        }
    }
}

fn table(ui: &mut Ui, view: &mut SortView, app: &FilesApp) {
    let Some(plan) = &mut view.plan else { return };
    if view.view.is_empty() {
        let text = if plan.moves.is_empty() {
            "Все файлы уже на своих местах"
        } else {
            "Ничего не подходит под поиск"
        };
        note(ui, text, theme::TEXT_DISABLED);
        return;
    }
    let width = ui.available_width();
    let size_x = width - 250.0;
    let dest_x = width - 240.0;
    let (header, _) = ui.allocate_exact_size(vec2(width, 24.0), Sense::hover());
    let painter = ui.painter();
    let small = theme::regular(12.5);
    painter.text(
        header.left_center() + vec2(36.0, 0.0),
        Align2::LEFT_CENTER,
        "Файл",
        small.clone(),
        theme::TEXT_DISABLED,
    );
    painter.text(
        pos2(header.left() + size_x, header.center().y),
        Align2::RIGHT_CENTER,
        "Размер",
        small.clone(),
        theme::TEXT_DISABLED,
    );
    painter.text(
        pos2(header.left() + dest_x, header.center().y),
        Align2::LEFT_CENTER,
        "Куда",
        small,
        theme::TEXT_DISABLED,
    );
    let row_height = 28.0;
    let root = plan.root.clone();
    let output = plan.output.clone();
    let mut toggled = false;
    ScrollArea::vertical().id_salt("sort-files").auto_shrink([false, false]).show_rows(
        ui,
        row_height,
        view.view.len(),
        |ui, rows| {
            ui.spacing_mut().item_spacing.y = 0.0;
            for row in rows {
                let index = view.view[row];
                let planned = &mut plan.moves[index];
                let (rect, response) = ui.allocate_exact_size(
                    vec2(ui.available_width(), row_height),
                    Sense::click_and_drag(),
                );
                // Строку можно бросить на категорию слева — поправить план руками.
                if response.drag_started() {
                    egui::DragAndDrop::set_payload(ui.ctx(), SortRowDrag(index));
                }
                if row % 2 == 1 {
                    ui.painter().rect_filled(
                        rect,
                        CornerRadius::same(4),
                        theme::CARD.gamma_multiply(0.35),
                    );
                }
                if response.hovered() {
                    ui.painter().rect_filled(
                        rect,
                        CornerRadius::same(4),
                        theme::CARD.gamma_multiply(0.8),
                    );
                }
                let boxed = Rect::from_center_size(
                    pos2(rect.left() + 14.0, rect.center().y),
                    vec2(16.0, 16.0),
                );
                paint_check(ui, boxed, Some(planned.enabled), response.hovered());
                if response.clicked() {
                    planned.enabled = !planned.enabled;
                    toggled = true;
                }
                let name =
                    planned.src.strip_prefix(&root).unwrap_or(&planned.src).display().to_string();
                let color =
                    if planned.enabled { theme::TEXT_PRIMARY } else { theme::TEXT_DISABLED };
                let galley = crate::pane_view::elided(
                    ui,
                    &name,
                    theme::regular(14.0),
                    color,
                    size_x - 120.0,
                );
                ui.painter().galley(
                    pos2(rect.left() + 36.0, rect.center().y - galley.size().y / 2.0),
                    galley,
                    color,
                );
                ui.painter().text(
                    pos2(rect.left() + size_x, rect.center().y),
                    Align2::RIGHT_CENTER,
                    format::size(planned.size),
                    theme::regular(13.0),
                    theme::TEXT_SECONDARY,
                );
                let chip = Rect::from_min_size(
                    pos2(rect.left() + dest_x, rect.center().y - 9.0),
                    vec2(18.0, 18.0),
                );
                paint_chip(ui, chip, Some(app.sorter.look(&planned.category)));
                let mut label = planned.destination_label(&output);
                if planned.by_content {
                    label.push_str("  · по содержимому");
                }
                let galley = crate::pane_view::elided(
                    ui,
                    &label,
                    theme::regular(13.5),
                    color,
                    rect.right() - chip.right() - 12.0,
                );
                ui.painter().galley(
                    pos2(chip.right() + 8.0, rect.center().y - galley.size().y / 2.0),
                    galley,
                    color,
                );
                response.on_hover_text(format!(
                    "{}\nкуда: {}",
                    planned.src.display(),
                    planned.dst.display()
                ));
            }
        },
    );
    if toggled {
        view.dirty = true;
    }
}

fn log(ui: &mut Ui, view: &mut SortView) {
    let Some(report) = &view.report else {
        note(ui, "Здесь появится журнал, когда вы разложите файлы", theme::TEXT_DISABLED);
        return;
    };
    ui.horizontal(|ui| {
        let color = if report.failed > 0 { theme::WARN } else { theme::TEXT_PRIMARY };
        ui.label(RichText::new(report.title()).font(theme::bold(15.0)).color(color));
        if let Some(note) = &report.note {
            ui.label(RichText::new(note).color(theme::WARN));
        }
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            ui.checkbox(&mut view.errors_only, "Только ошибки");
        });
    });
    ui.add_space(6.0);
    let lines: Vec<_> = report.lines.iter().filter(|l| !view.errors_only || !l.ok).collect();
    ScrollArea::vertical().id_salt("sort-log").auto_shrink([false, false]).show_rows(
        ui,
        22.0,
        lines.len(),
        |ui, rows| {
            for line in &lines[rows] {
                let (mark, color) =
                    if line.ok { ("✓", theme::accent()) } else { ("✕", theme::CRITICAL) };
                ui.horizontal(|ui| {
                    ui.label(RichText::new(mark).color(color));
                    ui.label(
                        RichText::new(&line.text)
                            .font(theme::regular(13.0))
                            .color(theme::TEXT_SECONDARY),
                    );
                });
            }
        },
    );
}

/// Подвал: переместить или копировать, куда, ход операции, главная кнопка.
fn footer(ui: &mut Ui, view: &mut SortView, app: &FilesApp, actions: &mut Actions) {
    if let Some(running) = &view.running {
        let fraction =
            if running.total > 0 { running.done as f32 / running.total as f32 } else { 0.0 };
        let (track, _) = ui.allocate_exact_size(vec2(300.0, 8.0), Sense::hover());
        ui.painter().rect_filled(track, 4.0, theme::FIELD);
        let done = Rect::from_min_size(track.min, vec2(track.width() * fraction, track.height()));
        ui.painter().rect_filled(done, 4.0, theme::accent());
        ui.label(RichText::new(format!(
            "{} из {}",
            format::count(running.done),
            format::count(running.total)
        )));
        ui.add(
            egui::Label::new(RichText::new(&running.current).color(theme::TEXT_SECONDARY))
                .truncate(),
        );
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            if widgets::button(ui, "Остановить", false).clicked() {
                running.cancel.cancel();
            }
        });
        return;
    }
    for (mode, title) in [(Mode::Move, "Переместить"), (Mode::Copy, "Копировать")]
    {
        if ui.selectable_label(view.options.mode == mode, title).clicked() {
            view.options.mode = mode;
        }
    }
    ui.add_space(16.0);
    ui.label(RichText::new("Куда:").color(theme::TEXT_SECONDARY));
    let other = app.other_pane().and_then(|id| app.pane(id)).and_then(|p| p.tab().dir());
    let title = match view.output {
        Output::Same => "в эту же папку".to_string(),
        Output::OtherPane => match &other {
            Some(dir) => format!("в соседнюю панель: {}", path_label(dir)),
            None => "в соседнюю панель (нет папки)".to_string(),
        },
        Output::Custom => "в другую папку".to_string(),
    };
    egui::ComboBox::from_id_salt("sort-output").selected_text(title).width(240.0).show_ui(
        ui,
        |ui| {
            ui.selectable_value(&mut view.output, Output::Same, "в эту же папку");
            ui.add_enabled_ui(other.is_some(), |ui| {
                let label = match &other {
                    Some(dir) => format!("в соседнюю панель: {}", path_label(dir)),
                    None => "в соседнюю панель".to_string(),
                };
                ui.selectable_value(&mut view.output, Output::OtherPane, label);
            });
            ui.selectable_value(&mut view.output, Output::Custom, "в другую папку…");
        },
    );
    if view.output == Output::Custom {
        ui.add(
            egui::TextEdit::singleline(&mut view.custom)
                .hint_text("D:\\Разобранное")
                .desired_width(220.0),
        );
    }
    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
        let (count, ..) = view.selected;
        let ready = count > 0 && view.plan.is_some() && view.scanning.is_none();
        let label = match (ready, view.options.mode) {
            (false, _) => "Нечего раскладывать".to_string(),
            (true, Mode::Move) => format!("Разложить {}", files(count)),
            (true, Mode::Copy) => format!("Скопировать {}", files(count)),
        };
        let button =
            egui::Button::new(RichText::new(label).font(theme::bold(14.5)).color(if ready {
                theme::BACKGROUND
            } else {
                theme::TEXT_DISABLED
            }))
            .fill(if ready { theme::accent() } else { theme::FIELD })
            .corner_radius(CornerRadius::same(6))
            .min_size(vec2(200.0, 36.0));
        if ui.add_enabled(ready, button).clicked() {
            actions.sort = true;
        }
    });
}

fn files(n: usize) -> String {
    format!("{} {}", format::count(n), format::plural(n, "файл", "файла", "файлов"))
}

/// Ключ сравнения папок: в Windows регистр не важен.
fn sorting_key(path: &Path) -> String {
    mh_files_core::sorting::names::path_key(path)
}
