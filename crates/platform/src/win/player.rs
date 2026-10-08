//! Media Engine: открыть файл, играть, отдавать кадры. Поток проигрывателя — MTA; Media
//! Engine сам держит звук и синхронизацию, а кадр видео по `OnVideoStreamTick` переносится
//! (`TransferVideoFrame`) в текстуру нужного размера, копируется в память и уходит окну.

use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, RecvTimeoutError};
use windows::Win32::Foundation::{HMODULE, RECT};
use windows::Win32::Graphics::Direct3D::{D3D_DRIVER_TYPE_HARDWARE, D3D_DRIVER_TYPE_WARP};
use windows::Win32::Graphics::Direct3D11::{
    D3D11_BIND_RENDER_TARGET, D3D11_CPU_ACCESS_READ, D3D11_CREATE_DEVICE_BGRA_SUPPORT,
    D3D11_CREATE_DEVICE_VIDEO_SUPPORT, D3D11_MAP_READ, D3D11_MAPPED_SUBRESOURCE, D3D11_SDK_VERSION,
    D3D11_TEXTURE2D_DESC, D3D11_USAGE_DEFAULT, D3D11_USAGE_STAGING, D3D11CreateDevice,
    ID3D11Device, ID3D11DeviceContext, ID3D11Multithread, ID3D11Texture2D,
};
use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_SAMPLE_DESC};
use windows::Win32::Media::MediaFoundation::{
    CLSID_MFMediaEngineClassFactory, IMFAttributes, IMFDXGIDeviceManager, IMFMediaEngine,
    IMFMediaEngineClassFactory, IMFMediaEngineNotify, IMFMediaEngineNotify_Impl,
    MF_MEDIA_ENGINE_CALLBACK, MF_MEDIA_ENGINE_DXGI_MANAGER, MF_MEDIA_ENGINE_EVENT_ERROR,
    MF_MEDIA_ENGINE_VIDEO_OUTPUT_FORMAT, MF_VERSION, MFCreateAttributes, MFCreateDXGIDeviceManager,
    MFSTARTUP_FULL, MFSTARTUP_LITE, MFShutdown, MFStartup,
};
use windows::Win32::System::Com::{
    CLSCTX_ALL, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx, CoUninitialize,
};
use windows::core::{BSTR, Interface, implement};

use crate::Waker;
use crate::player::{Command, Frame, Shared};

/// Как часто спрашивать движок о новом кадре и времени.
const TICK: Duration = Duration::from_millis(8);
/// Как часто будить окно ради часов, если кадров нет (звук).
const CLOCK_EVERY: Duration = Duration::from_millis(250);

pub fn run(
    path: &Path,
    max_side: u32,
    shared: &Arc<Shared>,
    commands: &Receiver<Command>,
    waker: &Waker,
) {
    let fail = |error: String| {
        if let Ok(mut state) = shared.state.lock() {
            state.error = Some(error);
        }
        waker();
    };
    // SAFETY: парный CoUninitialize — в конце этой же функции.
    let com = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) }.is_ok();
    // SAFETY: парный MFShutdown — в конце.
    if let Err(error) = unsafe { MFStartup(MF_VERSION, MFSTARTUP_FULL) } {
        fail(format!("Media Foundation недоступна: {error}"));
    } else {
        match Engine::new(path) {
            Ok(mut engine) => engine.play_loop(max_side, shared, commands, waker),
            Err(error) => fail(error),
        }
        // SAFETY: парный вызов к успешному MFStartup.
        let _ = unsafe { MFShutdown() };
    }
    if com {
        // SAFETY: парный вызов к успешному CoInitializeEx в этом потоке.
        unsafe { CoUninitialize() };
    }
}

/// События движка (ошибки) из его потока.
#[implement(IMFMediaEngineNotify)]
struct Notify {
    error: Arc<Mutex<Option<(usize, u32)>>>,
}

impl IMFMediaEngineNotify_Impl for Notify_Impl {
    fn EventNotify(&self, event: u32, param1: usize, param2: u32) -> windows::core::Result<()> {
        if event == MF_MEDIA_ENGINE_EVENT_ERROR.0 as u32
            && let Ok(mut error) = self.error.lock()
        {
            *error = Some((param1, param2));
        }
        Ok(())
    }
}

struct Engine {
    engine: IMFMediaEngine,
    device: ID3D11Device,
    context: ID3D11DeviceContext,
    error: Arc<Mutex<Option<(usize, u32)>>>,
    /// Текстура кадра и её копия для чтения процессором — под текущий размер.
    target: Option<(u32, u32, ID3D11Texture2D, ID3D11Texture2D)>,
}

