//! Всё, что зависит от Windows: Shell, COM, Win32. Остальной код вызывает только этот API.
//!
//! Правила (PLAN.md §4):
//! * функции здесь **блокирующие** и могут ждать диск, сеть или чужое расширение Shell —
//!   вызывать их только из фоновых потоков, никогда из потока UI;
//! * ни одна ошибка Windows не должна ронять процесс: всё возвращается как `Result<_, String>`
//!   с текстом для пользователя;
//! * вне Windows работают упрощённые заглушки — ровно настолько, чтобы программу можно было
//!   запускать и проверять на Linux. Дистрибутив только под Windows.

use std::sync::Arc;

pub mod clipboard;
pub mod dnd;
pub mod drives;
pub mod folders;
pub mod instance;
pub mod integration;
pub mod media;
pub mod net;
pub mod ops;
pub mod pdf;
pub mod preview_handler;
pub mod shell;
pub mod thumbs;
pub mod volume;
pub mod watch;
pub mod window;

#[cfg(windows)]
mod win;

/// Будит поток UI, когда фоновая работа что-то положила в канал.
pub type Waker = Arc<dyn Fn() + Send + Sync>;

/// Будильник, который никого не будит, — для тестов и консольных примеров.
pub fn no_waker() -> Waker {
    Arc::new(|| {})
}
