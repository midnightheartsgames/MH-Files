//! Контекстное меню Shell (`IContextMenu`) с пунктами сторонних расширений.
//!
//! Показывается в фоновом потоке: расширения грузятся и читают диск, UI ждать не должен.
//! Владелец меню — скрытое окно этого же потока: через него `IContextMenu2/3` получают
//! сообщения отрисовки, без которых подменю «Отправить», «Открыть с помощью» и значки
//! расширений остаются пустыми.

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::sync::Once;

use windows::Win32::Foundation::{
    ERROR_CANCELLED, HINSTANCE, HWND, LPARAM, LRESULT, POINT, WPARAM,
};
use windows::Win32::System::Com::IBindCtx;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Ole::{OleFlushClipboard, OleInitialize, OleUninitialize};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetAsyncKeyState, VIRTUAL_KEY, VK_CONTROL, VK_SHIFT,
};
use windows::Win32::UI::Shell::Common::ITEMIDLIST;
use windows::Win32::UI::Shell::{
    CMF_EXPLORE, CMF_EXTENDEDVERBS, CMF_NORMAL, CMIC_MASK_CONTROL_DOWN, CMIC_MASK_PTINVOKE,
    CMIC_MASK_SHIFT_DOWN, CMINVOKECOMMANDINFO, CMINVOKECOMMANDINFOEX, GCS_VERBW, IContextMenu,
    IContextMenu2, IContextMenu3, ILFindLastID, IShellFolder, SHBindToObject, SHBindToParent,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreatePopupMenu, CreateWindowExW, DefWindowProcW, DestroyMenu, DestroyWindow, GetCursorPos,
    HMENU, PostMessageW, RegisterClassW, SW_SHOWNORMAL, SetForegroundWindow, TPM_RETURNCMD,
    TPM_RIGHTBUTTON, TrackPopupMenuEx, WM_DRAWITEM, WM_INITMENUPOPUP, WM_MEASUREITEM, WM_MENUCHAR,
    WM_NULL, WNDCLASSW, WS_EX_TOOLWINDOW, WS_POPUP,
};
use windows::core::{HRESULT, Interface, PCSTR, PCWSTR, PSTR, w};

use super::com::{Apartment, describe, owner_hwnd, wide};
use super::shell::Pidls;
use crate::shell::MenuChoice;

/// Номера команд расширений. Ноль `TrackPopupMenuEx` возвращает, если ничего не выбрали.
const FIRST_COMMAND: u32 = 1;
const LAST_COMMAND: u32 = 0x7FFF;

// В windows 0.62 этих флагов нет; значения совпадают с SEE_MASK_UNICODE и SEE_MASK_NOASYNC.
const CMIC_MASK_UNICODE: u32 = 0x4000;
/// Поток меню завершится сразу после вызова — асинхронной команде некуда будет вернуться.
const CMIC_MASK_NOASYNC: u32 = 0x100;

const CLASS_NAME: PCWSTR = w!("MHFilesShellMenuOwner");

pub fn context_menu(paths: &[PathBuf]) -> Result<MenuChoice, String> {
    let _com = Apartment::sta();
    let _ole = Ole::init();
    let first = paths.first().ok_or("нет объектов для меню")?;
    // Одно меню умеет только объекты одной папки: остальные отбрасываются.
    let same: Vec<PathBuf> =
        paths.iter().filter(|path| path.parent() == first.parent()).cloned().collect();
    let pidls = Pidls::new(&same)?;
    let absolute = pidls.as_const();
    let window = OwnerWindow::new()?;
    // SAFETY: PIDL живут, пока жив `pidls`; относительные — указатели внутрь абсолютных.
    let menu: IContextMenu = unsafe {
        let folder: IShellFolder = SHBindToParent(absolute[0], None)
            .map_err(|error| describe("папка объектов недоступна", &error))?;
        let children: Vec<*const ITEMIDLIST> =
            absolute.iter().map(|&pidl| ILFindLastID(pidl).cast_const()).collect();
        folder
            .GetUIObjectOf(window.hwnd, &children, None)
            .map_err(|error| describe("меню объектов недоступно", &error))?
    };
    show(&menu, &window, None)
}