impl Engine {
    fn new(path: &Path) -> Result<Engine, String> {
        let (device, context) = create_device()?;
        let error = Arc::new(Mutex::new(None));
        let notify: IMFMediaEngineNotify = Notify { error: error.clone() }.into();
        // SAFETY: обычные вызовы COM; все объекты живут, пока жив `Engine`.
        unsafe {
            // Движок и наш поток работают с устройством одновременно.
            if let Ok(multithread) = context.cast::<ID3D11Multithread>() {
                let _ = multithread.SetMultithreadProtected(true);
            }
            let mut token = 0u32;
            let mut manager: Option<IMFDXGIDeviceManager> = None;
            MFCreateDXGIDeviceManager(&mut token, &mut manager).map_err(text)?;
            let manager = manager.ok_or("нет менеджера устройства")?;
            manager.ResetDevice(&device, token).map_err(text)?;
            let mut attributes: Option<IMFAttributes> = None;
            MFCreateAttributes(&mut attributes, 3).map_err(text)?;
            let attributes = attributes.ok_or("нет атрибутов")?;
            attributes.SetUnknown(&MF_MEDIA_ENGINE_CALLBACK, &notify).map_err(text)?;
            attributes.SetUnknown(&MF_MEDIA_ENGINE_DXGI_MANAGER, &manager).map_err(text)?;
            attributes
                .SetUINT32(
                    &MF_MEDIA_ENGINE_VIDEO_OUTPUT_FORMAT,
                    DXGI_FORMAT_B8G8R8A8_UNORM.0 as u32,
                )
                .map_err(text)?;
            let factory: IMFMediaEngineClassFactory =
                CoCreateInstance(&CLSID_MFMediaEngineClassFactory, None, CLSCTX_ALL)
                    .map_err(text)?;
            let engine = factory.CreateInstance(0, &attributes).map_err(text)?;
            engine.SetSource(&BSTR::from(path.to_string_lossy().as_ref())).map_err(text)?;
            engine.Play().map_err(text)?;
            Ok(Engine { engine, device, context, error, target: None })
        }
    }

    fn play_loop(
        &mut self,
        max_side: u32,
        shared: &Arc<Shared>,
        commands: &Receiver<Command>,
        waker: &Waker,
    ) {
        let mut last_pts = i64::MIN;
        let mut last_clock = Instant::now();
        let mut volume = 1.0;
        loop {
            match commands.recv_timeout(TICK) {
                Ok(command) => {
                    // SAFETY: обычные вызовы движка в его потоке.
                    unsafe {
                        let _ = match command {
                            Command::Play => {
                                if self.engine.IsEnded().as_bool() {
                                    let _ = self.engine.SetCurrentTime(0.0);
                                }
                                self.engine.Play()
                            }
                            Command::Pause => self.engine.Pause(),
                            Command::Seek(seconds) => self.engine.SetCurrentTime(seconds),
                            Command::Volume(value) => {
                                volume = value;
                                self.engine.SetVolume(value)
                            }
                        };
                    }
                    last_clock -= CLOCK_EVERY;
                }
                Err(RecvTimeoutError::Timeout) => {}
                // Проигрыватель брошен: окно закрыло просмотр.
                Err(RecvTimeoutError::Disconnected) => break,
            }
            if let Some((code, hresult)) = self.error.lock().ok().and_then(|mut e| e.take()) {
                if let Ok(mut state) = shared.state.lock() {
                    state.error = Some(describe_error(code, hresult));
                }
                waker();
                // Дальше играть нечего, но поток ждёт закрытия, чтобы не терять ошибку.
                while commands.recv().is_ok() {}
                break;
            }
            // SAFETY: обычные вызовы движка в его потоке.
            let (has_video, has_audio, duration, position, paused, ended) = unsafe {
                (
                    self.engine.HasVideo().as_bool(),
                    self.engine.HasAudio().as_bool(),
                    self.engine.GetDuration(),
                    self.engine.GetCurrentTime(),
                    self.engine.IsPaused().as_bool(),
                    self.engine.IsEnded().as_bool(),
                )
            };
            let mut new_frame = false;
            if has_video
                // SAFETY: обычный вызов; S_FALSE (нового кадра нет) тоже Ok — сравниваем время.
                && let Ok(pts) = unsafe { self.engine.OnVideoStreamTick() }
                && pts != last_pts
                && let Some(frame) = self.frame(max_side)
            {
                last_pts = pts;
                if let Ok(mut slot) = shared.frame.lock() {
                    *slot = Some(frame);
                }
                new_frame = true;
            }
            if let Ok(mut state) = shared.state.lock() {
                state.ready = duration.is_finite() || has_video || has_audio;
                state.duration = if duration.is_finite() { duration } else { 0.0 };
                state.position = if position.is_finite() { position } else { 0.0 };
                state.paused = paused;
                state.ended = ended;
                state.has_video = has_video;
                state.has_audio = has_audio;
                state.volume = volume;
            }
            if new_frame || last_clock.elapsed() >= CLOCK_EVERY {
                last_clock = Instant::now();
                waker();
            }
        }
        // SAFETY: движок больше не используется.
        let _ = unsafe { self.engine.Shutdown() };
    }

