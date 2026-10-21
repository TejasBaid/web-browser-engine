//! CSS gradients (`linear-gradient()`, `radial-gradient()` and their `repeating-`
//! variants): parsing into a computed form and rendering into an image tile.
//!
//! Colors are interpolated in premultiplied sRGB, as CSS Images Level 3 specifies.

use super::color::{Color, parse_color};
use super::parser::ComponentValue;
use super::tokenizer::Token;
use super::values::{LengthContext, LengthPercentage, parse_length};
use crate::image::Image;
use std::rc::Rc;

/// Size keyword of a radial gradient's ending shape.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum RadialExtent {
    /// `closest-side`
    ClosestSide,
    /// `farthest-side`
    FarthestSide,
    /// `closest-corner`
    ClosestCorner,
    /// `farthest-corner` (default)
    FarthestCorner,
}

/// The geometry of a gradient.
#[derive(Clone, Debug, PartialEq)]
pub enum GradientKind {
    /// A linear gradient at an angle (degrees, 0 = towards the top, clockwise).
    Linear {
        /// Angle in degrees.
        angle: f32,
    },
    /// A radial gradient.
    Radial {
        /// `circle` (true) or `ellipse`.
        circle: bool,
        /// Explicit radii (horizontal, vertical); `None` uses `extent`.
        radii: Option<(LengthPercentage, LengthPercentage)>,
        /// Size keyword.
        extent: RadialExtent,
        /// Center position.
        center: (LengthPercentage, LengthPercentage),
    },
}

/// A computed gradient.
#[derive(Clone, Debug, PartialEq)]
pub struct Gradient {
    /// Linear or radial geometry.
    pub kind: GradientKind,
    /// Color stops; positions are along the gradient line (percentages of its length).
    pub stops: Vec<(Color, Option<LengthPercentage>)>,
    /// `repeating-*` variant.
    pub repeating: bool,
}

fn items(values: &[ComponentValue]) -> Vec<&ComponentValue> {
    values.iter().filter(|v| !v.is_whitespace()).collect()
}

fn angle_of(v: &ComponentValue) -> Option<f32> {
    match v {
        ComponentValue::Token(Token::Dimension { value, unit, .. }) => match unit.to_ascii_lowercase().as_str() {
            "deg" => Some(*value),
            "rad" => Some(value.to_degrees()),
            "grad" => Some(value * 0.9),
            "turn" => Some(value * 360.0),
            _ => None,
        },
        ComponentValue::Token(Token::Number { value, .. }) if *value == 0.0 => Some(0.0),
        _ => None,
    }
}

