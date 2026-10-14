//! Native plugin controls shared by floating panels and docked sidebars.
use crate::plugin_api::{
    Element, Navigation, Panel, TableRow, TextColor, TextStyle, TreeNode, UIEvent,
};
use anyhow::{Result, bail};
use egui::{
    FontId, Id, Key, Modifiers, Rect, Response, RichText, Sense, Shape, Stroke, StrokeKind, Ui,
    Visuals, pos2,
    text::{LayoutJob, TextWrapping},
    vec2,
};
use std::collections::{HashMap, HashSet};

const INDENT: f32 = 14.0;
/// Rows a table shows before it scrolls.
const TABLE_ROWS: usize = 12;
/// Rows that page up and page down move.
const PAGE: usize = 10;

/// List state owned by the host, so expanding and selecting need no plugin round
/// trip. Tables keep their selection in the element instead.
#[derive(Clone, Debug, Default)]
pub struct ListState {
    expanded: HashSet<String>,
    selected: Option<String>,
    /// Scroll to the selection in the next frame, after a `navigate`.
    reveal: bool,
}

impl Panel {
    /// Keep host state from the panel this one replaces, for IDs that still exist.
    pub fn keep_state(&mut self, mut previous: Panel) {
        let mut lists = HashMap::new();
        walk(&self.elements, &mut |element| match element {
            Element::Tree { id, nodes } => {
                if let Some(mut state) = previous.lists.remove(id) {
                    let mut ids = HashSet::new();
                    collect_ids(nodes, &mut ids);
                    state.expanded.retain(|id| ids.contains(id.as_str()));
                    state.selected.take_if(|id| !ids.contains(id.as_str()));
                    lists.insert(id.clone(), state);
                }
            }
            Element::Table { id, .. } => {
                if let Some(state) = previous.lists.remove(id) {
                    lists.insert(id.clone(), state);
                }
            }
            _ => {}
        });
        self.lists = lists;
        self.focused = previous.focused;
        self.focus_request = previous.focus_request;
        // An update built before the plugin saw the latest keystrokes would
        // otherwise overwrite them.
        if let Some(id) = previous.editing
            && let Some(typed) = text_value(&mut previous.elements, &id)
            && let Some(value) = text_value(&mut self.elements, &id)
        {
            *value = std::mem::take(typed);
            self.editing = Some(id);
        }
    }
}

pub fn control_id(element: &Element) -> Option<&String> {
    match element {
        Element::Button { id, .. }
        | Element::Input { id, .. }
        | Element::TextArea { id, .. }
        | Element::Checkbox { id, .. }
        | Element::Slider { id, .. }
        | Element::Select { id, .. }
        | Element::Section { id, .. }
        | Element::Table { id, .. }
        | Element::Tree { id, .. } => Some(id),
        Element::Text { .. }
        | Element::Progress { .. }
        | Element::Spinner
        | Element::Separator
        | Element::Row { .. } => None,
    }
}

/// Visit every element, including those nested in rows and sections.
pub fn walk<'a>(elements: &'a [Element], f: &mut impl FnMut(&'a Element)) {
    for element in elements {
        f(element);
        if let Element::Row { elements } | Element::Section { elements, .. } = element {
            walk(elements, f);
        }
    }
}

fn text_value<'a>(elements: &'a mut [Element], control: &str) -> Option<&'a mut String> {
    elements.iter_mut().find_map(|element| match element {
        Element::Input { id, value, .. } | Element::TextArea { id, value, .. } if id == control => {
            Some(value)
        }
        Element::Row { elements } | Element::Section { elements, .. } => {
            text_value(elements, control)
        }
        _ => None,
    })
}

pub fn visit_trees<'a>(elements: &'a [Element], f: &mut impl FnMut(&'a str, &'a [TreeNode])) {
    walk(elements, &mut |element| {
        if let Element::Tree { id, nodes } = element {
            f(id, nodes);
        }
    });
}

