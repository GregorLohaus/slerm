use crate::{
    config::Config,
    input,
    pty::Pty,
    render::{Grid, Renderer, cell_size},
    theme::{self, Theme},
};
use anyhow::{Result, ensure};
use egui::{FontId, Rect, Sense, vec2};
use libghostty_vt::{
    Terminal, TerminalOptions, mouse,
    selection::{FormatOptions, Selection},
    terminal::{self, Mode, Point, PointCoordinate, ScrollViewport},
};
use std::{
    cell::RefCell,
    path::{Path, PathBuf},
    rc::Rc,
    sync::mpsc::TryRecvError,
};

pub struct Pane {
    pub terminal: Terminal<'static, 'static>,
    pub renderer: Renderer,
    pub pty: Pty,
    pub dead: bool,
    pub error: Option<String>,
    pub last_rect: Rect,
    size: (u16, u16, u32, u32),
    report_size: Rc<RefCell<terminal::SizeReportSize>>,
    callback_error: Rc<RefCell<Option<String>>>,
    mouse_encoder: mouse::Encoder<'static>,
    mouse_event: mouse::Event<'static>,
    selection_start: Option<PointCoordinate>,
    focused: bool,
}

/// OSC 7 carries a `file://host/path` URL. Only a directory on this machine can be
/// handed to a new shell, so a remote host (an SSH session, say) is rejected here
/// and the caller falls back to the local process.
fn local_pwd(pwd: &str) -> Option<PathBuf> {
    if pwd.is_empty() {
        return None;
    }
    // Some shells emit a bare path instead of a URL.
    let Some(rest) = pwd.strip_prefix("file://") else {
        return pwd.starts_with('/').then(|| PathBuf::from(pwd));
    };
    let (host, path) = rest.split_once('/')?;
    // A shell may report either the short name or the FQDN, so compare the leading
    // label of each.
    let short = |name: &str| name.split('.').next().unwrap_or(name).to_ascii_lowercase();
    let local = host.is_empty()
        || host.eq_ignore_ascii_case("localhost")
        || hostname().is_some_and(|name| short(&name) == short(host));
    local.then(|| PathBuf::from(percent_decode(&format!("/{path}"))))
}

fn hostname() -> Option<String> {
    unsafe extern "C" {
        fn gethostname(name: *mut std::ffi::c_char, len: usize) -> std::ffi::c_int;
    }
    let mut buf = [0u8; 256];
    // SAFETY: at most `len` bytes are written, leaving the final byte of `buf` as
    // the terminator even when the name is truncated.
    if unsafe { gethostname(buf.as_mut_ptr().cast(), buf.len() - 1) } != 0 {
        return None;
    }
    let end = buf.iter().position(|&b| b == 0)?;
    String::from_utf8(buf[..end].to_vec()).ok()
}

