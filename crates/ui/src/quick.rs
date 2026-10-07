//! Быстрый просмотр (пробел): крупный предпросмотр поверх окна. Стрелки листают объекты
//! списка, не закрывая просмотр; пробел или Esc закрывают. Видео и звук сразу играют:
//! Enter — пауза, Shift+стрелки — перемотка на 5 секунд.

use std::path::PathBuf;

use eframe::egui::{self, Color32, CornerRadius, Id, Key, RichText, Stroke, TextureHandle, vec2};
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
    /// Видео или звук: проигрыватель создаётся в первом кадре (ему нужен контекст окна).
    wants_player: bool,
    player: Option<mh_files_platform::player::Player>,
    frame: Option<TextureHandle>,
    /// Ползунок перемотки, пока его тянут.
    seeking: Option<f64>,
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
            wants_player: false,
            player: None,
            frame: None,
            seeking: None,
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
        self.player = None;
        self.frame = None;
        self.seeking = None;
        let name = path.file_name().unwrap_or_default().to_string_lossy();
        self.wants_player = app.settings.preview.media
            && !is_dir
            && mh_files_core::entry::media_kind(&name).is_some()
            && !matches!(tab.location, mh_files_core::location::Location::Archive { .. });
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
    if std::mem::take(&mut quick.wants_player)
        && let Some(path) = &quick.path
    {
        let repaint = ctx.clone();
        let waker: mh_files_platform::Waker =
            std::sync::Arc::new(move || repaint.request_repaint());
        quick.player = Some(mh_files_platform::player::Player::open(path, 1920, waker));
    }
    let mut turn = None;
    let screen = ctx.content_rect();
    let mut keep = true;
    let size = vec2(screen.width() * 0.86, screen.height() * 0.86);
    // Затемнение и карточка — один слой (Modal). Двумя Area порядок слоёв в памяти egui
    // переворачивался после щелчка по затемнению, и при следующем открытии затемнение
    // ложилось поверх карточки: всё темнело, любой щелчок закрывал просмотр.
    let modal = egui::Modal::new(Id::new("quick-view"))
        .backdrop_color(Color32::from_black_alpha(190))
        .frame(
            egui::Frame::new()
                .fill(theme::PANEL)
                .stroke(Stroke::new(1.0, theme::CARD_STROKE))
                .corner_radius(CornerRadius::same(8))
                .inner_margin(egui::Margin::same(14)),
        )
        .show(ctx, |ui| {
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
                    ui.label(RichText::new(format::size(entry.size)).color(theme::TEXT_SECONDARY));
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.label(
                        RichText::new(format!(
                            "{index} / {}   стрелки — листать, {}пробел — закрыть",
                            tab.listing.len(),
                            if quick.player.is_some() {
                                "Enter — пауза, Shift+стрелки — перемотка, "
                            } else {
                                ""
                            }
                        ))
                        .color(theme::TEXT_DISABLED),
                    );
                });
            });
            ui.add_space(10.0);
            let height = ui.available_height() - 10.0;
            let playing =
                quick.player.as_ref().is_some_and(|player| player.state().error.is_none());
            if playing {
                media(ui, &mut quick, height);
            } else {
                if let Some(error) = quick.player.as_ref().and_then(|p| p.state().error) {
                    ui.label(RichText::new(error).color(theme::TEXT_DISABLED));
                    ui.add_space(6.0);
                }
                turn = preview_ui::show(
                    ui,
                    quick.preview.as_ref(),
                    quick.texture.as_ref(),
                    height,
                    "quick-text",
                );
            }
        });
    if modal.backdrop_response.clicked() {
        keep = false;
    }
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
    // Enter — пауза, Shift+стрелки — на 5 секунд назад и вперёд.
    if let Some(player) = &quick.player {
        let state = player.state();
        ctx.input_mut(|i| {
            if i.consume_key(egui::Modifiers::NONE, Key::Enter) {
                if state.paused || state.ended { player.play() } else { player.pause() }
            }
            if i.consume_key(egui::Modifiers::SHIFT, Key::ArrowRight) {
                player.seek(state.position + 5.0);
            }
            if i.consume_key(egui::Modifiers::SHIFT, Key::ArrowLeft) {
                player.seek(state.position - 5.0);
            }
        });
    }
    if keep {
        app.quick = Some(quick);
    }
}

