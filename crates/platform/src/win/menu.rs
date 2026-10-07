//! Контекстное меню Shell (`IContextMenu`) с пунктами сторонних расширений.
//!
//! Показывается в фоновом потоке: расширения грузятся и читают диск, UI ждать не должен.
//! Владелец меню — скрытое окно этого же потока: через него `IContextMenu2/3` получают
//! сообщения отрисовки, без которых подменю «Отправить», «Открыть с помощью» и значки
//! расширений остаются пустыми.

use std::cell::RefCell;
use std::mem::ManuallyDrop;
use std::path::{Path, PathBuf};
use std::sync::Once;
use std::time::Duration;

use crossbeam_channel::{Receiver, RecvTimeoutError};

use windows::Win32::Foundation::{
    ERROR_CANCELLED, HINSTANCE, HWND, LPARAM, LRESULT, POINT, WPARAM,
};
use windows::Win32::Graphics::Gdi::{
    BI_RGB, BITMAP, BITMAPINFO, BITMAPINFOHEADER, CreateCompatibleDC, DIB_RGB_COLORS, DeleteDC,
    GetDIBits, GetObjectW, HBITMAP, HGDIOBJ,
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
    CMIC_MASK_SHIFT_DOWN, CMINVOKECOMMANDINFO, CMINVOKECOMMANDINFOEX, DEFCONTEXTMENU, GCS_VERBW,
    IContextMenu, IContextMenu2, IContextMenu3, ILFindLastID, IShellFolder, SHBindToObject,
    SHBindToParent, SHCreateDefaultContextMenu,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreatePopupMenu, CreateWindowExW, DefWindowProcW, DestroyMenu, DestroyWindow, DispatchMessageW,
    GetCursorPos, GetMenuItemCount, GetMenuItemInfoW, HMENU, MENUITEMINFOW, MFS_DISABLED,
    MFT_OWNERDRAW, MFT_SEPARATOR, MIIM_BITMAP, MIIM_CHECKMARKS, MIIM_FTYPE, MIIM_ID, MIIM_STATE,
    MIIM_STRING, MIIM_SUBMENU, MSG, PM_REMOVE, PeekMessageW, PostMessageW, RegisterClassW,
    SW_SHOWNORMAL, SetForegroundWindow, TPM_RETURNCMD, TPM_RIGHTBUTTON, TrackPopupMenuEx,
    TranslateMessage, WM_DRAWITEM, WM_INITMENUPOPUP, WM_MEASUREITEM, WM_MENUCHAR, WM_NULL,
    WNDCLASSW, WS_EX_TOOLWINDOW, WS_POPUP,
};
use windows::core::{HRESULT, Interface, PCSTR, PCWSTR, PSTR, PWSTR, w};

use super::com::{Apartment, describe, owner_hwnd, wide};
use super::shell::Pidls;
use crate::shell::{MenuChoice, MenuGate, MenuIcon, MenuTarget, ShellMenuItem, menu_label};

/// Номера команд расширений. Ноль `TrackPopupMenuEx` возвращает, если ничего не выбрали.
const FIRST_COMMAND: u32 = 1;
const LAST_COMMAND: u32 = 0x7FFF;

// В windows 0.62 этих флагов нет; значения совпадают с SEE_MASK_UNICODE и SEE_MASK_NOASYNC.
const CMIC_MASK_UNICODE: u32 = 0x4000;
/// Поток меню завершится сразу после вызова — асинхронной команде некуда будет вернуться.
const CMIC_MASK_NOASYNC: u32 = 0x100;

const CLASS_NAME: PCWSTR = w!("MHFilesShellMenuOwner");

/// `extensions` — с пунктами сторонних расширений (как в Проводнике); без них меню собирает
/// сама Windows из своих команд — так оно не зависнет на чужой библиотеке.
pub fn context_menu(
    paths: &[PathBuf],
    gate: &MenuGate,
    extensions: bool,
) -> Result<MenuChoice, String> {
    let _com = Apartment::sta();
    let _ole = Ole::init();
    let window = OwnerWindow::new()?;
    let (menu, _pidls) = object_menu(paths, window.hwnd, extensions)?;
    show(&menu, &window, None, gate)
}