    /// Текущий кадр, вписанный в `max_side`.
    fn frame(&mut self, max_side: u32) -> Option<Frame> {
        let (mut width, mut height) = (0u32, 0u32);
        // SAFETY: обычный вызов движка.
        unsafe { self.engine.GetNativeVideoSize(Some(&mut width), Some(&mut height)) }.ok()?;
        if width == 0 || height == 0 {
            return None;
        }
        let scale = (max_side as f64 / width.max(height) as f64).min(1.0);
        let (width, height) =
            (((width as f64 * scale) as u32).max(2), ((height as f64 * scale) as u32).max(2));
        if !self.target.as_ref().is_some_and(|(w, h, ..)| *w == width && *h == height) {
            self.target = Some((
                width,
                height,
                self.texture(width, height, false)?,
                self.texture(width, height, true)?,
            ));
        }
        let (_, _, target, staging) = self.target.as_ref()?;
        let rect = RECT { left: 0, top: 0, right: width as i32, bottom: height as i32 };
        // SAFETY: текстуры созданы этим устройством и живут в `self`; отображение снимается
        // до выхода из блока.
        unsafe {
            self.engine.TransferVideoFrame(target, None, &rect, None).ok()?;
            self.context.CopyResource(staging, target);
            let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
            self.context.Map(staging, 0, D3D11_MAP_READ, 0, Some(&mut mapped)).ok()?;
            let mut rgba = Vec::with_capacity((width * height * 4) as usize);
            for row in 0..height as usize {
                let line = std::slice::from_raw_parts(
                    (mapped.pData as *const u8).add(row * mapped.RowPitch as usize),
                    width as usize * 4,
                );
                for pixel in line.as_chunks::<4>().0 {
                    rgba.extend_from_slice(&[pixel[2], pixel[1], pixel[0], 255]);
                }
            }
            self.context.Unmap(staging, 0);
            Some(Frame { width, height, rgba })
        }
    }

    fn texture(&self, width: u32, height: u32, staging: bool) -> Option<ID3D11Texture2D> {
        let desc = D3D11_TEXTURE2D_DESC {
            Width: width,
            Height: height,
            MipLevels: 1,
            ArraySize: 1,
            Format: DXGI_FORMAT_B8G8R8A8_UNORM,
            SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
            Usage: if staging { D3D11_USAGE_STAGING } else { D3D11_USAGE_DEFAULT },
            BindFlags: if staging { 0 } else { D3D11_BIND_RENDER_TARGET.0 as u32 },
            CPUAccessFlags: if staging { D3D11_CPU_ACCESS_READ.0 as u32 } else { 0 },
            MiscFlags: 0,
        };
        let mut texture = None;
        // SAFETY: описание живёт до конца вызова.
        unsafe { self.device.CreateTexture2D(&desc, None, Some(&mut texture)) }.ok()?;
        texture
    }
}

/// Устройство D3D11 с поддержкой видео: видеокарта, а без неё — программный WARP.
fn create_device() -> Result<(ID3D11Device, ID3D11DeviceContext), String> {
    let mut last = String::new();
    for driver in [D3D_DRIVER_TYPE_HARDWARE, D3D_DRIVER_TYPE_WARP] {
        let (mut device, mut context) = (None, None);
        // SAFETY: выходные параметры — локальные переменные.
        let created = unsafe {
            D3D11CreateDevice(
                None,
                driver,
                HMODULE::default(),
                D3D11_CREATE_DEVICE_VIDEO_SUPPORT | D3D11_CREATE_DEVICE_BGRA_SUPPORT,
                None,
                D3D11_SDK_VERSION,
                Some(&mut device),
                None,
                Some(&mut context),
            )
        };
        match (created, device, context) {
            (Ok(()), Some(device), Some(context)) => return Ok((device, context)),
            (Err(error), ..) => last = error.to_string(),
            _ => {}
        }
    }
    Err(format!("нет устройства Direct3D для видео: {last}"))
}

fn text(error: windows::core::Error) -> String {
    format!("файл не открылся: {error}")
}

/// MF_MEDIA_ENGINE_ERR_*: 1 — прервано, 2 — сеть, 3 — декодирование, 4 — формат.
fn describe_error(code: usize, hresult: u32) -> String {
    let what = match code {
        2 => "файл недоступен",
        3 => "не удалось декодировать — файл повреждён?",
        4 => "формат не поддерживается Windows: нет кодека (HEVC, AV1 — из Microsoft Store)",
        _ => "воспроизведение прервано",
    };
    format!("{what} (0x{hresult:08X})")
}

