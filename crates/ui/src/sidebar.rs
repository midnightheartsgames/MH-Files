//! Боковая панель: «Этот компьютер», диски с заполнением, известные папки и свои группы.
//! Группы можно создавать, переименовывать, сворачивать, менять местами; папки добавляются
//! перетаскиванием на заголовок группы или командой Ctrl+D.

use std::path::{Path, PathBuf};

use eframe::egui::{
    self, Align2, Color32, CornerRadius, Id, Painter, Rect, Response, RichText, ScrollArea, Sense,
    Ui, pos2, vec2,
};
use mh_files_core::format;
use mh_files_core::location::Location;
use mh_files_core::settings::Group;

use crate::app::{Action, DropZone, FilesApp, Target, drive_title};
use crate::{icons, theme, widgets};

#[derive(Debug, Clone)]
pub enum Edit {
    ToggleGroup(usize),
    AddGroup,
    RemoveGroup(usize),
    RenameGroup(usize, String),
    MoveGroup(usize, isize),
    RemoveFavorite(usize, usize),
    RenameFavorite(usize, usize, String),
    MoveFavorite(usize, usize, isize),
}

pub fn apply_edit(groups: &mut Vec<Group>, edit: Edit) {
    fn shift<T>(items: &mut [T], index: usize, delta: isize) {
        let target = index as isize + delta;
        if index < items.len() && target >= 0 && (target as usize) < items.len() {
            items.swap(index, target as usize);
        }
    }
    match edit {
        Edit::ToggleGroup(g) => {
            if let Some(group) = groups.get_mut(g) {
                group.collapsed = !group.collapsed;
            }
        }
        Edit::AddGroup => {
            let existing: Vec<&str> = groups.iter().map(|g| g.name.as_str()).collect();
            let name = mh_files_core::names::unique_name("Новая группа", &existing);
            groups.push(Group { name, collapsed: false, items: Vec::new() });
        }
        Edit::RemoveGroup(g) => {
            if g < groups.len() {
                groups.remove(g);
            }
        }
        Edit::RenameGroup(g, name) => {
            if let Some(group) = groups.get_mut(g)
                && !name.trim().is_empty()
            {
                group.name = name.trim().to_string();
            }
        }
        Edit::MoveGroup(g, delta) => shift(groups, g, delta),
        Edit::RemoveFavorite(g, i) => {
            if let Some(group) = groups.get_mut(g)
                && i < group.items.len()
            {
                group.items.remove(i);
            }
        }
        Edit::RenameFavorite(g, i, name) => {
            if let Some(item) = groups.get_mut(g).and_then(|group| group.items.get_mut(i))
                && !name.trim().is_empty()
            {
                item.name = name.trim().to_string();
            }
        }
        Edit::MoveFavorite(g, i, delta) => {
            if let Some(group) = groups.get_mut(g) {
                shift(&mut group.items, i, delta);
            }
        }
    }
}

/// Что сейчас переименовывается в панели: группа или пункт, и текст поля.
#[derive(Clone)]
struct Renaming {
    group: usize,
    item: Option<usize>,
    text: String,
    fresh: bool,
}

const ROW: f32 = 26.0;

