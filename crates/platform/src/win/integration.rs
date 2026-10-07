//! Встраивание в Проводник через `HKCU\Software\Classes`:
//! * пункт «Открыть в MH Files» — ключи `shell\MHFiles` для папок, пустого места окна и
//!   дисков. `%V` — путь папки (для пустого места — папки, в которой щёлкнули);
//! * действие по умолчанию для папок и дисков — значение по умолчанию `Directory\shell` и
//!   `Drive\shell` = `MHFiles`. Убирается, только если там по-прежнему наше;
//! * архивы — ProgID `MHFiles.Archive` и его имя в `.<расширение>\OpenWithProgids`.

use std::ffi::{OsStr, OsString};
use std::os::windows::ffi::OsStringExt;
use std::path::{Path, PathBuf};

use windows::Win32::Foundation::{ERROR_FILE_NOT_FOUND, ERROR_SUCCESS, WIN32_ERROR};
use windows::Win32::System::Registry::{
    HKEY_CURRENT_USER, REG_NONE, REG_SZ, REG_VALUE_TYPE, RRF_RT_ANY, RRF_RT_REG_EXPAND_SZ,
    RRF_RT_REG_SZ, RegDeleteKeyValueW, RegDeleteTreeW, RegGetValueW, RegSetKeyValueW,
};
use windows::Win32::UI::Shell::{SHCNE_ASSOCCHANGED, SHCNF_IDLIST, SHChangeNotify};
use windows::core::PCWSTR;

use super::com::wide;
use crate::integration::{ARCHIVE_EXTENSIONS, Feature, Status};

/// Ключи пункта: папки, пустое место окна, диски.
const KEYS: [&str; 3] = [
    r"Software\Classes\Directory\shell\MHFiles",
    r"Software\Classes\Directory\Background\shell\MHFiles",
    r"Software\Classes\Drive\shell\MHFiles",
];

/// Ключи, чьё значение по умолчанию — действие при двойном щелчке.
const DEFAULT_VERB_KEYS: [&str; 2] =
    [r"Software\Classes\Directory\shell", r"Software\Classes\Drive\shell"];

/// Имя нашего действия (подключ `shell\MHFiles`).
const VERB: &str = "MHFiles";

const TITLE: &str = "Открыть в MH Files";

const ARCHIVE_PROGID: &str = "MHFiles.Archive";
const ARCHIVE_KEY: &str = r"Software\Classes\MHFiles.Archive";

pub fn status() -> Status {
    let explorer_menu = read_string(&format!(r"{}\command", KEYS[0]), None)
        .and_then(|command| command_exe(&command))
        .is_some_and(|exe| exe.is_file());
    let default_folders = explorer_menu
        && DEFAULT_VERB_KEYS
            .iter()
            .all(|key| read_string(key, None).is_some_and(|verb| verb == VERB));
    let archives = read_string(&format!(r"{ARCHIVE_KEY}\shell\open\command"), None)
        .and_then(|command| command_exe(&command))
        .is_some_and(|exe| exe.is_file())
        && value_exists(
            &format!(r"Software\Classes\.{}\OpenWithProgids", ARCHIVE_EXTENSIONS[0]),
            ARCHIVE_PROGID,
        );
    Status { explorer_menu, default_folders, archives }
}

pub fn set(feature: Feature, exe: &Path, on: bool) -> Result<(), String> {
    let result = match (feature, on) {
        (Feature::ExplorerMenu, true) => install_menu(exe),
        (Feature::ExplorerMenu, false) => {
            // Действие по умолчанию без пункта указывало бы в пустоту.
            let default = clear_default_verb();
            let menu = delete_keys(&KEYS)
                .map_err(|error| format!("не удалось убрать пункт из меню Проводника: {error}"));
            default.and(menu)
        }
        (Feature::DefaultFolders, true) => install_menu(exe).and_then(|()| {
            DEFAULT_VERB_KEYS
                .iter()
                .try_for_each(|key| set_value(key, None, OsStr::new(VERB)))
                .map_err(|error| {
                    let _ = clear_default_verb();
                    format!("не удалось сделать MH Files программой для папок: {error}")
                })
        }),
        (Feature::DefaultFolders, false) => clear_default_verb(),
        (Feature::Archives, true) => install_archives(exe),
        (Feature::Archives, false) => uninstall_archives(),
    };
    notify();
    result
}

fn install_menu(exe: &Path) -> Result<(), String> {
    check_exe(exe)?;
    let icon = quoted(exe, "\",0");
    let command = quoted(exe, "\" \"%V\"");
    let written = KEYS.iter().try_for_each(|key| {
        set_value(key, None, OsStr::new(TITLE))?;
        set_value(key, Some("Icon"), &icon)?;
        set_value(&format!(r"{key}\command"), None, &command)
    });
    written.map_err(|error| {
        // Половина пункта хуже, чем ничего: убрать то, что успели записать.
        let _ = delete_keys(&KEYS);
        format!("не удалось добавить пункт в меню Проводника: {error}")
    })
}

/// Вернуть Проводнику двойной щелчок по папкам — только если там по-прежнему наше: чужое
/// (другой файловый менеджер, поставленный после) не трогаем.
fn clear_default_verb() -> Result<(), String> {
    let mut result = Ok(());
    for key in DEFAULT_VERB_KEYS {
        if read_string(key, None).is_some_and(|verb| verb == VERB)
            && let Err(error) = delete_value(key, None)
            && result.is_ok()
        {
            result = Err(format!("не удалось вернуть папки Проводнику: {error}"));
        }
    }
    result
}

