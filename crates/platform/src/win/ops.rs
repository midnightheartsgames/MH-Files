//! Файловые операции через `IFileOperation`: системные корзина, конфликты имён, запрос прав
//! администратора и «Отменить» в Проводнике.
//!
//! Свой диалог прогресса Windows не показывает (`FOF_SILENT`): ход, пауза и отмена идут через
//! `IFileOperationProgressSink` в панель MH Files.

use std::cell::{Cell, RefCell};
use std::ffi::OsString;
use std::os::windows::ffi::OsStringExt;
use std::path::{Path, PathBuf};

use windows::Win32::Foundation::{E_ABORT, ERROR_CANCELLED};
use windows::Win32::Storage::FileSystem::FILE_ATTRIBUTE_DIRECTORY;
use windows::Win32::System::Com::{CLSCTX_ALL, CoCreateInstance, CoTaskMemFree};
use windows::Win32::UI::Shell::{
    COPYENGINE_E_USER_CANCELLED, FILEOPERATION_FLAGS, FOF_ALLOWUNDO, FOF_NOCONFIRMATION,
    FOF_NOCONFIRMMKDIR, FOF_RENAMEONCOLLISION, FOF_SILENT, FOF_WANTNUKEWARNING, FOFX_ADDUNDORECORD,
    FOFX_EARLYFAILURE, FOFX_RECYCLEONDELETE, FileOperation, IFileOperation,
    IFileOperationProgressSink, IFileOperationProgressSink_Impl, IShellItem, IShellItemArray,
    SHCreateItemFromParsingName, SHCreateShellItemArrayFromIDLists, SIGDN_FILESYSPATH,
};
use windows::core::{ComObject, HRESULT, PCWSTR, Ref, implement};

use super::com::{describe, owner_hwnd, wide};
use super::shell::Pidls;
use crate::ops::{Control, FileOp, OnConflict, OpOutcome, Progress};