pub fn show(ui: &mut Ui, app: &mut FilesApp) {
    // Со своим заголовком окна название уже стоит в нём.
    if !app.settings.appearance.custom_title_bar {
        header(ui);
        ui.add_space(6.0);
    }
    let current = app.tab().dir();
    let computer = matches!(app.tab().location, Location::Computer);
    ScrollArea::vertical().id_salt("sidebar").auto_shrink([false, false]).show(ui, |ui| {
        ui.spacing_mut().item_spacing.y = 1.0;
        let response = row(ui, icons::computer, "Этот компьютер", None, computer, false);
        if response.clicked() {
            app.actions
                .push(Action::Open { location: Location::Computer, target: Target::Current });
        }
        let index = matches!(&app.tab().location, Location::Index { query } if query.is_empty());
        let response =
            row(ui, icons::search, "Поиск по дискам", Some("Ctrl+E".into()), index, false);
        if response.clicked() {
            app.actions.push(Action::Run(crate::commands::CommandId::SearchEverywhere));
        }
        saved_searches(ui, app);

        ui.add_space(10.0);
        widgets::section_label(ui, "Диски");
        ui.add_space(2.0);
        for drive in app.drives.clone() {
            drive_row(ui, app, &drive, current.as_deref());
        }

        if !app.places.is_empty() {
            ui.add_space(10.0);
            widgets::section_label(ui, "Места");
            ui.add_space(2.0);
            for (folder, path) in app.places.clone() {
                place_row(ui, app, folder.title(), &path, current.as_deref(), None);
            }
        }

        for g in 0..app.settings.groups.len() {
            ui.add_space(10.0);
            group(ui, app, g, current.as_deref());
        }
        ui.add_space(8.0);
        let add = ui.add(
            egui::Button::new(RichText::new("+ Группа").color(theme::TEXT_SECONDARY)).frame(false),
        );
        if add.clicked() {
            app.actions.push(Action::Sidebar(Edit::AddGroup));
        }
    });
}

/// Сохранённые поиски по дискам.
fn saved_searches(ui: &mut Ui, app: &mut FilesApp) {
    if app.settings.saved_searches.is_empty() {
        return;
    }
    ui.add_space(10.0);
    widgets::section_label(ui, "Поиски");
    ui.add_space(2.0);
    for (i, saved) in app.settings.saved_searches.clone().into_iter().enumerate() {
        let selected =
            matches!(&app.tab().location, Location::Index { query } if *query == saved.query);
        let response = row(ui, icons::search, &saved.name, None, selected, false);
        let location = Location::Index { query: saved.query.clone() };
        if response.clicked() {
            app.actions.push(Action::Open { location: location.clone(), target: Target::Current });
        } else if response.middle_clicked() {
            app.actions.push(Action::Open { location: location.clone(), target: Target::NewTab });
        }
        response.on_hover_text(&saved.query).context_menu(|ui| {
            ui.set_min_width(200.0);
            if ui.button("Открыть в новой вкладке").clicked() {
                app.actions.push(Action::Open { location, target: Target::NewTab });
                ui.close();
            }
            if ui.button("Убрать из списка").clicked() {
                app.actions.push(Action::RemoveSavedSearch(i));
                ui.close();
            }
        });
    }
}

/// Заголовок в духе MH Sidebar: три столбика акцента и название.
fn header(ui: &mut Ui) {
    ui.horizontal(|ui| {
        let (rect, _) = ui.allocate_exact_size(vec2(18.0, 18.0), Sense::hover());
        for (i, h) in [7.0, 16.0, 11.0].into_iter().enumerate() {
            ui.painter().rect_filled(
                Rect::from_min_size(
                    pos2(rect.left() + i as f32 * 6.0, rect.bottom() - h),
                    vec2(4.0, h),
                ),
                1,
                theme::accent(),
            );
        }
        ui.label(
            RichText::new("MH FILES")
                .font(theme::bold(15.0))
                .color(theme::TEXT_SECONDARY)
                .extra_letter_spacing(1.3),
        );
    });
}

