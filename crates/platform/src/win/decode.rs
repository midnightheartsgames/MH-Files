//! Source Reader Media Foundation: отрезки звука (float, моно) и отдельные кадры видео (RGB32
//! через встроенный преобразователь) по времени. Те же кодеки, что у проигрывателя.

use std::path::Path;

use windows::Win32::Media::MediaFoundation::{
    IMF2DBuffer, IMFAttributes, IMFMediaBuffer, IMFSample, IMFSourceReader,
    MF_MT_AUDIO_NUM_CHANNELS, MF_MT_AUDIO_SAMPLES_PER_SECOND, MF_MT_DEFAULT_STRIDE,
    MF_MT_FRAME_SIZE, MF_MT_MAJOR_TYPE, MF_MT_MINIMUM_DISPLAY_APERTURE, MF_MT_SUBTYPE,
    MF_MT_VIDEO_ROTATION, MF_PD_DURATION, MF_SOURCE_READER_ALL_STREAMS,
    MF_SOURCE_READER_ENABLE_VIDEO_PROCESSING, MF_SOURCE_READER_FIRST_AUDIO_STREAM,
    MF_SOURCE_READER_FIRST_VIDEO_STREAM, MF_SOURCE_READER_MEDIASOURCE,
    MF_SOURCE_READERF_ENDOFSTREAM, MF_VERSION, MFAudioFormat_Float, MFCreateAttributes,
    MFCreateMediaType, MFCreateSourceReaderFromURL, MFMediaType_Audio, MFMediaType_Video,
    MFSTARTUP_LITE, MFShutdown, MFStartup, MFVideoFormat_RGB32,
};
use windows::Win32::System::Com::StructuredStorage::PROPVARIANT;
use windows::core::{GUID, HSTRING, Interface};

use crate::decode::Sound;
use crate::thumbs::Bitmap;

/// Больше стольких вызовов `ReadSample` на один кусок — файл ведёт себя странно: хватит.
const MAX_READS: usize = 20_000;
/// Кадров, которые декодируются после перехода, чтобы подойти к нужному времени.
const MAX_SKIP_FRAMES: usize = 90;

/// `MFStartup` на время жизни читателя (счётчик у Media Foundation общий на процесс).
struct Startup;

impl Startup {
    fn new() -> Result<Startup, String> {
        // SAFETY: парный MFShutdown — в Drop.
        unsafe { MFStartup(MF_VERSION, MFSTARTUP_LITE) }
            .map_err(|e| format!("Media Foundation недоступна: {e}"))?;
        Ok(Startup)
    }
}

impl Drop for Startup {
    fn drop(&mut self) {
        // SAFETY: парный вызов к успешному MFStartup.
        let _ = unsafe { MFShutdown() };
    }
}

fn text(error: windows::core::Error) -> String {
    error.message().to_string().trim().to_string() + &format!(" (0x{:08X})", error.code().0)
}

/// Длительность файла, секунды.
fn duration(reader: &IMFSourceReader) -> Option<f64> {
    // SAFETY: обычный вызов; PROPVARIANT освобождается своим Drop.
    let value = unsafe {
        reader.GetPresentationAttribute(MF_SOURCE_READER_MEDIASOURCE.0 as u32, &MF_PD_DURATION)
    }
    .ok()?;
    let ticks = u64::try_from(&value).ok()?;
    (ticks > 0).then(|| ticks as f64 / 1e7)
}

/// Перейти к секунде `at`. Source Reader встаёт на ближайшую точку перехода до неё.
fn seek(reader: &IMFSourceReader, at: f64) -> Result<(), String> {
    let position = PROPVARIANT::from((at.max(0.0) * 1e7) as i64);
    // SAFETY: обычный вызов; GUID_NULL — время в единицах по 100 нс.
    unsafe { reader.SetCurrentPosition(&GUID::zeroed(), &position) }.map_err(text)
}