/// Проигрыватель: кадр (или обложка для звука), под ним — кнопка, время, перемотка,
/// громкость.
fn media(ui: &mut egui::Ui, quick: &mut State, height: f32) {
    let Some(player) = &quick.player else { return };
    if let Some(frame) = player.take_frame() {
        let image = egui::ColorImage::from_rgba_unmultiplied(
            [frame.width as usize, frame.height as usize],
            &frame.rgba,
        );
        match &mut quick.frame {
            Some(texture) => texture.set(image, egui::TextureOptions::LINEAR),
            None => {
                quick.frame =
                    Some(ui.ctx().load_texture("quick-video", image, egui::TextureOptions::LINEAR))
            }
        }
    }
    let state = player.state();
    let controls = 40.0;
    let area = vec2(ui.available_width(), (height - controls).max(60.0));
    // Видео — кадр, звук — обложка из эскиза Windows, если она есть.
    let picture = if state.has_video { quick.frame.as_ref() } else { quick.texture.as_ref() };
    ui.allocate_ui(area, |ui| {
        ui.set_min_size(area);
        ui.centered_and_justified(|ui| match picture {
            Some(texture) => {
                let size = texture.size_vec2();
                let scale = (area.x / size.x).min(area.y / size.y);
                ui.add(egui::Image::new(texture).fit_to_exact_size(size * scale));
            }
            None if !state.ready => {
                ui.add(egui::Spinner::new().color(theme::accent()));
            }
            None => {
                let text = if state.has_video { "" } else { "Звук" };
                ui.label(RichText::new(text).font(theme::bold(28.0)).color(theme::TEXT_DISABLED));
            }
        });
    });
    ui.add_space(6.0);
    ui.horizontal(|ui| {
        let paused = state.paused || state.ended;
        let (rect, response) = ui.allocate_exact_size(vec2(32.0, 28.0), egui::Sense::click());
        let color = if response.hovered() { theme::TEXT_PRIMARY } else { theme::TEXT_SECONDARY };
        let c = rect.center();
        if paused {
            let points = vec![c + vec2(-5.0, -7.0), c + vec2(-5.0, 7.0), c + vec2(7.0, 0.0)];
            ui.painter().add(egui::Shape::convex_polygon(points, color, Stroke::NONE));
        } else {
            for dx in [-4.0, 4.0] {
                let bar = egui::Rect::from_center_size(c + vec2(dx, 0.0), vec2(3.5, 14.0));
                ui.painter().rect_filled(bar, CornerRadius::ZERO, color);
            }
        }
        if response.on_hover_text(if paused { "Играть (Enter)" } else { "Пауза (Enter)" }).clicked()
        {
            if paused { player.play() } else { player.pause() }
        }
        let position = quick.seeking.unwrap_or(state.position);
        ui.label(
            RichText::new(format!("{} / {}", clock(position), clock(state.duration)))
                .color(theme::TEXT_SECONDARY),
        );
        let volume_width = 110.0;
        let width = (ui.available_width() - volume_width - 40.0).max(80.0);
        if state.duration > 0.0 {
            let mut value = position;
            ui.spacing_mut().slider_width = width;
            let slider =
                ui.add(egui::Slider::new(&mut value, 0.0..=state.duration).show_value(false));
            if slider.dragged() || slider.changed() {
                quick.seeking = Some(value);
            }
            if (slider.drag_stopped() || slider.changed() && !slider.dragged())
                && let Some(target) = quick.seeking.take()
            {
                player.seek(target);
            }
        } else {
            ui.add_space(width);
        }
        let mut volume = state.volume;
        ui.spacing_mut().slider_width = volume_width - 20.0;
        if ui
            .add(egui::Slider::new(&mut volume, 0.0..=1.0).show_value(false))
            .on_hover_text("Громкость")
            .changed()
        {
            player.set_volume(volume);
        }
    });
}

/// «1:05», «1:02:03».
fn clock(seconds: f64) -> String {
    let total = seconds.max(0.0) as u64;
    let (hours, minutes, secs) = (total / 3600, total / 60 % 60, total % 60);
    if hours > 0 {
        format!("{hours}:{minutes:02}:{secs:02}")
    } else {
        format!("{minutes}:{secs:02}")
    }
}