fn collect_ids<'a>(nodes: &'a [TreeNode], ids: &mut HashSet<&'a str>) {
    for node in nodes {
        ids.insert(&node.id);
        if let Some(children) = &node.children {
            collect_ids(children, ids);
        }
    }
}

struct Cx<'a> {
    panel: &'a str,
    lists: &'a mut HashMap<String, ListState>,
    editing: &'a mut Option<String>,
    focused: &'a mut Option<String>,
    focus_request: &'a mut Option<Option<String>>,
    events: &'a mut Vec<UIEvent>,
}

impl Cx<'_> {
    /// Whether a pending `focusPanel` targets this control, consuming it if so.
    fn take_focus(&mut self, control: &str) -> bool {
        let matches = match &*self.focus_request {
            Some(Some(id)) => id == control,
            Some(None) => true,
            None => false,
        };
        if matches {
            *self.focus_request = None;
        }
        matches
    }
}

pub fn show(ui: &mut Ui, panel: &mut Panel, events: &mut Vec<UIEvent>) {
    let Panel {
        id,
        elements,
        keys,
        lists,
        editing,
        focused,
        focus_request,
        ..
    } = panel;
    // Declared keys go to the plugin before lists see them, but only while a
    // list has focus, so text fields keep their typing.
    if let (Some(control), Some(keys)) = (focused.take(), keys.as_ref()) {
        let shortcuts: Vec<_> = keys
            .iter()
            .filter_map(|key| Some((key, crate::config::shortcut(key).ok()?)))
            .collect();
        ui.input_mut(|input| {
            input.events.retain(|event| {
                let egui::Event::Key {
                    key,
                    pressed: true,
                    modifiers,
                    ..
                } = event
                else {
                    return true;
                };
                let Some((name, _)) = shortcuts.iter().find(|(_, shortcut)| {
                    let m = shortcut.modifiers;
                    shortcut.logical_key == *key
                        && m.ctrl == modifiers.ctrl
                        && m.shift == modifiers.shift
                        && m.alt == modifiers.alt
                        && m.mac_cmd == modifiers.mac_cmd
                }) else {
                    return true;
                };
                events.push(UIEvent::Key {
                    panel: id.clone(),
                    control: control.clone(),
                    key: (*name).clone(),
                });
                false
            });
        });
    }
    *editing = None;
    show_elements(
        ui,
        &mut Cx {
            panel: id,
            lists,
            editing,
            focused,
            focus_request,
            events,
        },
        elements,
    );
}

