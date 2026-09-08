use crate::{
    config::{self, Action, Config},
    input,
    layout::{Axis, Direction, Layout, PaneId, Tab},
    pane::{self, Pane},
    render::color,
};
use anyhow::{Result, ensure};
use egui::{Color32, CornerRadius, Rect, Stroke, vec2};
use std::{
    collections::{BTreeMap, HashSet},
    path::PathBuf,
};

pub struct App {
    config: Config,
    config_path: PathBuf,
    panes: BTreeMap<PaneId, Pane>,
    tabs: Vec<Tab>,
    current: usize,
    next_id: u64,
    error: Option<String>,
    suppressed: HashSet<egui::Key>,
}

/// Width kept free at the left of the tab bar for the macOS traffic lights,
/// which float over the content once the titlebar is hidden.
const TITLEBAR_INSET: f32 = 70.0;
/// Height the tab bar needs so the traffic lights fit inside it.
const TITLEBAR_HEIGHT: f32 = 28.0;

/// With the titlebar hidden the tab bar takes its place, so let the empty space
/// beside the tabs drag the window and double-click to zoom, like a titlebar.
fn drag_window(ui: &mut egui::Ui) {
    let used = ui.min_rect();
    let free = Rect::from_min_max(
        egui::pos2(used.right(), used.top()),
        egui::pos2(ui.max_rect().right(), used.bottom()),
    );
    if free.width() <= 0.0 {
        return;
    }
    let response = ui.interact(
        free,
        ui.id().with("titlebar drag"),
        egui::Sense::click_and_drag(),
    );
    if response.double_clicked() {
        let maximized = ui.input(|i| i.viewport().maximized.unwrap_or(false));
        ui.ctx()
            .send_viewport_cmd(egui::ViewportCommand::Maximized(!maximized));
    } else if response.drag_started_by(egui::PointerButton::Primary) {
        ui.ctx().send_viewport_cmd(egui::ViewportCommand::StartDrag);
    }
}

pub fn fonts(config: &Config) -> Result<egui::FontDefinitions> {
    let mut fonts = egui::FontDefinitions::default();
    if let Some(path) = &config.font_path {
        let data = std::fs::read(path)?;
        fonts
            .font_data
            .insert("terminal".into(), egui::FontData::from_owned(data).into());
        fonts
            .families
            .get_mut(&egui::FontFamily::Monospace)
            .unwrap()
            .insert(0, "terminal".into());
    }
    Ok(fonts)
}

impl App {
    pub fn new(
        cc: &eframe::CreationContext<'_>,
        config: Config,
        config_path: PathBuf,
    ) -> Result<Self> {
        cc.egui_ctx.set_visuals(egui::Visuals::dark());
        cc.egui_ctx.set_fonts(fonts(&config)?);
        let pane = Pane::new(&config, &cc.egui_ctx, None)?;
        Ok(Self {
            config,
            config_path,
            panes: BTreeMap::from([(1, pane)]),
            tabs: vec![Tab::new(1, 1)],
            current: 0,
            next_id: 2,
            error: None,
            suppressed: HashSet::new(),
        })
    }

    fn spawn(&mut self, ctx: &egui::Context) -> Result<PaneId> {
        ensure!(
            self.panes.len() < self.config.multiplexer.max_panes,
            "Pane limit reached"
        );
        // A new pane starts where the pane it was spawned from currently sits.
        let cwd = self
            .tabs
            .get(self.current)
            .and_then(|tab| self.panes.get(&tab.active))
            .and_then(Pane::cwd);
        let pane = Pane::new(&self.config, ctx, cwd.as_deref())?;
        let id = self.next_id;
        self.next_id += 1;
        self.panes.insert(id, pane);
        Ok(id)
    }