/// Разложить звук файла для эквалайзера: Source Reader отдаёт отсчёты float, анализатор
/// копит кадры спектра в `shared.spectrum` пачками. Свой поток — MTA со своей парой
/// `MFStartup`/`MFShutdown` (счётчик у Media Foundation общий на процесс).
pub fn spectrum(path: &Path, shared: &Arc<Shared>) {
    // SAFETY: парный CoUninitialize — в конце этой же функции.
    let com = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) }.is_ok();
    // SAFETY: парный MFShutdown — в конце.
    if unsafe { MFStartup(MF_VERSION, MFSTARTUP_LITE) }.is_ok() {
        // Не вышло (нет кодека, защищённый файл) — эквалайзера просто не будет.
        let _ = decode_spectrum(path, shared);
        // SAFETY: парный вызов к успешному MFStartup.
        let _ = unsafe { MFShutdown() };
    }
    if let Ok(mut spectrum) = shared.spectrum.lock() {
        spectrum.done = true;
    }
    if com {
        // SAFETY: парный вызов к успешному CoInitializeEx в этом потоке.
        unsafe { CoUninitialize() };
    }
}

fn decode_spectrum(path: &Path, shared: &Arc<Shared>) -> windows::core::Result<()> {
    use std::sync::atomic::Ordering;

    use windows::Win32::Media::MediaFoundation::{
        MF_MT_AUDIO_NUM_CHANNELS, MF_MT_AUDIO_SAMPLES_PER_SECOND, MF_MT_MAJOR_TYPE, MF_MT_SUBTYPE,
        MF_SOURCE_READER_ALL_STREAMS, MF_SOURCE_READER_FIRST_AUDIO_STREAM,
        MF_SOURCE_READERF_ENDOFSTREAM, MFAudioFormat_Float, MFCreateMediaType,
        MFCreateSourceReaderFromURL, MFMediaType_Audio,
    };
    use windows::core::HSTRING;

    use crate::spectrum::Analyzer;

    // SAFETY: обычные вызовы Media Foundation; все объекты живут до конца функции, буфер
    // отсчётов читается только между Lock и Unlock.
    unsafe {
        let reader = MFCreateSourceReaderFromURL(&HSTRING::from(path.as_os_str()), None)?;
        let audio = MF_SOURCE_READER_FIRST_AUDIO_STREAM.0 as u32;
        reader.SetStreamSelection(MF_SOURCE_READER_ALL_STREAMS.0 as u32, false)?;
        reader.SetStreamSelection(audio, true)?;
        let wanted = MFCreateMediaType()?;
        wanted.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Audio)?;
        wanted.SetGUID(&MF_MT_SUBTYPE, &MFAudioFormat_Float)?;
        reader.SetCurrentMediaType(audio, None, &wanted)?;
        let actual = reader.GetCurrentMediaType(audio)?;
        let rate = actual.GetUINT32(&MF_MT_AUDIO_SAMPLES_PER_SECOND)?;
        let channels = actual.GetUINT32(&MF_MT_AUDIO_NUM_CHANNELS)?;
        let mut analyzer = Analyzer::new(rate, channels);
        if let Ok(mut spectrum) = shared.spectrum.lock() {
            spectrum.rate = analyzer.frame_rate();
        }
        let mut pending = Vec::new();
        loop {
            if shared.stop.load(Ordering::Relaxed) {
                return Ok(());
            }
            let mut flags = 0u32;
            let mut sample = None;
            reader.ReadSample(audio, 0, None, Some(&mut flags), None, Some(&mut sample))?;
            if flags & MF_SOURCE_READERF_ENDOFSTREAM.0 as u32 != 0 {
                break;
            }
            let Some(sample) = sample else { continue };
            let buffer = sample.ConvertToContiguousBuffer()?;
            let mut data = std::ptr::null_mut();
            let mut length = 0u32;
            buffer.Lock(&mut data, None, Some(&mut length))?;
            let samples =
                std::slice::from_raw_parts(data as *const f32, length as usize / size_of::<f32>());
            let more = analyzer.push(samples, &mut pending);
            buffer.Unlock()?;
            // Пачками: окно берёт замок каждый кадр.
            if pending.len() >= 64 * crate::spectrum::BANDS || !more {
                if let Ok(mut spectrum) = shared.spectrum.lock() {
                    spectrum.data.append(&mut pending);
                }
                if !more {
                    break;
                }
            }
        }
        if let Ok(mut spectrum) = shared.spectrum.lock() {
            spectrum.data.append(&mut pending);
        }
    }
    Ok(())
}
