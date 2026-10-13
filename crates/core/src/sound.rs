//! Звук для автотегов (1.3.3): что слышно в записи — словами из онтологии AudioSet («speech»,
//! «music», «dog», «rain»).
//!
//! Модель — Audio Spectrogram Transformer (MIT, BSD-3), дообученный на AudioSet, в ONNX от
//! Xenova: на входе мел-спектрограмма 1024 кадра × 128 полос (10,24 с звука 16 кГц), на выходе
//! уверенность каждого из 527 классов. Здесь — подготовка входа (как `ASTFeatureExtractor`:
//! Kaldi fbank), словарь классов и выбор отрезков; без ввода-вывода.

use crate::index::snapshot::fnv;

/// Репозиторий модели на Hugging Face.
pub const SOUND_REPO: &str = "Xenova/ast-finetuned-audioset-10-10-0.4593";
/// Классы модели (`id2label`).
pub const SOUND_CONFIG: &str = "config.json";

/// Варианты модели: файл в репозитории, название, размер.
pub const SOUND_MODELS: [SoundModel; 2] = [
    SoundModel {
        file: "onnx/model.onnx", title: "AST — точнее, для видеокарты", size: "350 МБ"
    },
    SoundModel {
        file: "onnx/model_quantized.onnx",
        title: "AST int8 — быстрее на процессоре",
        size: "90 МБ",
    },
];

pub const DEFAULT_SOUND_MODEL: &str = SOUND_MODELS[0].file;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SoundModel {
    pub file: &'static str,
    pub title: &'static str,
    pub size: &'static str,
}

pub fn sound_model(file: &str) -> Option<&'static SoundModel> {
    SOUND_MODELS.iter().find(|m| m.file == file)
}

/// Частота звука на входе модели.
pub const SAMPLE_RATE: u32 = 16_000;
/// Полос мел-спектрограммы.
pub const MELS: usize = 128;
/// Кадров спектрограммы на входе модели.
pub const FRAMES: usize = 1024;
/// Окно кадра (25 мс) и шаг (10 мс) в отсчётах.
const WINDOW: usize = 400;
const HOP: usize = 160;
const FFT: usize = 512;
/// Отсчётов на один отрезок: ровно [`FRAMES`] кадров.
pub const CLIP_SAMPLES: usize = WINDOW + (FRAMES - 1) * HOP;
/// Длина отрезка в секундах (10,255 с).
pub const CLIP_SECONDS: f64 = CLIP_SAMPLES as f64 / SAMPLE_RATE as f64;
/// Нормировка `ASTFeatureExtractor`: (x − среднее) / (2 · σ).
const MEAN: f32 = -4.267_739_3;
const STD: f32 = 4.568_997_4;

/// Ниже этой уверенности звуковые теги не хранятся: у AudioSet уверенность ниже, чем у
/// картинок, — собака на записи часто 0,1–0,2.
pub const SOUND_FLOOR: f32 = 0.05;

/// Откуда брать отрезки записи длиной `duration` секунд: не больше `count`, равномерно по
/// длине (середины частей), чтобы песня с тихим вступлением не судилась по вступлению.
pub fn clip_starts(duration: f64, count: usize) -> Vec<f64> {
    if duration.is_nan() || duration <= CLIP_SECONDS {
        return vec![0.0];
    }
    let count = count.max(1).min((duration / CLIP_SECONDS).ceil() as usize).max(1);
    let span = duration - CLIP_SECONDS;
    (0..count).map(|i| span * (i as f64 + 0.5) / count as f64).collect()
}

/// Звук к другой частоте: sinc с окном Блэкмана, срез чуть ниже новой частоты Найквиста.
pub fn resample(input: &[f32], from: u32, to: u32) -> Vec<f32> {
    if from == to || input.is_empty() || from == 0 || to == 0 {
        return input.to_vec();
    }
    /// Нулей sinc по каждую сторону и точность таблицы ядра (долей отсчёта).
    const ZEROS: f64 = 16.0;
    const STEPS: f64 = 256.0;
    let ratio = f64::from(to) / f64::from(from);
    let cutoff = ratio.min(1.0) * 0.97;
    let half = ZEROS / cutoff;
    let table: Vec<f64> = (0..=(half * STEPS).ceil() as usize + 1)
        .map(|i| {
            let x = i as f64 / STEPS;
            if x > half {
                return 0.0;
            }
            let t = x / half;
            let window = 0.42
                + 0.5 * (std::f64::consts::PI * t).cos()
                + 0.08 * (2.0 * std::f64::consts::PI * t).cos();
            let arg = std::f64::consts::PI * cutoff * x;
            let sinc = if arg.abs() < 1e-12 { 1.0 } else { arg.sin() / arg };
            cutoff * sinc * window
        })
        .collect();
    let kernel = |x: f64| {
        let at = x.abs() * STEPS;
        let i = at as usize;
        if i + 1 >= table.len() {
            return 0.0;
        }
        let frac = at - i as f64;
        table[i] * (1.0 - frac) + table[i + 1] * frac
    };
    let out_len = (input.len() as f64 * ratio).floor() as usize;
    let last = input.len() - 1;
    (0..out_len)
        .map(|n| {
            let center = n as f64 / ratio;
            let first = (center - half).ceil().max(0.0) as usize;
            let end = ((center + half).floor().max(0.0) as usize).min(last);
            let mut sum = 0.0;
            for (k, &sample) in input.iter().enumerate().take(end + 1).skip(first) {
                sum += f64::from(sample) * kernel(k as f64 - center);
            }
            sum as f32
        })
        .collect()
}

