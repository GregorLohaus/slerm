use egui::Rect;

pub type PaneId = u64;

#[derive(Clone, Copy, Debug)]
pub enum Axis {
    Horizontal,
    Vertical,
}

#[derive(Clone, Copy)]
pub enum Direction {
    Left,
    Down,
    Up,
    Right,
}

#[derive(Debug)]
pub enum Layout {
    Pane(PaneId),
    Split {
        axis: Axis,
        ratio: f32,
        first: Box<Layout>,
        second: Box<Layout>,
    },
}

impl Layout {
    pub fn split(&mut self, target: PaneId, new: PaneId, axis: Axis) -> bool {
        match self {
            Self::Pane(id) if *id == target => {
                *self = Self::Split {
                    axis,
                    ratio: 0.5,
                    first: Box::new(Self::Pane(target)),
                    second: Box::new(Self::Pane(new)),
                };
                true
            }
            Self::Split { first, second, .. } => {
                first.split(target, new, axis) || second.split(target, new, axis)
            }
            _ => false,
        }
    }

    pub fn remove(self, target: PaneId) -> Option<Self> {
        match self {
            Self::Pane(id) => (id != target).then_some(Self::Pane(id)),
            Self::Split {
                axis,
                ratio,
                first,
                second,
            } => match (first.remove(target), second.remove(target)) {
                (Some(first), Some(second)) => Some(Self::Split {
                    axis,
                    ratio,
                    first: Box::new(first),
                    second: Box::new(second),
                }),
                (first, second) => first.or(second),
            },
        }
    }

    pub fn ids(&self, out: &mut Vec<PaneId>) {
        match self {
            Self::Pane(id) => out.push(*id),
            Self::Split { first, second, .. } => {
                first.ids(out);
                second.ids(out);
            }
        }
    }

    pub fn first(&self) -> PaneId {
        match self {
            Self::Pane(id) => *id,
            Self::Split { first, .. } => first.first(),
        }
    }

    pub fn arrange(
        &mut self,
        ui: &mut egui::Ui,
        rect: Rect,
        out: &mut Vec<(PaneId, Rect)>,
        path: u64,
    ) {
        match self {
            Self::Pane(id) => out.push((*id, rect)),
            Self::Split {
                axis,
                ratio,
                first,
                second,
            } => {
                let horizontal = matches!(axis, Axis::Horizontal);
                let span = if horizontal {
                    rect.width()
                } else {
                    rect.height()
                };
                let start = if horizontal { rect.left() } else { rect.top() };
                let mut a = rect;
                let mut b = rect;
                let mid = start + span * *ratio;
                if horizontal {
                    a.max.x = mid;
                    b.min.x = mid;
                } else {
                    a.max.y = mid;
                    b.min.y = mid;
                }
                let divider = if horizontal {
                    Rect::from_min_max(
                        egui::pos2(mid - 3.0, rect.top()),
                        egui::pos2(mid + 3.0, rect.bottom()),
                    )
                } else {
                    Rect::from_min_max(
                        egui::pos2(rect.left(), mid - 3.0),
                        egui::pos2(rect.right(), mid + 3.0),
                    )
                };
                let response =
                    ui.interact(divider, ui.id().with(("split", path)), egui::Sense::drag());
                response.clone().on_hover_cursor(if horizontal {
                    egui::CursorIcon::ResizeHorizontal
                } else {
                    egui::CursorIcon::ResizeVertical
                });
                if response.dragged()
                    && let Some(pos) = response.interact_pointer_pos()
                {
                    *ratio = ((if horizontal { pos.x } else { pos.y } - start) / span.max(1.0))
                        .clamp(0.1, 0.9);
                }
                first.arrange(ui, a, out, path * 2);
                second.arrange(ui, b, out, path * 2 + 1);
            }
        }
    }
}

pub struct Tab {
    pub id: u64,
    pub root: Option<Layout>,
    pub floats: Vec<PaneId>,
    pub active: PaneId,
    pub floats_visible: bool,
    last_active_float: Option<PaneId>,
    pub zoomed: bool,
}