    fn action(&mut self, action: Action, ctx: &egui::Context) -> Result<()> {
        let active = self.tabs[self.current].active;
        match action {
            Action::NewPane => {
                let tab = &self.tabs[self.current];
                let action = if tab.floats_visible && !tab.floats.is_empty() {
                    Action::NewFloating
                } else {
                    let rect = self.panes[&active].last_rect;
                    if rect.is_positive() && rect.height() > rect.width() {
                        Action::SplitDown
                    } else {
                        Action::SplitRight
                    }
                };
                self.action(action, ctx)?;
            }
            Action::SplitRight | Action::SplitDown => {
                let id = self.spawn(ctx)?;
                let tab = &mut self.tabs[self.current];
                let axis = if action == Action::SplitRight {
                    Axis::Horizontal
                } else {
                    Axis::Vertical
                };
                if let Some(root) = &mut tab.root {
                    root.split(
                        if tab.floats.contains(&active) {
                            root.first()
                        } else {
                            active
                        },
                        id,
                        axis,
                    );
                } else {
                    tab.root = Some(Layout::Pane(id));
                }
                tab.active = id;
                tab.zoomed = false;
            }
            Action::NewFloating => {
                let id = self.spawn(ctx)?;
                let tab = &mut self.tabs[self.current];
                tab.floats.push(id);
                tab.active = id;
                tab.floats_visible = true;
                tab.zoomed = false;
            }
            Action::ToggleFloating => {
                let tab = &mut self.tabs[self.current];
                let was_floating = tab.floats.contains(&active);
                tab.detach(active);
                if was_floating {
                    if let Some(root) = &mut tab.root {
                        root.split(root.first(), active, Axis::Horizontal);
                    } else {
                        tab.root = Some(Layout::Pane(active));
                    }
                } else {
                    tab.floats.push(active);
                    tab.floats_visible = true;
                }
            }
            Action::ToggleFloats => {
                if self.tabs[self.current].floats.is_empty() {
                    self.action(Action::NewFloating, ctx)?;
                } else {
                    self.tabs[self.current].toggle_floats();
                }
            }
            Action::FocusLeft | Action::FocusDown | Action::FocusUp | Action::FocusRight => {
                let direction = match action {
                    Action::FocusLeft => Direction::Left,
                    Action::FocusDown => Direction::Down,
                    Action::FocusUp => Direction::Up,
                    _ => Direction::Right,
                };
                let rects = self
                    .panes
                    .iter()
                    .map(|(id, pane)| (*id, pane.last_rect))
                    .collect::<Vec<_>>();
                self.tabs[self.current].focus_direction(direction, &rects);
            }
            Action::NextPane | Action::PreviousPane => {
                self.tabs[self.current].cycle(action == Action::PreviousPane)
            }
            Action::ClosePane => self.close(active, ctx),
            Action::ZoomPane => {
                let tab = &mut self.tabs[self.current];
                if tab.is_visible(tab.active) {
                    tab.zoomed = !tab.zoomed;
                }
            }
            Action::NewTab => {
                let id = self.spawn(ctx)?;
                self.tabs.push(Tab::new(id, id));
                self.current = self.tabs.len() - 1;
            }
            Action::NextTab => self.current = (self.current + 1) % self.tabs.len(),
            Action::PreviousTab => {
                self.current = (self.current + self.tabs.len() - 1) % self.tabs.len()
            }
            Action::ReloadConfig => {
                let config = Config::load(&self.config_path)?;
                let fonts = fonts(&config)?;
                for pane in self.panes.values_mut() {
                    pane::apply_config(&mut pane.terminal, &config)?;
                }
                ctx.set_fonts(fonts);
                self.config = config;
            }
            Action::Copy => {
                if let Some(text) = self.panes[&active].copy()? {
                    ctx.copy_text(text);
                }
            }
            Action::Paste => ctx.send_viewport_cmd(egui::ViewportCommand::RequestPaste),
        }
        Ok(())
    }

