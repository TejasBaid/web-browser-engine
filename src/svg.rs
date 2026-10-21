//! A small SVG renderer for icons and logos: `<svg>` with `viewBox` and
//! `preserveAspectRatio`, `<g>`, `<path>` (all commands including arcs), `<rect>`
//! (with rounded corners), `<circle>`, `<ellipse>`, `<line>`, `<polyline>`, `<polygon>`
//! and `<use>`; `transform` (matrix, translate, scale, rotate, skew); fill and stroke
//! colors, opacity, `fill-rule`, `stroke-width`, `currentColor`, presentation
//! attributes and `style=""` declarations.
//!
//! Shapes are flattened to polygons and filled with an anti-aliased scanline
//! rasterizer (exact horizontal coverage, 4 sub-scanlines per pixel). Strokes are
//! filled as unions of segment quads and round joins. Gradients, patterns, text,
//! clipping, masks and filters are not supported.

use crate::css::color::{Color, parse_color};
use crate::css::parser::parse_component_values;
use crate::dom::{Document, Namespace, NodeId};
use crate::image::Image;
use std::rc::Rc;

/// A parsed SVG document fragment ready to render.
pub struct SvgSource {
    /// The document containing the `<svg>` element.
    pub doc: Rc<Document>,
    /// The `<svg>` element.
    pub root: NodeId,
}

impl std::fmt::Debug for SvgSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "SvgSource({:?})", self.root)
    }
}

/// Parses SVG file contents (via the HTML parser's foreign-content rules).
pub fn parse_svg(text: &str) -> Option<SvgSource> {
    let doc = crate::html::parse_document(text);
    let root = doc.descendants(doc.root()).find(|&n| {
        doc.element(n)
            .is_some_and(|e| e.ns == Namespace::Svg && e.name == "svg")
    })?;
    Some(SvgSource {
        doc: Rc::new(doc),
        root,
    })
}

/// Parses an SVG length attribute in px (`12`, `12px`, `1.5em` with 16px em).
pub fn parse_svg_length(s: &str) -> Option<f32> {
    let s = s.trim();
    let num_end = s
        .find(|c: char| !(c.is_ascii_digit() || c == '.' || c == '-' || c == '+' || c == 'e' || c == 'E'))
        .unwrap_or(s.len());
    let v: f32 = s[..num_end].parse().ok()?;
    match &s[num_end..] {
        "" | "px" => Some(v),
        "pt" => Some(v * 96.0 / 72.0),
        "em" => Some(v * 16.0),
        "%" => None,
        _ => None,
    }
}

/// The intrinsic size of an `<svg>` element: `width`/`height` attributes, or the
/// `viewBox` size, or 300x150.
pub fn intrinsic_size(doc: &Document, svg: NodeId) -> (f32, f32) {
    let w = doc.attr(svg, "width").and_then(parse_svg_length);
    let h = doc.attr(svg, "height").and_then(parse_svg_length);
    let vb = doc.attr(svg, "viewBox").and_then(parse_view_box);
    match (w, h, vb) {
        (Some(w), Some(h), _) => (w, h),
        (Some(w), None, Some(v)) if v.2 > 0.0 => (w, w * v.3 / v.2),
        (None, Some(h), Some(v)) if v.3 > 0.0 => (h * v.2 / v.3, h),
        (None, None, Some(v)) => (v.2, v.3),
        (w, h, _) => (w.unwrap_or(300.0), h.unwrap_or(150.0)),
    }
}

fn parse_numbers(s: &str) -> Vec<f32> {
    let mut out = Vec::new();
    let b = s.as_bytes();
    let mut i = 0;
    while i < b.len() {
        let c = b[i];
        if c == b'-' || c == b'+' || c == b'.' || c.is_ascii_digit() {
            let start = i;
            i += 1;
            let mut seen_dot = c == b'.';
            let mut seen_e = false;
            while i < b.len() {
                let d = b[i];
                if d.is_ascii_digit() {
                    i += 1;
                } else if d == b'.' && !seen_dot && !seen_e {
                    seen_dot = true;
                    i += 1;
                } else if (d == b'e' || d == b'E') && !seen_e {
                    seen_e = true;
                    i += 1;
                    if i < b.len() && (b[i] == b'-' || b[i] == b'+') {
                        i += 1;
                    }
                } else {
                    break;
                }
            }
            if let Ok(v) = s[start..i].parse() {
                out.push(v);
            }
        } else {
            i += 1;
        }
    }
    out
}