impl Tab {
    pub fn new(id: u64, pane: PaneId) -> Self {
        Self {
            id,
            root: Some(Layout::Pane(pane)),
            floats: vec![],
            active: pane,
            floats_visible: false,
            last_active_float: None,
            zoomed: false,
        }
    }
    pub fn ids(&self) -> Vec<PaneId> {
        let mut ids = vec![];
        if let Some(root) = &self.root {
            root.ids(&mut ids);
        }
        ids.extend_from_slice(&self.floats);
        ids
    }
    pub fn detach(&mut self, id: PaneId) {
        self.root = self.root.take().and_then(|r| r.remove(id));
        self.floats.retain(|p| *p != id);
        if self.last_active_float == Some(id) {
            self.last_active_float = None;
        }
        if self.floats.is_empty() {
            self.floats_visible = false;
        }
        self.zoomed = false;
    }
    pub fn is_visible(&self, id: PaneId) -> bool {
        self.floats_visible || !self.floats.contains(&id)
    }

    pub fn navigation_ids(&self) -> Vec<PaneId> {
        if self.floats_visible && !self.floats.is_empty() {
            self.floats.clone()
        } else {
            self.ids()
                .into_iter()
                .filter(|id| !self.floats.contains(id))
                .collect()
        }
    }

    pub fn toggle_floats(&mut self) {
        self.floats_visible = !self.floats_visible;
        self.zoomed = false;
        if self.floats_visible {
            if let Some(id) = self
                .last_active_float
                .filter(|id| self.floats.contains(id))
                .or_else(|| self.floats.last().copied())
            {
                self.active = id;
            }
        } else if self.floats.contains(&self.active) {
            self.last_active_float = Some(self.active);
            if let Some(root) = &self.root {
                self.active = root.first();
            }
        }
    }

