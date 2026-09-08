use crate::theme::{self, Theme};
use anyhow::{Result, ensure};
use egui::{
    Color32, ColorImage, FontId, Painter, Rect, Stroke, TextureHandle, TextureOptions, pos2, vec2,
};
use libghostty_vt::{
    RenderState, Terminal,
    alloc::{Allocator, Bytes},
    kitty::graphics::{DecodePng, DecodedImage, ImageFormat, Layer, PlacementIterator},
    render::{CellIterator, Colors, CursorVisualStyle, Dirty, RowIterator},
    screen::CellWide,
    style::{RgbColor, Underline},
};
use std::{
    collections::{HashMap, HashSet},
    io::Cursor,
    sync::Arc,
};

/// Where a pane's terminal is drawn this frame, and the colors that are slerm's
/// own rather than the terminal's.
#[derive(Clone, Copy)]
pub struct Grid<'a> {
    pub rect: Rect,
    pub cell: egui::Vec2,
    pub font: &'a FontId,
    pub theme: &'a Theme,
    pub active: bool,
}

pub fn color(c: RgbColor) -> Color32 {
    Color32::from_rgb(c.r, c.g, c.b)
}

pub fn cell_size(painter: &Painter, font: &FontId) -> egui::Vec2 {
    painter.ctx().fonts_mut(|fonts| {
        vec2(
            fonts.glyph_width(font, 'M').ceil().max(1.0),
            fonts.row_height(font).ceil().max(1.0),
        )
    })
}

fn powerline_shape(text: &str, rect: Rect, color: Color32) -> Option<egui::Shape> {
    let mut chars = text.chars();
    let code = chars.next()? as u32;
    if !(0xe0b0..=0xe0b7).contains(&code) || chars.next().is_some() {
        return None;
    }
    let left = matches!(code, 0xe0b2 | 0xe0b3 | 0xe0b6 | 0xe0b7);
    let filled = code.is_multiple_of(2);
    let mut points = if code < 0xe0b4 {
        vec![
            rect.left_top(),
            pos2(rect.right(), rect.center().y),
            rect.left_bottom(),
        ]
    } else {
        (0..=24)
            .map(|step| {
                let angle = std::f32::consts::PI * (step as f32 / 24.0 - 0.5);
                pos2(
                    rect.left() + rect.width() * angle.cos().max(0.0),
                    rect.center().y + rect.height() * 0.5 * angle.sin(),
                )
            })
            .collect::<Vec<_>>()
    };
    points[0] = rect.left_top();
    *points.last_mut().unwrap() = rect.left_bottom();
    if left {
        for point in &mut points {
            point.x = rect.right() - (point.x - rect.left());
        }
    }
    Some(if filled {
        egui::Shape::convex_polygon(points, color, Stroke::NONE)
    } else {
        egui::Shape::line(points, Stroke::new(1.0_f32, color))
    })
}

pub struct PngDecoder;
impl DecodePng for PngDecoder {
    fn decode_png<'a>(
        &mut self,
        alloc: &'a Allocator<'_>,
        bytes: &[u8],
    ) -> Option<DecodedImage<'a>> {
        let mut decoder = png::Decoder::new(Cursor::new(bytes));
        decoder.set_transformations(png::Transformations::EXPAND | png::Transformations::STRIP_16);
        decoder.set_limits(png::Limits {
            bytes: 128 * 1024 * 1024,
        });
        let mut reader = decoder.read_info().ok()?;
        let size = reader.output_buffer_size()?;
        if size > 128 * 1024 * 1024 {
            return None;
        }
        let mut pixels = vec![0; size];
        let info = reader.next_frame(&mut pixels).ok()?;
        let count = (info.width as usize).checked_mul(info.height as usize)?;
        let len = count.checked_mul(4)?;
        if len > 128 * 1024 * 1024 {
            return None;
        }
        let mut data = Bytes::new_with_alloc(alloc, len).ok()?;
        let input = &pixels[..info.buffer_size()];
        match info.color_type {
            png::ColorType::Rgba => data.copy_from_slice(input),
            png::ColorType::Rgb => {
                for (out, src) in data.chunks_exact_mut(4).zip(input.chunks_exact(3)) {
                    out.copy_from_slice(&[src[0], src[1], src[2], 255]);
                }
            }
            png::ColorType::Grayscale => {
                for (out, value) in data.chunks_exact_mut(4).zip(input) {
                    out.copy_from_slice(&[*value, *value, *value, 255]);
                }
            }
            png::ColorType::GrayscaleAlpha => {
                for (out, src) in data.chunks_exact_mut(4).zip(input.chunks_exact(2)) {
                    out.copy_from_slice(&[src[0], src[0], src[0], src[1]]);
                }
            }
            _ => return None,
        }
        Some(DecodedImage {
            width: info.width,
            height: info.height,
            data,
        })
    }
}

