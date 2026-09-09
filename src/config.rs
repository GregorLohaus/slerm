use crate::theme::{self, Theme};
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
    pub font_family: Option<String>,
    pub font_bold_path: Option<PathBuf>,
    pub font_italic_path: Option<PathBuf>,
    pub font_bold_italic_path: Option<PathBuf>,
    pub font_fallbacks: Vec<String>,
    pub scrollback: usize,
    pub scrollback_editor: Option<String>,
    pub window_width: f32,
    pub window_height: f32,
    pub theme: String,
    /// Shorthand for the `[colors]` entries of the same name.
    pub background: Option<String>,
    pub foreground: Option<String>,
    pub accent: Option<String>,
    /// Colors overriding the theme, keyed by the names in `theme::FIELDS`.
    pub colors: BTreeMap<String, String>,
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
    pub floating_max_width: f32,
    pub floating_max_height: f32,
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
    OpenScrollback,
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
            // The fractions alone make a float fill half of a large display, which is
            // far more than a scratch terminal needs, so cap them in points too.
            floating_max_width: 960.0,
            floating_max_height: 640.0,
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
            font_family: None,
            font_bold_path: None,
            font_italic_path: None,
            font_bold_italic_path: None,
            font_fallbacks: Vec::new(),
            scrollback: 10000,
            scrollback_editor: None,
            window_width: 1100.0,
            window_height: 720.0,
            theme: theme::DEFAULT_NAME.into(),
            background: None,
            foreground: None,
            accent: None,
            colors: BTreeMap::new(),
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
                ("Alt+S", Action::OpenScrollback),
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
            config.font_path.is_none() || config.font_family.is_none(),
            "set either font_family or font_path, not both"
        );
        ensure!(
            config
                .font_family
                .iter()
                .chain(&config.font_fallbacks)
                .all(|name| !name.trim().is_empty()),
            "font family names must not be empty"
        );
        ensure!(
            !config.shell.is_empty() && !config.shell[0].is_empty(),
            "shell must contain an executable"
        );
        if let Some(editor) = &config.scrollback_editor {
            let command =
                shell_words::split(editor).context("Invalid scrollback_editor command")?;
            ensure!(
                !command.is_empty() && !command[0].is_empty(),
                "scrollback_editor must contain an executable"
            );
        }
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
        for points in [
            config.multiplexer.floating_max_width,
            config.multiplexer.floating_max_height,
        ] {
            ensure!(
                (160.0..=16384.0).contains(&points),
                "floating maximums must be 160..16384 points"
            );
        }
        config.theme()?;
        for key in config.keys.keys() {
            shortcut(key)?;
        }
        Ok(config)
    }

    /// The configured theme with the `[colors]` overrides applied. Validated by
    /// `parse`, so a config that loaded is a config whose theme resolves.
    pub fn theme(&self) -> Result<Theme> {
        let mut theme = theme::find(&self.theme)
            .with_context(|| format!("unknown theme: {} (try --list-themes)", self.theme))?;
        let shorthand = [
            ("background", &self.background),
            ("foreground", &self.foreground),
            ("accent", &self.accent),
        ];
        let named = shorthand
            .iter()
            .filter_map(|(name, value)| Some((*name, value.as_ref()?)))
            .chain(self.colors.iter().map(|(k, v)| (k.as_str(), v)));
        for (name, value) in named {
            theme.set(name, packed(value)?)?;
        }
        Ok(theme)
    }
}

/// A `#RRGGBB` string as the packed integer a theme stores.
pub fn packed(text: &str) -> Result<theme::Rgb> {
    let color = rgb(text)?;
    Ok((color.r as u32) << 16 | (color.g as u32) << 8 | color.b as u32)
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
            "font_family = ''",
            "font_fallbacks = [' ']",
            "font_family = 'Test'\nfont_path = '/font.ttf'",
            "shell = []",
            "scrollback_editor = ''",
            "scrollback_editor = ' '",
            "scrollback_editor = \"''\"",
            "scrollback_editor = \"'\"",
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
            ("Alt+S", Action::OpenScrollback),
        ] {
            assert_eq!(config.keys.get(binding), Some(&action));
            assert!(shortcut(binding).unwrap().modifiers.alt);
        }
    }
}
