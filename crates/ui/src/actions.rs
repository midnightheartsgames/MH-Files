//! Выполнение команд и действий. Единственное место, где команда что-то делает.

use std::path::PathBuf;

use eframe::egui::{self, Key};
use mh_files_core::layout::{PaneId, SplitDirection};
use mh_files_core::location::Location;
use mh_files_core::names;
use mh_files_core::selection::Modifiers;
use mh_files_core::session::{TabSession, ViewMode};
use mh_files_core::settings::{
    Favorite, Group, MAX_GRID, MAX_LIST_SCALE, MIN_GRID, MIN_LIST_SCALE, NewTabLocation,
    SavedSearch,
};
use mh_files_fs::{ShellJob, Transfer};
use mh_files_platform::clipboard::ClipboardFiles;
use mh_files_platform::folders::KnownFolder;
use mh_files_platform::ops::FileOp;
use mh_files_platform::window::Intercepted;

use crate::app::{Action, FilesApp, Level, TabTarget, Target, root_of};
use crate::commands::{CommandId, Lookup};
use crate::operations::Followup;
use crate::tabs::Pane;
use crate::{batch, dialogs, palette, quick, sidebar};

/// Команды, которые работают и тогда, когда фокус в текстовом поле.
const IN_TEXT: &[CommandId] = &[
    CommandId::CommandPalette,
    CommandId::GoTo,
    CommandId::Settings,
    CommandId::NewTab,
    CommandId::CloseTab,
    CommandId::NextTab,
    CommandId::PrevTab,
    CommandId::ToggleInspector,
    CommandId::ToggleSidebar,
    CommandId::Refresh,
    CommandId::Filter,
    CommandId::Search,
    CommandId::SearchContent,
    CommandId::SearchEverywhere,
];

/// Шаг масштаба интерфейса (Ctrl+Plus/Minus).
const ZOOM_STEP: f32 = 0.1;

/// Сколько натечь Ctrl+прокрутки (логарифм множителя egui) на один шаг размера. Щелчок
/// колеса даёт около 0,2 (40 точек × 1/200), но egui размазывает его на несколько кадров;
/// тачпад и жест щипка дают мелкие доли — шаги идут по мере жеста.
const WHEEL_STEP: f32 = 0.12;

/// Пауза, после которой недокрученный остаток забывается: иначе следующий щелчок колеса
/// через минуту дал бы два шага.
const WHEEL_IDLE: f64 = 0.3;

/// Шаг размера плиток и строк на щелчок колеса.
const GRID_STEP: f32 = 16.0;
const LIST_STEP: f32 = 0.1;

impl FilesApp {
    /// Ctrl+колесо мыши (и щипок на тачпаде) над списком файлов — размер содержимого, как в
    /// Проводнике: в плитках — сторона плитки, в таблице и колонках — строки со значками и
    /// шрифтом. Весь интерфейс масштабирует Ctrl+Plus/Minus. egui, пока зажат Ctrl, списки не
    /// прокручивает, а отдаёт прокрутку множителем `zoom_delta`.
    pub(crate) fn wheel_resize(&mut self, ctx: &egui::Context, view: ViewMode) {
        // Повторный проход того же кадра (egui перерисовывает при смене размеров) видит ту же
        // прокрутку — без проверки шаг засчитался бы дважды.
        if ctx.current_pass_index() > 0 {
            return;
        }
        let (delta, now) = ctx.input(|i| (i.zoom_delta(), i.time));
        let (mut accumulated, last) = self.zoom_rest;
        if delta == 1.0 {
            if now - last > WHEEL_IDLE {
                self.zoom_rest.0 = 0.0;
            }
            return;
        }
        accumulated += delta.ln();
        let step = if accumulated >= WHEEL_STEP {
            1.0
        } else if accumulated <= -WHEEL_STEP {
            -1.0
        } else {
            self.zoom_rest = (accumulated, now);
            return;
        };
        self.zoom_rest = (0.0, now);
        let a = &mut self.settings.appearance;
        let text = if view == ViewMode::Grid {
            a.grid_size = (a.grid_size + step * GRID_STEP).clamp(MIN_GRID, MAX_GRID);
            format!("плитки: {:.0} точек", a.grid_size)
        } else {
            // Округление — чтобы шаги по 0,1 не копили погрешность.
            let scale = ((a.list_scale + step * LIST_STEP) * 10.0).round() / 10.0;
            a.list_scale = scale.clamp(MIN_LIST_SCALE, MAX_LIST_SCALE);
            format!("строки: {:.0}%", a.list_scale * 100.0)
        };
        self.save_settings();
        self.set_status(text, Level::Info);
    }