#[derive(Default)]
struct CachedRow {
    backgrounds: Vec<egui::Shape>,
    foregrounds: Vec<egui::Shape>,
    selection: Option<(u16, u16)>,
}

fn layout_glyph(painter: &Painter, text: String, font: &FontId, height: f32) -> Arc<egui::Galley> {
    layout_styled_glyph(painter, text, font, height, false)
}

fn layout_styled_glyph(
    painter: &Painter,
    text: String,
    font: &FontId,
    height: f32,
    italics: bool,
) -> Arc<egui::Galley> {
    let mut job =
        egui::text::LayoutJob::simple(text, font.clone(), Color32::PLACEHOLDER, f32::INFINITY);
    job.sections[0].format.line_height = Some(height);
    job.sections[0].format.valign = egui::Align::Center;
    job.sections[0].format.italics = italics;
    painter.layout_job(job)
}

fn nerd_icon(text: &str) -> bool {
    // Includes the supplementary private-use plane used by Material Design.
    text.chars()
        .next()
        .is_some_and(|c| matches!(c as u32, 0xe000..=0xf8ff | 0xf0000..=0xffffd))
}

fn icon_shape(galley: Arc<egui::Galley>, rect: Rect, color: Color32) -> egui::Shape {
    // Use ink bounds: patched fonts can have bearings or outlines that extend
    // well beyond their nominal advance. Fit uniformly and center in the cell
    // allocation so icons cannot cover neighboring text or rows.
    let bounds = galley.mesh_bounds;
    let mut shape = egui::epaint::TextShape::new(egui::Pos2::ZERO, galley, color);
    if bounds.is_positive() {
        let scale = (rect.width() / bounds.width())
            .min(rect.height() / bounds.height())
            .min(1.0);
        shape.transform(egui::emath::TSTransform {
            scaling: scale,
            translation: rect.center().to_vec2() - bounds.center().to_vec2() * scale,
        });
    } else {
        shape.pos = rect.min;
    }
    shape.into()
}

pub struct Renderer {
    state: RenderState<'static>,
    rows: RowIterator<'static>,
    cells: CellIterator<'static>,
    placements: PlacementIterator<'static>,
    textures: HashMap<u32, (u64, TextureHandle)>,
    row_cache: Vec<CachedRow>,
    glyphs: HashMap<(String, bool, bool), Arc<egui::Galley>>,
    font_marker: Option<Arc<egui::Galley>>,
    synthetic_styles: [(bool, bool); 4],
    cached_colors: Option<Colors>,
    cached_selection: (Color32, Color32),
    cached_cell: egui::Vec2,
    cached_cols: u16,
    #[cfg(test)]
    rebuilt_rows: usize,
}

impl Renderer {
    pub fn new() -> Result<Self> {
        Ok(Self {
            state: RenderState::new()?,
            rows: RowIterator::new()?,
            cells: CellIterator::new()?,
            placements: PlacementIterator::new()?,
            textures: HashMap::new(),
            row_cache: Vec::new(),
            glyphs: HashMap::new(),
            font_marker: None,
            synthetic_styles: [(false, false); 4],
            cached_colors: None,
            cached_selection: (Color32::PLACEHOLDER, Color32::PLACEHOLDER),
            cached_cell: egui::Vec2::ZERO,
            cached_cols: 0,
            #[cfg(test)]
            rebuilt_rows: 0,
        })
    }

