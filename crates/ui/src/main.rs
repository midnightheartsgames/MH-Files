//! MH Files — файловый менеджер для Windows (PLAN.md).

// В отладочной сборке консоль остаётся: в неё пишут паники.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod actions;
mod app;
mod batch;
mod commands;
mod dialogs;
mod icons;
mod images;
mod inspector;
mod palette;
mod pane_view;
mod preview_ui;
mod quick;
mod settings_window;
mod sidebar;
mod statusbar;
mod storage;
mod tabs;
mod theme;
mod widgets;

use eframe::egui::{IconData, ViewportBuilder};

fn main() -> eframe::Result {
    let (settings, notice) = storage::load_settings();
    let session = storage::load_session();

    let mut viewport = ViewportBuilder::default()
        .with_title("MH Files")
        .with_app_id("mh-files")
        .with_inner_size([1280.0, 800.0])
        .with_min_inner_size([720.0, 420.0])
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
    let options = eframe::NativeOptions { viewport, persist_window: false, ..Default::default() };
    eframe::run_native(
        "MH Files",
        options,
        Box::new(move |cc| Ok(Box::new(app::FilesApp::new(cc, settings, session, notice)))),
    )
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
