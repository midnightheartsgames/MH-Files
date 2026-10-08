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