    fn close(&mut self, id: PaneId, ctx: &egui::Context) {
        let closed_rect = self
            .panes
            .remove(&id)
            .map(|pane| pane.last_rect)
            .filter(|rect| rect.is_positive() && rect.is_finite());
        for tab in &mut self.tabs {
            if !tab.ids().contains(&id) {
                continue;
            }
            let was_floating = tab.floats.contains(&id);
            tab.detach(id);
            if tab.root.is_none() {
                tab.floats_visible = true;
            }
            if tab.active == id {
                if was_floating && !tab.floats.is_empty() {
                    let nearest = tab
                        .floats
                        .iter()
                        .map(|candidate| {
                            let rect = self.panes[candidate].last_rect;
                            let distance = closed_rect
                                .filter(|_| rect.is_positive() && rect.is_finite())
                                .map_or(f32::INFINITY, |source| {
                                    source.center().distance_sq(rect.center())
                                });
                            (*candidate, distance)
                        })
                        .min_by(|a, b| a.1.total_cmp(&b.1).then_with(|| a.0.cmp(&b.0)));
                    if let Some((next, _)) = nearest {
                        tab.active = next;
                        tab.floats_visible = true;
                    }
                } else if let Some(next) = tab.navigation_ids().first() {
                    tab.active = *next;
                }
            }
        }
        self.tabs.retain(|t| !t.ids().is_empty());
        self.current = self.current.min(self.tabs.len().saturating_sub(1));
        if self.tabs.is_empty() {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
    }

    fn keyboard(&mut self, ctx: &egui::Context) -> Result<()> {
        let events = ctx.input(|i| i.events.clone());
        let modifiers = ctx.input(|i| i.modifiers);
        let mut terminal_events = vec![];
        let mut suppress_text = false;
        for event in events {
            if let egui::Event::Key {
                key,
                pressed,
                repeat,
                modifiers,
                ..
            } = &event
            {
                suppress_text = false;
                if !pressed && self.suppressed.remove(key) {
                    continue;
                }
                let action = self.config.keys.iter().find_map(|(s, a)| {
                    let shortcut = config::shortcut(s).ok()?;
                    let m = shortcut.modifiers;
                    (shortcut.logical_key == *key
                        && m.ctrl == modifiers.ctrl
                        && m.shift == modifiers.shift
                        && m.alt == modifiers.alt
                        && m.mac_cmd == modifiers.mac_cmd)
                        .then_some(*a)
                });
                if *pressed && let Some(action) = action {
                    self.flush_keys(&mut terminal_events)?;
                    self.suppressed.insert(*key);
                    suppress_text = true;
                    if !repeat {
                        self.action(action, ctx)?;
                    }
                    if self.tabs.is_empty() {
                        return Ok(());
                    }
                    continue;
                }
            }
            if suppress_text && matches!(event, egui::Event::Text(_)) {
                continue;
            }
            let active = self.tabs[self.current].active;
            if !self.tabs[self.current].is_visible(active) {
                continue;
            }
            match event {
                egui::Event::Paste(text) => {
                    self.flush_keys(&mut terminal_events)?;
                    self.panes.get_mut(&active).unwrap().paste(&text)?;
                }
                egui::Event::Copy => {
                    self.flush_keys(&mut terminal_events)?;
                    if modifiers.shift || modifiers.mac_cmd {
                        self.action(Action::Copy, ctx)?;
                    } else {
                        self.panes.get_mut(&active).unwrap().send(vec![3])?;
                    }
                }
                egui::Event::Cut => {
                    self.flush_keys(&mut terminal_events)?;
                    self.panes.get_mut(&active).unwrap().send(vec![24])?;
                }
                other => terminal_events.push(other),
            }
        }
        self.flush_keys(&mut terminal_events)
    }

    fn flush_keys(&mut self, events: &mut Vec<egui::Event>) -> Result<()> {
        if events.is_empty() {
            return Ok(());
        }
        let active = self.tabs[self.current].active;
        if !self.tabs[self.current].is_visible(active) {
            events.clear();
            return Ok(());
        }
        let pane = self.panes.get_mut(&active).unwrap();
        let encoded = input::encode(&pane.terminal, events)?;
        pane.send(encoded)?;
        events.clear();
        Ok(())
    }
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        if self.tabs.is_empty() {
            return;
        }
        for pane in self.panes.values_mut() {
            pane.drain(ctx);
        }
        grab_keys(ctx);
        if let Err(e) = self.keyboard(ctx) {
            self.error = Some(e.to_string());
        }
        if self.tabs.is_empty() {
            return;
        }
        let mut action = None;
        egui::TopBottomPanel::top("tabs").show(ctx, |ui| {
            ui.horizontal_wrapped(|ui| {
                if cfg!(target_os = "macos") {
                    ui.add_space(TITLEBAR_INSET);
                    ui.set_min_height(TITLEBAR_HEIGHT);
                }
                for (index, tab) in self.tabs.iter().enumerate() {
                    let title = self.panes[&tab.active].terminal.title().unwrap_or("");
                    let label = if title.is_empty() {
                        format!("{}: shell", index + 1)
                    } else {
                        format!(
                            "{}: {}",
                            index + 1,
                            title.chars().take(28).collect::<String>()
                        )
                    };
                    if ui.selectable_label(self.current == index, label).clicked() {
                        self.current = index;
                    }
                }
                if ui.button("+").on_hover_text("New tab").clicked() {
                    action = Some(Action::NewTab);
                }
                ui.menu_button("Pane", |ui| {
                    for (label, a) in [
                        ("New pane", Action::NewPane),
                        ("Split right", Action::SplitRight),
                        ("Split down", Action::SplitDown),
                        ("New floating pane", Action::NewFloating),
                        ("Float / dock", Action::ToggleFloating),
                        ("Show / hide floating panes", Action::ToggleFloats),
                        ("Zoom / restore", Action::ZoomPane),
                        ("Next pane", Action::NextPane),
                        ("Close pane", Action::ClosePane),
                    ] {
                        if ui.button(label).clicked() {
                            action = Some(a);
                            ui.close();
                        }
                    }
                });
                ui.menu_button("Settings", |ui| {
                    if ui.button("Reload config").clicked() {
                        action = Some(Action::ReloadConfig);
                        ui.close();
                    }
                });
            });
            if cfg!(target_os = "macos") {
                drag_window(ui);
            }
        });
        if let Some(error) = self.error.clone() {
            egui::TopBottomPanel::bottom("error").show(ctx, |ui| {
                ui.horizontal_wrapped(|ui| {
                    ui.colored_label(Color32::LIGHT_RED, error);
                    if ui.button("Dismiss").clicked() {
                        self.error = None;
                    }
                });
            });
        }
        let bg = color(config::rgb(&self.config.background).unwrap());
        let tab = &mut self.tabs[self.current];
        let mut tiled = vec![];
        let mut workspace = Rect::NOTHING;
        egui::CentralPanel::default()
            .frame(egui::Frame::NONE.fill(bg))
            .show(ctx, |ui| {
                workspace = ui.max_rect();
                if tab.zoomed {
                    tiled.push((tab.active, workspace));
                } else if let Some(root) = &mut tab.root {
                    root.arrange(ui, workspace, &mut tiled, 1);
                }
                for (id, rect) in &tiled {
                    let pane = self.panes.get_mut(id).unwrap();
                    let active = tab.active == *id;
                    if !tab.zoomed {
                        pane.last_rect = *rect;
                    }
                    if show_pane(ui, *id, pane, *rect, &self.config, active) {
                        tab.active = *id;
                    }
                }
            });
        if tab.floats_visible && !tab.zoomed {
            for (index, id) in tab.floats.clone().into_iter().enumerate() {
                let pane = self.panes.get_mut(&id).unwrap();
                let window_id = egui::Id::new(("float", tab.id, id));
                let active = tab.active == id;
                let (pos, size) = float_placement(workspace, index, &self.config.multiplexer);
                let result = egui::Window::new(pane_title(id, pane))
                    .id(window_id)
                    .title_bar(false)
                    .constrain_to(workspace)
                    .default_pos(pos)
                    .default_size(size)
                    .min_size(vec2(160.0, 100.0))
                    .frame(egui::Frame::NONE.fill(bg))
                    .show(ctx, |ui| {
                        let rect = ui.available_rect_before_wrap();
                        ui.allocate_rect(rect, egui::Sense::hover());
                        pane.last_rect = rect;
                        if show_pane(ui, id, pane, rect, &self.config, active) {
                            tab.active = id;
                        }
                    });
                if let Some(result) = result
                    && result.response.contains_pointer()
                    && ctx.input(|i| i.pointer.any_pressed())
                {
                    tab.active = id;
                }
                if active {
                    ctx.move_to_top(egui::LayerId::new(egui::Order::Middle, window_id));
                }
            }
        }
        let active = tab.active;
        for (id, pane) in &mut self.panes {
            if let Err(e) =
                pane.focus(*id == active && tab.is_visible(*id) && ctx.input(|i| i.focused))
            {
                pane.error = Some(e.to_string());
            }
        }
        if !self.tabs.is_empty()
            && let Some(action) = action
            && let Err(e) = self.action(action, ctx)
        {
            self.error = Some(e.to_string());
        }
    }
}

