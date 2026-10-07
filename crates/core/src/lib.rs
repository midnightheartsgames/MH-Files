//! Модель MH Files без ввода-вывода: всё здесь проверяется обычными тестами.
//!
//! Файловая система, Shell и поток UI сюда не заходят. Воркеры (`mh-files-fs`) приносят
//! [`Entry`], интерфейс (`mh-files`) показывает [`Listing`] и меняет его командами.

pub mod cli;
pub mod duplicates;
pub mod entry;
pub mod filter;
pub mod format;
pub mod fuzzy;
pub mod goto;
pub mod history;
pub mod index;
pub mod layout;
pub mod listing;
pub mod location;
pub mod names;
pub mod rename;
pub mod selection;
pub mod session;
pub mod settings;
pub mod sort;
pub mod sorting;

pub use entry::{Attributes, Entry, EntryKind};
pub use listing::Listing;
pub use location::Location;
pub use selection::Selection;
