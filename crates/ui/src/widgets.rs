//! Элементы в стиле MH: карточка, строка «подпись — управление», тумблер, кнопки. Окно
//! настроек устроено как у MH Monitoring: подписи слева, управление в общей колонке.

use eframe::egui::{
    self, Align, Color32, CornerRadius, Layout, Margin, Response, RichText, Sense, Stroke, Ui, vec2,
};

use crate::theme;

const CONTROL_COLUMN: f32 = 230.0;
const ROW_HEIGHT: f32 = 28.0;

pub fn page_title(ui: &mut Ui, title: &str) {
    ui.label(RichText::new(title).font(theme::bold(22.0)).color(theme::TEXT_PRIMARY));
    ui.add_space(8.0);
}

/// Карточка с заголовком — группа связанных настроек.
pub fn card<R>(ui: &mut Ui, title: &str, add_contents: impl FnOnce(&mut Ui) -> R) -> R {
    let inner = egui::Frame::new()
        .fill(theme::CARD)
        .stroke(Stroke::new(1.0, theme::CARD_STROKE))
        .corner_radius(CornerRadius::same(6))
        .inner_margin(Margin::symmetric(16, 12))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.label(RichText::new(title).font(theme::bold(15.0)).color(theme::TEXT_PRIMARY));
            ui.add_space(6.0);
            add_contents(ui)
        })
        .inner;
    ui.add_space(10.0);
    inner
}

/// Строка с подписью и управлением в общей колонке.
pub fn row<R>(ui: &mut Ui, label: &str, add_control: impl FnOnce(&mut Ui) -> R) -> R {
    ui.allocate_ui_with_layout(
        vec2(ui.available_width(), ROW_HEIGHT),
        Layout::left_to_right(Align::Center),
        |ui| {
            let start = ui.cursor().left();
            ui.label(RichText::new(label).font(theme::regular(14.0)).color(theme::TEXT_PRIMARY));
            let used = ui.cursor().left() - start;
            ui.add_space((CONTROL_COLUMN - used).max(8.0));
            add_control(ui)
        },
    )
    .inner
}

/// Строка с тумблером у правого края.
pub fn switch_row(ui: &mut Ui, label: &str, help: Option<&str>, value: &mut bool) -> Response {
    ui.allocate_ui_with_layout(
        vec2(ui.available_width(), ROW_HEIGHT),
        Layout::left_to_right(Align::Center),
        |ui| {
            ui.label(RichText::new(label).font(theme::regular(14.0)).color(theme::TEXT_PRIMARY));
            if let Some(help) = help {
                ui.add_space(4.0);
                help_icon(ui, help);
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| ui.add(Switch(value))).inner
        },
    )
    .inner
}

/// Кружок «?», по наведению — пояснение.
pub fn help_icon(ui: &mut Ui, text: &str) -> Response {
    let (rect, response) = ui.allocate_exact_size(vec2(16.0, 16.0), Sense::hover());
    let color = if response.hovered() { theme::accent() } else { theme::TEXT_SECONDARY };
    let painter = ui.painter();
    painter.circle_stroke(rect.center(), 7.0, Stroke::new(1.2, color));
    painter.text(rect.center(), egui::Align2::CENTER_CENTER, "?", theme::bold(11.0), color);
    response.on_hover_text(text)
}

/// Пояснение мелким шрифтом.
pub fn hint(ui: &mut Ui, text: &str) {
    ui.add(
        egui::Label::new(
            RichText::new(text).font(theme::regular(12.5)).color(theme::TEXT_DISABLED),
        )
        .wrap(),
    );
}

/// Кнопка в стиле окна: основная — залитая акцентом.
pub fn button(ui: &mut Ui, text: &str, primary: bool) -> Response {
    let font = theme::regular(14.0);
    let galley = ui.painter().layout_no_wrap(text.to_owned(), font, Color32::PLACEHOLDER);
    let size = vec2((galley.size().x + 28.0).max(110.0), 30.0);
    let (rect, response) = ui.allocate_exact_size(size, Sense::click());
    response
        .widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, ui.is_enabled(), text));
    if ui.is_rect_visible(rect) {
        // Свой цвет фона у кнопки egui не меняет при наведении — подсветка своя: светлее
        // при наведении, темнее при нажатии, рамка цвета акцента.
        let hover = ui.ctx().animate_bool_responsive(response.id, response.hovered());
        let (base, color) = if primary {
            (theme::accent(), theme::BACKGROUND)
        } else {
            (theme::FIELD, theme::TEXT_PRIMARY)
        };
        let lit = if primary { lerp_color(base, Color32::WHITE, 0.18) } else { theme::CARD_STROKE };
        let mut fill = lerp_color(base, lit, hover);
        if response.is_pointer_button_down_on() {
            fill = lerp_color(fill, Color32::BLACK, 0.15);
        }
        let stroke = lerp_color(theme::CARD_STROKE, theme::accent(), hover);
        let painter = ui.painter();
        painter.rect(
            rect,
            CornerRadius::same(4),
            fill,
            Stroke::new(1.0, stroke),
            egui::StrokeKind::Inside,
        );
        painter.galley(rect.center() - galley.size() / 2.0, galley, color);
    }
    response
}