/// Мел-спектрограмма для модели — как `ASTFeatureExtractor` (torchaudio `kaldi.fbank`: окно
/// Ханна, предыскажение 0,97, без постоянной составляющей, логарифм энергии полос), до
/// [`FRAMES`] кадров с нулями в конце и нормировкой. `samples` — моно 16 кГц, −1..1.
/// Выход — `FRAMES × MELS` построчно.
pub fn features(samples: &[f32]) -> Vec<f32> {
    let banks = mel_banks();
    let fft = Fft::new(FFT);
    let window: Vec<f64> = (0..WINDOW)
        .map(|n| 0.5 - 0.5 * (2.0 * std::f64::consts::PI * n as f64 / (WINDOW - 1) as f64).cos())
        .collect();
    let frames = if samples.len() >= WINDOW { 1 + (samples.len() - WINDOW) / HOP } else { 0 };
    let mut out = vec![0.0f32; FRAMES * MELS];
    let mut re = vec![0.0f64; FFT];
    let mut im = vec![0.0f64; FFT];
    let mut power = vec![0.0f64; FFT / 2 + 1];
    for frame in 0..frames.min(FRAMES) {
        let chunk = &samples[frame * HOP..frame * HOP + WINDOW];
        let mean = chunk.iter().map(|&s| f64::from(s)).sum::<f64>() / WINDOW as f64;
        re.fill(0.0);
        im.fill(0.0);
        let mut previous = f64::from(chunk[0]) - mean;
        for (i, &sample) in chunk.iter().enumerate() {
            let value = f64::from(sample) - mean;
            re[i] = (value - 0.97 * previous) * window[i];
            previous = value;
        }
        fft.run(&mut re, &mut im);
        for (bin, p) in power.iter_mut().enumerate() {
            *p = re[bin] * re[bin] + im[bin] * im[bin];
        }
        let row = &mut out[frame * MELS..(frame + 1) * MELS];
        for (mel, (start, weights)) in banks.iter().enumerate() {
            let energy: f64 = weights.iter().zip(&power[*start..]).map(|(w, p)| w * p).sum();
            row[mel] = energy.max(f64::from(f32::EPSILON)).ln() as f32;
        }
    }
    for value in &mut out {
        *value = (*value - MEAN) / (2.0 * STD);
    }
    out
}

/// Треугольные полосы Kaldi в мел-шкале (1127·ln(1 + f/700)) от 20 Гц до 8 кГц: для каждой —
/// первый бин БПФ и веса.
fn mel_banks() -> Vec<(usize, Vec<f64>)> {
    let mel = |f: f64| 1127.0 * (1.0 + f / 700.0).ln();
    let (low, high) = (mel(20.0), mel(f64::from(SAMPLE_RATE) / 2.0));
    let delta = (high - low) / (MELS + 1) as f64;
    let bin_width = f64::from(SAMPLE_RATE) / FFT as f64;
    (0..MELS)
        .map(|bank| {
            let left = low + bank as f64 * delta;
            let center = left + delta;
            let right = center + delta;
            let weights: Vec<(usize, f64)> = (0..FFT / 2)
                .filter_map(|bin| {
                    let m = mel(bin_width * bin as f64);
                    let up = (m - left) / (center - left);
                    let down = (right - m) / (right - center);
                    let weight = up.min(down);
                    (weight > 0.0).then_some((bin, weight))
                })
                .collect();
            let start = weights.first().map_or(0, |&(bin, _)| bin);
            (start, weights.into_iter().map(|(_, w)| w).collect())
        })
        .collect()
}

/// Быстрое преобразование Фурье по основанию 2.
struct Fft {
    cos: Vec<f64>,
    sin: Vec<f64>,
    reverse: Vec<usize>,
}