/// Parses a gradient function. `current` resolves `currentcolor`.
pub fn parse_gradient(v: &ComponentValue, ctx: &LengthContext, current: Color) -> Option<Gradient> {
    let ComponentValue::Function { name, args } = v else {
        return None;
    };
    let lname = name.to_ascii_lowercase();
    let lname = lname.trim_start_matches("-webkit-");
    let (linear, repeating) = match lname {
        "linear-gradient" => (true, false),
        "repeating-linear-gradient" => (true, true),
        "radial-gradient" => (false, false),
        "repeating-radial-gradient" => (false, true),
        _ => return None,
    };
    let parts: Vec<&[ComponentValue]> = args
        .split(|a| matches!(a, ComponentValue::Token(Token::Comma)))
        .collect();
    let mut first_stop = 0;
    let first = items(parts.first()?);
    let kind = if linear {
        let mut angle = 180.0;
        if let Some(a) = first.first().and_then(|v| angle_of(v)) {
            angle = a;
            first_stop = 1;
        } else if first
            .first()
            .and_then(|v| v.ident())
            .is_some_and(|s| s.eq_ignore_ascii_case("to"))
        {
            let words: Vec<String> = first[1..]
                .iter()
                .filter_map(|v| v.ident().map(|s| s.to_ascii_lowercase()))
                .collect();
            let has = |w: &str| words.iter().any(|x| x == w);
            angle = match (has("top"), has("bottom"), has("left"), has("right")) {
                (true, _, false, false) => 0.0,
                (_, true, false, false) => 180.0,
                (false, false, true, _) => 270.0,
                (false, false, _, true) => 90.0,
                (true, _, true, _) => 315.0,
                (true, _, _, true) => 45.0,
                (_, true, true, _) => 225.0,
                (_, true, _, true) => 135.0,
                _ => 180.0,
            };
            first_stop = 1;
        }
        GradientKind::Linear { angle }
    } else {
        let mut circle = false;
        let mut extent = RadialExtent::FarthestCorner;
        let mut radii = None;
        let pct = |p: f32| LengthPercentage::pct(p);
        let mut center = (pct(50.0), pct(50.0));
        let is_shape_part = first.iter().any(|v| {
            v.ident().is_some_and(|s| {
                matches!(
                    s.to_ascii_lowercase().as_str(),
                    "circle"
                        | "ellipse"
                        | "at"
                        | "closest-side"
                        | "farthest-side"
                        | "closest-corner"
                        | "farthest-corner"
                )
            })
        }) || (first.len() <= 2 && first.iter().all(|v| parse_length(v).is_some()));
        if is_shape_part {
            first_stop = 1;
            let at = first
                .iter()
                .position(|v| v.ident().is_some_and(|s| s.eq_ignore_ascii_case("at")));
            let (shape_part, pos_part) = match at {
                Some(i) => (&first[..i], &first[i + 1..]),
                None => (&first[..], &[][..]),
            };
            let mut lengths = Vec::new();
            for v in shape_part {
                match v.ident().map(|s| s.to_ascii_lowercase()).as_deref() {
                    Some("circle") => circle = true,
                    Some("ellipse") => circle = false,
                    Some("closest-side") => extent = RadialExtent::ClosestSide,
                    Some("farthest-side") => extent = RadialExtent::FarthestSide,
                    Some("closest-corner") => extent = RadialExtent::ClosestCorner,
                    Some("farthest-corner") => extent = RadialExtent::FarthestCorner,
                    _ => lengths.push(parse_length(v)?.compute(ctx)),
                }
            }
            match lengths.len() {
                1 => {
                    circle = true;
                    radii = Some((lengths[0].clone(), lengths[0].clone()));
                }
                2 => radii = Some((lengths[0].clone(), lengths[1].clone())),
                _ => {}
            }
            let pos: Vec<LengthPercentage> = pos_part
                .iter()
                .map(|v| match v.ident().map(|s| s.to_ascii_lowercase()).as_deref() {
                    Some("left") | Some("top") => Some(pct(0.0)),
                    Some("right") | Some("bottom") => Some(pct(100.0)),
                    Some("center") => Some(pct(50.0)),
                    _ => parse_length(v).map(|l| l.compute(ctx)),
                })
                .collect::<Option<Vec<_>>>()?;
            match pos.as_slice() {
                [a] => center.0 = a.clone(),
                [a, b] => center = (a.clone(), b.clone()),
                _ => {}
            }
            // `at top` alone refers to the vertical axis.
            if pos_part.len() == 1
                && pos_part[0]
                    .ident()
                    .is_some_and(|s| s.eq_ignore_ascii_case("top") || s.eq_ignore_ascii_case("bottom"))
            {
                center = (pct(50.0), pos[0].clone());
            }
        }
        GradientKind::Radial {
            circle,
            radii,
            extent,
            center,
        }
    };
    let mut stops = Vec::new();
    for part in &parts[first_stop..] {
        let it = items(part);
        if it.is_empty() {
            return None;
        }
        let color = if it[0].ident().is_some_and(|s| s.eq_ignore_ascii_case("currentcolor")) {
            current
        } else {
            match parse_color(it[0]) {
                Some(c) => c,
                None => {
                    // A transition hint (bare length) is accepted and ignored.
                    if it.len() == 1 && parse_length(it[0]).is_some() {
                        continue;
                    }
                    return None;
                }
            }
        };
        let positions: Vec<LengthPercentage> = it[1..]
            .iter()
            .map(|v| parse_length(v).map(|l| l.compute(ctx)))
            .collect::<Option<Vec<_>>>()?;
        match positions.len() {
            0 => stops.push((color, None)),
            1 => stops.push((color, Some(positions[0].clone()))),
            _ => {
                stops.push((color, Some(positions[0].clone())));
                stops.push((color, Some(positions[1].clone())));
            }
        }
    }
    if stops.len() < 2 {
        return None;
    }
    Some(Gradient { kind, stops, repeating })
}

/// Whether a component value is a gradient function the engine understands.
pub fn is_gradient(v: &ComponentValue) -> bool {
    matches!(v, ComponentValue::Function { name, .. } if name.to_ascii_lowercase().trim_start_matches("-webkit-").ends_with("gradient"))
}

