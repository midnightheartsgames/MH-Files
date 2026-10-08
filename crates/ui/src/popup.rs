//! Контекстные меню своими окнами. Меню объектов с пунктами Windows бывает выше окна
//! программы: как у Проводника, оно выходит за край окна — каждое меню и подменю рисуется
//! в своём окне без рамки поверх остальных (`show_viewport_immediate`), в координатах
//! экрана, и не уходит за край монитора: у правого края открывается влево, у нижнего —
//! вверх. Закрывается выбором пункта, Esc, щелчком в окне программы или уходом в другую
//! программу.

use eframe::egui::{
    self, Align, Context, Id, Image, Layout, Pos2, Rect, Response, ScrollArea, Stroke,
    TextWrapMode, Ui, UiBuilder, Vec2, ViewportBuilder, ViewportId, WidgetText, pos2, vec2,
};

use crate::theme;

const STATE: &str = "mh-menu";
/// Поля окна меню вокруг пунктов.
const MARGIN: f32 = 4.0;
/// Меню закрывается, если окна программы и меню так долго не в фокусе: переход фокуса между
/// окном программы и окном меню — не уход.
const UNFOCUSED_GRACE: f64 = 0.25;
/// Подменю закрывается, когда указатель над другими пунктами родителя дольше этого.
const SUBMENU_LINGER: f64 = 0.35;

/// Открытое меню: чья строка его открыла и где.
#[derive(Clone, Copy)]
struct Open {
    owner: Id,
    /// Точка щелчка в координатах экрана (точки egui).
    at: Pos2,
    /// Номер открытия: новое меню — новое окно.
    seq: u64,
    opened: f64,
    /// Кадр, в котором меню рисовалось: не рисовалось — его строка пропала, меню закрыто.
    drawn: u64,
    /// Окно программы или меню было в фокусе: уход фокуса считается только после этого.
    had_focus: bool,
    unfocused_since: Option<f64>,
}

/// Меню по правому щелчку на `response`, как `Response::context_menu`, но своим окном.
pub fn context_menu(response: &Response, kind: &str, add: impl FnOnce(&mut Ui)) {
    let ctx = &response.ctx;
    let key = Id::new(STATE);
    if response.secondary_clicked() {
        let local = response.interact_pointer_pos().unwrap_or(response.rect.center());
        let seq = ctx.data(|d| d.get_temp::<Open>(key)).map_or(0, |open| open.seq) + 1;
        let open = Open {
            owner: response.id,
            at: window_origin(ctx) + local.to_vec2(),
            seq,
            opened: ctx.input(|i| i.time),
            drawn: 0,
            had_focus: false,
            unfocused_since: None,
        };
        ctx.data_mut(|d| d.insert_temp(key, open));
    }
    let Some(mut open) = ctx.data(|d| d.get_temp::<Open>(key)) else { return };
    if open.owner != response.id {
        return;
    }
    open.drawn = ctx.cumulative_pass_nr();
    ctx.data_mut(|d| d.insert_temp(key, open));
    let viewport = ViewportId::from_hash_of((STATE, open.seq));
    if window(ctx, viewport, open.at, None, kind, add) {
        close(ctx);
    }
}

pub fn close(ctx: &Context) {
    ctx.data_mut(|d| d.remove::<Open>(Id::new(STATE)));
    ctx.request_repaint();
}

/// Начало кадра окна программы: Esc в нём (окно меню фокус не забирает) закрывает меню,
/// а не снимает выделение.
pub fn begin_frame(ctx: &Context) {
    let key = Id::new(STATE);
    if ctx.data(|d| d.get_temp::<Open>(key)).is_some()
        && ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Escape))
    {
        close(ctx);
    }
}

