//! Воспроизведение видео и звука в быстром просмотре. В Windows — Media Foundation (Media
//! Engine: те же кодеки, что у «Кино и ТВ»), кадры уменьшаются на видеокарте и отдаются
//! картинкой RGBA; звук идёт сам. Всё — в своём потоке: окно только рисует последний кадр.
//! Вне Windows воспроизведения нет — состояние сразу с ошибкой.

use std::path::Path;
use std::sync::{Arc, Mutex};

use crossbeam_channel::{Sender, unbounded};

use crate::Waker;

/// Кадр видео, уже уменьшенный до нужного размера.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Frame {
    pub width: u32,
    pub height: u32,
    /// RGBA построчно, без отступов.
    pub rgba: Vec<u8>,
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
    pub error: Option<String>,
}

#[cfg_attr(not(windows), allow(dead_code))]
pub(crate) enum Command {
    Play,
    Pause,
    Seek(f64),
    Volume(f64),
}

#[derive(Default)]
pub(crate) struct Shared {
    pub(crate) state: Mutex<PlayerState>,
    pub(crate) frame: Mutex<Option<Frame>>,
}

/// Проигрыватель одного файла. Останавливается и освобождает всё при уничтожении.
pub struct Player {
    shared: Arc<Shared>,
    commands: Sender<Command>,
}

impl Player {
    /// Открыть и сразу начать воспроизведение. `max_side` — предел стороны кадра в пикселях.
    pub fn open(path: &Path, max_side: u32, waker: Waker) -> Player {
        let shared = Arc::new(Shared::default());
        let (commands, receiver) = unbounded();
        #[cfg(windows)]
        {
            let (path, thread_shared) = (path.to_path_buf(), shared.clone());
            let started = std::thread::Builder::new().name("player".into()).spawn(move || {
                crate::win::player::run(&path, max_side, &thread_shared, &receiver, &waker)
            });
            if let Err(error) = started {
                shared.state.lock().unwrap().error = Some(error.to_string());
            }
        }
        #[cfg(not(windows))]
        {
            let _ = (path, max_side, waker, receiver);
            shared.state.lock().unwrap().error =
                Some("воспроизведение есть только в Windows".into());
        }
        Player { shared, commands }
    }

    pub fn state(&self) -> PlayerState {
        self.shared.state.lock().map(|state| state.clone()).unwrap_or_default()
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
}
