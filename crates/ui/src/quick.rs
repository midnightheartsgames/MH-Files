//! Быстрый просмотр (пробел): крупный предпросмотр поверх окна. Стрелки листают объекты
//! списка, не закрывая просмотр; пробел или Esc закрывают. Видео и звук сразу играют:
//! Enter или щелчок по кадру — пауза, Shift+стрелки — перемотка на 5 секунд, M — звук. Над
//! полоской перемотки — кадр под указателем, в конце — повтор.

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
    /// Кадр анимации (GIF) и когда следующий; Enter — пауза.
    playback: preview_ui::Playback,
    /// Видео или звук: проигрыватель создаётся в первом кадре (ему нужен контекст окна).
    wants_player: bool,
    player: Option<mh_files_platform::player::Player>,
    frame: Option<TextureHandle>,
    /// Ползунок перемотки, пока его тянут.
    seeking: Option<f64>,
    /// Эквалайзер звука: сглаженные полосы и их пики (0..1).
    bars: Vec<f32>,
    peaks: Vec<f32>,
    /// Громкость поменяли: запомнить в настройках; `true` — ползунок отпущен, пора сохранить.
    volume_changed: Option<(Option<f32>, bool)>,
    /// Звук выключили или сменили скорость — запомнить в настройках.
    sound_changed: Option<(bool, f32)>,
    /// Кадры над полоской перемотки: поток, готовые кадры (секунда, текстура) и о какой
    /// секунде спросили последней.
    peek: Option<mh_files_fs::peek::FramePeek>,
    peeks: Vec<(f64, TextureHandle)>,
    peek_asked: Option<f64>,
}

/// Скорости в меню.
const RATES: [f64; 8] = [0.25, 0.5, 0.75, 1.0, 1.25, 1.5, 1.75, 2.0];
/// Кадров над полоской в памяти не больше.
const MAX_PEEKS: usize = 150;