/// Следующий кусок потока: (время начала в секундах, кусок); `None` — поток кончился.
fn read(reader: &IMFSourceReader, stream: u32) -> Result<Option<(f64, IMFSample)>, String> {
    for _ in 0..MAX_READS {
        let mut flags = 0u32;
        let mut timestamp = 0i64;
        let mut sample: Option<IMFSample> = None;
        // SAFETY: выходные параметры — локальные переменные.
        unsafe {
            reader.ReadSample(
                stream,
                0,
                None,
                Some(&mut flags),
                Some(&mut timestamp),
                Some(&mut sample),
            )
        }
        .map_err(text)?;
        if flags & MF_SOURCE_READERF_ENDOFSTREAM.0 as u32 != 0 {
            return Ok(None);
        }
        if let Some(sample) = sample {
            return Ok(Some((timestamp as f64 / 1e7, sample)));
        }
    }
    Err("файл не отдаёт данные".into())
}

/// Звук файла кусками.
pub struct AudioReader {
    // Порядок полей — порядок уничтожения: читатель раньше MFShutdown.
    reader: IMFSourceReader,
    stream: u32,
    rate: u32,
    channels: usize,
    duration: Option<f64>,
    _startup: Startup,
}

impl AudioReader {
    pub fn open(path: &Path) -> Result<AudioReader, String> {
        let startup = Startup::new()?;
        // SAFETY: обычные вызовы Media Foundation; объекты живут в возвращаемом читателе.
        unsafe {
            let reader = MFCreateSourceReaderFromURL(&HSTRING::from(path.as_os_str()), None)
                .map_err(text)?;
            let stream = MF_SOURCE_READER_FIRST_AUDIO_STREAM.0 as u32;
            reader
                .SetStreamSelection(MF_SOURCE_READER_ALL_STREAMS.0 as u32, false)
                .map_err(text)?;
            reader.SetStreamSelection(stream, true).map_err(|_| "звука в файле нет".to_string())?;
            let wanted = MFCreateMediaType().map_err(text)?;
            wanted.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Audio).map_err(text)?;
            wanted.SetGUID(&MF_MT_SUBTYPE, &MFAudioFormat_Float).map_err(text)?;
            reader.SetCurrentMediaType(stream, None, &wanted).map_err(text)?;
            let actual = reader.GetCurrentMediaType(stream).map_err(text)?;
            let rate = actual.GetUINT32(&MF_MT_AUDIO_SAMPLES_PER_SECOND).map_err(text)?;
            let channels = actual.GetUINT32(&MF_MT_AUDIO_NUM_CHANNELS).map_err(text)? as usize;
            if rate == 0 || channels == 0 {
                return Err("странный формат звука".into());
            }
            let duration = duration(&reader);
            Ok(AudioReader { reader, stream, rate, channels, duration, _startup: startup })
        }
    }

    pub fn duration(&self) -> Option<f64> {
        self.duration
    }

    /// `seconds` секунд звука с секунды `start`, моно (среднее каналов).
    pub fn clip(&mut self, start: f64, seconds: f64) -> Result<Sound, String> {
        seek(&self.reader, start)?;
        let need = (seconds * f64::from(self.rate)).ceil() as usize;
        let mut samples = Vec::with_capacity(need);
        while samples.len() < need {
            let Some((time, sample)) = read(&self.reader, self.stream)? else { break };
            // Переход встаёт раньше нужного места: лишнее в начале — мимо.
            let mut skip = if time < start {
                ((start - time) * f64::from(self.rate)).round() as usize
            } else {
                0
            };
            with_bytes(&sample, |bytes| {
                let (floats, _) = bytes.as_chunks::<4>();
                for frame in floats.chunks_exact(self.channels) {
                    if skip > 0 {
                        skip -= 1;
                        continue;
                    }
                    if samples.len() >= need {
                        break;
                    }
                    let sum: f32 = frame.iter().map(|b| f32::from_le_bytes(*b)).sum();
                    samples.push(sum / self.channels as f32);
                }
            })?;
        }
        Ok(Sound { rate: self.rate, samples })
    }
}