/// Конец кадра окна программы: меню, строка которого не рисовалась, закрыто; щелчок в окне
/// программы или уход в другую программу — тоже.
pub fn end_frame(ctx: &Context) {
    let key = Id::new(STATE);
    let Some(mut open) = ctx.data(|d| d.get_temp::<Open>(key)) else { return };
    if open.drawn != ctx.cumulative_pass_nr() {
        close(ctx);
        return;
    }
    let now = ctx.input(|i| i.time);
    // Меню открывается при отпускании кнопки; нажатие после — щелчок мимо меню.
    if now - open.opened > 0.05 && ctx.input(|i| i.pointer.any_pressed()) {
        close(ctx);
        return;
    }
    let focused = ctx.input(|i| i.raw.viewports.values().any(|v| v.focused == Some(true)));
    if focused {
        open.had_focus = true;
        open.unfocused_since = None;
    } else if open.had_focus {
        let since = *open.unfocused_since.get_or_insert(now);
        if now - since > UNFOCUSED_GRACE {
            close(ctx);
            return;
        }
        ctx.request_repaint_after(std::time::Duration::from_millis(100));
    }
    ctx.data_mut(|d| d.insert_temp(key, open));
}

/// Компактные пункты: мельче шрифт, ниже строки.
pub fn compact(style: &mut egui::Style) {
    style.spacing.interact_size.y = 20.0;
    style.spacing.item_spacing.y = 1.0;
    style.spacing.button_padding = vec2(6.0, 1.0);
    for text_style in [egui::TextStyle::Body, egui::TextStyle::Button] {
        style.text_styles.insert(text_style, theme::regular(13.0));
    }
}

/// Подменю: строка со стрелкой; наведением открывается своё окно справа от строки (у края
/// монитора — слева). Выбор пункта в подменю закрывает всё меню.
pub fn submenu(
    ui: &mut Ui,
    label: impl Into<WidgetText>,
    icon: Option<Image<'_>>,
    add: impl FnOnce(&mut Ui),
) {
    let parent = ui.ctx().viewport_id();
    let key = Id::new((STATE, "sub", parent));
    let open: Option<(Id, f64)> = ui.data(|d| d.get_temp(key));
    let row_id = ui.next_auto_id();
    let is_open = open.is_some_and(|(id, _)| id == row_id);
    let button = match icon {
        Some(icon) => egui::Button::image_and_text(icon, label),
        None => egui::Button::new(label),
    };
    let row = ui.add(button.right_text("⏵").selected(is_open));
    let now = ui.input(|i| i.time);
    let mut state = open;
    if ui.is_enabled() && (row.hovered() || row.clicked()) {
        if is_open {
            // Время наведения — без перерисовки: подменю уже открыто.
            ui.data_mut(|d| d.insert_temp(key, (row.id, now)));
            state = Some((row.id, now));
        } else {
            state = Some((row.id, now));
        }
    } else if let Some((_, since)) = open.filter(|_| is_open)
        && ui.input(|i| i.pointer.hover_pos()).is_some()
    {
        // Указатель над другими пунктами родителя: подменю уходит не сразу — пока ведут
        // к нему наискосок.
        if now - since > SUBMENU_LINGER {
            state = None;
        } else {
            ui.ctx().request_repaint_of(ViewportId::ROOT);
        }
    }
    if state.map(|(id, _)| id) != open.map(|(id, _)| id) {
        ui.data_mut(|d| match state {
            Some(state) => {
                d.insert_temp(key, state);
            }
            None => d.remove::<(Id, f64)>(key),
        });
        ui.ctx().request_repaint_of(ViewportId::ROOT);
    }
    if !state.is_some_and(|(id, _)| id == row.id) {
        return;
    }
    let origin = window_origin(ui.ctx());
    let at = origin + vec2(row.rect.right() + MARGIN, row.rect.top() - MARGIN);
    let left = origin.x + row.rect.left() - MARGIN;
    let child = ViewportId::from_hash_of((STATE, parent, row.id));
    let kind = row.id.value().to_string();
    if window(ui.ctx(), child, at, Some(left), &kind, add) {
        ui.close();
    }
}

