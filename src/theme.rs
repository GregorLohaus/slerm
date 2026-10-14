use crate::themes::THEMES;
use anyhow::{Result, bail};
use egui::Color32;
use libghostty_vt::style::{Palette, PaletteIndex, RgbColor};

/// A packed `0xRRGGBB` color. Themes are compiled in, so they hold plain
/// integers rather than anything that needs allocating or parsing at startup.
pub type Rgb = u32;

/// Shared stroke width for pane borders and the default hollow cursor.
pub const BORDER_WIDTH: f32 = 1.0;
pub const CORNER_RADIUS: u8 = 5;
pub const CURSOR_CORNER_RADIUS: u8 = 2;

/// Every color slerm draws with. A theme covers the terminal grid, the sixteen
/// ANSI colors programs address by index, and the chrome around them, so that
/// picking a light theme does not leave a dark tab bar behind.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Theme {
    /// Whether the chrome should be drawn as a dark or a light widget set.
    pub dark: bool,
    pub background: Rgb,
    pub foreground: Rgb,
    pub cursor: Rgb,
    pub selection_bg: Rgb,
    pub selection_fg: Rgb,
    /// The focused pane: its border, its title and the cursor.
    pub accent: Rgb,
    /// The tab bar, menus and pane title bars.
    pub chrome_bg: Rgb,
    pub chrome_fg: Rgb,
    /// The title bar of the focused pane.
    pub chrome_active_bg: Rgb,
    /// The border of an unfocused pane.
    pub border: Rgb,
    pub popup_bg: Rgb,
    /// ANSI 0..=15: black, red, green, yellow, blue, magenta, cyan, white, then
    /// the same eight again in their bright form.
    pub ansi: [Rgb; 16],
}

/// The name of slerm's own theme, which is what a config without a `theme` gets.
pub const DEFAULT_NAME: &str = "slerm";

/// slerm's own colors. The palette is libghostty's built-in one, so the default
/// look is exactly what the terminal renders with no theme applied at all.
pub const DEFAULT: Theme = Theme {
    dark: true,
    background: 0x17191c,
    foreground: 0xe2e4e8,
    cursor: 0x58c4ad,
    selection_bg: 0x2f3d44,
    selection_fg: 0xe2e4e8,
    accent: 0x58c4ad,
    chrome_bg: 0x1f2124,
    chrome_fg: 0xa8adb5,
    chrome_active_bg: 0x2b2e33,
    border: 0x363636,
    popup_bg: 0x1f2124,
    ansi: [
        0x1d1f21, 0xcc6666, 0xb5bd68, 0xf0c674, 0x81a2be, 0xb294bb, 0x8abeb7, 0xc5c8c6, 0x666666,
        0xd54e53, 0xb9ca4a, 0xe7c547, 0x7aa6da, 0xc397d8, 0x70c0b1, 0xeaeaea,
    ],
};

/// The theme with this name, if there is one. slerm's own comes first; the rest
/// are the Helix themes, which `THEMES` keeps sorted for the binary search.
pub fn find(name: &str) -> Option<Theme> {
    if name == DEFAULT_NAME {
        return Some(DEFAULT);
    }
    let index = THEMES.binary_search_by_key(&name, |(name, _)| name).ok()?;
    Some(THEMES[index].1)
}

/// Every theme name, in the order a picker should list them.
pub fn names() -> impl Iterator<Item = &'static str> {
    std::iter::once(DEFAULT_NAME).chain(THEMES.iter().map(|(name, _)| *name))
}

pub fn color(rgb: Rgb) -> Color32 {
    Color32::from_rgb((rgb >> 16) as u8, (rgb >> 8) as u8, rgb as u8)
}

pub fn rgb_color(rgb: Rgb) -> RgbColor {
    RgbColor {
        r: (rgb >> 16) as u8,
        g: (rgb >> 8) as u8,
        b: rgb as u8,
    }
}

/// Where one of the names a config may override writes to.
type Field = fn(&mut Theme) -> &mut Rgb;

