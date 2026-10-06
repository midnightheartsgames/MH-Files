//! Мониторы.

use windows::Win32::Foundation::POINT;
use windows::Win32::Graphics::Gdi::{MONITOR_DEFAULTTONULL, MonitorFromPoint};

pub fn point_on_screen(x: i32, y: i32) -> bool {
    // SAFETY: функция только читает раскладку мониторов.
    let monitor = unsafe { MonitorFromPoint(POINT { x, y }, MONITOR_DEFAULTTONULL) };
    !monitor.is_invalid()
}
