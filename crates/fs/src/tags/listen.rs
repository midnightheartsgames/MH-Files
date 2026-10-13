//! Звук файла для модели звука: отрезки по 10 секунд, равномерно по записи, — в
//! спектрограммы. Звук читает Media Foundation (любой формат, для которого в Windows есть
//! кодек, и звуковая дорожка видео); WAV читается и без неё — на любой ОС.

use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

use mh_files_core::sound::{self, CLIP_SECONDS, SAMPLE_RATE};
use mh_files_platform::decode::{AudioReader, Sound};

/// Спектрограммы отрезков записи (не больше `count`). Звука нет или он не читается — ошибка.
pub fn clips(path: &Path, ext: &str, count: usize) -> Result<Vec<Vec<f32>>, String> {
    let mut source: Box<dyn Source> = match AudioReader::open(path) {
        Ok(reader) => Box::new(reader),
        Err(error) if ext == "wav" => {
            Box::new(Wav::open(path).map_err(|e| format!("{error}; {e}"))?)
        }
        Err(error) => return Err(error),
    };
    let starts = sound::clip_starts(source.duration().unwrap_or(0.0), count);
    let mut out = Vec::with_capacity(starts.len());
    for start in starts {
        let clip = source.clip(start, CLIP_SECONDS)?;
        // Меньше 0,1 с — не запись, а щелчок: модель о нём ничего не скажет.
        if clip.samples.len() < clip.rate as usize / 10 {
            continue;
        }
        let samples = sound::resample(&clip.samples, clip.rate, SAMPLE_RATE);
        out.push(sound::features(&samples));
    }
    if out.is_empty() {
        return Err("звука нет".into());
    }
    Ok(out)
}

trait Source {
    fn duration(&self) -> Option<f64>;
    fn clip(&mut self, start: f64, seconds: f64) -> Result<Sound, String>;
}

impl Source for AudioReader {
    fn duration(&self) -> Option<f64> {
        AudioReader::duration(self)
    }

    fn clip(&mut self, start: f64, seconds: f64) -> Result<Sound, String> {
        AudioReader::clip(self, start, seconds)
    }
}

/// WAV: PCM 8/16/24/32 бит или float 32, читается кусками с диска.
struct Wav {
    file: std::fs::File,
    float: bool,
    channels: usize,
    rate: u32,
    bytes_per_sample: usize,
    data_start: u64,
    data_len: u64,
}

impl Wav {
    fn open(path: &Path) -> Result<Wav, String> {
        let bad = || "это не WAV".to_string();
        let mut file = std::fs::File::open(path).map_err(|e| e.to_string())?;
        let mut head = [0u8; 12];
        file.read_exact(&mut head).map_err(|_| bad())?;
        if &head[..4] != b"RIFF" || &head[8..] != b"WAVE" {
            return Err(bad());
        }
        let mut format: Option<(u16, usize, u32, usize)> = None;
        loop {
            let mut chunk = [0u8; 8];
            file.read_exact(&mut chunk).map_err(|_| "в WAV нет звука".to_string())?;
            let len = u64::from(u32::from_le_bytes([chunk[4], chunk[5], chunk[6], chunk[7]]));
            match &chunk[..4] {
                b"fmt " => {
                    let mut body = vec![0u8; len.min(64) as usize];
                    file.read_exact(&mut body).map_err(|_| bad())?;
                    if body.len() < 16 {
                        return Err(bad());
                    }
                    let mut tag = u16::from_le_bytes([body[0], body[1]]);
                    // WAVE_FORMAT_EXTENSIBLE: настоящий формат — в начале GUID подформата.
                    if tag == 0xFFFE && body.len() >= 26 {
                        tag = u16::from_le_bytes([body[24], body[25]]);
                    }
                    let channels = usize::from(u16::from_le_bytes([body[2], body[3]]));
                    let rate = u32::from_le_bytes([body[4], body[5], body[6], body[7]]);
                    let bits = usize::from(u16::from_le_bytes([body[14], body[15]]));
                    format = Some((tag, channels, rate, bits));
                    file.seek(SeekFrom::Current((len - body.len() as u64 + (len & 1)) as i64))
                        .map_err(|e| e.to_string())?;
                }
                b"data" => {
                    let (tag, channels, rate, bits) = format.ok_or_else(bad)?;
                    let float = match (tag, bits) {
                        (1, 8 | 16 | 24 | 32) => false,
                        (3, 32) => true,
                        _ => return Err(format!("WAV в формате {tag} ({bits} бит) не читается")),
                    };
                    if channels == 0 || rate == 0 {
                        return Err(bad());
                    }
                    let data_start = file.stream_position().map_err(|e| e.to_string())?;
                    let size = file.metadata().map_err(|e| e.to_string())?.len();
                    let data_len = len.min(size.saturating_sub(data_start));
                    return Ok(Wav {
                        file,
                        float,
                        channels,
                        rate,
                        bytes_per_sample: bits / 8,
                        data_start,
                        data_len,
                    });
                }
                _ => {
                    file.seek(SeekFrom::Current((len + (len & 1)) as i64))
                        .map_err(|e| e.to_string())?;
                }
            }
        }
    }