fn percent_decode(path: &str) -> String {
    let mut out = Vec::with_capacity(path.len());
    let mut bytes = path.bytes();
    while let Some(byte) = bytes.next() {
        let decoded = (byte == b'%')
            .then(|| {
                let hex = [bytes.clone().next()?, bytes.clone().nth(1)?];
                let hex = std::str::from_utf8(&hex).ok()?;
                u8::from_str_radix(hex, 16).ok()
            })
            .flatten();
        match decoded {
            Some(value) => {
                bytes.nth(1);
                out.push(value);
            }
            None => out.push(byte),
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

pub fn terminal(config: &Config, theme: &Theme) -> Result<Terminal<'static, 'static>> {
    let mut term = Terminal::new(TerminalOptions {
        cols: 80,
        rows: 24,
        max_scrollback: config.scrollback,
    })?;
    term.resize(80, 24, 8, 16)?;
    apply_config(&mut term, config, theme)?;
    term.set_default_cursor_style(Some(terminal::CursorStyle::BlockHollow))?;
    term.vt_write(b"\x1b[0 q");
    term.on_xtversion(|_| Some(concat!("slerm ", env!("CARGO_PKG_VERSION"))))?;
    term.on_device_attributes(|_| {
        Some(terminal::DeviceAttributes {
            primary: terminal::PrimaryDeviceAttributes::new(
                terminal::ConformanceLevel::VT220,
                &[terminal::DeviceAttributeFeature::ANSI_COLOR],
            ),
            secondary: terminal::SecondaryDeviceAttributes {
                device_type: terminal::DeviceType::VT220,
                firmware_version: 1,
                rom_cartridge: 0,
            },
            tertiary: Default::default(),
        })
    })?;
    let scheme = if theme.dark {
        terminal::ColorScheme::Dark
    } else {
        terminal::ColorScheme::Light
    };
    term.on_color_scheme(move |_| Some(scheme))?;
    Ok(term)
}

pub fn apply_config(term: &mut Terminal<'_, '_>, config: &Config, theme: &Theme) -> Result<()> {
    let palette = theme.palette(term.default_color_palette()?);
    term.set_default_fg_color(Some(theme::rgb_color(theme.foreground)))?
        .set_default_bg_color(Some(theme::rgb_color(theme.background)))?
        .set_default_cursor_color(Some(theme::rgb_color(theme.cursor)))?
        .set_default_color_palette(Some(palette))?
        .set_kitty_image_storage_limit(config.kitty.image_storage_mb * 1024 * 1024)?
        .set_kitty_image_from_file_allowed(config.kitty.allow_file)?
        .set_kitty_image_from_temp_file_allowed(config.kitty.allow_temp_file)?
        .set_kitty_image_from_shared_mem_allowed(config.kitty.allow_shared_memory)?;
    Ok(())
}

impl Pane {
    pub fn new(
        config: &Config,
        theme: &Theme,
        ctx: &egui::Context,
        cwd: Option<&Path>,
    ) -> Result<Self> {
        let mut terminal = terminal(config, theme)?;
        let renderer = Renderer::new()?;
        let mouse_encoder = mouse::Encoder::new()?;
        let mouse_event = mouse::Event::new()?;
        let pty = Pty::spawn(config, ctx, cwd)?;
        let input = pty.input.clone();
        let callback_error = Rc::new(RefCell::new(None));
        let errors = callback_error.clone();
        terminal.on_pty_write(move |_, data| {
            if let Err(e) = input.try_send(data.to_vec()) {
                *errors.borrow_mut() = Some(format!("PTY response queue: {e}"));
            }
        })?;
        let report_size = Rc::new(RefCell::new(terminal::SizeReportSize {
            rows: 24,
            columns: 80,
            cell_width: 8,
            cell_height: 16,
        }));
        let size = report_size.clone();
        terminal.on_size(move |_| Some(*size.borrow()))?;
        Ok(Self {
            terminal,
            renderer,
            pty,
            dead: false,
            error: None,
            last_rect: Rect::NOTHING,
            size: (80, 24, 8, 16),
            report_size,
            callback_error,
            mouse_encoder,
            mouse_event,
            selection_start: None,
            focused: false,
        })
    }

    pub fn drain(&mut self, ctx: &egui::Context) {
        // Bound work per frame as well as queued bytes, so one busy PTY cannot starve others.
        let mut drained = 0;
        for _ in 0..64 {
            match self.pty.output.try_recv() {
                Ok(data) => {
                    self.terminal.vt_write(&data);
                    drained += 1;
                }
                Err(TryRecvError::Empty | TryRecvError::Disconnected) => break,
            }
        }
        if self.pty.exited.try_recv().is_ok() {
            self.dead = true;
        }
        if let Ok(e) = self.pty.errors.try_recv() {
            self.error = Some(e);
        }
        if let Some(e) = self.callback_error.borrow_mut().take() {
            self.error = Some(e);
        }
        // Workers wake the UI on output/exit/errors. Only poll again if we hit
        // the read budget and may have left queued output behind.
        if drained == 64 {
            ctx.request_repaint();
        }
    }

    pub fn send(&mut self, bytes: Vec<u8>) -> Result<()> {
        if bytes.is_empty() {
            return Ok(());
        }
        ensure!(!self.dead, "shell has exited");
        self.pty
            .input
            .try_send(bytes)
            .map_err(|e| anyhow::anyhow!("PTY input queue: {e}"))?;
        self.terminal.scroll_viewport(ScrollViewport::Bottom);
        Ok(())
    }

    pub fn paste(&mut self, text: &str) -> Result<()> {
        ensure!(text.len() <= 4 * 1024 * 1024, "paste exceeds 4 MiB");
        // ESC can terminate bracketed paste early. Strip it and NUL from clipboard input.
        let text = text
            .replace(['\x1b', '\0'], "")
            .replace("\r\n", "\n")
            .replace('\n', "\r");
        let data = if self.terminal.mode(Mode::BRACKETED_PASTE)? {
            format!("\x1b[200~{text}\x1b[201~").into_bytes()
        } else {
            text.into_bytes()
        };
        self.send(data)
    }

    pub fn copy(&self) -> Result<Option<String>> {
        Ok(self
            .terminal
            .format_selection_alloc(None, FormatOptions::new().with_trim(true).with_unwrap(true))?
            .map(|b| String::from_utf8_lossy(&b).into_owned()))
    }

    /// Where a pane spawned from this one should start: the directory the shell
    /// reports through OSC 7, or the foreground process group's own directory.
    pub fn cwd(&self) -> Option<PathBuf> {
        self.terminal
            .pwd()
            .ok()
            .and_then(local_pwd)
            .or_else(|| self.pty.cwd())
            .filter(|path| path.is_dir())
    }

    pub fn focus(&mut self, focused: bool) -> Result<()> {
        if focused != self.focused {
            self.focused = focused;
            if self
                .terminal
                .mode(Mode::new(1004, terminal::ModeKind::Dec))?
            {
                self.send(if focused { b"\x1b[I" } else { b"\x1b[O" }.to_vec())?;
            }
        }
        Ok(())
    }

    pub fn show(
        &mut self,
        ui: &mut egui::Ui,
        rect: Rect,
        config: &Config,
        theme: &Theme,
        active: bool,
    ) -> Result<bool> {
        let font = FontId::monospace(config.font_size);
        let cell = cell_size(ui.painter(), &font);
        let (cw, ch) = (cell.x, cell.y);
        let grid = rect.shrink(6.0);
        if grid.width() < cw || grid.height() < ch {
            return Ok(false);
        }
        let size = (
            (grid.width() / cw).floor().clamp(1.0, 1000.0) as u16,
            (grid.height() / ch).floor().clamp(1.0, 1000.0) as u16,
            cw as u32,
            ch as u32,
        );
        if size != self.size {
            self.pty.resize(size.0, size.1, size.2, size.3)?;
            self.terminal.resize(size.0, size.1, size.2, size.3)?;
            self.size = size;
            *self.report_size.borrow_mut() = terminal::SizeReportSize {
                columns: size.0,
                rows: size.1,
                cell_width: size.2,
                cell_height: size.3,
            };
        }
        let response = ui.interact(grid, ui.id().with("terminal"), Sense::click_and_drag());
        self.pointer(ui, &response, grid, cell)?;
        self.renderer.paint(
            &self.terminal,
            ui.painter(),
            &Grid {
                rect: grid,
                cell,
                font: &font,
                theme,
                active,
            },
        )?;
        if let Some(error) = &self.error {
            ui.painter().text(
                grid.left_bottom() - vec2(0.0, ch),
                egui::Align2::LEFT_TOP,
                error,
                FontId::proportional(12.0),
                theme::color(theme.ansi[9]),
            );
        }
        Ok(response.clicked()
            || response.drag_started()
            || (response.hovered() && ui.input(|i| i.pointer.any_pressed())))
    }

    fn pointer(
        &mut self,
        ui: &egui::Ui,
        response: &egui::Response,
        grid: Rect,
        cell: egui::Vec2,
    ) -> Result<()> {
        let modifiers = ui.input(|i| i.modifiers);
        let tracking = self.terminal.is_mouse_tracking()? && !modifiers.shift;
        let pos = response
            .interact_pointer_pos()
            .or_else(|| ui.input(|i| i.pointer.hover_pos()));
        let Some(pos) = pos else {
            return Ok(());
        };
        let local = pos - grid.min;
        if tracking && response.hovered() {
            let events = ui.input(|i| i.events.clone());
            self.mouse_encoder
                .set_options_from_terminal(&self.terminal)
                .set_size(mouse::EncoderSize {
                    screen_width: self.size.0 as u32 * self.size.2,
                    screen_height: self.size.1 as u32 * self.size.3,
                    cell_width: self.size.2,
                    cell_height: self.size.3,
                    padding_top: 0,
                    padding_bottom: 0,
                    padding_left: 0,
                    padding_right: 0,
                })
                .set_any_button_pressed(ui.input(|i| i.pointer.any_down()))
                .set_track_last_cell(true);
            self.mouse_event
                .set_position(mouse::Position {
                    x: local.x,
                    y: local.y,
                })
                .set_mods(input::mods(modifiers));
            let mut data = vec![];
            for event in events {
                match event {
                    egui::Event::PointerButton {
                        button, pressed, ..
                    } => {
                        let button = match button {
                            egui::PointerButton::Primary => mouse::Button::Left,
                            egui::PointerButton::Secondary => mouse::Button::Right,
                            egui::PointerButton::Middle => mouse::Button::Middle,
                            _ => continue,
                        };
                        self.mouse_event
                            .set_action(if pressed {
                                mouse::Action::Press
                            } else {
                                mouse::Action::Release
                            })
                            .set_button(Some(button));
                        self.mouse_encoder
                            .encode_to_vec(&self.mouse_event, &mut data)?;
                    }
                    egui::Event::PointerMoved(_) => {
                        let button = ui.input(|i| {
                            if i.pointer.primary_down() {
                                Some(mouse::Button::Left)
                            } else if i.pointer.secondary_down() {
                                Some(mouse::Button::Right)
                            } else {
                                None
                            }
                        });
                        self.mouse_event
                            .set_action(mouse::Action::Motion)
                            .set_button(button);
                        self.mouse_encoder
                            .encode_to_vec(&self.mouse_event, &mut data)?;
                    }
                    egui::Event::MouseWheel { delta, .. } => {
                        self.mouse_event
                            .set_action(mouse::Action::Press)
                            .set_button(Some(if delta.y > 0.0 {
                                mouse::Button::Four
                            } else {
                                mouse::Button::Five
                            }));
                        for _ in 0..3 {
                            self.mouse_encoder
                                .encode_to_vec(&self.mouse_event, &mut data)?;
                        }
                    }
                    _ => {}
                }
            }
            self.send(data)?;
        } else if !tracking {
            let point = PointCoordinate {
                x: (local.x / cell.x)
                    .floor()
                    .clamp(0.0, (self.size.0 - 1) as f32) as u16,
                y: (local.y / cell.y)
                    .floor()
                    .clamp(0.0, (self.size.1 - 1) as f32) as u32,
            };
            if response.drag_started() {
                self.selection_start = Some(point);
            }
            if response.clicked() {
                self.terminal.set_selection(None)?;
                self.selection_start = None;
            }
            if response.dragged()
                && let Some(start) = self.selection_start
            {
                let a = self.terminal.grid_ref(Point::Viewport(start))?;
                let b = self.terminal.grid_ref(Point::Viewport(point))?;
                self.terminal
                    .set_selection(Some(&Selection::new(a, b, modifiers.alt)))?;
            }
            if response.hovered() {
                let scroll = ui.input(|i| i.raw_scroll_delta.y);
                if scroll.abs() >= 1.0 {
                    self.terminal.scroll_viewport(ScrollViewport::Delta(
                        -(scroll / cell.y).round() as isize,
                    ));
                }
            }
        }
        Ok(())
    }
}