/// Parks keyboard focus on a widget that never exists, with every navigation key
/// filtered out. Otherwise egui spends Tab, the arrows and Escape on moving focus
/// between the tab bar and the menu buttons instead of leaving them for the shell.
fn grab_keys(ctx: &egui::Context) {
    let id = egui::Id::new("keys");
    // Registers the id for this pass so egui's dead-mans-switch keeps the focus.
    ctx.check_for_id_clash(id, Rect::NOTHING, "key sink");
    ctx.memory_mut(|m| {
        if m.focused() != Some(id) {
            m.request_focus(id);
        }
        // Only applies once the sink has held focus for a full frame.
        m.set_focus_lock_filter(
            id,
            egui::EventFilter {
                tab: true,
                horizontal_arrows: true,
                vertical_arrows: true,
                escape: true,
            },
        );
    });
}

/// Corner rounding shared by a pane's border and its title bar.
const CORNER: u8 = 5;

fn show_pane(
    ui: &mut egui::Ui,
    id: PaneId,
    pane: &mut Pane,
    rect: Rect,
    config: &Config,
    active: bool,
) -> bool {
    let accent = color(config::rgb(&config.accent).unwrap());
    let title_rect = Rect::from_min_size(rect.min, vec2(rect.width(), 25.0_f32.min(rect.height())));
    // The title bar only rounds off the top of the pane, unless the pane is so
    // short that the bar is all there is of it.
    let bottom = if title_rect.height() < rect.height() {
        0
    } else {
        CORNER
    };
    ui.painter().rect_filled(
        title_rect,
        CornerRadius {
            nw: CORNER,
            ne: CORNER,
            sw: bottom,
            se: bottom,
        },
        Color32::from_gray(if active { 43 } else { 31 }),
    );
    ui.painter()
        .with_clip_rect(title_rect.shrink(3.0).intersect(ui.clip_rect()))
        .text(
            title_rect.min + vec2(7.0, 4.0),
            egui::Align2::LEFT_TOP,
            pane_title(id, pane),
            egui::FontId::proportional(13.0),
            if active { accent } else { Color32::GRAY },
        );
    // Click-only leaves dragging to the floating window's existing move handler.
    let header = ui.interact(
        title_rect,
        ui.id().with(("title", id)),
        egui::Sense::click(),
    );
    let mut focused =
        header.clicked() || (header.contains_pointer() && ui.input(|i| i.pointer.any_pressed()));
    let content = Rect::from_min_max(egui::pos2(rect.left(), title_rect.bottom()), rect.max);
    // The pane rectangle is already allocated; a layout scope would grow its parent.
    let mut content_ui = ui.new_child(egui::UiBuilder::new().id_salt(id).max_rect(content));
    content_ui.set_clip_rect(content.intersect(ui.clip_rect()));
    match pane.show(&mut content_ui, content, config, active) {
        Ok(clicked) => focused |= clicked,
        Err(e) => pane.error = Some(e.to_string()),
    }
    ui.painter().rect_stroke(
        rect,
        CORNER,
        Stroke::new(
            1.0_f32,
            if active {
                accent
            } else {
                Color32::from_gray(54)
            },
        ),
        egui::StrokeKind::Inside,
    );
    focused
}