impl State {
    pub fn open(app: &FilesApp) -> State {
        let mut state = State {
            path: None,
            generation: 0,
            cancel: None,
            preview: None,
            texture: None,
            page: 0,
            playback: preview_ui::Playback::default(),
            wants_player: false,
            player: None,
            frame: None,
            seeking: None,
            bars: Vec::new(),
            peaks: Vec::new(),
            volume_changed: None,
            sound_changed: None,
            peek: None,
            peeks: Vec::new(),
            peek_asked: None,
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
        self.playback = preview_ui::Playback::default();
        let is_dir = tab.entry(&path).is_some_and(Entry::is_dir);
        self.player = None;
        self.frame = None;
        self.seeking = None;
        self.bars.clear();
        self.peaks.clear();
        self.peek = None;
        self.peeks.clear();
        self.peek_asked = None;
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
            animate: true,
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
        self.playback = preview_ui::Playback::default();
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
        // Эквалайзер — только звуку: у видео есть картинка.
        let name = path.file_name().unwrap_or_default().to_string_lossy();
        let audio =
            mh_files_core::entry::media_kind(&name) == Some(mh_files_core::entry::MediaKind::Audio);
        let preview = &app.settings.preview;
        let sound = mh_files_platform::player::Sound {
            volume: f64::from(preview.volume.clamp(0.0, 1.0)),
            muted: preview.muted,
            rate: f64::from(preview.rate).clamp(0.25, 4.0),
        };
        if !audio {
            quick.peek = Some(mh_files_fs::peek::FramePeek::open(path, 240, waker.clone()));
        }
        quick.player =
            Some(mh_files_platform::player::Player::open(path, 1920, audio, sound, waker));
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
                                "Enter — пауза, Shift+стрелки — перемотка, M — звук, "
                            } else if preview_ui::animated(quick.preview.as_ref()) {
                                "Enter — пауза, "
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
                quick.playback.tick(ui.ctx(), quick.preview.as_ref(), quick.texture.as_mut());
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
    // Громкость запоминается между открытиями просмотра и запусками программы.
    if let Some((volume, save)) = quick.volume_changed.take() {
        if let Some(volume) = volume {
            app.settings.preview.volume = volume;
        }
        if save {
            app.save_settings();
        }
    }
    if let Some((muted, rate)) = quick.sound_changed.take() {
        app.settings.preview.muted = muted;
        app.settings.preview.rate = rate;
        app.save_settings();
    }
    // GIF: Enter — пауза.
    if quick.player.is_none()
        && preview_ui::animated(quick.preview.as_ref())
        && ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, Key::Enter))
    {
        quick.playback.paused = !quick.playback.paused;
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
            if i.consume_key(egui::Modifiers::NONE, Key::M) {
                player.set_muted(!state.muted);
                quick.sound_changed = Some((!state.muted, state.rate as f32));
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
    // Кадры над полоской, что успели посчитаться.
    if let Some(peek) = &quick.peek {
        for (at, bitmap) in peek.take() {
            let image = egui::ColorImage::from_rgba_unmultiplied(
                [bitmap.width as usize, bitmap.height as usize],
                &bitmap.rgba,
            );
            let texture = ui.ctx().load_texture("quick-peek", image, egui::TextureOptions::LINEAR);
            if quick.peeks.len() >= MAX_PEEKS {
                drop(quick.peeks.remove(0));
            }
            quick.peeks.push((at, texture));
        }
    }
    let state = player.state();
    let controls = 40.0;
    let area = vec2(ui.available_width(), (height - controls).max(60.0));
    // Звук — эквалайзер (и обложка слева, если есть); не разложился — как раньше.
    let position = quick.seeking.unwrap_or(state.position);
    if !state.has_video && state.ready && equalizer(ui, quick, &state, position, area) {
        controls_row(ui, quick, &state);
        return;
    }
    // Видео — кадр, звук — обложка из эскиза Windows, если она есть.
    let picture = if state.has_video { quick.frame.as_ref() } else { quick.texture.as_ref() };
    let mut toggle = false;
    ui.allocate_ui(area, |ui| {
        ui.set_min_size(area);
        ui.centered_and_justified(|ui| match picture {
            Some(texture) => {
                let size = texture.size_vec2();
                let scale = (area.x / size.x).min(area.y / size.y);
                let response = ui.add(
                    egui::Image::new(texture)
                        .fit_to_exact_size(size * scale)
                        .sense(egui::Sense::click()),
                );
                // Кончилось — посередине кнопка повтора.
                if state.ended {
                    let c = response.rect.center();
                    let hovered = response.hovered();
                    ui.painter().circle_filled(
                        c,
                        34.0,
                        Color32::from_black_alpha(if hovered { 200 } else { 150 }),
                    );
                    replay_icon(ui.painter(), c, 14.0, Color32::WHITE);
                }
                let tip = if state.ended {
                    "Сначала"
                } else if state.paused {
                    "Играть"
                } else {
                    "Пауза"
                };
                toggle = response.on_hover_text(tip).clicked();
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
    if toggle && let Some(player) = &quick.player {
        if state.paused || state.ended { player.play() } else { player.pause() }
    }
    controls_row(ui, quick, &state);
}

/// Кнопка, время, перемотка, громкость.
fn controls_row(
    ui: &mut egui::Ui,
    quick: &mut State,
    state: &mh_files_platform::player::PlayerState,
) {
    let Some(player) = &quick.player else { return };
    ui.add_space(6.0);
    let mut peek = None;
    ui.horizontal(|ui| {
        let paused = state.paused || state.ended;
        let (rect, response) = ui.allocate_exact_size(vec2(32.0, 28.0), egui::Sense::click());
        let color = if response.hovered() { theme::TEXT_PRIMARY } else { theme::TEXT_SECONDARY };
        let c = rect.center();
        if state.ended {
            replay_icon(ui.painter(), c, 8.0, color);
        } else if paused {
            let points = vec![c + vec2(-5.0, -7.0), c + vec2(-5.0, 7.0), c + vec2(7.0, 0.0)];
            ui.painter().add(egui::Shape::convex_polygon(points, color, Stroke::NONE));
        } else {
            for dx in [-4.0, 4.0] {
                let bar = egui::Rect::from_center_size(c + vec2(dx, 0.0), vec2(3.5, 14.0));
                ui.painter().rect_filled(bar, CornerRadius::ZERO, color);
            }
        }
        let tip = if state.ended {
            "Сначала (Enter)"
        } else if paused {
            "Играть (Enter)"
        } else {
            "Пауза (Enter)"
        };
        if response.on_hover_text(tip).clicked() {
            if paused { player.play() } else { player.pause() }
        }
        let position = quick.seeking.unwrap_or(state.position);
        ui.label(
            RichText::new(format!("{} / {}", clock(position), clock(state.duration)))
                .color(theme::TEXT_SECONDARY),
        );
        let volume_width = 110.0;
        let tail = volume_width + 52.0 + 32.0 + 40.0;
        let width = (ui.available_width() - tail).max(80.0);
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
            if state.has_video
                && let Some(pointer) = slider.hover_pos()
            {
                // Ось ползунка — без радиуса ручки по краям, как у egui::Slider.
                let range = slider.rect.x_range().shrink(slider.rect.height() / 2.5);
                let share = ((pointer.x - range.min) / range.span().max(1.0)).clamp(0.0, 1.0);
                peek = Some((f64::from(share) * state.duration, slider.rect, pointer.x));
            }
        } else {
            ui.add_space(width);
        }
        // Скорость.
        let rate = if state.rate > 0.0 { state.rate } else { 1.0 };
        let mut picked_rate = None;
        ui.menu_button(rate_label(rate), |ui| {
            for value in RATES {
                if ui.selectable_label((value - rate).abs() < 1e-3, rate_label(value)).clicked() {
                    picked_rate = Some(value);
                    ui.close();
                }
            }
        })
        .response
        .on_hover_text("Скорость воспроизведения");
        if let Some(value) = picked_rate {
            player.set_rate(value);
            quick.sound_changed = Some((state.muted, value as f32));
        }
        // Звук: выключить и вернуть.
        let (rect, response) = ui.allocate_exact_size(vec2(28.0, 28.0), egui::Sense::click());
        let color = if response.hovered() { theme::TEXT_PRIMARY } else { theme::TEXT_SECONDARY };
        speaker_icon(ui.painter(), rect.center(), state.muted || state.volume <= 0.0, color);
        let tip = if state.muted {
            "Включить звук (M)"
        } else {
            "Выключить звук (M)"
        };
        if response.on_hover_text(tip).clicked() {
            player.set_muted(!state.muted);
            quick.sound_changed = Some((!state.muted, rate as f32));
        }
        let mut volume = state.volume;
        ui.spacing_mut().slider_width = volume_width - 20.0;
        let slider = ui
            .add(egui::Slider::new(&mut volume, 0.0..=1.0).show_value(false))
            .on_hover_text("Громкость");
        if slider.changed() {
            player.set_volume(volume);
            // Громкость тронули — значит, звук нужен.
            if state.muted {
                player.set_muted(false);
                quick.sound_changed = Some((false, rate as f32));
            }
        }
        if slider.changed() || slider.drag_stopped() {
            // Пока тянут, громкость только запоминается; отпустили — сохраняется.
            let value = slider.changed().then_some(volume as f32);
            quick.volume_changed = Some((value, !slider.dragged()));
        }
    });
    if let Some((at, bar, x)) = peek {
        peek_over(ui, quick, at, bar, x, state.duration);
    }
}

/// «1×», «1,5×», «0,25×».
fn rate_label(rate: f64) -> String {
    let text = format!("{rate}").replace('.', ",");
    format!("{text}×")
}

/// Кадр над полоской перемотки у секунды `at` (указатель — на `x`). Готового нет — спросить
/// поток и показать ближайший готовый.
fn peek_over(ui: &egui::Ui, quick: &mut State, at: f64, bar: egui::Rect, x: f32, duration: f64) {
    let Some(peek) = &quick.peek else { return };
    // Кадры — с шагом в 1/150 длины (не чаще секунды): соседние положения указателя
    // показывают один и тот же кадр без нового чтения.
    let step = (duration / 150.0).max(1.0);
    let bucket = (at / step).floor() * step + step / 2.0;
    let bucket = bucket.min(duration);
    let exact = quick.peeks.iter().find(|(t, _)| (t - bucket).abs() < 1e-6);
    if exact.is_none() && quick.peek_asked != Some(bucket) {
        peek.request(bucket);
        quick.peek_asked = Some(bucket);
    }
    let texture = exact.or_else(|| {
        quick
            .peeks
            .iter()
            .filter(|(t, _)| (t - at).abs() <= step * 4.0)
            .min_by(|a, b| (a.0 - at).abs().total_cmp(&(b.0 - at).abs()))
    });
    let width = 200.0;
    let height = texture.map_or(112.0, |(_, t)| width * t.size_vec2().y / t.size_vec2().x.max(1.0));
    let screen = ui.ctx().content_rect();
    let left = (x - width / 2.0 - 6.0).clamp(screen.left() + 4.0, screen.right() - width - 16.0);
    let top = bar.top() - height - 36.0;
    egui::Area::new(Id::new("quick-peek"))
        .order(egui::Order::Tooltip)
        .fixed_pos(egui::pos2(left, top))
        .interactable(false)
        .show(ui.ctx(), |ui| {
            egui::Frame::popup(ui.style()).inner_margin(egui::Margin::same(4)).show(ui, |ui| {
                ui.vertical_centered(|ui| {
                    match texture {
                        Some((_, texture)) => {
                            ui.add(
                                egui::Image::new(texture).fit_to_exact_size(vec2(width, height)),
                            );
                        }
                        None => {
                            let (rect, _) =
                                ui.allocate_exact_size(vec2(width, height), egui::Sense::hover());
                            ui.painter().rect_filled(rect, 2.0, Color32::from_black_alpha(120));
                        }
                    }
                    ui.label(RichText::new(clock(at)).font(theme::regular(12.5)));
                });
            });
        });
}

/// Круговая стрелка «сначала»: дуга почти на полный круг и наконечник.
fn replay_icon(painter: &egui::Painter, center: egui::Pos2, radius: f32, color: Color32) {
    let stroke = Stroke::new((radius / 5.0).max(1.6), color);
    let start = -std::f32::consts::FRAC_PI_2 + 0.5;
    let end = start + std::f32::consts::TAU * 0.8;
    let point = |a: f32| center + vec2(a.cos(), a.sin()) * radius;
    let points: Vec<egui::Pos2> =
        (0..=24).map(|i| point(start + (end - start) * i as f32 / 24.0)).collect();
    painter.add(egui::Shape::line(points, stroke));
    // Наконечник в начале дуги, против хода стрелки.
    let tip = point(start);
    let tangent = vec2(-start.sin(), start.cos());
    let normal = vec2(start.cos(), start.sin());
    let size = radius * 0.55;
    let arrow = vec![tip - tangent * size, tip + normal * size * 0.7, tip - normal * size * 0.7];
    painter.add(egui::Shape::convex_polygon(arrow, color, Stroke::NONE));
}

/// Динамик: со звуковыми дугами или перечёркнутый.
fn speaker_icon(painter: &egui::Painter, center: egui::Pos2, muted: bool, color: Color32) {
    let c = center + vec2(-4.0, 0.0);
    let body = egui::Rect::from_center_size(c + vec2(-3.0, 0.0), vec2(4.0, 6.0));
    painter.rect_filled(body, CornerRadius::ZERO, color);
    let cone =
        vec![c + vec2(-1.0, -3.0), c + vec2(4.0, -7.0), c + vec2(4.0, 7.0), c + vec2(-1.0, 3.0)];
    painter.add(egui::Shape::convex_polygon(cone, color, Stroke::NONE));
    let stroke = Stroke::new(1.5, color);
    if muted {
        let x = c + vec2(10.0, 0.0);
        painter.line_segment([x + vec2(-3.0, -3.0), x + vec2(3.0, 3.0)], stroke);
        painter.line_segment([x + vec2(-3.0, 3.0), x + vec2(3.0, -3.0)], stroke);
    } else {
        for radius in [5.0f32, 9.0] {
            let points: Vec<egui::Pos2> = (0..=10)
                .map(|i| {
                    let a = -0.8 + 1.6 * i as f32 / 10.0;
                    c + vec2(4.0, 0.0) + vec2(a.cos(), a.sin()) * radius
                })
                .collect();
            painter.add(egui::Shape::line(points, stroke));
        }
    }
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

/// Эквалайзер звука: полосы спектра в такт воспроизведению, с пиками, которые медленно
/// опускаются. Обложка (эскиз Windows), если есть, — квадратом слева. `false` — спектра нет
/// (не разложился), рисовать нечего.
fn equalizer(
    ui: &mut egui::Ui,
    quick: &mut State,
    state: &mh_files_platform::player::PlayerState,
    position: f64,
    area: egui::Vec2,
) -> bool {
    use mh_files_platform::spectrum::BANDS;
    let Some(player) = &quick.player else { return false };
    let mut levels = [0.0f32; BANDS];
    let known = player.levels(position, &mut levels);
    if !known && player.spectrum_done() && quick.bars.is_empty() {
        return false;
    }
    let playing = !state.paused && !state.ended;
    if !playing || !known {
        levels = [0.0; BANDS];
    }
    quick.bars.resize(BANDS, 0.0);
    quick.peaks.resize(BANDS, 0.0);
    let dt = ui.input(|i| i.stable_dt).min(0.1);
    // Быстрый подъём, плавный спад; пики опускаются чуть больше чем за секунду.
    let (attack, release) = (1.0 - (-dt / 0.03).exp(), 1.0 - (-dt / 0.18).exp());
    for ((bar, peak), target) in quick.bars.iter_mut().zip(&mut quick.peaks).zip(levels) {
        *bar += (target - *bar) * if target > *bar { attack } else { release };
        *peak = if *bar > *peak { *bar } else { (*peak - 0.7 * dt).max(*bar) };
    }
    let (rect, response) = ui.allocate_exact_size(area, egui::Sense::click());
    if response.clicked() {
        if state.paused || state.ended { player.play() } else { player.pause() }
    }
    let painter = ui.painter_at(rect);
    let mut bars_rect = rect.shrink2(vec2(8.0, 12.0));
    if let Some(cover) = &quick.texture {
        let side = (rect.height() * 0.8).min(rect.width() * 0.35);
        let cover_rect = egui::Rect::from_min_size(
            egui::pos2(rect.left() + 8.0, rect.center().y - side / 2.0),
            vec2(side, side),
        );
        let size = cover.size_vec2();
        let scale = (side / size.x).min(side / size.y);
        let image = egui::Rect::from_center_size(cover_rect.center(), size * scale);
        painter.image(
            cover.id(),
            image,
            egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
            Color32::WHITE,
        );
        bars_rect.min.x = cover_rect.right() + 24.0;
    }
    let gap = 3.0;
    let width = ((bars_rect.width() - gap * (BANDS - 1) as f32) / BANDS as f32).clamp(2.0, 18.0);
    let total = width * BANDS as f32 + gap * (BANDS - 1) as f32;
    let left = bars_rect.center().x - total / 2.0;
    let bottom = bars_rect.bottom();
    let height = bars_rect.height();
    let accent = theme::accent();
    painter.line_segment(
        [egui::pos2(left, bottom + 2.0), egui::pos2(left + total, bottom + 2.0)],
        Stroke::new(1.0, theme::CARD_STROKE),
    );
    for i in 0..BANDS {
        let x = left + i as f32 * (width + gap);
        let level = quick.bars[i];
        let h = (level * height).max(2.0);
        let color = accent.gamma_multiply(0.45 + 0.55 * level);
        painter.rect_filled(
            egui::Rect::from_min_max(egui::pos2(x, bottom - h), egui::pos2(x + width, bottom)),
            CornerRadius::same(2),
            color,
        );
        let peak = bottom - (quick.peaks[i] * height).max(2.0) - 3.0;
        painter.rect_filled(
            egui::Rect::from_min_max(egui::pos2(x, peak - 2.0), egui::pos2(x + width, peak)),
            CornerRadius::ZERO,
            theme::TEXT_SECONDARY,
        );
    }
    let moving = quick.bars.iter().chain(&quick.peaks).any(|&v| v > 0.002);
    if playing || moving || !known {
        ui.ctx().request_repaint();
    }
    true
}