    pub fn paint(
        &mut self,
        term: &Terminal<'static, '_>,
        painter: &Painter,
        grid: &Grid<'_>,
    ) -> Result<()> {
        let &Grid {
            rect,
            cell,
            font,
            theme,
            active,
        } = grid;
        let painter = painter.with_clip_rect(rect.intersect(painter.clip_rect()));
        // Keep one egui galley alive in its cache each frame. A new identity means
        // fonts/DPI/the atlas were reset, invalidating every cached glyph's UVs.
        let marker = layout_glyph(&painter, "M".into(), font, cell.y);
        let fonts_changed = self
            .font_marker
            .as_ref()
            .is_none_or(|old| !Arc::ptr_eq(old, &marker))
            || self.cached_cell != cell
            || self.glyphs.len() > 4096;
        if fonts_changed {
            self.row_cache.clear();
            self.glyphs.clear();
        }
        self.font_marker = Some(marker);
        self.cached_cell = cell;
        let style_fonts = painter.ctx().fonts(|fonts| {
            [(false, false), (true, false), (false, true), (true, true)].map(|(bold, italic)| {
                let family = crate::fonts::family(bold, italic);
                if font.family == egui::FontFamily::Monospace
                    && fonts.definitions().families.contains_key(&family)
                {
                    FontId::new(font.size, family)
                } else {
                    font.clone()
                }
            })
        });
        if fonts_changed {
            self.synthetic_styles = painter.ctx().fonts(|fonts| {
                let first_face = |index: usize| {
                    fonts.definitions().families[&style_fonts[index].family]
                        .first()
                        .map(|name| &fonts.definitions().font_data[name])
                };
                std::array::from_fn::<_, 4, _>(|index| {
                    (
                        index & 1 != 0 && first_face(index) == first_face(index & !1),
                        index & 2 != 0 && first_face(index) == first_face(index & !2),
                    )
                })
            });
        }
        let synthetic = self.synthetic_styles;
        let snapshot = self.state.update(term)?;
        let colors = snapshot.colors()?;
        let cols = snapshot.cols()?;
        let row_count = snapshot.rows()? as usize;
        let selected_colors = (
            theme::color(theme.selection_bg),
            theme::color(theme.selection_fg),
        );
        let rebuild_all = snapshot.dirty()? == Dirty::Full
            || self.cached_colors.as_ref() != Some(&colors)
            || self.cached_selection != selected_colors
            || self.cached_cols != cols
            || self.row_cache.len() != row_count;
        self.row_cache.resize_with(row_count, CachedRow::default);
        self.cached_colors = Some(colors.clone());
        self.cached_selection = selected_colors;
        self.cached_cols = cols;
        painter.rect_filled(rect, 0, color(colors.background));
        let mut rows = self.rows.update(&snapshot)?;
        let mut text = String::new();
        let mut y = 0;
        while let Some(row) = rows.next() {
            // Selection can change independently of a row's text dirty flag.
            let selection = row.selection()?.map(|s| (s.start_x, s.end_x));
            if !rebuild_all && !row.dirty()? && self.row_cache[y].selection == selection {
                y += 1;
                continue;
            }
            #[cfg(test)]
            {
                self.rebuilt_rows += 1;
            }
            let mut cached = CachedRow {
                selection,
                ..Default::default()
            };
            let backgrounds = &mut cached.backgrounds;
            let foregrounds = &mut cached.foregrounds;
            let mut cells = self.cells.update(row)?;
            let mut x = 0;
            while let Some(c) = cells.next() {
                let pos = pos2(x as f32 * cell.x, y as f32 * cell.y);
                let cell_rect = Rect::from_min_size(pos, cell);
                let style = c.style()?;
                let mut fg = color(c.fg_color()?.unwrap_or(colors.foreground));
                let explicit_bg = c.bg_color()?;
                let mut bg = color(explicit_bg.unwrap_or(colors.background));
                let selected = c.is_selected()?;
                if style.inverse {
                    std::mem::swap(&mut fg, &mut bg);
                }
                // A selection takes the theme's own colors rather than inverting
                // the cell, which would be invisible over inverted text.
                if selected {
                    (bg, fg) = selected_colors;
                }
                if explicit_bg.is_some() || style.inverse || selected {
                    backgrounds.push(egui::Shape::rect_filled(cell_rect, 0, bg));
                }
                if style.faint {
                    fg = fg.gamma_multiply(0.6);
                }
                let wide = c.raw_cell()?.wide()?;
                if !style.invisible
                    && !matches!(wide, CellWide::SpacerHead | CellWide::SpacerTail)
                    && c.graphemes_len()? > 0
                {
                    c.graphemes_utf8(&mut text)?;
                    // Placeholder cells are reserved by the Kitty protocol, not printable text.
                    if let Some(separator) = powerline_shape(&text, cell_rect, fg) {
                        foregrounds.push(separator);
                    } else if text != " " && !text.starts_with('\u{10eeee}') {
                        let index = usize::from(style.bold) + 2 * usize::from(style.italic);
                        let icon = nerd_icon(&text);
                        let key = (text.clone(), style.bold, style.italic);
                        let galley = if let Some(galley) = self.glyphs.get(&key) {
                            galley.clone()
                        } else {
                            let font = &style_fonts[index];
                            let galley = layout_styled_glyph(
                                &painter,
                                text.clone(),
                                font,
                                cell.y,
                                synthetic[index].1 && !icon,
                            );
                            self.glyphs.insert(key, galley.clone());
                            galley
                        };
                        // Keep style emphasis for regular-only fonts; real style
                        // faces are drawn once. Never distort symbol outlines.
                        if synthetic[index].0 && !icon {
                            foregrounds.push(egui::Shape::galley(
                                pos + vec2(0.45, 0.0),
                                galley.clone(),
                                fg,
                            ));
                        }
                        foregrounds.push(if icon {
                            let width = if wide == CellWide::Wide { 2.0 } else { 1.0 };
                            icon_shape(
                                galley,
                                Rect::from_min_size(pos, vec2(cell.x * width, cell.y)),
                                fg,
                            )
                        } else {
                            egui::Shape::galley(pos, galley, fg)
                        });
                    }
                }
                for (enabled, height) in [
                    (style.underline != Underline::None, cell.y - 2.0),
                    (style.strikethrough, cell.y * 0.55),
                    (style.overline, 1.0),
                ] {
                    if enabled {
                        foregrounds.push(egui::Shape::line_segment(
                            [pos + vec2(0.0, height), pos + vec2(cell.x, height)],
                            Stroke::new(1.0_f32, fg),
                        ));
                    }
                }
                x += 1;
            }
            self.row_cache[y] = cached;
            row.set_dirty(false)?;
            y += 1;
        }
        // Separate passes preserve Kitty's three z-order bands exactly.
        let mut seen = HashSet::new();
        for layer in [Layer::BelowBg, Layer::BelowText, Layer::AboveText] {
            if layer == Layer::BelowText {
                painter.extend(
                    self.row_cache
                        .iter()
                        .flat_map(|row| row.backgrounds.iter())
                        .cloned()
                        .map(|mut shape| {
                            shape.translate(rect.min.to_vec2());
                            shape
                        }),
                );
            }
            if layer == Layer::AboveText {
                painter.extend(
                    self.row_cache
                        .iter()
                        .flat_map(|row| row.foregrounds.iter())
                        .cloned()
                        .map(|mut shape| {
                            shape.translate(rect.min.to_vec2());
                            shape
                        }),
                );
            }
            paint_images(
                term,
                &painter,
                rect,
                cell,
                layer,
                &mut self.placements,
                &mut self.textures,
                &mut seen,
            )?;
        }
        self.textures.retain(|id, _| seen.contains(id));
        if snapshot.cursor_visible()?
            && let Some(cursor) = snapshot.cursor_viewport()?
        {
            let mut cursor_rect = Rect::from_min_size(
                rect.min + vec2(cursor.x as f32 * cell.x, cursor.y as f32 * cell.y),
                cell,
            );
            let c = color(colors.cursor.unwrap_or(colors.foreground));
            match snapshot.cursor_visual_style()? {
                CursorVisualStyle::Bar if active => {
                    cursor_rect.max.x = cursor_rect.min.x + 2.0;
                    painter.rect_filled(cursor_rect, 0, c);
                }
                CursorVisualStyle::Underline if active => {
                    cursor_rect.min.y = cursor_rect.max.y - 2.0;
                    painter.rect_filled(cursor_rect, 0, c);
                }
                _ => {
                    painter.rect_stroke(
                        cursor_rect,
                        0,
                        Stroke::new(if active { 1.5_f32 } else { 0.75_f32 }, c),
                        egui::StrokeKind::Inside,
                    );
                }
            }
        }
        snapshot.set_dirty(Dirty::Clean)?;
        Ok(())
    }
}