pub fn background_menu(dir: &Path) -> Result<MenuChoice, String> {
    let _com = Apartment::sta();
    let _ole = Ole::init();
    let pidls = Pidls::new(std::slice::from_ref(&dir.to_path_buf()))?;
    let window = OwnerWindow::new()?;
    // SAFETY: PIDL жив до конца вызова; папка хранит свою копию.
    let menu: IContextMenu = unsafe {
        let folder: IShellFolder =
            SHBindToObject(None::<&IShellFolder>, pidls.as_const()[0], None::<&IBindCtx>).map_err(
                |error| describe(&format!("папка {} недоступна", dir.display()), &error),
            )?;
        folder
            .CreateViewObject(window.hwnd)
            .map_err(|error| describe("меню папки недоступно", &error))?
    };
    let dir_w = wide(dir);
    show(&menu, &window, Some(PCWSTR(dir_w.as_ptr())))
}

fn show(
    menu: &IContextMenu,
    window: &OwnerWindow,
    directory: Option<PCWSTR>,
) -> Result<MenuChoice, String> {
    let popup = Popup::new()?;
    let shift = key_down(VK_SHIFT);
    let mut flags = CMF_NORMAL | CMF_EXPLORE;
    if shift {
        // Как в Проводнике: Shift добавляет «Копировать как путь», «Открыть окно PowerShell»…
        flags |= CMF_EXTENDEDVERBS;
    }
    // SAFETY: меню живо до конца функции; расширения добавляют в него свои пункты.
    unsafe { menu.QueryContextMenu(popup.0, 0, FIRST_COMMAND, LAST_COMMAND, flags) }
        .ok()
        .map_err(|error| describe("меню не собрано", &error))?;

    let _active = Active::new(menu);
    let mut cursor = POINT::default();
    // SAFETY: обычные вызовы для окна этого потока. Без SetForegroundWindow меню не закроется
    // щелчком мимо, а WM_NULL после него — известное исправление из KB135788.
    let command = unsafe {
        let _ = GetCursorPos(&mut cursor);
        let _ = SetForegroundWindow(window.hwnd);
        let command = TrackPopupMenuEx(
            popup.0,
            (TPM_RETURNCMD | TPM_RIGHTBUTTON).0,
            cursor.x,
            cursor.y,
            window.hwnd,
            None,
        );
        let _ = PostMessageW(Some(window.hwnd), WM_NULL, WPARAM(0), LPARAM(0));
        command.0
    };
    let Some(offset) = u32::try_from(command).ok().and_then(|id| id.checked_sub(FIRST_COMMAND))
    else {
        return Ok(MenuChoice::Dismissed);
    };
    let offset = offset as usize;
    let verb = verb_of(menu, offset);
    // Переименованию Shell нужно своё представление папки; у MH Files — своё поле ввода.
    if verb.as_deref() == Some("rename") {
        return Ok(MenuChoice::Invoked { verb });
    }

    let mut mask = CMIC_MASK_UNICODE | CMIC_MASK_PTINVOKE | CMIC_MASK_NOASYNC;
    if shift {
        mask |= CMIC_MASK_SHIFT_DOWN;
    }
    if key_down(VK_CONTROL) {
        mask |= CMIC_MASK_CONTROL_DOWN;
    }
    let info = CMINVOKECOMMANDINFOEX {
        cbSize: size_of::<CMINVOKECOMMANDINFOEX>() as u32,
        fMask: mask,
        hwnd: owner_hwnd().unwrap_or(window.hwnd),
        // Команда по номеру (MAKEINTRESOURCE), а не по имени: имя есть не у всех пунктов.
        lpVerb: PCSTR(std::ptr::without_provenance(offset)),
        lpVerbW: PCWSTR(std::ptr::without_provenance(offset)),
        lpDirectoryW: directory.unwrap_or(PCWSTR::null()),
        nShow: SW_SHOWNORMAL.0,
        ptInvoke: cursor,
        ..Default::default()
    };
    // SAFETY: структура EX начинается с полей CMINVOKECOMMANDINFO, cbSize говорит о
    // расширенной версии; всё живёт до конца вызова.
    let invoked =
        unsafe { menu.InvokeCommand(std::ptr::from_ref(&info).cast::<CMINVOKECOMMANDINFO>()) };
    match invoked {
        Ok(()) => {}
        // «Отмена» в диалоге самой команды — не ошибка.
        Err(error) if error.code() == HRESULT::from_win32(ERROR_CANCELLED.0) => {}
        Err(error) => return Err(describe("команда меню не выполнена", &error)),
    }
    if matches!(verb.as_deref(), Some("cut" | "copy")) {
        // Shell кладёт в буфер свой объект данных, а поток меню сейчас завершится: без этого
        // содержимое буфера пропало бы вместе с ним.
        // SAFETY: обычный вызов в потоке с инициализированным OLE.
        let _ = unsafe { OleFlushClipboard() };
    }
    Ok(MenuChoice::Invoked { verb })
}

