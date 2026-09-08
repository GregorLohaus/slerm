//! Converts the themes Helix ships in `runtime/themes` into `src/themes.rs`.
//!
//! Helix themes describe an editor: they name syntax scopes, not the sixteen
//! ANSI colors a terminal needs. The conversion therefore reads the UI scopes
//! directly (`ui.background`, `ui.statusline`, ...) and *infers* the ANSI
//! palette by scoring every color the theme uses against the hue each slot is
//! expected to hold, preferring the scopes that conventionally carry it.
//!
//!     cargo run --example gen_themes -- <helix>/runtime/themes src/themes.rs

use std::{collections::BTreeMap, fmt::Write as _, path::Path};
use toml::Value;

type Rgb = [u8; 3];

/// A color as written in a theme: its RGB value, plus the ANSI slot it named if
/// it was written as one of Helix's terminal color names rather than as hex.
#[derive(Clone, Copy, PartialEq, Eq)]
struct Color {
    rgb: Rgb,
    ansi: Option<u8>,
}

/// Helix's terminal color names and the palette slots they select. `gray` is
/// the dim one and `light-gray` the bright one, matching how Helix hands them
/// to crossterm (`Gray` -> dark grey, `LightGray` -> grey).
const NAMED: &[(&str, u8)] = &[
    ("black", 0),
    ("red", 1),
    ("green", 2),
    ("yellow", 3),
    ("blue", 4),
    ("magenta", 5),
    ("cyan", 6),
    ("light-gray", 7),
    ("gray", 8),
    ("light-red", 9),
    ("light-green", 10),
    ("light-yellow", 11),
    ("light-blue", 12),
    ("light-magenta", 13),
    ("light-cyan", 14),
    ("white", 15),
];

/// The xterm defaults, used both for themes that name terminal colors outright
/// and as the last resort for a slot nothing in a theme fits.
const XTERM: [Rgb; 16] = [
    [0x00, 0x00, 0x00],
    [0xcd, 0x00, 0x00],
    [0x00, 0xcd, 0x00],
    [0xcd, 0xcd, 0x00],
    [0x00, 0x00, 0xee],
    [0xcd, 0x00, 0xcd],
    [0x00, 0xcd, 0xcd],
    [0xe5, 0xe5, 0xe5],
    [0x7f, 0x7f, 0x7f],
    [0xff, 0x00, 0x00],
    [0x00, 0xff, 0x00],
    [0xff, 0xff, 0x00],
    [0x5c, 0x5c, 0xff],
    [0xff, 0x00, 0xff],
    [0x00, 0xff, 0xff],
    [0xff, 0xff, 0xff],
];

/// The hue each chromatic slot aims for, and the scopes that conventionally
/// carry it. Earlier scopes are preferred, but only as a tie-break: a theme
/// whose `function` is teal still gets a teal blue rather than a stray hue.
const SLOTS: [(u8, f32, &[&str]); 6] = [
    (
        1,
        0.0,
        &[
            "error",
            "diagnostic.error",
            "diff.minus",
            "keyword.control.exception",
            "markup.list.unchecked",
            "variable.builtin",
        ],
    ),
    (
        2,
        120.0,
        &[
            "string",
            "diff.plus",
            "markup.raw",
            "hint",
            "diagnostic.hint",
            "variable.other.member",
        ],
    ),
    (
        3,
        48.0,
        &[
            "warning",
            "diagnostic.warning",
            "constant.numeric",
            "attribute",
            "type",
            "markup.heading",
        ],
    ),
    (
        4,
        230.0,
        &[
            "function",
            "info",
            "diagnostic.info",
            "constructor",
            "markup.link.url",
            "diff.delta",
        ],
    ),
    (
        5,
        300.0,
        &[
            "keyword",
            "constant",
            "special",
            "namespace",
            "markup.link.text",
            "type.enum.variant",
        ],
    ),
    (
        6,
        185.0,
        &[
            "operator",
            "constant.character.escape",
            "markup.quote",
            "string.special",
            "punctuation.special",
            "label",
        ],
    ),
];