/// Where a floating pane opens, from its place in the tab's float list rather than
/// its pane id: the first one is centred and every later one steps down and right
/// from it, so the same slot always lands in the same spot. The cascade wraps
/// before it can walk a float off the far edge.
fn float_placement(
    workspace: Rect,
    index: usize,
    config: &config::Multiplexer,
) -> (egui::Pos2, egui::Vec2) {
    // The fractions are of the workspace, so on a large display they hand a scratch
    // terminal far more room than it needs; the maximums keep it in hand.
    let size = (workspace.size() * vec2(config.floating_width, config.floating_height))
        .min(vec2(config.floating_max_width, config.floating_max_height));
    let step = (index % 6) as f32 * 28.0;
    (workspace.center() - size / 2.0 + vec2(step, step), size)
}

fn pane_title(id: PaneId, pane: &Pane) -> String {
    let title = pane.terminal.title().unwrap_or("");
    format!(
        "{id}  {}{}",
        if title.is_empty() { "shell" } else { title },
        if pane.dead { " [exited]" } else { "" }
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_app(ctx: &egui::Context) -> App {
        let config = Config {
            shell: vec!["/bin/sh".into()],
            ..Default::default()
        };
        let pane = Pane::new(&config, ctx, None).unwrap();
        App {
            config,
            config_path: PathBuf::new(),
            panes: BTreeMap::from([(1, pane)]),
            tabs: vec![Tab::new(1, 1)],
            current: 0,
            next_id: 2,
            error: None,
            suppressed: HashSet::new(),
        }
    }

    #[test]
    fn floating_pane_size_stays_constant_across_frames() {
        let ctx = egui::Context::default();
        let mut app = test_app(&ctx);
        let mut sizes = vec![];
        for frame in 0..20 {
            let input = egui::RawInput {
                screen_rect: Some(Rect::from_min_size(egui::Pos2::ZERO, vec2(1000.0, 800.0))),
                time: Some(frame as f64 / 60.0),
                ..Default::default()
            };
            let _ = ctx.run(input, |ctx| {
                let window = egui::Window::new("float")
                    .title_bar(false)
                    .default_size(vec2(400.0, 240.0))
                    .frame(egui::Frame::NONE)
                    .show(ctx, |ui| {
                        let rect = ui.available_rect_before_wrap();
                        ui.allocate_rect(rect, egui::Sense::hover());
                        show_pane(
                            ui,
                            1,
                            app.panes.get_mut(&1).unwrap(),
                            rect,
                            &app.config,
                            true,
                        );
                    })
                    .unwrap();
                if frame >= 2 {
                    sizes.push(window.response.rect.size());
                }
            });
        }
        assert!(
            sizes.iter().all(|size| *size == sizes[0]),
            "floating pane grew: {sizes:?}"
        );
    }

    #[test]
    fn floats_open_centered_then_cascade_and_stay_within_the_maximums() {
        let config = config::Multiplexer::default();
        let workspace = Rect::from_min_size(egui::pos2(0.0, 30.0), vec2(3000.0, 1800.0));
        let (first, size) = float_placement(workspace, 0, &config);
        assert_eq!(
            size,
            vec2(config.floating_max_width, config.floating_max_height),
            "a large workspace must not hand out a proportionally large float"
        );
        assert_eq!(
            first + size / 2.0,
            workspace.center(),
            "the first is centered"
        );

        let (second, _) = float_placement(workspace, 1, &config);
        assert!(
            second.x > first.x && second.y > first.y,
            "later floats step off it"
        );
        // Six slots later the cascade is back where it started, so it cannot walk away.
        assert_eq!(float_placement(workspace, 6, &config).0, first);
        assert_eq!(float_placement(workspace, 7, &config).0, second);

        // A small workspace keeps the fractions instead of the maximums.
        let small = Rect::from_min_size(egui::Pos2::ZERO, vec2(800.0, 600.0));
        let (pos, size) = float_placement(small, 0, &config);
        assert_eq!(
            size,
            small.size() * vec2(config.floating_width, config.floating_height)
        );
        assert_eq!(pos + size / 2.0, small.center());
    }

    #[test]
    fn toggle_creates_first_float_then_reuses_it_and_close_removes_focus() {
        let ctx = egui::Context::default();
        let mut app = test_app(&ctx);
        app.action(Action::ToggleFloats, &ctx).unwrap();
        assert_eq!(app.tabs[0].floats, vec![2]);
        assert_eq!(app.tabs[0].active, 2);
        assert!(app.tabs[0].floats_visible);
        app.action(Action::ToggleFloats, &ctx).unwrap();
        assert!(!app.tabs[0].floats_visible);
        assert_eq!(app.tabs[0].active, 1);
        app.action(Action::ToggleFloats, &ctx).unwrap();
        assert_eq!(app.panes.len(), 2);
        assert_eq!(app.tabs[0].active, 2);
        app.action(Action::ClosePane, &ctx).unwrap();
        assert_eq!(app.tabs[0].active, 1);
        assert!(!app.panes.contains_key(&2));
        app.action(Action::ToggleFloats, &ctx).unwrap();
        assert_eq!(app.tabs[0].floats, vec![3]);
        assert!(app.tabs[0].floats_visible);
    }

    #[test]
    fn new_pane_floats_only_when_existing_floats_are_visible() {
        let ctx = egui::Context::default();
        let mut app = test_app(&ctx);
        app.action(Action::NewPane, &ctx).unwrap();
        assert!(app.tabs[0].floats.is_empty());
        assert_eq!(app.tabs[0].active, 2);

        app.action(Action::ToggleFloats, &ctx).unwrap();
        assert_eq!(app.tabs[0].floats, vec![3]);
        app.action(Action::NewPane, &ctx).unwrap();
        assert_eq!(app.tabs[0].floats, vec![3, 4]);

        app.action(Action::ToggleFloats, &ctx).unwrap();
        app.action(Action::NewPane, &ctx).unwrap();
        assert_eq!(app.tabs[0].floats, vec![3, 4]);
        assert_eq!(app.tabs[0].active, 5);
        assert!(!app.tabs[0].floats_visible);

        app.action(Action::ToggleFloats, &ctx).unwrap();
        app.action(Action::ClosePane, &ctx).unwrap();
        app.action(Action::ClosePane, &ctx).unwrap();
        assert!(app.tabs[0].floats.is_empty());
        assert!(!app.tabs[0].floats_visible);
        app.action(Action::NewPane, &ctx).unwrap();
        assert!(app.tabs[0].floats.is_empty());
        assert_eq!(app.tabs[0].active, 6);

        app.action(Action::ToggleFloats, &ctx).unwrap();
        assert_eq!(app.tabs[0].floats, vec![7]);
        app.action(Action::ToggleFloating, &ctx).unwrap();
        assert!(!app.tabs[0].floats_visible);
        app.action(Action::NewPane, &ctx).unwrap();
        assert!(app.tabs[0].floats.is_empty());
        assert_eq!(app.tabs[0].active, 8);
    }

    #[test]
    fn closing_float_focuses_another_float_before_tiles() {
        let ctx = egui::Context::default();
        let mut app = test_app(&ctx);
        app.action(Action::NewFloating, &ctx).unwrap();
        app.action(Action::NewFloating, &ctx).unwrap();
        app.action(Action::ClosePane, &ctx).unwrap();
        assert_eq!(app.tabs[0].active, 2);
        app.action(Action::NewFloating, &ctx).unwrap();
        app.close(1, &ctx);
        app.action(Action::ToggleFloats, &ctx).unwrap();
        app.action(Action::ClosePane, &ctx).unwrap();
        assert_eq!(app.tabs[0].active, 2);
        assert!(app.tabs[0].floats_visible);
    }

    #[test]
    fn closing_float_selects_nearest_float_not_first_or_nearer_tile() {
        let ctx = egui::Context::default();
        let mut app = test_app(&ctx);
        for _ in 0..3 {
            app.action(Action::NewFloating, &ctx).unwrap();
        }
        for (id, x, y) in [
            (1, 200.0, 200.0),
            (2, 0.0, 0.0),
            (3, 180.0, 180.0),
            (4, 200.0, 200.0),
        ] {
            app.panes.get_mut(&id).unwrap().last_rect =
                Rect::from_min_size(egui::pos2(x, y), vec2(100.0, 100.0));
        }
        app.action(Action::ClosePane, &ctx).unwrap();
        assert_eq!(app.tabs[0].active, 3);
        app.action(Action::ClosePane, &ctx).unwrap();
        assert_eq!(app.tabs[0].active, 2);
        app.action(Action::ClosePane, &ctx).unwrap();
        assert_eq!(app.tabs[0].active, 1);
    }
}
