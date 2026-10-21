//! The software rasterizer: executes a display list into an RGBA pixel buffer.
//!
//! Pixels are premultiplied RGBA8 with source-over compositing in sRGB space (like
//! browsers). Axis-aligned box edges are snapped to device pixels as Chrome does;
//! rounded corners, borders and shadows use analytic or supersampled coverage for
//! anti-aliasing. Clips form a stack of rectangles with optional rounded-corner masks;
//! opacity groups render into an offscreen layer that is composited on pop.

use super::display_list::{DisplayItem, DisplayList, Radii};
use crate::css::color::Color;
use crate::image::Image;
use crate::layout::{Edges, Point, Rect};
use crate::style::computed::BorderStyle;
use crate::text::{FontDb, SUBPIXEL_STEPS};

/// An RGBA8 premultiplied pixel buffer.
#[derive(Clone)]
pub struct Canvas {
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
    /// Premultiplied RGBA pixels, row-major.
    pub data: Vec<u8>,
}

impl Canvas {
    /// A transparent canvas.
    pub fn new(width: u32, height: u32) -> Canvas {
        Canvas {
            width,
            height,
            data: vec![0; (width * height * 4) as usize],
        }
    }

    /// Fills the whole canvas with an opaque color.
    pub fn clear(&mut self, c: Color) {
        let p = premul(c, 1.0);
        for px in self.data.as_chunks_mut::<4>().0 {
            px.copy_from_slice(&p);
        }
    }

    /// Converts to straight-alpha RGBA bytes (for PNG output).
    pub fn to_rgba(&self) -> Vec<u8> {
        let mut out = self.data.clone();
        for px in out.as_chunks_mut::<4>().0 {
            let a = px[3] as u32;
            if a != 0 && a != 255 {
                for c in px.iter_mut().take(3) {
                    *c = ((*c as u32 * 255 + a / 2) / a).min(255) as u8;
                }
            }
        }
        out
    }

    /// Writes the canvas as a PNG file.
    pub fn save_png(&self, path: &std::path::Path) -> Result<(), String> {
        image::save_buffer(path, &self.to_rgba(), self.width, self.height, image::ColorType::Rgba8)
            .map_err(|e| e.to_string())
    }

    /// Composites a premultiplied color with coverage `cov` (0..=1) onto pixel (x, y).
    #[inline]
    fn blend(&mut self, x: i32, y: i32, src: [f32; 4], cov: f32) {
        if x < 0 || y < 0 || x >= self.width as i32 || y >= self.height as i32 || cov <= 0.0 {
            return;
        }
        let i = ((y as u32 * self.width + x as u32) * 4) as usize;
        let a = src[3] * cov;
        if a <= 0.0 {
            return;
        }
        let inv = 1.0 - a / 255.0;
        let d = &mut self.data[i..i + 4];
        d[0] = (src[0] * cov + d[0] as f32 * inv + 0.5).min(255.0) as u8;
        d[1] = (src[1] * cov + d[1] as f32 * inv + 0.5).min(255.0) as u8;
        d[2] = (src[2] * cov + d[2] as f32 * inv + 0.5).min(255.0) as u8;
        d[3] = (a + d[3] as f32 * inv + 0.5).min(255.0) as u8;
    }
}

fn premul(c: Color, opacity: f32) -> [u8; 4] {
    let a = c.a as f32 / 255.0 * opacity;
    [
        (c.r as f32 * a + 0.5) as u8,
        (c.g as f32 * a + 0.5) as u8,
        (c.b as f32 * a + 0.5) as u8,
        (a * 255.0 + 0.5) as u8,
    ]
}

fn premul_f(c: Color) -> [f32; 4] {
    let a = c.a as f32 / 255.0;
    [c.r as f32 * a, c.g as f32 * a, c.b as f32 * a, c.a as f32]
}

/// Integer pixel rectangle `[x0, x1) x [y0, y1)`.
#[derive(Clone, Copy, Debug, PartialEq)]
struct IRect {
    x0: i32,
    y0: i32,
    x1: i32,
    y1: i32,
}

impl IRect {
    fn intersect(&self, o: &IRect) -> IRect {
        IRect {
            x0: self.x0.max(o.x0),
            y0: self.y0.max(o.y0),
            x1: self.x1.min(o.x1),
            y1: self.y1.min(o.y1),
        }
    }
    fn is_empty(&self) -> bool {
        self.x1 <= self.x0 || self.y1 <= self.y0
    }
}

/// Snaps a rectangle to device pixels by rounding its edges.
fn snap(r: &Rect) -> IRect {
    IRect {
        x0: r.x.round() as i32,
        y0: r.y.round() as i32,
        x1: r.right().round() as i32,
        y1: r.bottom().round() as i32,
    }
}

struct Clip {
    bounds: IRect,
    /// Rounded clip: (rect, radii) tested per pixel.
    rounded: Option<(Rect, Radii)>,
}

/// Rasterization state.
struct Raster<'a> {
    fonts: &'a FontDb,
    /// Layer stack: (pixels, group opacity, device position of the layer's origin).
    layers: Vec<(Canvas, f32, (i32, i32))>,
    /// Nesting depth of skipped (off-screen) opacity groups.
    skip_depth: usize,
    clips: Vec<Clip>,
    /// Document-space offset applied to non-fixed content (the scroll position).
    scroll: Point,
    fixed_depth: usize,
}