/// Строка панели: значок, подпись, справа — мелкий текст.
fn row(
    ui: &mut Ui,
    icon: impl FnOnce(&Painter, Rect, Color32),
    label: &str,
    right: Option<String>,
    selected: bool,
    drop_hover: bool,
) -> Response {
    let (rect, response) = ui.allocate_exact_size(vec2(ui.available_width(), ROW), Sense::click());
    let painter = ui.painter();
    if drop_hover {
        painter.rect_filled(rect, CornerRadius::same(4), theme::accent().gamma_multiply(0.35));
    } else if selected {
        painter.rect_filled(rect, CornerRadius::same(4), theme::CARD);
        painter.rect_filled(
            Rect::from_min_size(rect.min, vec2(3.0, rect.height())),
            CornerRadius::same(1),
            theme::accent(),
        );
    } else if response.hovered() {
        painter.rect_filled(rect, CornerRadius::same(4), theme::CARD.gamma_multiply(0.6));
    }
    let icon_rect =
        Rect::from_center_size(pos2(rect.left() + 17.0, rect.center().y), vec2(15.0, 15.0));
    let color = if selected { theme::accent() } else { theme::TEXT_SECONDARY };
    icon(painter, icon_rect, color);
    let right_width = right.as_ref().map_or(0.0, |text| text.chars().count() as f32 * 6.5 + 8.0);
    let text_color =
        if selected { theme::TEXT_PRIMARY } else { theme::TEXT_SECONDARY.gamma_multiply(1.15) };
    let galley = crate::pane_view::elided(
        ui,
        label,
        theme::regular(14.0),
        text_color,
        rect.width() - 36.0 - right_width,
    );
    painter.galley(
        pos2(rect.left() + 32.0, rect.center().y - galley.size().y / 2.0),
        galley,
        text_color,
    );
    if let Some(right) = right {
        painter.text(
            pos2(rect.right() - 6.0, rect.center().y),
            Align2::RIGHT_CENTER,
            right,
            theme::regular(12.0),
            theme::TEXT_DISABLED,
        );
    }
    response
}

fn drive_row(
    ui: &mut Ui,
    app: &mut FilesApp,
    drive: &mh_files_platform::drives::DriveInfo,
    current: Option<&Path>,
) {
    let selected = current == Some(drive.root.as_path());
    let hover = app.drop_hover.as_deref() == Some(drive.root.as_path());
    let right = (drive.total > 0).then(|| {
        format!("{} / {}", short_size(drive.used()), format::size(drive.total).replace(",0", ""))
    });
    let response = row(ui, icons::drive, &drive_title(drive), right, selected, hover);
    let bar = Rect::from_min_size(
        pos2(response.rect.left() + 32.0, response.rect.bottom() - 3.0),
        vec2(response.rect.width() - 38.0, 3.0),
    );
    if drive.total > 0 {
        widgets::paint_usage_bar(ui.painter(), bar, drive.used_fraction());
    }
    ui.add_space(3.0);
    app.drop_zones.push(DropZone {
        rect: response.rect,
        dir: drive.root.clone(),
        priority: 2,
        favorite_group: None,
    });
    open_on_click(app, &response, &drive.root);
    let root = drive.root.clone();
    let tip = if drive.file_system.is_empty() {
        root.display().to_string()
    } else {
        format!("{} · {}", root.display(), drive.file_system)
    };
    location_menu(app, &response.on_hover_text(tip), &root, None);
}

/// Число без единицы для «занято / всего»: единица стоит у второго числа.
fn short_size(bytes: u64) -> String {
    let text = format::size(bytes);
    text.split(' ').next().unwrap_or("").replace(",0", "")
}

fn place_row(
    ui: &mut Ui,
    app: &mut FilesApp,
    title: &str,
    path: &Path,
    current: Option<&Path>,
    favorite: Option<(usize, usize)>,
) {
    let selected = current == Some(path);
    let hover = app.drop_hover.as_deref() == Some(path);
    let response =
        row(ui, |p, r, c| icons::folder(p, r, c.gamma_multiply(0.9)), title, None, selected, hover);
    app.drop_zones.push(DropZone {
        rect: response.rect,
        dir: path.to_path_buf(),
        priority: 2,
        favorite_group: None,
    });
    open_on_click(app, &response, path);
    let response = response.on_hover_text(path.display().to_string());
    location_menu(app, &response, path, favorite);
}

fn open_on_click(app: &mut FilesApp, response: &Response, path: &Path) {
    let location = Location::Dir(path.to_path_buf());
    if response.clicked() {
        app.actions.push(Action::Open { location, target: Target::Current });
    } else if response.middle_clicked() {
        app.actions.push(Action::Open { location, target: Target::NewTab });
    }
}