fn parse_view_box(s: &str) -> Option<(f32, f32, f32, f32)> {
    let v = parse_numbers(s);
    (v.len() == 4 && v[2] > 0.0 && v[3] > 0.0).then(|| (v[0], v[1], v[2], v[3]))
}

/// A 2D affine transform `[a b c d e f]` (x' = a x + c y + e, y' = b x + d y + f).
#[derive(Clone, Copy, Debug, PartialEq)]
struct Transform([f32; 6]);

impl Transform {
    const IDENTITY: Transform = Transform([1.0, 0.0, 0.0, 1.0, 0.0, 0.0]);

    fn then(&self, o: &Transform) -> Transform {
        // self applied first, then o: o * self
        let [a, b, c, d, e, f] = self.0;
        let [a2, b2, c2, d2, e2, f2] = o.0;
        Transform([
            a2 * a + c2 * b,
            b2 * a + d2 * b,
            a2 * c + c2 * d,
            b2 * c + d2 * d,
            a2 * e + c2 * f + e2,
            b2 * e + d2 * f + f2,
        ])
    }

    fn apply(&self, x: f32, y: f32) -> (f32, f32) {
        let [a, b, c, d, e, f] = self.0;
        (a * x + c * y + e, b * x + d * y + f)
    }

    fn scale_factor(&self) -> f32 {
        let [a, b, c, d, _, _] = self.0;
        ((a * d - b * c).abs()).sqrt()
    }
}

fn parse_transform(s: &str) -> Transform {
    let mut t = Transform::IDENTITY;
    let mut rest = s;
    while let Some(open) = rest.find('(') {
        let name = rest[..open].trim().trim_start_matches(',').trim();
        let Some(close) = rest[open..].find(')') else { break };
        let args = parse_numbers(&rest[open + 1..open + close]);
        let m = match (name, args.as_slice()) {
            ("matrix", [a, b, c, d, e, f]) => Transform([*a, *b, *c, *d, *e, *f]),
            ("translate", [x]) => Transform([1.0, 0.0, 0.0, 1.0, *x, 0.0]),
            ("translate", [x, y]) => Transform([1.0, 0.0, 0.0, 1.0, *x, *y]),
            ("scale", [s]) => Transform([*s, 0.0, 0.0, *s, 0.0, 0.0]),
            ("scale", [x, y]) => Transform([*x, 0.0, 0.0, *y, 0.0, 0.0]),
            ("rotate", [a]) => rotate(*a, 0.0, 0.0),
            ("rotate", [a, cx, cy]) => rotate(*a, *cx, *cy),
            ("skewX", [a]) => Transform([1.0, 0.0, a.to_radians().tan(), 1.0, 0.0, 0.0]),
            ("skewY", [a]) => Transform([1.0, a.to_radians().tan(), 0.0, 1.0, 0.0, 0.0]),
            _ => Transform::IDENTITY,
        };
        // Transforms in a list apply right to left to points.
        t = m.then(&t);
        rest = &rest[open + close + 1..];
    }
    t
}

fn rotate(deg: f32, cx: f32, cy: f32) -> Transform {
    let (s, c) = deg.to_radians().sin_cos();
    let r = Transform([c, s, -s, c, 0.0, 0.0]);
    let to = Transform([1.0, 0.0, 0.0, 1.0, -cx, -cy]);
    let back = Transform([1.0, 0.0, 0.0, 1.0, cx, cy]);
    to.then(&r).then(&back)
}

/// Inherited painting properties.
#[derive(Clone, Debug)]
struct Paint {
    fill: Option<Color>,
    stroke: Option<Color>,
    stroke_width: f32,
    fill_opacity: f32,
    stroke_opacity: f32,
    opacity: f32,
    even_odd: bool,
    current_color: Color,
}

fn paint_value(v: &str, current: Color) -> Option<Option<Color>> {
    let v = v.trim();
    if v.eq_ignore_ascii_case("none") || v.starts_with("url(") {
        return Some(None);
    }
    if v.eq_ignore_ascii_case("currentcolor") {
        return Some(Some(current));
    }
    let cv = parse_component_values(v);
    let first = cv.iter().find(|c| !c.is_whitespace())?;
    parse_color(first).map(Some)
}