/// Resolves stop positions (in px along a gradient line of `len` px) per CSS Images 3:
/// missing first/last stops go to 0/100%, decreasing positions are clamped, and
/// unpositioned stops are spread evenly.
fn resolve_stops(stops: &[(Color, Option<LengthPercentage>)], len: f32) -> Vec<(f32, [f32; 4])> {
    let n = stops.len();
    let mut pos: Vec<Option<f32>> = stops.iter().map(|(_, p)| p.as_ref().map(|l| l.resolve(len))).collect();
    if pos[0].is_none() {
        pos[0] = Some(0.0);
    }
    if pos[n - 1].is_none() {
        pos[n - 1] = Some(len);
    }
    let mut max = f32::NEG_INFINITY;
    for p in pos.iter_mut().flatten() {
        if *p < max {
            *p = max;
        }
        max = max.max(*p);
    }
    let mut i = 0;
    while i < n {
        if pos[i].is_none() {
            let start = i - 1;
            let mut end = i;
            while pos[end].is_none() {
                end += 1;
            }
            let (a, b) = (pos[start].unwrap(), pos[end].unwrap());
            for (k, slot) in pos.iter_mut().enumerate().take(end).skip(start + 1) {
                *slot = Some(a + (b - a) * (k - start) as f32 / (end - start) as f32);
            }
            i = end;
        }
        i += 1;
    }
    stops
        .iter()
        .zip(pos)
        .map(|((c, _), p)| {
            let a = c.a as f32 / 255.0;
            (p.unwrap(), [c.r as f32 * a, c.g as f32 * a, c.b as f32 * a, c.a as f32])
        })
        .collect()
}

fn color_at(stops: &[(f32, [f32; 4])], t: f32) -> [f32; 4] {
    if t <= stops[0].0 {
        return stops[0].1;
    }
    for w in stops.windows(2) {
        let (p0, c0) = w[0];
        let (p1, c1) = w[1];
        if t <= p1 {
            if p1 - p0 < 1e-6 {
                return c1;
            }
            let f = (t - p0) / (p1 - p0);
            return [
                c0[0] + (c1[0] - c0[0]) * f,
                c0[1] + (c1[1] - c0[1]) * f,
                c0[2] + (c1[2] - c0[2]) * f,
                c0[3] + (c1[3] - c0[3]) * f,
            ];
        }
    }
    stops[stops.len() - 1].1
}