fn location_menu(
    app: &mut FilesApp,
    response: &Response,
    path: &Path,
    favorite: Option<(usize, usize)>,
) {
    let path: PathBuf = path.to_path_buf();
    response.context_menu(|ui| {
        ui.set_min_width(220.0);
        let location = Location::Dir(path.clone());
        if ui.button("Открыть в новой вкладке").clicked() {
            app.actions.push(Action::Open { location: location.clone(), target: Target::NewTab });
            ui.close();
        }
        if ui.button("Открыть в соседней панели").clicked() {
            app.actions.push(Action::Open { location, target: Target::OtherPane });
            ui.close();
        }
        if ui.button("Копировать путь").clicked() {
            ui.ctx().copy_text(path.display().to_string());
            ui.close();
        }
        if ui.button("Свойства").clicked() {
            app.actions.push(Action::Shell(mh_files_fs::ShellJob::Properties(vec![path.clone()])));
            ui.close();
        }
        if let Some((g, i)) = favorite {
            ui.separator();
            if ui.button("Переименовать").clicked() {
                let text = app.settings.groups[g].items[i].name.clone();
                set_renaming(
                    ui.ctx(),
                    Some(Renaming { group: g, item: Some(i), text, fresh: true }),
                );
                ui.close();
            }
            if ui.button("Выше").clicked() {
                app.actions.push(Action::Sidebar(Edit::MoveFavorite(g, i, -1)));
                ui.close();
            }
            if ui.button("Ниже").clicked() {
                app.actions.push(Action::Sidebar(Edit::MoveFavorite(g, i, 1)));
                ui.close();
            }
            if ui.button("Убрать из группы").clicked() {
                app.actions.push(Action::Sidebar(Edit::RemoveFavorite(g, i)));
                ui.close();
            }
        }
    });
}

fn renaming_id() -> Id {
    Id::new("sidebar-renaming")
}

fn get_renaming(ctx: &egui::Context) -> Option<Renaming> {
    ctx.data(|data| data.get_temp::<Option<Renaming>>(renaming_id())).flatten()
}

fn set_renaming(ctx: &egui::Context, value: Option<Renaming>) {
    ctx.data_mut(|data| data.insert_temp(renaming_id(), value));
}

/// Поле переименования группы или пункта. `Some` — итоговое имя, когда ввод закончен.
fn rename_field(ui: &mut Ui, state: &mut Renaming) -> Option<Option<String>> {
    let id = Id::new(("sidebar-rename-field", state.group, state.item));
    let response =
        ui.add(egui::TextEdit::singleline(&mut state.text).id(id).desired_width(f32::INFINITY));
    if std::mem::take(&mut state.fresh) {
        response.request_focus();
        crate::pane_view::select_all(ui.ctx(), id, state.text.chars().count());
    }
    if response.lost_focus() {
        let cancelled = ui.input(|i| i.key_pressed(egui::Key::Escape));
        return Some((!cancelled).then(|| state.text.clone()));
    }
    None
}