fn apply_property(p: &mut Paint, name: &str, value: &str) {
    match name {
        "fill" => {
            if let Some(c) = paint_value(value, p.current_color) {
                p.fill = c;
            }
        }
        "stroke" => {
            if let Some(c) = paint_value(value, p.current_color) {
                p.stroke = c;
            }
        }
        "stroke-width" => {
            if let Some(w) = parse_svg_length(value) {
                p.stroke_width = w.max(0.0);
            }
        }
        "fill-opacity" => p.fill_opacity = value.trim().parse().unwrap_or(p.fill_opacity),
        "stroke-opacity" => p.stroke_opacity = value.trim().parse().unwrap_or(p.stroke_opacity),
        "opacity" => p.opacity *= value.trim().parse::<f32>().unwrap_or(1.0).clamp(0.0, 1.0),
        "fill-rule" => p.even_odd = value.trim() == "evenodd",
        "color" => {
            if let Some(Some(c)) = paint_value(value, p.current_color) {
                p.current_color = c;
            }
        }
        _ => {}
    }
}

/// A flattened shape ready to fill.
struct Polygons(Vec<Vec<(f32, f32)>>);

/// Renders a parsed SVG file into a `width` x `height` image.
pub fn render(src: &SvgSource, width: u32, height: u32, current_color: Color) -> Option<Rc<Image>> {
    render_element(&src.doc, src.root, width, height, current_color)
}

/// Renders the `<svg>` element `root` of `doc` into a `width` x `height` image.
pub fn render_element(
    doc: &Document,
    root: NodeId,
    width: u32,
    height: u32,
    current_color: Color,
) -> Option<Rc<Image>> {
    if width == 0 || height == 0 || width * height > 16_000_000 {
        return None;
    }
    let (iw, ih) = intrinsic_size(doc, root);
    let vb = doc
        .attr(root, "viewBox")
        .and_then(parse_view_box)
        .unwrap_or((0.0, 0.0, iw, ih));
    let (sx, sy) = (width as f32 / vb.2, height as f32 / vb.3);
    let par = doc.attr(root, "preserveAspectRatio").unwrap_or("xMidYMid meet");
    let base = if par.trim() == "none" {
        Transform([sx, 0.0, 0.0, sy, -vb.0 * sx, -vb.1 * sy])
    } else {
        let s = if par.contains("slice") { sx.max(sy) } else { sx.min(sy) };
        let (ax, ay) = (
            if par.contains("xMin") {
                0.0
            } else if par.contains("xMax") {
                1.0
            } else {
                0.5
            },
            if par.contains("YMin") {
                0.0
            } else if par.contains("YMax") {
                1.0
            } else {
                0.5
            },
        );
        let tx = (width as f32 - vb.2 * s) * ax - vb.0 * s;
        let ty = (height as f32 - vb.3 * s) * ay - vb.1 * s;
        Transform([s, 0.0, 0.0, s, tx, ty])
    };
    let mut canvas = vec![0f32; (width * height * 4) as usize];
    let paint = Paint {
        fill: Some(Color::BLACK),
        stroke: None,
        stroke_width: 1.0,
        fill_opacity: 1.0,
        stroke_opacity: 1.0,
        opacity: 1.0,
        even_odd: false,
        current_color,
    };
    let mut r = Renderer {
        doc,
        width,
        height,
        canvas: &mut canvas,
        depth: 0,
    };
    r.render_children(root, &base, &paint_with_attrs(doc, root, &paint));
    let pixels: Vec<u8> = canvas
        .iter()
        .map(|v| (v * 255.0 + 0.5).clamp(0.0, 255.0) as u8)
        .collect();
    Some(Rc::new(Image { width, height, pixels }))
}

fn paint_with_attrs(doc: &Document, n: NodeId, parent: &Paint) -> Paint {
    let mut p = parent.clone();
    p.opacity = 1.0;
    let e = doc.element(n).unwrap();
    for a in &e.attrs {
        apply_property(&mut p, &a.name, &a.value);
    }
    if let Some(style) = e.attr("style") {
        for decl in style.split(';') {
            if let Some((k, v)) = decl.split_once(':') {
                apply_property(&mut p, k.trim(), v.trim());
            }
        }
    }
    p
}

struct Renderer<'a> {
    doc: &'a Document,
    width: u32,
    height: u32,
    canvas: &'a mut [f32],
    depth: u32,
}

