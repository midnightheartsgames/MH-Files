//! Известные папки через `SHGetKnownFolderPath`: учитывает перенос папок пользователем
//! (например, «Документы» на другом диске) и OneDrive.

use std::ffi::OsString;
use std::os::windows::ffi::OsStringExt;
use std::path::PathBuf;

use windows::Win32::System::Com::CoTaskMemFree;
use windows::Win32::UI::Shell::{
    FOLDERID_Desktop, FOLDERID_Documents, FOLDERID_Downloads, FOLDERID_Music, FOLDERID_Pictures,
    FOLDERID_Profile, FOLDERID_Videos, KF_FLAG_DEFAULT, SHGetKnownFolderPath,
};

use crate::folders::KnownFolder;

pub fn known_folder(folder: KnownFolder) -> Option<PathBuf> {
    let id = match folder {
        KnownFolder::Home => FOLDERID_Profile,
        KnownFolder::Desktop => FOLDERID_Desktop,
        KnownFolder::Documents => FOLDERID_Documents,
        KnownFolder::Downloads => FOLDERID_Downloads,
        KnownFolder::Pictures => FOLDERID_Pictures,
        KnownFolder::Videos => FOLDERID_Videos,
        KnownFolder::Music => FOLDERID_Music,
    };
    // SAFETY: строку выделяет Shell, освобождаем её через CoTaskMemFree сразу после копирования.
    unsafe {
        let raw = SHGetKnownFolderPath(&id, KF_FLAG_DEFAULT, None).ok()?;
        let path = OsString::from_wide(raw.as_wide());
        CoTaskMemFree(Some(raw.0 as *const _));
        (!path.is_empty()).then(|| PathBuf::from(path))
    }
}