/// One theme file, before inheritance is resolved.
#[derive(Default)]
struct Raw {
    inherits: Option<String>,
    palette: BTreeMap<String, String>,
    /// Scope -> (fg, bg), each already flattened out of Helix's several
    /// spellings (bare string, `{ fg, bg }`, `{ underline = { color } }`).
    scopes: BTreeMap<String, (Option<String>, Option<String>)>,
}

fn read(path: &Path) -> Raw {
    let text = std::fs::read_to_string(path).expect("cannot read theme file");
    let table: toml::Table = toml::from_str(&text).expect("theme is not valid TOML");
    let mut raw = Raw::default();
    for (key, value) in table {
        match (key.as_str(), value) {
            ("inherits", Value::String(name)) => raw.inherits = Some(name),
            ("palette", Value::Table(entries)) => {
                for (name, value) in entries {
                    if let Value::String(color) = value {
                        raw.palette.insert(name, color);
                    }
                }
            }
            (_, Value::String(fg)) => {
                raw.scopes.insert(key, (Some(fg), None));
            }
            (_, Value::Table(style)) => {
                let text = |v: Option<&Value>| v.and_then(Value::as_str).map(str::to_owned);
                let fg = text(style.get("fg")).or_else(|| {
                    style
                        .get("underline")
                        .and_then(Value::as_table)
                        .and_then(|u| text(u.get("color")))
                });
                raw.scopes.insert(key, (fg, text(style.get("bg"))));
            }
            _ => {}
        }
    }
    raw
}

/// A theme with its `inherits` chain folded in: the child's palette entries and
/// whole scope entries win over the parent's, exactly as Helix merges them.
struct Theme {
    palette: BTreeMap<String, String>,
    scopes: BTreeMap<String, (Option<String>, Option<String>)>,
}

fn flatten(name: &str, raws: &BTreeMap<String, Raw>) -> Theme {
    let mut chain = vec![];
    let mut current = Some(name.to_owned());
    while let Some(name) = current {
        if chain.contains(&name) {
            break;
        }
        current = raws.get(&name).and_then(|r| r.inherits.clone());
        chain.push(name);
    }
    let mut theme = Theme {
        palette: BTreeMap::new(),
        scopes: BTreeMap::new(),
    };
    // Ancestors first so that descendants overwrite them.
    for name in chain.iter().rev() {
        let Some(raw) = raws.get(name) else { continue };
        theme.palette.extend(raw.palette.clone());
        theme.scopes.extend(raw.scopes.clone());
    }
    theme
}

impl Theme {
    fn color(&self, text: &str) -> Option<Color> {
        let text = self.palette.get(text).map(String::as_str).unwrap_or(text);
        if let Some((_, index)) = NAMED.iter().find(|(name, _)| *name == text) {
            return Some(Color {
                rgb: XTERM[*index as usize],
                ansi: Some(*index),
            });
        }
        let digits = text.strip_prefix('#')?;
        let expand = |c: u8| (c << 4) | c;
        let nibble = |i: usize| u8::from_str_radix(&digits[i..i + 1], 16).ok();
        let byte = |i: usize| u8::from_str_radix(&digits[i..i + 2], 16).ok();
        let rgb = match digits.len() {
            3 => [expand(nibble(0)?), expand(nibble(1)?), expand(nibble(2)?)],
            6 | 8 => [byte(0)?, byte(2)?, byte(4)?],
            _ => return None,
        };
        Some(Color { rgb, ansi: None })
    }

    fn fg(&self, scope: &str) -> Option<Color> {
        self.color(self.scopes.get(scope)?.0.as_deref()?)
    }

    fn bg(&self, scope: &str) -> Option<Color> {
        self.color(self.scopes.get(scope)?.1.as_deref()?)
    }

    /// Every color the theme mentions anywhere, with the number of scopes that
    /// use it. This is the pool the ANSI slots are chosen from; the count
    /// separates a theme's working colors from one-off accents such as the
    /// color of a jump label.
    fn all(&self) -> Vec<(Color, usize)> {
        let mut counts: BTreeMap<Rgb, (Color, usize)> = BTreeMap::new();
        let named = self.palette.values().map(String::as_str);
        // `ui.*` carries backgrounds, greys and one-off markers such as the
        // color of a jump label, none of which belong in a terminal palette.
        let scoped = self
            .scopes
            .iter()
            .filter(|(scope, _)| !scope.starts_with("ui."))
            .flat_map(|(_, (fg, bg))| [fg, bg])
            .filter_map(|c| c.as_deref());
        for color in named.chain(scoped).filter_map(|c| self.color(c)) {
            counts.entry(color.rgb).or_insert((color, 0)).1 += 1;
        }
        counts.into_values().collect()
    }
}

