//! Действия Shell: `ShellExecuteExW`, «Открыть с помощью», свойства, терминал, Проводник.

use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use windows::Win32::Foundation::ERROR_CANCELLED;
use windows::Win32::System::Com::{
    CLSCTX_INPROC_SERVER, CoCreateInstance, IDataObject, IPersistFile,
};
use windows::Win32::UI::Shell::Common::ITEMIDLIST;
use windows::Win32::UI::Shell::{
    BHID_DataObject, ILCreateFromPathW, ILFree, IShellItemArray, IShellLinkW,
    OAIF_ALLOW_REGISTRATION, OAIF_EXEC, OAIF_REGISTER_EXT, OPENASINFO, SEE_MASK_INVOKEIDLIST,
    SEE_MASK_NOASYNC, SHCreateShellItemArrayFromIDLists, SHELLEXECUTEINFOW, SHMultiFileProperties,
    SHOpenFolderAndSelectItems, SHOpenWithDialog, ShellExecuteExW, ShellLink,
};
use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
use windows::core::{HRESULT, Interface, PCWSTR, w};

use super::com::{Apartment, describe, owner_hwnd, wide};

/// Процесс без окна консоли: cmd нужен только как посредник.
const CREATE_NO_WINDOW: u32 = 0x0800_0000;
/// Своя консоль для PowerShell, даже если у нас самих консоль есть (отладочная сборка).
const CREATE_NEW_CONSOLE: u32 = 0x0000_0010;

pub fn open(path: &Path) -> Result<(), String> {
    let _com = Apartment::sta();
    let file = wide(path);
    let dir = path.parent().map(wide);
    let mut info = SHELLEXECUTEINFOW {
        cbSize: size_of::<SHELLEXECUTEINFOW>() as u32,
        // Без SEE_MASK_FLAG_NO_UI: для файла без программы Windows сама предложит выбрать её.
        fMask: SEE_MASK_NOASYNC,
        hwnd: owner_hwnd().unwrap_or_default(),
        lpFile: PCWSTR(file.as_ptr()),
        lpDirectory: dir.as_ref().map_or(PCWSTR::null(), |dir| PCWSTR(dir.as_ptr())),
        nShow: SW_SHOWNORMAL.0,
        ..Default::default()
    };
    // SAFETY: все строки живут до конца вызова.
    match unsafe { ShellExecuteExW(&mut info) } {
        Ok(()) => Ok(()),
        Err(error) if is_cancelled(error.code()) => Ok(()),
        Err(error) => Err(describe(&format!("не удалось открыть {}", path.display()), &error)),
    }
}

pub fn open_with(path: &Path) -> Result<(), String> {
    let _com = Apartment::sta();
    let file = wide(path);
    let info = OPENASINFO {
        pcszFile: PCWSTR(file.as_ptr()),
        pcszClass: PCWSTR::null(),
        oaifInFlags: OAIF_ALLOW_REGISTRATION | OAIF_REGISTER_EXT | OAIF_EXEC,
    };
    // SAFETY: строка пути живёт до конца вызова; диалог модальный.
    match unsafe { SHOpenWithDialog(owner_hwnd(), &info) } {
        Ok(()) => Ok(()),
        Err(error) if is_cancelled(error.code()) => Ok(()),
        Err(error) => Err(describe("диалог «Открыть с помощью» не открылся", &error)),
    }
}

pub fn properties(paths: &[PathBuf]) -> Result<(), String> {
    let _com = Apartment::sta();
    match paths {
        [] => Ok(()),
        [path] => single_properties(path),
        _ => multi_properties(paths),
    }
}

fn single_properties(path: &Path) -> Result<(), String> {
    let file = wide(path);
    let mut info = SHELLEXECUTEINFOW {
        cbSize: size_of::<SHELLEXECUTEINFOW>() as u32,
        fMask: SEE_MASK_INVOKEIDLIST | SEE_MASK_NOASYNC,
        hwnd: owner_hwnd().unwrap_or_default(),
        lpVerb: w!("properties"),
        lpFile: PCWSTR(file.as_ptr()),
        nShow: SW_SHOWNORMAL.0,
        ..Default::default()
    };
    // SAFETY: строки живут до конца вызова.
    unsafe { ShellExecuteExW(&mut info) }
        .map_err(|error| describe(&format!("нет свойств для {}", path.display()), &error))
}