/// The names a config may override, paired with the field each one writes.
/// Ordered so that `[colors]` reads the way a palette is usually written.
const FIELDS: [(&str, Field); 27] = [
    ("background", |t| &mut t.background),
    ("foreground", |t| &mut t.foreground),
    ("cursor", |t| &mut t.cursor),
    ("selection_background", |t| &mut t.selection_bg),
    ("selection_foreground", |t| &mut t.selection_fg),
    ("accent", |t| &mut t.accent),
    ("chrome_background", |t| &mut t.chrome_bg),
    ("chrome_foreground", |t| &mut t.chrome_fg),
    ("chrome_active_background", |t| &mut t.chrome_active_bg),
    ("border", |t| &mut t.border),
    ("popup_background", |t| &mut t.popup_bg),
    ("black", |t| &mut t.ansi[0]),
    ("red", |t| &mut t.ansi[1]),
    ("green", |t| &mut t.ansi[2]),
    ("yellow", |t| &mut t.ansi[3]),
    ("blue", |t| &mut t.ansi[4]),
    ("magenta", |t| &mut t.ansi[5]),
    ("cyan", |t| &mut t.ansi[6]),
    ("white", |t| &mut t.ansi[7]),
    ("bright_black", |t| &mut t.ansi[8]),
    ("bright_red", |t| &mut t.ansi[9]),
    ("bright_green", |t| &mut t.ansi[10]),
    ("bright_yellow", |t| &mut t.ansi[11]),
    ("bright_blue", |t| &mut t.ansi[12]),
    ("bright_magenta", |t| &mut t.ansi[13]),
    ("bright_cyan", |t| &mut t.ansi[14]),
    ("bright_white", |t| &mut t.ansi[15]),
];

impl Theme {
    /// Overrides one color by the name a config file uses for it.
    pub fn set(&mut self, name: &str, value: Rgb) -> Result<()> {
        let Some((_, field)) = FIELDS.iter().find(|(known, _)| *known == name) else {
            bail!("unknown color: {name}");
        };
        *field(self) = value;
        Ok(())
    }

    /// The 256-color palette a terminal renders indexed colors through. Only
    /// the first sixteen are themed; the color cube and greyscale ramp above
    /// them are fixed by the protocol, so they keep libghostty's values.
    pub fn palette(&self, default: Palette) -> Palette {
        const SLOTS: [PaletteIndex; 16] = [
            PaletteIndex::BLACK,
            PaletteIndex::RED,
            PaletteIndex::GREEN,
            PaletteIndex::YELLOW,
            PaletteIndex::BLUE,
            PaletteIndex::MAGENTA,
            PaletteIndex::CYAN,
            PaletteIndex::WHITE,
            PaletteIndex::BRIGHT_BLACK,
            PaletteIndex::BRIGHT_RED,
            PaletteIndex::BRIGHT_GREEN,
            PaletteIndex::BRIGHT_YELLOW,
            PaletteIndex::BRIGHT_BLUE,
            PaletteIndex::BRIGHT_MAGENTA,
            PaletteIndex::BRIGHT_CYAN,
            PaletteIndex::BRIGHT_WHITE,
        ];
        let mut palette = default;
        for (slot, rgb) in SLOTS.into_iter().zip(self.ansi) {
            palette.set(slot, rgb_color(rgb));
        }
        palette
    }