fn luma(c: Rgb) -> f32 {
    let channel = |v: u8| {
        let v = v as f32 / 255.0;
        if v <= 0.04045 {
            v / 12.92
        } else {
            ((v + 0.055) / 1.055).powf(2.4)
        }
    };
    0.2126 * channel(c[0]) + 0.7152 * channel(c[1]) + 0.0722 * channel(c[2])
}

fn contrast(a: Rgb, b: Rgb) -> f32 {
    let (a, b) = (luma(a) + 0.05, luma(b) + 0.05);
    if a > b { a / b } else { b / a }
}

/// Hue in degrees, saturation and lightness in 0..1.
fn hsl(c: Rgb) -> (f32, f32, f32) {
    let [r, g, b] = [
        c[0] as f32 / 255.0,
        c[1] as f32 / 255.0,
        c[2] as f32 / 255.0,
    ];
    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    let (span, sum) = (max - min, max + min);
    let lightness = sum / 2.0;
    if span < f32::EPSILON {
        return (0.0, 0.0, lightness);
    }
    let saturation = span / (1.0 - (sum - 1.0).abs());
    let hue = if max == r {
        60.0 * (((g - b) / span) % 6.0)
    } else if max == g {
        60.0 * ((b - r) / span + 2.0)
    } else {
        60.0 * ((r - g) / span + 4.0)
    };
    (hue.rem_euclid(360.0), saturation, lightness)
}

fn hue_distance(a: f32, b: f32) -> f32 {
    let d = (a - b).abs() % 360.0;
    d.min(360.0 - d)
}

fn mix(a: Rgb, b: Rgb, t: f32) -> Rgb {
    let channel = |i: usize| (a[i] as f32 + (b[i] as f32 - a[i] as f32) * t).round() as u8;
    [channel(0), channel(1), channel(2)]
}

const BLACK: Rgb = [0, 0, 0];
const WHITE: Rgb = [255, 255, 255];

/// The converted theme, mirroring the struct in `src/theme.rs`.
struct Out {
    dark: bool,
    background: Rgb,
    foreground: Rgb,
    cursor: Rgb,
    selection_bg: Rgb,
    selection_fg: Rgb,
    accent: Rgb,
    chrome_bg: Rgb,
    chrome_fg: Rgb,
    chrome_active_bg: Rgb,
    border: Rgb,
    popup_bg: Rgb,
    ansi: [Rgb; 16],
}