    pub(crate) fn handle_keys(&mut self, ctx: &egui::Context) {
        // Сбрасываются каждый кадр, даже когда открыт диалог: иначе сработают позже невпопад.
        let intercepted = mh_files_platform::window::take_intercepted();
        let overlay = self.palette.is_some()
            || self.dialog.is_some()
            || self.batch.is_some()
            || self.quick.is_some();
        if overlay {
            return;
        }
        let text_focus = ctx.egui_wants_keyboard_input();
        let renaming = self.tab().rename.is_some();
        if !text_focus && !renaming {
            for shortcut in intercepted {
                let command = match shortcut {
                    Intercepted::Paste => CommandId::Paste,
                    Intercepted::DeletePermanent => CommandId::DeletePermanent,
                };
                self.actions.push(Action::Run(command));
            }
        }
        let events = ctx.input(|i| i.events.clone());
        let mut consumed = Vec::new();
        // Буква второго шага последовательности приходит и как текст — его не набирать.
        let mut swallow_text = false;
        for (index, event) in events.iter().enumerate() {
            match event {
                egui::Event::Key { key, pressed: true, modifiers, .. } => {
                    let pending = self.chord_pending();
                    if pending.is_none()
                        && !text_focus
                        && !renaming
                        && self.list_key(*key, *modifiers)
                    {
                        consumed.push(index);
                        continue;
                    }
                    match self.keymap.lookup(pending.as_ref(), *key, *modifiers) {
                        Lookup::Command(command) => {
                            self.pending_chord = None;
                            if (text_focus || renaming) && !IN_TEXT.contains(&command) {
                                continue;
                            }
                            self.actions.push(Action::Run(command));
                            consumed.push(index);
                            swallow_text = pending.is_some();
                        }
                        Lookup::Prefix(first) if !text_focus && !renaming => {
                            self.pending_chord = Some((first, std::time::Instant::now()));
                            consumed.push(index);
                            swallow_text = true;
                        }
                        Lookup::Prefix(_) => {}
                        Lookup::None if pending.is_some() => {
                            // Неизвестный второй шаг: последовательность сброшена, клавиша съедена.
                            self.pending_chord = None;
                            consumed.push(index);
                            swallow_text = true;
                        }
                        Lookup::None => {}
                    }
                }
                // egui превращает Ctrl+C/X/V в отдельные события, а не в нажатия клавиш.
                egui::Event::Copy if !text_focus && !renaming => {
                    self.actions.push(Action::Run(CommandId::Copy));
                    consumed.push(index);
                }
                egui::Event::Cut if !text_focus && !renaming => {
                    // В Windows Shift+Delete тоже приходит как «Вырезать» — его ловит перехватчик.
                    if ctx.input(|i| i.modifiers.ctrl || i.modifiers.command) {
                        self.actions.push(Action::Run(CommandId::Cut));
                    }
                    consumed.push(index);
                }
                egui::Event::Paste(_) if !text_focus && !renaming => {
                    // В Windows вставку видит перехватчик (и без текста в буфере).
                    if !cfg!(windows) {
                        self.actions.push(Action::Run(CommandId::Paste));
                    }
                    consumed.push(index);
                }
                egui::Event::Text(_) if swallow_text => {
                    swallow_text = false;
                    consumed.push(index);
                }
                egui::Event::Text(text) if !text_focus && !renaming => {
                    let modifiers = ctx.input(|i| i.modifiers);
                    if !modifiers.ctrl && !modifiers.alt && !text.trim().is_empty() {
                        self.tab_mut().type_ahead(text);
                        consumed.push(index);
                    }
                }
                _ => {}
            }
        }
        if !consumed.is_empty() {
            ctx.input_mut(|input| {
                let mut index = 0;
                input.events.retain(|_| {
                    let keep = !consumed.contains(&index);
                    index += 1;
                    keep
                });
            });
        }
    }

    /// Стрелки и прочая навигация по списку. `true` — клавиша обработана.
    fn list_key(&mut self, key: Key, modifiers: egui::Modifiers) -> bool {
        if modifiers.alt {
            return false;
        }
        let tab = self.tab_mut();
        if matches!(tab.location, Location::Computer) {
            return false;
        }
        let select = Modifiers { ctrl: modifiers.ctrl, shift: modifiers.shift };
        // Колонки: влево — к родителю, вправо — внутрь папки под курсором.
        if tab.view == ViewMode::Columns && !modifiers.shift && !modifiers.ctrl {
            match key {
                Key::ArrowLeft => {
                    self.actions.push(Action::Run(CommandId::GoUp));
                    return true;
                }
                Key::ArrowRight => {
                    let inside =
                        tab.selection.cursor().and_then(|path| tab.entry(path)).and_then(|entry| {
                            crate::columns::preview_location(&tab.location, entry)
                        });
                    if let Some(location) = inside {
                        self.actions.push(Action::OpenSelect { location, select: None });
                    }
                    return true;
                }
                _ => {}
            }
        }
        let grid = tab.view == ViewMode::Grid;
        let columns = tab.grid_columns.max(1) as isize;
        let page = tab.page_rows.max(1) as isize * if grid { columns } else { 1 };
        match key {
            Key::ArrowDown => tab.move_cursor(if grid { columns } else { 1 }, select),
            Key::ArrowUp => tab.move_cursor(if grid { -columns } else { -1 }, select),
            Key::ArrowRight if grid => tab.move_cursor(1, select),
            Key::ArrowLeft if grid => tab.move_cursor(-1, select),
            Key::PageDown => tab.move_cursor(page, select),
            Key::PageUp => tab.move_cursor(-page, select),
            Key::Home => tab.cursor_to(0, select),
            Key::End => tab.cursor_to(usize::MAX, select),
            Key::Space if modifiers.ctrl => tab.selection.toggle_cursor(),
            Key::Escape => {
                if tab.filter_open {
                    tab.filter_open = false;
                    tab.set_filter(String::new());
                } else {
                    tab.selection.clear();
                }
            }
            _ => return false,
        }
        true
    }