/// Меню объектов. Одно меню умеет только объекты одной папки: остальные отбрасываются.
/// PIDL возвращаются вместе с меню: пусть живут, пока живо оно.
fn object_menu(
    paths: &[PathBuf],
    hwnd: HWND,
    extensions: bool,
) -> Result<(IContextMenu, Pidls), String> {
    let first = paths.first().ok_or("нет объектов для меню")?;
    let same: Vec<PathBuf> =
        paths.iter().filter(|path| path.parent() == first.parent()).cloned().collect();
    let pidls = Pidls::new(&same)?;
    let absolute = pidls.as_const();
    // SAFETY: PIDL живут, пока жив `pidls`; относительные — указатели внутрь абсолютных.
    let menu: IContextMenu = unsafe {
        let folder: IShellFolder = SHBindToParent(absolute[0], None)
            .map_err(|error| describe("папка объектов недоступна", &error))?;
        let children: Vec<*const ITEMIDLIST> =
            absolute.iter().map(|&pidl| ILFindLastID(pidl).cast_const()).collect();
        if extensions {
            folder
                .GetUIObjectOf(hwnd, &children, None)
                .map_err(|error| describe("меню объектов недоступно", &error))?
        } else {
            let parent = first.parent().ok_or("у объекта нет папки")?;
            let parent = Pidls::new(&[parent.to_path_buf()])?;
            let mut children: Vec<*mut ITEMIDLIST> =
                children.iter().map(|&pidl| pidl.cast_mut()).collect();
            // Без ключей реестра (`aKeys`) в меню только встроенные команды Windows.
            let mut info = DEFCONTEXTMENU {
                hwnd,
                pidlFolder: parent.as_const()[0].cast_mut(),
                psf: ManuallyDrop::new(Some(folder)),
                cidl: children.len() as u32,
                apidl: children.as_mut_ptr(),
                ..Default::default()
            };
            let menu = SHCreateDefaultContextMenu(&info);
            ManuallyDrop::drop(&mut info.psf);
            menu.map_err(|error| describe("меню объектов недоступно", &error))?
        }
    };
    Ok((menu, pidls))
}

pub fn background_menu(dir: &Path, gate: &MenuGate) -> Result<MenuChoice, String> {
    let _com = Apartment::sta();
    let _ole = Ole::init();
    let window = OwnerWindow::new()?;
    let menu = folder_menu(dir, window.hwnd)?;
    let dir_w = wide(dir);
    show(&menu, &window, Some(PCWSTR(dir_w.as_ptr())), gate)
}

/// Меню пустого места папки.
fn folder_menu(dir: &Path, hwnd: HWND) -> Result<IContextMenu, String> {
    let pidls = Pidls::new(std::slice::from_ref(&dir.to_path_buf()))?;
    // SAFETY: PIDL жив до конца вызова; папка хранит свою копию.
    unsafe {
        let folder: IShellFolder =
            SHBindToObject(None::<&IShellFolder>, pidls.as_const()[0], None::<&IBindCtx>).map_err(
                |error| describe(&format!("папка {} недоступна", dir.display()), &error),
            )?;
        folder.CreateViewObject(hwnd).map_err(|error| describe("меню папки недоступно", &error))
    }
}

/// См. [`crate::shell::live_menu`].
pub fn live_menu(
    target: &MenuTarget,
    commands: &Receiver<u32>,
    ready: impl FnOnce(Result<Vec<ShellMenuItem>, String>),
) -> Result<MenuChoice, String> {
    let _com = Apartment::sta();
    let _ole = Ole::init();
    // Shift в момент щелчка — как в Проводнике: расширенные команды.
    let shift = key_down(VK_SHIFT);
    let built = (|| {
        let window = OwnerWindow::new()?;
        let (menu, pidls) = match target {
            MenuTarget::Items(paths) => {
                let (menu, pidls) = object_menu(paths, window.hwnd, true)?;
                (menu, Some(pidls))
            }
            MenuTarget::Background(dir) => (folder_menu(dir, window.hwnd)?, None),
        };
        let popup = Popup::new()?;
        query(&menu, &popup, shift)?;
        let items = read_menu(&menu, popup.0, 0);
        Ok::<_, String>((window, menu, pidls, popup, items))
    })();
    let (window, menu, _pidls, _popup, items) = match built {
        Ok(built) => built,
        Err(error) => {
            ready(Err(error));
            return Ok(MenuChoice::Dismissed);
        }
    };
    ready(Ok(items));
    let Some(id) = wait_command(commands) else {
        return Ok(MenuChoice::Dismissed);
    };
    let Some(offset) = id.checked_sub(FIRST_COMMAND) else {
        return Ok(MenuChoice::Dismissed);
    };
    let directory = match target {
        MenuTarget::Background(dir) => Some(wide(dir)),
        MenuTarget::Items(_) => None,
    };
    let mut cursor = POINT::default();
    // SAFETY: простой запрос положения курсора.
    let _ = unsafe { GetCursorPos(&mut cursor) };
    let directory = directory.as_ref().map(|dir| PCWSTR(dir.as_ptr()));
    let choice = invoke(&menu, &window, offset as usize, directory, shift, cursor);
    // Меню — раньше своего окна-владельца и PIDL.
    drop(menu);
    choice
}