fn convert(theme: &Theme) -> Out {
    let text = theme
        .fg("ui.text")
        .or_else(|| theme.fg("ui.background"))
        .or_else(|| theme.fg("ui.statusline"));
    let background = theme
        .bg("ui.background")
        .or_else(|| theme.bg("ui.popup"))
        .or_else(|| theme.bg("ui.statusline"));
    // A theme that never states a background (the `term16` family leaves it to
    // the terminal) is placed by the brightness of its text instead.
    let dark = match (background, text) {
        (Some(bg), _) => luma(bg.rgb) < 0.22,
        (None, Some(fg)) => luma(fg.rgb) > 0.22,
        (None, None) => true,
    };
    let background = background.map_or(if dark { BLACK } else { WHITE }, |c| c.rgb);
    let mut foreground = text.map_or(if dark { WHITE } else { BLACK }, |c| c.rgb);
    // A theme that states neither (peachpuff leaves both to the terminal) can
    // resolve to text the same shade as its background, which renders nothing.
    if contrast(foreground, background) < 2.0 {
        foreground = if dark { WHITE } else { BLACK };
    }
    let far = if dark { WHITE } else { BLACK };

    let ansi = palette(theme, background, foreground, dark);
    let cursor = theme
        .bg("ui.cursor.primary")
        .or_else(|| theme.bg("ui.cursor"))
        .or_else(|| theme.fg("ui.cursor.primary"))
        .or_else(|| theme.fg("ui.cursor"))
        .map_or(foreground, |c| c.rgb);
    let selection_bg = theme
        .bg("ui.selection.primary")
        .or_else(|| theme.bg("ui.selection"))
        .map_or(mix(background, far, 0.22), |c| c.rgb);
    let selection_fg = theme
        .fg("ui.selection.primary")
        .or_else(|| theme.fg("ui.selection"))
        .map(|c| c.rgb)
        .filter(|c| contrast(*c, selection_bg) >= 2.5)
        .unwrap_or_else(|| {
            if contrast(foreground, selection_bg) >= 2.5 {
                foreground
            } else {
                mix(
                    selection_bg,
                    if luma(selection_bg) > 0.3 {
                        BLACK
                    } else {
                        WHITE
                    },
                    0.85,
                )
            }
        });
    let chrome_bg = theme
        .bg("ui.statusline")
        .or_else(|| theme.bg("ui.popup"))
        .map(|c| c.rgb)
        .filter(|c| *c != background)
        .unwrap_or_else(|| mix(background, far, 0.09));
    let chrome_fg = theme
        .fg("ui.statusline")
        .map(|c| c.rgb)
        .filter(|c| contrast(*c, chrome_bg) >= 2.0)
        .unwrap_or_else(|| mix(chrome_bg, far, 0.7));
    // The accent marks the focused pane: it is drawn both on the title bar and
    // as a border on the terminal background, so it has to read on both.
    let readable = |c: Rgb| contrast(c, chrome_bg) >= 2.5 && contrast(c, background) >= 2.5;
    let vivid = [theme.fg("ui.cursor.match").map(|c| c.rgb), Some(cursor)]
        .into_iter()
        .flatten()
        .find(|c| readable(*c) && hsl(*c).1 > 0.30);
    let accent = vivid
        // A theme whose cursor is a plain grey still deserves a colored focus
        // ring, so fall back to its own blue, cyan or magenta.
        .or_else(|| {
            [4, 6, 5, 3]
                .into_iter()
                .map(|i| ansi[i])
                .find(|c| readable(*c))
        })
        .or(Some(cursor).filter(|c| readable(*c)))
        .unwrap_or(foreground);
    Out {
        dark,
        background,
        foreground,
        cursor,
        selection_bg,
        selection_fg,
        accent,
        chrome_bg,
        chrome_fg,
        // The focused pane's title bar steps away from the unfocused ones,
        // whichever direction that is for this theme's chrome.
        chrome_active_bg: mix(
            chrome_bg,
            if luma(chrome_bg) > 0.18 { BLACK } else { WHITE },
            0.14,
        ),
        // Unfocused borders are a hint, not a frame, so a theme with a loud
        // window color is pulled back towards its background.
        border: theme
            .fg("ui.window")
            .or_else(|| theme.fg("ui.virtual.indent-guide"))
            .map_or(mix(background, far, 0.28), |c| mix(background, c.rgb, 0.65)),
        popup_bg: theme.bg("ui.popup").map_or(chrome_bg, |c| c.rgb),
        ansi,
    }
}