impl Renderer<'_> {
    fn render_children(&mut self, n: NodeId, t: &Transform, paint: &Paint) {
        if self.depth > 32 {
            return;
        }
        self.depth += 1;
        let children: Vec<NodeId> = self.doc.element_children(n).collect();
        for c in children {
            self.render_node(c, t, paint);
        }
        self.depth -= 1;
    }

    fn render_node(&mut self, n: NodeId, parent_t: &Transform, parent_paint: &Paint) {
        let doc = self.doc;
        let Some(e) = doc.element(n) else { return };
        if e.ns != Namespace::Svg {
            return;
        }
        if e.attr("display").is_some_and(|d| d.trim() == "none")
            || e.attr("style")
                .is_some_and(|s| s.replace(' ', "").contains("display:none"))
        {
            return;
        }
        let paint = paint_with_attrs(doc, n, parent_paint);
        let mut t = *parent_t;
        if let Some(tr) = e.attr("transform") {
            t = parse_transform(tr).then(parent_t);
        }
        let num = |name: &str| e.attr(name).and_then(parse_svg_length).unwrap_or(0.0);
        let shape: Option<Vec<Vec<(f32, f32)>>> = match e.name.as_str() {
            "g" | "a" | "switch" => {
                // Group opacity is approximated by multiplying into children.
                let mut p = paint.clone();
                p.opacity *= parent_paint.opacity;
                self.render_children(n, &t, &p);
                return;
            }
            "svg" => {
                self.render_children(n, &t, &paint);
                return;
            }
            "use" => {
                let href = e
                    .attr("href")
                    .or_else(|| e.attrs.iter().find(|a| a.name == "href").map(|a| a.value.as_str()));
                if let Some(id) = href.and_then(|h| h.strip_prefix('#'))
                    && let Some(target) = doc.element_by_id(id)
                    && self.depth < 8
                {
                    let tt = Transform([1.0, 0.0, 0.0, 1.0, num("x"), num("y")]).then(&t);
                    self.depth += 4;
                    if doc.element(target).is_some_and(|te| te.name == "symbol") {
                        self.render_children(target, &tt, &paint);
                    } else {
                        self.render_node(target, &tt, &paint);
                    }
                    self.depth -= 4;
                }
                return;
            }
            "path" => e.attr("d").map(path_to_polylines),
            "rect" => {
                let (x, y, w, h) = (num("x"), num("y"), num("width"), num("height"));
                if w <= 0.0 || h <= 0.0 {
                    return;
                }
                let mut rx = e.attr("rx").and_then(parse_svg_length);
                let mut ry = e.attr("ry").and_then(parse_svg_length);
                if rx.is_none() {
                    rx = ry;
                }
                if ry.is_none() {
                    ry = rx;
                }
                let (rx, ry) = (rx.unwrap_or(0.0).min(w / 2.0), ry.unwrap_or(0.0).min(h / 2.0));
                if rx > 0.0 && ry > 0.0 {
                    let d = format!(
                        "M{} {}H{}A{rx} {ry} 0 0 1 {} {}V{}A{rx} {ry} 0 0 1 {} {}H{}A{rx} {ry} 0 0 1 {} {}V{}A{rx} {ry} 0 0 1 {} {}Z",
                        x + rx,
                        y,
                        x + w - rx,
                        x + w,
                        y + ry,
                        y + h - ry,
                        x + w - rx,
                        y + h,
                        x + rx,
                        x,
                        y + h - ry,
                        y + ry,
                        x + rx,
                        y
                    );
                    Some(path_to_polylines(&d))
                } else {
                    Some(vec![vec![(x, y), (x + w, y), (x + w, y + h), (x, y + h), (x, y)]])
                }
            }
            "circle" => {
                let r = num("r");
                (r > 0.0).then(|| vec![ellipse_points(num("cx"), num("cy"), r, r)])
            }
            "ellipse" => {
                let (rx, ry) = (num("rx"), num("ry"));
                (rx > 0.0 && ry > 0.0).then(|| vec![ellipse_points(num("cx"), num("cy"), rx, ry)])
            }
            "line" => Some(vec![vec![(num("x1"), num("y1")), (num("x2"), num("y2"))]]),
            "polyline" | "polygon" => e.attr("points").map(|pts| {
                let v = parse_numbers(pts);
                let mut poly: Vec<(f32, f32)> = v.as_chunks::<2>().0.iter().map(|c| (c[0], c[1])).collect();
                if e.name == "polygon" && !poly.is_empty() {
                    poly.push(poly[0]);
                }
                vec![poly]
            }),
            _ => None, // defs, symbol, title, desc, clipPath, mask, linearGradient, text, ...
        };
        let Some(polys) = shape else { return };
        let is_line = matches!(e.name.as_str(), "line" | "polyline");
        let alpha = paint.opacity * parent_paint.opacity;
        let transformed: Vec<Vec<(f32, f32)>> = polys
            .iter()
            .map(|p| p.iter().map(|&(x, y)| t.apply(x, y)).collect())
            .collect();
        if let Some(fill) = paint.fill
            && !is_line
        {
            self.fill(
                &Polygons(transformed.clone()),
                fill,
                paint.fill_opacity * alpha,
                paint.even_odd,
            );
        }
        if let Some(stroke) = paint.stroke
            && paint.stroke_width > 0.0
        {
            let w = paint.stroke_width * t.scale_factor();
            let outline = stroke_polygons(&transformed, w);
            self.fill(&outline, stroke, paint.stroke_opacity * alpha, false);
        }
    }

    /// Fills polygons with anti-aliasing: 4 sub-scanlines per pixel, exact horizontal
    /// span coverage.
    fn fill(&mut self, polys: &Polygons, color: Color, opacity: f32, even_odd: bool) {
        let mut edges: Vec<(f32, f32, f32, f32, i32)> = Vec::new(); // x0,y0,x1,y1,dir
        let (mut min_y, mut max_y) = (f32::INFINITY, f32::NEG_INFINITY);
        for p in &polys.0 {
            if p.len() < 2 {
                continue;
            }
            let n = p.len();
            for i in 0..n {
                let (a, b) = (p[i], p[(i + 1) % n]);
                if (a.1 - b.1).abs() < 1e-6 {
                    continue;
                }
                let dir = if b.1 > a.1 { 1 } else { -1 };
                edges.push((a.0, a.1, b.0, b.1, dir));
                min_y = min_y.min(a.1.min(b.1));
                max_y = max_y.max(a.1.max(b.1));
            }
        }
        if edges.is_empty() {
            return;
        }
        let (w, h) = (self.width as usize, self.height as usize);
        let y0 = (min_y.floor().max(0.0)) as usize;
        let y1 = (max_y.ceil().min(h as f32)) as usize;
        let mut cov = vec![0f32; w + 2];
        const SUB: usize = 4;
        for py in y0..y1 {
            cov.iter_mut().for_each(|c| *c = 0.0);
            let (mut lo, mut hi) = (w, 0usize);
            for s in 0..SUB {
                let sy = py as f32 + (s as f32 + 0.5) / SUB as f32;
                let mut xs: Vec<(f32, i32)> = edges
                    .iter()
                    .filter(|e| (e.1 <= sy && e.3 > sy) || (e.3 <= sy && e.1 > sy))
                    .map(|e| {
                        let t = (sy - e.1) / (e.3 - e.1);
                        (e.0 + t * (e.2 - e.0), e.4)
                    })
                    .collect();
                xs.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
                let mut winding = 0;
                for k in 0..xs.len() {
                    winding += xs[k].1;
                    let inside = if even_odd { (k + 1) % 2 == 1 } else { winding != 0 };
                    if inside && k + 1 < xs.len() {
                        let (a, b) = (xs[k].0.max(0.0), xs[k + 1].0.min(w as f32));
                        if b > a {
                            add_span(&mut cov, a, b, 1.0 / SUB as f32);
                            lo = lo.min(a as usize);
                            hi = hi.max(b.ceil() as usize);
                        }
                    }
                }
            }
            let premul = [color.r as f32 / 255.0, color.g as f32 / 255.0, color.b as f32 / 255.0];
            let ca = color.a as f32 / 255.0 * opacity;
            for (px, &cv) in cov.iter().enumerate().take(hi.min(w)).skip(lo) {
                let c = cv.min(1.0) * ca;
                if c <= 0.0 {
                    continue;
                }
                let i = (py * w + px) * 4;
                let d = &mut self.canvas[i..i + 4];
                let inv = 1.0 - c;
                d[0] = premul[0] * c + d[0] * inv;
                d[1] = premul[1] * c + d[1] * inv;
                d[2] = premul[2] * c + d[2] * inv;
                d[3] = c + d[3] * inv;
            }
        }
    }
}