pub fn execute(
    op: &FileOp,
    control: &Control,
    progress: &mut dyn FnMut(Progress),
) -> Result<OpOutcome, String> {
    // SAFETY: вызывается в STA-потоке операций; все COM-объекты живут до конца функции.
    let operation: IFileOperation =
        unsafe { CoCreateInstance(&FileOperation, None, CLSCTX_ALL) }
            .map_err(|error| describe("не удалось запустить файловую операцию", &error))?;
    if let Some(owner) = owner_hwnd() {
        // SAFETY: HWND главного окна; при ошибке диалоги просто будут без владельца.
        let _ = unsafe { operation.SetOwnerWindow(owner) };
    }

    let base = FOF_ALLOWUNDO | FOF_NOCONFIRMMKDIR | FOFX_ADDUNDORECORD;
    // Объекты верхнего уровня: только для них строятся пары «откуда → куда».
    let top: Vec<PathBuf>;
    // Что вернуть, если обработчик ничего не сообщил (переименование, новая папка).
    let mut fallback_created = Vec::new();
    let mut fallback_pairs = Vec::new();
    // SAFETY: строки и PIDL живут до конца вызовов; IFileOperation копирует то, что ему нужно.
    unsafe {
        match op {
            FileOp::Copy { sources, dest, on_conflict } => {
                let sources = without_skipped(sources, dest, *on_conflict);
                if sources.is_empty() {
                    return Ok(OpOutcome::done(Vec::new(), Vec::new()));
                }
                let same = sources.iter().all(|source| same_dir(source.parent(), dest));
                set_flags(&operation, base | FOF_SILENT | conflict_flags(*on_conflict, same))?;
                let items = item_array(&sources)?;
                operation
                    .CopyItems(&items, &item(dest)?)
                    .map_err(|error| describe("копирование не подготовлено", &error))?;
                top = sources;
            }
            FileOp::Move { sources, dest, on_conflict } => {
                // Перемещение в ту же папку ничего не меняет, а с переименованием при
                // конфликте сделало бы копию с новым именем.
                let sources: Vec<PathBuf> = without_skipped(sources, dest, *on_conflict)
                    .into_iter()
                    .filter(|source| !same_dir(source.parent(), dest))
                    .collect();
                if sources.is_empty() {
                    return Ok(OpOutcome::done(Vec::new(), Vec::new()));
                }
                set_flags(&operation, base | FOF_SILENT | conflict_flags(*on_conflict, false))?;
                let items = item_array(&sources)?;
                operation
                    .MoveItems(&items, &item(dest)?)
                    .map_err(|error| describe("перемещение не подготовлено", &error))?;
                top = sources;
            }
            FileOp::Delete { paths, permanent: false } => {
                // Как Проводник в Windows 10+: в корзину без вопроса. Но если объект в корзину не
                // помещается, NOCONFIRMATION молча удалил бы его насовсем — WANTNUKEWARNING
                // возвращает для этого случая системный вопрос.
                set_flags(
                    &operation,
                    FOF_ALLOWUNDO
                        | FOF_NOCONFIRMATION
                        | FOF_WANTNUKEWARNING
                        | FOFX_RECYCLEONDELETE
                        | FOF_SILENT,
                )?;
                let items = item_array(paths)?;
                operation
                    .DeleteItems(&items)
                    .map_err(|error| describe("удаление не подготовлено", &error))?;
                top = Vec::new();
            }
            FileOp::Delete { paths, permanent: true } => {
                // Точный список путей пользователь уже подтвердил в нашем окне.
                set_flags(&operation, FOF_NOCONFIRMATION | FOFX_EARLYFAILURE | FOF_SILENT)?;
                let items = item_array(paths)?;
                operation
                    .DeleteItems(&items)
                    .map_err(|error| describe("удаление не подготовлено", &error))?;
                top = Vec::new();
            }
            FileOp::Rename { path, new_name } => {
                set_flags(&operation, base)?;
                let name = wide(new_name);
                operation
                    .RenameItem(&item(path)?, PCWSTR(name.as_ptr()), None)
                    .map_err(|error| describe("переименование не подготовлено", &error))?;
                top = vec![path.clone()];
                fallback_created.push(path.with_file_name(new_name));
                fallback_pairs.push((path.clone(), path.with_file_name(new_name)));
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
                top = Vec::new();
                fallback_created.push(parent.join(name));
            }
        }
    }

    let sink = ComObject::new(Sink::new(control, progress, &top));
    let result = {
        let _advice = Advice::new(&operation, &sink);
        // SAFETY: обработчик прогресса вызывается в этом же потоке, пока идёт вызов.
        unsafe { operation.PerformOperations() }
    };
    if let Err(error) = result {
        if is_cancelled(error.code()) || control.is_cancelled() {
            return Ok(OpOutcome::Aborted);
        }
        return Err(describe(&op.describe(), &error));
    }
    // SAFETY: обычный вызов метода живого объекта.
    let aborted = unsafe { operation.GetAnyOperationsAborted() };
    if control.is_cancelled() || aborted.is_ok_and(|aborted| aborted.as_bool()) {
        return Ok(OpOutcome::Aborted);
    }

    let done = sink.state.take();
    if done.created.is_empty() && done.pairs.is_empty() {
        return Ok(OpOutcome::done(fallback_created, fallback_pairs));
    }
    Ok(OpOutcome::done(done.created, done.pairs))
}

/// `Skip`: объекты, чьё имя в папке назначения уже занято, в очередь не попадают — тогда
/// системный диалог конфликта не появится.
fn without_skipped(sources: &[PathBuf], dest: &Path, on_conflict: OnConflict) -> Vec<PathBuf> {
    if on_conflict != OnConflict::Skip {
        return sources.to_vec();
    }
    sources
        .iter()
        .filter(|source| source.file_name().is_none_or(|name| !dest.join(name).exists()))
        .cloned()
        .collect()
}