/// Picks the sixteen ANSI colors. The six chromatic slots score every color the
/// theme uses against the slot's hue; the greys are derived from the background
/// and foreground so that they always stay legible against the theme's own
/// background.
fn palette(theme: &Theme, background: Rgb, foreground: Rgb, dark: bool) -> [Rgb; 16] {
    let pool = theme.all();
    let mut ansi = XTERM;
    // A theme whose palette names terminal colors outright is already
    // describing a palette; those entries are taken as written.
    let named: BTreeMap<u8, Rgb> = theme
        .palette
        .keys()
        .filter_map(|name| Some((slot_named(name)?, theme.color(name)?.rgb)))
        .collect();
    let mut taken: Vec<Rgb> = vec![];
    let mut assigned = [false; 7];
    for (index, rgb) in &named {
        ansi[*index as usize] = *rgb;
        if let Some(slot) = assigned.get_mut(*index as usize) {
            *slot = true;
            taken.push(*rgb);
        }
    }

    // Score every slot against every color the theme uses, then hand each color
    // to the slot that wants it most. Assigning greedily one slot at a time
    // would let an early slot take a color a later slot needed far more, which
    // is how palettes with no true blue end up with two cyans.
    let mut pairs: Vec<(f32, usize, Rgb)> = vec![];
    for (index, hue, scopes) in SLOTS {
        if assigned[index as usize] {
            continue;
        }
        let preferred: Vec<Rgb> = scopes
            .iter()
            .filter_map(|s| Some(theme.fg(s)?.rgb))
            .collect();
        for (candidate, uses) in &pool {
            let (h, s, l) = hsl(candidate.rgb);
            // A grey, an unreadable color, the plain text color and a color the
            // theme only spelled as a terminal name (which says nothing about
            // what that terminal color should be) are never palette entries.
            if s < 0.10
                || contrast(candidate.rgb, background) < 1.8
                || candidate.ansi.is_some()
                || candidate.rgb == foreground
                || candidate.rgb == background
            {
                continue;
            }
            let rank = preferred
                .iter()
                .position(|p| *p == candidate.rgb)
                .map_or(20.0, |r| r as f32 * 2.5);
            // Whichever slot's hue this color sits closest to has the better
            // claim on it, so that a theme's red is not spent on magenta just
            // because it happens to be the color of `keyword`.
            let closest = SLOTS
                .iter()
                .map(|(_, target, _)| *target)
                .min_by(|a, b| hue_distance(h, *a).total_cmp(&hue_distance(h, *b)));
            let score = hue_distance(h, hue) * 1.5
                + rank
                + (1.0 - s) * 38.0
                + if closest == Some(hue) { 0.0 } else { 45.0 }
                // A color used once is a one-off accent, not a working color.
                + match uses {
                    0 | 1 => 22.0,
                    2 => 10.0,
                    _ => 0.0,
                }
                + if l > 0.85 { 40.0 } else { 0.0 };
            pairs.push((score, index as usize, candidate.rgb));
        }
    }
    pairs.sort_by(|a, b| a.0.total_cmp(&b.0).then(a.1.cmp(&b.1)));
    for (_, index, rgb) in &pairs {
        if !assigned[*index] && !taken.contains(rgb) {
            assigned[*index] = true;
            ansi[*index] = *rgb;
            taken.push(*rgb);
        }
    }
    // A theme with fewer than six usable colors leaves slots over; those share
    // whichever color came closest rather than falling back to a stock xterm
    // primary that belongs to no theme at all.
    for (_, index, rgb) in &pairs {
        if !assigned[*index] {
            assigned[*index] = true;
            ansi[*index] = *rgb;
        }
    }
    for (index, _, _) in SLOTS {
        if !assigned[index as usize] {
            ansi[index as usize] = mix(XTERM[index as usize], foreground, 0.4);
        }
    }

    // The bright variant is the theme's own lighter shade of the same color
    // where it has one, and a lightened copy otherwise. Matching the hue is not
    // enough: a pale grey-blue is a poor bright blue, so the candidate has to
    // keep most of the base color's chroma, and it cannot be a color another
    // slot already owns.
    let chroma = |c: Rgb| (*c.iter().max().unwrap() - *c.iter().min().unwrap()) as f32;
    for (index, _, _) in SLOTS {
        if named.contains_key(&(index + 8)) {
            continue;
        }
        let base = ansi[index as usize];
        let (base_hue, _, lightness) = hsl(base);
        let brighter = pool
            .iter()
            .map(|(c, _)| c)
            .filter(|c| {
                let (h, _, l) = hsl(c.rgb);
                hue_distance(h, base_hue) <= 30.0
                    && chroma(c.rgb) >= chroma(base) * 0.7
                    && (0.05..=0.40).contains(&(l - lightness))
                    && contrast(c.rgb, background) >= 2.0
                    && !taken.contains(&c.rgb)
            })
            .min_by(|a, b| hsl(a.rgb).2.total_cmp(&hsl(b.rgb).2));
        ansi[index as usize + 8] = brighter.map_or_else(
            || mix(base, WHITE, if lightness > 0.7 { 0.12 } else { 0.26 }),
            |c| c.rgb,
        );
    }

    // Greys: black through bright white always span from the theme's darker end
    // to its lighter end, so ANSI text stays readable in light themes too.
    let comment = theme
        .fg("comment")
        .map(|c| c.rgb)
        .filter(|c| contrast(*c, background) >= 2.0);
    let greys = if dark {
        [
            mix(background, WHITE, 0.13),
            foreground,
            comment.unwrap_or_else(|| mix(background, WHITE, 0.42)),
            mix(foreground, WHITE, 0.35),
        ]
    } else {
        [
            mix(foreground, BLACK, 0.25),
            mix(background, BLACK, 0.13),
            comment.unwrap_or_else(|| mix(foreground, background, 0.45)),
            background,
        ]
    };
    for (slot, grey) in [0, 7, 8, 15].into_iter().zip(greys) {
        ansi[slot as usize] = *named.get(&slot).unwrap_or(&grey);
    }
    ansi
}

