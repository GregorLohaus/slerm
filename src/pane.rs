use crate::{
    config::Config,
    input,
    pty::Pty,
    render::{Grid, Renderer, cell_size},
    theme::{self, Theme},
};
use anyhow::{Context, Result, ensure};
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
    scrollback_scroll: ScrollAccumulator,
    mouse_scroll: ScrollAccumulator,
    selection_start: Option<PointCoordinate>,
    focused: bool,
    saved_session: Option<(Box<Pane>, ScrollbackFile)>,
}

struct ScrollbackFile(PathBuf);

#[derive(Default)]
struct ScrollAccumulator(f32);

impl ScrollAccumulator {
    fn rows(&mut self, delta: f32) -> isize {
        self.0 += delta;
        let rows = self.0.trunc();
        self.0 -= rows;
        rows as isize
    }

    fn wheel(&mut self, unit: egui::MouseWheelUnit, delta: f32, cell: f32, page: u16) -> isize {
        self.rows(match unit {
            egui::MouseWheelUnit::Point => delta / cell,
            egui::MouseWheelUnit::Line => delta * 3.0,
            egui::MouseWheelUnit::Page => delta * f32::from(page),
        })
    }
}

impl ScrollbackFile {
    fn new(bytes: &[u8]) -> Result<Self> {
        use std::{
            fs::OpenOptions,
            io::Write,
            os::unix::fs::OpenOptionsExt,
            sync::atomic::{AtomicU64, Ordering},
        };
        static NEXT: AtomicU64 = AtomicU64::new(0);
        loop {
            let path = std::env::temp_dir().join(format!(
                "slerm-scrollback-{}-{}.txt",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            match OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(&path)
            {
                Ok(mut file) => {
                    let temporary = Self(path);
                    file.write_all(bytes)?;
                    return Ok(temporary);
                }
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(error) => return Err(error.into()),
            }
        }
    }
}

#[cfg(test)]
mod scroll_tests {
    use super::*;

    #[test]
    fn precise_scrolling_preserves_distance_across_frames() {
        let mut scroll = ScrollAccumulator::default();
        // Sub-pixel events from a slow gesture must eventually move a row.
        let rows: isize = (0..128).map(|_| scroll.rows(0.25 / 16.0)).sum();
        assert_eq!(rows, 2);
        assert_eq!(scroll.0, 0.0);

        let mut batched = ScrollAccumulator::default();
        assert_eq!(batched.rows(32.0 / 16.0), rows);
        assert_eq!(scroll.rows(1.75), 1);
        assert_eq!(scroll.rows(0.25), 1);
        assert_eq!(scroll.rows(-0.75), 0);
        assert_eq!(scroll.rows(-0.25), -1);
        assert_eq!(scroll.rows(0.75), 0);
        assert_eq!(scroll.rows(-0.75), 0);
    }

    #[test]
    fn mouse_tracking_respects_precise_distance_and_wheel_units() {
        use egui::MouseWheelUnit::{Line, Page, Point};
        let mut scroll = ScrollAccumulator::default();
        assert_eq!(scroll.wheel(Point, 0.0, 16.0, 24), 0);
        assert_eq!(scroll.wheel(Point, 4.0, 16.0, 24), 0);
        assert_eq!(scroll.wheel(Point, 12.0, 16.0, 24), 1);
        assert_eq!(scroll.wheel(Point, -32.0, 16.0, 24), -2);
        assert_eq!(scroll.wheel(Line, 1.0, 16.0, 24), 3);
        assert_eq!(scroll.wheel(Page, -1.0, 16.0, 24), -24);
    }
}

#[cfg(test)]
mod scrollback_tests {
    use super::*;
    use std::time::{Duration, Instant};

    #[test]
    fn editor_receives_history_and_restores_original_session_on_exit() {
        let ctx = egui::Context::default();
        let config = Config {
            shell: vec![
                "/bin/sh".into(),
                "-c".into(),
                "read answer; printf ORIGINAL_ALIVE; read answer".into(),
            ],
            scrollback_editor: Config::parse(
                "scrollback_editor = \"/bin/sh -c 'printf EDITOR_READY; read answer' editor\"",
            )
            .unwrap()
            .scrollback_editor,
            ..Default::default()
        };
        let mut pane = Pane::new(&config, &theme::DEFAULT, &ctx, None).unwrap();
        pane.terminal.resize(12, 2, 8, 16).unwrap();
        pane.terminal
            .vt_write(b"history\r\n\x1b[31mred\x1b[0m\r\nlong line that wraps\r\nlast");
        pane.open_scrollback(&config, &theme::DEFAULT, &ctx)
            .unwrap();
        let (original, file) = pane.saved_session.as_ref().unwrap();
        assert!(!original.dead);
        let path = file.0.clone();
        let contents = std::fs::read_to_string(&path).unwrap();
        assert!(
            contents.contains("history\nred\nlong line that wraps\nlast"),
            "{contents:?}"
        );
        assert!(!contents.contains('\x1b'));
        pane.send(b"done\n".to_vec()).unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        while pane.saved_session.is_some() && Instant::now() < deadline {
            pane.drain(&ctx);
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(pane.saved_session.is_none());
        assert!(!pane.dead);
        assert!(!path.exists());
        pane.send(b"continue\n".to_vec()).unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            pane.drain(&ctx);
            let text = libghostty_vt::fmt::Formatter::new(
                &pane.terminal,
                libghostty_vt::fmt::FormatterOptions::new().with_unwrap(true),
            )
            .unwrap()
            .format_alloc(None)
            .unwrap();
            if String::from_utf8_lossy(&text).contains("ORIGINAL_ALIVE") {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "original process did not resume input"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    #[test]
    fn invalid_editor_leaves_the_original_pane_intact() {
        let ctx = egui::Context::default();
        let config = Config {
            shell: vec!["/bin/sh".into()],
            ..Default::default()
        };
        let mut pane = Pane::new(&config, &theme::DEFAULT, &ctx, None).unwrap();
        for editor in ["", "'", "/no/such/slerm-editor"] {
            assert!(
                pane.open_scrollback_with_editor(&config, &theme::DEFAULT, &ctx, editor)
                    .is_err()
            );
            assert!(pane.saved_session.is_none());
            assert!(!pane.dead);
        }
    }
}

impl Drop for ScrollbackFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
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
            scrollback_scroll: ScrollAccumulator::default(),
            mouse_scroll: ScrollAccumulator::default(),
            selection_start: None,
            focused: false,
            saved_session: None,
        })
    }

    pub fn drain(&mut self, ctx: &egui::Context) {
        if let Some((session, _)) = &mut self.saved_session {
            session.drain(ctx);
        }
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
        if self.dead
            && let Some((session, _file)) = self.saved_session.take()
        {
            *self = *session;
            ctx.request_repaint();
        }
    }

    pub fn open_scrollback(
        &mut self,
        config: &Config,
        theme: &Theme,
        ctx: &egui::Context,
    ) -> Result<()> {
        if self.saved_session.is_some() {
            return Ok(());
        }
        let editor = config
            .scrollback_editor
            .clone()
            .unwrap_or_else(|| std::env::var("EDITOR").unwrap_or_else(|_| "vi".into()));
        self.open_scrollback_with_editor(config, theme, ctx, &editor)
    }

    fn open_scrollback_with_editor(
        &mut self,
        config: &Config,
        theme: &Theme,
        ctx: &egui::Context,
        editor: &str,
    ) -> Result<()> {
        use libghostty_vt::fmt::{Formatter, FormatterOptions};
        let mut command =
            shell_words::split(editor).context("Invalid scrollback editor command")?;
        ensure!(
            !command.is_empty() && !command[0].is_empty(),
            "Scrollback editor command is empty"
        );
        self.drain(ctx);
        let bytes = Formatter::new(
            &self.terminal,
            FormatterOptions::new().with_trim(true).with_unwrap(true),
        )?
        .format_alloc(None)?;
        let file = ScrollbackFile::new(&bytes).context("Cannot save scrollback")?;
        command.push(
            file.0
                .to_str()
                .context("Scrollback path is not valid UTF-8")?
                .into(),
        );
        let mut editor_config = config.clone();
        editor_config.shell = command;
        let mut editor_pane = Self::new(&editor_config, theme, ctx, self.cwd().as_deref())
            .context("Cannot open scrollback editor")?;
        editor_pane.last_rect = self.last_rect;
        let session = std::mem::replace(self, editor_pane);
        self.saved_session = Some((Box::new(session), file));
        ctx.request_repaint();
        Ok(())
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
        self.scrollback_scroll = ScrollAccumulator::default();
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
        self.terminal
            .format_selection_alloc(None, FormatOptions::new().with_trim(true).with_unwrap(true))?
            .map(|b| String::from_utf8(b.to_vec()).context("Selection is not valid UTF-8"))
            .transpose()
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
        if !response.hovered() || tracking {
            self.scrollback_scroll = ScrollAccumulator::default();
        }
        if !response.hovered() || !tracking {
            self.mouse_scroll = ScrollAccumulator::default();
        }
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
                        input::append_encoded(&mut data, |buffer| {
                            self.mouse_encoder.encode(&self.mouse_event, buffer)
                        })?;
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
                        input::append_encoded(&mut data, |buffer| {
                            self.mouse_encoder.encode(&self.mouse_event, buffer)
                        })?;
                    }
                    egui::Event::MouseWheel { unit, delta, .. } => {
                        let rows = self.mouse_scroll.wheel(unit, delta.y, cell.y, self.size.1);
                        self.mouse_event
                            .set_action(mouse::Action::Press)
                            .set_button(Some(if rows > 0 {
                                mouse::Button::Four
                            } else {
                                mouse::Button::Five
                            }));
                        for _ in 0..rows.unsigned_abs() {
                            input::append_encoded(&mut data, |buffer| {
                                self.mouse_encoder.encode(&self.mouse_event, buffer)
                            })?;
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
                let origin = ui.input(|i| i.pointer.press_origin()).unwrap_or(pos);
                let origin = origin - grid.min;
                self.selection_start = Some(PointCoordinate {
                    x: (origin.x / cell.x)
                        .floor()
                        .clamp(0.0, (self.size.0 - 1) as f32) as u16,
                    y: (origin.y / cell.y)
                        .floor()
                        .clamp(0.0, (self.size.1 - 1) as f32) as u32,
                });
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
                let rows = self.scrollback_scroll.rows(scroll / cell.y);
                if rows != 0 {
                    self.terminal.scroll_viewport(ScrollViewport::Delta(-rows));
                }
            }
        }
        Ok(())
    }
}
