use anyhow::{Result, ensure};
use egui::{
    Color32, ColorImage, FontId, Painter, Rect, Stroke, TextureHandle, TextureOptions, pos2, vec2,
};
use libghostty_vt::{
    RenderState, Terminal,
    alloc::{Allocator, Bytes},
    kitty::graphics::{DecodePng, DecodedImage, ImageFormat, Layer, PlacementIterator},
    render::{CellIterator, CursorVisualStyle, Dirty, RowIterator},
    style::{RgbColor, Underline},
};
use std::{
    collections::{HashMap, HashSet},
    io::Cursor,
};

pub fn color(c: RgbColor) -> Color32 {
    Color32::from_rgb(c.r, c.g, c.b)
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

pub struct Renderer {
    state: RenderState<'static>,
    rows: RowIterator<'static>,
    cells: CellIterator<'static>,
    placements: PlacementIterator<'static>,
    textures: HashMap<u32, (u64, TextureHandle)>,
}

impl Renderer {
    pub fn new() -> Result<Self> {
        Ok(Self {
            state: RenderState::new()?,
            rows: RowIterator::new()?,
            cells: CellIterator::new()?,
            placements: PlacementIterator::new()?,
            textures: HashMap::new(),
        })
    }

    pub fn paint(
        &mut self,
        term: &Terminal<'static, '_>,
        painter: &Painter,
        rect: Rect,
        cell: egui::Vec2,
        font: &FontId,
        active: bool,
    ) -> Result<()> {
        let painter = painter.with_clip_rect(rect.intersect(painter.clip_rect()));
        let snapshot = self.state.update(term)?;
        let colors = snapshot.colors()?;
        painter.rect_filled(rect, 0, color(colors.background));
        let mut backgrounds = vec![];
        let mut foregrounds = vec![];
        let mut rows = self.rows.update(&snapshot)?;
        let mut text = String::new();
        let mut y = 0;
        while let Some(row) = rows.next() {
            let mut cells = self.cells.update(row)?;
            let mut x = 0;
            while let Some(c) = cells.next() {
                let pos = rect.min + vec2(x as f32 * cell.x, y as f32 * cell.y);
                let cell_rect = Rect::from_min_size(pos, cell);
                let style = c.style()?;
                let mut fg = color(c.fg_color()?.unwrap_or(colors.foreground));
                let explicit_bg = c.bg_color()?;
                let mut bg = color(explicit_bg.unwrap_or(colors.background));
                let selected = c.is_selected()?;
                if style.inverse ^ selected {
                    std::mem::swap(&mut fg, &mut bg);
                }
                if explicit_bg.is_some() || style.inverse || selected {
                    backgrounds.push(egui::Shape::rect_filled(cell_rect, 0, bg));
                }
                if style.faint {
                    fg = fg.gamma_multiply(0.6);
                }
                if !style.invisible && c.graphemes_len()? > 0 {
                    c.graphemes_utf8(&mut text)?;
                    // Placeholder cells are reserved by the Kitty protocol, not printable text.
                    if !text.starts_with('\u{10eeee}') {
                        let galley = painter.layout_no_wrap(text.clone(), font.clone(), fg);
                        foregrounds.push(egui::Shape::galley(pos, galley.clone(), fg));
                        if style.bold {
                            foregrounds.push(egui::Shape::galley(
                                pos + vec2(0.45, 0.0),
                                galley,
                                fg,
                            ));
                        }
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
                            Stroke::new(1.0, fg),
                        ));
                    }
                }
                x += 1;
            }
            row.set_dirty(false)?;
            y += 1;
        }
        // Separate passes preserve Kitty's three z-order bands exactly.
        let mut seen = HashSet::new();
        for layer in [Layer::BelowBg, Layer::BelowText, Layer::AboveText] {
            if layer == Layer::BelowText {
                painter.extend(std::mem::take(&mut backgrounds));
            }
            if layer == Layer::AboveText {
                painter.extend(std::mem::take(&mut foregrounds));
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
                        Stroke::new(if active { 1.5 } else { 0.75 }, c),
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