/// Adds `weight` times the covered length of [a, b) to each pixel.
fn add_span(cov: &mut [f32], a: f32, b: f32, weight: f32) {
    let ia = a.floor() as usize;
    let ib = b.floor() as usize;
    if ia == ib {
        cov[ia] += (b - a) * weight;
        return;
    }
    cov[ia] += (ia as f32 + 1.0 - a) * weight;
    for c in cov.iter_mut().take(ib).skip(ia + 1) {
        *c += weight;
    }
    if ib < cov.len() {
        cov[ib] += (b - ib as f32) * weight;
    }
}

fn ellipse_points(cx: f32, cy: f32, rx: f32, ry: f32) -> Vec<(f32, f32)> {
    let n = ((rx.max(ry) * 2.0) as usize).clamp(16, 128);
    (0..=n)
        .map(|i| {
            let a = i as f32 / n as f32 * std::f32::consts::TAU;
            (cx + rx * a.cos(), cy + ry * a.sin())
        })
        .collect()
}

/// Converts strokes into fillable polygons: one quad per segment plus round joins.
fn stroke_polygons(polys: &[Vec<(f32, f32)>], width: f32) -> Polygons {
    let hw = width / 2.0;
    let mut out = Vec::new();
    for p in polys {
        for seg in p.windows(2) {
            let (a, b) = (seg[0], seg[1]);
            let (dx, dy) = (b.0 - a.0, b.1 - a.1);
            let len = (dx * dx + dy * dy).sqrt();
            if len < 1e-6 {
                continue;
            }
            let (nx, ny) = (-dy / len * hw, dx / len * hw);
            out.push(vec![
                (a.0 + nx, a.1 + ny),
                (b.0 + nx, b.1 + ny),
                (b.0 - nx, b.1 - ny),
                (a.0 - nx, a.1 - ny),
            ]);
        }
        if width > 1.5 {
            for &(x, y) in p.iter() {
                out.push(ellipse_points(x, y, hw, hw));
            }
        }
    }
    Polygons(out)
}

