//! Корзина: `SHQueryRecycleBinW`, `SHEmptyRecycleBinW`, окно `shell:RecycleBinFolder`.

use windows::Win32::Foundation::{E_UNEXPECTED, ERROR_CANCELLED};
use windows::Win32::UI::Shell::{
    SEE_MASK_NOASYNC, SHELLEXECUTEINFOW, SHEmptyRecycleBinW, SHQUERYRBINFO, SHQueryRecycleBinW,
    ShellExecuteExW,
};
use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
use windows::core::{HRESULT, PCWSTR, w};

use super::com::{Apartment, describe, owner_hwnd};
use crate::recycle::BinInfo;

pub fn query() -> Option<BinInfo> {
    let mut info =
        SHQUERYRBINFO { cbSize: size_of::<SHQUERYRBINFO>() as u32, ..Default::default() };
    // SAFETY: пустой путь — все диски; структура живёт до конца вызова.
    unsafe { SHQueryRecycleBinW(PCWSTR::null(), &mut info) }.ok()?;
    Some(BinInfo { items: info.i64NumItems.max(0) as u64, bytes: info.i64Size.max(0) as u64 })
}

pub fn bin_dirs() -> Vec<std::path::PathBuf> {
    use crate::drives::DriveKind;
    let Some(sid) = user_sid() else { return Vec::new() };
    crate::drives::drive_roots()
        .into_iter()
        .filter(|(_, kind)| matches!(kind, DriveKind::Fixed | DriveKind::Removable))
        .map(|(root, _)| root.join("$Recycle.Bin").join(&sid))
        .filter(|dir| dir.is_dir())
        .collect()
}

/// SID пользователя процесса строкой (`S-1-5-21-…`): так называется его папка в корзине.
fn user_sid() -> Option<String> {
    use windows::Win32::Foundation::{CloseHandle, HANDLE, HLOCAL, LocalFree};
    use windows::Win32::Security::Authorization::ConvertSidToStringSidW;
    use windows::Win32::Security::{GetTokenInformation, TOKEN_QUERY, TOKEN_USER, TokenUser};
    use windows::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};
    use windows::core::PWSTR;

    // SAFETY: маркер закрывается в конце; буфер живёт, пока читается SID из него; строку SID
    // выделяет система — она освобождается LocalFree.
    unsafe {
        let mut token = HANDLE::default();
        OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token).ok()?;
        let mut needed = 0u32;
        let _ = GetTokenInformation(token, TokenUser, None, 0, &mut needed);
        let mut buffer = vec![0u64; (needed as usize).div_ceil(8)];
        let read = GetTokenInformation(
            token,
            TokenUser,
            Some(buffer.as_mut_ptr().cast()),
            needed,
            &mut needed,
        );
        let _ = CloseHandle(token);
        read.ok()?;
        let user = &*(buffer.as_ptr() as *const TOKEN_USER);
        let mut text = PWSTR::null();
        ConvertSidToStringSidW(user.User.Sid, &mut text).ok()?;
        let sid = text.to_string().ok();
        let _ = LocalFree(Some(HLOCAL(text.0.cast())));
        sid
    }
}

pub fn open() -> Result<(), String> {
    let _com = Apartment::sta();
    let mut info = SHELLEXECUTEINFOW {
        cbSize: size_of::<SHELLEXECUTEINFOW>() as u32,
        fMask: SEE_MASK_NOASYNC,
        hwnd: owner_hwnd().unwrap_or_default(),
        lpFile: w!("shell:RecycleBinFolder"),
        nShow: SW_SHOWNORMAL.0,
        ..Default::default()
    };
    // SAFETY: строка статическая, структура живёт до конца вызова.
    unsafe { ShellExecuteExW(&mut info) }.map_err(|e| describe("корзина не открылась", &e))
}

pub fn empty() -> Result<(), String> {
    let _com = Apartment::sta();
    // SAFETY: пустой путь — все диски; флаги 0 — Windows сама спросит и покажет ход.
    match unsafe { SHEmptyRecycleBinW(owner_hwnd(), PCWSTR::null(), 0) } {
        Ok(()) => Ok(()),
        // Отказались в подтверждении или корзина уже пуста.
        Err(error)
            if error.code() == HRESULT::from_win32(ERROR_CANCELLED.0)
                || error.code() == E_UNEXPECTED =>
        {
            Ok(())
        }
        Err(error) => Err(describe("корзина не очищена", &error)),
    }
}