/// Номер выбранной команды; `None` — канал закрыт (меню закрыли). Пока ждёт, обслуживает
/// сообщения окон потока: расширениям, которые что-то себе посылают, есть кому ответить.
fn wait_command(commands: &Receiver<u32>) -> Option<u32> {
    loop {
        let mut msg = MSG::default();
        // SAFETY: MSG живёт до конца вызовов; окна этого потока обслуживает этот же поток.
        unsafe {
            while PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() {
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }
        match commands.recv_timeout(Duration::from_millis(30)) {
            Ok(id) => return Some(id),
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => return None,
        }
    }
}

/// Сколько уровней подменю читать: «Отправить ▸» — второй, глубже почти не бывает.
const MAX_DEPTH: usize = 3;

/// Пункты меню Win32 одного уровня. Подменю сначала получают `WM_INITMENUPOPUP` — так их
/// заполняют расширения («Отправить», «Создать», «Открыть с помощью»), как при показе.
fn read_menu(menu: &IContextMenu, hmenu: HMENU, depth: usize) -> Vec<ShellMenuItem> {
    // SAFETY: меню создано этим потоком и живо до конца функции.
    let count = unsafe { GetMenuItemCount(Some(hmenu)) }.max(0) as u32;
    let mut items = Vec::with_capacity(count as usize);
    for index in 0..count {
        let mut text = [0u16; 512];
        let mut info = MENUITEMINFOW {
            cbSize: size_of::<MENUITEMINFOW>() as u32,
            fMask: MIIM_FTYPE
                | MIIM_STATE
                | MIIM_ID
                | MIIM_SUBMENU
                | MIIM_STRING
                | MIIM_BITMAP
                | MIIM_CHECKMARKS,
            dwTypeData: PWSTR(text.as_mut_ptr()),
            cch: text.len() as u32 - 1,
            ..Default::default()
        };
        // SAFETY: буфер текста живёт до конца вызова, его размер — в `cch`.
        if unsafe { GetMenuItemInfoW(hmenu, index, true, &mut info) }.is_err() {
            continue;
        }
        if info.fType.contains(MFT_SEPARATOR) {
            items.push(ShellMenuItem::Separator);
            continue;
        }
        // У нарисованных расширением пунктов (MFT_OWNERDRAW) текста может не быть — такие
        // пропускаются: показать их в своём меню нечем.
        let len = (info.cch as usize).min(text.len());
        let raw = if info.fType.contains(MFT_OWNERDRAW) {
            String::new()
        } else {
            String::from_utf16_lossy(&text[..len])
        };
        let label = menu_label(&raw);
        if label.is_empty() {
            continue;
        }
        let enabled = info.fState.0 & MFS_DISABLED.0 == 0;
        let icon = menu_bitmap(info.hbmpItem).or_else(|| menu_bitmap(info.hbmpUnchecked));
        if !info.hSubMenu.is_invalid() {
            if depth + 1 >= MAX_DEPTH {
                continue;
            }
            init_popup(menu, info.hSubMenu, index);
            let children = read_menu(menu, info.hSubMenu, depth + 1);
            items.push(ShellMenuItem::Submenu { label, icon, enabled, items: children });
            continue;
        }
        if !(FIRST_COMMAND..=LAST_COMMAND).contains(&info.wID) {
            continue;
        }
        let verb = verb_of(menu, (info.wID - FIRST_COMMAND) as usize);
        items.push(ShellMenuItem::Command { id: info.wID, label, verb, icon, enabled });
    }
    items
}

/// Подменю вот-вот покажут: расширение заполняет его по `WM_INITMENUPOPUP`.
fn init_popup(menu: &IContextMenu, submenu: HMENU, index: u32) {
    let wparam = WPARAM(submenu.0 as usize);
    let lparam = LPARAM(index as isize);
    // SAFETY: параметры — те, что Windows шлёт при открытии подменю.
    unsafe {
        if let Ok(menu3) = menu.cast::<IContextMenu3>() {
            let mut result = LRESULT(0);
            let _ = menu3.HandleMenuMsg2(WM_INITMENUPOPUP, wparam, lparam, Some(&mut result));
        } else if let Ok(menu2) = menu.cast::<IContextMenu2>() {
            let _ = menu2.HandleMenuMsg(WM_INITMENUPOPUP, wparam, lparam);
        }
    }
}

/// Картинка пункта меню (32-бит, premultiplied alpha). Особые значения (`HBMMENU_CALLBACK` —
/// значок рисует само расширение, системные кнопки окна) и пустые — `None`.
fn menu_bitmap(bitmap: HBITMAP) -> Option<MenuIcon> {
    // HBMMENU_CALLBACK = -1, системные HBMMENU_* — от 1 до 11.
    if (-1..=11).contains(&(bitmap.0 as isize)) {
        return None;
    }
    let mut header = BITMAP::default();
    // SAFETY: GetObjectW пишет не больше переданного размера.
    let size = unsafe {
        GetObjectW(
            HGDIOBJ(bitmap.0),
            size_of::<BITMAP>() as i32,
            Some(std::ptr::from_mut(&mut header).cast()),
        )
    };
    let (width, height) = (header.bmWidth, header.bmHeight.abs());
    if size == 0 || !(1..=256).contains(&width) || !(1..=256).contains(&height) {
        return None;
    }
    let mut info = BITMAPINFO {
        bmiHeader: BITMAPINFOHEADER {
            biSize: size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: width,
            // Отрицательная высота — строки сверху вниз.
            biHeight: -height,
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB.0,
            ..Default::default()
        },
        ..Default::default()
    };
    let mut pixels = vec![0u8; width as usize * height as usize * 4];
    // SAFETY: буфер вмещает width×height пикселей по 4 байта; DC свой и удаляется сразу.
    let lines = unsafe {
        let dc = CreateCompatibleDC(None);
        let lines = GetDIBits(
            dc,
            bitmap,
            0,
            height as u32,
            Some(pixels.as_mut_ptr().cast()),
            &mut info,
            DIB_RGB_COLORS,
        );
        let _ = DeleteDC(dc);
        lines
    };
    if lines == 0 {
        return None;
    }
    // Картинка без прозрачности (24 бита или нулевой альфа-канал) — непрозрачная.
    let opaque = header.bmBitsPixel < 32 || pixels.as_chunks::<4>().0.iter().all(|px| px[3] == 0);
    for px in pixels.as_chunks_mut::<4>().0 {
        px.swap(0, 2);
        if opaque {
            px[3] = 255;
        } else {
            // Premultiplied: цвет не ярче альфы. Иначе картинка в обычной альфе — умножить.
            let alpha = px[3];
            if px[..3].iter().any(|&c| c > alpha) {
                for c in &mut px[..3] {
                    *c = (u16::from(*c) * u16::from(alpha) / 255) as u8;
                }
            }
        }
    }
    Some(MenuIcon { width: width as u32, height: height as u32, rgba: pixels })
}

fn show(
    menu: &IContextMenu,
    window: &OwnerWindow,
    directory: Option<PCWSTR>,
    gate: &MenuGate,
) -> Result<MenuChoice, String> {
    let popup = Popup::new()?;
    let shift = key_down(VK_SHIFT);
    query(menu, &popup, shift)?;
    // Пока расширения собирали меню, его могли перестать ждать (сторож в fs::shell).
    if !gate.try_show() {
        return Ok(MenuChoice::Dismissed);
    }

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
    invoke(menu, window, offset as usize, directory, shift, cursor)
}

/// Расширения добавляют в `popup` свои пункты. `shift` — как в Проводнике: Shift добавляет
/// «Копировать как путь», «Открыть окно PowerShell»…
fn query(menu: &IContextMenu, popup: &Popup, shift: bool) -> Result<(), String> {
    let mut flags = CMF_NORMAL | CMF_EXPLORE;
    if shift {
        flags |= CMF_EXTENDEDVERBS;
    }
    // SAFETY: меню живо до конца вызова; расширения добавляют в него свои пункты.
    unsafe { menu.QueryContextMenu(popup.0, 0, FIRST_COMMAND, LAST_COMMAND, flags) }
        .ok()
        .map_err(|error| describe("меню не собрано", &error))
}

/// Выполнить команду меню с номером `offset` (от `FIRST_COMMAND`).
fn invoke(
    menu: &IContextMenu,
    window: &OwnerWindow,
    offset: usize,
    directory: Option<PCWSTR>,
    shift: bool,
    cursor: POINT,
) -> Result<MenuChoice, String> {
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
