//! Реализация под Windows. Наружу не видна: остальной код ходит через модули верхнего уровня.

pub mod clipboard;
pub mod com;
pub mod dnd;
pub mod drives;
pub mod files;
pub mod folders;
pub mod instance;
pub mod integration;
pub mod locale;
pub mod media;
pub mod menu;
pub mod net;
pub mod ops;
pub mod pdf;
pub mod player;
pub mod preview_handler;
pub mod shell;
pub mod thumbs;
pub mod volume;
pub mod watch;
pub mod window;