fn conflict_flags(on_conflict: OnConflict, same_folder: bool) -> FILEOPERATION_FLAGS {
    match on_conflict {
        // Копия в ту же папку — «имя - копия», а не замена объекта самим собой.
        OnConflict::Replace if !same_folder => FOF_NOCONFIRMATION,
        OnConflict::KeepBoth | OnConflict::Replace => FOF_RENAMEONCOLLISION,
        OnConflict::Ask if same_folder => FOF_RENAMEONCOLLISION,
        // Конфликтующие при Skip уже убраны; Ask — системный диалог.
        OnConflict::Ask | OnConflict::Skip => FILEOPERATION_FLAGS(0),
    }
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

/// Путь объекта Shell в файловой системе; `None` для виртуальных (корзина и т. п.).
fn item_path(item: Option<&IShellItem>) -> Option<PathBuf> {
    // SAFETY: строку выделяет Shell, мы копируем её и освобождаем CoTaskMemFree.
    unsafe {
        let name = item?.GetDisplayName(SIGDN_FILESYSPATH).ok()?;
        let path = PathBuf::from(OsString::from_wide(name.as_wide()));
        CoTaskMemFree(Some(name.0 as *const _));
        Some(path)
    }
}

/// Ключ сравнения путей: в Windows регистр не важен.
fn path_key(path: &Path) -> String {
    path.to_string_lossy().trim_end_matches('\\').to_lowercase()
}

/// Пути в Windows сравниваются без учёта регистра.
fn same_dir(parent: Option<&Path>, dest: &Path) -> bool {
    parent.is_some_and(|parent| path_key(parent) == path_key(dest))
}

/// Пользователь нажал «Отмена» в диалоге конфликта или UAC, либо отменил нашей кнопкой.
fn is_cancelled(code: HRESULT) -> bool {
    code == HRESULT::from_win32(ERROR_CANCELLED.0)
        || code == COPYENGINE_E_USER_CANCELLED
        || code == E_ABORT
}

/// Подписка обработчика на время `PerformOperations`. При снятии обработчик перестаёт
/// трогать `control` и `progress`: после `execute` их уже нет, а ссылку на него движок
/// может держать и дольше.
struct Advice<'a> {
    operation: &'a IFileOperation,
    sink: &'a ComObject<Sink>,
    cookie: Option<u32>,
}

impl<'a> Advice<'a> {
    fn new(operation: &'a IFileOperation, sink: &'a ComObject<Sink>) -> Advice<'a> {
        let interface: IFileOperationProgressSink = sink.to_interface();
        // SAFETY: обычный вызов; без подписки операция просто идёт без прогресса и паузы.
        let cookie = unsafe { operation.Advise(&interface) }.ok();
        Advice { operation, sink, cookie }
    }
}

impl Drop for Advice<'_> {
    fn drop(&mut self) {
        self.sink.live.set(false);
        if let Some(cookie) = self.cookie {
            // SAFETY: cookie получен от Advise этого же объекта.
            let _ = unsafe { self.operation.Unadvise(cookie) };
        }
    }
}

#[derive(Default)]
struct Results {
    /// Последний объект, о котором сообщил движок: его показывает панель прогресса.
    item: Option<PathBuf>,
    created: Vec<PathBuf>,
    pairs: Vec<(PathBuf, PathBuf)>,
}