/// Имя команды для Shell («delete», «cut», «7-Zip.Extract»…), если расширение его сообщает.
fn verb_of(menu: &IContextMenu, offset: usize) -> Option<String> {
    let mut buffer = [0u16; 256];
    // SAFETY: GCS_VERBW пишет UTF-16 не длиннее переданного числа символов.
    unsafe {
        menu.GetCommandString(
            offset,
            GCS_VERBW,
            None,
            PSTR(buffer.as_mut_ptr().cast()),
            buffer.len() as u32,
        )
    }
    .ok()?;
    let len = buffer.iter().position(|&unit| unit == 0).unwrap_or(buffer.len());
    let verb = String::from_utf16_lossy(&buffer[..len]);
    (!verb.is_empty()).then_some(verb)
}

/// Состояние клавиши прямо сейчас. `GetKeyState` здесь не годится: он отражает ввод своего
/// потока, а клавиатуру получает поток UI.
fn key_down(key: VIRTUAL_KEY) -> bool {
    // SAFETY: простой запрос состояния клавиши.
    unsafe { GetAsyncKeyState(i32::from(key.0)) < 0 }
}

/// OLE в потоке меню: без него команды «Копировать» и «Вырезать» не положат объекты в буфер.
struct Ole {
    initialized: bool,
}

impl Ole {
    fn init() -> Ole {
        // SAFETY: парный вызов — в Drop; в потоке MTA просто не получится, меню работает и так.
        Ole { initialized: unsafe { OleInitialize(None) }.is_ok() }
    }
}

impl Drop for Ole {
    fn drop(&mut self) {
        if self.initialized {
            // SAFETY: парный вызов к успешному OleInitialize в этом же потоке.
            unsafe { OleUninitialize() };
        }
    }
}

struct Popup(HMENU);

impl Popup {
    fn new() -> Result<Popup, String> {
        // SAFETY: обычный вызов; меню уничтожается в Drop.
        unsafe { CreatePopupMenu() }.map(Popup).map_err(|error| describe("меню не создано", &error))
    }
}

impl Drop for Popup {
    fn drop(&mut self) {
        // SAFETY: меню создано нами и больше не используется.
        let _ = unsafe { DestroyMenu(self.0) };
    }
}

/// Скрытое окно-владелец меню в текущем потоке.
struct OwnerWindow {
    hwnd: HWND,
}