    pub(crate) fn run_actions(&mut self, ctx: &egui::Context) {
        let mut guard = 0;
        while !self.actions.is_empty() && guard < 64 {
            guard += 1;
            let actions = std::mem::take(&mut self.actions);
            for action in actions {
                self.apply(ctx, action);
            }
        }
    }

    fn apply(&mut self, ctx: &egui::Context, action: Action) {
        let workers = self.workers.clone();
        match action {
            Action::Run(command) => self.execute(ctx, command),
            Action::Open { location, target } => self.open_location(location, target),
            Action::FocusPane(pane) => {
                if self.pane(pane).is_some() {
                    self.focused = pane;
                }
            }
            Action::SelectTab { pane, index } => {
                if let Some(pane) = self.pane_mut(pane)
                    && index < pane.tabs.len()
                {
                    pane.active = index;
                }
                self.focused = pane;
            }
            Action::CloseTab { pane, index } => self.close_tab(pane, index),
            Action::NewTabIn(pane) => {
                self.focused = pane;
                self.execute(ctx, CommandId::NewTab);
            }
            Action::SetSort { pane, column } => {
                if let Some(pane) = self.pane_mut(pane) {
                    let tab = pane.tab_mut();
                    let sort = tab.options.sort.toggled(column);
                    tab.set_sort(sort);
                }
            }
            Action::SetView(view) => self.tab_mut().view = view,
            Action::OpenSelect { location, select } => {
                let workers = self.workers.clone();
                let tab = self.tab_mut();
                if tab.location != location {
                    tab.navigate(location, &workers, true);
                }
                match select {
                    Some(path) => {
                        tab.pending_select = Some(path);
                        tab.reveal_pending();
                    }
                    None => tab.select_first = true,
                }
            }
            Action::RemoveSavedSearch(index) => {
                if index < self.settings.saved_searches.len() {
                    self.settings.saved_searches.remove(index);
                    self.save_settings();
                }
            }
            Action::CommitRename { tab, path, new_name } => self.commit_rename(tab, path, new_name),
            Action::Drop { paths, dest, copy } => self.drop_files(paths, dest, copy),
            Action::AddFavorites { group, paths } => self.add_favorites(group, paths),
            Action::Sidebar(edit) => {
                sidebar::apply_edit(&mut self.settings.groups, edit);
                self.save_settings();
            }
            Action::CrumbMenu { pane, dir, at } => {
                let generation = self.crumb_menu.as_ref().map_or(0, |m| m.generation) + 1;
                let ticket = mh_files_fs::Ticket { owner: crate::app::OWNER_CRUMBS, generation };
                workers.complete(ticket, dir.clone(), String::new());
                self.crumb_menu =
                    Some(crate::app::CrumbMenu { pane, dir, at, names: None, generation });
            }
            Action::Shell(job) => workers.shell(job),
            Action::Status(text, level) => self.set_status(text, level),
            Action::MoveTab { from, tab, target } => self.move_tab(from, tab, target),
            Action::FolderSizes(dirs) => self.request_folder_sizes(dirs),
        }
    }

    /// Доступна ли команда сейчас — для палитры и меню.
    pub fn available(&self, command: CommandId) -> bool {
        use CommandId::*;
        let tab = self.tab();
        let has_targets = !tab.targets().is_empty();
        let has_dir = tab.dir().is_some();
        let in_archive = matches!(tab.location, Location::Archive { .. });
        let in_recycle = tab.location == Location::RecycleBin;
        let archive_selected = tab.targets().iter().any(|p| {
            p.file_name()
                .is_some_and(|n| mh_files_fs::archive::is_archive_name(&n.to_string_lossy()))
        });
        match command {
            // Архив только для чтения; его записей нет на диске.
            Cut | Delete | DeletePermanent | Rename | BatchRename | MoveToOtherPane
            | Properties | OpenWith | RevealInExplorer | WindowsMenu | AddFavorite
            | FolderSizes | LabelRed | LabelOrange | LabelYellow | LabelGreen | LabelBlue
            | LabelPurple | LabelClear
                if in_archive =>
            {
                false
            }
            // В корзине объекты — не на своих местах: их можно вернуть или стереть, не больше.
            Restore => in_recycle && has_targets,
            FindDuplicates | SortFolder | AddFavorite | WindowsMenu | FolderSizes | Extract
            | ExtractHere
                if in_recycle =>
            {
                false
            }
            _ if in_recycle && command.needs_targets() => {
                has_targets && matches!(command, Delete | DeletePermanent | CopyPath | CopyName)
            }
            OpenRecycleBin | EmptyRecycleBin => cfg!(windows),
            Extract => in_archive || archive_selected,
            ExtractHere => archive_selected && !in_archive,
            FindDuplicates => has_dir || has_targets && !in_archive,
            SortFolder => has_dir || tab.targets().len() == 1 && !in_archive,
            SelectExtraCopies => tab.duplicates.as_ref().is_some_and(|d| !d.groups.is_empty()),
            _ if command.needs_targets() && !has_targets => false,
            GoBack => tab.history.can_back(),
            GoForward => tab.history.can_forward(),
            GoUp => tab.location.parent().is_some(),
            Paste | NewFolder | NewFile | OpenTerminal => has_dir,
            Search | SearchContent => has_dir,
            SaveSearch => matches!(&tab.location, Location::Index { query } if !query.is_empty()),
            Reindex => self.indexer.status().enabled,
            CheckUpdates => !self.update.checking,
            CopyToOtherPane | MoveToOtherPane | OpenInOtherPane => {
                has_targets && self.other_pane().is_some()
            }
            ClosePane => self.panes.len() > 1,
            ReopenTab => !self.closed.is_empty(),
            BatchRename => has_targets,
            Undo => self.operations.undo_label().is_some(),
            WindowsMenu => cfg!(windows) && (has_targets || has_dir),
            FolderSizes => has_dir,
            _ => true,
        }
    }