fn show_elements(ui: &mut Ui, cx: &mut Cx, elements: &mut [Element]) {
    let panel = cx.panel;
    for element in elements {
        match element {
            Element::Text { text, style, color } => {
                let mut text = RichText::new(text.as_str());
                text = match style {
                    Some(TextStyle::Heading) => text.heading(),
                    Some(TextStyle::Strong) => text.strong(),
                    Some(TextStyle::Weak) => text.weak(),
                    Some(TextStyle::Small) => text.small(),
                    Some(TextStyle::Monospace) => text.monospace(),
                    None => text,
                };
                if let Some(color) = color {
                    let visuals = ui.visuals();
                    text = text.color(match color {
                        TextColor::Accent => visuals.hyperlink_color,
                        TextColor::Warning => visuals.warn_fg_color,
                        TextColor::Error => visuals.error_fg_color,
                    });
                }
                ui.label(text);
            }
            Element::Separator => {
                ui.separator();
            }
            Element::Spinner => {
                ui.spinner();
            }
            Element::Progress { value, text } => {
                let mut bar = egui::ProgressBar::new(value.clamp(0.0, 1.0));
                bar = match text {
                    Some(text) => bar.text(text.as_str()),
                    None => bar.show_percentage(),
                };
                ui.add(bar);
            }
            Element::Button { id, label } => {
                if ui
                    .push_id(id.as_str(), |ui| ui.button(label.as_str()))
                    .inner
                    .clicked()
                {
                    cx.events.push(UIEvent::Click {
                        panel: panel.into(),
                        control: id.clone(),
                    });
                }
            }
            Element::Input { id, label, value } => {
                ui.label(label.as_str());
                let response = ui.add(egui::TextEdit::singleline(value).id_salt(id.as_str()));
                if cx.take_focus(id) {
                    response.request_focus();
                }
                if response.has_focus() {
                    *cx.editing = Some(id.clone());
                }
                if response.changed() {
                    cx.events.push(UIEvent::Change {
                        panel: panel.into(),
                        control: id.clone(),
                        value: value.clone(),
                    });
                }
            }
            Element::TextArea {
                id,
                value,
                rows,
                code,
                readonly,
            } => {
                let rows = rows.unwrap_or(6) as usize;
                let code = code.unwrap_or(false);
                let readonly = readonly.unwrap_or(false);
                let style = if code {
                    egui::TextStyle::Monospace
                } else {
                    egui::TextStyle::Body
                };
                let max_height = ui.text_style_height(&style) * rows as f32 + 8.0;
                fn edit<'t>(
                    text: &'t mut dyn egui::TextBuffer,
                    id: &str,
                    rows: usize,
                    code: bool,
                ) -> egui::TextEdit<'t> {
                    let edit = egui::TextEdit::multiline(text)
                        .id_salt(id)
                        .desired_rows(rows)
                        .desired_width(f32::INFINITY);
                    if code { edit.code_editor() } else { edit }
                }
                let changed = egui::ScrollArea::vertical()
                    .id_salt(id.as_str())
                    .max_height(max_height)
                    // Read-only areas usually show logs or terminal output.
                    .stick_to_bottom(readonly)
                    .show(ui, |ui| {
                        if readonly {
                            ui.add(edit(&mut value.as_str(), id, rows, code));
                            false
                        } else {
                            let response = ui.add(edit(value, id, rows, code));
                            if cx.take_focus(id) {
                                response.request_focus();
                            }
                            if response.has_focus() {
                                *cx.editing = Some(id.clone());
                            }
                            response.changed()
                        }
                    })
                    .inner;
                if changed {
                    cx.events.push(UIEvent::Change {
                        panel: panel.into(),
                        control: id.clone(),
                        value: value.clone(),
                    });
                }
            }
            Element::Checkbox { id, label, checked } => {
                if ui
                    .push_id(id.as_str(), |ui| ui.checkbox(checked, label.as_str()))
                    .inner
                    .changed()
                {
                    cx.events.push(UIEvent::Check {
                        panel: panel.into(),
                        control: id.clone(),
                        checked: *checked,
                    });
                }
            }
            Element::Slider {
                id,
                label,
                value,
                min,
                max,
                step,
            } => {
                let response = ui.push_id(id.as_str(), |ui| {
                    let mut slider = egui::Slider::new(value, *min..=*max);
                    if let Some(step) = step {
                        slider = slider.step_by(*step);
                    }
                    if let Some(label) = label {
                        slider = slider.text(label.as_str());
                    }
                    ui.add(slider)
                });
                // One event per adjustment, not one per frame of a drag.
                let response = response.inner;
                if response.drag_stopped() || (response.changed() && !response.dragged()) {
                    cx.events.push(UIEvent::Number {
                        panel: panel.into(),
                        control: id.clone(),
                        value: *value,
                    });
                }
            }
            Element::Select {
                id,
                label,
                value,
                options,
            } => {
                let mut changed = false;
                egui::ComboBox::new(id.as_str(), label.as_deref().unwrap_or_default())
                    .selected_text(value.as_str())
                    .show_ui(ui, |ui| {
                        for option in options.iter() {
                            changed |= ui
                                .selectable_value(value, option.clone(), option.as_str())
                                .clicked();
                        }
                    });
                if changed {
                    cx.events.push(UIEvent::Change {
                        panel: panel.into(),
                        control: id.clone(),
                        value: value.clone(),
                    });
                }
            }
            Element::Row { elements } => {
                ui.horizontal_wrapped(|ui| show_elements(ui, cx, elements));
            }
            Element::Section {
                id,
                title,
                open,
                elements,
            } => {
                egui::CollapsingHeader::new(RichText::new(title.as_str()).strong())
                    .id_salt(id.as_str())
                    .default_open(open.unwrap_or(true))
                    .show(ui, |ui| show_elements(ui, cx, elements));
            }
            Element::Table {
                id,
                columns,
                rows,
                selected,
            } => {
                let focus = cx.take_focus(id);
                let focused = ui
                    .push_id(id.as_str(), |ui| {
                        table(ui, cx, id, columns, rows, selected, focus)
                    })
                    .inner;
                if focused {
                    *cx.focused = Some(id.clone());
                }
            }
            Element::Tree { id, nodes } => {
                let focus = cx.take_focus(id);
                let mut tree = Tree {
                    panel,
                    control: id,
                    state: cx.lists.entry(id.clone()).or_default(),
                    events: cx.events,
                };
                let focused = ui
                    .push_id(id.as_str(), |ui| tree.show(ui, nodes, focus))
                    .inner;
                if focused {
                    *cx.focused = Some(id.clone());
                }
            }
        }
    }
}