    pub fn focus_direction(&mut self, direction: Direction, rects: &[(PaneId, Rect)]) {
        if self.floats_visible
            && !self.floats.is_empty()
            && matches!(direction, Direction::Down | Direction::Up)
        {
            self.cycle(matches!(direction, Direction::Up));
            return;
        }
        let Some((_, source)) = rects
            .iter()
            .find(|(id, _)| *id == self.active && self.is_visible(*id))
        else {
            return;
        };
        if !source.is_positive() || !source.is_finite() {
            return;
        }
        let horizontal = matches!(direction, Direction::Left | Direction::Right);
        let sign = if matches!(direction, Direction::Left | Direction::Up) {
            -1.0
        } else {
            1.0
        };
        let ids = self.navigation_ids();
        let next = rects
            .iter()
            .filter_map(|(id, rect)| {
                if *id == self.active
                    || !ids.contains(id)
                    || !self.is_visible(*id)
                    || !rect.is_positive()
                    || !rect.is_finite()
                {
                    return None;
                }
                let delta = rect.center() - source.center();
                let forward = sign * if horizontal { delta.x } else { delta.y };
                if forward <= 0.5 {
                    return None;
                }
                let overlap = if horizontal {
                    source.bottom().min(rect.bottom()) - source.top().max(rect.top())
                } else {
                    source.right().min(rect.right()) - source.left().max(rect.left())
                };
                // Prefer aligned neighbors, then the nearest center, without wrapping.
                Some((*id, overlap <= 0.0, delta.length_sq()))
            })
            .min_by(|a, b| {
                a.1.cmp(&b.1)
                    .then_with(|| a.2.total_cmp(&b.2))
                    .then_with(|| a.0.cmp(&b.0))
            });
        if let Some((id, _, _)) = next {
            self.active = id;
        }
    }
    pub fn cycle(&mut self, backwards: bool) {
        let ids = self.navigation_ids();
        if ids.is_empty() {
            return;
        }
        self.active = match ids.iter().position(|p| *p == self.active) {
            Some(pos) => ids[(pos + if backwards { ids.len() - 1 } else { 1 }) % ids.len()],
            None => ids[if backwards { ids.len() - 1 } else { 0 }],
        };
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn removal_collapses_splits_without_losing_siblings() {
        let mut root = Layout::Pane(1);
        assert!(root.split(1, 2, Axis::Horizontal));
        assert!(root.split(2, 3, Axis::Vertical));
        assert!(!root.split(99, 4, Axis::Vertical));
        let root = root.remove(2).unwrap();
        let mut ids = vec![];
        root.ids(&mut ids);
        assert_eq!(ids, vec![1, 3]);
        let root = root.remove(1).unwrap();
        assert_eq!(root.first(), 3);
        assert!(root.remove(3).is_none());
    }
    #[test]
    fn focus_skips_hidden_floats() {
        let mut tab = Tab::new(1, 1);
        tab.floats.push(2);
        tab.floats_visible = true;
        tab.cycle(false);
        assert_eq!(tab.active, 2);
        tab.floats_visible = false;
        tab.cycle(false);
        assert_eq!(tab.active, 1);
    }

    #[test]
    fn floating_visibility_controls_focus_even_without_tiles() {
        let mut tab = Tab::new(1, 1);
        assert!(!tab.floats_visible);
        tab.toggle_floats();
        assert!(tab.floats_visible);
        tab.floats.push(2);
        tab.active = 2;
        tab.toggle_floats();
        assert_eq!(tab.active, 1);
        assert!(!tab.is_visible(2));
        tab.toggle_floats();
        assert_eq!(tab.active, 2);
        tab.root = None;
        tab.toggle_floats();
        assert!(!tab.is_visible(2));
        tab.toggle_floats();
        assert!(tab.is_visible(2));
    }

    #[test]
    fn showing_floats_restores_the_pane_active_when_hidden() {
        let mut tab = Tab::new(1, 1);
        tab.root.as_mut().unwrap().split(1, 2, Axis::Horizontal);
        tab.floats = vec![3, 4, 5];
        tab.toggle_floats();
        assert_eq!(tab.active, 5);
        tab.cycle(true);
        assert_eq!(tab.active, 4);

        tab.toggle_floats();
        assert_eq!(tab.active, 1);
        tab.cycle(false);
        assert_eq!(tab.active, 2);
        tab.toggle_floats();
        assert_eq!(tab.active, 4);

        tab.cycle(true);
        tab.toggle_floats();
        tab.toggle_floats();
        assert_eq!(tab.active, 3);

        tab.root = None;
        tab.toggle_floats();
        tab.toggle_floats();
        assert_eq!(tab.active, 3);
    }

    #[test]
    fn showing_floats_falls_back_when_remembered_pane_is_detached() {
        for dock in [false, true] {
            let mut tab = Tab::new(1, 1);
            tab.floats = vec![2, 3];
            tab.floats_visible = true;
            tab.active = 2;
            tab.toggle_floats();
            tab.detach(2);
            if dock {
                tab.root.as_mut().unwrap().split(1, 2, Axis::Horizontal);
            }
            tab.toggle_floats();
            assert_eq!(tab.active, 3);
        }
    }

    #[test]
    fn directional_focus_uses_geometry_and_ignores_hidden_or_foreign_panes() {
        let mut tab = Tab::new(1, 1);
        let root = tab.root.as_mut().unwrap();
        root.split(1, 2, Axis::Horizontal);
        root.split(1, 3, Axis::Vertical);
        root.split(2, 4, Axis::Vertical);
        tab.floats.push(5);
        let rect = |x, y| Rect::from_min_size(egui::pos2(x, y), egui::vec2(100.0, 100.0));
        let panes = [
            (1, rect(0.0, 0.0)),
            (2, rect(100.0, 0.0)),
            (3, rect(0.0, 100.0)),
            (4, rect(100.0, 100.0)),
            (5, rect(20.0, 0.0)),
            (99, rect(10.0, 0.0)),
        ];
        tab.focus_direction(Direction::Right, &panes);
        assert_eq!(tab.active, 2);
        tab.focus_direction(Direction::Down, &panes);
        assert_eq!(tab.active, 4);
        tab.focus_direction(Direction::Left, &panes);
        assert_eq!(tab.active, 3);
        tab.focus_direction(Direction::Up, &panes);
        assert_eq!(tab.active, 1);
        tab.focus_direction(Direction::Left, &panes);
        assert_eq!(tab.active, 1);
        tab.floats_visible = true;
        tab.focus_direction(Direction::Right, &panes);
        assert_eq!(tab.active, 5);
    }

    #[test]
    fn split_rectangles_touch_without_gaps() {
        let ctx = egui::Context::default();
        let mut layout = Layout::Pane(1);
        layout.split(1, 2, Axis::Horizontal);
        layout.split(2, 3, Axis::Vertical);
        let mut rects = vec![];
        let workspace = Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(800.0, 600.0));
        let _ = ctx.run(egui::RawInput::default(), |ctx| {
            egui::CentralPanel::default()
                .show(ctx, |ui| layout.arrange(ui, workspace, &mut rects, 1));
        });
        assert_eq!(rects.len(), 3);
        assert_eq!(rects[0].1.right(), rects[1].1.left());
        assert_eq!(rects[1].1.bottom(), rects[2].1.top());
        assert_eq!(
            rects.iter().map(|(_, rect)| rect.area()).sum::<f32>(),
            workspace.area()
        );
    }