    /// egui's widget colors, so that the tab bar, menus and buttons follow the
    /// theme rather than staying on egui's own dark palette.
    pub fn visuals(&self) -> egui::Visuals {
        let mut visuals = if self.dark {
            egui::Visuals::dark()
        } else {
            egui::Visuals::light()
        };
        let (chrome, accent, text) = (
            color(self.chrome_bg),
            color(self.accent),
            color(self.chrome_fg),
        );
        let widgets = &mut visuals.widgets;
        widgets.noninteractive.bg_fill = chrome;
        widgets.noninteractive.weak_bg_fill = chrome;
        widgets.noninteractive.fg_stroke = egui::Stroke::new(1.0_f32, text);
        widgets.noninteractive.bg_stroke = egui::Stroke::new(1.0_f32, color(self.border));
        for widget in [
            &mut widgets.inactive,
            &mut widgets.hovered,
            &mut widgets.active,
            &mut widgets.open,
        ] {
            widget.bg_fill = color(self.chrome_active_bg);
            widget.weak_bg_fill = color(self.chrome_active_bg);
            widget.fg_stroke = egui::Stroke::new(1.0_f32, color(self.foreground));
            widget.bg_stroke = egui::Stroke::new(1.0_f32, color(self.border));
        }
        widgets.hovered.weak_bg_fill = color(self.selection_bg);
        widgets.active.weak_bg_fill = color(self.selection_bg);
        widgets.active.fg_stroke = egui::Stroke::new(1.0_f32, accent);
        visuals.panel_fill = chrome;
        visuals.window_fill = color(self.popup_bg);
        visuals.extreme_bg_color = color(self.background);
        visuals.selection.bg_fill = color(self.selection_bg);
        visuals.selection.stroke = egui::Stroke::new(1.0_f32, accent);
        visuals.hyperlink_color = accent;
        visuals.warn_fg_color = color(self.ansi[3]);
        visuals.error_fg_color = color(self.ansi[1]);
        visuals.window_stroke = egui::Stroke::new(1.0_f32, color(self.border));
        visuals
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_helix_theme_is_present_sorted_and_legible() {
        assert!(THEMES.len() > 150, "expected the full Helix theme set");
        assert!(
            THEMES.windows(2).all(|w| w[0].0 < w[1].0),
            "THEMES must be sorted for find() to binary search it"
        );
        for name in ["catppuccin_mocha", "gruvbox", "nord", "onedark", "dracula"] {
            assert!(find(name).is_some(), "missing {name}");
        }
        assert_eq!(find(DEFAULT_NAME), Some(DEFAULT));
        assert_eq!(find("no such theme"), None);
        assert_eq!(names().count(), THEMES.len() + 1);

        // A theme whose text matches its background, or whose palette collapses
        // onto one color, would render an unusable terminal.
        for (name, theme) in THEMES {
            let luma = |rgb: Rgb| {
                let channel = |shift: u32| ((rgb >> shift) & 0xff) as f32 / 255.0;
                0.2126 * channel(16) + 0.7152 * channel(8) + 0.0722 * channel(0)
            };
            assert!(
                (luma(theme.foreground) - luma(theme.background)).abs() > 0.15,
                "{name}: text is not readable on its background"
            );
            assert_eq!(theme.dark, luma(theme.background) < luma(theme.foreground));
            // Themes built on three or four colors (acme, vesper) reuse them
            // across slots; anything flatter than that has lost its palette.
            let distinct: std::collections::BTreeSet<_> = theme.ansi.iter().collect();
            assert!(distinct.len() >= 9, "{name}: palette is too flat");
        }
    }

    #[test]
    fn overrides_are_named_and_reach_the_palette() {
        let mut theme = DEFAULT;
        theme.set("red", 0x123456).unwrap();
        theme.set("bright_white", 0xfedcba).unwrap();
        theme.set("accent", 0x00ff00).unwrap();
        assert_eq!(theme.ansi[1], 0x123456);
        assert_eq!(theme.ansi[15], 0xfedcba);
        assert_eq!(theme.accent, 0x00ff00);
        for unknown in ["dark", "purple", "ansi", ""] {
            assert!(theme.set(unknown, 0).is_err(), "accepted {unknown}");
        }

        let palette = theme.palette(Palette([RgbColor { r: 0, g: 0, b: 0 }; 256]));
        assert_eq!(palette.get(PaletteIndex::RED), rgb_color(0x123456));
        assert_eq!(palette.get(PaletteIndex::BRIGHT_WHITE), rgb_color(0xfedcba));
        // Everything above the sixteen named slots is left as it was.
        assert_eq!(palette.0[16], RgbColor { r: 0, g: 0, b: 0 });
    }
}