// Shared by tables and trees: keyboard focus, virtualized rows, and row painting.

/// Keys for a focused list. Escape hands focus back to the terminal.
fn nav(ui: &Ui, focus: Id) -> Option<Navigation> {
    if !ui.memory(|m| m.has_focus(focus)) {
        return None;
    }
    let pressed = |key| ui.input_mut(|input| input.consume_key(Modifiers::NONE, key));
    if pressed(Key::Escape) {
        ui.memory_mut(|m| m.surrender_focus(focus));
        return None;
    }
    [
        (Key::ArrowDown, Navigation::Next),
        (Key::ArrowUp, Navigation::Previous),
        (Key::Home, Navigation::First),
        (Key::End, Navigation::Last),
        (Key::PageDown, Navigation::PageDown),
        (Key::PageUp, Navigation::PageUp),
        (Key::ArrowLeft, Navigation::Left),
        (Key::ArrowRight, Navigation::Right),
        (Key::Enter, Navigation::Activate),
    ]
    .into_iter()
    .find_map(|(key, to)| pressed(key).then_some(to))
}

/// The row a movement lands on, for movements that need no tree structure.
fn step(to: Navigation, len: usize, current: Option<usize>) -> Option<usize> {
    let last = len.checked_sub(1)?;
    Some(match to {
        Navigation::Next => current.map_or(0, |i| (i + 1).min(last)),
        Navigation::Previous => current.map_or(0, |i| i.saturating_sub(1)),
        Navigation::First => 0,
        Navigation::Last => last,
        Navigation::PageDown => current.map_or(0, |i| (i + PAGE).min(last)),
        Navigation::PageUp => current.map_or(0, |i| i.saturating_sub(PAGE)),
        Navigation::Left | Navigation::Right | Navigation::Activate => return None,
    })
}

fn find_element<'a>(elements: &'a mut [Element], control: &str) -> Option<&'a mut Element> {
    elements.iter_mut().find_map(|element| match element {
        Element::Tree { id, .. } | Element::Table { id, .. } if id == control => Some(element),
        Element::Row { elements } | Element::Section { elements, .. } => {
            find_element(elements, control)
        }
        _ => None,
    })
}