fn group(ui: &mut Ui, app: &mut FilesApp, g: usize, current: Option<&Path>) {
    let group = app.settings.groups[g].clone();
    let mut renaming = get_renaming(ui.ctx());
    let editing_title = renaming.as_ref().is_some_and(|r| r.group == g && r.item.is_none());
    if editing_title {
        let state = renaming.as_mut().unwrap();
        if let Some(result) = rename_field(ui, state) {
            if let Some(name) = result {
                app.actions.push(Action::Sidebar(Edit::RenameGroup(g, name)));
            }
            set_renaming(ui.ctx(), None);
        } else {
            set_renaming(ui.ctx(), renaming.clone());
        }
    } else {
        let (rect, response) =
            ui.allocate_exact_size(vec2(ui.available_width(), 20.0), Sense::click());
        let painter = ui.painter();
        let hover = app.drop_hover.is_some()
            && rect.contains(ui.input(|i| i.pointer.hover_pos()).unwrap_or_default());
        if hover {
            painter.rect_filled(rect, CornerRadius::same(4), theme::accent().gamma_multiply(0.35));
        }
        let chevron =
            Rect::from_center_size(pos2(rect.left() + 6.0, rect.center().y), vec2(10.0, 10.0));
        if group.collapsed {
            icons::chevron_right(painter, chevron, theme::TEXT_DISABLED);
        } else {
            icons::chevron_down(painter, chevron, theme::TEXT_DISABLED);
        }
        painter.text(
            pos2(rect.left() + 16.0, rect.center().y),
            Align2::LEFT_CENTER,
            group.name.to_uppercase(),
            theme::bold(11.5),
            if response.hovered() { theme::TEXT_SECONDARY } else { theme::TEXT_DISABLED },
        );
        // Бросок на заголовок — добавить в группу.
        app.drop_zones.push(DropZone {
            rect,
            dir: PathBuf::new(),
            priority: 4,
            favorite_group: Some(g),
        });
        if response.clicked() {
            app.actions.push(Action::Sidebar(Edit::ToggleGroup(g)));
        }
        response.context_menu(|ui| {
            ui.set_min_width(200.0);
            if ui.button("Переименовать группу").clicked() {
                set_renaming(
                    ui.ctx(),
                    Some(Renaming { group: g, item: None, text: group.name.clone(), fresh: true }),
                );
                ui.close();
            }
            if ui.button("Новая группа").clicked() {
                app.actions.push(Action::Sidebar(Edit::AddGroup));
                ui.close();
            }
            if ui.button("Выше").clicked() {
                app.actions.push(Action::Sidebar(Edit::MoveGroup(g, -1)));
                ui.close();
            }
            if ui.button("Ниже").clicked() {
                app.actions.push(Action::Sidebar(Edit::MoveGroup(g, 1)));
                ui.close();
            }
            ui.separator();
            if ui.button("Удалить группу").clicked() {
                app.actions.push(Action::Sidebar(Edit::RemoveGroup(g)));
                ui.close();
            }
        });
    }
    if group.collapsed {
        return;
    }
    if group.items.is_empty() {
        ui.label(
            RichText::new("Перетащите сюда папку или нажмите Ctrl+D")
                .font(theme::regular(12.0))
                .color(theme::TEXT_DISABLED),
        );
    }
    for (i, item) in group.items.iter().enumerate() {
        let editing = renaming.as_ref().is_some_and(|r| r.group == g && r.item == Some(i));
        if editing {
            let state = renaming.as_mut().unwrap();
            if let Some(result) = rename_field(ui, state) {
                if let Some(name) = result {
                    app.actions.push(Action::Sidebar(Edit::RenameFavorite(g, i, name)));
                }
                set_renaming(ui.ctx(), None);
            } else {
                set_renaming(ui.ctx(), renaming.clone());
            }
        } else {
            place_row(ui, app, &item.name, &item.path, current, Some((g, i)));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mh_files_core::settings::Favorite;

    #[test]
    fn edits() {
        let mut groups = vec![Group {
            name: "A".into(),
            collapsed: false,
            items: vec![
                Favorite { name: "x".into(), path: "/x".into() },
                Favorite { name: "y".into(), path: "/y".into() },
            ],
        }];
        apply_edit(&mut groups, Edit::MoveFavorite(0, 0, 1));
        assert_eq!(groups[0].items[0].name, "y");
        apply_edit(&mut groups, Edit::MoveFavorite(0, 0, -1));
        assert_eq!(groups[0].items[0].name, "y", "за край не сдвигается");
        apply_edit(&mut groups, Edit::AddGroup);
        apply_edit(&mut groups, Edit::AddGroup);
        assert_eq!(groups[2].name, "Новая группа (2)");
        apply_edit(&mut groups, Edit::RenameGroup(1, "  ".into()));
        assert_eq!(groups[1].name, "Новая группа");
        apply_edit(&mut groups, Edit::RemoveFavorite(0, 5));
        apply_edit(&mut groups, Edit::RemoveGroup(0));
        assert_eq!(groups.len(), 2);
    }
}