/// Flattens SVG path data into polylines (one per subpath).
pub fn path_to_polylines(d: &str) -> Vec<Vec<(f32, f32)>> {
    let mut out: Vec<Vec<(f32, f32)>> = Vec::new();
    let mut cur: Vec<(f32, f32)> = Vec::new();
    let (mut x, mut y) = (0.0f32, 0.0f32);
    let (mut sx, mut sy) = (0.0f32, 0.0f32);
    let mut last_ctrl: Option<(f32, f32)> = None;
    let mut last_cmd = b' ';
    // Tokenize into commands and numbers.
    let bytes = d.as_bytes();
    let mut i = 0;
    let mut cmd = b' ';
    let mut nums: Vec<f32> = Vec::new();
    let mut tokens: Vec<(u8, Vec<f32>)> = Vec::new();
    while i < bytes.len() {
        let c = bytes[i];
        if c.is_ascii_alphabetic() && c != b'e' && c != b'E' {
            if cmd != b' ' {
                tokens.push((cmd, std::mem::take(&mut nums)));
            }
            cmd = c;
            i += 1;
        } else if c == b'-' || c == b'+' || c == b'.' || c.is_ascii_digit() {
            let start = i;
            i += 1;
            let mut dot = c == b'.';
            while i < bytes.len() {
                let ch = bytes[i];
                if ch.is_ascii_digit() {
                    i += 1;
                } else if ch == b'.' && !dot {
                    dot = true;
                    i += 1;
                } else if ch == b'e' || ch == b'E' {
                    i += 1;
                    if i < bytes.len() && (bytes[i] == b'-' || bytes[i] == b'+') {
                        i += 1;
                    }
                } else {
                    break;
                }
            }
            // Arc flags may be written without separators ("0 01 0").
            let text = &d[start..i];
            if let Ok(v) = text.parse() {
                nums.push(v);
            }
        } else {
            i += 1;
        }
    }
    if cmd != b' ' {
        tokens.push((cmd, nums));
    }
    let flush = |cur: &mut Vec<(f32, f32)>, out: &mut Vec<Vec<(f32, f32)>>| {
        if cur.len() > 1 {
            out.push(std::mem::take(cur));
        } else {
            cur.clear();
        }
    };
    for (cmd, args) in tokens {
        let rel = cmd.is_ascii_lowercase();
        let up = cmd.to_ascii_uppercase();
        let arity = match up {
            b'M' | b'L' | b'T' => 2,
            b'H' | b'V' => 1,
            b'C' => 6,
            b'S' | b'Q' => 4,
            b'A' => 7,
            _ => 0,
        };
        if up == b'Z' {
            if !cur.is_empty() {
                cur.push((sx, sy));
            }
            flush(&mut cur, &mut out);
            x = sx;
            y = sy;
            last_cmd = b'Z';
            continue;
        }
        if arity == 0 || args.is_empty() {
            continue;
        }
        for (k, a) in args.chunks(arity).enumerate() {
            if a.len() < arity {
                break;
            }
            let (ox, oy) = if rel { (x, y) } else { (0.0, 0.0) };
            let op = if up == b'M' && k > 0 { b'L' } else { up };
            match op {
                b'M' => {
                    flush(&mut cur, &mut out);
                    x = a[0] + ox;
                    y = a[1] + oy;
                    sx = x;
                    sy = y;
                    cur.push((x, y));
                    last_ctrl = None;
                }
                b'L' => {
                    x = a[0] + ox;
                    y = a[1] + oy;
                    cur.push((x, y));
                    last_ctrl = None;
                }
                b'H' => {
                    x = a[0] + if rel { x } else { 0.0 };
                    cur.push((x, y));
                    last_ctrl = None;
                }
                b'V' => {
                    y = a[0] + if rel { y } else { 0.0 };
                    cur.push((x, y));
                    last_ctrl = None;
                }
                b'C' | b'S' => {
                    let (c1, c2, end) = if op == b'C' {
                        ((a[0] + ox, a[1] + oy), (a[2] + ox, a[3] + oy), (a[4] + ox, a[5] + oy))
                    } else {
                        let c1 = match last_ctrl {
                            Some((lx, ly)) if matches!(last_cmd, b'C' | b'S') => (2.0 * x - lx, 2.0 * y - ly),
                            _ => (x, y),
                        };
                        (c1, (a[0] + ox, a[1] + oy), (a[2] + ox, a[3] + oy))
                    };
                    if cur.is_empty() {
                        cur.push((x, y));
                    }
                    cubic(&mut cur, (x, y), c1, c2, end);
                    last_ctrl = Some(c2);
                    x = end.0;
                    y = end.1;
                }
                b'Q' | b'T' => {
                    let (c, end) = if op == b'Q' {
                        ((a[0] + ox, a[1] + oy), (a[2] + ox, a[3] + oy))
                    } else {
                        let c = match last_ctrl {
                            Some((lx, ly)) if matches!(last_cmd, b'Q' | b'T') => (2.0 * x - lx, 2.0 * y - ly),
                            _ => (x, y),
                        };
                        (c, (a[0] + ox, a[1] + oy))
                    };
                    if cur.is_empty() {
                        cur.push((x, y));
                    }
                    let c1 = (x + 2.0 / 3.0 * (c.0 - x), y + 2.0 / 3.0 * (c.1 - y));
                    let c2 = (end.0 + 2.0 / 3.0 * (c.0 - end.0), end.1 + 2.0 / 3.0 * (c.1 - end.1));
                    cubic(&mut cur, (x, y), c1, c2, end);
                    last_ctrl = Some(c);
                    x = end.0;
                    y = end.1;
                }
                b'A' => {
                    let end = (a[5] + ox, a[6] + oy);
                    if cur.is_empty() {
                        cur.push((x, y));
                    }
                    arc(&mut cur, (x, y), a[0], a[1], a[2], a[3] != 0.0, a[4] != 0.0, end);
                    x = end.0;
                    y = end.1;
                    last_ctrl = None;
                }
                _ => {}
            }
            last_cmd = op;
        }
    }
    flush(&mut cur, &mut out);
    out
}

