//! Сеанс: что восстановить после перезапуска — окно, раскладку, панели, вкладки.
//! Выделение не сохраняется: после изменений на диске оно только путает.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::layout::{LayoutNode, PaneId};
use crate::location::Location;
use crate::sort::SortOrder;

pub const SESSION_VERSION: u32 = 1;
/// Сколько недавних папок помнить.
pub const RECENT_LIMIT: usize = 64;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum ViewMode {
    #[default]
    Details,
    Grid,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TabSession {
    pub location: Location,
    #[serde(default)]
    pub view: ViewMode,
    #[serde(default)]
    pub sort: SortOrder,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PaneSession {
    pub id: PaneId,
    pub tabs: Vec<TabSession>,
    #[serde(default)]
    pub active: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct WindowGeometry {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
    #[serde(default)]
    pub maximized: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Session {
    pub version: u32,
    pub layout: LayoutNode,
    pub panes: Vec<PaneSession>,
    pub focused: PaneId,
    #[serde(default)]
    pub window: Option<WindowGeometry>,
    #[serde(default)]
    pub recent: Vec<PathBuf>,
}

impl Session {
    /// Одна панель с одной вкладкой.
    pub fn single(location: Location) -> Session {
        Session {
            version: SESSION_VERSION,
            layout: LayoutNode::Pane(PaneId(1)),
            panes: vec![PaneSession {
                id: PaneId(1),
                tabs: vec![TabSession {
                    location,
                    view: ViewMode::Details,
                    sort: SortOrder::default(),
                }],
                active: 0,
            }],
            focused: PaneId(1),
            window: None,
            recent: Vec::new(),
        }
    }

    pub fn parse(json: &str) -> Option<Session> {
        let session: Session = serde_json::from_str(json).ok()?;
        session.sanitized()
    }

    pub fn to_json(&self) -> String {
        serde_json::to_string_pretty(self).expect("сеанс сериализуется")
    }

    /// Согласует раскладку и панели. Несогласуемое — `None`, и сеанс начинается заново.
    pub fn sanitized(mut self) -> Option<Session> {
        self.layout.sanitize();
        let ids = self.layout.panes();
        self.panes.retain(|pane| ids.contains(&pane.id) && !pane.tabs.is_empty());
        if self.panes.len() != ids.len() {
            return None;
        }
        for pane in &mut self.panes {
            pane.active = pane.active.min(pane.tabs.len() - 1);
        }
        if !ids.contains(&self.focused) {
            self.focused = ids[0];
        }
        self.recent.truncate(RECENT_LIMIT);
        self.version = SESSION_VERSION;
        Some(self)
    }

    /// Запомнить посещённую папку первой в списке недавних.
    pub fn remember(&mut self, path: PathBuf) {
        remember(&mut self.recent, path);
    }
}

/// Добавляет путь в начало списка недавних без повторов.
pub fn remember(recent: &mut Vec<PathBuf>, path: PathBuf) {
    recent.retain(|p| p != &path);
    recent.insert(0, path);
    recent.truncate(RECENT_LIMIT);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout::SplitDirection;

    #[test]
    fn round_trip() {
        let mut session = Session::single(Location::Dir(PathBuf::from("/home")));
        session.layout.split(PaneId(1), SplitDirection::Horizontal, PaneId(2));
        session.panes.push(PaneSession {
            id: PaneId(2),
            tabs: vec![TabSession {
                location: Location::Computer,
                view: ViewMode::Grid,
                sort: SortOrder::default(),
            }],
            active: 5,
        });
        let parsed = Session::parse(&session.to_json()).unwrap();
        assert_eq!(parsed.panes[1].active, 0, "активная вкладка в границах");
        assert_eq!(parsed.layout.panes(), [PaneId(1), PaneId(2)]);
    }

    #[test]
    fn inconsistent_sessions_are_dropped() {
        let mut session = Session::single(Location::Computer);
        session.layout.split(PaneId(1), SplitDirection::Vertical, PaneId(7));
        assert!(session.sanitized().is_none(), "у панели 7 нет вкладок");
        assert!(Session::parse("garbage").is_none());
    }

    #[test]
    fn recent_is_mru() {
        let mut recent = Vec::new();
        remember(&mut recent, PathBuf::from("/a"));
        remember(&mut recent, PathBuf::from("/b"));
        remember(&mut recent, PathBuf::from("/a"));
        assert_eq!(recent, [PathBuf::from("/a"), PathBuf::from("/b")]);
    }
}