/// Тумблер «вкл/выкл».
pub struct Switch<'a>(pub &'a mut bool);

impl egui::Widget for Switch<'_> {
    fn ui(self, ui: &mut Ui) -> Response {
        let size = vec2(38.0, 20.0);
        let (rect, mut response) = ui.allocate_exact_size(size, Sense::click());
        if response.clicked() {
            *self.0 = !*self.0;
            response.mark_changed();
        }
        response.widget_info(|| {
            egui::WidgetInfo::selected(egui::WidgetType::Checkbox, ui.is_enabled(), *self.0, "")
        });
        if ui.is_rect_visible(rect) {
            let progress = ui.ctx().animate_bool_responsive(response.id, *self.0);
            let track = lerp_color(theme::SWITCH_OFF, theme::accent(), progress);
            let track = if ui.is_enabled() { track } else { track.gamma_multiply(0.4) };
            let radius = rect.height() / 2.0;
            let painter = ui.painter();
            painter.rect_filled(rect, CornerRadius::same(radius as u8), track);
            let x = egui::lerp((rect.left() + radius)..=(rect.right() - radius), progress);
            let knob = if response.hovered() { Color32::WHITE } else { theme::TEXT_PRIMARY };
            painter.circle_filled(egui::pos2(x, rect.center().y), radius - 3.0, knob);
        }
        response
    }
}

pub fn lerp_color(from: Color32, to: Color32, t: f32) -> Color32 {
    let mix = |a: u8, b: u8| (f32::from(a) + (f32::from(b) - f32::from(a)) * t).round() as u8;
    Color32::from_rgb(mix(from.r(), to.r()), mix(from.g(), to.g()), mix(from.b(), to.b()))
}

/// Полоса заполнения (диски): цвет по порогам, как нагрузка в MH Monitoring.
pub fn paint_usage_bar(painter: &egui::Painter, rect: egui::Rect, fraction: f32) {
    let radius = CornerRadius::same((rect.height() / 2.0) as u8);
    painter.rect_filled(rect, radius, theme::FIELD);
    let fraction = fraction.clamp(0.0, 1.0);
    if fraction > 0.0 {
        let mut filled = rect;
        filled.set_width((rect.width() * fraction).max(rect.height()));
        painter.rect_filled(filled, radius, theme::usage_color(fraction));
    }
}

/// Плоская кнопка-значок для панелей инструментов. Рисует `paint` в квадрате.
pub fn icon_button(
    ui: &mut Ui,
    enabled: bool,
    tooltip: &str,
    paint: impl FnOnce(&egui::Painter, egui::Rect, Color32),
) -> Response {
    let size = vec2(26.0, 26.0);
    let sense = if enabled { Sense::click() } else { Sense::hover() };
    let (rect, response) = ui.allocate_exact_size(size, sense);
    if ui.is_rect_visible(rect) {
        let painter = ui.painter();
        if enabled && response.hovered() {
            painter.rect_filled(rect, CornerRadius::same(4), theme::CARD);
        }
        let color = if !enabled {
            theme::TEXT_DISABLED
        } else if response.hovered() {
            theme::TEXT_PRIMARY
        } else {
            theme::TEXT_SECONDARY
        };
        paint(painter, rect.shrink(6.0), color);
    }
    if tooltip.is_empty() { response } else { response.on_hover_text(tooltip) }
}

/// Подпись раздела боковой панели: мелкие заглавные, как заголовок MH Sidebar.
pub fn section_label(ui: &mut Ui, text: &str) {
    ui.label(
        RichText::new(text.to_uppercase())
            .font(theme::bold(11.5))
            .color(theme::TEXT_DISABLED)
            .extra_letter_spacing(1.2),
    );
}
