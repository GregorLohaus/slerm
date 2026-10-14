//! Native plugin controls shared by floating panels and docked sidebars.
use crate::plugin_api::{Element, Panel, TreeNode, UIEvent};
use egui::{
    Key, Modifiers, Rect, Sense, Shape, Stroke, StrokeKind, TextStyle, pos2,
    text::{LayoutJob, TextWrapping},
    vec2,
};
use std::collections::{HashMap, HashSet};

const INDENT: f32 = 14.0;

/// Tree state owned by the host, so expanding and selecting need no plugin round trip.
#[derive(Clone, Debug, Default)]
pub struct TreeState {
    expanded: HashSet<String>,
    selected: Option<String>,
}

impl Panel {
    /// Keep tree state from the panel this one replaces, for node IDs that still exist.
    pub fn keep_state(&mut self, mut previous: Panel) {
        let mut trees = HashMap::new();
        visit_trees(&self.elements, &mut |id, nodes| {
            if let Some(mut state) = previous.trees.remove(id) {
                let mut ids = HashSet::new();
                collect_ids(nodes, &mut ids);
                state.expanded.retain(|id| ids.contains(id.as_str()));
                state.selected.take_if(|id| !ids.contains(id.as_str()));
                trees.insert(id.to_owned(), state);
            }
        });
        self.trees = trees;
    }
}

pub fn visit_trees<'a>(elements: &'a [Element], f: &mut impl FnMut(&'a str, &'a [TreeNode])) {
    for element in elements {
        match element {
            Element::Row { elements } => visit_trees(elements, f),
            Element::Tree { id, nodes } => f(id, nodes),
            _ => {}
        }
    }
}

fn collect_ids<'a>(nodes: &'a [TreeNode], ids: &mut HashSet<&'a str>) {
    for node in nodes {
        ids.insert(&node.id);
        if let Some(children) = &node.children {
            collect_ids(children, ids);
        }
    }
}

pub fn show(ui: &mut egui::Ui, panel: &mut Panel, events: &mut Vec<UIEvent>) {
    let Panel {
        id,
        elements,
        trees,
        ..
    } = panel;
    show_elements(ui, id, elements, trees, events);
}

fn show_elements(
    ui: &mut egui::Ui,
    panel: &str,
    elements: &mut [Element],
    trees: &mut HashMap<String, TreeState>,
    events: &mut Vec<UIEvent>,
) {
    for element in elements {
        match element {
            Element::Text { text } => {
                ui.label(text.as_str());
            }
            Element::Separator => {
                ui.separator();
            }
            Element::Button { id, label } => {
                if ui
                    .push_id(id.as_str(), |ui| ui.button(label.as_str()))
                    .inner
                    .clicked()
                {
                    events.push(UIEvent::Click {
                        panel: panel.into(),
                        control: id.clone(),
                    });
                }
            }
            Element::Input { id, label, value } => {
                ui.label(label.as_str());
                if ui
                    .add(egui::TextEdit::singleline(value).id_salt(id.as_str()))
                    .changed()
                {
                    events.push(UIEvent::Change {
                        panel: panel.into(),
                        control: id.clone(),
                        value: value.clone(),
                    });
                }
            }
            Element::Row { elements } => {
                ui.horizontal_wrapped(|ui| show_elements(ui, panel, elements, trees, events));
            }
            Element::Tree { id, nodes } => {
                let mut tree = Tree {
                    panel,
                    control: id,
                    state: trees.entry(id.clone()).or_default(),
                    events,
                };
                ui.push_id(id.as_str(), |ui| tree.show(ui, nodes));
            }
        }
    }
}

struct Row<'a> {
    node: &'a TreeNode,
    depth: usize,
    branch: bool,
    expanded: bool,
}

fn flatten<'a>(nodes: &'a [TreeNode], depth: usize, state: &TreeState, rows: &mut Vec<Row<'a>>) {
    for node in nodes {
        let branch = node.is_branch();
        let expanded = branch && state.expanded.contains(&node.id);
        rows.push(Row {
            node,
            depth,
            branch,
            expanded,
        });
        if expanded && let Some(children) = &node.children {
            flatten(children, depth + 1, state, rows);
        }
    }
}

struct Tree<'a> {
    panel: &'a str,
    control: &'a str,
    state: &'a mut TreeState,
    events: &'a mut Vec<UIEvent>,
}

