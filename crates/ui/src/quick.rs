//! Быстрый просмотр (пробел): крупный предпросмотр поверх окна. Стрелки листают объекты
//! списка, не закрывая просмотр; пробел или Esc закрывают.

use std::path::PathBuf;

use eframe::egui::{
    self, Align2, Color32, CornerRadius, Id, Key, RichText, Stroke, TextureHandle, vec2,
};
use mh_files_core::Entry;
use mh_files_core::format;
use mh_files_core::selection::Modifiers;
use mh_files_fs::{CancelToken, Preview, PreviewRequest, Ticket};

use crate::app::{FilesApp, OWNER_QUICK};
use crate::{preview_ui, theme};

pub struct State {
    path: Option<PathBuf>,
    generation: u64,
    cancel: Option<CancelToken>,
    preview: Option<Preview>,
    texture: Option<TextureHandle>,
    /// Страница PDF.
    page: u32,
}

impl State {
    pub fn open(app: &FilesApp) -> State {
        let mut state = State {
            path: None,
            generation: 0,
            cancel: None,
            preview: None,
            texture: None,
            page: 0,
        };
        state.request(app, None);
        state
    }

    /// Запросить предпросмотр объекта под курсором; `page` — другая страница того же PDF.
    fn request(&mut self, app: &FilesApp, page: Option<u32>) {
        let tab = app.tab();
        let Some(path) = tab.selection.cursor().cloned().or_else(|| tab.targets().first().cloned())
        else {
            return;
        };
        if self.path.as_ref() == Some(&path) && page.is_none() {
            return;
        }
        self.page = page.unwrap_or(0);
        if let Some(cancel) = self.cancel.take() {
            cancel.cancel();
        }
        self.generation += 1;
        self.path = Some(path.clone());
        self.preview = None;
        self.texture = None;
        let is_dir = tab.entry(&path).is_some_and(Entry::is_dir);
        let request = PreviewRequest {
            path,
            is_dir,
            max_side: 2048,
            text_limit: app.settings.preview.text_limit_kb as usize * 1024 * 4,
            image_limit: u64::from(app.settings.preview.image_limit_mb) * 1024 * 1024,
            page: self.page,
        };
        let ticket = Ticket { owner: OWNER_QUICK, generation: self.generation };
        self.cancel = Some(app.workers.preview(ticket, request));
    }

    pub fn on_preview(
        &mut self,
        ctx: &egui::Context,
        ticket: Ticket,
        path: PathBuf,
        preview: Preview,
    ) {
        if ticket.generation != self.generation || self.path.as_ref() != Some(&path) {
            return;
        }
        self.texture = preview_ui::texture_of(ctx, &preview, "quick");
        self.preview = Some(preview);
    }
}

pub fn show(ctx: &egui::Context, app: &mut FilesApp) {
    let (close, step) = ctx.input_mut(|i| {
        let close = i.consume_key(egui::Modifiers::NONE, Key::Space)
            || i.consume_key(egui::Modifiers::NONE, Key::Escape);
        let mut step = 0isize;
        for key in [Key::ArrowRight, Key::ArrowDown] {
            if i.consume_key(egui::Modifiers::NONE, key) {
                step = 1;
            }
        }
        for key in [Key::ArrowLeft, Key::ArrowUp] {
            if i.consume_key(egui::Modifiers::NONE, key) {
                step = -1;
            }
        }
        (close, step)
    });
    if close {
        app.quick = None;
        return;
    }
    if step != 0 {
        app.tab_mut().move_cursor(step, Modifiers::default());
        if let Some(mut quick) = app.quick.take() {
            quick.request(app, None);
            app.quick = Some(quick);
        }
    }
    let Some(mut quick) = app.quick.take() else { return };
    let mut turn = None;
    let screen = ctx.content_rect();
    let mut keep = true;
    egui::Area::new(Id::new("quick-backdrop"))
        .order(egui::Order::Foreground)
        .fixed_pos(screen.min)
        .show(ctx, |ui| {
            let response = ui.allocate_rect(screen, egui::Sense::click());
            ui.painter().rect_filled(screen, CornerRadius::ZERO, Color32::from_black_alpha(190));
            if response.clicked() {
                keep = false;
            }
        });
    let size = vec2(screen.width() * 0.86, screen.height() * 0.86);
    egui::Area::new(Id::new("quick-view"))
        .order(egui::Order::Foreground)
        .anchor(Align2::CENTER_CENTER, vec2(0.0, 0.0))
        .show(ctx, |ui| {
            egui::Frame::new()
                .fill(theme::PANEL)
                .stroke(Stroke::new(1.0, theme::CARD_STROKE))
                .corner_radius(CornerRadius::same(8))
                .inner_margin(egui::Margin::same(14))
                .show(ui, |ui| {
                    ui.set_width(size.x);
                    ui.set_height(size.y);
                    let tab = app.tab();
                    let name = quick
                        .path
                        .as_ref()
                        .map(|p| mh_files_core::location::path_label(p))
                        .unwrap_or_default();
                    let index = tab.selection.cursor_row(&tab.listing).map_or(0, |r| r + 1);
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(name).font(theme::bold(18.0)));
                        if let Some(entry) = quick.path.as_ref().and_then(|p| tab.entry(p))
                            && !entry.is_dir()
                        {
                            ui.label(
                                RichText::new(format::size(entry.size))
                                    .color(theme::TEXT_SECONDARY),
                            );
                        }
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            ui.label(
                                RichText::new(format!(
                                    "{index} / {}   стрелки — листать, пробел — закрыть",
                                    tab.listing.len()
                                ))
                                .color(theme::TEXT_DISABLED),
                            );
                        });
                    });
                    ui.add_space(10.0);
                    let height = ui.available_height() - 10.0;
                    turn = preview_ui::show(
                        ui,
                        quick.preview.as_ref(),
                        quick.texture.as_ref(),
                        height,
                        "quick-text",
                    );
                });
        });
    // PageUp/PageDown листают страницы PDF.
    if let Some(Preview::Image { pages: Some((page, count)), .. }) = &quick.preview {
        let (page, count) = (*page, *count);
        ctx.input_mut(|i| {
            if i.consume_key(egui::Modifiers::NONE, Key::PageDown) && page + 1 < count {
                turn = Some(page + 1);
            }
            if i.consume_key(egui::Modifiers::NONE, Key::PageUp) && page > 0 {
                turn = Some(page - 1);
            }
        });
    }
    if let Some(page) = turn {
        quick.request(app, Some(page));
    }
    if keep {
        app.quick = Some(quick);
    }
}