fn cubic(out: &mut Vec<(f32, f32)>, p0: (f32, f32), p1: (f32, f32), p2: (f32, f32), p3: (f32, f32)) {
    let len = dist(p0, p1) + dist(p1, p2) + dist(p2, p3);
    let n = (len / 2.0).ceil().clamp(4.0, 64.0) as usize;
    for i in 1..=n {
        let t = i as f32 / n as f32;
        let mt = 1.0 - t;
        let x = mt * mt * mt * p0.0 + 3.0 * mt * mt * t * p1.0 + 3.0 * mt * t * t * p2.0 + t * t * t * p3.0;
        let y = mt * mt * mt * p0.1 + 3.0 * mt * mt * t * p1.1 + 3.0 * mt * t * t * p2.1 + t * t * t * p3.1;
        out.push((x, y));
    }
}

fn dist(a: (f32, f32), b: (f32, f32)) -> f32 {
    ((a.0 - b.0).powi(2) + (a.1 - b.1).powi(2)).sqrt()
}

/// Appends an elliptical arc (SVG endpoint parameterization, F.6.5).
#[allow(clippy::too_many_arguments)]
fn arc(
    out: &mut Vec<(f32, f32)>,
    p0: (f32, f32),
    rx: f32,
    ry: f32,
    rot: f32,
    large: bool,
    sweep: bool,
    p1: (f32, f32),
) {
    let (mut rx, mut ry) = (rx.abs(), ry.abs());
    if rx < 1e-6 || ry < 1e-6 || dist(p0, p1) < 1e-6 {
        out.push(p1);
        return;
    }
    let (s, c) = rot.to_radians().sin_cos();
    let dx = (p0.0 - p1.0) / 2.0;
    let dy = (p0.1 - p1.1) / 2.0;
    let x1 = c * dx + s * dy;
    let y1 = -s * dx + c * dy;
    let lambda = (x1 * x1) / (rx * rx) + (y1 * y1) / (ry * ry);
    if lambda > 1.0 {
        let k = lambda.sqrt();
        rx *= k;
        ry *= k;
    }
    let num = rx * rx * ry * ry - rx * rx * y1 * y1 - ry * ry * x1 * x1;
    let den = rx * rx * y1 * y1 + ry * ry * x1 * x1;
    let mut coef = (num / den).max(0.0).sqrt();
    if large == sweep {
        coef = -coef;
    }
    let cx1 = coef * rx * y1 / ry;
    let cy1 = -coef * ry * x1 / rx;
    let cx = c * cx1 - s * cy1 + (p0.0 + p1.0) / 2.0;
    let cy = s * cx1 + c * cy1 + (p0.1 + p1.1) / 2.0;
    let angle = |ux: f32, uy: f32, vx: f32, vy: f32| {
        let dot = ux * vx + uy * vy;
        let len = (ux * ux + uy * uy).sqrt() * (vx * vx + vy * vy).sqrt();
        let mut a = (dot / len).clamp(-1.0, 1.0).acos();
        if ux * vy - uy * vx < 0.0 {
            a = -a;
        }
        a
    };
    let ux = (x1 - cx1) / rx;
    let uy = (y1 - cy1) / ry;
    let vx = (-x1 - cx1) / rx;
    let vy = (-y1 - cy1) / ry;
    let theta1 = angle(1.0, 0.0, ux, uy);
    let mut delta = angle(ux, uy, vx, vy);
    if !sweep && delta > 0.0 {
        delta -= std::f32::consts::TAU;
    } else if sweep && delta < 0.0 {
        delta += std::f32::consts::TAU;
    }
    let n = ((delta.abs() * rx.max(ry)) / 2.0).ceil().clamp(4.0, 96.0) as usize;
    for i in 1..=n {
        let t = theta1 + delta * i as f32 / n as f32;
        let (st, ct) = t.sin_cos();
        out.push((cx + c * rx * ct - s * ry * st, cy + s * rx * ct + c * ry * st));
    }
}

