//! Файловые операции через `IFileOperation`: системные корзина, прогресс, конфликты имён,
//! запрос прав администратора и «Отменить» в Проводнике.

use std::path::{Path, PathBuf};

use windows::Win32::Foundation::ERROR_CANCELLED;
use windows::Win32::Storage::FileSystem::FILE_ATTRIBUTE_DIRECTORY;
use windows::Win32::System::Com::{CLSCTX_ALL, CoCreateInstance};
use windows::Win32::UI::Shell::{
    COPYENGINE_E_USER_CANCELLED, FILEOPERATION_FLAGS, FOF_ALLOWUNDO, FOF_NOCONFIRMATION,
    FOF_NOCONFIRMMKDIR, FOF_RENAMEONCOLLISION, FOF_WANTNUKEWARNING, FOFX_ADDUNDORECORD,
    FOFX_EARLYFAILURE, FOFX_RECYCLEONDELETE, FileOperation, IFileOperation, IShellItem,
    IShellItemArray, SHCreateItemFromParsingName, SHCreateShellItemArrayFromIDLists,
};
use windows::core::{HRESULT, PCWSTR};

use super::com::{describe, owner_hwnd, wide};
use super::shell::Pidls;
use crate::ops::{FileOp, OpOutcome};

pub fn execute(op: &FileOp) -> Result<OpOutcome, String> {
    // SAFETY: вызывается в STA-потоке операций; все COM-объекты живут до конца функции.
    let operation: IFileOperation =
        unsafe { CoCreateInstance(&FileOperation, None, CLSCTX_ALL) }
            .map_err(|error| describe("не удалось запустить файловую операцию", &error))?;
    if let Some(owner) = owner_hwnd() {
        // SAFETY: HWND главного окна; при ошибке диалоги просто будут без владельца.
        let _ = unsafe { operation.SetOwnerWindow(owner) };
    }

    let base = FOF_ALLOWUNDO | FOF_NOCONFIRMMKDIR | FOFX_ADDUNDORECORD;
    let mut created = Vec::new();
    // SAFETY: строки и PIDL живут до конца вызовов; IFileOperation копирует то, что ему нужно.
    unsafe {
        match op {
            FileOp::Copy { sources, dest } => {
                let mut flags = base;
                // Копия в ту же папку — «имя - копия», а не диалог конфликта с самим собой.
                if sources.iter().all(|source| same_dir(source.parent(), dest)) {
                    flags |= FOF_RENAMEONCOLLISION;
                }
                set_flags(&operation, flags)?;
                let items = item_array(sources)?;
                operation
                    .CopyItems(&items, &item(dest)?)
                    .map_err(|error| describe("копирование не подготовлено", &error))?;
            }
            FileOp::Move { sources, dest } => {
                set_flags(&operation, base)?;
                let items = item_array(sources)?;
                operation
                    .MoveItems(&items, &item(dest)?)
                    .map_err(|error| describe("перемещение не подготовлено", &error))?;
            }
            FileOp::Delete { paths, permanent: false } => {
                // Как Проводник в Windows 10+: в корзину без вопроса. Но если объект в корзину не
                // помещается, NOCONFIRMATION молча удалил бы его насовсем — WANTNUKEWARNING
                // возвращает для этого случая системный вопрос.
                set_flags(
                    &operation,
                    FOF_ALLOWUNDO | FOF_NOCONFIRMATION | FOF_WANTNUKEWARNING | FOFX_RECYCLEONDELETE,
                )?;
                let items = item_array(paths)?;
                operation
                    .DeleteItems(&items)
                    .map_err(|error| describe("удаление не подготовлено", &error))?;
            }
            FileOp::Delete { paths, permanent: true } => {
                // Точный список путей пользователь уже подтвердил в нашем окне.
                set_flags(&operation, FOF_NOCONFIRMATION | FOFX_EARLYFAILURE)?;
                let items = item_array(paths)?;
                operation
                    .DeleteItems(&items)
                    .map_err(|error| describe("удаление не подготовлено", &error))?;
            }
            FileOp::Rename { path, new_name } => {
                set_flags(&operation, base)?;
                let name = wide(new_name);
                operation
                    .RenameItem(&item(path)?, PCWSTR(name.as_ptr()), None)
                    .map_err(|error| describe("переименование не подготовлено", &error))?;
                created.push(path.with_file_name(new_name));
            }
            FileOp::NewFolder { parent, name } => {
                // Без FOF_RENAMEONCOLLISION: свободное имя выбирает интерфейс, и путь известен.
                set_flags(&operation, base)?;
                let name_w = wide(name);
                operation
                    .NewItem(
                        &item(parent)?,
                        FILE_ATTRIBUTE_DIRECTORY.0,
                        PCWSTR(name_w.as_ptr()),
                        PCWSTR::null(),
                        None,
                    )
                    .map_err(|error| describe("создание папки не подготовлено", &error))?;
                created.push(parent.join(name));
            }
        }

        if let Err(error) = operation.PerformOperations() {
            if is_cancelled(error.code()) {
                return Ok(OpOutcome::Aborted);
            }
            return Err(describe(&op.describe(), &error));
        }
        if operation.GetAnyOperationsAborted().is_ok_and(|aborted| aborted.as_bool()) {
            return Ok(OpOutcome::Aborted);
        }
    }
    Ok(OpOutcome::Done { created })
}

unsafe fn set_flags(operation: &IFileOperation, flags: FILEOPERATION_FLAGS) -> Result<(), String> {
    // SAFETY: обычный вызов метода живого объекта.
    unsafe { operation.SetOperationFlags(flags) }
        .map_err(|error| describe("не удалось настроить операцию", &error))
}

fn item(path: &Path) -> Result<IShellItem, String> {
    let path_w = wide(path);
    // SAFETY: строка живёт до конца вызова.
    unsafe { SHCreateItemFromParsingName(PCWSTR(path_w.as_ptr()), None) }
        .map_err(|error| describe(&path.display().to_string(), &error))
}

fn item_array(paths: &[PathBuf]) -> Result<IShellItemArray, String> {
    if paths.is_empty() {
        return Err("пустой список объектов".into());
    }
    let pidls = Pidls::new(paths)?;
    // SAFETY: PIDL живут до конца вызова; массив хранит свои копии.
    unsafe { SHCreateShellItemArrayFromIDLists(&pidls.as_const()) }
        .map_err(|error| describe("не удалось собрать список объектов", &error))
}

/// Пути в Windows сравниваются без учёта регистра.
fn same_dir(parent: Option<&Path>, dest: &Path) -> bool {
    let norm = |path: &Path| path.to_string_lossy().trim_end_matches('\\').to_lowercase();
    parent.is_some_and(|parent| norm(parent) == norm(dest))
}

/// Пользователь нажал «Отмена» в диалоге прогресса, конфликта или UAC.
fn is_cancelled(code: HRESULT) -> bool {
    code == HRESULT::from_win32(ERROR_CANCELLED.0) || code == COPYENGINE_E_USER_CANCELLED
}