impl Tree<'_> {
    fn select(&mut self, row: &Row) {
        if self.state.selected.as_ref() != Some(&row.node.id) {
            self.state.selected = Some(row.node.id.clone());
            self.events.push(UIEvent::Select {
                panel: self.panel.into(),
                control: self.control.into(),
                item: row.node.id.clone(),
            });
        }
    }

    fn toggle(&mut self, row: &Row, expanded: bool) {
        if expanded {
            self.state.expanded.insert(row.node.id.clone());
        } else {
            self.state.expanded.remove(&row.node.id);
        }
        self.events.push(UIEvent::Toggle {
            panel: self.panel.into(),
            control: self.control.into(),
            item: row.node.id.clone(),
            expanded,
        });
    }

    fn activate(&mut self, row: &Row) {
        if row.branch {
            self.toggle(row, !row.expanded);
        } else {
            self.events.push(UIEvent::Activate {
                panel: self.panel.into(),
                control: self.control.into(),
                item: row.node.id.clone(),
            });
        }
    }

    fn show(&mut self, ui: &mut egui::Ui, nodes: &[TreeNode]) {
        let mut rows = Vec::new();
        flatten(nodes, 0, self.state, &mut rows);
        let focus = ui.make_persistent_id("tree focus");
        let reveal = self.keyboard(ui, focus, &rows);
        let focused = ui.memory(|m| m.has_focus(focus));
        let font = TextStyle::Body.resolve(ui.style());
        let height = (ui.text_style_height(&TextStyle::Body) + 6.0).round();
        let visuals = ui.visuals().clone();
        let scroll = ui.scope(|ui| {
            // Contiguous rows, so highlights and indent guides join up.
            ui.spacing_mut().item_spacing.y = 0.0;
            egui::ScrollArea::vertical()
                .auto_shrink([false, false])
                .show_rows(ui, height, rows.len(), |ui, range| {
                    let first = range.start;
                    for row in &rows[range] {
                        let (rect, response) = ui.allocate_exact_size(
                            vec2(ui.available_width(), height),
                            Sense::click(),
                        );
                        let painter = ui.painter_at(rect);
                        let selected = self.state.selected.as_ref() == Some(&row.node.id);
                        if selected {
                            painter.rect_filled(rect, 4.0, visuals.selection.bg_fill);
                            if focused {
                                painter.rect_stroke(
                                    rect,
                                    4.0,
                                    Stroke::new(1.0_f32, visuals.selection.stroke.color),
                                    StrokeKind::Inside,
                                );
                            }
                        } else if response.hovered() {
                            painter.rect_filled(
                                rect,
                                4.0,
                                visuals.widgets.hovered.weak_bg_fill.gamma_multiply(0.6),
                            );
                        }
                        // Guides sit under each ancestor's chevron.
                        let guide =
                            Stroke::new(1.0_f32, visuals.widgets.noninteractive.bg_stroke.color);
                        for level in 0..row.depth {
                            let x = (rect.left() + 10.0 + level as f32 * INDENT).round() + 0.5;
                            painter.vline(x, rect.y_range(), guide);
                        }
                        let mut x = rect.left() + 4.0 + row.depth as f32 * INDENT;
                        let center = rect.center().y;
                        let text = if selected {
                            visuals.strong_text_color()
                        } else {
                            visuals.text_color()
                        };
                        if row.branch {
                            chevron(&painter, pos2(x + 6.0, center), row.expanded, text);
                        }
                        let chevron_end = x + INDENT;
                        x = chevron_end + 2.0;
                        if let Some(icon) = &row.node.icon {
                            let color = if row.branch {
                                visuals.hyperlink_color
                            } else {
                                visuals.weak_text_color()
                            };
                            let galley = painter.layout_no_wrap(icon.clone(), font.clone(), color);
                            let width = galley.size().x.max(font.size);
                            painter.galley(pos2(x, center - galley.size().y / 2.0), galley, color);
                            x += width + 6.0;
                        }
                        let mut job = LayoutJob::simple_singleline(
                            row.node.label.clone(),
                            font.clone(),
                            text,
                        );
                        job.wrap =
                            TextWrapping::truncate_at_width((rect.right() - 4.0 - x).max(0.0));
                        let galley = painter.layout_job(job);
                        let elided = galley.elided;
                        painter.galley(pos2(x, center - galley.size().y / 2.0), galley, text);
                        if response.clicked() {
                            ui.memory_mut(|m| m.request_focus(focus));
                            let on_chevron = row.branch
                                && response
                                    .interact_pointer_pos()
                                    .is_some_and(|pos| pos.x < chevron_end);
                            if on_chevron {
                                self.toggle(row, !row.expanded);
                            } else {
                                self.select(row);
                                if response.double_clicked() {
                                    self.activate(row);
                                }
                            }
                        }
                        if elided {
                            response.on_hover_text(&row.node.label);
                        }
                    }
                    // show_rows only builds visible rows. Scroll to a keyboard
                    // selection by its index, including rows outside this frame.
                    if reveal
                        && let Some(index) = self
                            .state
                            .selected
                            .as_ref()
                            .and_then(|id| rows.iter().position(|row| &row.node.id == id))
                    {
                        let top = ui.max_rect().top() + (index as f32 - first as f32) * height;
                        ui.scroll_to_rect(
                            Rect::from_min_size(pos2(ui.max_rect().left(), top), vec2(1.0, height)),
                            None,
                        );
                    }
                })
        });
        ui.interact(
            scroll.inner.inner_rect,
            focus,
            Sense::focusable_noninteractive(),
        );
    }

    /// Arrow keys move and expand, Enter activates. Returns whether to scroll to the selection.
    fn keyboard(&mut self, ui: &egui::Ui, focus: egui::Id, rows: &[Row]) -> bool {
        if !ui.memory(|m| m.has_focus(focus)) || rows.is_empty() {
            return false;
        }
        let pressed = |key| ui.input_mut(|input| input.consume_key(Modifiers::NONE, key));
        if pressed(Key::Escape) {
            ui.memory_mut(|m| m.surrender_focus(focus));
            return false;
        }
        let current = self
            .state
            .selected
            .as_ref()
            .and_then(|id| rows.iter().position(|row| &row.node.id == id));
        let last = rows.len() - 1;
        let next = if pressed(Key::ArrowDown) {
            Some(current.map_or(0, |i| (i + 1).min(last)))
        } else if pressed(Key::ArrowUp) {
            Some(current.map_or(0, |i| i.saturating_sub(1)))
        } else if pressed(Key::Home) {
            Some(0)
        } else if pressed(Key::End) {
            Some(last)
        } else {
            let Some(index) = current else { return false };
            let row = &rows[index];
            if pressed(Key::Enter) {
                self.activate(row);
                None
            } else if pressed(Key::ArrowRight) {
                if row.branch && !row.expanded {
                    self.toggle(row, true);
                    None
                } else {
                    // Move into an expanded row's first child.
                    rows.get(index + 1)
                        .filter(|child| child.depth > row.depth)
                        .map(|_| index + 1)
                }
            } else if pressed(Key::ArrowLeft) {
                if row.expanded {
                    self.toggle(row, false);
                    None
                } else {
                    rows[..index]
                        .iter()
                        .rposition(|parent| parent.depth < row.depth)
                }
            } else {
                None
            }
        };
        let Some(next) = next else { return false };
        self.select(&rows[next]);
        true
    }
}

