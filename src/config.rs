use anyhow::{Context, Result, bail, ensure};
use serde::Deserialize;
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};

pub const EXAMPLE: &str = include_str!("../config.example.toml");

#[derive(Clone, Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub shell: Vec<String>,
    pub working_directory: Option<PathBuf>,
    pub font_size: f32,
    pub font_path: Option<PathBuf>,
    pub scrollback: usize,
    pub window_width: f32,
    pub window_height: f32,
    pub background: String,
    pub foreground: String,
    pub accent: String,
    pub kitty: Kitty,
    pub multiplexer: Multiplexer,
    pub keys: BTreeMap<String, Action>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Kitty {
    pub image_storage_mb: u64,
    pub allow_file: bool,
    pub allow_temp_file: bool,
    pub allow_shared_memory: bool,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Multiplexer {
    pub max_panes: usize,
    pub floating_width: f32,
    pub floating_height: f32,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Action {
    SplitRight,
    SplitDown,
    NewFloating,
    NewPane,
    ToggleFloating,
    ToggleFloats,
    NextPane,
    PreviousPane,
    FocusLeft,
    FocusDown,
    FocusUp,
    FocusRight,
    ClosePane,
    ZoomPane,
    NewTab,
    NextTab,
    PreviousTab,
    ReloadConfig,
    Copy,
    Paste,
}

impl Default for Kitty {
    fn default() -> Self {
        Self {
            image_storage_mb: 64,
            allow_file: true,
            allow_temp_file: true,
            allow_shared_memory: true,
        }
    }
}
impl Default for Multiplexer {
    fn default() -> Self {
        Self {
            max_panes: 32,
            floating_width: 0.65,
            floating_height: 0.65,
        }
    }
}
impl Default for Config {
    fn default() -> Self {
        Self {
            shell: vec![std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".into())],
            working_directory: None,
            font_size: 16.0,
            font_path: None,
            scrollback: 10000,
            window_width: 1100.0,
            window_height: 720.0,
            background: "#17191c".into(),
            foreground: "#e2e4e8".into(),
            accent: "#58c4ad".into(),
            kitty: Kitty::default(),
            multiplexer: Multiplexer::default(),
            keys: [
                ("Alt+F", Action::ToggleFloats),
                ("Alt+M", Action::NewPane),
                ("Alt+H", Action::FocusLeft),
                ("Alt+J", Action::FocusDown),
                ("Alt+K", Action::FocusUp),
                ("Alt+L", Action::FocusRight),
                ("Alt+A", Action::NewTab),
                ("Alt+Shift+H", Action::PreviousTab),
                ("Alt+Shift+L", Action::NextTab),
                ("Alt+X", Action::ClosePane),
                ("Ctrl+Shift+Z", Action::ZoomPane),
                ("Ctrl+Shift+R", Action::ReloadConfig),
                ("Ctrl+Shift+C", Action::Copy),
                ("Ctrl+Shift+V", Action::Paste),
            ]
            .into_iter()
            .map(|(k, v)| (k.into(), v))
            .collect(),
        }
    }
}

impl Config {
    pub fn path() -> Result<PathBuf> {
        if let Some(dir) = std::env::var_os("XDG_CONFIG_HOME").filter(|p| !p.is_empty()) {
            return Ok(PathBuf::from(dir).join("slerm/config.toml"));
        }
        Ok(
            PathBuf::from(std::env::var_os("HOME").context("HOME is not set")?)
                .join(".config/slerm/config.toml"),
        )
    }

    pub fn load(path: &Path) -> Result<Self> {
        match std::fs::read_to_string(path) {
            Ok(text) => {
                Self::parse(&text).with_context(|| format!("Invalid config: {}", path.display()))
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(e).with_context(|| format!("Cannot read {}", path.display())),
        }
    }

    pub fn parse(text: &str) -> Result<Self> {
        let config: Self = toml::from_str(text)?;
        ensure!(
            !config.shell.is_empty() && !config.shell[0].is_empty(),
            "shell must contain an executable"
        );
        ensure!(
            (8.0..=48.0).contains(&config.font_size),
            "font_size must be 8..48"
        );
        ensure!(
            (320.0..=16384.0).contains(&config.window_width)
                && (200.0..=16384.0).contains(&config.window_height),
            "invalid window dimensions"
        );
        ensure!(
            config.scrollback <= 1_000_000,
            "scrollback must be <= 1000000"
        );
        ensure!(
            (1..=1024).contains(&config.kitty.image_storage_mb),
            "image_storage_mb must be 1..1024"
        );
        ensure!(
            (1..=128).contains(&config.multiplexer.max_panes),
            "max_panes must be 1..128"
        );
        for fraction in [
            config.multiplexer.floating_width,
            config.multiplexer.floating_height,
        ] {
            ensure!(
                (0.2..=0.95).contains(&fraction),
                "floating dimensions must be fractions in 0.2..0.95"
            );
        }
        for color in [&config.background, &config.foreground, &config.accent] {
            rgb(color)?;
        }
        for key in config.keys.keys() {
            shortcut(key)?;
        }
        Ok(config)
    }
}

pub fn rgb(text: &str) -> Result<libghostty_vt::style::RgbColor> {
    ensure!(
        text.len() == 7
            && text.starts_with('#')
            && text[1..].bytes().all(|c| c.is_ascii_hexdigit()),
        "color must be #RRGGBB: {text}"
    );
    let value = u32::from_str_radix(&text[1..], 16)?;
    Ok(libghostty_vt::style::RgbColor {
        r: (value >> 16) as u8,
        g: (value >> 8) as u8,
        b: value as u8,
    })
}

pub fn shortcut(text: &str) -> Result<egui::KeyboardShortcut> {
    let mut mods = egui::Modifiers::NONE;
    let parts: Vec<_> = text.split('+').collect();
    for part in &parts[..parts.len() - 1] {
        match *part {
            "Ctrl" => mods.ctrl = true,
            "Shift" => mods.shift = true,
            "Alt" => mods.alt = true,
            "Super" => {
                mods.mac_cmd = true;
                mods.command = true;
            }
            _ => bail!("unknown modifier in {text}"),
        }
    }
    let key = egui::Key::from_name(parts[parts.len() - 1])
        .with_context(|| format!("unknown key in {text}"))?;
    Ok(egui::KeyboardShortcut::new(mods, key))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn validates_example_and_rejects_bad_values() {
        let example = Config::parse(EXAMPLE).unwrap();
        assert_eq!(example.keys, Config::default().keys);
        for text in [
            "font_size = nan",
            "shell = []",
            "background = '#xx0000'",
            "font_sze = 16",
            "[keys]\n'Hyper+Q' = 'close_pane'",
            "[multiplexer]\nfloating_width = 2",
        ] {
            assert!(Config::parse(text).is_err(), "accepted {text}");
        }
    }

    #[test]
    fn default_shortcuts_match_pane_and_tab_actions() {
        let config = Config::default();
        for (binding, action) in [
            ("Alt+F", Action::ToggleFloats),
            ("Alt+M", Action::NewPane),
            ("Alt+H", Action::FocusLeft),
            ("Alt+J", Action::FocusDown),
            ("Alt+K", Action::FocusUp),
            ("Alt+L", Action::FocusRight),
            ("Alt+A", Action::NewTab),
            ("Alt+Shift+H", Action::PreviousTab),
            ("Alt+Shift+L", Action::NextTab),
            ("Alt+X", Action::ClosePane),
        ] {
            assert_eq!(config.keys.get(binding), Some(&action));
            assert!(shortcut(binding).unwrap().modifiers.alt);
        }
    }
}