/// Одно окно свойств на несколько объектов, как при выделении в Проводнике.
fn multi_properties(paths: &[PathBuf]) -> Result<(), String> {
    let pidls = Pidls::new(paths)?;
    // SAFETY: PIDL живут, пока жив `pidls`; массив и объект данных копируют то, что им нужно.
    unsafe {
        let items: IShellItemArray = SHCreateShellItemArrayFromIDLists(&pidls.as_const())
            .map_err(|error| describe("не удалось собрать список объектов", &error))?;
        let data: IDataObject = items
            .BindToHandler(None, &BHID_DataObject)
            .map_err(|error| describe("не удалось собрать список объектов", &error))?;
        SHMultiFileProperties(&data, 0)
            .map_err(|error| describe("окно свойств не открылось", &error))
    }
}

pub fn open_terminal(dir: &Path, command: &str) -> Result<(), String> {
    if !command.trim().is_empty() {
        let command = command.replace("{dir}", &dir.to_string_lossy());
        // `start ""` даёт консольной программе своё окно, а сам cmd остаётся невидимым.
        // Командная строка передаётся как есть: кавычки в ней расставил пользователь.
        return Command::new("cmd")
            .arg("/C")
            .raw_arg(format!("start \"\" {command}"))
            .current_dir(dir)
            .creation_flags(CREATE_NO_WINDOW)
            .spawn()
            .map(|_| ())
            .map_err(|error| format!("команда терминала не запущена: {error}"));
    }
    if Command::new("wt.exe").arg("-d").arg(dir).spawn().is_ok() {
        return Ok(());
    }
    Command::new("powershell.exe")
        .arg("-NoExit")
        .current_dir(dir)
        .creation_flags(CREATE_NEW_CONSOLE)
        .spawn()
        .map(|_| ())
        .map_err(|error| format!("не запущены ни Windows Terminal, ни PowerShell: {error}"))
}

pub fn reveal_in_explorer(path: &Path) -> Result<(), String> {
    let _com = Apartment::sta();
    if let Ok(pidls) = Pidls::new(std::slice::from_ref(&path.to_path_buf())) {
        // SAFETY: PIDL жив до конца вызова. Полный PIDL без списка — «открыть родителя и
        // выделить этот объект».
        if unsafe { SHOpenFolderAndSelectItems(pidls.as_const()[0], None, 0) }.is_ok() {
            return Ok(());
        }
    }
    Command::new("explorer.exe")
        .raw_arg(format!("/select,\"{}\"", path.display()))
        .spawn()
        .map(|_| ())
        .map_err(|error| format!("Проводник не запущен: {error}"))
}

/// Ярлык `.lnk` на `target` по пути `link`.
pub fn create_shortcut(target: &Path, link: &Path) -> Result<(), String> {
    let _com = Apartment::sta();
    let target_w = wide(target);
    let link_w = wide(link);
    let failed = |error: &windows::core::Error| {
        describe(&format!("ярлык {} не создан", link.display()), error)
    };
    // SAFETY: строки живут до конца вызовов; COM-объекты освобождаются при выходе.
    unsafe {
        let shell_link: IShellLinkW =
            CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER).map_err(|e| failed(&e))?;
        shell_link.SetPath(PCWSTR(target_w.as_ptr())).map_err(|e| failed(&e))?;
        if let Some(dir) = target.parent() {
            let dir_w = wide(dir);
            let _ = shell_link.SetWorkingDirectory(PCWSTR(dir_w.as_ptr()));
        }
        let file: IPersistFile = shell_link.cast().map_err(|e| failed(&e))?;
        file.Save(PCWSTR(link_w.as_ptr()), true).map_err(|e| failed(&e))
    }
}

/// Отмена пользователем — не ошибка.
fn is_cancelled(code: HRESULT) -> bool {
    code == HRESULT::from_win32(ERROR_CANCELLED.0)
}

/// PIDL путей; освобождаются при уничтожении.
pub(super) struct Pidls(Vec<*mut ITEMIDLIST>);

impl Pidls {
    pub(super) fn new(paths: &[PathBuf]) -> Result<Pidls, String> {
        let mut pidls = Pidls(Vec::with_capacity(paths.len()));
        for path in paths {
            let path_w = wide(path);
            // SAFETY: строка живёт до конца вызова; результат освобождается в Drop.
            let pidl = unsafe { ILCreateFromPathW(PCWSTR(path_w.as_ptr())) };
            if pidl.is_null() {
                return Err(format!("объект не найден: {}", path.display()));
            }
            pidls.0.push(pidl);
        }
        Ok(pidls)
    }

    pub(super) fn as_const(&self) -> Vec<*const ITEMIDLIST> {
        self.0.iter().map(|&pidl| pidl as *const _).collect()
    }
}

impl Drop for Pidls {
    fn drop(&mut self) {
        for &pidl in &self.0 {
            // SAFETY: PIDL получен от ILCreateFromPathW и больше не используется.
            unsafe { ILFree(Some(pidl)) };
        }
    }
}
