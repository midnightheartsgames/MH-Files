//! Пункт «Открыть в MH Files» в контекстном меню Проводника: ключи `shell\MHFiles` в
//! `HKCU\Software\Classes` для папок, пустого места окна и дисков. `%V` — путь папки (для
//! пустого места — папки, в которой щёлкнули).

use std::ffi::{OsStr, OsString};
use std::os::windows::ffi::OsStringExt;
use std::path::{Path, PathBuf};

use windows::Win32::Foundation::{ERROR_FILE_NOT_FOUND, ERROR_SUCCESS, WIN32_ERROR};
use windows::Win32::System::Registry::{
    HKEY_CURRENT_USER, REG_SZ, RRF_RT_REG_EXPAND_SZ, RRF_RT_REG_SZ, RegDeleteTreeW, RegGetValueW,
    RegSetKeyValueW,
};
use windows::Win32::UI::Shell::{SHCNE_ASSOCCHANGED, SHCNF_IDLIST, SHChangeNotify};
use windows::core::PCWSTR;

use super::com::wide;

/// Ключи пункта: папки, пустое место окна, диски.
const KEYS: [&str; 3] = [
    r"Software\Classes\Directory\shell\MHFiles",
    r"Software\Classes\Directory\Background\shell\MHFiles",
    r"Software\Classes\Drive\shell\MHFiles",
];

const TITLE: &str = "Открыть в MH Files";

pub fn explorer_menu_installed() -> bool {
    read_default(&format!(r"{}\command", KEYS[0]))
        .and_then(|command| command_exe(&command))
        .is_some_and(|exe| exe.is_file())
}

pub fn install_explorer_menu(exe: &Path) -> Result<(), String> {
    if !exe.is_absolute() {
        return Err(format!("путь к программе должен быть полным: {}", exe.display()));
    }
    let quoted = |tail: &str| {
        let mut value = OsString::from("\"");
        value.push(exe.as_os_str());
        value.push(tail);
        value
    };
    let icon = quoted("\",0");
    let command = quoted("\" \"%V\"");
    let written = KEYS.iter().try_for_each(|key| {
        set_value(key, None, OsStr::new(TITLE))?;
        set_value(key, Some("Icon"), &icon)?;
        set_value(&format!(r"{key}\command"), None, &command)
    });
    if let Err(error) = written {
        // Половина пункта хуже, чем ничего: убрать то, что успели записать.
        let _ = delete_keys();
        notify();
        return Err(format!("не удалось добавить пункт в меню Проводника: {error}"));
    }
    notify();
    Ok(())
}

pub fn uninstall_explorer_menu() -> Result<(), String> {
    let deleted = delete_keys();
    notify();
    deleted.map_err(|error| format!("не удалось убрать пункт из меню Проводника: {error}"))
}

/// Удаляет все ключи пункта; отсутствующий ключ — не ошибка. Ошибка — первая встреченная.
fn delete_keys() -> Result<(), String> {
    let mut result = Ok(());
    for key in KEYS {
        let subkey = wide(key);
        // SAFETY: имя ключа живёт до конца вызова.
        let status = unsafe { RegDeleteTreeW(HKEY_CURRENT_USER, PCWSTR(subkey.as_ptr())) };
        if status != ERROR_SUCCESS && status != ERROR_FILE_NOT_FOUND && result.is_ok() {
            result = Err(win32_text(status));
        }
    }
    result
}

/// Записывает строку `REG_SZ`, создавая ключ при необходимости. `None` — значение по умолчанию.
fn set_value(key: &str, name: Option<&str>, value: &OsStr) -> Result<(), String> {
    let subkey = wide(key);
    let name = name.map(wide);
    let data = wide(value);
    // SAFETY: строки живут до конца вызова; размер данных — в байтах, с завершающим нулём.
    let status = unsafe {
        RegSetKeyValueW(
            HKEY_CURRENT_USER,
            PCWSTR(subkey.as_ptr()),
            name.as_ref().map_or(PCWSTR::null(), |name| PCWSTR(name.as_ptr())),
            REG_SZ.0,
            Some(data.as_ptr() as *const _),
            (data.len() * 2) as u32,
        )
    };
    if status == ERROR_SUCCESS { Ok(()) } else { Err(win32_text(status)) }
}

/// Значение по умолчанию ключа `HKCU\<key>`; `None`, если ключа или значения нет.
fn read_default(key: &str) -> Option<OsString> {
    let subkey = wide(key);
    let flags = RRF_RT_REG_SZ | RRF_RT_REG_EXPAND_SZ;
    let mut bytes = 0u32;
    // SAFETY: имя живёт до конца вызова; без буфера функция только сообщает нужный размер.
    let status = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            PCWSTR(subkey.as_ptr()),
            PCWSTR::null(),
            flags,
            None,
            None,
            Some(&mut bytes),
        )
    };
    if status != ERROR_SUCCESS || bytes == 0 || bytes > 64 * 1024 {
        return None;
    }
    let mut buffer = vec![0u16; (bytes as usize).div_ceil(2)];
    let mut bytes = (buffer.len() * 2) as u32;
    // SAFETY: буфер живёт до конца вызова, `bytes` — его размер в байтах.
    let status = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            PCWSTR(subkey.as_ptr()),
            PCWSTR::null(),
            flags,
            None,
            Some(buffer.as_mut_ptr() as *mut _),
            Some(&mut bytes),
        )
    };
    if status != ERROR_SUCCESS {
        return None;
    }
    let len = ((bytes as usize) / 2).min(buffer.len());
    let text = &buffer[..len];
    let text = text.iter().position(|&unit| unit == 0).map_or(text, |end| &text[..end]);
    Some(OsString::from_wide(text))
}

/// Путь программы из строки команды: в кавычках или до первого пробела.
fn command_exe(command: &OsStr) -> Option<PathBuf> {
    let command = command.to_string_lossy();
    let command = command.trim_start();
    let exe = match command.strip_prefix('"') {
        Some(rest) => rest.split('"').next()?,
        None => command.split(' ').next()?,
    };
    (!exe.is_empty()).then(|| PathBuf::from(exe))
}

/// Проводник перечитывает ассоциации и меню.
fn notify() {
    // SAFETY: событие без путей; указатели не передаются.
    unsafe { SHChangeNotify(SHCNE_ASSOCCHANGED, SHCNF_IDLIST, None, None) };
}

fn win32_text(status: WIN32_ERROR) -> String {
    std::io::Error::from_raw_os_error(status.0 as i32).to_string()
}
