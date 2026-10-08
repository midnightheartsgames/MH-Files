//! Раскладка панелей — дерево разбиений, как в File Pilot: одна панель, две рядом, две одна
//! над другой, три, четыре… Не «левая + правая» навсегда.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct PaneId(pub u64);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SplitDirection {
    /// Панели рядом, разделитель вертикальный.
    Horizontal,
    /// Панели одна над другой.
    Vertical,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum LayoutNode {
    Pane(PaneId),
    Split {
        direction: SplitDirection,
        /// Доля первой части, 0.1..=0.9.
        ratio: f32,
        first: Box<LayoutNode>,
        second: Box<LayoutNode>,
    },
}

pub const MIN_RATIO: f32 = 0.1;
pub const MAX_RATIO: f32 = 0.9;

impl LayoutNode {
    /// Панели слева направо и сверху вниз — порядок обхода по Tab.
    pub fn panes(&self) -> Vec<PaneId> {
        let mut panes = Vec::new();
        self.collect(&mut panes);
        panes
    }

    fn collect(&self, out: &mut Vec<PaneId>) {
        match self {
            LayoutNode::Pane(id) => out.push(*id),
            LayoutNode::Split { first, second, .. } => {
                first.collect(out);
                second.collect(out);
            }
        }
    }

    pub fn contains(&self, pane: PaneId) -> bool {
        match self {
            LayoutNode::Pane(id) => *id == pane,
            LayoutNode::Split { first, second, .. } => {
                first.contains(pane) || second.contains(pane)
            }
        }
    }

    /// Делит панель `target` пополам; новая встаёт второй. `false` — такой панели нет.
    pub fn split(&mut self, target: PaneId, direction: SplitDirection, new: PaneId) -> bool {
        self.split_at(target, direction, new, false)
    }

    /// То же, но новая панель может встать первой (слева или сверху) — бросок вкладки в
    /// левый или верхний край панели.
    pub fn split_at(
        &mut self,
        target: PaneId,
        direction: SplitDirection,
        new: PaneId,
        new_first: bool,
    ) -> bool {
        match self {
            LayoutNode::Pane(id) if *id == target => {
                let (a, b) = if new_first { (new, target) } else { (target, new) };
                *self = LayoutNode::Split {
                    direction,
                    ratio: 0.5,
                    first: Box::new(LayoutNode::Pane(a)),
                    second: Box::new(LayoutNode::Pane(b)),
                };
                true
            }
            LayoutNode::Pane(_) => false,
            LayoutNode::Split { first, second, .. } => {
                first.split_at(target, direction, new, new_first)
                    || second.split_at(target, direction, new, new_first)
            }
        }
    }

    /// Новая панель во весь край раскладки: слева или справа (`Horizontal`), сверху или
    /// снизу (`Vertical`) от всех остальных. Доля — как у одной колонки: при одной панели
    /// пополам, при двух — треть.
    pub fn split_edge(&mut self, direction: SplitDirection, new: PaneId, new_first: bool) {
        let share = 1.0 / (self.columns(direction) + 1) as f32;
        let rest = std::mem::replace(self, LayoutNode::Pane(new));
        let (first, second, ratio) = if new_first {
            (LayoutNode::Pane(new), rest, share)
        } else {
            (rest, LayoutNode::Pane(new), 1.0 - share)
        };
        *self = LayoutNode::Split {
            direction,
            ratio: ratio.clamp(MIN_RATIO, MAX_RATIO),
            first: Box::new(first),
            second: Box::new(second),
        };
    }

    /// Сколько панелей встаёт в ряд по `direction` на верхнем уровне.
    pub fn columns(&self, direction: SplitDirection) -> usize {
        match self {
            LayoutNode::Split { direction: d, first, second, .. } if *d == direction => {
                first.columns(direction) + second.columns(direction)
            }
            _ => 1,
        }
    }

    /// Убирает панель; её соседка занимает освободившееся место. Последнюю панель убрать
    /// нельзя — `false`.
    pub fn remove(&mut self, target: PaneId) -> bool {
        match self {
            LayoutNode::Pane(_) => false,
            LayoutNode::Split { first, second, .. } => {
                if **first == LayoutNode::Pane(target) {
                    *self = (**second).clone();
                    true
                } else if **second == LayoutNode::Pane(target) {
                    *self = (**first).clone();
                    true
                } else {
                    first.remove(target) || second.remove(target)
                }
            }
        }
    }