impl Fft {
    fn new(n: usize) -> Fft {
        let bits = n.trailing_zeros();
        let reverse = (0..n).map(|i| i.reverse_bits() >> (usize::BITS - bits)).collect();
        let angle = |k: usize| -2.0 * std::f64::consts::PI * k as f64 / n as f64;
        Fft {
            cos: (0..n / 2).map(|k| angle(k).cos()).collect(),
            sin: (0..n / 2).map(|k| angle(k).sin()).collect(),
            reverse,
        }
    }

    fn run(&self, re: &mut [f64], im: &mut [f64]) {
        let n = re.len();
        for i in 0..n {
            let j = self.reverse[i];
            if j > i {
                re.swap(i, j);
                im.swap(i, j);
            }
        }
        let mut size = 2;
        while size <= n {
            let step = n / size;
            for start in (0..n).step_by(size) {
                for k in 0..size / 2 {
                    let (c, s) = (self.cos[k * step], self.sin[k * step]);
                    let (a, b) = (start + k, start + k + size / 2);
                    let tr = re[b] * c - im[b] * s;
                    let ti = re[b] * s + im[b] * c;
                    re[b] = re[a] - tr;
                    im[b] = im[a] - ti;
                    re[a] += tr;
                    im[a] += ti;
                }
            }
            size *= 2;
        }
    }
}

/// Классы модели: имена для показа по номеру выхода.
#[derive(Debug, Clone, PartialEq)]
pub struct SoundVocabulary {
    names: Vec<String>,
    fingerprint: u64,
}

impl SoundVocabulary {
    /// Из `config.json` модели (`id2label`: `"0": "Speech"`).
    pub fn parse(config: &str) -> Result<SoundVocabulary, String> {
        let value: serde_json::Value =
            serde_json::from_str(config).map_err(|e| format!("config.json не читается: {e}"))?;
        let labels = value
            .get("id2label")
            .and_then(|v| v.as_object())
            .ok_or("в config.json нет id2label")?;
        let mut raw = vec![String::new(); labels.len()];
        for (id, label) in labels {
            let index: usize = id.parse().map_err(|_| format!("странный номер класса {id}"))?;
            let slot = raw.get_mut(index).ok_or(format!("номер класса {id} вне списка"))?;
            *slot = label.as_str().unwrap_or_default().to_string();
        }
        if raw.is_empty() || raw.iter().any(String::is_empty) {
            return Err("список классов звука неполный".into());
        }
        Ok(SoundVocabulary::from_labels(&raw))
    }

    /// Из меток AudioSet: «Male speech, man speaking» → «male speech» (первая часть, без
    /// заглавных); одинаковые первые части — с уточнением: «inside (small room)».
    pub fn from_labels(labels: &[String]) -> SoundVocabulary {
        let first = |label: &str| label.split(',').next().unwrap_or(label).trim().to_lowercase();
        let names = labels
            .iter()
            .map(|label| {
                let short = first(label);
                let shared = labels.iter().filter(|other| first(other) == short).count() > 1;
                match label.split_once(',') {
                    Some((_, rest)) if shared => {
                        format!("{short} ({})", rest.trim().to_lowercase())
                    }
                    _ => short,
                }
            })
            .collect();
        SoundVocabulary { names, fingerprint: fnv(labels.join("\n").as_bytes()) }
    }

    pub fn len(&self) -> usize {
        self.names.len()
    }

    pub fn is_empty(&self) -> bool {
        self.names.is_empty()
    }

    pub fn fingerprint(&self) -> u64 {
        self.fingerprint
    }

    pub fn name(&self, index: usize) -> &str {
        self.names.get(index).map_or("", String::as_str)
    }

    pub fn names(&self) -> &[String] {
        &self.names
    }
}

