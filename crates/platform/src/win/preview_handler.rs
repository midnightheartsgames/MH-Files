//! Хост обработчиков предпросмотра (`IPreviewHandler`), как область просмотра Проводника.
//!
//! Всё COM и окно живут в отдельном STA-потоке со своим циклом сообщений; поток UI только
//! кладёт команды в канал и будит окно. Обработчик создаётся по возможности отдельным
//! процессом (`prevhost.exe`, как у Проводника): упавший обработчик не роняет программу.
//!
//! Окно — всплывающее (`WS_POPUP`) **без владельца**. Окно-ребёнок или окно с владельцем из
//! чужого потока неявно объединяет очереди ввода потоков, и тогда зависший обработчик
//! останавливает ввод и в главном окне. Поэтому за главным окном поток следит сам, через
//! WinEvent: двигается вместе с ним, прячется при сворачивании и держится в z-порядке прямо
//! над ним.

use std::cell::Cell;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicIsize, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::sync::{Arc, Mutex, Once};
use std::time::{Duration, Instant};

use windows::Win32::Foundation::{COLORREF, HINSTANCE, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{ClientToScreen, CreateSolidBrush};
use windows::Win32::System::Com::{
    CLSCTX_INPROC_SERVER, CLSCTX_LOCAL_SERVER, CoCreateInstance, STGM_READ, STGM_SHARE_DENY_NONE,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Accessibility::{HWINEVENTHOOK, SetWinEventHook, UnhookWinEvent};
use windows::Win32::UI::Shell::PropertiesSystem::{IInitializeWithFile, IInitializeWithStream};
use windows::Win32::UI::Shell::{
    ASSOCF_INIT_IGNOREUNKNOWN, ASSOCF_NOTRUNCATE, ASSOCSTR_SHELLEXTENSION, AssocQueryStringW,
    IInitializeWithItem, IPreviewHandler, IPreviewHandlerVisuals, IShellItem,
    SHCreateItemFromParsingName, SHCreateStreamOnFileEx,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, EVENT_OBJECT_LOCATIONCHANGE,
    EVENT_SYSTEM_FOREGROUND, EVENT_SYSTEM_MINIMIZEEND, GW_HWNDPREV, GWL_EXSTYLE,
    GetForegroundWindow, GetMessageW, GetWindow, GetWindowLongW, GetWindowThreadProcessId,
    HWND_TOP, IDC_ARROW, IsChild, IsIconic, IsWindowVisible, LoadCursorW, MA_NOACTIVATE, MSG,
    OBJID_WINDOW, PostMessageW, RegisterClassW, SW_HIDE, SWP_NOACTIVATE, SWP_NOZORDER,
    SWP_SHOWWINDOW, SetForegroundWindow, SetWindowPos, ShowWindow, ShowWindowAsync,
    TranslateMessage, WINEVENT_OUTOFCONTEXT, WM_APP, WM_CLOSE, WM_MOUSEACTIVATE, WNDCLASSW,
    WS_CLIPCHILDREN, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP,
};
use windows::core::{GUID, Interface, PCWSTR, PWSTR, w};

use super::com::{Apartment, describe, owner_hwnd, wide};

/// IID `IPreviewHandler` строкой — под этим ключом `ShellEx` в реестре лежит CLSID обработчика.
const PREVIEW_HANDLER_IID: &str = "{8895b1c6-b41f-4c1c-a562-0d564250836f}";

const CLASS_NAME: PCWSTR = w!("MHFilesPreviewHost");

/// «Проверь канал команд и положение главного окна». Само сообщение ничего не делает: цикл
/// сообщений после любого сообщения смотрит канал и флаг.
const WM_WAKE: u32 = WM_APP + 1;

/// Столько поток может быть занят одной командой, прежде чем обработчик считается зависшим.
/// Office при первом запуске грузится секунды — порог с запасом.
const HANG: Duration = Duration::from_secs(10);

/// Фон окна и просьба к обработчику о тёмном фоне и светлом тексте (COLORREF — 0x00BBGGRR).
const BACKGROUND: COLORREF = COLORREF(0x001D_1612);
const TEXT: COLORREF = COLORREF(0x00E6_E1DC);

pub fn has_handler(ext: &str) -> bool {
    handler_clsid(ext).is_some()
}

/// CLSID обработчика предпросмотра для расширения. `AssocQueryStringW` сама смотрит
/// `.ext\ShellEx`, ProgID и `SystemFileAssociations`.
fn handler_clsid(ext: &str) -> Option<GUID> {
    let ext = ext.trim_start_matches('.');
    if ext.is_empty() {
        return None;
    }
    let assoc = wide(format!(".{ext}"));
    let iid = wide(PREVIEW_HANDLER_IID);
    let mut buffer = [0u16; 64];
    let mut len = buffer.len() as u32;
    // SAFETY: строки и буфер живут до конца вызова; len — размер буфера в символах.
    let found = unsafe {
        AssocQueryStringW(
            ASSOCF_INIT_IGNOREUNKNOWN | ASSOCF_NOTRUNCATE,
            ASSOCSTR_SHELLEXTENSION,
            PCWSTR(assoc.as_ptr()),
            PCWSTR(iid.as_ptr()),
            Some(PWSTR(buffer.as_mut_ptr())),
            &mut len,
        )
    };
    if found.is_err() {
        return None;
    }
    let end = buffer.iter().position(|&unit| unit == 0).unwrap_or(buffer.len());
    let text = String::from_utf16_lossy(&buffer[..end]);
    GUID::try_from(text.trim().trim_start_matches('{').trim_end_matches('}')).ok()
}

/// Команда потоку окна. Прямоугольник — в клиентских координатах главного окна.
enum Command {
    Show(PathBuf, RECT),
    Hide,
}

/// Общее для потока UI и потока окна.
struct Shared {
    /// HWND окна; ноль — окна ещё (или уже) нет.
    hwnd: AtomicIsize,
    error: Mutex<Option<String>>,
    /// С какого момента поток занят командой: миллисекунды от `start` плюс один; ноль — свободен.
    busy_since: AtomicU64,
    start: Instant,
}

impl Shared {
    fn new() -> Shared {
        Shared {
            hwnd: AtomicIsize::new(0),
            error: Mutex::new(None),
            busy_since: AtomicU64::new(0),
            start: Instant::now(),
        }
    }

    fn now(&self) -> u64 {
        self.start.elapsed().as_millis() as u64 + 1
    }

    fn set_busy(&self, busy: bool) {
        self.busy_since.store(if busy { self.now() } else { 0 }, Ordering::SeqCst);
    }

    fn hung(&self) -> bool {
        let since = self.busy_since.load(Ordering::SeqCst);
        since != 0 && self.now().saturating_sub(since) > HANG.as_millis() as u64
    }

    fn error(&self) -> Option<String> {
        self.error.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).clone()
    }

    fn set_error(&self, error: Option<String>) {
        *self.error.lock().unwrap_or_else(|poisoned| poisoned.into_inner()) = error;
    }

    /// Будит цикл сообщений окна. Окна ещё нет — не страшно: поток посмотрит канал сразу
    /// после создания окна.
    fn wake(&self) {
        let hwnd = self.hwnd.load(Ordering::SeqCst);
        if hwnd != 0 {
            // SAFETY: PostMessageW не ждёт получателя; уничтоженное окно — просто ошибка.
            let _ = unsafe { PostMessageW(Some(HWND(hwnd as _)), WM_WAKE, WPARAM(0), LPARAM(0)) };
        }
    }
}

/// Поток окна со стороны UI.
struct Thread {
    commands: Sender<Command>,
    shared: Arc<Shared>,
}

impl Thread {
    fn spawn() -> Result<Thread, String> {
        let (commands, receiver) = mpsc::channel();
        let shared = Arc::new(Shared::new());
        let thread_shared = shared.clone();
        std::thread::Builder::new()
            .name("preview-handler".into())
            .spawn(move || run(&receiver, &thread_shared))
            .map_err(|error| format!("поток предпросмотра не запущен: {error}"))?;
        Ok(Thread { commands, shared })
    }

    fn send(&self, command: Command) {
        if self.commands.send(command).is_ok() {
            self.shared.wake();
        }
    }

    /// Бросить поток: прячет окно, не дожидаясь потока, и закрывает канал. Очнувшись, поток
    /// выгрузит обработчик и завершится сам.
    fn abandon(self) {
        let Thread { commands, shared } = self;
        drop(commands);
        let hwnd = shared.hwnd.load(Ordering::SeqCst);
        if hwnd != 0 {
            // SAFETY: асинхронный вызов — не ждёт поток окна; уничтоженное окно — просто ошибка.
            let _ = unsafe { ShowWindowAsync(HWND(hwnd as _), SW_HIDE) };
        }
        shared.wake();
    }
}

/// Сторона потока UI: канал команд и то, что уже отправлено.
pub struct Host {
    thread: Option<Thread>,
    /// Последний отправленный показ: путь и прямоугольник. Повтор ничего не шлёт.
    shown: Option<(PathBuf, RECT)>,
    /// Поток не запустился.
    spawn_error: Option<String>,
}

impl Host {
    pub fn new() -> Host {
        Host { thread: None, shown: None, spawn_error: None }
    }

    pub fn show(&mut self, path: &Path, rect: (i32, i32, i32, i32)) {
        // Без главного окна некуда привязать окно предпросмотра.
        if owner_hwnd().is_none() {
            return;
        }
        let (x, y, width, height) = rect;
        let rect = RECT { left: x, top: y, right: x + width.max(1), bottom: y + height.max(1) };
        let same_path = self.shown.as_ref().is_some_and(|(shown, _)| shown == path);
        if same_path && self.shown.as_ref().is_some_and(|(_, shown)| *shown == rect) {
            return;
        }
        // Висит на прошлом файле — бросить поток вместе с обработчиком и начать заново.
        if !same_path && let Some(thread) = self.thread.take_if(|thread| thread.shared.hung()) {
            thread.abandon();
        }
        if self.thread.is_none() {
            match Thread::spawn() {
                Ok(thread) => self.thread = Some(thread),
                Err(error) => {
                    self.spawn_error = Some(error);
                    return;
                }
            }
        }
        if let Some(thread) = &self.thread {
            thread.send(Command::Show(path.to_path_buf(), rect));
        }
        self.shown = Some((path.to_path_buf(), rect));
    }

    pub fn hide(&mut self) {
        if self.shown.take().is_none() {
            return;
        }
        if let Some(thread) = self.thread.take_if(|thread| thread.shared.hung()) {
            thread.abandon();
        } else if let Some(thread) = &self.thread {
            thread.send(Command::Hide);
        }
    }

    pub fn error(&self) -> Option<String> {
        if let Some(error) = &self.spawn_error {
            return Some(error.clone());
        }
        let thread = self.thread.as_ref()?;
        if thread.shared.hung() {
            return Some("обработчик предпросмотра не отвечает".into());
        }
        thread.shared.error()
    }
}

impl Drop for Host {
    fn drop(&mut self) {
        // Не ждём: поток сам выгрузит обработчик и уничтожит окно.
        if let Some(thread) = self.thread.take() {
            thread.abandon();
        }
    }
}

thread_local! {
    /// Главное окно сдвинулось, свернулось или сменилось активное окно — пора поправить своё.
    static OWNER_CHANGED: Cell<bool> = const { Cell::new(false) };
    /// Окно этого потока — его будит обработчик WinEvent.
    static OWN_WINDOW: Cell<isize> = const { Cell::new(0) };
}

/// Тело потока окна.
fn run(commands: &Receiver<Command>, shared: &Shared) {
    let _apartment = Apartment::sta();
    let Some(owner) = owner_hwnd() else {
        shared.set_error(Some("окно предпросмотра не к чему привязать".into()));
        return;
    };
    let window = match Window::create() {
        Ok(window) => window,
        Err(error) => {
            shared.set_error(Some(error));
            return;
        }
    };
    OWN_WINDOW.set(window.0.0 as isize);
    let _hooks = Hooks::install(owner);
    shared.hwnd.store(window.0.0 as isize, Ordering::SeqCst);

    let mut view = View::new(window.0, owner);
    loop {
        // Важна только последняя команда: промежуточные файлы показывать незачем.
        let mut last = None;
        let quit = loop {
            match commands.try_recv() {
                Ok(command) => last = Some(command),
                Err(TryRecvError::Empty) => break false,
                Err(TryRecvError::Disconnected) => break true,
            }
        };
        if quit {
            break;
        }
        if let Some(command) = last {
            shared.set_busy(true);
            view.apply(command, shared);
            shared.set_busy(false);
        }
        if OWNER_CHANGED.replace(false) {
            view.place();
        }

        let mut msg = MSG::default();
        // SAFETY: MSG живёт до конца вызовов; окна этого потока обслуживает этот же поток.
        unsafe {
            // 0 — WM_QUIT, -1 — ошибка: в обоих случаях цикл закончен.
            if GetMessageW(&mut msg, None, 0, 0).0 <= 0 {
                break;
            }
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
    shared.hwnd.store(0, Ordering::SeqCst);
    view.unload();
    OWN_WINDOW.set(0);
}

/// Что показано в окне.
struct View {
    hwnd: HWND,
    owner: HWND,
    handler: Option<IPreviewHandler>,
    /// Файл последнего показа — даже если обработчик не загрузился: второй раз не пробуем.
    path: Option<PathBuf>,
    /// Где показывать, в клиентских координатах главного окна.
    rect: RECT,
    /// Окно сейчас на экране.
    visible: bool,
}

impl View {
    fn new(hwnd: HWND, owner: HWND) -> View {
        View { hwnd, owner, handler: None, path: None, rect: RECT::default(), visible: false }
    }

    fn apply(&mut self, command: Command, shared: &Shared) {
        match command {
            Command::Hide => {
                self.unload();
                self.path = None;
                self.place();
                shared.set_error(None);
            }
            Command::Show(path, rect) => {
                let resized = size(&rect) != size(&self.rect);
                self.rect = rect;
                if self.path.as_deref() == Some(path.as_path()) {
                    self.place();
                    if resized && let Some(handler) = &self.handler {
                        let client = client_rect(&rect);
                        // SAFETY: обработчик загружен в этом потоке; RECT живёт до конца вызова.
                        let _ = unsafe { handler.SetRect(&client) };
                    }
                    return;
                }
                self.unload();
                shared.set_error(None);
                self.path = Some(path.clone());
                // Окно на экране до загрузки: обработчик меряет себя по окну-родителю.
                self.place_visible(true);
                match load(&path, self.hwnd, &client_rect(&rect)) {
                    Ok(handler) => self.handler = Some(handler),
                    Err(error) => {
                        shared.set_error(Some(error));
                        self.place();
                    }
                }
            }
        }
    }

    /// Выгрузить обработчик, если он есть.
    fn unload(&mut self) {
        if let Some(handler) = self.handler.take() {
            // SAFETY: обработчик загружен в этом потоке; после Unload объект освобождается.
            let _ = unsafe { handler.Unload() };
        }
    }

    /// Поставить окно на место: видно, если есть обработчик и главное окно не свёрнуто.
    fn place(&mut self) {
        self.place_visible(self.handler.is_some());
    }

    fn place_visible(&mut self, wanted: bool) {
        // SAFETY: функции только читают состояние главного окна, сообщений ему не шлют.
        let owner_shown =
            unsafe { IsWindowVisible(self.owner).as_bool() && !IsIconic(self.owner).as_bool() };
        let mut origin = POINT { x: self.rect.left, y: self.rect.top };
        // SAFETY: то же — только пересчёт координат.
        let located = unsafe { ClientToScreen(self.owner, &mut origin) }.as_bool();
        if !(wanted && owner_shown && located) {
            if self.visible {
                // SAFETY: окно этого потока.
                let _ = unsafe { ShowWindow(self.hwnd, SW_HIDE) };
                self.visible = false;
            }
            return;
        }
        let (width, height) = size(&self.rect);
        let after = self.insert_after();
        let mut flags = SWP_NOACTIVATE | SWP_SHOWWINDOW;
        if after.is_none() {
            flags |= SWP_NOZORDER;
        }
        // SAFETY: окно этого потока; окно «после которого» только называется, сообщений ему
        // не шлют.
        let _ = unsafe { SetWindowPos(self.hwnd, after, origin.x, origin.y, width, height, flags) };
        self.visible = true;
    }

    /// Куда поставить окно в z-порядке, чтобы оно было прямо над главным. `None` — уже там.
    fn insert_after(&self) -> Option<HWND> {
        // SAFETY: чтение z-порядка и стиля чужого окна; сообщений не шлёт.
        unsafe {
            match GetWindow(self.owner, GW_HWNDPREV) {
                Ok(above) if above == self.hwnd => None,
                // Над главным окном — окно «поверх всех»: встаём в начало обычных окон, иначе
                // сами стали бы «поверх всех».
                Ok(above) if GetWindowLongW(above, GWL_EXSTYLE) as u32 & WS_EX_TOPMOST.0 == 0 => {
                    Some(above)
                }
                _ => Some(HWND_TOP),
            }
        }
    }
}

fn size(rect: &RECT) -> (i32, i32) {
    (rect.right - rect.left, rect.bottom - rect.top)
}

/// Прямоугольник для обработчика — вся клиентская область окна.
fn client_rect(rect: &RECT) -> RECT {
    let (width, height) = size(rect);
    RECT { left: 0, top: 0, right: width, bottom: height }
}

/// Создать, открыть файлом и показать обработчик в окне `hwnd`.
fn load(path: &Path, hwnd: HWND, client: &RECT) -> Result<IPreviewHandler, String> {
    let ext = path.extension().and_then(|ext| ext.to_str()).unwrap_or("");
    let clsid = handler_clsid(ext).ok_or_else(|| match ext {
        "" => "у файла без расширения нет обработчика предпросмотра".to_string(),
        ext => format!("для .{ext} нет обработчика предпросмотра"),
    })?;
    // Сначала отдельным процессом, как Проводник; не умеет — в нашем процессе.
    // SAFETY: обычное создание COM-объекта в STA этого потока.
    let handler: IPreviewHandler = unsafe {
        CoCreateInstance(&clsid, None, CLSCTX_LOCAL_SERVER)
            .or_else(|_| CoCreateInstance(&clsid, None, CLSCTX_INPROC_SERVER))
    }
    .map_err(|error| describe("обработчик предпросмотра не загрузился", &error))?;
    initialize(&handler, path)
        .map_err(|error| describe("файл не открылся в обработчике предпросмотра", &error))?;

    if let Ok(visuals) = handler.cast::<IPreviewHandlerVisuals>() {
        // SAFETY: обычные вызовы; не умеет — останется его оформление.
        unsafe {
            let _ = visuals.SetBackgroundColor(BACKGROUND);
            let _ = visuals.SetTextColor(TEXT);
        }
    }

    // SAFETY: функция только читает, какое окно активно.
    let foreground = unsafe { GetForegroundWindow() };
    // SAFETY: окно этого потока живо; RECT живёт до конца вызова.
    let shown = unsafe { handler.SetWindow(hwnd, client).and_then(|()| handler.DoPreview()) };
    if let Err(error) = shown {
        // SAFETY: обработчик создан здесь; после Unload он больше не используется.
        let _ = unsafe { handler.Unload() };
        return Err(describe("файл не открылся в обработчике предпросмотра", &error));
    }
    // Обработчик забрал активность себе — вернуть её окну, у которого она была.
    // SAFETY: функции только читают состояние окон; SetForegroundWindow при отказе ничего не
    // меняет.
    unsafe {
        let now = GetForegroundWindow();
        if now != foreground
            && !foreground.is_invalid()
            && (now == hwnd || IsChild(hwnd, now).as_bool())
        {
            let _ = SetForegroundWindow(foreground);
        }
    }
    Ok(handler)
}

/// Открыть файл в обработчике: путём, элементом Shell или потоком — что он умеет.
fn initialize(handler: &IPreviewHandler, path: &Path) -> windows::core::Result<()> {
    let path_w = wide(path);
    let path_p = PCWSTR(path_w.as_ptr());
    let mode = STGM_READ.0;
    let mut last = None;
    // SAFETY: строка пути живёт до конца функции; объекты COM — до конца своих вызовов.
    unsafe {
        if let Ok(init) = handler.cast::<IInitializeWithFile>() {
            match init.Initialize(path_p, mode) {
                Ok(()) => return Ok(()),
                Err(error) => last = Some(error),
            }
        }
        if let Ok(init) = handler.cast::<IInitializeWithItem>() {
            let opened = SHCreateItemFromParsingName::<_, _, IShellItem>(path_p, None)
                .and_then(|item| init.Initialize(&item, mode));
            match opened {
                Ok(()) => return Ok(()),
                Err(error) => last = Some(error),
            }
        }
        match handler.cast::<IInitializeWithStream>() {
            Ok(init) => {
                let stream = SHCreateStreamOnFileEx(
                    path_p,
                    (STGM_READ | STGM_SHARE_DENY_NONE).0,
                    0,
                    false,
                    None,
                )?;
                init.Initialize(&stream, mode)
            }
            Err(error) => Err(last.unwrap_or(error)),
        }
    }
}

/// Окно предпросмотра; уничтожается вместе с потоком.
struct Window(HWND);

impl Window {
    fn create() -> Result<Window, String> {
        // SAFETY: модуль процесса живёт всё время работы.
        let instance: HINSTANCE = unsafe { GetModuleHandleW(None) }
            .map_err(|error| describe("окно предпросмотра не создано", &error))?
            .into();
        static REGISTER: Once = Once::new();
        REGISTER.call_once(|| {
            // SAFETY: кисть и курсор нужны классу окна всё время работы программы и не
            // освобождаются. Неудачу регистрации покажет CreateWindowExW.
            unsafe {
                let class = WNDCLASSW {
                    lpfnWndProc: Some(window_proc),
                    hInstance: instance,
                    lpszClassName: CLASS_NAME,
                    hbrBackground: CreateSolidBrush(BACKGROUND),
                    hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
                    ..Default::default()
                };
                RegisterClassW(&class);
            }
        });
        // SAFETY: имя класса — статическая строка; окно уничтожается в Drop. Без владельца —
        // см. описание модуля.
        let hwnd = unsafe {
            CreateWindowExW(
                WS_EX_NOACTIVATE | WS_EX_TOOLWINDOW,
                CLASS_NAME,
                PCWSTR::null(),
                WS_POPUP | WS_CLIPCHILDREN,
                0,
                0,
                0,
                0,
                None,
                None,
                Some(instance),
                None,
            )
        }
        .map_err(|error| describe("окно предпросмотра не создано", &error))?;
        Ok(Window(hwnd))
    }
}

impl Drop for Window {
    fn drop(&mut self) {
        // SAFETY: окно создано в этом потоке и больше не используется.
        let _ = unsafe { DestroyWindow(self.0) };
    }
}

unsafe extern "system" fn window_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match message {
        // Только будит цикл сообщений; работа — там, не здесь: сюда можно попасть изнутри
        // вызова обработчика.
        WM_WAKE => LRESULT(0),
        // Щелчок по предпросмотру не забирает активность у главного окна.
        WM_MOUSEACTIVATE => LRESULT(MA_NOACTIVATE as isize),
        // Окно закрывает только поток.
        WM_CLOSE => LRESULT(0),
        // SAFETY: стандартная обработка остальных сообщений.
        _ => unsafe { DefWindowProcW(hwnd, message, wparam, lparam) },
    }
}

/// Подписки WinEvent на изменения главного окна; снимаются при уничтожении.
struct Hooks(Vec<HWINEVENTHOOK>);

impl Hooks {
    fn install(owner: HWND) -> Hooks {
        let mut process = 0u32;
        // SAFETY: функция только читает, чьё это окно.
        let thread = unsafe { GetWindowThreadProcessId(owner, Some(&mut process)) };
        // SAFETY: обработчик — функция с нужной сигнатурой, живёт всё время работы; события
        // приходят вне контекста, через цикл сообщений этого потока. Неудача — нулевая
        // подписка: окно просто не будет следовать за главным.
        let hooks = unsafe {
            [
                // Сдвиг и размер главного окна.
                SetWinEventHook(
                    EVENT_OBJECT_LOCATIONCHANGE,
                    EVENT_OBJECT_LOCATIONCHANGE,
                    None,
                    Some(on_win_event),
                    process,
                    thread,
                    WINEVENT_OUTOFCONTEXT,
                ),
                // Смена активного окна (главное могло встать поверх предпросмотра),
                // сворачивание и разворачивание.
                SetWinEventHook(
                    EVENT_SYSTEM_FOREGROUND,
                    EVENT_SYSTEM_MINIMIZEEND,
                    None,
                    Some(on_win_event),
                    0,
                    0,
                    WINEVENT_OUTOFCONTEXT,
                ),
            ]
        };
        Hooks(hooks.into_iter().filter(|hook| !hook.is_invalid()).collect())
    }
}

impl Drop for Hooks {
    fn drop(&mut self) {
        for hook in &self.0 {
            // SAFETY: подписка создана в этом потоке и снимается один раз.
            let _ = unsafe { UnhookWinEvent(*hook) };
        }
    }
}

/// Обработчик WinEvent: только ставит флаг и будит цикл сообщений.
unsafe extern "system" fn on_win_event(
    _hook: HWINEVENTHOOK,
    event: u32,
    hwnd: HWND,
    object: i32,
    _child: i32,
    _thread: u32,
    _time: u32,
) {
    if event == EVENT_OBJECT_LOCATIONCHANGE
        && (object != OBJID_WINDOW.0 || owner_hwnd() != Some(hwnd))
    {
        return;
    }
    OWNER_CHANGED.set(true);
    let own = OWN_WINDOW.get();
    if own != 0 {
        // SAFETY: окно этого потока; PostMessageW не ждёт.
        let _ = unsafe { PostMessageW(Some(HWND(own as _)), WM_WAKE, WPARAM(0), LPARAM(0)) };
    }
}
