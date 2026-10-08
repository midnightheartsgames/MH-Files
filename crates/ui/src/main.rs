//! MH Files — файловый менеджер для Windows (PLAN.md).

// В отладочной сборке консоль остаётся: в неё пишут паники.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod actions;
mod app;
mod archives;
mod batch;
mod categories_editor;
mod columns;
mod commands;
mod crash;
mod diag;
mod dialogs;
mod icons;
mod images;
mod inspector;
mod operations;
mod palette;
mod pane_view;
mod popup;
mod preview_ui;
mod quick;
mod settings_window;
mod shell_menu;
mod sidebar;
mod sorter;
mod startup;
mod statusbar;
mod storage;
mod tabs;
mod theme;
mod titlebar;
mod widgets;

use eframe::egui::{IconData, ViewportBuilder};

fn main() -> eframe::Result {
    let started = std::time::Instant::now();
    let line = mh_files_core::cli::CommandLine::parse(std::env::args().skip(1));
    if line.version {
        println!("MH Files {}", env!("CARGO_PKG_VERSION"));
        return Ok(());
    }
    let data = storage::init(line.portable);
    let cwd = std::env::current_dir().unwrap_or_default();
    let (open, missing) = startup::resolve(&line.paths, &cwd);

    let (settings, notice) = storage::load_settings();
    // Одна копия: пути уходят в уже открытое окно, а этот процесс завершается.
    let repaint = std::sync::Arc::new(std::sync::OnceLock::new());
    let mut incoming = None;
    let mut keepalive: Option<Box<dyn std::any::Any>> = None;
    if settings.system.single_instance && !line.new_window {
        use mh_files_platform::instance::{Claim, claim};
        let args: Vec<String> = open.iter().map(|(p, _)| p.display().to_string()).collect();
        let (rx, handler) = startup::incoming_channel(repaint.clone());
        match claim(&instance_key(&data.config), &args, handler) {
            Claim::Forwarded => return Ok(()),
            Claim::First(server) => {
                incoming = Some(rx);
                keepalive = Some(Box::new(server));
            }
            Claim::Failed(_) => {}
        }
    }
    crash::install_hook(&data.config);
    let previous = crash::start(&data.config);
    let upgraded_from = storage::backup_on_upgrade();
    let session = storage::load_session();
    let labels = storage::load_labels();
    let startup = startup::Startup {
        started,
        open,
        missing,
        unknown_flags: line.unknown,
        previous,
        upgraded_from,
        incoming,
        repaint,
        keepalive,
        labels,
    };

    let mut viewport = ViewportBuilder::default()
        .with_title("MH Files")
        .with_app_id("mh-files")
        .with_inner_size([1280.0, 800.0])
        .with_min_inner_size([720.0, 420.0])
        .with_decorations(!settings.appearance.custom_title_bar)
        .with_icon(app_icon());
    if let Some(window) = session.as_ref().and_then(|s| s.window) {
        // Окно на отключённом мониторе не восстанавливается — откроется по центру.
        let visible =
            mh_files_platform::window::point_on_screen(window.x as i32 + 60, window.y as i32 + 20);
        if visible && window.width >= 400.0 && window.height >= 300.0 {
            viewport = viewport
                .with_position([window.x, window.y])
                .with_inner_size([window.width, window.height]);
        }
        viewport = viewport.with_maximized(window.maximized);
    }
    #[cfg_attr(not(windows), allow(unused_mut))]
    let mut options =
        eframe::NativeOptions { viewport, persist_window: false, ..Default::default() };
    #[cfg(windows)]
    {
        // Ctrl+V с файлами в буфере и Shift+Delete egui забирает себе — их видит перехватчик.
        options.event_loop_builder = Some(Box::new(|builder| {
            use winit::platform::windows::EventLoopBuilderExtWindows;
            builder.with_msg_hook(mh_files_platform::window::message_hook);
        }));
    }
    eframe::run_native(
        "MH Files",
        options,
        Box::new(move |cc| {
            Ok(Box::new(app::FilesApp::new(cc, settings, session, notice, startup)))
        }),
    )
}