/// Уверенность классов по нескольким отрезкам: для каждого класса — наибольшая (событие на
/// одном отрезке — тоже событие записи). Выход модели — логиты: сигмоида здесь.
pub fn combine_clips(clips: &[Vec<f32>]) -> Vec<f32> {
    let len = clips.iter().map(Vec::len).max().unwrap_or(0);
    let mut best = vec![0.0f32; len];
    for clip in clips {
        for (slot, &logit) in best.iter_mut().zip(clip) {
            let p = if logit.is_finite() { 1.0 / (1.0 + (-logit).exp()) } else { 0.0 };
            *slot = slot.max(p);
        }
    }
    best
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clips_spread_over_the_record() {
        assert_eq!(clip_starts(0.0, 3), vec![0.0], "длина неизвестна — с начала");
        assert_eq!(clip_starts(5.0, 3), vec![0.0], "короче отрезка — один");
        let starts = clip_starts(300.0, 3);
        assert_eq!(starts.len(), 3);
        assert!((starts[1] - (300.0 - CLIP_SECONDS) / 2.0).abs() < 1e-9, "середина");
        assert!(starts.iter().all(|&s| s >= 0.0 && s + CLIP_SECONDS <= 300.0));
        assert_eq!(clip_starts(15.0, 5).len(), 2, "не больше, чем помещается");
        assert_eq!(CLIP_SAMPLES, 164_080);
    }

    #[test]
    fn resampling_keeps_tones_and_drops_aliases() {
        let tone = |freq: f64, rate: u32, seconds: f64| -> Vec<f32> {
            (0..(f64::from(rate) * seconds) as usize)
                .map(|i| {
                    (2.0 * std::f64::consts::PI * freq * i as f64 / f64::from(rate)).sin() as f32
                })
                .collect()
        };
        let rms = |x: &[f32]| {
            let middle = &x[x.len() / 4..x.len() * 3 / 4];
            (middle.iter().map(|v| f64::from(*v).powi(2)).sum::<f64>() / middle.len() as f64).sqrt()
        };
        let down = resample(&tone(1000.0, 44_100, 1.0), 44_100, 16_000);
        assert_eq!(down.len(), 16_000);
        assert!(
            (rms(&down) - std::f64::consts::FRAC_1_SQRT_2).abs() < 0.01,
            "1 кГц проходит: {}",
            rms(&down)
        );
        // Сравнение с идеальным тоном той же частоты.
        let ideal = tone(1000.0, 16_000, 1.0);
        let error = down[4000..12_000]
            .iter()
            .zip(&ideal[4000..12_000])
            .map(|(a, b)| (a - b).abs())
            .fold(0.0f32, f32::max);
        assert!(error < 0.01, "форма тона: {error}");
        // 12 кГц выше новой частоты Найквиста — исчезает, а не отражается.
        let alias = resample(&tone(12_000.0, 44_100, 1.0), 44_100, 16_000);
        assert!(rms(&alias) < 0.01, "наложение: {}", rms(&alias));
        let up = resample(&tone(440.0, 8000, 0.5), 8000, 16_000);
        assert_eq!(up.len(), 8000);
        assert!((rms(&up) - std::f64::consts::FRAC_1_SQRT_2).abs() < 0.01);
    }

    #[test]
    fn features_are_kaldi_fbank() {
        // Тон 1 кГц: энергия — в полосе около 1 кГц, тишина за пределами записи — нули до
        // нормировки.
        let samples: Vec<f32> = (0..16_000)
            .map(|i| {
                (0.5 * (2.0 * std::f64::consts::PI * 1000.0 * i as f64 / 16_000.0).sin()) as f32
            })
            .collect();
        let out = features(&samples);
        assert_eq!(out.len(), FRAMES * MELS);
        let row = &out[10 * MELS..11 * MELS];
        let loudest = (0..MELS).max_by(|&a, &b| row[a].total_cmp(&row[b])).unwrap();
        let mel = |f: f64| 1127.0 * (1.0 + f / 700.0).ln();
        let expected = ((mel(1000.0) - mel(20.0)) / ((mel(8000.0) - mel(20.0)) / 129.0)) - 1.0;
        assert!((loudest as f64 - expected).abs() <= 1.5, "{loudest} ≈ {expected}");
        let frames = 1 + (16_000 - WINDOW) / HOP;
        let padding = (0.0 - MEAN) / (2.0 * STD);
        assert!(out[frames * MELS..].iter().all(|&v| (v - padding).abs() < 1e-6));
        assert!(out[(frames - 1) * MELS] != padding);
    }

    #[test]
    fn audioset_names() {
        let labels: Vec<String> = [
            "Speech",
            "Male speech, man speaking",
            "Inside, small room",
            "Inside, large room or hall",
            "Hip hop music",
        ]
        .map(String::from)
        .to_vec();
        let v = SoundVocabulary::from_labels(&labels);
        assert_eq!(
            v.names(),
            [
                "speech",
                "male speech",
                "inside (small room)",
                "inside (large room or hall)",
                "hip hop music"
            ]
        );
        let config = r#"{"id2label": {"1": "Music", "0": "Speech"}, "other": 1}"#;
        let parsed = SoundVocabulary::parse(config).unwrap();
        assert_eq!(parsed.names(), ["speech", "music"]);
        assert!(SoundVocabulary::parse(r#"{"id2label": {"0": "A", "2": "B"}}"#).is_err());
        let combined = combine_clips(&[vec![0.0, -10.0], vec![-2.0, 2.0]]);
        assert!((combined[0] - 0.5).abs() < 1e-6 && (combined[1] - 0.8808).abs() < 1e-3);
    }
}
