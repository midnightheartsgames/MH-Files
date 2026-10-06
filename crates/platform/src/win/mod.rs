//! Реализация под Windows. Наружу не видна: остальной код ходит через модули верхнего уровня.

pub mod clipboard;
pub mod com;
pub mod drives;
pub mod folders;
pub mod ops;
pub mod shell;
pub mod thumbs;
pub mod watch;
pub mod window;
