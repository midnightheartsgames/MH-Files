//! Мониторы.

use windows::Win32::Foundation::POINT;
use windows::Win32::Graphics::Gdi::{MONITOR_DEFAULTTONULL, MonitorFromPoint};

pub fn point_on_screen(x: i32, y: i32) -> bool {
    // SAFETY: функция только читает раскладку мониторов.
    let monitor = unsafe { MonitorFromPoint(POINT { x, y }, MONITOR_DEFAULTTONULL) };
    !monitor.is_invalid()
}

/// Перехватчик сообщений цикла winit: замечает сочетания, которые egui забирает себе раньше
/// программы, и ничего не съедает.
pub fn message_hook(msg: *const std::ffi::c_void) -> bool {
    use windows::Win32::UI::Input::KeyboardAndMouse::{
        GetKeyState, VIRTUAL_KEY, VK_CONTROL, VK_DELETE, VK_INSERT, VK_MENU, VK_SHIFT, VK_V,
    };
    use windows::Win32::UI::WindowsAndMessaging::{MSG, WM_KEYDOWN};

    if msg.is_null() {
        return false;
    }
    // SAFETY: winit передаёт указатель на MSG, живой на время вызова.
    let msg = unsafe { &*(msg as *const MSG) };
    if msg.message != WM_KEYDOWN {
        return false;
    }
    // SAFETY: GetKeyState только читает состояние клавиатуры этого потока.
    let down = |key: VIRTUAL_KEY| unsafe { GetKeyState(key.0 as i32) } < 0;
    let key = VIRTUAL_KEY(msg.wParam.0 as u16);
    let (ctrl, shift, alt) = (down(VK_CONTROL), down(VK_SHIFT), down(VK_MENU));
    let shortcut = match key {
        VK_V if ctrl && !shift && !alt => Some(crate::window::Intercepted::Paste),
        VK_INSERT if shift && !ctrl && !alt => Some(crate::window::Intercepted::Paste),
        VK_DELETE if shift && !ctrl && !alt => Some(crate::window::Intercepted::DeletePermanent),
        _ => None,
    };
    if let Some(shortcut) = shortcut {
        crate::window::note(shortcut);
    }
    false
}
