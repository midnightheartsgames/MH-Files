//! Мониторы, перехват сочетаний, Snap Layouts для своего заголовка окна.

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

pub fn bring_to_front() {
    use windows::Win32::UI::WindowsAndMessaging::{
        IsIconic, SW_RESTORE, SetForegroundWindow, ShowWindowAsync,
    };

    let Some(hwnd) = super::com::owner_hwnd() else { return };
    // SAFETY: HWND главного окна; если окно уже закрыто, функции просто вернут ошибку.
    // ShowWindowAsync только ставит команду в очередь окна и не ждёт его поток.
    unsafe {
        if IsIconic(hwnd).as_bool() {
            let _ = ShowWindowAsync(hwnd, SW_RESTORE);
        }
        let _ = SetForegroundWindow(hwnd);
    }
}

// ── Snap Layouts для своего заголовка ────────────────────────────────────────────────────
//
// Окно без системной рамки (winit сам даёт ему тень DWM и убирает неклиентскую область).
// Чтобы Windows 11 показала Snap Layouts, окно должно ответить на WM_NCHITTEST кодом
// HTMAXBUTTON там, где нарисована кнопка «Развернуть». Тогда мышь над ней — уже неклиентская:
// egui её не видит, поэтому наведение и щелчок обрабатываются здесь.

use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::Graphics::Dwm::{
    DWMWA_WINDOW_CORNER_PREFERENCE, DWMWCP_ROUND, DwmSetWindowAttribute,
};
use windows::Win32::Graphics::Gdi::{InvalidateRect, ScreenToClient};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    TME_LEAVE, TME_NONCLIENT, TRACKMOUSEEVENT, TrackMouseEvent,
};
use windows::Win32::UI::Shell::{DefSubclassProc, SetWindowSubclass};
use windows::Win32::UI::WindowsAndMessaging::{
    HTCLIENT, HTMAXBUTTON, IsZoomed, SW_MAXIMIZE, SW_RESTORE, ShowWindow, WM_MOUSEMOVE,
    WM_NCHITTEST, WM_NCLBUTTONDBLCLK, WM_NCLBUTTONDOWN, WM_NCLBUTTONUP, WM_NCMOUSELEAVE,
    WM_NCMOUSEMOVE,
};

static BUTTON: Mutex<Option<[i32; 4]>> = Mutex::new(None);
static HOVERED: AtomicBool = AtomicBool::new(false);
static PRESSED: AtomicBool = AtomicBool::new(false);
static SUBCLASSED: AtomicBool = AtomicBool::new(false);
const SUBCLASS_ID: usize = 0x4D48_4649; // «MHFI»

pub fn set_maximize_button(rect: Option<[i32; 4]>) {
    if let Ok(mut button) = BUTTON.lock() {
        *button = rect;
    }
    if rect.is_none() || SUBCLASSED.load(Ordering::Relaxed) {
        return;
    }
    let Some(hwnd) = super::com::owner_hwnd() else { return };
    // SAFETY: вызывается в потоке окна; обработчик — функция без состояния, данные — в статиках.
    // Скруглённые углы Windows 11 окну без рамки нужно попросить; в Windows 10 атрибута нет.
    unsafe {
        if SetWindowSubclass(hwnd, Some(subclass), SUBCLASS_ID, 0).as_bool() {
            SUBCLASSED.store(true, Ordering::Relaxed);
        }
        let corners = DWMWCP_ROUND;
        let _ = DwmSetWindowAttribute(
            hwnd,
            DWMWA_WINDOW_CORNER_PREFERENCE,
            &corners as *const _ as *const _,
            size_of_val(&corners) as u32,
        );
    }
}

pub fn maximize_button_hovered() -> bool {
    HOVERED.load(Ordering::Relaxed)
}

fn set_hovered(hwnd: HWND, hovered: bool) {
    if HOVERED.swap(hovered, Ordering::Relaxed) != hovered {
        // SAFETY: окно этого потока; WM_PAINT заставит eframe перерисовать заголовок.
        let _ = unsafe { InvalidateRect(Some(hwnd), None, false) };
    }
}

/// Точка экрана из LPARAM — над кнопкой?
fn over_button(hwnd: HWND, lparam: LPARAM) -> bool {
    let Some([left, top, right, bottom]) = BUTTON.lock().ok().and_then(|button| *button) else {
        return false;
    };
    let mut point = windows::Win32::Foundation::POINT {
        x: (lparam.0 & 0xFFFF) as i16 as i32,
        y: ((lparam.0 >> 16) & 0xFFFF) as i16 as i32,
    };
    // SAFETY: окно этого потока, точка — локальная переменная.
    if !unsafe { ScreenToClient(hwnd, &mut point) }.as_bool() {
        return false;
    }
    (left..right).contains(&point.x) && (top..bottom).contains(&point.y)
}

unsafe extern "system" fn subclass(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
    _id: usize,
    _data: usize,
) -> LRESULT {
    let at_button = wparam.0 as u32 == HTMAXBUTTON;
    match msg {
        WM_NCHITTEST => {
            // SAFETY: обычная передача сообщения следующему обработчику.
            let hit = unsafe { DefSubclassProc(hwnd, msg, wparam, lparam) };
            if hit.0 as u32 == HTCLIENT && over_button(hwnd, lparam) {
                return LRESULT(HTMAXBUTTON as isize);
            }
            return hit;
        }
        WM_NCMOUSEMOVE if at_button => {
            if !HOVERED.load(Ordering::Relaxed) {
                let mut track = TRACKMOUSEEVENT {
                    cbSize: size_of::<TRACKMOUSEEVENT>() as u32,
                    dwFlags: TME_LEAVE | TME_NONCLIENT,
                    hwndTrack: hwnd,
                    dwHoverTime: 0,
                };
                // SAFETY: структура живёт до конца вызова.
                let _ = unsafe { TrackMouseEvent(&mut track) };
            }
            set_hovered(hwnd, true);
            return LRESULT(0);
        }
        WM_NCMOUSEMOVE | WM_NCMOUSELEAVE | WM_MOUSEMOVE => {
            set_hovered(hwnd, false);
            if msg != WM_MOUSEMOVE {
                PRESSED.store(false, Ordering::Relaxed);
            }
        }
        WM_NCLBUTTONDOWN | WM_NCLBUTTONDBLCLK if at_button => {
            // Не отдавать системе: она нарисовала бы старую кнопку Windows 95 поверх нашей.
            PRESSED.store(true, Ordering::Relaxed);
            return LRESULT(0);
        }
        WM_NCLBUTTONUP if at_button => {
            if PRESSED.swap(false, Ordering::Relaxed) {
                // SAFETY: окно этого потока.
                unsafe {
                    let show = if IsZoomed(hwnd).as_bool() { SW_RESTORE } else { SW_MAXIMIZE };
                    let _ = ShowWindow(hwnd, show);
                }
            }
            return LRESULT(0);
        }
        _ => {}
    }
    // SAFETY: обычная передача сообщения следующему обработчику.
    unsafe { DefSubclassProc(hwnd, msg, wparam, lparam) }
}