#[allow(clippy::too_many_arguments)]
fn paint_images(
    term: &Terminal<'_, '_>,
    painter: &Painter,
    rect: Rect,
    cell: egui::Vec2,
    layer: Layer,
    iterator: &mut PlacementIterator<'_>,
    textures: &mut HashMap<u32, (u64, TextureHandle)>,
    seen: &mut HashSet<u32>,
) -> Result<()> {
    let graphics = term.kitty_graphics()?;
    let mut placements = iterator.update(&graphics)?;
    placements.set_layer(layer)?;
    let mut images = vec![];
    while let Some(p) = placements.next() {
        let id = p.image_id()?;
        let Some(image) = graphics.image(id) else {
            continue;
        };
        let info = p.placement_render_info(&image, term)?;
        if !info.viewport_visible || info.pixel_width == 0 || info.pixel_height == 0 {
            continue;
        }
        let size = [image.width()? as usize, image.height()? as usize];
        if size.contains(&0) {
            continue;
        }
        ensure!(
            size[0] <= painter.ctx().input(|i| i.max_texture_side)
                && size[1] <= painter.ctx().input(|i| i.max_texture_side),
            "Kitty image exceeds GPU texture size limit"
        );
        seen.insert(id);
        let generation = image.generation()?;
        if textures.get(&id).is_none_or(|(g, _)| *g != generation) {
            let data = image.data()?;
            let pixels = match image.format()? {
                ImageFormat::Rgba => ColorImage::from_rgba_unmultiplied(size, data),
                ImageFormat::Rgb => ColorImage::from_rgb(size, data),
                _ => continue,
            };
            textures.insert(
                id,
                (
                    generation,
                    painter.ctx().load_texture(
                        format!("kitty-{id}"),
                        pixels,
                        TextureOptions::LINEAR,
                    ),
                ),
            );
        }
        let scale = vec2(1.0, 1.0);
        let dest = Rect::from_min_size(
            rect.min
                + vec2(
                    info.viewport_col as f32 * cell.x,
                    info.viewport_row as f32 * cell.y,
                )
                + vec2(
                    p.x_offset()? as f32 * scale.x,
                    p.y_offset()? as f32 * scale.y,
                ),
            vec2(
                info.pixel_width as f32 * scale.x,
                info.pixel_height as f32 * scale.y,
            ),
        );
        let uv = Rect::from_min_max(
            pos2(
                info.source_x as f32 / size[0] as f32,
                info.source_y as f32 / size[1] as f32,
            ),
            pos2(
                (info.source_x + info.source_width) as f32 / size[0] as f32,
                (info.source_y + info.source_height) as f32 / size[1] as f32,
            ),
        );
        images.push((p.z()?, id, p.placement_id()?, dest, uv));
    }
    images.sort_by_key(|(z, id, pid, _, _)| (*z, *id, *pid));
    for (_, id, _, dest, uv) in images {
        painter.image(textures[&id].1.id(), dest, uv, Color32::WHITE);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sgr_styles_use_distinct_cached_faces_without_double_drawing() {
        let ctx = egui::Context::default();
        let config = crate::config::Config {
            font_path: std::env::var_os("SLERM_TEST_FONT").map(Into::into),
            ..Default::default()
        };
        ctx.set_fonts(crate::fonts::load(&config).unwrap());
        let mut term = crate::pane::terminal(&config, &theme::DEFAULT).unwrap();
        term.resize(20, 2, 8, 16).unwrap();
        term.vt_write(b"M\x1b[1mM\x1b[22;3mM\x1b[1mM\x1b[0mM");
        let mut renderer = Renderer::new().unwrap();
        let rect = Rect::from_min_size(egui::Pos2::ZERO, vec2(200.0, 100.0));
        let font = FontId::monospace(16.0);
        frame(&ctx, &mut renderer, &term, rect, &font);
        assert_eq!(renderer.glyphs.len(), 4);
        assert_eq!(
            renderer.row_cache[0].foregrounds.len(),
            if config.font_path.is_some() { 5 } else { 7 }
        );
        for (bold, italic) in [(false, false), (true, false), (false, true), (true, true)] {
            let galley = &renderer.glyphs[&("M".into(), bold, italic)];
            assert_eq!(
                galley.job.sections[0].format.font_id.family,
                crate::fonts::family(bold, italic)
            );
        }
        let before = renderer.glyphs[&("M".into(), true, true)].clone();
        let mut fonts = crate::fonts::load(&config).unwrap();
        fonts
            .families
            .get_mut(&crate::fonts::family(true, true))
            .unwrap()
            .reverse();
        ctx.set_fonts(fonts);
        frame(&ctx, &mut renderer, &term, rect, &font);
        assert!(!Arc::ptr_eq(
            &before,
            &renderer.glyphs[&("M".into(), true, true)]
        ));
    }

    #[test]
    fn icons_fit_ink_bounds_at_different_scales_and_cell_widths() {
        let config = crate::config::Config {
            font_path: std::env::var_os("SLERM_TEST_FONT").map(Into::into),
            ..Default::default()
        };
        let fonts = crate::fonts::load(&config).unwrap();
        for scale in [1.0, 1.5, 2.0] {
            let ctx = egui::Context::default();
            ctx.set_fonts(fonts.clone());
            ctx.set_pixels_per_point(scale);
            let _ = ctx.run(egui::RawInput::default(), |ctx| {
                let painter = ctx.layer_painter(egui::LayerId::background());
                for text in ["\u{e0a0}", "\u{f121}", "\u{f0001}"] {
                    assert!(nerd_icon(text));
                    for width in [4.0, 9.0, 18.0] {
                        let rect = Rect::from_min_size(pos2(23.0, 41.0), vec2(width, 18.0));
                        let galley =
                            layout_glyph(&painter, text.into(), &FontId::monospace(20.0), 18.0);
                        let shape = icon_shape(galley, rect, Color32::WHITE);
                        let bounds = shape.visual_bounding_rect();
                        assert!(
                            rect.expand(0.001).contains_rect(bounds),
                            "{text:?}: {bounds:?} outside {rect:?}"
                        );
                        assert!(bounds.center().distance(rect.center()) < 0.001);
                    }
                }
            });
        }
        assert!(!nerd_icon("M"));
        assert!(!nerd_icon("\u{10eeee}"));
    }

    fn frame(
        ctx: &egui::Context,
        renderer: &mut Renderer,
        term: &Terminal<'static, '_>,
        rect: Rect,
        font: &FontId,
    ) -> Vec<egui::epaint::ClippedShape> {
        ctx.run(egui::RawInput::default(), |ctx| {
            let painter = ctx.layer_painter(egui::LayerId::background());
            renderer
                .paint(
                    term,
                    &painter,
                    &Grid {
                        rect,
                        cell: cell_size(&painter, font),
                        font,
                        theme: &theme::DEFAULT,
                        active: true,
                    },
                )
                .unwrap();
        })
        .shapes
    }

    #[test]
    fn cached_rows_match_fresh_rendering_after_terminal_changes() {
        use libghostty_vt::{
            selection::Selection,
            terminal::{Point, PointCoordinate},
        };

        let ctx = egui::Context::default();
        let mut term =
            crate::pane::terminal(&crate::config::Config::default(), &crate::theme::DEFAULT)
                .unwrap();
        term.resize(20, 5, 8, 16).unwrap();
        term.vt_write("\x1b[31mNOR\u{e0b4}\x1b[0m Android\r\nsecond line".as_bytes());
        let rect = Rect::from_min_size(pos2(12.0, 24.0), vec2(200.0, 120.0));
        let font = FontId::monospace(16.0);
        let mut renderer = Renderer::new().unwrap();
        let initial = frame(&ctx, &mut renderer, &term, rect, &font);
        assert_eq!(renderer.rebuilt_rows, 5);
        assert_eq!(initial, frame(&ctx, &mut renderer, &term, rect, &font));
        assert_eq!(renderer.rebuilt_rows, 5, "idle rows should be reused");

        term.vt_write(b"!");
        frame(&ctx, &mut renderer, &term, rect, &font);
        assert_eq!(
            renderer.rebuilt_rows, 6,
            "only the edited row should rebuild"
        );
        let moved = rect.translate(vec2(31.0, 17.0));
        let actual = frame(&ctx, &mut renderer, &term, moved, &font);
        assert_eq!(renderer.rebuilt_rows, 6, "moving a pane should reuse rows");
        assert_eq!(
            actual,
            frame(&ctx, &mut Renderer::new().unwrap(), &term, moved, &font)
        );

        let a = term
            .grid_ref(Point::Viewport(PointCoordinate { x: 0, y: 0 }))
            .unwrap();
        let b = term
            .grid_ref(Point::Viewport(PointCoordinate { x: 4, y: 1 }))
            .unwrap();
        term.set_selection(Some(&Selection::new(a, b, false)))
            .unwrap();
        let selected = frame(&ctx, &mut renderer, &term, rect, &font);
        assert_ne!(selected, initial);
        assert_eq!(
            selected,
            frame(&ctx, &mut Renderer::new().unwrap(), &term, rect, &font)
        );
        term.set_selection(None).unwrap();
        let cleared = frame(&ctx, &mut renderer, &term, rect, &font);
        assert_ne!(cleared, selected);
        assert_eq!(
            cleared,
            frame(&ctx, &mut Renderer::new().unwrap(), &term, rect, &font)
        );

        for change in [
            "\x1b[2;1H\x1b[1;4;7mchanged\x1b[0m",
            "\r\n1\r\n2\r\n3\r\n4\r\n5\r\n6",
            "\x1b[?1049hother screen",
            "\x1b[?1049l",
            "\x1b]10;#abcdef\x07\x1b]11;#123456\x07",
            "\x1b[2J",
        ] {
            term.vt_write(change.as_bytes());
            assert_eq!(
                frame(&ctx, &mut renderer, &term, rect, &font),
                frame(&ctx, &mut Renderer::new().unwrap(), &term, rect, &font),
                "cached output differs after {change:?}"
            );
        }
        term.resize(12, 3, 8, 16).unwrap();
        assert_eq!(
            frame(&ctx, &mut renderer, &term, rect, &font),
            frame(&ctx, &mut Renderer::new().unwrap(), &term, rect, &font)
        );
        assert_eq!(renderer.row_cache.len(), 3);
    }

    #[test]
    fn cached_glyphs_rebuild_after_font_or_scale_changes() {
        let ctx = egui::Context::default();
        let mut term =
            crate::pane::terminal(&crate::config::Config::default(), &crate::theme::DEFAULT)
                .unwrap();
        term.resize(20, 5, 8, 16).unwrap();
        term.vt_write(b"font cache");
        let rect = Rect::from_min_size(egui::Pos2::ZERO, vec2(200.0, 120.0));
        let font = FontId::monospace(16.0);
        let mut renderer = Renderer::new().unwrap();
        frame(&ctx, &mut renderer, &term, rect, &font);
        let old = renderer.font_marker.clone().unwrap();
        let mut fonts = egui::FontDefinitions::default();
        let name = fonts.families[&egui::FontFamily::Monospace][0].clone();
        fonts
            .font_data
            .insert("reloaded".into(), fonts.font_data[&name].clone());
        fonts
            .families
            .get_mut(&egui::FontFamily::Monospace)
            .unwrap()
            .insert(0, "reloaded".into());
        ctx.set_fonts(fonts);
        frame(&ctx, &mut renderer, &term, rect, &font);
        assert_eq!(renderer.rebuilt_rows, 10);
        assert!(!Arc::ptr_eq(&old, renderer.font_marker.as_ref().unwrap()));

        ctx.set_pixels_per_point(2.0);
        let scaled = frame(&ctx, &mut renderer, &term, rect, &font);
        assert_eq!(renderer.rebuilt_rows, 15);
        assert_eq!(
            scaled,
            frame(&ctx, &mut Renderer::new().unwrap(), &term, rect, &font)
        );
        let larger = FontId::monospace(20.0);
        assert_eq!(
            frame(&ctx, &mut renderer, &term, rect, &larger),
            frame(&ctx, &mut Renderer::new().unwrap(), &term, rect, &larger)
        );
        assert_eq!(renderer.rebuilt_rows, 20);
    }

    #[test]
    #[ignore = "manual CPU rendering benchmark"]
    fn renderer_frame_cost() {
        let config = crate::config::Config {
            font_path: std::env::var_os("SLERM_TEST_FONT").map(Into::into),
            ..Default::default()
        };
        let ctx = egui::Context::default();
        ctx.set_fonts(crate::app::fonts(&config).unwrap());
        let mut term = crate::pane::terminal(&config, &theme::DEFAULT).unwrap();
        term.resize(160, 50, 9, 18).unwrap();
        let screen = (1..=50)
            .map(|row| {
                format!(
                    "\x1b[{row};1H\x1b[{}m{}",
                    31 + row % 7,
                    "NOR Android 0123456789 ".repeat(7)
                )
            })
            .collect::<String>();
        term.vt_write(screen.as_bytes());
        let mut renderer = Renderer::new().unwrap();
        for mode in ["idle", "typing", "full"] {
            let start = std::time::Instant::now();
            for frame in 0..100 {
                match mode {
                    "typing" => term.vt_write(format!("\x1b[25;1H{frame:03}").as_bytes()),
                    "full" => term.vt_write(screen.as_bytes()),
                    _ => {}
                }
                let output = ctx.run(
                    egui::RawInput {
                        screen_rect: Some(Rect::from_min_size(
                            egui::Pos2::ZERO,
                            vec2(1600.0, 1000.0),
                        )),
                        ..Default::default()
                    },
                    |ctx| {
                        renderer
                            .paint(
                                &term,
                                &ctx.layer_painter(egui::LayerId::background()),
                                &Grid {
                                    rect: Rect::from_min_size(
                                        egui::Pos2::ZERO,
                                        vec2(1440.0, 900.0),
                                    ),
                                    cell: vec2(9.0, 18.0),
                                    font: &FontId::monospace(16.0),
                                    theme: &theme::DEFAULT,
                                    active: true,
                                },
                            )
                            .unwrap();
                    },
                );
                std::hint::black_box(ctx.tessellate(output.shapes, output.pixels_per_point));
            }
            eprintln!(
                "{mode}: {:.2} ms/frame",
                start.elapsed().as_secs_f64() * 10.0
            );
        }
    }

    #[test]
    fn powerline_separators_fill_the_cell_height_without_font_padding() {
        let rect = Rect::from_min_size(pos2(10.0, 20.0), vec2(9.0, 18.0));
        for code in 0xe0b0..=0xe0b7 {
            let text = char::from_u32(code).unwrap().to_string();
            let Some(egui::Shape::Path(path)) = powerline_shape(&text, rect, Color32::WHITE) else {
                panic!("missing separator");
            };
            assert_eq!(path.points.first().unwrap().y, rect.top());
            assert_eq!(path.points.last().unwrap().y, rect.bottom());
            assert!(path.points.iter().all(|point| rect.contains(*point)));
            let edge = if matches!(code, 0xe0b2 | 0xe0b3 | 0xe0b6 | 0xe0b7) {
                rect.right()
            } else {
                rect.left()
            };
            assert_eq!(path.points.first().unwrap().x, edge);
            assert_eq!(path.points.last().unwrap().x, edge);
        }
        assert!(powerline_shape("M", rect, Color32::WHITE).is_none());
    }

    #[test]
    fn terminal_cells_match_font_line_metrics() {
        let config = crate::config::Config {
            font_path: std::env::var_os("SLERM_TEST_FONT").map(Into::into),
            ..Default::default()
        };
        for scale in [1.0, 1.5, 2.0] {
            let ctx = egui::Context::default();
            ctx.set_pixels_per_point(scale);
            ctx.set_fonts(crate::app::fonts(&config).unwrap());
            let _ = ctx.run(egui::RawInput::default(), |ctx| {
                let painter = ctx.layer_painter(egui::LayerId::background());
                for size in [12.0, 16.0, 20.0, 24.0] {
                    let font = FontId::monospace(size);
                    let cell = cell_size(&painter, &font);
                    let line_height = ctx.fonts_mut(|fonts| fonts.row_height(&font));
                    assert_eq!(
                        cell.y,
                        line_height.ceil(),
                        "font size {size}, scale {scale}"
                    );
                    let text = painter.layout_no_wrap("NOR Android".into(), font, Color32::WHITE);
                    assert!(
                        (cell.y - text.size().y).abs() <= 1.0,
                        "cell {cell:?} vs text {:?}",
                        text.size()
                    );
                }
            });
        }
    }
}