    #[test]
    fn up_and_down_cycle_visible_floats_without_geometry() {
        let mut tab = Tab::new(1, 1);
        tab.floats = vec![2, 3, 4];
        tab.floats_visible = true;
        for (direction, expected) in [
            (Direction::Down, 2),
            (Direction::Down, 3),
            (Direction::Down, 4),
            (Direction::Down, 2),
            (Direction::Up, 4),
            (Direction::Up, 3),
            (Direction::Up, 2),
        ] {
            tab.focus_direction(direction, &[]);
            assert_eq!(tab.active, expected);
        }

        tab.toggle_floats();
        assert_eq!(tab.active, 1);
        tab.toggle_floats();
        assert_eq!(tab.active, 2);
    }

    #[test]
    fn visible_floats_constrain_directional_and_cyclic_navigation() {
        let mut tab = Tab::new(1, 1);
        tab.root.as_mut().unwrap().split(1, 2, Axis::Horizontal);
        tab.floats = vec![3, 4, 5, 6];
        tab.floats_visible = true;
        tab.active = 3;
        let rect = |x, y| Rect::from_min_size(egui::pos2(x, y), egui::vec2(80.0, 80.0));
        let rects = [
            (1, rect(50.0, 0.0)),
            (2, rect(0.0, 50.0)),
            (3, rect(0.0, 0.0)),
            (4, rect(100.0, 0.0)),
            (5, rect(100.0, 100.0)),
            (6, rect(0.0, 100.0)),
        ];
        for (direction, expected) in [
            (Direction::Right, 4),
            (Direction::Down, 5),
            (Direction::Left, 6),
            (Direction::Up, 5),
            (Direction::Up, 4),
            (Direction::Up, 3),
        ] {
            tab.focus_direction(direction, &rects);
            assert_eq!(tab.active, expected);
        }
        tab.focus_direction(Direction::Left, &rects);
        assert_eq!(tab.active, 3);
        tab.cycle(true);
        assert_eq!(tab.active, 6);
        tab.cycle(false);
        assert_eq!(tab.active, 3);
        tab.floats = vec![3];
        tab.focus_direction(Direction::Right, &rects);
        assert_eq!(tab.active, 3);
        tab.focus_direction(Direction::Down, &rects);
        assert_eq!(tab.active, 3);
        tab.cycle(false);
        assert_eq!(tab.active, 3);
        tab.floats_visible = false;
        tab.cycle(false);
        assert_eq!(tab.active, 1);
        tab.cycle(false);
        assert_eq!(tab.active, 2);
    }
}