/// Обработчик событий `IFileOperation`.
///
/// `control` и `report` — ссылки из `execute` со стёртым временем жизни (COM-объект обязан
/// быть `'static`). Разыменовываются только пока `live`: флаг снимает [`Advice`] до выхода из
/// `execute`. Движок зовёт обработчик в нашем STA-потоке и не повторно, так что `&mut` на
/// замыкание в каждый момент одно.
#[implement(IFileOperationProgressSink)]
struct Sink {
    live: Cell<bool>,
    control: *const Control,
    report: *mut (dyn FnMut(Progress) + 'static),
    /// Ключи путей верхнего уровня: движок сообщает и о вложенных объектах папок.
    top: Vec<String>,
    state: RefCell<Results>,
}

impl Sink {
    fn new(control: &Control, report: &mut dyn FnMut(Progress), top: &[PathBuf]) -> Sink {
        let report: *mut (dyn FnMut(Progress) + '_) = report;
        // SAFETY: меняется только время жизни; см. описание `Sink`.
        let report = unsafe {
            std::mem::transmute::<
                *mut (dyn FnMut(Progress) + '_),
                *mut (dyn FnMut(Progress) + 'static),
            >(report)
        };
        Sink {
            live: Cell::new(true),
            control,
            report,
            top: top.iter().map(|path| path_key(path)).collect(),
            state: RefCell::default(),
        }
    }

    /// Пауза держит движок здесь; отмена возвращает ошибку, и движок останавливается.
    fn checkpoint(&self) -> windows::core::Result<()> {
        if !self.live.get() {
            return Ok(());
        }
        // SAFETY: `live` — `execute` ещё не вернулся, ссылка действительна.
        let control = unsafe { &*self.control };
        if control.checkpoint() {
            Ok(())
        } else {
            Err(HRESULT::from_win32(ERROR_CANCELLED.0).into())
        }
    }

    fn started(&self, item: Ref<IShellItem>) -> windows::core::Result<()> {
        if let Some(path) = item_path(item.as_ref()) {
            self.state.borrow_mut().item = Some(path);
        }
        self.checkpoint()
    }

    fn finished(&self, item: Ref<IShellItem>, result: HRESULT, created: Ref<IShellItem>) {
        // При «Пропустить» в диалоге конфликта нового объекта нет.
        if result.is_err() || created.is_null() {
            return;
        }
        let (Some(from), Some(to)) = (item_path(item.as_ref()), item_path(created.as_ref())) else {
            return;
        };
        if !self.top.contains(&path_key(&from)) {
            return;
        }
        let mut state = self.state.borrow_mut();
        state.created.push(to.clone());
        state.pairs.push((from, to));
    }
}

impl IFileOperationProgressSink_Impl for Sink_Impl {
    fn StartOperations(&self) -> windows::core::Result<()> {
        Ok(())
    }

    fn FinishOperations(&self, _result: HRESULT) -> windows::core::Result<()> {
        Ok(())
    }

    fn PreRenameItem(
        &self,
        _flags: u32,
        item: Ref<IShellItem>,
        _new_name: &PCWSTR,
    ) -> windows::core::Result<()> {
        self.started(item)
    }

    fn PostRenameItem(
        &self,
        _flags: u32,
        item: Ref<IShellItem>,
        _new_name: &PCWSTR,
        result: HRESULT,
        created: Ref<IShellItem>,
    ) -> windows::core::Result<()> {
        self.finished(item, result, created);
        Ok(())
    }

    fn PreMoveItem(
        &self,
        _flags: u32,
        item: Ref<IShellItem>,
        _dest: Ref<IShellItem>,
        _new_name: &PCWSTR,
    ) -> windows::core::Result<()> {
        self.started(item)
    }

    fn PostMoveItem(
        &self,
        _flags: u32,
        item: Ref<IShellItem>,
        _dest: Ref<IShellItem>,
        _new_name: &PCWSTR,
        result: HRESULT,
        created: Ref<IShellItem>,
    ) -> windows::core::Result<()> {
        self.finished(item, result, created);
        Ok(())
    }

    fn PreCopyItem(
        &self,
        _flags: u32,
        item: Ref<IShellItem>,
        _dest: Ref<IShellItem>,
        _new_name: &PCWSTR,
    ) -> windows::core::Result<()> {
        self.started(item)
    }

    fn PostCopyItem(
        &self,
        _flags: u32,
        item: Ref<IShellItem>,
        _dest: Ref<IShellItem>,
        _new_name: &PCWSTR,
        result: HRESULT,
        created: Ref<IShellItem>,
    ) -> windows::core::Result<()> {
        self.finished(item, result, created);
        Ok(())
    }

    fn PreDeleteItem(&self, _flags: u32, item: Ref<IShellItem>) -> windows::core::Result<()> {
        self.started(item)
    }

    fn PostDeleteItem(
        &self,
        _flags: u32,
        _item: Ref<IShellItem>,
        _result: HRESULT,
        _created: Ref<IShellItem>,
    ) -> windows::core::Result<()> {
        Ok(())
    }

    fn PreNewItem(
        &self,
        _flags: u32,
        _dest: Ref<IShellItem>,
        _new_name: &PCWSTR,
    ) -> windows::core::Result<()> {
        self.checkpoint()
    }

    fn PostNewItem(
        &self,
        _flags: u32,
        _dest: Ref<IShellItem>,
        _new_name: &PCWSTR,
        _template: &PCWSTR,
        _attributes: u32,
        result: HRESULT,
        created: Ref<IShellItem>,
    ) -> windows::core::Result<()> {
        if result.is_ok()
            && let Some(path) = item_path(created.as_ref())
        {
            self.state.borrow_mut().created.push(path);
        }
        Ok(())
    }

    fn UpdateProgress(&self, total: u32, done: u32) -> windows::core::Result<()> {
        self.checkpoint()?;
        if !self.live.get() {
            return Ok(());
        }
        let item = self.state.borrow().item.clone();
        // SAFETY: `live` — замыкание из `execute` живо; см. описание `Sink`.
        let report = unsafe { &mut *self.report };
        report(Progress { done: done.into(), total: total.into(), item });
        Ok(())
    }

    fn ResetTimer(&self) -> windows::core::Result<()> {
        Ok(())
    }

    fn PauseTimer(&self) -> windows::core::Result<()> {
        Ok(())
    }

    fn ResumeTimer(&self) -> windows::core::Result<()> {
        Ok(())
    }
}