/// Apply a `navigate` request as if the user pressed the matching key.
pub fn navigate(panel: &mut Panel, control: &str, to: Navigation) -> Result<Vec<UIEvent>> {
    let mut events = Vec::new();
    let state = panel.lists.entry(control.into()).or_default();
    match find_element(&mut panel.elements, control) {
        Some(Element::Tree { nodes, .. }) => {
            let mut rows = Vec::new();
            flatten(nodes, 0, state, &mut rows);
            let mut tree = Tree {
                panel: &panel.id,
                control,
                state,
                events: &mut events,
            };
            if tree.navigate(&rows, to).is_some() {
                tree.state.reveal = true;
            }
        }
        Some(Element::Table { rows, selected, .. }) => {
            if table_navigate(&panel.id, control, rows, selected, to, &mut events).is_some() {
                state.reveal = true;
            }
        }
        _ => bail!("no tree or table {control} in panel {}", panel.id),
    }
    Ok(events)
}

fn table_navigate(
    panel: &str,
    control: &str,
    rows: &[TableRow],
    selected: &mut Option<String>,
    to: Navigation,
    events: &mut Vec<UIEvent>,
) -> Option<usize> {
    let current = selected
        .as_ref()
        .and_then(|id| rows.iter().position(|row| &row.id == id));
    if let Navigation::Activate = to {
        events.push(UIEvent::Activate {
            panel: panel.into(),
            control: control.into(),
            item: rows[current?].id.clone(),
        });
        return None;
    }
    let index = step(to, rows.len(), current)?;
    table_select(panel, control, &rows[index], selected, events);
    Some(index)
}

fn table_select(
    panel: &str,
    control: &str,
    row: &TableRow,
    selected: &mut Option<String>,
    events: &mut Vec<UIEvent>,
) {
    if selected.as_ref() != Some(&row.id) {
        *selected = Some(row.id.clone());
        events.push(UIEvent::Select {
            panel: panel.into(),
            control: control.into(),
            item: row.id.clone(),
        });
    }
}

fn row_height(ui: &Ui) -> f32 {
    (ui.text_style_height(&egui::TextStyle::Body) + 6.0).round()
}

