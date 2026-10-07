//! Замеры для PLAN.md §10: сколько от запуска до первого кадра и сколько читалась каждая
//! открытая папка. Видны в «Настройки → Система» и копируются одной кнопкой — так снимаются
//! цифры на «холодном» HDD, в сети и на огромных папках.

use std::collections::VecDeque;
use std::path::PathBuf;
use std::time::{Duration, Instant};

/// Сколько последних папок помнить.
const KEEP: usize = 40;

pub struct Diagnostics {
    pub started: Instant,
    pub first_frame: Option<Duration>,
    /// Папка, сколько записей, сколько читалась.
    pub listings: VecDeque<(PathBuf, usize, Duration)>,
}

impl Diagnostics {
    pub fn new(started: Instant) -> Diagnostics {
        Diagnostics { started, first_frame: None, listings: VecDeque::new() }
    }

    pub fn frame(&mut self) {
        if self.first_frame.is_none() {
            self.first_frame = Some(self.started.elapsed());
        }
    }

    pub fn listing(&mut self, dir: PathBuf, entries: usize, took: Duration) {
        self.listings.push_front((dir, entries, took));
        self.listings.truncate(KEEP);
    }

    /// Отчёт текстом — для буфера обмена.
    pub fn report(&self) -> String {
        let mut text = format!(
            "MH Files {} · {}\nЗапуск до первого кадра: {}\n\nПапки (последние первыми):\n",
            env!("CARGO_PKG_VERSION"),
            std::env::consts::OS,
            self.first_frame.map_or("—".to_string(), |d| format!("{} мс", d.as_millis()))
        );
        for (dir, entries, took) in &self.listings {
            text.push_str(&format!("{} мс\t{entries}\t{}\n", took.as_millis(), dir.display()));
        }
        text
    }
}