fn chevron(painter: &egui::Painter, center: egui::Pos2, expanded: bool, color: egui::Color32) {
    let r = 3.0;
    let points = if expanded {
        vec![
            center + vec2(-r, -r / 2.0),
            center + vec2(0.0, r / 2.0),
            center + vec2(r, -r / 2.0),
        ]
    } else {
        vec![
            center + vec2(-r / 2.0, -r),
            center + vec2(r / 2.0, 0.0),
            center + vec2(-r / 2.0, r),
        ]
    };
    painter.add(Shape::line(
        points,
        Stroke::new(1.5_f32, color.gamma_multiply(0.8)),
    ));
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn tree_state_survives_updates_for_remaining_nodes() {
        let panel = |nodes| {
            serde_json::from_value::<Panel>(
                json!({"id":"p", "title":"P", "elements":[{"type":"tree", "id":"t", "nodes": nodes}]}),
            )
            .unwrap()
        };
        let mut previous =
            panel(json!([{"id":"a", "label":"a", "branch":true}, {"id":"b", "label":"b"}]));
        previous.trees.insert(
            "t".into(),
            TreeState {
                expanded: HashSet::from(["a".into(), "gone".into()]),
                selected: Some("b".into()),
            },
        );
        let mut next =
            panel(json!([{"id":"a", "label":"a", "children":[{"id":"c", "label":"c"}]}]));
        next.keep_state(previous);
        let state = &next.trees["t"];
        assert_eq!(state.expanded, HashSet::from(["a".into()]));
        assert_eq!(state.selected, None);
    }
}
