//! Native plugin controls shared by floating panels and docked sidebars.
use crate::plugin_api::{Element, Panel, TreeRow, UIEvent};

pub fn show(ui: &mut egui::Ui, panel: &mut Panel, events: &mut Vec<UIEvent>) {
    for element in &mut panel.elements {
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
                        panel: panel.id.clone(),
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
                        panel: panel.id.clone(),
                        control: id.clone(),
                        value: value.clone(),
                    });
                }
            }
            Element::Tree { id, rows, selected } => {
                ui.push_id(id.as_str(), |ui| {
                    tree(ui, &panel.id, id, rows, selected, events)
                });
            }
        }
    }
}

fn tree(
    ui: &mut egui::Ui,
    panel: &str,
    control: &str,
    rows: &[TreeRow],
    selected: &mut Option<String>,
    events: &mut Vec<UIEvent>,
) {
    let focus = ui.make_persistent_id("tree focus");
    let mut reveal = false;
    if ui.memory(|m| m.has_focus(focus))
        && ui.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Escape))
    {
        ui.memory_mut(|m| m.surrender_focus(focus));
    }
    if ui.memory(|m| m.has_focus(focus)) && !rows.is_empty() {
        let current = selected
            .as_ref()
            .and_then(|id| rows.iter().position(|row| &row.id == id));
        let index = current.unwrap_or(0);
        let pressed = |key| ui.input_mut(|input| input.consume_key(egui::Modifiers::NONE, key));
        let next = if pressed(egui::Key::ArrowDown) {
            Some(current.map_or(0, |i| (i + 1).min(rows.len() - 1)))
        } else if pressed(egui::Key::ArrowUp) {
            Some(index.saturating_sub(1))
        } else if pressed(egui::Key::Home) {
            Some(0)
        } else if pressed(egui::Key::End) {
            Some(rows.len() - 1)
        } else {
            None
        };
        if let Some(next) = next {
            *selected = Some(rows[next].id.clone());
            events.push(UIEvent::Select {
                panel: panel.into(),
                control: control.into(),
                item: rows[next].id.clone(),
            });
            reveal = true;
        } else if let Some(index) = current {
            let row = &rows[index];
            if pressed(egui::Key::Enter) {
                activate(panel, control, row, events);
            } else if pressed(egui::Key::ArrowRight) {
                if row.expandable && !row.expanded {
                    toggle(panel, control, row, true, events);
                }
            } else if pressed(egui::Key::ArrowLeft) {
                if row.expanded {
                    toggle(panel, control, row, false, events);
                } else if let Some(parent) = rows[..index]
                    .iter()
                    .rfind(|parent| parent.depth < row.depth)
                {
                    *selected = Some(parent.id.clone());
                    events.push(UIEvent::Select {
                        panel: panel.into(),
                        control: control.into(),
                        item: parent.id.clone(),
                    });
                    reveal = true;
                }
            }
        }
    }
    let height = ui.text_style_height(&egui::TextStyle::Body).max(20.0);
    let scroll = egui::ScrollArea::vertical()
        .auto_shrink([false, false])
        .max_height(ui.available_height().max(height))
        .show_rows(ui, height, rows.len(), |ui, range| {
            let first_visible = range.start;
            for index in range {
                let row = &rows[index];
                ui.push_id(&row.id, |ui| {
                    ui.horizontal(|ui| {
                        ui.add_space(row.depth as f32 * 14.0);
                        let arrow = if row.expandable {
                            if row.expanded { "▾" } else { "▸" }
                        } else {
                            "·"
                        };
                        if ui
                            .add_sized([18.0, height], egui::Button::new(arrow).frame(false))
                            .clicked()
                        {
                            ui.memory_mut(|m| m.request_focus(focus));
                            if row.expandable {
                                toggle(panel, control, row, !row.expanded, events);
                            }
                        }
                        let response = ui.add_sized(
                            [ui.available_width(), height],
                            egui::Button::selectable(
                                selected.as_ref() == Some(&row.id),
                                &row.label,
                            )
                            .truncate(),
                        );
                        if response.clicked() {
                            *selected = Some(row.id.clone());
                            ui.memory_mut(|m| m.request_focus(focus));
                            events.push(UIEvent::Select {
                                panel: panel.into(),
                                control: control.into(),
                                item: row.id.clone(),
                            });
                        }
                        if response.double_clicked() {
                            activate(panel, control, row, events);
                        }
                    });
                });
            }
            // show_rows only constructs visible widgets. Scroll to a keyboard
            // selection by its row index, including rows outside this frame.
            if reveal
                && let Some(index) = selected
                    .as_ref()
                    .and_then(|id| rows.iter().position(|row| &row.id == id))
            {
                let y = ui.max_rect().top()
                    + (index as f32 - first_visible as f32)
                        * (height + ui.spacing().item_spacing.y);
                ui.scroll_to_rect(
                    egui::Rect::from_min_size(
                        egui::pos2(ui.max_rect().left(), y),
                        egui::vec2(1.0, height),
                    ),
                    None,
                );
            }
        });
    ui.interact(
        scroll.inner_rect,
        focus,
        egui::Sense::focusable_noninteractive(),
    );
}

fn toggle(panel: &str, control: &str, row: &TreeRow, expanded: bool, events: &mut Vec<UIEvent>) {
    events.push(UIEvent::Toggle {
        panel: panel.into(),
        control: control.into(),
        item: row.id.clone(),
        expanded,
    });
}

fn activate(panel: &str, control: &str, row: &TreeRow, events: &mut Vec<UIEvent>) {
    if row.expandable {
        toggle(panel, control, row, !row.expanded, events);
    } else {
        events.push(UIEvent::Activate {
            panel: panel.into(),
            control: control.into(),
            item: row.id.clone(),
        });
    }
}