/// Renders a gradient filling a `width` x `height` box into an image of `pw` x `ph`
/// pixels (the box may be rendered at reduced resolution and scaled when painted).
pub fn render_gradient(g: &Gradient, width: f32, height: f32, pw: u32, ph: u32) -> Option<Rc<Image>> {
    if pw == 0 || ph == 0 || width <= 0.0 || height <= 0.0 {
        return None;
    }
    let (sx, sy) = (width / pw as f32, height / ph as f32);
    let mut pixels = vec![0u8; (pw * ph * 4) as usize];
    // Each kind maps a box point to a distance along the gradient line, of length `len`.
    let (len, eval): (f32, Box<dyn Fn(f32, f32) -> f32>) = match &g.kind {
        GradientKind::Linear { angle } => {
            let a = angle.to_radians();
            let (dx, dy) = (a.sin(), -a.cos());
            let len = (width * dx).abs() + (height * dy).abs();
            let (cx, cy) = (width / 2.0, height / 2.0);
            (len, Box::new(move |x, y| (x - cx) * dx + (y - cy) * dy + len / 2.0))
        }
        GradientKind::Radial {
            circle,
            radii,
            extent,
            center,
        } => {
            let cx = center.0.resolve(width);
            let cy = center.1.resolve(height);
            let (l, r, t, b) = (cx.abs(), (width - cx).abs(), cy.abs(), (height - cy).abs());
            let (mut rx, mut ry) = match radii {
                Some((h, v)) => (h.resolve(width), v.resolve(height)),
                None => match extent {
                    RadialExtent::ClosestSide => (l.min(r), t.min(b)),
                    RadialExtent::FarthestSide => (l.max(r), t.max(b)),
                    RadialExtent::ClosestCorner => {
                        (l.min(r) * std::f32::consts::SQRT_2, t.min(b) * std::f32::consts::SQRT_2)
                    }
                    RadialExtent::FarthestCorner => {
                        (l.max(r) * std::f32::consts::SQRT_2, t.max(b) * std::f32::consts::SQRT_2)
                    }
                },
            };
            if *circle && radii.is_none() {
                let rr = match extent {
                    RadialExtent::ClosestSide => l.min(r).min(t).min(b),
                    RadialExtent::FarthestSide => l.max(r).max(t).max(b),
                    RadialExtent::ClosestCorner => (l.min(r).powi(2) + t.min(b).powi(2)).sqrt(),
                    RadialExtent::FarthestCorner => (l.max(r).powi(2) + t.max(b).powi(2)).sqrt(),
                };
                rx = rr;
                ry = rr;
            }
            let (rx, ry) = (rx.max(0.01), ry.max(0.01));
            (
                rx,
                Box::new(move |x, y| (((x - cx) / rx).powi(2) + ((y - cy) / ry).powi(2)).sqrt() * rx),
            )
        }
    };
    let len = len.max(0.01);
    let stops = resolve_stops(&g.stops, len);
    let (first, last) = (stops[0].0, stops[stops.len() - 1].0);
    let period = last - first;
    for py in 0..ph {
        for px in 0..pw {
            let x = (px as f32 + 0.5) * sx;
            let y = (py as f32 + 0.5) * sy;
            let mut t = eval(x, y);
            if g.repeating && period > 0.0 {
                t = first + (t - first).rem_euclid(period);
            }
            let c = color_at(&stops, t);
            let i = ((py * pw + px) * 4) as usize;
            for k in 0..4 {
                pixels[i + k] = (c[k] + 0.5).clamp(0.0, 255.0) as u8;
            }
        }
    }
    Some(Rc::new(Image {
        width: pw,
        height: ph,
        pixels,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::css::parser::parse_component_values;

    fn ctx() -> LengthContext {
        LengthContext {
            font_size: 16.0,
            root_font_size: 16.0,
            viewport_width: 800.0,
            viewport_height: 600.0,
        }
    }

    fn grad(s: &str) -> Gradient {
        let v = parse_component_values(s);
        parse_gradient(&v[0], &ctx(), Color::BLACK).unwrap_or_else(|| panic!("bad gradient {s}"))
    }

    fn px(img: &Image, x: u32, y: u32) -> [u8; 4] {
        let i = ((y * img.width + x) * 4) as usize;
        [img.pixels[i], img.pixels[i + 1], img.pixels[i + 2], img.pixels[i + 3]]
    }

    #[test]
    fn parses_directions_and_stops() {
        assert_eq!(
            grad("linear-gradient(red, blue)").kind,
            GradientKind::Linear { angle: 180.0 }
        );
        assert_eq!(
            grad("linear-gradient(to right, red, blue)").kind,
            GradientKind::Linear { angle: 90.0 }
        );
        assert_eq!(grad("linear-gradient(45deg, red 10%, blue 20px, green)").stops.len(), 3);
        assert_eq!(grad("linear-gradient(red 0 50%, blue 50%)").stops.len(), 3);
        assert!(matches!(
            grad("radial-gradient(circle at top left, red, blue)").kind,
            GradientKind::Radial { circle: true, .. }
        ));
        let v = parse_component_values("linear-gradient(red)");
        assert!(parse_gradient(&v[0], &ctx(), Color::BLACK).is_none());
    }

    #[test]
    fn renders_linear_and_radial() {
        let g = grad("linear-gradient(to right, #000, #fff)");
        let img = render_gradient(&g, 100.0, 10.0, 100, 10).unwrap();
        assert!(px(&img, 0, 5)[0] < 5);
        assert!(px(&img, 99, 5)[0] > 250);
        assert!((px(&img, 50, 5)[0] as i32 - 128).abs() < 4);
        let g = grad("radial-gradient(circle closest-side, red, transparent)");
        let img = render_gradient(&g, 20.0, 20.0, 20, 20).unwrap();
        assert!(px(&img, 10, 10)[3] > 220);
        assert_eq!(px(&img, 0, 0)[3], 0);
        let g = grad("repeating-linear-gradient(90deg, red 0 10px, blue 10px 20px)");
        let img = render_gradient(&g, 40.0, 1.0, 40, 1).unwrap();
        assert_eq!(px(&img, 5, 0), [255, 0, 0, 255]);
        assert_eq!(px(&img, 15, 0), [0, 0, 255, 255]);
        assert_eq!(px(&img, 25, 0), [255, 0, 0, 255]);
    }
}
