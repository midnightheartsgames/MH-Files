//! Перетаскивание файлов наружу: `SHDoDragDrop` с объектом данных Shell — получатель видит
//! то же, что при перетаскивании из Проводника, а курсор несёт картинку объектов.

use std::path::PathBuf;

use windows::Win32::Foundation::{
    DRAGDROP_S_CANCEL, DRAGDROP_S_DROP, DRAGDROP_S_USEDEFAULTCURSORS, S_OK,
};
use windows::Win32::System::Com::IDataObject;
use windows::Win32::System::Ole::{
    DROPEFFECT, DROPEFFECT_COPY, DROPEFFECT_LINK, DROPEFFECT_MOVE, IDropSource, IDropSource_Impl,
};
use windows::Win32::System::SystemServices::{MK_LBUTTON, MK_RBUTTON, MODIFIERKEYS_FLAGS};
use windows::Win32::UI::Shell::{
    BHID_DataObject, IShellItemArray, SHCreateShellItemArrayFromIDLists, SHDoDragDrop,
};
use windows::core::{BOOL, HRESULT, implement};

use super::com::{describe, owner_hwnd};
use super::shell::Pidls;
use crate::dnd::DropEffect;

pub fn drag_out(paths: &[PathBuf], right: bool) -> Result<DropEffect, String> {
    if paths.is_empty() {
        return Err("нечего перетаскивать".into());
    }
    // OLE в потоке UI уже инициализировал winit; свой апартамент здесь не нужен.
    let pidls = Pidls::new(paths)?;
    // SAFETY: PIDL живут до конца вызова; массив и объект данных хранят свои копии.
    let data: IDataObject = unsafe {
        let items: IShellItemArray = SHCreateShellItemArrayFromIDLists(&pidls.as_const())
            .map_err(|error| describe("не удалось собрать список объектов", &error))?;
        items
            .BindToHandler(None, &BHID_DataObject)
            .map_err(|error| describe("не удалось собрать список объектов", &error))?
    };
    let button = if right { MK_RBUTTON } else { MK_LBUTTON };
    let source: IDropSource =
        DropSource { button, other: if right { MK_LBUTTON } else { MK_RBUTTON } }.into();
    // SAFETY: вызов в потоке, получающем ввод мыши; сам крутит цикл сообщений до отпускания.
    let effect = unsafe {
        SHDoDragDrop(
            owner_hwnd(),
            &data,
            &source,
            DROPEFFECT_COPY | DROPEFFECT_MOVE | DROPEFFECT_LINK,
        )
    }
    .map_err(|error| describe("перетаскивание не удалось", &error))?;
    Ok(drop_effect(effect))
}

/// Получатель мог сделать «оптимизированное» перемещение и вернуть NONE при уже унесённых
/// файлах — не страшно: список всё равно перечитывается.
fn drop_effect(effect: DROPEFFECT) -> DropEffect {
    if effect.0 & DROPEFFECT_MOVE.0 != 0 {
        DropEffect::Move
    } else if effect.0 & DROPEFFECT_COPY.0 != 0 {
        DropEffect::Copy
    } else if effect.0 & DROPEFFECT_LINK.0 != 0 {
        DropEffect::Link
    } else {
        DropEffect::None
    }
}

/// Источник перетаскивания: отпустили свою кнопку — бросить; Esc или вторая кнопка —
/// отменить, как в Проводнике.
#[implement(IDropSource)]
struct DropSource {
    button: MODIFIERKEYS_FLAGS,
    other: MODIFIERKEYS_FLAGS,
}

impl IDropSource_Impl for DropSource_Impl {
    fn QueryContinueDrag(&self, escape: BOOL, keys: MODIFIERKEYS_FLAGS) -> HRESULT {
        if escape.as_bool() || keys.0 & self.other.0 != 0 {
            DRAGDROP_S_CANCEL
        } else if keys.0 & self.button.0 == 0 {
            DRAGDROP_S_DROP
        } else {
            S_OK
        }
    }

    fn GiveFeedback(&self, _effect: DROPEFFECT) -> HRESULT {
        DRAGDROP_S_USEDEFAULTCURSORS
    }
}