    fn frame_bytes(&self) -> usize {
        self.channels * self.bytes_per_sample
    }
}

impl Source for Wav {
    fn duration(&self) -> Option<f64> {
        Some(self.data_len as f64 / self.frame_bytes() as f64 / f64::from(self.rate))
    }

    fn clip(&mut self, start: f64, seconds: f64) -> Result<Sound, String> {
        let frame = self.frame_bytes() as u64;
        let total = self.data_len / frame;
        let first = ((start.max(0.0) * f64::from(self.rate)) as u64).min(total);
        let count = ((seconds * f64::from(self.rate)).ceil() as u64).min(total - first);
        self.file
            .seek(SeekFrom::Start(self.data_start + first * frame))
            .map_err(|e| e.to_string())?;
        let mut bytes = vec![0u8; (count * frame) as usize];
        self.file.read_exact(&mut bytes).map_err(|e| format!("WAV не читается: {e}"))?;
        let size = self.bytes_per_sample;
        let value = |b: &[u8]| -> f32 {
            match (self.float, size) {
                (true, _) => f32::from_le_bytes([b[0], b[1], b[2], b[3]]),
                (false, 1) => (f32::from(b[0]) - 128.0) / 128.0,
                (false, 2) => f32::from(i16::from_le_bytes([b[0], b[1]])) / 32768.0,
                (false, 3) => (i32::from_le_bytes([0, b[0], b[1], b[2]]) >> 8) as f32 / 8_388_608.0,
                _ => i32::from_le_bytes([b[0], b[1], b[2], b[3]]) as f32 / 2_147_483_648.0,
            }
        };
        let samples = bytes
            .chunks_exact(self.frame_bytes())
            .map(|frame| frame.chunks_exact(size).map(value).sum::<f32>() / self.channels as f32)
            .collect();
        Ok(Sound { rate: self.rate, samples })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// WAV 16 бит, стерео, с лишним блоком перед данными.
    fn wav(path: &Path, rate: u32, seconds: f64) {
        let frames = (f64::from(rate) * seconds) as usize;
        let mut data = Vec::new();
        for i in 0..frames {
            let v = (0.5
                * (2.0 * std::f64::consts::PI * 440.0 * i as f64 / f64::from(rate)).sin()
                * 32767.0) as i16;
            data.extend_from_slice(&v.to_le_bytes());
            data.extend_from_slice(&v.to_le_bytes());
        }
        let mut out = Vec::new();
        out.extend_from_slice(b"RIFF");
        out.extend_from_slice(&((4 + 26 + 24 + 8 + data.len()) as u32).to_le_bytes());
        out.extend_from_slice(b"WAVE");
        out.extend_from_slice(b"LIST");
        out.extend_from_slice(&5u32.to_le_bytes());
        out.extend_from_slice(b"abcde\0");
        out.extend_from_slice(b"fmt ");
        out.extend_from_slice(&16u32.to_le_bytes());
        out.extend_from_slice(&1u16.to_le_bytes());
        out.extend_from_slice(&2u16.to_le_bytes());
        out.extend_from_slice(&rate.to_le_bytes());
        out.extend_from_slice(&(rate * 4).to_le_bytes());
        out.extend_from_slice(&4u16.to_le_bytes());
        out.extend_from_slice(&16u16.to_le_bytes());
        out.extend_from_slice(b"data");
        out.extend_from_slice(&(data.len() as u32).to_le_bytes());
        out.extend_from_slice(&data);
        std::fs::write(path, out).unwrap();
    }

    #[test]
    fn reads_wav_clips() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("тон.wav");
        wav(&path, 22_050, 30.0);
        let mut reader = Wav::open(&path).unwrap();
        assert!((reader.duration().unwrap() - 30.0).abs() < 1e-6);
        let clip = reader.clip(29.0, 5.0).unwrap();
        assert_eq!(clip.samples.len(), 22_050, "конец записи");
        assert!(clip.samples.iter().all(|s| s.abs() <= 0.51));
        let clips = clips(&path, "wav", 3).unwrap();
        assert_eq!(clips.len(), 3);
        assert!(clips.iter().all(|c| c.len() == sound::FRAMES * sound::MELS));
        let short = dir.path().join("щелчок.wav");
        wav(&short, 8000, 0.05);
        assert!(super::clips(&short, "wav", 3).is_err());
        let text = dir.path().join("не звук.wav");
        std::fs::write(&text, "привет").unwrap();
        assert!(super::clips(&text, "wav", 3).is_err());
    }
}