/// Имя единственной копии зависит от папки данных: переносная копия на флешке и
/// установленная — разные программы со своими настройками, пути друг другу не передают.
/// FNV-1a: имя не должно меняться от сборки к сборке.
fn instance_key(data: &std::path::Path) -> String {
    let text = data.to_string_lossy().to_lowercase();
    let hash = text
        .bytes()
        .fold(0xcbf2_9ce4_8422_2325u64, |h, b| (h ^ b as u64).wrapping_mul(0x0100_0000_01b3));
    format!("MH-Files-{hash:016x}")
}

/// Сторона значков окон в пикселях.
const ICON_SIZE: usize = 64;
const ICON_BACKGROUND: [u8; 4] = [0x12, 0x16, 0x1D, 0xFF];
const ICON_ACCENT: [u8; 4] = [0x3F, 0xD0, 0xD8, 0xFF];

/// Значок окна рисуется кодом — та же папка со столбиками, что в `MH-Files.ico`
/// (`tools/make-icon.py`): фигуры [`icons::LOGO`], каждая следующая поверх предыдущих.
fn app_icon() -> IconData {
    icon_tile(|x, y| {
        let mut share = 0.0;
        for &(left, top, right, bottom, radius, value) in icons::LOGO {
            if in_rounded(x, y, (left, top, right, bottom), radius) {
                share = value;
            }
        }
        share
    })
}

/// Точка внутри скруглённого прямоугольника.
fn in_rounded(x: f32, y: f32, (left, top, right, bottom): (f32, f32, f32, f32), r: f32) -> bool {
    if !((left..right).contains(&x) && (top..bottom).contains(&y)) {
        return false;
    }
    let cx = x.clamp(left + r, right - r);
    let cy = y.clamp(top + r, bottom - r);
    (x - cx).powi(2) + (y - cy).powi(2) <= r * r
}

/// Значок окна настроек: на той же плитке — шестерёнка, чтобы на панели задач и в Alt+Tab
/// настройки не путались с главным окном.
pub(crate) fn settings_icon() -> std::sync::Arc<IconData> {
    static ICON: std::sync::OnceLock<std::sync::Arc<IconData>> = std::sync::OnceLock::new();
    ICON.get_or_init(|| {
        std::sync::Arc::new(icon_tile(|x, y| {
            let (dx, dy) = (x - 32.0, y - 32.0);
            let radius = dx.hypot(dy);
            // Восемь зубцов: доля оборота внутри своего сектора меньше половины — зубец.
            let turn = (dy.atan2(dx) / std::f32::consts::TAU * 8.0).rem_euclid(1.0);
            let outer = if (0.25..0.75).contains(&turn) { 25.0 } else { 19.0 };
            if (9.0..outer).contains(&radius) { 1.0 } else { 0.0 }
        }))
    })
    .clone()
}

/// Тёмная скруглённая плитка с рисунком цвета акцента: `shade(x, y)` — доля акцента в точке
/// (0 — фон плитки). Края сглажены: каждый пиксель — среднее 4×4 точек.
fn icon_tile(shade: impl Fn(f32, f32) -> f32) -> IconData {
    const SIZE: usize = ICON_SIZE;
    const SAMPLES: usize = 4;
    let mut rgba = vec![0u8; SIZE * SIZE * 4];
    let radius = 12.0f32;
    for y in 0..SIZE {
        for x in 0..SIZE {
            let (mut tile, mut mark) = (0usize, 0.0f32);
            for sy in 0..SAMPLES {
                for sx in 0..SAMPLES {
                    let fx = x as f32 + (sx as f32 + 0.5) / SAMPLES as f32;
                    let fy = y as f32 + (sy as f32 + 0.5) / SAMPLES as f32;
                    let cx = fx.clamp(radius, SIZE as f32 - radius);
                    let cy = fy.clamp(radius, SIZE as f32 - radius);
                    if (fx - cx).powi(2) + (fy - cy).powi(2) <= radius * radius {
                        tile += 1;
                        mark += shade(fx, fy);
                    }
                }
            }
            let pixel = &mut rgba[(y * SIZE + x) * 4..][..4];
            let total = (SAMPLES * SAMPLES) as f32;
            let share = mark / tile.max(1) as f32;
            for channel in 0..3 {
                let mixed = f32::from(ICON_BACKGROUND[channel]) * (1.0 - share)
                    + f32::from(ICON_ACCENT[channel]) * share;
                pixel[channel] = mixed.round() as u8;
            }
            pixel[3] = (tile as f32 / total * 255.0).round() as u8;
        }
    }
    IconData { rgba, width: SIZE as u32, height: SIZE as u32 }
}