fn install_archives(exe: &Path) -> Result<(), String> {
    check_exe(exe)?;
    let written = (|| {
        set_value(ARCHIVE_KEY, None, OsStr::new("Архив"))?;
        set_value(&format!(r"{ARCHIVE_KEY}\DefaultIcon"), None, &quoted(exe, "\",0"))?;
        set_value(
            &format!(r"{ARCHIVE_KEY}\shell\open"),
            Some("FriendlyAppName"),
            OsStr::new("MH Files"),
        )?;
        set_value(&format!(r"{ARCHIVE_KEY}\shell\open\command"), None, &quoted(exe, "\" \"%1\""))?;
        for ext in ARCHIVE_EXTENSIONS {
            set_empty(&format!(r"Software\Classes\.{ext}\OpenWithProgids"), ARCHIVE_PROGID)?;
        }
        Ok::<(), String>(())
    })();
    written.map_err(|error| {
        let _ = uninstall_archives();
        format!("не удалось связать архивы с MH Files: {error}")
    })
}

fn uninstall_archives() -> Result<(), String> {
    let mut result = delete_keys(&[ARCHIVE_KEY]);
    for ext in ARCHIVE_EXTENSIONS {
        let key = format!(r"Software\Classes\.{ext}\OpenWithProgids");
        if let Err(error) = delete_value(&key, Some(ARCHIVE_PROGID))
            && result.is_ok()
        {
            result = Err(error);
        }
    }
    result.map_err(|error| format!("не удалось отвязать архивы от MH Files: {error}"))
}

fn check_exe(exe: &Path) -> Result<(), String> {
    if exe.is_absolute() {
        Ok(())
    } else {
        Err(format!("путь к программе должен быть полным: {}", exe.display()))
    }
}

/// `"<exe><tail>`: кавычка открывается здесь, закрывает её `tail`.
fn quoted(exe: &Path, tail: &str) -> OsString {
    let mut value = OsString::from("\"");
    value.push(exe.as_os_str());
    value.push(tail);
    value
}

/// Удаляет ключи со всем содержимым; отсутствующий ключ — не ошибка. Ошибка — первая
/// встреченная.
fn delete_keys(keys: &[&str]) -> Result<(), String> {
    let mut result = Ok(());
    for key in keys {
        let subkey = wide(key);
        // SAFETY: имя ключа живёт до конца вызова.
        let status = unsafe { RegDeleteTreeW(HKEY_CURRENT_USER, PCWSTR(subkey.as_ptr())) };
        if status != ERROR_SUCCESS && status != ERROR_FILE_NOT_FOUND && result.is_ok() {
            result = Err(win32_text(status));
        }
    }
    result
}

/// Удаляет значение; отсутствующее — не ошибка. `None` — значение по умолчанию.
fn delete_value(key: &str, name: Option<&str>) -> Result<(), String> {
    let subkey = wide(key);
    let name = name.map(wide);
    // SAFETY: строки живут до конца вызова.
    let status = unsafe {
        RegDeleteKeyValueW(
            HKEY_CURRENT_USER,
            PCWSTR(subkey.as_ptr()),
            name.as_ref().map_or(PCWSTR::null(), |name| PCWSTR(name.as_ptr())),
        )
    };
    if status == ERROR_SUCCESS || status == ERROR_FILE_NOT_FOUND {
        Ok(())
    } else {
        Err(win32_text(status))
    }
}

/// Записывает строку `REG_SZ`, создавая ключ при необходимости. `None` — значение по умолчанию.
fn set_value(key: &str, name: Option<&str>, value: &OsStr) -> Result<(), String> {
    let data = wide(value);
    write(key, name, REG_SZ, Some(&data))
}

/// Пустое значение `REG_NONE` — так записываются `OpenWithProgids`.
fn set_empty(key: &str, name: &str) -> Result<(), String> {
    write(key, Some(name), REG_NONE, None)
}

fn write(
    key: &str,
    name: Option<&str>,
    kind: REG_VALUE_TYPE,
    data: Option<&[u16]>,
) -> Result<(), String> {
    let subkey = wide(key);
    let name = name.map(wide);
    // SAFETY: строки живут до конца вызова; размер данных — в байтах, с завершающим нулём.
    let status = unsafe {
        RegSetKeyValueW(
            HKEY_CURRENT_USER,
            PCWSTR(subkey.as_ptr()),
            name.as_ref().map_or(PCWSTR::null(), |name| PCWSTR(name.as_ptr())),
            kind.0,
            data.map(|data| data.as_ptr() as *const _),
            data.map_or(0, |data| (data.len() * 2) as u32),
        )
    };
    if status == ERROR_SUCCESS { Ok(()) } else { Err(win32_text(status)) }
}

/// Есть ли значение `name` любого типа в `HKCU\<key>`.
fn value_exists(key: &str, name: &str) -> bool {
    let subkey = wide(key);
    let name = wide(name);
    // SAFETY: строки живут до конца вызова; без буфера функция только проверяет значение.
    let status = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            PCWSTR(subkey.as_ptr()),
            PCWSTR(name.as_ptr()),
            RRF_RT_ANY,
            None,
            None,
            None,
        )
    };
    status == ERROR_SUCCESS
}

/// Строковое значение ключа `HKCU\<key>` (`None` — по умолчанию); `None`, если ключа или
/// значения нет.
fn read_string(key: &str, name: Option<&str>) -> Option<OsString> {
    let subkey = wide(key);
    let name = name.map(wide);
    let name = name.as_ref().map_or(PCWSTR::null(), |name| PCWSTR(name.as_ptr()));
    let flags = RRF_RT_REG_SZ | RRF_RT_REG_EXPAND_SZ;
    let mut bytes = 0u32;
    // SAFETY: строки живут до конца вызова; без буфера функция только сообщает нужный размер.
    let status = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            PCWSTR(subkey.as_ptr()),
            name,
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
            name,
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
