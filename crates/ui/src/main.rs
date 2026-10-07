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
mod preview_ui;
mod quick;
mod settings_window;
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

/// Значок окна рисуется кодом: три столбика акцента, как у MH Sidebar, на тёмной плитке.
fn app_icon() -> IconData {
    const SIZE: usize = 64;
    let mut rgba = vec![0u8; SIZE * SIZE * 4];
    let background = [0x12, 0x16, 0x1D, 0xFF];
    let accent = [0x3F, 0xD0, 0xD8, 0xFF];
    let radius = 12.0f32;
    for y in 0..SIZE {
        for x in 0..SIZE {
            let (fx, fy) = (x as f32 + 0.5, y as f32 + 0.5);
            let cx = fx.clamp(radius, SIZE as f32 - radius);
            let cy = fy.clamp(radius, SIZE as f32 - radius);
            let inside = (fx - cx).powi(2) + (fy - cy).powi(2) <= radius * radius;
            if inside {
                rgba[(y * SIZE + x) * 4..][..4].copy_from_slice(&background);
            }
        }
    }
    for (i, height) in [22usize, 40, 30].into_iter().enumerate() {
        let left = 15 + i * 13;
        for y in (52 - height)..52 {
            for x in left..left + 9 {
                rgba[(y * SIZE + x) * 4..][..4].copy_from_slice(&accent);
            }
        }
    }
    IconData { rgba, width: SIZE as u32, height: SIZE as u32 }
}
