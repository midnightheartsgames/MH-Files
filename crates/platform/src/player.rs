//! Воспроизведение видео и звука в быстром просмотре. В Windows — Media Foundation (Media
//! Engine: те же кодеки, что у «Кино и ТВ»), кадры уменьшаются на видеокарте и отдаются
//! картинкой RGBA; звук идёт сам. Всё — в своём потоке: окно только рисует последний кадр.
//! Вне Windows воспроизведения нет — состояние сразу с ошибкой.

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use crossbeam_channel::{Sender, unbounded};

use crate::Waker;
use crate::spectrum::Spectrum;

/// Кадр видео, уже уменьшенный до нужного размера.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Frame {
    pub width: u32,
    pub height: u32,
    /// RGBA построчно, без отступов.
    pub rgba: Vec<u8>,
}

/// Звук с первого кадра: громкость 0..1, выключен ли он и скорость.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Sound {
    pub volume: f64,
    pub muted: bool,
    pub rate: f64,
}

/// Что сейчас с воспроизведением.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct PlayerState {
    /// Файл открыт, длительность и дорожки известны.
    pub ready: bool,
    /// Секунды; ноль — неизвестно (поток).
    pub duration: f64,
    pub position: f64,
    pub paused: bool,
    pub ended: bool,
    pub has_video: bool,
    pub has_audio: bool,
    pub volume: f64,
    pub muted: bool,
    /// Скорость воспроизведения (1 — обычная).
    pub rate: f64,
    pub error: Option<String>,
}

#[cfg_attr(not(windows), allow(dead_code))]
pub(crate) enum Command {
    Play,
    Pause,
    Seek(f64),
    Volume(f64),
    Muted(bool),
    Rate(f64),
}

#[derive(Default)]
pub(crate) struct Shared {
    pub(crate) state: Mutex<PlayerState>,
    pub(crate) frame: Mutex<Option<Frame>>,
    /// Спектр звука для эквалайзера — раскладывается своим потоком.
    pub(crate) spectrum: Mutex<Spectrum>,
    /// Проигрыватель закрыт: фоновым потокам пора заканчивать.
    pub(crate) stop: AtomicBool,
}

/// Проигрыватель одного файла. Останавливается и освобождает всё при уничтожении.
pub struct Player {
    shared: Arc<Shared>,
    commands: Sender<Command>,
}

impl Player {
    /// Открыть и сразу начать воспроизведение. `max_side` — предел стороны кадра в пикселях.
    /// `spectrum` — ещё и разложить звук для эквалайзера ([`Player::levels`]); `sound` —
    /// громкость, выключен ли звук и скорость — с первого звука.
    pub fn open(path: &Path, max_side: u32, spectrum: bool, sound: Sound, waker: Waker) -> Player {
        let shared = Arc::new(Shared::default());
        let (commands, receiver) = unbounded();
        #[cfg(windows)]
        {
            if spectrum {
                let (path, thread_shared) = (path.to_path_buf(), shared.clone());
                let _ = std::thread::Builder::new()
                    .name("player-spectrum".into())
                    .spawn(move || crate::win::player::spectrum(&path, &thread_shared));
            }
            let (path, thread_shared) = (path.to_path_buf(), shared.clone());
            let started = std::thread::Builder::new().name("player".into()).spawn(move || {
                crate::win::player::run(&path, max_side, sound, &thread_shared, &receiver, &waker)
            });
            if let Err(error) = started {
                shared.state.lock().unwrap().error = Some(error.to_string());
            }
        }
        #[cfg(not(windows))]
        {
            let _ = (path, max_side, spectrum);
            // Проверка интерфейса под Linux: часы без кадров и звука.
            if std::env::var_os("MH_FILES_FAKE_PLAYER").is_some() {
                let thread_shared = shared.clone();
                std::thread::spawn(move || fake(&thread_shared, sound, &receiver, &waker));
            } else {
                shared.state.lock().unwrap().error =
                    Some("воспроизведение есть только в Windows".into());
            }
        }
        Player { shared, commands }
    }

    pub fn state(&self) -> PlayerState {
        self.shared.state.lock().map(|state| state.clone()).unwrap_or_default()
    }

    /// Полосы эквалайзера (0..1, длиной [`crate::spectrum::BANDS`]) на секунде `seconds`;
    /// `false` — до этого места звук ещё не разложен или разложить не вышло.
    pub fn levels(&self, seconds: f64, out: &mut [f32]) -> bool {
        self.shared.spectrum.lock().is_ok_and(|spectrum| spectrum.levels(seconds, out))
    }

    /// Звук разложен до конца — больше данных не будет.
    pub fn spectrum_done(&self) -> bool {
        self.shared.spectrum.lock().is_ok_and(|spectrum| spectrum.done)
    }

    /// Новый кадр с прошлого вызова.
    pub fn take_frame(&self) -> Option<Frame> {
        self.shared.frame.lock().ok()?.take()
    }

    pub fn play(&self) {
        let _ = self.commands.send(Command::Play);
    }

    pub fn pause(&self) {
        let _ = self.commands.send(Command::Pause);
    }

    /// Перейти к секунде `seconds`.
    pub fn seek(&self, seconds: f64) {
        let _ = self.commands.send(Command::Seek(seconds.max(0.0)));
    }

    /// Громкость 0..1.
    pub fn set_volume(&self, volume: f64) {
        let _ = self.commands.send(Command::Volume(volume.clamp(0.0, 1.0)));
    }

    /// Выключить звук или вернуть.
    pub fn set_muted(&self, muted: bool) {
        let _ = self.commands.send(Command::Muted(muted));
    }

    /// Скорость воспроизведения: 0,25–4 (1 — обычная).
    pub fn set_rate(&self, rate: f64) {
        let _ = self.commands.send(Command::Rate(rate.clamp(0.25, 4.0)));
    }
}

/// Имитация воспроизведения для проверки интерфейса вне Windows: полторы минуты «видео».
#[cfg(not(windows))]
fn fake(
    shared: &Arc<Shared>,
    sound: Sound,
    commands: &crossbeam_channel::Receiver<Command>,
    waker: &Waker,
) {
    let mut state = PlayerState {
        ready: true,
        duration: 95.0,
        has_video: true,
        has_audio: true,
        volume: sound.volume,
        muted: sound.muted,
        rate: sound.rate,
        ..PlayerState::default()
    };
    let tick = std::time::Duration::from_millis(50);
    loop {
        match commands.recv_timeout(tick) {
            Ok(Command::Play) => {
                if state.ended {
                    state.position = 0.0;
                }
                state.paused = false;
            }
            Ok(Command::Pause) => state.paused = true,
            Ok(Command::Seek(at)) => state.position = at.min(state.duration),
            Ok(Command::Volume(volume)) => state.volume = volume,
            Ok(Command::Muted(muted)) => state.muted = muted,
            Ok(Command::Rate(rate)) => state.rate = rate,
            Err(crossbeam_channel::RecvTimeoutError::Timeout) => {}
            Err(crossbeam_channel::RecvTimeoutError::Disconnected) => return,
        }
        if !state.paused && !state.ended {
            state.position += tick.as_secs_f64() * state.rate;
        }
        state.ended = state.position >= state.duration;
        if state.ended {
            state.position = state.duration;
        }
        *shared.state.lock().unwrap() = state.clone();
        waker();
    }
}

impl Drop for Player {
    fn drop(&mut self) {
        // Поток проигрывателя кончается сам, когда закрывается канал команд; разбор спектра
        // смотрит на этот флаг.
        self.shared.stop.store(true, Ordering::Relaxed);
    }
}