/// The palette slot a `[palette]` key names outright, so that a theme defining
/// `red`/`bright-blue` and friends keeps its own values.
fn slot_named(name: &str) -> Option<u8> {
    let name = name.to_ascii_lowercase().replace('_', "-");
    let (bright, base) = match name
        .strip_prefix("bright-")
        .or_else(|| name.strip_prefix("light-"))
    {
        Some(base) => (8, base.to_owned()),
        None => (0, name),
    };
    let index = match base.as_str() {
        "black" => 0,
        "red" => 1,
        "green" => 2,
        "yellow" => 3,
        "blue" => 4,
        "magenta" => 5,
        "cyan" => 6,
        "white" => 7,
        _ => return None,
    };
    Some(index + bright)
}

fn main() {
    let mut args = std::env::args().skip(1);
    let dir = args
        .next()
        .expect("usage: gen_themes <themes dir> <out.rs>");
    let out = args
        .next()
        .expect("usage: gen_themes <themes dir> <out.rs>");

    let mut raws = BTreeMap::new();
    for entry in std::fs::read_dir(&dir).expect("cannot read themes directory") {
        let path = entry.expect("cannot read theme").path();
        if path.extension().is_some_and(|e| e == "toml") {
            let name = path.file_stem().unwrap().to_string_lossy().into_owned();
            raws.insert(name, read(&path));
        }
    }

    let mut text = String::from(
        "//! The themes Helix ships with, converted by `cargo run --example gen_themes`.\n\
         //! Generated file: edit the generator, not this.\n\
         //!\n\
         //! Upstream: <https://github.com/helix-editor/helix/tree/master/runtime/themes>.\n\
         //! The individual themes keep their own authors and licenses; see `themes/LICENSES`.\n\n\
         use crate::theme::Theme;\n\n\
         /// Every built-in theme, sorted by name so that lookup can binary search.\n\
         pub static THEMES: &[(&str, Theme)] = &[\n",
    );
    for name in raws.keys() {
        let out = convert(&flatten(name, &raws));
        let hex = |c: Rgb| format!("0x{:02x}{:02x}{:02x}", c[0], c[1], c[2]);
        let ansi = out
            .ansi
            .iter()
            .map(|c| hex(*c))
            .collect::<Vec<_>>()
            .join(", ");
        writeln!(
            text,
            "    (\n        {name:?},\n        Theme {{\n            \
             dark: {dark},\n            background: {bg},\n            foreground: {fg},\n            \
             cursor: {cursor},\n            \
             selection_bg: {sel_bg},\n            selection_fg: {sel_fg},\n            \
             accent: {accent},\n            chrome_bg: {chrome_bg},\n            \
             chrome_fg: {chrome_fg},\n            chrome_active_bg: {chrome_active},\n            \
             border: {border},\n            popup_bg: {popup},\n            \
             ansi: [\n                {ansi},\n            ],\n        }},\n    ),",
            dark = out.dark,
            bg = hex(out.background),
            fg = hex(out.foreground),
            cursor = hex(out.cursor),
            sel_bg = hex(out.selection_bg),
            sel_fg = hex(out.selection_fg),
            accent = hex(out.accent),
            chrome_bg = hex(out.chrome_bg),
            chrome_fg = hex(out.chrome_fg),
            chrome_active = hex(out.chrome_active_bg),
            border = hex(out.border),
            popup = hex(out.popup_bg),
        )
        .unwrap();
    }
    text.push_str("];\n");
    std::fs::write(&out, text).expect("cannot write output");
    eprintln!("wrote {} themes to {out}", raws.len());
}