/// Байты куска на время `use_bytes`.
fn with_bytes(sample: &IMFSample, use_bytes: impl FnOnce(&[u8])) -> Result<(), String> {
    // SAFETY: буфер заперт, пока читаются его байты; срез не переживает Unlock.
    unsafe {
        let buffer: IMFMediaBuffer = sample.ConvertToContiguousBuffer().map_err(text)?;
        let mut data = std::ptr::null_mut();
        let mut length = 0u32;
        buffer.Lock(&mut data, None, Some(&mut length)).map_err(text)?;
        if !data.is_null() {
            use_bytes(std::slice::from_raw_parts(data, length as usize));
        }
        buffer.Unlock().map_err(text)
    }
}

/// Кадры видео по времени.
pub struct VideoReader {
    reader: IMFSourceReader,
    stream: u32,
    width: u32,
    height: u32,
    /// Видимая часть кадра: (x, y, ширина, высота) — у H.264 1080p кадр бывает 1088.
    visible: (u32, u32, u32, u32),
    stride: i32,
    /// Поворот при показе, градусы по часовой (видео с телефона).
    rotation: u32,
    duration: Option<f64>,
    _startup: Startup,
}

impl VideoReader {
    pub fn open(path: &Path) -> Result<VideoReader, String> {
        let startup = Startup::new()?;
        // SAFETY: обычные вызовы Media Foundation; объекты живут в возвращаемом читателе.
        unsafe {
            let mut attributes: Option<IMFAttributes> = None;
            MFCreateAttributes(&mut attributes, 1).map_err(text)?;
            let attributes = attributes.ok_or("нет атрибутов")?;
            // Преобразование цвета в RGB32 — силами Source Reader.
            attributes.SetUINT32(&MF_SOURCE_READER_ENABLE_VIDEO_PROCESSING, 1).map_err(text)?;
            let reader = MFCreateSourceReaderFromURL(&HSTRING::from(path.as_os_str()), &attributes)
                .map_err(text)?;
            let stream = MF_SOURCE_READER_FIRST_VIDEO_STREAM.0 as u32;
            reader
                .SetStreamSelection(MF_SOURCE_READER_ALL_STREAMS.0 as u32, false)
                .map_err(text)?;
            reader.SetStreamSelection(stream, true).map_err(|_| "видео в файле нет".to_string())?;
            let rotation = reader
                .GetNativeMediaType(stream, 0)
                .ok()
                .and_then(|native| native.GetUINT32(&MF_MT_VIDEO_ROTATION).ok())
                .unwrap_or(0)
                % 360;
            let wanted = MFCreateMediaType().map_err(text)?;
            wanted.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video).map_err(text)?;
            wanted.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_RGB32).map_err(text)?;
            reader
                .SetCurrentMediaType(stream, None, &wanted)
                .map_err(|e| format!("кадры не декодируются (нет кодека?): {}", text(e)))?;
            let actual = reader.GetCurrentMediaType(stream).map_err(text)?;
            let size = actual.GetUINT64(&MF_MT_FRAME_SIZE).map_err(text)?;
            let (width, height) = ((size >> 32) as u32, size as u32);
            if width == 0 || height == 0 {
                return Err("размер кадра неизвестен".into());
            }
            let stride =
                actual.GetUINT32(&MF_MT_DEFAULT_STRIDE).map_or(width as i32 * 4, |s| s as i32);
            // MFVideoArea: OffsetX и OffsetY (дробная часть u16, целая i16), затем SIZE.
            let mut area = [0u8; 16];
            let visible = actual
                .GetBlob(&MF_MT_MINIMUM_DISPLAY_APERTURE, &mut area, None)
                .ok()
                .map(|()| {
                    let x = i16::from_le_bytes([area[2], area[3]]).max(0) as u32;
                    let y = i16::from_le_bytes([area[6], area[7]]).max(0) as u32;
                    let w =
                        i32::from_le_bytes([area[8], area[9], area[10], area[11]]).max(0) as u32;
                    let h =
                        i32::from_le_bytes([area[12], area[13], area[14], area[15]]).max(0) as u32;
                    (x, y, w, h)
                })
                .filter(|&(x, y, w, h)| w > 0 && h > 0 && x + w <= width && y + h <= height)
                .unwrap_or((0, 0, width, height));
            let duration = duration(&reader);
            Ok(VideoReader {
                reader,
                stream,
                width,
                height,
                visible,
                stride,
                rotation,
                duration,
                _startup: startup,
            })
        }
    }

    pub fn duration(&self) -> Option<f64> {
        self.duration
    }

    /// Кадр около секунды `at`: после перехода декодируется вперёд, пока не дойдёт до неё
    /// (не дальше [`MAX_SKIP_FRAMES`] кадров), — быстро даже у видео с редкими ключевыми
    /// кадрами. Конец файла раньше — последний кадр.
    pub fn frame(&mut self, at: f64) -> Result<Bitmap, String> {
        seek(&self.reader, at)?;
        let mut last = None;
        for _ in 0..MAX_SKIP_FRAMES {
            let Some((time, sample)) = read(&self.reader, self.stream)? else { break };
            let reached = time + 0.05 >= at;
            last = Some(sample);
            if reached {
                break;
            }
        }
        let sample = last.ok_or("кадра нет")?;
        let bitmap = self.pixels(&sample)?;
        Ok(crate::decode::rotate(bitmap, self.rotation))
    }

    /// RGB32 (B, G, R, X) куска → RGBA видимой части.
    fn pixels(&self, sample: &IMFSample) -> Result<Bitmap, String> {
        let (x0, y0, width, height) = self.visible;
        let mut rgba = Vec::with_capacity((width * height * 4) as usize);
        let mut copy = |base: *const u8, pitch: i32| {
            for row in y0..y0 + height {
                // SAFETY: строка `row` лежит внутри запертого буфера: pitch и размер кадра —
                // из его же формата.
                let line = unsafe {
                    std::slice::from_raw_parts(
                        base.offset(row as isize * pitch as isize).add(x0 as usize * 4),
                        width as usize * 4,
                    )
                };
                for pixel in line.as_chunks::<4>().0 {
                    rgba.extend_from_slice(&[pixel[2], pixel[1], pixel[0], 255]);
                }
            }
        };
        // SAFETY: буфер заперт на время копирования.
        unsafe {
            let buffer: IMFMediaBuffer = sample.ConvertToContiguousBuffer().map_err(text)?;
            if let Ok(buffer2d) = buffer.cast::<IMF2DBuffer>() {
                let mut scanline = std::ptr::null_mut();
                let mut pitch = 0i32;
                buffer2d.Lock2D(&mut scanline, &mut pitch).map_err(text)?;
                if !scanline.is_null() && pitch.unsigned_abs() >= self.width * 4 {
                    copy(scanline, pitch);
                }
                buffer2d.Unlock2D().map_err(text)?;
            } else {
                let mut data = std::ptr::null_mut();
                let mut length = 0u32;
                buffer.Lock(&mut data, None, Some(&mut length)).map_err(text)?;
                let need = self.stride.unsigned_abs() as usize * self.height as usize;
                if !data.is_null() && length as usize >= need {
                    // Отрицательный шаг — строки снизу вверх: первая строка картинки в конце.
                    let base = if self.stride < 0 {
                        data.add((self.height as usize - 1) * self.stride.unsigned_abs() as usize)
                    } else {
                        data
                    };
                    copy(base, self.stride);
                }
                buffer.Unlock().map_err(text)?;
            }
        }
        if rgba.len() != (width * height * 4) as usize {
            return Err("кадр не читается".into());
        }
        Ok(Bitmap { width, height, rgba })
    }
}