/// Renders a display list into a canvas of `width` x `height` pixels, with the
/// document scrolled by `scroll` (fixed-position content ignores the scroll).
pub fn rasterize(list: &DisplayList, fonts: &FontDb, width: u32, height: u32, scroll: Point) -> Canvas {
    let mut base = Canvas::new(width, height);
    base.clear(list.background);
    let mut r = Raster {
        fonts,
        layers: vec![(base, 1.0, (0, 0))],
        skip_depth: 0,
        clips: vec![Clip {
            bounds: IRect {
                x0: 0,
                y0: 0,
                x1: width as i32,
                y1: height as i32,
            },
            rounded: None,
        }],
        scroll,
        fixed_depth: 0,
    };
    let view = Rect::new(0.0, 0.0, width as f32, height as f32);
    for item in &list.items {
        if r.skip_depth > 0 {
            match item {
                DisplayItem::PushOpacity { .. } => r.skip_depth += 1,
                DisplayItem::PopOpacity => r.skip_depth -= 1,
                _ => {}
            }
            continue;
        }
        r.execute(item, &view);
    }
    while r.layers.len() > 1 {
        r.pop_layer();
    }
    r.layers.pop().unwrap().0
}

impl Raster<'_> {
    fn offset(&self) -> Point {
        if self.fixed_depth > 0 {
            Point::default()
        } else {
            Point::new(-self.scroll.x, -self.scroll.y)
        }
    }

    fn canvas(&mut self) -> &mut Canvas {
        &mut self.layers.last_mut().unwrap().0
    }

    /// Blends into the current layer at device pixel (x, y).
    #[inline]
    fn blend_px(&mut self, x: i32, y: i32, src: [f32; 4], cov: f32) {
        let (c, _, (ox, oy)) = self.layers.last_mut().unwrap();
        c.blend(x - *ox, y - *oy, src, cov);
    }

    fn clip(&self) -> &Clip {
        self.clips.last().unwrap()
    }

    /// Coverage multiplier from the rounded clip stack at a pixel.
    #[inline]
    fn clip_coverage(&self, x: i32, y: i32) -> f32 {
        let mut cov = 1.0;
        for c in self.clips.iter().skip(1) {
            if let Some((rect, radii)) = &c.rounded {
                cov *= rounded_rect_coverage(rect, radii, x as f32 + 0.5, y as f32 + 0.5);
                if cov <= 0.0 {
                    return 0.0;
                }
            }
        }
        cov
    }

    fn has_rounded_clip(&self) -> bool {
        self.clips.iter().any(|c| c.rounded.is_some())
    }

    fn execute(&mut self, item: &DisplayItem, view: &Rect) {
        let off = self.offset();
        match item {
            DisplayItem::Rect { rect, color, radii } => {
                let r = rect.translate(off.x, off.y);
                if !r.intersects(view) {
                    return;
                }
                if radii.iter().any(|c| c.0 > 0.0 && c.1 > 0.0) {
                    self.fill_rounded(&r, radii, *color);
                } else {
                    self.fill_rect(&r, *color);
                }
            }
            DisplayItem::Border {
                rect,
                widths,
                colors,
                styles,
                radii,
            } => {
                let r = rect.translate(off.x, off.y);
                if r.intersects(view) {
                    self.draw_border(&r, widths, colors, styles, radii);
                }
            }
            DisplayItem::BoxShadow {
                rect,
                radii,
                dx,
                dy,
                blur,
                spread,
                color,
            } => {
                let r = rect.translate(off.x, off.y);
                self.draw_box_shadow(&r, radii, *dx, *dy, *blur, *spread, *color, view);
            }
            DisplayItem::Text { origin, run, color } => {
                let o = Point::new(origin.x + off.x, origin.y + off.y);
                if o.y + run.font_size * 2.0 < 0.0 || o.y - run.font_size * 2.0 > view.height {
                    return;
                }
                self.draw_text(o, run, *color);
            }
            DisplayItem::Image { rect, clip, image } => {
                let r = rect.translate(off.x, off.y);
                let c = clip.translate(off.x, off.y);
                if r.intersects(view) {
                    self.draw_image(&r, &c, image);
                }
            }
            DisplayItem::TiledImage {
                area,
                tile,
                repeat_x,
                repeat_y,
                image,
            } => {
                let a = area.translate(off.x, off.y);
                let t = tile.translate(off.x, off.y);
                if a.intersects(view) {
                    self.draw_tiled(&a, &t, *repeat_x, *repeat_y, image);
                }
            }
            DisplayItem::PushClip { rect, radii } => {
                let r = rect.translate(off.x, off.y);
                let bounds = snap(&r).intersect(&self.clip().bounds);
                let rounded = if radii.iter().any(|c| c.0 > 0.0 && c.1 > 0.0) {
                    Some((r, *radii))
                } else {
                    None
                };
                self.clips.push(Clip { bounds, rounded });
            }
            DisplayItem::PopClip => {
                if self.clips.len() > 1 {
                    self.clips.pop();
                }
            }
            DisplayItem::PushOpacity { opacity, bounds } => {
                // Only allocate (and later composite) the visible part of the group.
                let region = snap(&bounds.translate(off.x, off.y)).intersect(&self.clip().bounds);
                if region.is_empty() || *opacity <= 0.0 {
                    self.skip_depth = 1;
                    return;
                }
                let layer = Canvas::new((region.x1 - region.x0) as u32, (region.y1 - region.y0) as u32);
                self.layers.push((layer, *opacity, (region.x0, region.y0)));
                self.clips.push(Clip {
                    bounds: region,
                    rounded: None,
                });
            }
            DisplayItem::PopOpacity => {
                if self.layers.len() > 1 {
                    self.clips.pop();
                    self.pop_layer();
                }
            }
            DisplayItem::PushFixed => self.fixed_depth += 1,
            DisplayItem::PopFixed => self.fixed_depth = self.fixed_depth.saturating_sub(1),
        }
    }

    fn pop_layer(&mut self) {
        let (layer, opacity, (lx, ly)) = self.layers.pop().unwrap();
        let (dst, _, (dx0, dy0)) = self.layers.last_mut().unwrap();
        for y in 0..layer.height as i32 {
            let ty = y + ly - *dy0;
            if ty < 0 || ty >= dst.height as i32 {
                continue;
            }
            for x in 0..layer.width as i32 {
                let tx = x + lx - *dx0;
                if tx < 0 || tx >= dst.width as i32 {
                    continue;
                }
                let si = ((y as u32 * layer.width + x as u32) * 4) as usize;
                let s = &layer.data[si..si + 4];
                if s[3] == 0 {
                    continue;
                }
                let di = ((ty as u32 * dst.width + tx as u32) * 4) as usize;
                let d = &mut dst.data[di..di + 4];
                let sa = s[3] as f32 * opacity;
                let inv = 1.0 - sa / 255.0;
                for k in 0..3 {
                    d[k] = (s[k] as f32 * opacity + d[k] as f32 * inv + 0.5).min(255.0) as u8;
                }
                d[3] = (sa + d[3] as f32 * inv + 0.5).min(255.0) as u8;
            }
        }
    }

    /// Fills a pixel-snapped rectangle.
    fn fill_rect(&mut self, r: &Rect, color: Color) {
        let ir = snap(r).intersect(&self.clip().bounds);
        self.fill_irect(ir, color);
    }

    fn fill_irect(&mut self, ir: IRect, color: Color) {
        if ir.is_empty() || color.a == 0 {
            return;
        }
        let src = premul_f(color);
        let rounded = self.has_rounded_clip();
        if color.a == 255 && !rounded {
            let p = premul(color, 1.0);
            let (c, _, (ox, oy)) = self.layers.last_mut().unwrap();
            let local = IRect {
                x0: (ir.x0 - *ox).max(0),
                y0: (ir.y0 - *oy).max(0),
                x1: (ir.x1 - *ox).min(c.width as i32),
                y1: (ir.y1 - *oy).min(c.height as i32),
            };
            for y in local.y0..local.y1 {
                let row = (y as u32 * c.width) as usize * 4;
                for x in local.x0..local.x1 {
                    let i = row + x as usize * 4;
                    c.data[i..i + 4].copy_from_slice(&p);
                }
            }
            return;
        }
        for y in ir.y0..ir.y1 {
            for x in ir.x0..ir.x1 {
                let cov = if rounded { self.clip_coverage(x, y) } else { 1.0 };
                self.blend_px(x, y, src, cov);
            }
        }
    }

    fn fill_rounded(&mut self, r: &Rect, radii: &Radii, color: Color) {
        let ir = IRect {
            x0: r.x.floor() as i32,
            y0: r.y.floor() as i32,
            x1: r.right().ceil() as i32,
            y1: r.bottom().ceil() as i32,
        }
        .intersect(&self.clip().bounds);
        let src = premul_f(color);
        for y in ir.y0..ir.y1 {
            for x in ir.x0..ir.x1 {
                let cov = rounded_rect_coverage(r, radii, x as f32 + 0.5, y as f32 + 0.5) * self.clip_coverage(x, y);
                self.blend_px(x, y, src, cov);
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn draw_box_shadow(
        &mut self,
        r: &Rect,
        radii: &Radii,
        dx: f32,
        dy: f32,
        blur: f32,
        spread: f32,
        color: Color,
        view: &Rect,
    ) {
        let shadow = Rect::new(
            r.x + dx - spread,
            r.y + dy - spread,
            r.width + 2.0 * spread,
            r.height + 2.0 * spread,
        );
        if shadow.width <= 0.0 || shadow.height <= 0.0 {
            return;
        }
        let sradii: Radii = radii.map(|(h, v)| ((h + spread).max(0.0), (v + spread).max(0.0)));
        let pad = (blur * 1.5).ceil() + 1.0;
        let area = Rect::new(
            shadow.x - pad,
            shadow.y - pad,
            shadow.width + 2.0 * pad,
            shadow.height + 2.0 * pad,
        );
        if !area.intersects(view) {
            return;
        }
        let x0 = area.x.floor() as i32;
        let y0 = area.y.floor() as i32;
        let w = (area.right().ceil() as i32 - x0).max(1) as usize;
        let h = (area.bottom().ceil() as i32 - y0).max(1) as usize;
        if w * h > 16_000_000 {
            return;
        }
        let mut mask = vec![0f32; w * h];
        for yy in 0..h {
            for xx in 0..w {
                let px = (x0 + xx as i32) as f32 + 0.5;
                let py = (y0 + yy as i32) as f32 + 0.5;
                mask[yy * w + xx] = rounded_rect_coverage(&shadow, &sradii, px, py);
            }
        }
        if blur > 0.0 {
            // Gaussian approximated by three box blurs (sigma = blur / 2).
            let sigma = blur / 2.0;
            let box_r = ((sigma * sigma * 4.0 + 1.0).sqrt() / 2.0).round().max(1.0) as usize;
            for _ in 0..3 {
                box_blur(&mut mask, w, h, box_r);
            }
        }
        let src = premul_f(color);
        let clip = self.clip().bounds;
        for yy in 0..h {
            let py = y0 + yy as i32;
            if py < clip.y0 || py >= clip.y1 {
                continue;
            }
            for xx in 0..w {
                let px = x0 + xx as i32;
                if px < clip.x0 || px >= clip.x1 {
                    continue;
                }
                // The shadow is not drawn under the box itself.
                let inside = rounded_rect_coverage(r, radii, px as f32 + 0.5, py as f32 + 0.5);
                let cov = mask[yy * w + xx] * (1.0 - inside) * self.clip_coverage(px, py);
                self.blend_px(px, py, src, cov);
            }
        }
    }

    fn draw_text(&mut self, origin: Point, run: &crate::layout::fragment::TextRun, color: Color) {
        let src = premul_f(color);
        let clip = self.clip().bounds;
        let rounded = self.has_rounded_clip();
        let baseline = origin.y.round() as i32;
        let gamma = text_gamma(color);
        for g in &run.glyphs {
            let pen = origin.x + g.x;
            let mut ix = pen.floor();
            let mut bin = ((pen - ix) * SUBPIXEL_STEPS as f32).round() as u8;
            if bin >= SUBPIXEL_STEPS {
                bin = 0;
                ix += 1.0;
            }
            let raster = self.fonts.rasterize(g.face, g.glyph, run.font_size, bin);
            if raster.width == 0 {
                continue;
            }
            let gx = ix as i32 + raster.left;
            let gy = baseline + (g.y.round() as i32) + raster.top;
            for yy in 0..raster.height as i32 {
                let py = gy + yy;
                if py < clip.y0 || py >= clip.y1 {
                    continue;
                }
                for xx in 0..raster.width as i32 {
                    let px = gx + xx;
                    if px < clip.x0 || px >= clip.x1 {
                        continue;
                    }
                    let c = raster.coverage[(yy as u32 * raster.width + xx as u32) as usize];
                    if c == 0 {
                        continue;
                    }
                    let mut cov = text_coverage(c, gamma);
                    if rounded {
                        cov *= self.clip_coverage(px, py);
                    }
                    self.blend_px(px, py, src, cov);
                }
            }
        }
    }

    fn draw_image(&mut self, dest: &Rect, clip: &Rect, img: &Image) {
        if dest.width <= 0.0 || dest.height <= 0.0 {
            return;
        }
        let ir = snap(dest).intersect(&snap(clip)).intersect(&self.clip().bounds);
        let sx = img.width as f32 / dest.width;
        let sy = img.height as f32 / dest.height;
        let rounded = self.has_rounded_clip();
        // Supersample when minifying to avoid aliasing.
        let ss = (sx.max(sy).ceil() as usize).clamp(1, 4);
        for y in ir.y0..ir.y1 {
            for x in ir.x0..ir.x1 {
                let mut acc = [0f32; 4];
                for j in 0..ss {
                    for i in 0..ss {
                        let fx = x as f32 + (i as f32 + 0.5) / ss as f32;
                        let fy = y as f32 + (j as f32 + 0.5) / ss as f32;
                        let u = (fx - dest.x) * sx;
                        let v = (fy - dest.y) * sy;
                        let s = img.sample_bilinear(u, v);
                        for k in 0..4 {
                            acc[k] += s[k];
                        }
                    }
                }
                let n = (ss * ss) as f32;
                let src = [acc[0] / n, acc[1] / n, acc[2] / n, acc[3] / n];
                let cov = if rounded { self.clip_coverage(x, y) } else { 1.0 };
                self.blend_px(x, y, src, cov);
            }
        }
    }

    fn draw_tiled(&mut self, area: &Rect, tile: &Rect, rx: bool, ry: bool, img: &Image) {
        let ir = snap(area).intersect(&self.clip().bounds);
        if ir.is_empty() {
            return;
        }
        let sx = img.width as f32 / tile.width;
        let sy = img.height as f32 / tile.height;
        let rounded = self.has_rounded_clip();
        for y in ir.y0..ir.y1 {
            let mut ty = y as f32 + 0.5 - tile.y;
            if ry {
                ty = ty.rem_euclid(tile.height);
            } else if ty < 0.0 || ty >= tile.height {
                continue;
            }
            for x in ir.x0..ir.x1 {
                let mut tx = x as f32 + 0.5 - tile.x;
                if rx {
                    tx = tx.rem_euclid(tile.width);
                } else if tx < 0.0 || tx >= tile.width {
                    continue;
                }
                let s = img.sample_bilinear(tx * sx, ty * sy);
                let cov = if rounded { self.clip_coverage(x, y) } else { 1.0 };
                self.blend_px(x, y, s, cov);
            }
        }
    }

    fn draw_border(&mut self, r: &Rect, w: &Edges, colors: &[Color; 4], styles: &[BorderStyle; 4], radii: &Radii) {
        let rounded = radii.iter().any(|c| c.0 > 0.0 && c.1 > 0.0);
        let shaded = shade_colors(colors, styles);
        let simple = styles.iter().zip([w.top, w.right, w.bottom, w.left]).all(|(s, wd)| {
            wd == 0.0
                || matches!(
                    s,
                    BorderStyle::Solid
                        | BorderStyle::Inset
                        | BorderStyle::Outset
                        | BorderStyle::Groove
                        | BorderStyle::Ridge
                )
        });
        if !rounded && simple {
            self.draw_border_rects(r, w, &shaded, styles);
            return;
        }
        if !rounded {
            // Dashed / dotted / double on straight sides.
            self.draw_patterned_border(r, w, &shaded, styles);
            return;
        }
        self.draw_rounded_border(r, w, &shaded, radii);
    }

    /// Solid borders without radii: each side is a trapezoid; corners are split along
    /// the diagonal when adjacent colors differ.
    fn draw_border_rects(&mut self, r: &Rect, w: &Edges, colors: &[Color; 4], styles: &[BorderStyle; 4]) {
        let outer = snap(r);
        let inner = IRect {
            x0: (r.x + w.left).round() as i32,
            y0: (r.y + w.top).round() as i32,
            x1: (r.right() - w.right).round() as i32,
            y1: (r.bottom() - w.bottom).round() as i32,
        };
        let same = colors
            .iter()
            .zip(styles)
            .zip([w.top, w.right, w.bottom, w.left])
            .filter(|(_, wd)| *wd > 0.0)
            .map(|((c, _), _)| *c)
            .collect::<Vec<_>>();
        let uniform = same.windows(2).all(|p| p[0] == p[1]);
        let clip = self.clip().bounds;
        if uniform && !same.is_empty() {
            let c = same[0];
            let top = IRect {
                x0: outer.x0,
                y0: outer.y0,
                x1: outer.x1,
                y1: inner.y0.max(outer.y0),
            };
            let bottom = IRect {
                x0: outer.x0,
                y0: inner.y1.min(outer.y1),
                x1: outer.x1,
                y1: outer.y1,
            };
            let left = IRect {
                x0: outer.x0,
                y0: inner.y0,
                x1: inner.x0,
                y1: inner.y1,
            };
            let right = IRect {
                x0: inner.x1,
                y0: inner.y0,
                x1: outer.x1,
                y1: inner.y1,
            };
            for part in [top, bottom, left, right] {
                if w.top == 0.0 && part == top || w.bottom == 0.0 && part == bottom {
                    continue;
                }
                self.fill_irect(part.intersect(&clip), c);
            }
            return;
        }
        // Per-pixel side assignment with diagonal corner joins.
        let ow = (outer.x1 - outer.x0) as f32;
        let oh = (outer.y1 - outer.y0) as f32;
        let bounds = outer.intersect(&clip);
        let srcs = colors.map(premul_f);
        let rounded = self.has_rounded_clip();
        for y in bounds.y0..bounds.y1 {
            for x in bounds.x0..bounds.x1 {
                if x >= inner.x0 && x < inner.x1 && y >= inner.y0 && y < inner.y1 {
                    continue;
                }
                let fx = x as f32 + 0.5 - outer.x0 as f32;
                let fy = y as f32 + 0.5 - outer.y0 as f32;
                let side = corner_side(fx, fy, ow, oh, w);
                if [w.top, w.right, w.bottom, w.left][side] <= 0.0 {
                    continue;
                }
                let cov = if rounded { self.clip_coverage(x, y) } else { 1.0 };
                self.blend_px(x, y, srcs[side], cov);
            }
        }
    }

    fn draw_patterned_border(&mut self, r: &Rect, w: &Edges, colors: &[Color; 4], styles: &[BorderStyle; 4]) {
        let widths = [w.top, w.right, w.bottom, w.left];
        for side in 0..4 {
            let wd = widths[side];
            if wd <= 0.0 {
                continue;
            }
            let color = colors[side];
            let horizontal = side % 2 == 0;
            let (len, x0, y0) = match side {
                0 => (r.width, r.x, r.y),
                1 => (r.height, r.right() - wd, r.y),
                2 => (r.width, r.x, r.bottom() - wd),
                _ => (r.height, r.x, r.y),
            };
            match styles[side] {
                BorderStyle::Dashed | BorderStyle::Dotted => {
                    let dotted = styles[side] == BorderStyle::Dotted;
                    let dash = if dotted { wd } else { (wd * 3.0).max(3.0) };
                    let gap = dash;
                    let n = ((len + gap) / (dash + gap)).floor().max(1.0);
                    let step = if n > 1.0 { (len - dash) / (n - 1.0) } else { 0.0 };
                    for i in 0..n as i32 {
                        let p = i as f32 * step;
                        let seg = if horizontal {
                            Rect::new(x0 + p, y0, dash, wd)
                        } else {
                            Rect::new(x0, y0 + p, wd, dash)
                        };
                        if dotted && wd >= 2.0 {
                            let rr = wd / 2.0;
                            self.fill_rounded(&seg, &[(rr, rr); 4], color);
                        } else {
                            self.fill_rect(&seg, color);
                        }
                    }
                }
                BorderStyle::Double => {
                    let t = (wd / 3.0).round().max(1.0);
                    let (a, b) = if horizontal {
                        (Rect::new(x0, y0, len, t), Rect::new(x0, y0 + wd - t, len, t))
                    } else {
                        (Rect::new(x0, y0, t, len), Rect::new(x0 + wd - t, y0, t, len))
                    };
                    self.fill_rect(&a, color);
                    self.fill_rect(&b, color);
                }
                _ => {
                    let seg = if horizontal {
                        Rect::new(x0, y0, len, wd)
                    } else {
                        Rect::new(x0, y0 + w.top, wd, len - w.top - w.bottom)
                    };
                    self.fill_rect(&seg, color);
                }
            }
        }
    }

    /// Borders with rounded corners: 4x4 supersampled ring coverage, with each sample
    /// assigned to a side by the corner diagonals.
    fn draw_rounded_border(&mut self, r: &Rect, w: &Edges, colors: &[Color; 4], radii: &Radii) {
        let inner = Rect::new(
            r.x + w.left,
            r.y + w.top,
            (r.width - w.horizontal()).max(0.0),
            (r.height - w.vertical()).max(0.0),
        );
        let inner_radii: Radii = [
            ((radii[0].0 - w.left).max(0.0), (radii[0].1 - w.top).max(0.0)),
            ((radii[1].0 - w.right).max(0.0), (radii[1].1 - w.top).max(0.0)),
            ((radii[2].0 - w.right).max(0.0), (radii[2].1 - w.bottom).max(0.0)),
            ((radii[3].0 - w.left).max(0.0), (radii[3].1 - w.bottom).max(0.0)),
        ];
        let ir = IRect {
            x0: r.x.floor() as i32,
            y0: r.y.floor() as i32,
            x1: r.right().ceil() as i32,
            y1: r.bottom().ceil() as i32,
        }
        .intersect(&self.clip().bounds);
        let srcs = colors.map(premul_f);
        let widths = [w.top, w.right, w.bottom, w.left];
        let uniform = colors.iter().all(|c| *c == colors[0]);
        const N: usize = 4;
        for y in ir.y0..ir.y1 {
            for x in ir.x0..ir.x1 {
                // Fast reject: pixel fully inside the inner box away from corners.
                let cx = x as f32 + 0.5;
                let cy = y as f32 + 0.5;
                if cx > inner.x + 1.0 + inner_radii[0].0.max(inner_radii[3].0)
                    && cx < inner.right() - 1.0 - inner_radii[1].0.max(inner_radii[2].0)
                    && cy > inner.y + 1.0
                    && cy < inner.bottom() - 1.0
                {
                    continue;
                }
                let mut counts = [0u32; 4];
                let mut total = 0u32;
                for sj in 0..N {
                    for si in 0..N {
                        let px = x as f32 + (si as f32 + 0.5) / N as f32;
                        let py = y as f32 + (sj as f32 + 0.5) / N as f32;
                        if !inside_rounded(r, radii, px, py) || inside_rounded(&inner, &inner_radii, px, py) {
                            continue;
                        }
                        total += 1;
                        if !uniform {
                            let side = corner_side(px - r.x, py - r.y, r.width, r.height, w);
                            counts[side] += 1;
                        }
                    }
                }
                if total == 0 {
                    continue;
                }
                let clipc = self.clip_coverage(x, y);
                if uniform {
                    if widths.iter().any(|&wd| wd > 0.0) {
                        self.canvas()
                            .blend(x, y, srcs[0], total as f32 / (N * N) as f32 * clipc);
                    }
                } else {
                    for side in 0..4 {
                        if counts[side] > 0 && widths[side] > 0.0 {
                            self.canvas()
                                .blend(x, y, srcs[side], counts[side] as f32 / (N * N) as f32 * clipc);
                        }
                    }
                }
            }
        }
    }
}

/// Which side (0 top, 1 right, 2 bottom, 3 left) a point in the border area belongs to,
/// splitting corners along the line from the outer corner to the inner corner.
fn corner_side(fx: f32, fy: f32, ow: f32, oh: f32, w: &Edges) -> usize {
    let in_top = fy < w.top;
    let in_bottom = fy >= oh - w.bottom;
    let in_left = fx < w.left;
    let in_right = fx >= ow - w.right;
    match (in_top, in_bottom, in_left, in_right) {
        (true, _, true, _) => {
            // top-left corner: compare fy/top vs fx/left
            if w.left == 0.0 || (w.top > 0.0 && fy * w.left < fx * w.top) {
                0
            } else {
                3
            }
        }
        (true, _, _, true) => {
            let dx = ow - fx;
            if w.right == 0.0 || (w.top > 0.0 && fy * w.right < dx * w.top) {
                0
            } else {
                1
            }
        }
        (_, true, true, _) => {
            let dy = oh - fy;
            if w.left == 0.0 || (w.bottom > 0.0 && dy * w.left < fx * w.bottom) {
                2
            } else {
                3
            }
        }
        (_, true, _, true) => {
            let dx = ow - fx;
            let dy = oh - fy;
            if w.right == 0.0 || (w.bottom > 0.0 && dy * w.right < dx * w.bottom) {
                2
            } else {
                1
            }
        }
        (true, _, _, _) => 0,
        (_, true, _, _) => 2,
        (_, _, true, _) => 3,
        _ => 1,
    }
}

/// Blink's `Color::Dark()`.
fn dark(c: Color) -> Color {
    if c.r == 255 && c.g == 255 && c.b == 255 {
        return Color::rgba(0xab, 0xab, 0xab, c.a);
    }
    let (r, g, b) = (c.r as f32 / 255.0, c.g as f32 / 255.0, c.b as f32 / 255.0);
    let v = r.max(g).max(b);
    let m = if v > 0.0 { ((v - 0.33) / v).max(0.0) } else { 0.0 };
    Color::rgba((r * m * 255.0) as u8, (g * m * 255.0) as u8, (b * m * 255.0) as u8, c.a)
}

/// Colors for 3D border styles (inset/outset/groove/ridge darken some sides).
fn shade_colors(colors: &[Color; 4], styles: &[BorderStyle; 4]) -> [Color; 4] {
    let mut out = *colors;
    for side in 0..4 {
        let top_left = side == 0 || side == 3;
        let darken = match styles[side] {
            BorderStyle::Inset | BorderStyle::Groove => top_left,
            BorderStyle::Outset | BorderStyle::Ridge => !top_left,
            _ => false,
        };
        if darken {
            out[side] = dark(colors[side]);
        }
    }
    out
}

/// 8-bit glyph coverage to blend coverage, with a slight contrast boost similar to
/// Skia's text gamma on macOS.
#[inline]
fn text_coverage(c: u8, gamma: f32) -> f32 {
    let v = c as f32 / 255.0;
    v.powf(gamma)
}

/// Coverage exponent for a text color: light text on dark backgrounds gets a stronger
/// boost, approximating Skia's luminance-dependent contrast on macOS.
fn text_gamma(color: Color) -> f32 {
    let luma = (0.299 * color.r as f32 + 0.587 * color.g as f32 + 0.114 * color.b as f32) / 255.0;
    // Exponents fitted against Chrome screenshots of the reftest gallery.
    const DARK: f32 = 0.8;
    const LIGHT: f32 = 0.3;
    DARK + (LIGHT - DARK) * luma
}

fn inside_rounded(r: &Rect, radii: &Radii, px: f32, py: f32) -> bool {
    if px < r.x || px >= r.right() || py < r.y || py >= r.bottom() {
        return false;
    }
    let corners = [
        (
            r.x + radii[0].0,
            r.y + radii[0].1,
            radii[0],
            px < r.x + radii[0].0 && py < r.y + radii[0].1,
        ),
        (
            r.right() - radii[1].0,
            r.y + radii[1].1,
            radii[1],
            px > r.right() - radii[1].0 && py < r.y + radii[1].1,
        ),
        (
            r.right() - radii[2].0,
            r.bottom() - radii[2].1,
            radii[2],
            px > r.right() - radii[2].0 && py > r.bottom() - radii[2].1,
        ),
        (
            r.x + radii[3].0,
            r.bottom() - radii[3].1,
            radii[3],
            px < r.x + radii[3].0 && py > r.bottom() - radii[3].1,
        ),
    ];
    for (cx, cy, (rx, ry), within) in corners {
        if within && rx > 0.0 && ry > 0.0 {
            let dx = (px - cx) / rx;
            let dy = (py - cy) / ry;
            if dx * dx + dy * dy > 1.0 {
                return false;
            }
        }
    }
    true
}

/// Approximate coverage of a pixel centred at (px, py) by a rounded rectangle.
fn rounded_rect_coverage(r: &Rect, radii: &Radii, px: f32, py: f32) -> f32 {
    // Edge coverage (pixel is 1x1 centred at px, py).
    let cov_x = ((px + 0.5).min(r.right()) - (px - 0.5).max(r.x)).clamp(0.0, 1.0);
    let cov_y = ((py + 0.5).min(r.bottom()) - (py - 0.5).max(r.y)).clamp(0.0, 1.0);
    let mut cov = cov_x * cov_y;
    if cov <= 0.0 {
        return 0.0;
    }
    let corners = [
        (
            r.x + radii[0].0,
            r.y + radii[0].1,
            radii[0],
            px < r.x + radii[0].0 && py < r.y + radii[0].1,
        ),
        (
            r.right() - radii[1].0,
            r.y + radii[1].1,
            radii[1],
            px > r.right() - radii[1].0 && py < r.y + radii[1].1,
        ),
        (
            r.right() - radii[2].0,
            r.bottom() - radii[2].1,
            radii[2],
            px > r.right() - radii[2].0 && py > r.bottom() - radii[2].1,
        ),
        (
            r.x + radii[3].0,
            r.bottom() - radii[3].1,
            radii[3],
            px < r.x + radii[3].0 && py > r.bottom() - radii[3].1,
        ),
    ];
    for (cx, cy, (rx, ry), within) in corners {
        if within && rx > 0.0 && ry > 0.0 {
            let dx = (px - cx) / rx;
            let dy = (py - cy) / ry;
            let d = (dx * dx + dy * dy).sqrt();
            // Signed distance in px, approximated for ellipses.
            let dist = (d - 1.0) * rx.min(ry);
            cov *= (0.5 - dist).clamp(0.0, 1.0);
        }
    }
    cov
}

#[allow(clippy::needless_range_loop)]
fn box_blur(mask: &mut [f32], w: usize, h: usize, r: usize) {
    let mut tmp = vec![0f32; mask.len()];
    let norm = 1.0 / (2 * r + 1) as f32;
    for y in 0..h {
        let row = &mask[y * w..(y + 1) * w];
        let mut acc = 0.0;
        for i in 0..=r.min(w - 1) {
            acc += row[i];
        }
        for x in 0..w {
            tmp[y * w + x] = acc * norm;
            let add = x + r + 1;
            if add < w {
                acc += row[add];
            }
            if x >= r {
                acc -= row[x - r];
            }
        }
    }
    for x in 0..w {
        let mut acc = 0.0;
        for i in 0..=r.min(h - 1) {
            acc += tmp[i * w + x];
        }
        for y in 0..h {
            mask[y * w + x] = acc * norm;
            let add = y + r + 1;
            if add < h {
                acc += tmp[add * w + x];
            }
            if y >= r {
                acc -= tmp[(y - r) * w + x];
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn px(c: &Canvas, x: u32, y: u32) -> [u8; 4] {
        let i = ((y * c.width + x) * 4) as usize;
        [c.data[i], c.data[i + 1], c.data[i + 2], c.data[i + 3]]
    }

    fn list(items: Vec<DisplayItem>) -> DisplayList {
        DisplayList {
            items,
            background: Color::WHITE,
            ..Default::default()
        }
    }

    #[test]
    fn fills_snapped_rects_and_blends_alpha() {
        let fonts = FontDb::new();
        let l = list(vec![
            DisplayItem::Rect {
                rect: Rect::new(1.4, 1.6, 2.0, 2.0),
                color: Color::rgb(255, 0, 0),
                radii: [(0.0, 0.0); 4],
            },
            DisplayItem::Rect {
                rect: Rect::new(0.0, 0.0, 10.0, 10.0),
                color: Color::rgba(0, 0, 255, 128),
                radii: [(0.0, 0.0); 4],
            },
        ]);
        let c = rasterize(&l, &fonts, 10, 10, Point::default());
        // (1.4,1.6)-(3.4,3.6) snaps to [1,3)x[2,4): red under half-transparent blue.
        assert_eq!(px(&c, 1, 2), [127, 0, 128, 255]);
        assert_eq!(px(&c, 1, 1), [127, 127, 255, 255]);
    }

    #[test]
    fn clips_and_opacity_groups() {
        let fonts = FontDb::new();
        let l = list(vec![
            DisplayItem::PushClip {
                rect: Rect::new(0.0, 0.0, 5.0, 10.0),
                radii: [(0.0, 0.0); 4],
            },
            DisplayItem::PushOpacity {
                opacity: 0.5,
                bounds: Rect::new(0.0, 0.0, 10.0, 10.0),
            },
            DisplayItem::Rect {
                rect: Rect::new(0.0, 0.0, 10.0, 10.0),
                color: Color::BLACK,
                radii: [(0.0, 0.0); 4],
            },
            DisplayItem::PopOpacity,
            DisplayItem::PopClip,
        ]);
        let c = rasterize(&l, &fonts, 10, 10, Point::default());
        assert_eq!(px(&c, 2, 2), [128, 128, 128, 255]);
        assert_eq!(px(&c, 7, 2), [255, 255, 255, 255]);
    }

    #[test]
    fn rounded_corners_are_antialiased() {
        let fonts = FontDb::new();
        let l = list(vec![DisplayItem::Rect {
            rect: Rect::new(0.0, 0.0, 20.0, 20.0),
            color: Color::BLACK,
            radii: [(10.0, 10.0); 4],
        }]);
        let c = rasterize(&l, &fonts, 20, 20, Point::default());
        assert_eq!(px(&c, 0, 0), [255, 255, 255, 255], "corner outside the arc");
        assert_eq!(px(&c, 10, 10), [0, 0, 0, 255]);
        let edge = px(&c, 1, 4)[0];
        assert!(edge > 0 && edge < 255, "partial coverage on the arc: {edge}");
    }

    #[test]
    fn border_sides_have_their_own_colors() {
        let fonts = FontDb::new();
        let l = list(vec![DisplayItem::Border {
            rect: Rect::new(0.0, 0.0, 20.0, 20.0),
            widths: Edges::all(4.0),
            colors: [
                Color::rgb(255, 0, 0),
                Color::rgb(0, 255, 0),
                Color::rgb(0, 0, 255),
                Color::BLACK,
            ],
            styles: [BorderStyle::Solid; 4],
            radii: [(0.0, 0.0); 4],
        }]);
        let c = rasterize(&l, &fonts, 20, 20, Point::default());
        assert_eq!(px(&c, 10, 1), [255, 0, 0, 255]);
        assert_eq!(px(&c, 18, 10), [0, 255, 0, 255]);
        assert_eq!(px(&c, 10, 18), [0, 0, 255, 255]);
        assert_eq!(px(&c, 1, 10), [0, 0, 0, 255]);
        assert_eq!(px(&c, 10, 10), [255, 255, 255, 255]);
    }

    #[test]
    fn scroll_offsets_non_fixed_content_only() {
        let fonts = FontDb::new();
        let l = list(vec![
            DisplayItem::Rect {
                rect: Rect::new(0.0, 10.0, 5.0, 5.0),
                color: Color::BLACK,
                radii: [(0.0, 0.0); 4],
            },
            DisplayItem::PushFixed,
            DisplayItem::Rect {
                rect: Rect::new(5.0, 0.0, 5.0, 5.0),
                color: Color::rgb(255, 0, 0),
                radii: [(0.0, 0.0); 4],
            },
            DisplayItem::PopFixed,
        ]);
        let c = rasterize(&l, &fonts, 10, 10, Point::new(0.0, 10.0));
        assert_eq!(px(&c, 1, 1), [0, 0, 0, 255]);
        assert_eq!(px(&c, 6, 1), [255, 0, 0, 255]);
    }
}