/// Show `count` uniform rows, building only the visible ones. `reveal` scrolls to a row.
fn rows(
    ui: &mut Ui,
    focus: Id,
    count: usize,
    max_rows: Option<usize>,
    reveal: Option<usize>,
    mut add: impl FnMut(&mut Ui, usize),
) {
    let height = row_height(ui);
    let scroll = ui.scope(|ui| {
        // Contiguous rows, so highlights and indent guides join up.
        ui.spacing_mut().item_spacing.y = 0.0;
        let mut area = egui::ScrollArea::vertical().auto_shrink([false, max_rows.is_some()]);
        if let Some(max_rows) = max_rows {
            area = area.max_height(height * max_rows as f32);
        }
        area.show_rows(ui, height, count, |ui, range| {
            let first = range.start;
            for index in range {
                add(ui, index);
            }
            // show_rows only builds visible rows, so scroll by index.
            if let Some(index) = reveal {
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

/// Allocate a full-width row and paint its hover or selection background.
fn row_frame(ui: &mut Ui, selected: bool, focused: bool) -> (Rect, Response) {
    let (rect, response) =
        ui.allocate_exact_size(vec2(ui.available_width(), row_height(ui)), Sense::click());
    let visuals = ui.visuals();
    let painter = ui.painter();
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
    (rect, response)
}

/// Paint one line of text from `x` to `right`, ending in an ellipsis if it does
/// not fit. Returns whether it was shortened.
fn paint_text(
    painter: &egui::Painter,
    text: &str,
    font: &FontId,
    color: egui::Color32,
    x: f32,
    right: f32,
    center: f32,
) -> bool {
    let mut job = LayoutJob::simple_singleline(text.into(), font.clone(), color);
    job.wrap = TextWrapping::truncate_at_width((right - x).max(0.0));
    let galley = painter.layout_job(job);
    let elided = galley.elided;
    painter.galley(pos2(x, center - galley.size().y / 2.0), galley, color);
    elided
}

fn text_color(visuals: &Visuals, selected: bool) -> egui::Color32 {
    if selected {
        visuals.strong_text_color()
    } else {
        visuals.text_color()
    }
}

/// Returns whether the table has keyboard focus.
fn table(
    ui: &mut Ui,
    cx: &mut Cx,
    control: &str,
    columns: &[String],
    rows_: &[TableRow],
    selected: &mut Option<String>,
    take_focus: bool,
) -> bool {
    let font = egui::TextStyle::Body.resolve(ui.style());
    let visuals = ui.visuals().clone();
    // Size columns to their content, measuring the header and the first rows.
    let measure = |text: &str| {
        ui.painter()
            .layout_no_wrap(text.into(), font.clone(), visuals.text_color())
            .size()
            .x
            + 16.0
    };
    let available = ui.available_width() - 12.0;
    let mut widths: Vec<f32> = columns.iter().map(|c| measure(c)).collect();
    for row in rows_.iter().take(200) {
        for (width, cell) in widths.iter_mut().zip(&row.cells) {
            *width = width.max(measure(cell));
        }
    }
    for width in &mut widths {
        *width = width.min(available * 0.45);
    }
    let lefts: Vec<f32> = widths
        .iter()
        .scan(6.0, |x, width| {
            let left = *x;
            *x += width;
            Some(left)
        })
        .collect();
    let paint_cells = |painter: &egui::Painter, rect: Rect, cells: &[String], color| {
        let mut elided = false;
        for (index, cell) in cells.iter().enumerate() {
            let x = rect.left() + lefts[index];
            // The last column takes the remaining width.
            let right = match lefts.get(index + 1) {
                Some(next) => rect.left() + next - 8.0,
                None => rect.right() - 6.0,
            };
            elided |= paint_text(painter, cell, &font, color, x, right, rect.center().y);
        }
        elided
    };
    let (header, _) =
        ui.allocate_exact_size(vec2(ui.available_width(), row_height(ui)), Sense::hover());
    paint_cells(ui.painter(), header, columns, visuals.weak_text_color());
    ui.painter().hline(
        header.x_range(),
        header.bottom(),
        visuals.widgets.noninteractive.bg_stroke,
    );

    let focus = ui.make_persistent_id("table focus");
    if take_focus {
        ui.memory_mut(|m| m.request_focus(focus));
    }
    let state = cx.lists.entry(control.into()).or_default();
    let mut reveal = std::mem::take(&mut state.reveal)
        .then(|| {
            selected
                .as_ref()
                .and_then(|id| rows_.iter().position(|row| &row.id == id))
        })
        .flatten();
    if let Some(to) = nav(ui, focus) {
        reveal = table_navigate(cx.panel, control, rows_, selected, to, cx.events).or(reveal);
    }
    let focused = ui.memory(|m| m.has_focus(focus));
    rows(
        ui,
        focus,
        rows_.len(),
        Some(TABLE_ROWS),
        reveal,
        |ui, index| {
            let row = &rows_[index];
            let is_selected = selected.as_ref() == Some(&row.id);
            let (rect, response) = row_frame(ui, is_selected, focused);
            let elided = paint_cells(
                &ui.painter_at(rect),
                rect,
                &row.cells,
                text_color(&visuals, is_selected),
            );
            if response.clicked() {
                ui.memory_mut(|m| m.request_focus(focus));
                table_select(cx.panel, control, row, selected, cx.events);
                if response.double_clicked() {
                    cx.events.push(UIEvent::Activate {
                        panel: cx.panel.into(),
                        control: control.into(),
                        item: row.id.clone(),
                    });
                }
            }
            if elided {
                response.on_hover_text(row.cells.join("  "));
            }
        },
    );
    focused
}

struct Row<'a> {
    node: &'a TreeNode,
    depth: usize,
    branch: bool,
    expanded: bool,
}

fn flatten<'a>(nodes: &'a [TreeNode], depth: usize, state: &ListState, rows: &mut Vec<Row<'a>>) {
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
    state: &'a mut ListState,
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

    /// Move, collapse, expand, or activate. Returns the row to scroll to.
    fn navigate(&mut self, rows: &[Row], to: Navigation) -> Option<usize> {
        let current = self
            .state
            .selected
            .as_ref()
            .and_then(|id| rows.iter().position(|row| &row.node.id == id));
        let next = match to {
            Navigation::Activate => {
                self.activate(&rows[current?]);
                None
            }
            Navigation::Right => {
                let index = current?;
                let row = &rows[index];
                if row.branch && !row.expanded {
                    self.toggle(row, true);
                    None
                } else {
                    // Move into an expanded row's first child.
                    rows.get(index + 1)
                        .filter(|child| child.depth > row.depth)
                        .map(|_| index + 1)
                }
            }
            Navigation::Left => {
                let index = current?;
                let row = &rows[index];
                if row.expanded {
                    self.toggle(row, false);
                    None
                } else {
                    rows[..index]
                        .iter()
                        .rposition(|parent| parent.depth < row.depth)
                }
            }
            movement => step(movement, rows.len(), current),
        }?;
        self.select(&rows[next]);
        Some(next)
    }

    /// Returns whether the tree has keyboard focus.
    fn show(&mut self, ui: &mut Ui, nodes: &[TreeNode], take_focus: bool) -> bool {
        let mut flat = Vec::new();
        flatten(nodes, 0, self.state, &mut flat);
        let focus = ui.make_persistent_id("tree focus");
        if take_focus {
            ui.memory_mut(|m| m.request_focus(focus));
        }
        let mut reveal = std::mem::take(&mut self.state.reveal)
            .then(|| {
                self.state
                    .selected
                    .as_ref()
                    .and_then(|id| flat.iter().position(|row| &row.node.id == id))
            })
            .flatten();
        if let Some(to) = nav(ui, focus) {
            reveal = self.navigate(&flat, to).or(reveal);
        }
        let focused = ui.memory(|m| m.has_focus(focus));
        let font = egui::TextStyle::Body.resolve(ui.style());
        let visuals = ui.visuals().clone();
        rows(ui, focus, flat.len(), None, reveal, |ui, index| {
            let row = &flat[index];
            let selected = self.state.selected.as_ref() == Some(&row.node.id);
            let (rect, response) = row_frame(ui, selected, focused);
            let painter = ui.painter_at(rect);
            // Guides sit under each ancestor's chevron.
            for level in 0..row.depth {
                let x = (rect.left() + 10.0 + level as f32 * INDENT).round() + 0.5;
                painter.vline(x, rect.y_range(), visuals.widgets.noninteractive.bg_stroke);
            }
            let center = rect.center().y;
            let text = text_color(&visuals, selected);
            let mut x = rect.left() + 4.0 + row.depth as f32 * INDENT;
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
            let elided = paint_text(
                &painter,
                &row.node.label,
                &font,
                text,
                x,
                rect.right() - 4.0,
                center,
            );
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
        });
        focused
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
        previous.lists.insert(
            "t".into(),
            ListState {
                expanded: HashSet::from(["a".into(), "gone".into()]),
                selected: Some("b".into()),
                reveal: false,
            },
        );
        let mut next =
            panel(json!([{"id":"a", "label":"a", "children":[{"id":"c", "label":"c"}]}]));
        next.keep_state(previous);
        let state = &next.lists["t"];
        assert_eq!(state.expanded, HashSet::from(["a".into()]));
        assert_eq!(state.selected, None);
    }

    #[test]
    fn focused_text_keeps_local_edits_across_updates() {
        let panel = |message: &str, note: &str| {
            serde_json::from_value::<Panel>(json!({"id":"p", "title":"P", "elements":[
                {"type":"section", "id":"s", "title":"S", "elements":[
                    {"type":"textArea", "id":"message", "value": message}
                ]},
                {"type":"input", "id":"note", "label":"Note", "value": note}
            ]}))
            .unwrap()
        };
        let value = |panel: &mut Panel, id| text_value(&mut panel.elements, id).cloned();
        let mut previous = panel("typed locally", "old");
        previous.editing = Some("message".into());
        let mut next = panel("stale", "from plugin");
        next.keep_state(previous);
        assert_eq!(
            value(&mut next, "message").as_deref(),
            Some("typed locally")
        );
        assert_eq!(value(&mut next, "note").as_deref(), Some("from plugin"));
        // Once the field loses focus, plugin values apply again.
        let mut last = panel("cleared", "x");
        next.editing = None;
        last.keep_state(next);
        assert_eq!(value(&mut last, "message").as_deref(), Some("cleared"));
    }

    fn items(events: &[UIEvent]) -> Vec<String> {
        events
            .iter()
            .map(|event| match event {
                UIEvent::Select { item, .. } => format!("select {item}"),
                UIEvent::Toggle { item, expanded, .. } => format!("toggle {item} {expanded}"),
                UIEvent::Activate { item, .. } => format!("activate {item}"),
                UIEvent::Key { control, key, .. } => format!("key {control} {key}"),
                other => format!("{other:?}"),
            })
            .collect()
    }

    #[test]
    fn navigate_moves_trees_and_tables_like_their_keys() {
        let mut panel = serde_json::from_value::<Panel>(json!({"id":"p", "title":"P", "elements":[
            {"type":"tree", "id":"t", "nodes":[
                {"id":"a", "label":"a", "children":[{"id":"b", "label":"b"}]},
                {"id":"c", "label":"c"}
            ]},
            {"type":"section", "id":"s", "title":"S", "elements":[
                {"type":"table", "id":"rows", "columns":["N"], "rows":[
                    {"id":"1", "cells":["one"]}, {"id":"2", "cells":["two"]}
                ]}
            ]}
        ]}))
        .unwrap();
        let mut go = |control, to| items(&navigate(&mut panel, control, to).unwrap());
        assert_eq!(go("t", Navigation::Next), ["select a"]);
        assert_eq!(go("t", Navigation::Right), ["toggle a true"]);
        assert_eq!(go("t", Navigation::Right), ["select b"]);
        assert_eq!(go("t", Navigation::Left), ["select a"]);
        assert_eq!(go("t", Navigation::Last), ["select c"]);
        assert_eq!(go("t", Navigation::Activate), ["activate c"]);
        assert_eq!(go("t", Navigation::PageUp), ["select a"]);
        assert_eq!(go("rows", Navigation::Last), ["select 2"]);
        assert_eq!(go("rows", Navigation::Activate), ["activate 2"]);
        assert!(navigate(&mut panel, "s", Navigation::Next).is_err());
        assert!(panel.lists["t"].reveal);
    }

    #[test]
    fn focused_lists_take_focus_and_send_declared_keys() {
        let ctx = egui::Context::default();
        let mut panel = serde_json::from_value::<Panel>(json!({"id":"p", "title":"P",
            "keys": ["J", "Shift+G"],
            "elements":[{"type":"tree", "id":"t", "nodes":[{"id":"a", "label":"a"}]}]
        }))
        .unwrap();
        panel.focus_request = Some(None);
        let key = |key, shift| egui::Event::Key {
            key,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers {
                shift,
                ..Default::default()
            },
        };
        let mut frame = |input: Vec<egui::Event>| {
            let mut events = Vec::new();
            let raw = egui::RawInput {
                events: input,
                ..Default::default()
            };
            let _ = ctx.run(raw, |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| show(ui, &mut panel, &mut events));
            });
            events
        };
        // Keys are not captured until a list has focus.
        assert!(frame(vec![key(Key::J, false)]).is_empty());
        frame(vec![]);
        let events = frame(vec![
            key(Key::J, false),
            key(Key::G, false),
            key(Key::G, true),
            key(Key::ArrowDown, false),
        ]);
        // Plain G was not declared; ArrowDown still reaches the tree.
        assert_eq!(items(&events), ["key t J", "key t Shift+G", "select a"]);
    }
}