    pub fn execute(&mut self, ctx: &egui::Context, command: CommandId) {
        use CommandId::*;
        let workers = self.workers.clone();
        let targets = self.tab().targets();
        let dir = self.tab().dir();
        if command.needs_targets() && targets.is_empty() {
            return;
        }
        match command {
            Open => self.open_targets(&targets, Target::Current),
            OpenInNewTab => self.open_targets(&targets, Target::NewTab),
            OpenInOtherPane => self.open_targets(&targets, Target::OtherPane),
            OpenWith => workers.shell(ShellJob::OpenWith(targets[0].clone())),
            GoBack => {
                let tab = self.tab_mut();
                if let Some(location) = tab.history.back(tab.location.clone()) {
                    tab.navigate(location, &workers, false);
                }
            }
            GoForward => {
                let tab = self.tab_mut();
                if let Some(location) = tab.history.forward(tab.location.clone()) {
                    tab.navigate(location, &workers, false);
                }
            }
            GoUp => {
                if let Some(parent) = self.tab().location.parent() {
                    self.open_location(parent, Target::Current);
                }
            }
            GoComputer => self.open_location(Location::Computer, Target::Current),
            GoHome => self.open_location(self.home_location(), Target::Current),
            GoDesktop | GoDocuments | GoDownloads | GoPictures => {
                let wanted = match command {
                    GoDesktop => KnownFolder::Desktop,
                    GoDocuments => KnownFolder::Documents,
                    GoDownloads => KnownFolder::Downloads,
                    _ => KnownFolder::Pictures,
                };
                match self.places.iter().find(|(folder, _)| *folder == wanted) {
                    Some((_, path)) => {
                        self.open_location(Location::Dir(path.clone()), Target::Current)
                    }
                    None => self.set_status(format!("нет папки «{}»", wanted.title()), Level::Info),
                }
            }
            Undo => self.undo(),
            WindowsMenu => {
                let job = if targets.is_empty() {
                    dir.map(ShellJob::BackgroundMenu)
                } else {
                    Some(ShellJob::ContextMenu(targets))
                };
                if let Some(job) = job {
                    workers.shell(job);
                }
            }
            FolderSizes => {
                let tab = self.tab();
                // Выделенные папки, а без них — все папки списка.
                let selected: Vec<PathBuf> = targets
                    .iter()
                    .filter(|p| tab.entry(p).is_some_and(|e| e.is_dir()))
                    .cloned()
                    .collect();
                let dirs = if selected.is_empty() {
                    tab.listing.iter().filter(|e| e.is_dir()).map(|e| e.path()).collect()
                } else {
                    selected
                };
                self.request_folder_sizes(dirs);
            }
            Refresh => {
                // Сортировщик: построить план заново (reload ниже так и делает).
                self.tab_mut().reload(&workers, true);
                if matches!(self.tab().location, Location::Computer) {
                    workers.drives();
                }
            }
            EditAddress => {
                let tab = self.tab_mut();
                let text = match &tab.location {
                    Location::Dir(path) => path.display().to_string(),
                    _ => String::new(),
                };
                tab.address = Some(text);
                tab.focus_address = true;
            }
            GoTo => self.palette = Some(palette::State::goto(self)),
            CommandPalette => self.palette = Some(palette::State::commands()),
            Filter => {
                let tab = self.tab_mut();
                tab.filter_open = true;
                tab.focus_filter = true;
            }
            Search => {
                if let Some(dir) = dir {
                    self.palette = Some(palette::State::search(dir));
                }
            }
            SearchContent => {
                if let Some(dir) = dir {
                    self.palette = Some(palette::State::search_content(dir));
                }
            }
            Extract => self.extract_command(false),
            ExtractHere => self.extract_command(true),
            FindDuplicates => self.find_duplicates(),
            SortFolder => self.open_sorter(),
            SelectExtraCopies => {
                let keep = self.tab().duplicates.as_ref().map(|d| d.keep).unwrap_or_default();
                self.select_extra_copies(keep);
            }
            SearchEverywhere => {
                if !matches!(self.tab().location, Location::Index { .. }) {
                    let query = String::new();
                    self.open_location(Location::Index { query }, Target::Current);
                }
                if let Some(view) = &mut self.tab_mut().index {
                    view.focus = true;
                }
            }
            SaveSearch => {
                if let Location::Index { query } = self.tab().location.clone()
                    && !query.is_empty()
                {
                    let saved = &mut self.settings.saved_searches;
                    if saved.iter().any(|s| s.query == query) {
                        self.set_status("такой поиск уже сохранён", Level::Info);
                    } else {
                        saved.push(SavedSearch { name: query.clone(), query });
                        self.save_settings();
                        self.set_status("поиск сохранён в боковой панели", Level::Info);
                    }
                }
            }
            CheckUpdates => self.check_updates(),
            Reindex => {
                self.indexer.rescan();
                self.set_status("индекс перестраивается в фоне", Level::Info);
            }
            Copy if matches!(self.tab().location, Location::Archive { .. }) => {
                self.copy_from_archive();
            }
            CopyToOtherPane if matches!(self.tab().location, Location::Archive { .. }) => {
                self.extract_command(false);
            }
            Copy | Cut => {
                let cut = command == Cut;
                self.cut = if cut { targets.iter().cloned().collect() } else { Default::default() };
                workers.shell(ShellJob::SetClipboard { paths: targets.clone(), cut });
                let verb = if cut { "вырезано" } else { "скопировано" };
                self.set_status(
                    format!("{verb}: {}", mh_files_core::format::items(targets.len())),
                    Level::Info,
                );
            }
            Paste => {
                if let Some(dest) = dir {
                    workers.shell(ShellJob::ReadClipboard { dest });
                }
            }
            CopyToOtherPane | MoveToOtherPane => {
                let dest =
                    self.other_pane().and_then(|id| self.pane(id)).and_then(|p| p.tab().dir());
                match dest {
                    Some(dest) => {
                        let copy = command == CopyToOtherPane;
                        self.start_transfer(Transfer { sources: targets, dest, copy }, None);
                    }
                    None => self.set_status("в соседней панели не папка", Level::Error),
                }
            }
            CopyPath => {
                let text: Vec<String> = targets.iter().map(|p| p.display().to_string()).collect();
                ctx.copy_text(text.join("\r\n"));
                self.set_status("путь скопирован", Level::Info);
            }
            CopyName => {
                let text: Vec<String> = targets
                    .iter()
                    .filter_map(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))
                    .collect();
                ctx.copy_text(text.join("\r\n"));
            }
            Rename => {
                if targets.len() > 1 {
                    self.execute(ctx, BatchRename);
                } else {
                    let path = targets[0].clone();
                    self.tab_mut().start_rename(path);
                }
            }
            BatchRename => {
                let tab = self.tab();
                let items: Vec<_> = targets
                    .iter()
                    .filter_map(|path| tab.entry(path))
                    .map(|entry| mh_files_core::rename::Item {
                        path: entry.path(),
                        is_dir: entry.is_dir(),
                        modified: entry.modified,
                        created: entry.created,
                    })
                    .collect();
                let siblings = batch::siblings(tab);
                self.batch = Some(batch::State::new(items, siblings));
            }
            // Из корзины удаляют только насовсем: и объект, и его сведения.
            Delete | DeletePermanent if self.tab().location == Location::RecycleBin => {
                let (shown, files) = self.recycled_files(&targets);
                if !files.is_empty() {
                    self.dialog = Some(dialogs::Dialog::delete_recycled(shown, files));
                }
            }
            Restore => {
                let items: Vec<(PathBuf, PathBuf)> = self
                    .tab()
                    .recycled
                    .iter()
                    .flat_map(|map| targets.iter().filter_map(|path| map.get(path)))
                    .map(|item| (item.data.clone(), item.original.clone()))
                    .collect();
                if !items.is_empty() {
                    self.submit(FileOp::Restore { items }, None);
                }
            }
            OpenRecycleBin => self.open_location(Location::RecycleBin, Target::Current),
            EmptyRecycleBin => workers.shell(ShellJob::EmptyRecycleBin),
            Delete => {
                if self.settings.files.confirm_recycle {
                    self.dialog = Some(dialogs::Dialog::delete(targets, false));
                } else {
                    self.submit(FileOp::Delete { paths: targets, permanent: false }, None);
                }
            }
            DeletePermanent => self.dialog = Some(dialogs::Dialog::delete(targets, true)),
            NewFolder => {
                if let Some(parent) = dir {
                    let tab = self.tab();
                    let existing: Vec<&str> =
                        tab.listing.all().iter().map(|e| e.name.as_str()).collect();
                    let name = names::unique_name("Новая папка", &existing);
                    let tab = self.tab().id;
                    self.submit(
                        FileOp::NewFolder { parent, name },
                        Some(Followup::RenameCreated { tab }),
                    );
                }
            }
            NewFile => {
                if let Some(parent) = dir {
                    let tab = self.tab();
                    let existing: Vec<&str> =
                        tab.listing.all().iter().map(|e| e.name.as_str()).collect();
                    let name =
                        names::unique_file_name("Новый текстовый документ", "txt", &existing);
                    let tab = self.tab().id;
                    self.submit(
                        FileOp::NewFile { parent, name },
                        Some(Followup::RenameCreated { tab }),
                    );
                }
            }
            Properties => {
                let paths = if targets.is_empty() { dir.into_iter().collect() } else { targets };
                if !paths.is_empty() {
                    workers.shell(ShellJob::Properties(paths));
                }
            }
            OpenTerminal => {
                if let Some(dir) = dir {
                    workers
                        .shell(ShellJob::Terminal { dir, command: self.settings.terminal.clone() });
                }
            }
            RevealInExplorer => workers.shell(ShellJob::Reveal(targets[0].clone())),
            LabelRed | LabelOrange | LabelYellow | LabelGreen | LabelBlue | LabelPurple
            | LabelClear => {
                if let Some(color) = command.label()
                    && self.labels.set(&targets, color)
                {
                    self.save_labels();
                }
            }
            AddFavorite => {
                let tab = self.tab();
                let dirs: Vec<PathBuf> = targets
                    .iter()
                    .filter(|p| tab.entry(p).is_some_and(|e| e.is_dir()))
                    .cloned()
                    .collect();
                let paths = if dirs.is_empty() { dir.into_iter().collect() } else { dirs };
                self.add_favorites(0, paths);
            }
            SelectAll => {
                let tab = self.tab_mut();
                tab.selection.select_all(&tab.listing);
            }
            InvertSelection => {
                let tab = self.tab_mut();
                tab.selection.invert(&tab.listing);
            }
            ClearSelection => self.tab_mut().selection.clear(),
            QuickLook => self.quick = Some(quick::State::open(self)),
            ViewDetails => self.tab_mut().view = ViewMode::Details,
            ViewGrid => self.tab_mut().view = ViewMode::Grid,
            ViewColumns => self.tab_mut().view = ViewMode::Columns,
            ToggleHidden => {
                self.settings.files.show_hidden = !self.settings.files.show_hidden;
                self.refresh_view_options();
                self.save_settings();
                let state = if self.settings.files.show_hidden {
                    "показаны"
                } else {
                    "скрыты"
                };
                self.set_status(format!("скрытые файлы {state}"), Level::Info);
            }
            ToggleInspector => {
                self.settings.panes.show_inspector = !self.settings.panes.show_inspector;
                self.save_settings();
            }
            ToggleSidebar => {
                self.settings.panes.show_sidebar = !self.settings.panes.show_sidebar;
                self.save_settings();
            }
            ZoomIn | ZoomOut | ZoomReset => {
                let scale = &mut self.settings.appearance.font_scale;
                *scale = match command {
                    ZoomIn => *scale + ZOOM_STEP,
                    ZoomOut => *scale - ZOOM_STEP,
                    _ => 1.0,
                };
                let settings = std::mem::take(&mut self.settings).sanitized();
                self.apply_settings(ctx, settings);
            }
            NewTab => {
                let location = match self.settings.panes.new_tab {
                    NewTabLocation::Same => self.tab().location.clone(),
                    NewTabLocation::Home => self.home_location(),
                    NewTabLocation::Computer => Location::Computer,
                };
                self.open_location(location, Target::NewTab);
            }
            CloseTab => {
                let pane = self.focused;
                let index = self.focused_pane().active;
                self.close_tab(pane, index);
            }
            ReopenTab => {
                if let Some(session) = self.closed.pop() {
                    let tab = self.make_tab(&session);
                    let pane = self.focused_pane_mut();
                    pane.tabs.insert(pane.active + 1, tab);
                    pane.active += 1;
                }
            }
            DuplicateTab => {
                let session = self.tab().session();
                let tab = self.make_tab(&session);
                let pane = self.focused_pane_mut();
                pane.tabs.insert(pane.active + 1, tab);
                pane.active += 1;
            }
            NextTab | PrevTab => {
                let pane = self.focused_pane_mut();
                let n = pane.tabs.len();
                pane.active = if command == NextTab {
                    (pane.active + 1) % n
                } else {
                    (pane.active + n - 1) % n
                };
            }
            SplitRight | SplitDown => {
                let direction = if command == SplitRight {
                    SplitDirection::Horizontal
                } else {
                    SplitDirection::Vertical
                };
                self.split(direction);
            }
            ClosePane => self.close_pane(self.focused),
            NextPane => self.focused = self.layout.next_pane(self.focused, false),
            Settings => self.settings_window.open(&self.settings, &self.sorter.config),
        }
    }

    /// Открыть объекты: папки — переходом, файлы — программой по умолчанию.
    fn open_targets(&mut self, targets: &[PathBuf], target: Target) {
        if self.open_in_archive(targets, target) {
            return;
        }
        if self.tab().location == Location::RecycleBin {
            self.set_status("из корзины не открыть — сначала «Восстановить»", Level::Info);
            return;
        }
        let tab = self.tab();
        let mut places = Vec::new();
        let mut files = Vec::new();
        for path in targets {
            match tab.entry(path) {
                Some(entry) if entry.is_dir() => places.push(Location::Dir(path.clone())),
                // zip и 7z открываются как папки; в программе — «Открыть с помощью».
                Some(entry) if mh_files_fs::archive::is_archive_name(&entry.name) => {
                    places.push(Location::Archive { archive: path.clone(), inner: String::new() })
                }
                Some(_) => files.push(path.clone()),
                None => {}
            }
        }
        for file in files {
            self.workers.shell(ShellJob::Open(file));
        }
        // Несколько папок — каждая в своей вкладке.
        let many = places.len() > 1;
        for place in places {
            let target = if many && target == Target::Current { Target::NewTab } else { target };
            self.open_location(place, target);
        }
    }

    pub fn open_location(&mut self, location: Location, target: Target) {
        let workers = self.workers.clone();
        self.remember(&location);
        match target {
            Target::Current => self.tab_mut().navigate(location, &workers, true),
            Target::NewTab => {
                let view = self.tab().view;
                let tab = self.make_tab(&TabSession { location, view, sort: Default::default() });
                let pane = self.focused_pane_mut();
                pane.tabs.insert(pane.active + 1, tab);
                pane.active += 1;
            }
            Target::EdgePane { first } => {
                let view = self.tab().view;
                let tab = self.make_tab(&TabSession { location, view, sort: Default::default() });
                let id = PaneId(self.new_id());
                self.layout.split_edge(SplitDirection::Horizontal, id, first);
                self.panes.push(Pane { id, tabs: vec![tab], active: 0 });
                self.focused = id;
            }
            Target::OtherPane => {
                if self.other_pane().is_none() {
                    self.split(SplitDirection::Horizontal);
                } else if let Some(other) = self.other_pane() {
                    self.focused = other;
                }
                self.tab_mut().navigate(location, &workers, true);
            }
        }
    }

    fn split(&mut self, direction: SplitDirection) {
        let id = mh_files_core::layout::PaneId(self.new_id());
        let session = self.tab().session();
        let tab = self.make_tab(&session);
        if self.layout.split(self.focused, direction, id) {
            self.panes.push(Pane { id, tabs: vec![tab], active: 0 });
            self.focused = id;
        }
    }

    fn close_pane(&mut self, id: mh_files_core::layout::PaneId) {
        if self.panes.len() > 1 && self.layout.remove(id) {
            if let Some(index) = self.panes.iter().position(|p| p.id == id) {
                let mut pane = self.panes.remove(index);
                for tab in &mut pane.tabs {
                    tab.stop();
                    self.closed.push(tab.session());
                }
            }
            self.focused = self.layout.panes()[0];
        }
    }

    fn close_tab(&mut self, pane_id: mh_files_core::layout::PaneId, index: usize) {
        let alone = self.panes.len() == 1;
        let Some(pane) = self.pane_mut(pane_id) else { return };
        if pane.tabs.len() == 1 && !alone {
            self.close_pane(pane_id);
            return;
        }
        // Последняя вкладка последней панели: окно не пустеет — на её месте «Этот компьютер».
        if pane.tabs.len() == 1 {
            if pane.tabs[0].location == Location::Computer {
                return;
            }
            let view = pane.tabs[0].view;
            let computer =
                TabSession { location: Location::Computer, view, sort: Default::default() };
            let fresh = self.make_tab(&computer);
            let Some(pane) = self.pane_mut(pane_id) else { return };
            let mut tab = std::mem::replace(&mut pane.tabs[0], fresh);
            pane.active = 0;
            tab.stop();
            self.closed.push(tab.session());
            if self.closed.len() > 32 {
                self.closed.remove(0);
            }
            return;
        }
        let mut tab = pane.tabs.remove(index);
        if pane.active > index || pane.active >= pane.tabs.len() {
            pane.active = pane.active.saturating_sub(1);
        }
        tab.stop();
        self.closed.push(tab.session());
        if self.closed.len() > 32 {
            self.closed.remove(0);
        }
    }

    /// Перенести вкладку в другую панель или в новую панель рядом.
    fn move_tab(&mut self, from: PaneId, tab_id: u64, target: TabTarget) {
        let Some(source) = self.panes.iter().position(|p| p.id == from) else { return };
        let Some(index) = self.panes[source].tabs.iter().position(|t| t.id == tab_id) else {
            return;
        };
        let alone = self.panes[source].tabs.len() == 1;
        match target {
            TabTarget::Into(pane) if pane == from => return,
            TabTarget::Split { pane, .. } if pane == from && alone => return,
            _ => {}
        }
        let tab = self.panes[source].tabs.remove(index);
        let pane = &mut self.panes[source];
        if pane.active >= pane.tabs.len() || pane.active > index {
            pane.active = pane.active.saturating_sub(1);
        }
        match target {
            TabTarget::Into(pane_id) => {
                if let Some(pane) = self.pane_mut(pane_id) {
                    pane.tabs.push(tab);
                    pane.active = pane.tabs.len() - 1;
                }
                self.focused = pane_id;
            }
            TabTarget::Split { pane, direction, first } => {
                let id = PaneId(self.new_id());
                if self.layout.split_at(pane, direction, id, first) {
                    self.panes.push(Pane { id, tabs: vec![tab], active: 0 });
                    self.focused = id;
                }
            }
        }
        // Панель без вкладок исчезает, её место занимает соседка.
        if self.pane(from).is_some_and(|p| p.tabs.is_empty()) && self.layout.remove(from) {
            self.panes.retain(|p| p.id != from);
            if self.focused == from {
                self.focused = self.layout.panes()[0];
            }
        }
    }

    fn commit_rename(&mut self, tab_id: u64, path: PathBuf, new_name: String) {
        let new_name = new_name.trim().to_string();
        let old = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        if new_name == old || new_name.is_empty() {
            return;
        }
        if let Err(error) = names::validate(&new_name) {
            self.set_status(format!("«{new_name}»: {error}"), Level::Error);
            return;
        }
        self.submit(
            FileOp::Rename { path, new_name },
            Some(Followup::SelectCreated { tab: tab_id }),
        );
    }

    pub(crate) fn paste_files(&mut self, dest: PathBuf, files: Option<ClipboardFiles>) {
        let Some(files) = files.filter(|f| !f.paths.is_empty()) else {
            self.set_status("в буфере обмена нет файлов", Level::Info);
            return;
        };
        let followup = files.cut.then_some(Followup::ClearClipboard);
        let transfer = Transfer { sources: files.paths, dest, copy: !files.cut };
        self.start_transfer(transfer, followup);
    }

    fn drop_files(&mut self, paths: Vec<PathBuf>, dest: PathBuf, copy: Option<bool>) {
        let sources: Vec<PathBuf> = paths
            .into_iter()
            .filter(|p| p.parent() != Some(dest.as_path()) && *p != dest)
            .collect();
        if sources.is_empty() {
            return;
        }
        if let Some(inside) = sources.iter().find(|p| dest.starts_with(p)) {
            self.set_status(
                format!("нельзя положить папку в саму себя: {}", inside.display()),
                Level::Error,
            );
            return;
        }
        if writable_zip(&dest).is_some() {
            self.add_to_zip(sources, dest);
            return;
        }
        // Как в Проводнике: на тот же диск — перемещение, на другой — копирование.
        let copy = copy.unwrap_or_else(|| sources.iter().any(|p| root_of(p) != root_of(&dest)));
        self.start_transfer(Transfer { sources, dest, copy }, None);
    }

    /// Удалённые объекты вкладки корзины: прежние пути (для вопроса) и файлы в корзине —
    /// сам объект и его сведения.
    fn recycled_files(&self, targets: &[PathBuf]) -> (Vec<PathBuf>, Vec<PathBuf>) {
        let Some(map) = &self.tab().recycled else { return Default::default() };
        let mut shown = Vec::new();
        let mut files = Vec::new();
        for item in targets.iter().filter_map(|path| map.get(path)) {
            shown.push(item.original.clone());
            files.push(item.data.clone());
            files.push(item.info.clone());
        }
        (shown, files)
    }

    /// Бросок в zip: копирование в «сжатую папку» Проводника (`IFileOperation`) — Windows
    /// сама дописывает архив и спрашивает про занятые имена. Своего кода записи в архив нет.
    fn add_to_zip(&mut self, sources: Vec<PathBuf>, dest: PathBuf) {
        let Some((archive, inner)) = Location::containing_archive(&dest) else { return };
        let name = mh_files_core::location::path_label(&archive);
        self.set_status(format!("дописываю в «{name}»…"), Level::Info);
        self.workers.add_to_zip(archive, inner, sources);
    }

    /// Файлы дописаны в zip: сказать, что вышло, и перечитать архив и его папку.
    pub fn on_zip_added(
        &mut self,
        archive: PathBuf,
        result: Result<mh_files_fs::zip_write::Added, String>,
    ) {
        let name = mh_files_core::location::path_label(&archive);
        match result {
            Ok(added) => {
                let mut text =
                    format!("в «{name}» добавлено: {}", mh_files_core::format::items(added.files));
                if let Some((was, now)) = added.renamed.first() {
                    let more = added.renamed.len() - 1;
                    text += &format!("; имя занято — «{was}» лёг как «{now}»");
                    if more > 0 {
                        text += &format!(" и ещё {more}");
                    }
                }
                self.set_status(text, Level::Info);
            }
            Err(error) => self.set_status(format!("«{name}»: {error}"), Level::Error),
        }
        let workers = self.workers.clone();
        for tab in self.panes.iter_mut().flat_map(|pane| pane.tabs.iter_mut()) {
            if matches!(&tab.location, Location::Archive { archive: a, .. } if *a == archive) {
                tab.reload(&workers, true);
            }
        }
        if let Some(parent) = archive.parent() {
            self.reload_dirs(&[parent.to_path_buf()]);
        }
    }

    fn add_favorites(&mut self, group: usize, paths: Vec<PathBuf>) {
        if self.settings.groups.is_empty() {
            self.settings.groups.push(Group {
                name: "Избранное".into(),
                collapsed: false,
                items: Vec::new(),
            });
        }
        let group = group.min(self.settings.groups.len() - 1);
        let known_files: Vec<PathBuf> = self
            .panes
            .iter()
            .flat_map(|pane| pane.tabs.iter())
            .flat_map(|tab| tab.listing.all().iter())
            .filter(|entry| !entry.is_dir())
            .map(|entry| entry.path())
            .collect();
        let items = &mut self.settings.groups[group].items;
        let mut added = 0;
        for path in paths {
            if known_files.contains(&path) || items.iter().any(|f| f.path == path) {
                continue;
            }
            let name = mh_files_core::location::path_label(&path);
            items.push(Favorite { name, path });
            added += 1;
        }
        if added > 0 {
            self.save_settings();
            self.set_status(
                format!("добавлено в «{}»", self.settings.groups[group].name),
                Level::Info,
            );
        }
    }
}

/// Куда бросают — внутрь zip (сам архив или папка в нём)? Тогда архив. Только zip на диске:
/// 7z и rar Проводник не пишет, а во вложенный архив — некуда.
pub fn writable_zip(dest: &std::path::Path) -> Option<PathBuf> {
    let (archive, _) = Location::containing_archive(dest)?;
    let name = archive.file_name()?.to_string_lossy();
    let nested = archive.parent().and_then(Location::containing_archive).is_some();
    (mh_files_core::entry::extension_of(&name) == "zip" && !nested).then_some(archive)
}