impl OwnerWindow {
    fn new() -> Result<OwnerWindow, String> {
        // SAFETY: модуль процесса живёт всё время работы.
        let instance: HINSTANCE = unsafe { GetModuleHandleW(None) }
            .map_err(|error| describe("окно меню не создано", &error))?
            .into();
        static REGISTER: Once = Once::new();
        REGISTER.call_once(|| {
            let class = WNDCLASSW {
                lpfnWndProc: Some(window_proc),
                hInstance: instance,
                lpszClassName: CLASS_NAME,
                ..Default::default()
            };
            // SAFETY: имя класса — статическая строка. Неудачу покажет CreateWindowExW.
            unsafe { RegisterClassW(&class) };
        });
        let mut cursor = POINT::default();
        // SAFETY: обычные вызовы; окно уничтожается в Drop. Владелец — главное окно, чтобы
        // меню и диалоги команд не уходили за него.
        let hwnd = unsafe {
            let _ = GetCursorPos(&mut cursor);
            CreateWindowExW(
                WS_EX_TOOLWINDOW,
                CLASS_NAME,
                PCWSTR::null(),
                WS_POPUP,
                cursor.x,
                cursor.y,
                0,
                0,
                owner_hwnd(),
                None,
                Some(instance),
                None,
            )
        }
        .map_err(|error| describe("окно меню не создано", &error))?;
        Ok(OwnerWindow { hwnd })
    }
}

impl Drop for OwnerWindow {
    fn drop(&mut self) {
        // SAFETY: окно создано в этом потоке и больше не используется.
        let _ = unsafe { DestroyWindow(self.hwnd) };
    }
}

/// Расширенные интерфейсы показанного меню — их ждёт оконная процедура.
#[derive(Clone)]
struct Handlers {
    menu3: Option<IContextMenu3>,
    menu2: Option<IContextMenu2>,
}

thread_local! {
    static ACTIVE: RefCell<Option<Handlers>> = const { RefCell::new(None) };
}

/// Меню, показанное сейчас в этом потоке; снимается при уничтожении.
struct Active;

impl Active {
    fn new(menu: &IContextMenu) -> Active {
        let handlers = Handlers { menu3: menu.cast().ok(), menu2: menu.cast().ok() };
        ACTIVE.with_borrow_mut(|active| *active = Some(handlers));
        Active
    }
}

impl Drop for Active {
    fn drop(&mut self) {
        ACTIVE.with_borrow_mut(|active| *active = None);
    }
}

unsafe extern "system" fn window_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    if matches!(message, WM_INITMENUPOPUP | WM_DRAWITEM | WM_MEASUREITEM | WM_MENUCHAR)
        && let Some(result) = forward(message, wparam, lparam)
    {
        return result;
    }
    // SAFETY: стандартная обработка остальных сообщений.
    unsafe { DefWindowProcW(hwnd, message, wparam, lparam) }
}

/// Передаёт сообщение меню расширению. `None` — расширение его не обработало.
fn forward(message: u32, wparam: WPARAM, lparam: LPARAM) -> Option<LRESULT> {
    // Копия (AddRef), а не заимствование: расширение может прислать сообщение повторно.
    let handlers = ACTIVE.with_borrow(Clone::clone)?;
    // SAFETY: параметры сообщения передаются как есть; интерфейсы живы, пока живы копии.
    unsafe {
        if let Some(menu3) = &handlers.menu3 {
            let mut result = LRESULT(0);
            return menu3
                .HandleMenuMsg2(message, wparam, lparam, Some(&mut result))
                .ok()
                .map(|()| result);
        }
        if message != WM_MENUCHAR
            && let Some(menu2) = &handlers.menu2
        {
            menu2.HandleMenuMsg(message, wparam, lparam).ok()?;
            // WM_DRAWITEM и WM_MEASUREITEM ждут TRUE, WM_INITMENUPOPUP — ноль.
            let handled = matches!(message, WM_DRAWITEM | WM_MEASUREITEM);
            return Some(LRESULT(isize::from(handled)));
        }
    }
    None
}
