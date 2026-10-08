//! Спектр звука для эквалайзера в быстром просмотре. Звук раскладывается заранее, в фоне
//! (см. [`crate::player`]): окно по времени воспроизведения только берёт готовые полосы.
//!
//! Моно с частотой около 20–24 кГц, окно Ханна на 1024 отсчёта с шагом 512 — около 43 кадров
//! в секунду; 48 полос с логарифмическим шагом от 40 Гц, громкость в дБ — байтом.

/// Отсчётов в окне БПФ.
const WINDOW: usize = 1024;
/// Шаг между кадрами.
const HOP: usize = 512;
/// Полос на кадр.
pub const BANDS: usize = 48;
/// Нижняя частота первой полосы, Гц.
const LOW: f32 = 40.0;
/// Тише этого — ноль, дБ относительно полной громкости.
const FLOOR_DB: f32 = -60.0;
/// Больше этого спектр не копится (около двух часов звука).
const LIMIT: usize = 16 << 20;

/// Разложенный звук: `bands` значений 0..255 на кадр, `rate` кадров в секунду.
#[derive(Debug, Clone, Default)]
pub struct Spectrum {
    pub rate: f32,
    pub data: Vec<u8>,
    /// Разложено до конца (или не вышло — тогда `data` пуст).
    pub done: bool,
}

impl Spectrum {
    /// Полосы (0..1) на секунде `seconds` в `out` (длиной [`BANDS`]); `false` — до этого места
    /// звук ещё не разложен.
    pub fn levels(&self, seconds: f64, out: &mut [f32]) -> bool {
        let frames = self.data.len() / BANDS;
        if self.rate <= 0.0 || frames == 0 || seconds < 0.0 {
            return false;
        }
        // Кадр описывает окно, а не его начало: середина окна — на полокна позже.
        let at = (seconds as f32 * self.rate - (WINDOW / 2) as f32 / HOP as f32).max(0.0);
        let first = at.floor() as usize;
        if first + 1 >= frames {
            if self.done && first < frames + 1 {
                out.iter_mut().for_each(|v| *v = 0.0);
                return frames > 0;
            }
            return false;
        }
        let t = at - first as f32;
        let (a, b) = (&self.data[first * BANDS..], &self.data[(first + 1) * BANDS..]);
        for (i, value) in out.iter_mut().take(BANDS).enumerate() {
            *value = (f32::from(a[i]) * (1.0 - t) + f32::from(b[i]) * t) / 255.0;
        }
        true
    }
}

/// Раскладывает поток отсчётов в кадры спектра.
pub struct Analyzer {
    channels: usize,
    decimate: usize,
    rate: f32,
    /// Накопленные для прореживания отсчёты.
    sum: f32,
    summed: usize,
    mono: Vec<f32>,
    window: Vec<f32>,
    /// Диапазон бинов БПФ каждой полосы.
    ranges: Vec<(usize, usize)>,
    re: Vec<f32>,
    im: Vec<f32>,
}

impl Analyzer {
    /// `sample_rate` и `channels` — как у входного звука.
    pub fn new(sample_rate: u32, channels: u32) -> Analyzer {
        let decimate = (sample_rate as usize / 20_000).max(1);
        let rate = sample_rate as f32 / decimate as f32;
        let window = (0..WINDOW)
            .map(|i| {
                let x = std::f32::consts::TAU * i as f32 / (WINDOW - 1) as f32;
                0.5 - 0.5 * x.cos()
            })
            .collect();
        let high = (rate / 2.0 * 0.95).clamp(LOW * 2.0, 16_000.0);
        let bin = rate / WINDOW as f32;
        let ranges = (0..BANDS)
            .map(|i| {
                let f0 = LOW * (high / LOW).powf(i as f32 / BANDS as f32);
                let f1 = LOW * (high / LOW).powf((i + 1) as f32 / BANDS as f32);
                let k0 = ((f0 / bin).round() as usize).clamp(1, WINDOW / 2 - 1);
                let k1 = ((f1 / bin).round() as usize).clamp(k0, WINDOW / 2 - 1);
                (k0, k1)
            })
            .collect();
        Analyzer {
            channels: channels.max(1) as usize,
            decimate,
            rate,
            sum: 0.0,
            summed: 0,
            mono: Vec::with_capacity(WINDOW * 2),
            window,
            ranges,
            re: vec![0.0; WINDOW],
            im: vec![0.0; WINDOW],
        }
    }

    /// Кадров спектра в секунду.
    pub fn frame_rate(&self) -> f32 {
        self.rate / HOP as f32
    }

    /// Отсчёты (каналы вперемешку) — в кадры, дописываются в `out`. `false` — предел
    /// размера спектра достигнут, дальше раскладывать незачем.
    pub fn push(&mut self, samples: &[f32], out: &mut Vec<u8>) -> bool {
        for frame in samples.chunks(self.channels) {
            let value = frame.iter().sum::<f32>() / frame.len() as f32;
            self.sum += value;
            self.summed += 1;
            if self.summed < self.decimate {
                continue;
            }
            self.mono.push(self.sum / self.summed as f32);
            self.sum = 0.0;
            self.summed = 0;
            if self.mono.len() >= WINDOW {
                self.frame(out);
                self.mono.drain(..HOP);
                if out.len() >= LIMIT {
                    return false;
                }
            }
        }
        true
    }