/// Окно меню в точке `at` экрана; `left` — левый край строки родителя: у правого края
/// монитора подменю встаёт слева от неё. `true` — меню пора закрыть.
fn window(
    ctx: &Context,
    id: ViewportId,
    at: Pos2,
    left: Option<f32>,
    kind: &str,
    add: impl FnOnce(&mut Ui),
) -> bool {
    // Размер — по прошлому такому же меню; первый раз — примерный, со следующего кадра точный.
    let size_key = Id::new((STATE, "size", kind));
    let measured: Option<Vec2> = ctx.data(|d| d.get_temp(size_key));
    let size = measured.unwrap_or(vec2(260.0, 240.0));
    let area = work_area(ctx, at);
    let width = size.x.min(area.width());
    let height = size.y.min(area.height());
    let mut x = at.x;
    if x + width > area.right() {
        x = left.unwrap_or(at.x) - width;
    }
    let x = x.clamp(area.left(), (area.right() - width).max(area.left()));
    let mut y = at.y;
    if y + height > area.bottom() {
        // Корневое меню, как в Windows, открывается над курсором, если снизу места нет.
        y = if left.is_none() && at.y - height >= area.top() {
            at.y - height
        } else {
            area.bottom() - height
        };
    }
    let y = y.max(area.top());
    let builder = ViewportBuilder::default()
        .with_title("MH Files")
        .with_decorations(false)
        .with_resizable(false)
        .with_always_on_top()
        .with_taskbar(false)
        .with_active(false)
        .with_window_type(egui::X11WindowType::PopupMenu)
        .with_position(pos2(x, y))
        .with_inner_size(vec2(width, height));
    let mut add = Some(add);
    ctx.show_viewport_immediate(id, builder, |ui, _| {
        let full = ui.max_rect();
        ui.painter().rect(
            full,
            0,
            theme::PANEL,
            Stroke::new(1.0, theme::CARD_STROKE),
            egui::StrokeKind::Inside,
        );
        let mut menu = ui.new_child(
            UiBuilder::new()
                .max_rect(full.shrink(MARGIN))
                .layout(Layout::top_down_justified(Align::LEFT))
                .closable(),
        );
        egui::containers::menu::menu_style(menu.style_mut());
        compact(menu.style_mut());
        menu.style_mut().wrap_mode = Some(TextWrapMode::Extend);
        let output = ScrollArea::vertical()
            .id_salt("menu")
            .auto_shrink([true, true])
            .max_height(full.height() - 2.0 * MARGIN)
            .show(&mut menu, |ui| {
                if let Some(add) = add.take() {
                    add(ui);
                }
            });
        let wanted = (output.content_size + Vec2::splat(2.0 * MARGIN)).ceil();
        if measured.is_none_or(|old| (old - wanted).length() > 0.5) {
            ui.data_mut(|d| d.insert_temp(size_key, wanted));
            ui.ctx().request_repaint_of(ViewportId::ROOT);
        }
        menu.should_close()
            || ui.input(|i| i.key_pressed(egui::Key::Escape) || i.viewport().close_requested())
    })
}

/// Левый верхний угол текущего окна на экране (точки egui).
fn window_origin(ctx: &Context) -> Pos2 {
    ctx.input(|i| i.viewport().inner_rect.map_or(Pos2::ZERO, |rect| rect.min))
}

/// Рабочая область монитора у точки экрана `at` (точки egui).
fn work_area(ctx: &Context, at: Pos2) -> Rect {
    let ppp = ctx.pixels_per_point();
    let physical = (at.x * ppp).round() as i32;
    let area = mh_files_platform::window::work_area(physical, (at.y * ppp).round() as i32);
    match area {
        Some([left, top, right, bottom]) => Rect::from_min_max(
            pos2(left as f32 / ppp, top as f32 / ppp),
            pos2(right as f32 / ppp, bottom as f32 / ppp),
        ),
        // Вне Windows — монитор окна от начала координат.
        None => {
            let size = ctx.input(|i| i.viewport().monitor_size).unwrap_or(vec2(1920.0, 1080.0));
            Rect::from_min_size(Pos2::ZERO, size)
        }
    }
}