/// Finds `<svg>` elements in an HTML document (for tests and tools).
pub fn find_svg(doc: &Document) -> Option<NodeId> {
    doc.descendants(doc.root()).find(|&n| {
        doc.element(n)
            .is_some_and(|e| e.ns == Namespace::Svg && e.name == "svg")
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn px(img: &Image, x: u32, y: u32) -> [u8; 4] {
        let i = ((y * img.width + x) * 4) as usize;
        [img.pixels[i], img.pixels[i + 1], img.pixels[i + 2], img.pixels[i + 3]]
    }

    #[test]
    fn renders_shapes_with_viewbox() {
        let src = parse_svg(
            r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 10 10" width="20" height="20">
                 <rect width="5" height="10" fill="red"/>
                 <circle cx="7.5" cy="2.5" r="2" fill="#00f"/>
                 <path d="M5 5 h5 v5 h-5 z" fill="currentColor"/>
               </svg>"##,
        )
        .unwrap();
        assert_eq!(intrinsic_size(&src.doc, src.root), (20.0, 20.0));
        let img = render(&src, 20, 20, Color::rgb(0, 128, 0)).unwrap();
        assert_eq!(px(&img, 2, 10), [255, 0, 0, 255]);
        assert_eq!(px(&img, 15, 5), [0, 0, 255, 255]);
        assert_eq!(px(&img, 15, 15), [0, 128, 0, 255]);
        assert_eq!(px(&img, 15, 9)[3], 0, "between shapes stays transparent");
    }

    #[test]
    fn path_commands_flatten() {
        let p = path_to_polylines("M0 0 L10 0 l0 10 H0 Z M20 20 c5 0 5 5 10 5 a5 5 0 0 1 -5 5");
        assert_eq!(p.len(), 2);
        assert_eq!(
            p[0],
            vec![(0.0, 0.0), (10.0, 0.0), (10.0, 10.0), (0.0, 10.0), (0.0, 0.0)]
        );
        let last = *p[1].last().unwrap();
        assert!((last.0 - 25.0).abs() < 1e-3 && (last.1 - 30.0).abs() < 1e-3, "{last:?}");
    }

    #[test]
    fn transforms_and_strokes() {
        let src = parse_svg(
            r#"<svg width="20" height="20"><g transform="translate(10 0)"><line x1="0" y1="10" x2="10" y2="10" stroke="black" stroke-width="4"/></g></svg>"#,
        )
        .unwrap();
        let img = render(&src, 20, 20, Color::BLACK).unwrap();
        assert_eq!(px(&img, 15, 10)[3], 255);
        assert_eq!(px(&img, 5, 10)[3], 0);
    }
}