    fn frame(&mut self, out: &mut Vec<u8>) {
        for i in 0..WINDOW {
            self.re[i] = self.mono[i] * self.window[i];
            self.im[i] = 0.0;
        }
        fft(&mut self.re, &mut self.im);
        // Синус полной громкости в окне Ханна даёт пик около WINDOW/4.
        let full = WINDOW as f32 / 4.0;
        for (band, &(k0, k1)) in self.ranges.iter().enumerate() {
            let peak = (k0..=k1)
                .map(|k| (self.re[k] * self.re[k] + self.im[k] * self.im[k]).sqrt())
                .fold(0.0f32, f32::max);
            // Высокие частоты в музыке тише: лёгкий подъём к верху, чтобы правые полосы жили.
            let tilt = 9.0 * band as f32 / (BANDS - 1) as f32;
            let db = 20.0 * (peak / full + 1e-9).log10() + tilt;
            let level = ((db - FLOOR_DB) / -FLOOR_DB).clamp(0.0, 1.0);
            out.push((level * 255.0).round() as u8);
        }
    }
}

/// Быстрое преобразование Фурье на месте, длина — степень двойки.
fn fft(re: &mut [f32], im: &mut [f32]) {
    let n = re.len();
    let mut j = 0;
    for i in 1..n {
        let mut bit = n >> 1;
        while j & bit != 0 {
            j ^= bit;
            bit >>= 1;
        }
        j |= bit;
        if i < j {
            re.swap(i, j);
            im.swap(i, j);
        }
    }
    let mut len = 2;
    while len <= n {
        let angle = -std::f32::consts::TAU / len as f32;
        let (w_re, w_im) = (angle.cos(), angle.sin());
        for start in (0..n).step_by(len) {
            let (mut c_re, mut c_im) = (1.0f32, 0.0f32);
            for k in 0..len / 2 {
                let (a, b) = (start + k, start + k + len / 2);
                let t_re = re[b] * c_re - im[b] * c_im;
                let t_im = re[b] * c_im + im[b] * c_re;
                re[b] = re[a] - t_re;
                im[b] = im[a] - t_im;
                re[a] += t_re;
                im[a] += t_im;
                (c_re, c_im) = (c_re * w_re - c_im * w_im, c_re * w_im + c_im * w_re);
            }
        }
        len <<= 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sine(freq: f32, rate: u32, seconds: f32, channels: usize) -> Vec<f32> {
        let count = (rate as f32 * seconds) as usize;
        (0..count)
            .flat_map(|i| {
                let v = (std::f32::consts::TAU * freq * i as f32 / rate as f32).sin();
                std::iter::repeat_n(v, channels)
            })
            .collect()
    }

    #[test]
    fn fft_finds_the_tone() {
        let mut re: Vec<f32> =
            (0..64).map(|i| (std::f32::consts::TAU * 5.0 * i as f32 / 64.0).cos()).collect();
        let mut im = vec![0.0; 64];
        fft(&mut re, &mut im);
        let peak = (0..32).max_by(|&a, &b| re[a].abs().total_cmp(&re[b].abs())).unwrap();
        assert_eq!(peak, 5);
        assert!((re[5] - 32.0).abs() < 1e-3);
    }

    #[test]
    fn tone_lights_its_band_only() {
        let mut analyzer = Analyzer::new(44_100, 2);
        assert!((analyzer.frame_rate() - 22_050.0 / 512.0).abs() < 0.01);
        let mut data = Vec::new();
        assert!(analyzer.push(&sine(1000.0, 44_100, 1.0, 2), &mut data));
        assert_eq!(data.len() % BANDS, 0);
        let frames = data.len() / BANDS;
        assert!(frames > 30, "{frames}");
        let last = &data[(frames - 1) * BANDS..];
        let loudest = (0..BANDS).max_by_key(|&i| last[i]).unwrap();
        let (k0, k1) = analyzer.ranges[loudest];
        let bin = analyzer.rate / WINDOW as f32;
        assert!((k0 as f32 - 1.0) * bin <= 1000.0 && 1000.0 <= (k1 as f32 + 1.0) * bin);
        assert!(last[loudest] > 230, "громкий тон почти в полную шкалу: {}", last[loudest]);
        assert!(last[0] < 60 && last[BANDS - 1] < 60, "края тихие: {last:?}");
    }

    #[test]
    fn silence_is_zero_and_levels_follow_time() {
        let mut analyzer = Analyzer::new(48_000, 1);
        let mut data = Vec::new();
        analyzer.push(&vec![0.0; 48_000], &mut data);
        assert!(data.iter().all(|&v| v == 0));
        let spectrum = Spectrum { rate: analyzer.frame_rate(), data, done: false };
        let mut out = [1.0; BANDS];
        assert!(spectrum.levels(0.5, &mut out));
        assert!(out.iter().all(|&v| v == 0.0));
        assert!(!spectrum.levels(5.0, &mut out), "дальше разложенного — нет данных");
    }
}