    /// Ближайшая панель в обходе после `pane` (по кругу).
    pub fn next_pane(&self, pane: PaneId, backwards: bool) -> PaneId {
        let panes = self.panes();
        let Some(i) = panes.iter().position(|&p| p == pane) else { return panes[0] };
        let n = panes.len();
        if backwards { panes[(i + n - 1) % n] } else { panes[(i + 1) % n] }
    }

    /// Приводит дерево в порядок после загрузки: доли в допустимых границах.
    pub fn sanitize(&mut self) {
        if let LayoutNode::Split { ratio, first, second, .. } = self {
            *ratio = if ratio.is_finite() { ratio.clamp(MIN_RATIO, MAX_RATIO) } else { 0.5 };
            first.sanitize();
            second.sanitize();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_and_remove() {
        let mut layout = LayoutNode::Pane(PaneId(1));
        assert!(layout.split(PaneId(1), SplitDirection::Horizontal, PaneId(2)));
        assert!(layout.split(PaneId(2), SplitDirection::Vertical, PaneId(3)));
        assert_eq!(layout.panes(), [PaneId(1), PaneId(2), PaneId(3)]);
        assert_eq!(layout.next_pane(PaneId(3), false), PaneId(1));
        assert_eq!(layout.next_pane(PaneId(1), true), PaneId(3));
        assert!(layout.remove(PaneId(2)));
        assert_eq!(layout.panes(), [PaneId(1), PaneId(3)]);
        assert!(layout.remove(PaneId(1)));
        assert_eq!(layout, LayoutNode::Pane(PaneId(3)));
        assert!(!layout.remove(PaneId(3)), "последняя панель остаётся");
        assert!(!layout.split(PaneId(9), SplitDirection::Vertical, PaneId(10)));
        assert!(layout.split_at(PaneId(3), SplitDirection::Horizontal, PaneId(4), true));
        assert_eq!(layout.panes(), [PaneId(4), PaneId(3)], "новая панель слева");
    }

    #[test]
    fn split_edge_spans_the_whole_side() {
        let mut layout = LayoutNode::Pane(PaneId(1));
        layout.split_edge(SplitDirection::Horizontal, PaneId(2), false);
        let LayoutNode::Split { ratio, .. } = &layout else { panic!() };
        assert_eq!(*ratio, 0.5);
        assert_eq!(layout.panes(), [PaneId(1), PaneId(2)]);
        // Слева от обеих: треть ширины, остальные две — справа, как были.
        layout.split_edge(SplitDirection::Horizontal, PaneId(3), true);
        assert_eq!(layout.panes(), [PaneId(3), PaneId(1), PaneId(2)]);
        let LayoutNode::Split { ratio, second, .. } = &layout else { panic!() };
        assert!((*ratio - 1.0 / 3.0).abs() < 1e-6);
        assert!(matches!(&**second, LayoutNode::Split { .. }));
        // Справа от панелей одна над другой — пополам.
        let mut stacked = LayoutNode::Pane(PaneId(1));
        stacked.split(PaneId(1), SplitDirection::Vertical, PaneId(2));
        stacked.split_edge(SplitDirection::Horizontal, PaneId(3), false);
        let LayoutNode::Split { ratio, .. } = &stacked else { panic!() };
        assert_eq!(*ratio, 0.5);
    }

    #[test]
    fn sanitize_clamps_ratios() {
        let mut layout = LayoutNode::Split {
            direction: SplitDirection::Horizontal,
            ratio: f32::NAN,
            first: Box::new(LayoutNode::Pane(PaneId(1))),
            second: Box::new(LayoutNode::Split {
                direction: SplitDirection::Vertical,
                ratio: 5.0,
                first: Box::new(LayoutNode::Pane(PaneId(2))),
                second: Box::new(LayoutNode::Pane(PaneId(3))),
            }),
        };
        layout.sanitize();
        let LayoutNode::Split { ratio, second, .. } = &layout else { panic!() };
        assert_eq!(*ratio, 0.5);
        let LayoutNode::Split { ratio, .. } = &**second else { panic!() };
        assert_eq!(*ratio, MAX_RATIO);
    }
}
