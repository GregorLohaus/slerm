use crate::{
    config::{self, Action, Config},
    input,
    layout::{Axis, Direction, Layout, PaneId, Tab},
    pane::{self, Pane},
    plugin_api::{Event, Operation, PaneState, State, TabState},
    plugins::{self, Host},
    theme::{self, Theme},
};
use anyhow::{Result, ensure};
use egui::{CornerRadius, Rect, Stroke, vec2};
use std::{
    collections::{BTreeMap, HashSet},
    path::PathBuf,
};

pub struct App {
    config: Config,
    theme: Theme,
    config_path: PathBuf,
    panes: BTreeMap<PaneId, Pane>,
    tabs: Vec<Tab>,
    current: usize,
    next_id: u64,
    error: Option<String>,
    suppressed: HashSet<egui::Key>,
    plugins: Host,
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
    crate::fonts::load(config)
}

impl App {
    pub fn new(
        cc: &eframe::CreationContext<'_>,
        config: Config,
        config_path: PathBuf,
    ) -> Result<Self> {
        let theme = config.theme()?;
        cc.egui_ctx.set_visuals(theme.visuals());
        cc.egui_ctx.set_fonts(fonts(&config)?);
        let pane = Pane::new(&config, &theme, &cc.egui_ctx, None)?;
        let config_dir = config_path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or(std::path::Path::new("."));
        let (plugins, errors) = Host::start(&config.plugins, config_dir, &cc.egui_ctx);
        Ok(Self {
            config,
            theme,
            config_path,
            panes: BTreeMap::from([(1, pane)]),
            tabs: vec![Tab::new(1, 1)],
            current: 0,
            next_id: 2,
            error: (!errors.is_empty()).then(|| errors.join("\n")),
            suppressed: HashSet::new(),
            plugins,
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
        let pane = Pane::new(&self.config, &self.theme, ctx, cwd.as_deref())?;
        let id = self.next_id;
        self.next_id += 1;
        self.panes.insert(id, pane);
        Ok(id)
    }

    /// Switches theme for this session. Every open pane is retinted, since a
    /// terminal keeps its palette rather than asking for it each frame.
    fn set_theme(&mut self, name: &str, ctx: &egui::Context) -> Result<()> {
        let previous = std::mem::replace(&mut self.config.theme, name.to_owned());
        self.theme = self.config.theme().inspect_err(|_| {
            self.config.theme = previous;
        })?;
        for pane in self.panes.values_mut() {
            pane::apply_config(&mut pane.terminal, &self.config, &self.theme)?;
        }
        ctx.set_visuals(self.theme.visuals());
        Ok(())
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
                self.theme = config.theme()?;
                for pane in self.panes.values_mut() {
                    pane::apply_config(&mut pane.terminal, &config, &self.theme)?;
                }
                ctx.set_fonts(fonts);
                ctx.set_visuals(self.theme.visuals());
                self.config = config;
            }
            Action::Copy => {
                if let Some(text) = self.panes[&active].copy()? {
                    ctx.copy_text(text);
                }
            }
            Action::Paste => ctx.send_viewport_cmd(egui::ViewportCommand::RequestPaste),
            Action::OpenScrollback => {
                self.panes.get_mut(&active).unwrap().open_scrollback(
                    &self.config,
                    &self.theme,
                    ctx,
                )?;
            }
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
            let previous_order = tab.navigation_ids();
            let closed_index = previous_order.iter().position(|pane| *pane == id);
            tab.detach(id);
            if tab.root.is_none() {
                tab.floats_visible = true;
            }
            if tab.active == id {
                let nearest = tab
                    .navigation_ids()
                    .into_iter()
                    .map(|candidate| {
                        let rect = self.panes[&candidate].last_rect;
                        let distance = closed_rect
                            .filter(|_| rect.is_positive() && rect.is_finite())
                            .map_or(f32::INFINITY, |source| {
                                source.center().distance_sq(rect.center())
                            });
                        let order_distance = closed_index
                            .zip(previous_order.iter().position(|pane| *pane == candidate))
                            .map_or(usize::MAX, |(closed, next)| closed.abs_diff(next));
                        (candidate, distance, order_distance)
                    })
                    .min_by(|a, b| {
                        a.1.total_cmp(&b.1)
                            .then_with(|| a.2.cmp(&b.2))
                            .then_with(|| a.0.cmp(&b.0))
                    });
                if let Some((next, _, _)) = nearest {
                    tab.active = next;
                }
            }
        }
        self.tabs.retain(|t| !t.ids().is_empty());
        self.current = self.current.min(self.tabs.len().saturating_sub(1));
        if self.tabs.is_empty() {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
    }

    fn plugin_state(&self) -> State {
        State {
            active_tab: self.tabs.get(self.current).map(|tab| tab.id),
            tabs: self
                .tabs
                .iter()
                .map(|tab| TabState {
                    id: tab.id,
                    active_pane: tab.active,
                    floats_visible: tab.floats_visible,
                    zoomed: tab.zoomed,
                    panes: tab
                        .ids()
                        .iter()
                        .map(|id| PaneState {
                            id: *id,
                            floating: tab.floats.contains(id),
                            title: self.panes[id].terminal.title().unwrap_or("").into(),
                            exited: self.panes[id].dead,
                            cwd: self.panes[id]
                                .cwd()
                                .and_then(|path| path.to_str().map(str::to_owned)),
                        })
                        .collect(),
                })
                .collect(),
        }
    }

    fn plugin_request(
        &mut self,
        index: usize,
        operation: Operation,
        ctx: &egui::Context,
    ) -> Result<serde_json::Value> {
        use serde_json::json;
        match operation {
            Operation::GetState {} => return Ok(serde_json::to_value(self.plugin_state())?),
            Operation::OpenTerminal { program, args, cwd } => {
                ensure!(!self.tabs.is_empty(), "no active tab");
                ensure!(
                    self.panes.len() < self.config.multiplexer.max_panes,
                    "Pane limit reached"
                );
                ensure!(
                    !program.is_empty() && !program.contains('\0'),
                    "invalid program"
                );
                ensure!(
                    args.len() <= 256 && args.iter().all(|arg| !arg.contains('\0')),
                    "invalid program arguments"
                );
                let cwd = std::path::Path::new(&cwd);
                ensure!(
                    cwd.is_absolute() && cwd.is_dir(),
                    "working directory must be an existing absolute directory"
                );
                let mut config = self.config.clone();
                config.shell = std::iter::once(program).chain(args).collect();
                let pane = Pane::new(&config, &self.theme, ctx, Some(cwd))?;
                let id = self.next_id;
                self.next_id += 1;
                self.panes.insert(id, pane);
                let tab = &mut self.tabs[self.current];
                let target = if tab.floats.contains(&tab.active) {
                    tab.root.as_ref().map(Layout::first)
                } else {
                    Some(tab.active)
                };
                if let Some(root) = &mut tab.root {
                    root.split(target.unwrap(), id, Axis::Horizontal);
                } else {
                    tab.root = Some(Layout::Pane(id));
                }
                tab.active = id;
                tab.floats_visible = false;
                tab.zoomed = false;
                ctx.memory_mut(|memory| memory.request_focus(egui::Id::new("keys")));
                return Ok(json!(crate::plugin_api::PaneState {
                    id,
                    title: String::new(),
                    floating: false,
                    exited: false,
                    cwd: cwd.to_str().map(str::to_owned),
                }));
            }
            Operation::Execute { action } => {
                ensure!(!self.tabs.is_empty(), "no active tab");
                self.action(action, ctx)?;
            }
            Operation::FocusPane { pane } => {
                let index = self
                    .tabs
                    .iter()
                    .position(|tab| tab.ids().contains(&pane))
                    .ok_or_else(|| anyhow::anyhow!("unknown pane {pane}"))?;
                self.current = index;
                let tab = &mut self.tabs[index];
                tab.active = pane;
                tab.floats_visible = tab.floats.contains(&pane);
            }
            Operation::FocusTab { tab } => {
                self.current = self
                    .tabs
                    .iter()
                    .position(|candidate| candidate.id == tab)
                    .ok_or_else(|| anyhow::anyhow!("unknown tab {tab}"))?;
            }
            Operation::ReadTerminal { pane } => {
                use libghostty_vt::fmt::{Formatter, FormatterOptions};
                let pane = self
                    .panes
                    .get(&pane)
                    .ok_or_else(|| anyhow::anyhow!("unknown pane {pane}"))?;
                let bytes = Formatter::new(
                    &pane.terminal,
                    FormatterOptions::new().with_trim(true).with_unwrap(true),
                )?
                .format_alloc(None)?;
                ensure!(
                    bytes.len() <= 128 * 1024,
                    "terminal snapshot exceeds 128 KiB"
                );
                return Ok(json!(String::from_utf8_lossy(&bytes)));
            }
            Operation::WriteTerminal { pane, text } => {
                self.panes
                    .get_mut(&pane)
                    .ok_or_else(|| anyhow::anyhow!("unknown pane {pane}"))?
                    .send(text.into_bytes())?;
            }
            Operation::RegisterCommand { command } => {
                ensure!(plugins::valid_id(&command.id), "invalid command id");
                ensure!(
                    !command.title.trim().is_empty() && command.title.len() <= 200,
                    "invalid command title"
                );
                let commands = &mut self.plugins.runtimes[index].commands;
                ensure!(
                    commands.contains_key(&command.id) || commands.len() < 64,
                    "command limit reached"
                );
                commands.insert(command.id.clone(), command);
            }
            Operation::UnregisterCommand { command } => {
                self.plugins.runtimes[index].commands.remove(&command);
            }
            Operation::SetPanel { panel } => {
                panel.validate()?;
                let panels = &mut self.plugins.runtimes[index].panels;
                ensure!(
                    panels.contains_key(&panel.id) || panels.len() < 8,
                    "panel limit reached"
                );
                panels.insert(panel.id.clone(), panel);
            }
            Operation::RemovePanel { panel } => {
                self.plugins.runtimes[index].panels.remove(&panel);
            }
            Operation::ReportError { message } => {
                self.error = Some(format!(
                    "Plugin {}: {message}",
                    self.plugins.runtimes[index].id
                ));
            }
        }
        Ok(serde_json::Value::Null)
    }

    fn poll_plugins(&mut self, ctx: &egui::Context) {
        use serde_json::json;
        if self.plugins.runtimes.is_empty() {
            return;
        }
        for index in 0..self.plugins.runtimes.len() {
            // Bound work per frame even when a plugin continuously sends requests.
            for count in 0..32 {
                let Some(request) = self.plugins.runtimes[index].poll() else {
                    break;
                };
                let response = match request {
                    Ok(request) => {
                        let response = match self.plugin_request(index, request.operation, ctx) {
                            Ok(result) => json!({"result": result}),
                            Err(error) => json!({"error": error.to_string()}),
                        };
                        request
                            .reply
                            .try_send(response)
                            .map_err(|e| anyhow::anyhow!(e.to_string()))
                    }
                    Err(error) => Err(anyhow::anyhow!(error)),
                };
                if let Err(e) = response {
                    let plugin = &mut self.plugins.runtimes[index];
                    self.error = Some(format!("Plugin {}: {e}", plugin.id));
                    plugin.stop();
                    break;
                }
                if count == 31 {
                    ctx.request_repaint();
                }
            }
        }
        // Directory changes need polling even when the shell emits no OSC 7.
        // Bound OS process-directory lookups rather than doing them each frame.
        let now = std::time::Instant::now();
        if let Some(next) = self.plugins.next_state_poll
            && now < next
        {
            ctx.request_repaint_after(next - now);
            return;
        }
        self.plugins.next_state_poll = Some(now + std::time::Duration::from_millis(250));
        ctx.request_repaint_after(std::time::Duration::from_millis(250));
        let state = self.plugin_state();
        let serialized = serde_json::to_value(&state).unwrap();
        for plugin in &mut self.plugins.runtimes {
            if plugin.is_running() && plugin.last_state.as_ref() != Some(&serialized) {
                if let Err(e) = plugin.send(Event::State {
                    state: state.clone(),
                }) {
                    self.error = Some(format!("Plugin {}: {e}", plugin.id));
                    plugin.stop();
                } else {
                    plugin.last_state = Some(serialized.clone());
                }
            }
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
        #[cfg(target_os = "macos")]
        if let Some(error) = crate::macos::take_error() {
            self.error = Some(error);
        }
        if self.tabs.is_empty() {
            return;
        }
        for pane in self.panes.values_mut() {
            pane.drain(ctx);
        }
        self.poll_plugins(ctx);
        if self.tabs.is_empty() {
            return;
        }
        // Native plugin text fields must receive typing without forwarding it to the PTY.
        let editing_ui = ctx.memory(|m| m.focused().is_some_and(|id| id != egui::Id::new("keys")));
        if !editing_ui {
            grab_keys(ctx);
            if let Err(e) = self.keyboard(ctx) {
                self.error = Some(e.to_string());
            }
        }
        if self.tabs.is_empty() {
            return;
        }
        let mut action = None;
        let mut picked = None;
        let mut plugin_command = None;
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
                        ("Open scrollback in editor", Action::OpenScrollback),
                    ] {
                        if ui.button(label).clicked() {
                            action = Some(a);
                            ui.close();
                        }
                    }
                });
                if self.plugins.runtimes.iter().any(|p| !p.commands.is_empty()) {
                    ui.menu_button("Plugins", |ui| {
                        for (index, plugin) in self.plugins.runtimes.iter().enumerate() {
                            ui.menu_button(&plugin.id, |ui| {
                                for command in plugin.commands.values() {
                                    if ui.button(&command.title).clicked() {
                                        plugin_command = Some((index, command.id.clone()));
                                        ui.close();
                                    }
                                }
                            });
                        }
                    });
                }
                ui.menu_button("Settings", |ui| {
                    if ui.button("Reload config").clicked() {
                        action = Some(Action::ReloadConfig);
                        ui.close();
                    }
                    ui.menu_button("Theme", |ui| {
                        ui.label("Applies now; set `theme` in the config to keep it.");
                        egui::ScrollArea::vertical()
                            .max_height(420.0)
                            .show(ui, |ui| {
                                for name in theme::names() {
                                    let current = self.config.theme == name;
                                    if ui.selectable_label(current, name).clicked() && !current {
                                        picked = Some(name);
                                        ui.close();
                                    }
                                }
                            });
                    });
                });
            });
            if cfg!(target_os = "macos") {
                drag_window(ui);
            }
        });
        if let Some(error) = self.error.clone() {
            egui::TopBottomPanel::bottom("error").show(ctx, |ui| {
                ui.horizontal_wrapped(|ui| {
                    ui.colored_label(theme::color(self.theme.ansi[9]), error);
                    if ui.button("Dismiss").clicked() {
                        self.error = None;
                    }
                });
            });
        }
        for error in self.plugins.show_panels(ctx) {
            self.error = Some(error);
        }
        let bg = theme::color(self.theme.background);
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
                    if show_pane(ui, *id, pane, *rect, &self.config, &self.theme, active) {
                        tab.active = *id;
                    }
                }
            });
        if tab.floats_visible && !tab.zoomed {
            for (index, id) in tab.floats.clone().into_iter().enumerate() {
                let pane = self.panes.get_mut(&id).unwrap();
                let window_id = egui::Id::new(("float", tab.id, id));
                let active = tab.active == id;
                let window = egui::Window::new(pane_title(id, pane))
                    .id(window_id)
                    .title_bar(false)
                    .constrain_to(workspace)
                    .min_size(vec2(160.0, 100.0))
                    .frame(egui::Frame::NONE.fill(bg));
                let result = position_float(
                    ctx,
                    window,
                    window_id,
                    index,
                    workspace,
                    &self.config.multiplexer,
                )
                .show(ctx, |ui| {
                    let rect = ui.available_rect_before_wrap();
                    ui.allocate_rect(rect, egui::Sense::hover());
                    pane.last_rect = rect;
                    if show_pane(ui, id, pane, rect, &self.config, &self.theme, active) {
                        tab.active = id;
                    }
                });
                if let Some(result) = result {
                    remember_float_interaction(ctx, window_id, &result.response);
                    if result.response.contains_pointer() && ctx.input(|i| i.pointer.any_pressed())
                    {
                        tab.active = id;
                    }
                }
                if active {
                    ctx.move_to_top(egui::LayerId::new(egui::Order::Middle, window_id));
                }
            }
        }
        let active = tab.active;
        let active_visible = tab.is_visible(active);
        if let Some((index, command)) = plugin_command
            && let Err(e) = self.plugins.invoke(index, &command)
        {
            self.error = Some(e.to_string());
        }
        for (id, pane) in &mut self.panes {
            if let Err(e) = pane
                .focus(*id == active && active_visible && !editing_ui && ctx.input(|i| i.focused))
            {
                pane.error = Some(e.to_string());
            }
        }
        if let Some(name) = picked
            && let Err(e) = self.set_theme(name, ctx)
        {
            self.error = Some(e.to_string());
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
const CORNER: u8 = theme::CORNER_RADIUS;

fn show_pane(
    ui: &mut egui::Ui,
    id: PaneId,
    pane: &mut Pane,
    rect: Rect,
    config: &Config,
    theme: &Theme,
    active: bool,
) -> bool {
    let accent = theme::color(theme.accent);
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
        theme::color(if active {
            theme.chrome_active_bg
        } else {
            theme.chrome_bg
        }),
    );
    ui.painter()
        .with_clip_rect(title_rect.shrink(3.0).intersect(ui.clip_rect()))
        .text(
            title_rect.min + vec2(7.0, 4.0),
            egui::Align2::LEFT_TOP,
            pane_title(id, pane),
            egui::FontId::proportional(13.0),
            if active {
                accent
            } else {
                theme::color(theme.chrome_fg)
            },
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
    match pane.show(&mut content_ui, content, config, theme, active) {
        Ok(clicked) => focused |= clicked,
        Err(e) => pane.error = Some(e.to_string()),
    }
    // Layer-aware hit testing prevents a tile behind a floating pane from
    // consuming the drop. Some backends omit the pointer during native drags;
    // in that case the active, visible pane receives it.
    let drop_target =
        ui.rect_contains_pointer(rect) || (active && ui.input(|i| i.pointer.hover_pos().is_none()));
    let hovering_files = drop_target && ui.input(|i| !i.raw.hovered_files.is_empty());
    if drop_target {
        let files = ui.input_mut(|i| std::mem::take(&mut i.raw.dropped_files));
        match input::dropped_file_paths(&files) {
            Ok(Some(text)) => match pane.paste(&text) {
                Ok(()) => focused = true,
                Err(e) => pane.error = Some(e.to_string()),
            },
            Ok(None) => {}
            Err(e) => pane.error = Some(e.to_string()),
        }
    }
    ui.painter().rect_stroke(
        rect,
        CORNER,
        Stroke::new(
            if hovering_files {
                2.0
            } else {
                theme::BORDER_WIDTH
            },
            if active || hovering_files {
                accent
            } else {
                theme::color(theme.border)
            },
        ),
        egui::StrokeKind::Inside,
    );
    focused
}

/// Where a floating pane opens, from its place in the tab's float list rather than
/// its pane id: the first one is centred and every later one steps down and right
/// from it, so the same slot always lands in the same spot. The cascade wraps
/// every six panes, with each new stack shifted slightly to the right.
fn float_placement(
    workspace: Rect,
    index: usize,
    config: &config::Multiplexer,
) -> (egui::Pos2, egui::Vec2) {
    // The fractions are of the workspace, so on a large display they hand a scratch
    // terminal far more room than it needs; the maximums keep it in hand.
    let size = (workspace.size() * vec2(config.floating_width, config.floating_height))
        .min(vec2(config.floating_max_width, config.floating_max_height))
        .max(vec2(160.0, 100.0))
        .min(workspace.size());
    (workspace.center() - size / 2.0 + float_offset(index), size)
}

fn float_offset(index: usize) -> egui::Vec2 {
    let step = (index % 6) as f32 * 28.0;
    let stack = (index / 6) as f32 * 12.0;
    vec2(step + stack, step)
}

#[derive(Clone, Copy, Default)]
struct FloatPlacement {
    configured_size: Option<egui::Vec2>,
    manual: bool,
}

/// Opt out of automatic placement independently of the input that moved the pane.
/// Future movement commands should call this before changing its geometry.
fn mark_float_manual(ctx: &egui::Context, window_id: egui::Id) {
    ctx.data_mut(|data| {
        data.get_temp_mut_or_default::<FloatPlacement>(window_id.with("placement"))
            .manual = true;
    });
}

fn remember_float_interaction(ctx: &egui::Context, window_id: egui::Id, response: &egui::Response) {
    let placement = ctx.data_mut(|data| {
        data.get_temp::<FloatPlacement>(window_id.with("placement"))
            .unwrap_or_default()
    });
    // Edge resizing has its own response, so the area's dragged flag only covers
    // movement. Compare sizes during a drag to also preserve a manual resize.
    let resized = placement.configured_size.is_some_and(|size| {
        (response.rect.size() - size).abs().max_elem() > 1.0
            && ctx.input(|i| i.pointer.any_down() && i.pointer.is_decidedly_dragging())
    });
    // egui can report a drag as soon as the pointer is pressed, before it moves.
    let moved = response.dragged()
        && response
            .total_drag_delta()
            .is_some_and(|delta| delta != egui::Vec2::ZERO);
    if moved || resized {
        mark_float_manual(ctx, window_id);
    }
}

/// Follow configured geometry until the pane is explicitly moved or resized.
fn position_float<'a>(
    ctx: &egui::Context,
    window: egui::Window<'a>,
    window_id: egui::Id,
    index: usize,
    workspace: Rect,
    config: &config::Multiplexer,
) -> egui::Window<'a> {
    let (pos, size) = float_placement(workspace, index, config);
    let mut window = window
        .default_pos(pos)
        .default_size(size)
        .min_size(vec2(160.0, 100.0).min(workspace.size()));
    let (manual, size_changed) = ctx.data_mut(|data| {
        let placement = data.get_temp_mut_or_default::<FloatPlacement>(window_id.with("placement"));
        let size_changed = placement.configured_size != Some(size);
        placement.configured_size = Some(size);
        (placement.manual, size_changed)
    });
    if !manual {
        window = window.current_pos(pos);
        if size_changed {
            // default_size alone does not update egui's remembered size. Clamp
            // for this frame, then restore normal resize limits on the next one.
            window = window.fixed_size(size).resizable(true);
        }
    }
    window
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

    fn pump_plugins(app: &mut App, ctx: &egui::Context, done: impl Fn(&App) -> bool) {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(8);
        loop {
            for pane in app.panes.values_mut() {
                pane.drain(ctx);
            }
            app.poll_plugins(ctx);
            assert!(app.error.is_none(), "plugin error: {:?}", app.error);
            if done(app) {
                break;
            }
            assert!(std::time::Instant::now() < deadline, "plugin timed out");
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    }

    #[test]
    fn embedded_plugin_controls_panes_and_native_ui() {
        let ctx = egui::Context::default();
        let mut app = test_app(&ctx);
        let path = std::env::temp_dir().join(format!("slerm-plugin-{}.js", std::process::id()));
        std::fs::write(&path, r#"
            __slerm.activate({
                async activate(api) {
                    let rejected = false;
                    try { await api.focusPane({pane:999}); } catch (_) { rejected = true; }
                    if (!rejected) throw Error('missing validation');
                    await api.execute({action:'new_floating'});
                    const state = await api.getState();
                    if (state.tabs[0].panes.length !== 2) throw Error('wrong state');
                    await api.writeTerminal({pane:state.tabs[0].activePane, text:'echo SDK_INPUT\r'});
                    if (typeof await api.readTerminal({pane:1}) !== 'string') throw Error('wrong terminal result');
                    await api.setPanel({panel:{id:'test', title:'Tools', elements:[
                        {type:'button', id:'done', label:'Done'},
                        {type:'input', id:'name', label:'Name', value:'Slerm'}
                    ]}});
                    await api.registerCommand({command:{id:'ready',title:'Ready'}});
                },
                async onEvent(event, api) {
                    if (event.event === 'command') await api.execute({action:'new_tab'});
                    if (event.event === 'ui') {
                        await api.removePanel({panel:event.data.panel});
                        await api.registerCommand({command:{id:'passed',title:'Passed'}});
                    }
                }
            }, 1);
        "#).unwrap();
        let (host, errors) = Host::start(
            &[config::Plugin {
                id: "test".into(),
                path: path.clone(),
            }],
            std::path::Path::new("."),
            &ctx,
        );
        std::fs::remove_file(path).unwrap();
        assert!(errors.is_empty(), "{errors:?}");
        app.plugins = host;
        pump_plugins(&mut app, &ctx, |app| {
            app.plugins.runtimes[0].commands.contains_key("ready")
        });
        assert_eq!(app.tabs[0].floats.len(), 1);
        assert_eq!(app.plugins.runtimes[0].panels.len(), 1);
        let _ = ctx.run(egui::RawInput::default(), |ctx| {
            assert!(app.plugins.show_panels(ctx).is_empty());
        });
        app.plugins.invoke(0, "ready").unwrap();
        pump_plugins(&mut app, &ctx, |app| app.tabs.len() == 2);
        app.plugins.runtimes[0]
            .send(Event::Ui {
                data: crate::plugin_api::UIEvent::Click {
                    panel: "test".into(),
                    control: "done".into(),
                },
            })
            .unwrap();
        pump_plugins(&mut app, &ctx, |app| {
            app.plugins.runtimes[0].commands.contains_key("passed")
        });
        assert!(app.plugins.runtimes[0].panels.is_empty());
    }

    #[test]
    #[ignore = "build the example first: npm run example --prefix sdk"]
    fn bundled_typescript_example_runs_in_boa() {
        let ctx = egui::Context::default();
        let mut app = test_app(&ctx);
        let (host, errors) = Host::start(
            &[config::Plugin {
                id: "workspace".into(),
                path: "sdk/dist/workspace.js".into(),
            }],
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")),
            &ctx,
        );
        assert!(errors.is_empty(), "{errors:?}");
        app.plugins = host;
        pump_plugins(&mut app, &ctx, |app| {
            app.plugins.runtimes[0].panels.contains_key("workspace")
        });
        app.plugins.runtimes[0]
            .send(Event::Ui {
                data: crate::plugin_api::UIEvent::Click {
                    panel: "workspace".into(),
                    control: "scratch".into(),
                },
            })
            .unwrap();
        pump_plugins(&mut app, &ctx, |app| app.tabs[0].floats.len() == 1);
    }

    fn test_app(ctx: &egui::Context) -> App {
        let config = Config {
            shell: vec!["/bin/sh".into()],
            ..Default::default()
        };
        let theme = config.theme().unwrap();
        let pane = Pane::new(&config, &theme, ctx, None).unwrap();
        App {
            config,
            theme,
            config_path: PathBuf::new(),
            panes: BTreeMap::from([(1, pane)]),
            tabs: vec![Tab::new(1, 1)],
            current: 0,
            next_id: 2,
            error: None,
            suppressed: HashSet::new(),
            plugins: Host::default(),
        }
    }

    #[test]
    fn clipboard_preserves_unicode_graphemes_and_soft_wraps() {
        use libghostty_vt::{
            selection::Selection,
            terminal::{Point, PointCoordinate},
        };
        let ctx = egui::Context::default();
        let mut app = test_app(&ctx);
        for (cols, text, last_row) in [
            (
                80,
                "✓ ✔ ☑ \u{f00c} \u{f42e} \u{f012c} café e\u{301} 界 👩\u{200d}💻",
                0,
            ),
            (10, "one ✓ two 界 three", 1),
            (80, "first ✓\r\nsecond 界", 1),
        ] {
            let pane = app.panes.get_mut(&1).unwrap();
            assert!(pane.copy().unwrap().is_none());
            pane.terminal.resize(cols, 4, 8, 16).unwrap();
            pane.terminal.vt_write(b"\x1b[2J\x1b[H");
            // Split inside UTF-8 characters just as PTY reads can do.
            for bytes in text.as_bytes().chunks(2) {
                pane.terminal.vt_write(bytes);
            }
            let a = pane
                .terminal
                .grid_ref(Point::Viewport(PointCoordinate { x: 0, y: 0 }))
                .unwrap();
            let b = pane
                .terminal
                .grid_ref(Point::Viewport(PointCoordinate {
                    x: cols - 1,
                    y: last_row,
                }))
                .unwrap();
            pane.terminal
                .set_selection(Some(&Selection::new(a, b, false)))
                .unwrap();
            let output = ctx.run(egui::RawInput::default(), |ctx| {
                app.action(Action::Copy, ctx).unwrap()
            });
            let copies: Vec<_> = output
                .platform_output
                .commands
                .into_iter()
                .filter_map(|command| {
                    if let egui::OutputCommand::CopyText(text) = command {
                        Some(text)
                    } else {
                        None
                    }
                })
                .collect();
            assert_eq!(copies, vec![text.replace("\r\n", "\n")]);
            app.panes
                .get_mut(&1)
                .unwrap()
                .terminal
                .set_selection(None)
                .unwrap();
        }
    }

    #[test]
    fn selection_starts_at_mouse_down_before_drag_threshold() {
        let ctx = egui::Context::default();
        let mut app = test_app(&ctx);
        app.panes.get_mut(&1).unwrap().terminal.vt_write(b"abcdef");
        let rect = Rect::from_min_size(egui::pos2(0.0, 0.0), vec2(400.0, 200.0));
        let mut cell = vec2(8.0, 16.0);
        let mut run = |events| {
            let _ = ctx.run(
                egui::RawInput {
                    screen_rect: Some(rect),
                    events,
                    ..Default::default()
                },
                |ctx| {
                    egui::CentralPanel::default().show(ctx, |ui| {
                        cell = crate::render::cell_size(
                            ui.painter(),
                            &egui::FontId::monospace(app.config.font_size),
                        );
                        app.panes
                            .get_mut(&1)
                            .unwrap()
                            .show(ui, rect, &app.config, &app.theme, true)
                            .unwrap();
                    });
                },
            );
            cell
        };
        let cell = run(vec![]);
        let start = rect.min + vec2(6.0 + cell.x * 0.5, 6.0 + cell.y * 0.5);
        let end = start + vec2(cell.x * 3.0, 0.0);
        run(vec![
            egui::Event::PointerMoved(start),
            egui::Event::PointerButton {
                pos: start,
                button: egui::PointerButton::Primary,
                pressed: true,
                modifiers: egui::Modifiers::NONE,
            },
        ]);
        run(vec![egui::Event::PointerMoved(end)]);
        run(vec![egui::Event::PointerButton {
            pos: end,
            button: egui::PointerButton::Primary,
            pressed: false,
            modifiers: egui::Modifiers::NONE,
        }]);
        assert_eq!(app.panes[&1].copy().unwrap().as_deref(), Some("abcd"));
    }

    #[test]
    fn file_drops_target_the_visible_pane_once() {
        let ctx = egui::Context::default();
        let mut app = test_app(&ctx);
        let second = Pane::new(&app.config, &app.theme, &ctx, None).unwrap();
        app.panes.insert(2, second);
        let screen = Rect::from_min_size(egui::Pos2::ZERO, vec2(600.0, 400.0));
        let mut float = Rect::NOTHING;
        let mut run = |events, dropped_files| {
            let mut focused = Vec::new();
            let _ = ctx.run(
                egui::RawInput {
                    screen_rect: Some(screen),
                    events,
                    dropped_files,
                    ..Default::default()
                },
                |ctx| {
                    egui::CentralPanel::default().show(ctx, |ui| {
                        if show_pane(
                            ui,
                            1,
                            app.panes.get_mut(&1).unwrap(),
                            screen,
                            &app.config,
                            &app.theme,
                            true,
                        ) {
                            focused.push(1);
                        }
                    });
                    egui::Area::new(egui::Id::new("drop test float"))
                        .fixed_pos(egui::pos2(100.0, 80.0))
                        .order(egui::Order::Foreground)
                        .show(ctx, |ui| {
                            float = ui.allocate_space(vec2(250.0, 200.0)).1;
                            if show_pane(
                                ui,
                                2,
                                app.panes.get_mut(&2).unwrap(),
                                float,
                                &app.config,
                                &app.theme,
                                false,
                            ) {
                                focused.push(2);
                            }
                        });
                },
            );
            (focused, float)
        };
        for _ in 0..3 {
            run(vec![], vec![]);
        }
        let (_, float) = run(vec![], vec![]);
        for (pointer, expected) in [
            (egui::Event::PointerMoved(float.center()), 2),
            (egui::Event::PointerMoved(egui::pos2(20.0, 50.0)), 1),
            (egui::Event::PointerGone, 1),
        ] {
            let (focused, _) = run(
                vec![pointer],
                vec![egui::DroppedFile {
                    path: Some("/tmp/a ✓ file".into()),
                    ..Default::default()
                }],
            );
            assert_eq!(focused, vec![expected]);
            assert!(run(vec![], vec![]).0.is_empty(), "drop must not repeat");
        }
        assert!(app.panes.values().all(|pane| pane.error.is_none()));
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
                            &app.theme,
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
    fn removing_floats_repositions_survivors_in_cycle_order() {
        let ctx = egui::Context::default();
        let config = config::Multiplexer::default();
        let workspace = Rect::from_min_size(egui::Pos2::ZERO, vec2(1600.0, 1200.0));
        let mut tab = Tab::new(1, 1);
        tab.floats = vec![2, 3, 4, 5];
        tab.floats_visible = true;
        let render = |tab: &Tab, moved: Option<PaneId>, workspace: Rect| {
            let mut positions = BTreeMap::new();
            // Allow egui's initial sizing pass to settle.
            for _ in 0..3 {
                let _ = ctx.run(
                    egui::RawInput {
                        screen_rect: Some(workspace),
                        ..Default::default()
                    },
                    |ctx| {
                        for (index, &id) in tab.floats.iter().enumerate() {
                            let window_id = egui::Id::new(("float", tab.id, id));
                            let mut window = egui::Window::new(id.to_string())
                                .id(window_id)
                                .title_bar(false)
                                .frame(egui::Frame::NONE);
                            if moved == Some(id) {
                                mark_float_manual(ctx, window_id);
                                window = window.current_pos(egui::pos2(100.0, 100.0));
                            }
                            let response =
                                position_float(ctx, window, window_id, index, workspace, &config)
                                    .show(ctx, |ui| {
                                        ui.allocate_rect(
                                            ui.available_rect_before_wrap(),
                                            egui::Sense::hover(),
                                        );
                                    })
                                    .unwrap();
                            remember_float_interaction(ctx, window_id, &response.response);
                            positions.insert(id, response.response.rect.min);
                        }
                    },
                );
            }
            positions
        };
        render(&tab, Some(2), workspace);
        render(&tab, Some(5), workspace);
        assert_eq!(render(&tab, None, workspace)[&2], egui::pos2(100.0, 100.0));
        // Workspace changes preserve manual panes while recentering the rest.
        let resized = Rect::from_min_size(egui::Pos2::ZERO, vec2(1800.0, 1400.0));
        let positions = render(&tab, None, resized);
        for (index, &id) in tab.floats.iter().enumerate() {
            let expected = if id == 2 || id == 5 {
                egui::pos2(100.0, 100.0)
            } else {
                float_placement(resized, index, &config).0
            };
            assert_eq!(positions[&id], expected);
        }
        render(&tab, None, workspace);
        render(&tab, Some(2), workspace);
        // Remove the middle pane, then the first, then append a new float.
        for (removed, added) in [(3, None), (2, None), (4, Some(6))] {
            tab.detach(removed);
            if let Some(id) = added {
                tab.floats.push(id);
            }
            let positions = render(&tab, None, workspace);
            tab.active = tab.floats[0];
            for index in 0..tab.floats.len() {
                let expected = if tab.active == 2 || tab.active == 5 {
                    egui::pos2(100.0, 100.0)
                } else {
                    float_placement(workspace, index, &config).0
                };
                assert_eq!(positions[&tab.active], expected);
                tab.cycle(false);
            }
            assert_eq!(tab.active, tab.floats[0]);
        }
    }

    fn render_float(
        ctx: &egui::Context,
        workspace: Rect,
        config: &config::Multiplexer,
        events: Vec<egui::Event>,
    ) -> Rect {
        let window_id = egui::Id::new("test float");
        let mut rect = Rect::NOTHING;
        let _ = ctx.run(
            egui::RawInput {
                screen_rect: Some(workspace),
                events,
                ..Default::default()
            },
            |ctx| {
                let window = egui::Window::new("float")
                    .id(window_id)
                    .title_bar(false)
                    .constrain_to(workspace)
                    .min_size(vec2(160.0, 100.0))
                    .frame(egui::Frame::NONE);
                let response = position_float(ctx, window, window_id, 0, workspace, config)
                    .show(ctx, |ui| {
                        let rect = ui.available_rect_before_wrap();
                        ui.allocate_rect(rect, egui::Sense::hover());
                        let header = Rect::from_min_size(rect.min, vec2(rect.width(), 24.0));
                        ui.interact(header, ui.id().with("header"), egui::Sense::click());
                        let content = Rect::from_min_max(header.left_bottom(), rect.max);
                        ui.interact(
                            content,
                            ui.id().with("content"),
                            egui::Sense::click_and_drag(),
                        );
                    })
                    .unwrap();
                remember_float_interaction(ctx, window_id, &response.response);
                rect = response.response.rect;
            },
        );
        rect
    }

    #[test]
    fn automatic_floats_follow_workspace_and_config_changes() {
        let ctx = egui::Context::default();
        let mut config = config::Multiplexer::default();
        for (width, height, fraction, maximum) in [
            (1000.0, 800.0, 0.65, 960.0),
            (800.0, 600.0, 0.65, 960.0),
            (1800.0, 1400.0, 0.65, 960.0),
            (1800.0, 1400.0, 0.4, 960.0),
            (1800.0, 1400.0, 0.4, 500.0),
            (200.0, 150.0, 0.4, 500.0),
            (100.0, 80.0, 0.4, 500.0),
            (1000.0, 800.0, 0.65, 960.0),
        ] {
            config.floating_width = fraction;
            config.floating_height = fraction;
            config.floating_max_width = maximum;
            config.floating_max_height = maximum;
            let workspace = Rect::from_min_size(egui::pos2(0.0, 30.0), vec2(width, height));
            let (pos, size) = float_placement(workspace, 0, &config);
            for _ in 0..3 {
                render_float(&ctx, workspace, &config, vec![]);
            }
            let rect = render_float(&ctx, workspace, &config, vec![]);
            assert!((rect.min - pos).abs().max_elem() < 0.1, "{rect:?}");
            assert!((rect.size() - size).abs().max_elem() < 0.1, "{rect:?}");
        }
    }

    #[test]
    fn manual_float_geometry_survives_workspace_config_and_visibility_changes() {
        for gesture in ["move", "resize", "select", "click"] {
            let ctx = egui::Context::default();
            let window_id = egui::Id::new("test float");
            let mut config = config::Multiplexer::default();
            let workspace = Rect::from_min_size(egui::Pos2::ZERO, vec2(1000.0, 800.0));
            for _ in 0..3 {
                render_float(&ctx, workspace, &config, vec![]);
            }
            let before = render_float(&ctx, workspace, &config, vec![]);
            let start = match gesture {
                "resize" => before.right_bottom() + vec2(2.0, 2.0),
                "select" => before.center(),
                _ => before.center_top() + vec2(0.0, 12.0),
            };
            let end = if gesture == "click" {
                start
            } else {
                start + vec2(50.0, 40.0)
            };
            let button = |pos, pressed| egui::Event::PointerButton {
                pos,
                button: egui::PointerButton::Primary,
                pressed,
                modifiers: egui::Modifiers::default(),
            };
            render_float(
                &ctx,
                workspace,
                &config,
                vec![egui::Event::PointerMoved(start)],
            );
            render_float(&ctx, workspace, &config, vec![button(start, true)]);
            if gesture != "click" {
                render_float(
                    &ctx,
                    workspace,
                    &config,
                    vec![egui::Event::PointerMoved(end)],
                );
            }
            render_float(&ctx, workspace, &config, vec![button(end, false)]);
            let after = render_float(&ctx, workspace, &config, vec![]);
            let manual = matches!(gesture, "move" | "resize");
            assert_eq!(
                ctx.data_mut(|data| data
                    .get_temp::<FloatPlacement>(window_id.with("placement"))
                    .unwrap()
                    .manual),
                manual,
                "{gesture}"
            );
            match gesture {
                "move" => {
                    assert_eq!(after.min, before.min + vec2(50.0, 40.0));
                    assert_eq!(after.size(), before.size());
                }
                "resize" => assert_ne!(after.size(), before.size()),
                _ => assert_eq!(after, before),
            }
            // Hidden and zoomed floats both skip rendering their window.
            let _ = ctx.run(egui::RawInput::default(), |_| {});
            config.floating_width = 0.4;
            config.floating_height = 0.4;
            let workspace = Rect::from_min_size(egui::Pos2::ZERO, vec2(1200.0, 1000.0));
            for _ in 0..3 {
                render_float(&ctx, workspace, &config, vec![]);
            }
            let restored = render_float(&ctx, workspace, &config, vec![]);
            let (pos, size) = float_placement(workspace, 0, &config);
            assert_eq!(
                restored,
                if manual {
                    after
                } else {
                    Rect::from_min_size(pos, size)
                },
                "{gesture}"
            );
        }
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
        // Each group of six restarts the cascade slightly to the right.
        assert_eq!(
            float_placement(workspace, 6, &config).0,
            first + vec2(12.0, 0.0)
        );
        assert_eq!(
            float_placement(workspace, 7, &config).0,
            second + vec2(12.0, 0.0)
        );
        assert_eq!(
            float_placement(workspace, 12, &config).0,
            first + vec2(24.0, 0.0)
        );

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
    fn closing_tile_selects_nearest_visible_pane_and_preserves_other_focus() {
        let ctx = egui::Context::default();
        let mut app = test_app(&ctx);
        for _ in 0..3 {
            app.action(Action::SplitDown, &ctx).unwrap();
        }
        app.action(Action::NewFloating, &ctx).unwrap();
        app.action(Action::ToggleFloats, &ctx).unwrap();
        app.tabs[0].active = 4;
        // Pane 2 is closest, despite being neither the first nor the previous
        // pane in traversal order. The hidden float overlaps the closing pane.
        for (id, y) in [(1, 0.0), (2, 200.0), (3, 100.0), (4, 300.0), (5, 300.0)] {
            app.panes.get_mut(&id).unwrap().last_rect =
                Rect::from_min_size(egui::pos2(0.0, y), vec2(100.0, 100.0));
        }
        app.action(Action::ClosePane, &ctx).unwrap();
        assert_eq!(app.tabs[0].active, 2);
        assert!(!app.tabs[0].floats_visible);
        app.close(1, &ctx);
        assert_eq!(app.tabs[0].active, 2);
        app.action(Action::ClosePane, &ctx).unwrap();
        assert_eq!(app.tabs[0].active, 3);
        app.action(Action::ClosePane, &ctx).unwrap();
        assert_eq!(app.tabs[0].active, 5);
        assert!(app.tabs[0].floats_visible);
    }

    #[test]
    fn closing_unrendered_tile_selects_adjacent_pane() {
        let ctx = egui::Context::default();
        let mut app = test_app(&ctx);
        for _ in 0..3 {
            app.action(Action::SplitDown, &ctx).unwrap();
        }
        app.action(Action::ClosePane, &ctx).unwrap();
        assert_eq!(app.tabs[0].active, 3);
        app.action(Action::ClosePane, &ctx).unwrap();
        assert_eq!(app.tabs[0].active, 2);
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
