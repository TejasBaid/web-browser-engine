//! CSS colors: named colors, hex notation, `rgb()/rgba()`, `hsl()/hsla()` and `hwb()`.

use super::parser::ComponentValue;
use super::tokenizer::Token;

/// An sRGB color with straight (non-premultiplied) 8-bit alpha.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
pub struct Color {
    /// Red.
    pub r: u8,
    /// Green.
    pub g: u8,
    /// Blue.
    pub b: u8,
    /// Alpha (255 = opaque).
    pub a: u8,
}

impl Color {
    /// Fully transparent black.
    pub const TRANSPARENT: Color = Color { r: 0, g: 0, b: 0, a: 0 };
    /// Opaque black.
    pub const BLACK: Color = Color::rgb(0, 0, 0);
    /// Opaque white.
    pub const WHITE: Color = Color::rgb(255, 255, 255);

    /// An opaque color.
    pub const fn rgb(r: u8, g: u8, b: u8) -> Color {
        Color { r, g, b, a: 255 }
    }

    /// A color with alpha.
    pub const fn rgba(r: u8, g: u8, b: u8, a: u8) -> Color {
        Color { r, g, b, a }
    }

    /// Whether the color is fully transparent.
    pub fn is_transparent(self) -> bool {
        self.a == 0
    }

    /// Returns the color with its alpha multiplied by `opacity` (0..=1).
    pub fn with_opacity(self, opacity: f32) -> Color {
        Color {
            a: (self.a as f32 * opacity.clamp(0.0, 1.0)).round() as u8,
            ..self
        }
    }
}

/// Parses a color from a single component value. `currentcolor` is not handled here
/// (it is a computed-value concept; see [`crate::style`]).
pub fn parse_color(v: &ComponentValue) -> Option<Color> {
    match v {
        ComponentValue::Token(Token::Ident(name)) => named_color(&name.to_ascii_lowercase()),
        ComponentValue::Token(Token::Hash { value, .. }) => parse_hex(value),
        ComponentValue::Function { name, args } => {
            let name = name.to_ascii_lowercase();
            match name.as_str() {
                "rgb" | "rgba" => parse_rgb(args),
                "hsl" | "hsla" => parse_hsl(args),
                "hwb" => parse_hwb(args),
                // `light-dark(light, dark)`: the engine renders the light scheme.
                "light-dark" => args
                    .split(|a| matches!(a, ComponentValue::Token(Token::Comma)))
                    .next()
                    .and_then(|first| first.iter().find(|v| !v.is_whitespace()))
                    .and_then(parse_color),
                _ => None,
            }
        }
        _ => None,
    }
}

/// Parses `#rgb`, `#rgba`, `#rrggbb` or `#rrggbbaa` (without the `#`).
pub fn parse_hex(hex: &str) -> Option<Color> {
    if !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let d = |i: usize| u8::from_str_radix(&hex[i..i + 1], 16).unwrap();
    let dd = |i: usize| u8::from_str_radix(&hex[i..i + 2], 16).unwrap();
    match hex.len() {
        3 => Some(Color::rgb(d(0) * 17, d(1) * 17, d(2) * 17)),
        4 => Some(Color::rgba(d(0) * 17, d(1) * 17, d(2) * 17, d(3) * 17)),
        6 => Some(Color::rgb(dd(0), dd(2), dd(4))),
        8 => Some(Color::rgba(dd(0), dd(2), dd(4), dd(6))),
        _ => None,
    }
}

/// Parses the legacy HTML color syntax used by `bgcolor`/`color` attributes
/// ("rules for parsing a legacy colour value"), simplified.
pub fn parse_legacy_color(s: &str) -> Option<Color> {
    let s = s.trim();
    if s.is_empty() || s.eq_ignore_ascii_case("transparent") {
        return None;
    }
    if let Some(c) = named_color(&s.to_ascii_lowercase()) {
        return Some(c);
    }
    let hex = s.strip_prefix('#').unwrap_or(s);
    if (hex.len() == 3 || hex.len() == 6) && hex.bytes().all(|b| b.is_ascii_hexdigit()) {
        return parse_hex(hex);
    }
    // Replace non-hex characters with '0', pad to a multiple of 3, take the leading
    // two hex digits of each component.
    let mut digits: String = hex
        .chars()
        .take(128)
        .map(|c| if c.is_ascii_hexdigit() { c } else { '0' })
        .collect();
    if digits.is_empty() {
        return None;
    }
    while !digits.len().is_multiple_of(3) {
        digits.push('0');
    }
    let len = digits.len() / 3;
    let mut parts: Vec<&str> = (0..3).map(|i| &digits[i * len..(i + 1) * len]).collect();
    let mut trim = len.saturating_sub(8);
    let mut l = len - trim;
    while l > 2 && parts.iter().all(|p| p[trim..].starts_with('0')) {
        trim += 1;
        l -= 1;
    }
    for p in parts.iter_mut() {
        let t = &p[trim..];
        *p = &t[..t.len().min(2)];
    }
    let comp = |p: &str| u8::from_str_radix(&format!("{p:0<2}"), 16).unwrap_or(0);
    Some(Color::rgb(comp(parts[0]), comp(parts[1]), comp(parts[2])))
}

/// Splits function arguments into numeric-ish components, accepting both the legacy
/// comma syntax and the modern space syntax with `/ alpha`.
fn color_args(args: &[ComponentValue]) -> Option<(Vec<&Token>, Option<&Token>)> {
    let mut comps = Vec::new();
    let mut alpha = None;
    let mut after_slash = false;
    for a in args {
        match a {
            ComponentValue::Token(Token::Whitespace) | ComponentValue::Token(Token::Comma) => {}
            ComponentValue::Token(Token::Delim('/')) => after_slash = true,
            ComponentValue::Token(t) => {
                if after_slash {
                    alpha = Some(t);
                } else {
                    comps.push(t);
                }
            }
            _ => return None,
        }
    }
    if comps.len() == 4 && alpha.is_none() {
        alpha = comps.pop();
    }
    Some((comps, alpha))
}

fn alpha_value(t: Option<&Token>) -> Option<u8> {
    match t {
        None => Some(255),
        Some(Token::Number { value, .. }) => Some((value.clamp(0.0, 1.0) * 255.0).round() as u8),
        Some(Token::Percentage(p)) => Some((p.clamp(0.0, 100.0) / 100.0 * 255.0).round() as u8),
        Some(Token::Ident(s)) if s.eq_ignore_ascii_case("none") => Some(0),
        _ => None,
    }
}

fn parse_rgb(args: &[ComponentValue]) -> Option<Color> {
    let (comps, alpha) = color_args(args)?;
    if comps.len() != 3 {
        return None;
    }
    let mut ch = [0u8; 3];
    for (i, t) in comps.iter().enumerate() {
        ch[i] = match t {
            Token::Number { value, .. } => value.clamp(0.0, 255.0).round() as u8,
            Token::Percentage(p) => (p.clamp(0.0, 100.0) * 2.55).round() as u8,
            Token::Ident(s) if s.eq_ignore_ascii_case("none") => 0,
            _ => return None,
        };
    }
    Some(Color::rgba(ch[0], ch[1], ch[2], alpha_value(alpha)?))
}

fn hue_degrees(t: &Token) -> Option<f32> {
    match t {
        Token::Number { value, .. } => Some(*value),
        Token::Dimension { value, unit, .. } => match unit.to_ascii_lowercase().as_str() {
            "deg" => Some(*value),
            "rad" => Some(value.to_degrees()),
            "grad" => Some(value * 0.9),
            "turn" => Some(value * 360.0),
            _ => None,
        },
        Token::Ident(s) if s.eq_ignore_ascii_case("none") => Some(0.0),
        _ => None,
    }
}

fn percent_or_number(t: &Token) -> Option<f32> {
    match t {
        Token::Percentage(p) => Some(p / 100.0),
        Token::Number { value, .. } => Some(value / 100.0),
        Token::Ident(s) if s.eq_ignore_ascii_case("none") => Some(0.0),
        _ => None,
    }
}

fn parse_hsl(args: &[ComponentValue]) -> Option<Color> {
    let (comps, alpha) = color_args(args)?;
    if comps.len() != 3 {
        return None;
    }
    let h = hue_degrees(comps[0])?;
    let s = percent_or_number(comps[1])?.clamp(0.0, 1.0);
    let l = percent_or_number(comps[2])?.clamp(0.0, 1.0);
    let (r, g, b) = hsl_to_rgb(h, s, l);
    Some(Color::rgba(to_u8(r), to_u8(g), to_u8(b), alpha_value(alpha)?))
}

fn parse_hwb(args: &[ComponentValue]) -> Option<Color> {
    let (comps, alpha) = color_args(args)?;
    if comps.len() != 3 {
        return None;
    }
    let h = hue_degrees(comps[0])?;
    let w = percent_or_number(comps[1])?.clamp(0.0, 1.0);
    let bl = percent_or_number(comps[2])?.clamp(0.0, 1.0);
    if w + bl >= 1.0 {
        let gray = w / (w + bl);
        return Some(Color::rgba(to_u8(gray), to_u8(gray), to_u8(gray), alpha_value(alpha)?));
    }
    let (r, g, b) = hsl_to_rgb(h, 1.0, 0.5);
    let f = |c: f32| c * (1.0 - w - bl) + w;
    Some(Color::rgba(to_u8(f(r)), to_u8(f(g)), to_u8(f(b)), alpha_value(alpha)?))
}

fn to_u8(v: f32) -> u8 {
    (v.clamp(0.0, 1.0) * 255.0).round() as u8
}

/// Converts HSL (hue in degrees, saturation and lightness in 0..=1) to RGB in 0..=1.
pub fn hsl_to_rgb(h: f32, s: f32, l: f32) -> (f32, f32, f32) {
    let h = h.rem_euclid(360.0) / 360.0;
    let t2 = if l <= 0.5 { l * (s + 1.0) } else { l + s - l * s };
    let t1 = l * 2.0 - t2;
    let hue = |mut h: f32| {
        if h < 0.0 {
            h += 1.0;
        }
        if h > 1.0 {
            h -= 1.0;
        }
        if h * 6.0 < 1.0 {
            t1 + (t2 - t1) * h * 6.0
        } else if h * 2.0 < 1.0 {
            t2
        } else if h * 3.0 < 2.0 {
            t1 + (t2 - t1) * (2.0 / 3.0 - h) * 6.0
        } else {
            t1
        }
    };
    (hue(h + 1.0 / 3.0), hue(h), hue(h - 1.0 / 3.0))
}

/// Looks up a CSS named color (lowercase name), including `transparent` and the common
/// CSS system colors.
pub fn named_color(name: &str) -> Option<Color> {
    let rgb = |v: u32| Color::rgb((v >> 16) as u8, (v >> 8) as u8, v as u8);
    let c = match name {
        "transparent" => return Some(Color::TRANSPARENT),
        "aliceblue" => 0xf0f8ff,
        "antiquewhite" => 0xfaebd7,
        "aqua" => 0x00ffff,
        "aquamarine" => 0x7fffd4,
        "azure" => 0xf0ffff,
        "beige" => 0xf5f5dc,
        "bisque" => 0xffe4c4,
        "black" => 0x000000,
        "blanchedalmond" => 0xffebcd,
        "blue" => 0x0000ff,
        "blueviolet" => 0x8a2be2,
        "brown" => 0xa52a2a,
        "burlywood" => 0xdeb887,
        "cadetblue" => 0x5f9ea0,
        "chartreuse" => 0x7fff00,
        "chocolate" => 0xd2691e,
        "coral" => 0xff7f50,
        "cornflowerblue" => 0x6495ed,
        "cornsilk" => 0xfff8dc,
        "crimson" => 0xdc143c,
        "cyan" => 0x00ffff,
        "darkblue" => 0x00008b,
        "darkcyan" => 0x008b8b,
        "darkgoldenrod" => 0xb8860b,
        "darkgray" | "darkgrey" => 0xa9a9a9,
        "darkgreen" => 0x006400,
        "darkkhaki" => 0xbdb76b,
        "darkmagenta" => 0x8b008b,
        "darkolivegreen" => 0x556b2f,
        "darkorange" => 0xff8c00,
        "darkorchid" => 0x9932cc,
        "darkred" => 0x8b0000,
        "darksalmon" => 0xe9967a,
        "darkseagreen" => 0x8fbc8f,
        "darkslateblue" => 0x483d8b,
        "darkslategray" | "darkslategrey" => 0x2f4f4f,
        "darkturquoise" => 0x00ced1,
        "darkviolet" => 0x9400d3,
        "deeppink" => 0xff1493,
        "deepskyblue" => 0x00bfff,
        "dimgray" | "dimgrey" => 0x696969,
        "dodgerblue" => 0x1e90ff,
        "firebrick" => 0xb22222,
        "floralwhite" => 0xfffaf0,
        "forestgreen" => 0x228b22,
        "fuchsia" => 0xff00ff,
        "gainsboro" => 0xdcdcdc,
        "ghostwhite" => 0xf8f8ff,
        "gold" => 0xffd700,
        "goldenrod" => 0xdaa520,
        "gray" | "grey" => 0x808080,
        "green" => 0x008000,
        "greenyellow" => 0xadff2f,
        "honeydew" => 0xf0fff0,
        "hotpink" => 0xff69b4,
        "indianred" => 0xcd5c5c,
        "indigo" => 0x4b0082,
        "ivory" => 0xfffff0,
        "khaki" => 0xf0e68c,
        "lavender" => 0xe6e6fa,
        "lavenderblush" => 0xfff0f5,
        "lawngreen" => 0x7cfc00,
        "lemonchiffon" => 0xfffacd,
        "lightblue" => 0xadd8e6,
        "lightcoral" => 0xf08080,
        "lightcyan" => 0xe0ffff,
        "lightgoldenrodyellow" => 0xfafad2,
        "lightgray" | "lightgrey" => 0xd3d3d3,
        "lightgreen" => 0x90ee90,
        "lightpink" => 0xffb6c1,
        "lightsalmon" => 0xffa07a,
        "lightseagreen" => 0x20b2aa,
        "lightskyblue" => 0x87cefa,
        "lightslategray" | "lightslategrey" => 0x778899,
        "lightsteelblue" => 0xb0c4de,
        "lightyellow" => 0xffffe0,
        "lime" => 0x00ff00,
        "limegreen" => 0x32cd32,
        "linen" => 0xfaf0e6,
        "magenta" => 0xff00ff,
        "maroon" => 0x800000,
        "mediumaquamarine" => 0x66cdaa,
        "mediumblue" => 0x0000cd,
        "mediumorchid" => 0xba55d3,
        "mediumpurple" => 0x9370db,
        "mediumseagreen" => 0x3cb371,
        "mediumslateblue" => 0x7b68ee,
        "mediumspringgreen" => 0x00fa9a,
        "mediumturquoise" => 0x48d1cc,
        "mediumvioletred" => 0xc71585,
        "midnightblue" => 0x191970,
        "mintcream" => 0xf5fffa,
        "mistyrose" => 0xffe4e1,
        "moccasin" => 0xffe4b5,
        "navajowhite" => 0xffdead,
        "navy" => 0x000080,
        "oldlace" => 0xfdf5e6,
        "olive" => 0x808000,
        "olivedrab" => 0x6b8e23,
        "orange" => 0xffa500,
        "orangered" => 0xff4500,
        "orchid" => 0xda70d6,
        "palegoldenrod" => 0xeee8aa,
        "palegreen" => 0x98fb98,
        "paleturquoise" => 0xafeeee,
        "palevioletred" => 0xdb7093,
        "papayawhip" => 0xffefd5,
        "peachpuff" => 0xffdab9,
        "peru" => 0xcd853f,
        "pink" => 0xffc0cb,
        "plum" => 0xdda0dd,
        "powderblue" => 0xb0e0e6,
        "purple" => 0x800080,
        "rebeccapurple" => 0x663399,
        "red" => 0xff0000,
        "rosybrown" => 0xbc8f8f,
        "royalblue" => 0x4169e1,
        "saddlebrown" => 0x8b4513,
        "salmon" => 0xfa8072,
        "sandybrown" => 0xf4a460,
        "seagreen" => 0x2e8b57,
        "seashell" => 0xfff5ee,
        "sienna" => 0xa0522d,
        "silver" => 0xc0c0c0,
        "skyblue" => 0x87ceeb,
        "slateblue" => 0x6a5acd,
        "slategray" | "slategrey" => 0x708090,
        "snow" => 0xfffafa,
        "springgreen" => 0x00ff7f,
        "steelblue" => 0x4682b4,
        "tan" => 0xd2b48c,
        "teal" => 0x008080,
        "thistle" => 0xd8bfd8,
        "tomato" => 0xff6347,
        "turquoise" => 0x40e0d0,
        "violet" => 0xee82ee,
        "wheat" => 0xf5deb3,
        "white" => 0xffffff,
        "whitesmoke" => 0xf5f5f5,
        "yellow" => 0xffff00,
        "yellowgreen" => 0x9acd32,
        // System colors (light scheme defaults).
        "canvas" | "field" | "buttonhighlight" | "window" => 0xffffff,
        "canvastext" | "fieldtext" | "buttontext" | "windowtext" => 0x000000,
        "linktext" => 0x0000ee,
        "visitedtext" => 0x551a8b,
        "activetext" => 0xff0000,
        "buttonface" | "buttonborder" => 0xefefef,
        "graytext" => 0x808080,
        "highlight" => 0xb5d5ff,
        "highlighttext" => 0x000000,
        "mark" => 0xffff00,
        "marktext" => 0x000000,
        _ => return None,
    };
    Some(rgb(c))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::css::parser::parse_component_values;

    fn color(s: &str) -> Option<Color> {
        let v = parse_component_values(s);
        parse_color(&v[0])
    }

    #[test]
    fn all_syntaxes() {
        assert_eq!(color("red"), Some(Color::rgb(255, 0, 0)));
        assert_eq!(color("RebeccaPurple"), Some(Color::rgb(0x66, 0x33, 0x99)));
        assert_eq!(color("#0f8"), Some(Color::rgb(0, 255, 0x88)));
        assert_eq!(color("#0f88"), Some(Color::rgba(0, 255, 0x88, 0x88)));
        assert_eq!(color("#123456"), Some(Color::rgb(0x12, 0x34, 0x56)));
        assert_eq!(color("#12345680"), Some(Color::rgba(0x12, 0x34, 0x56, 0x80)));
        assert_eq!(color("rgb(1, 2, 3)"), Some(Color::rgb(1, 2, 3)));
        assert_eq!(color("rgba(255,0,0,0.5)"), Some(Color::rgba(255, 0, 0, 128)));
        assert_eq!(color("rgb(100% 0% 50% / 25%)"), Some(Color::rgba(255, 0, 128, 64)));
        assert_eq!(color("hsl(120, 100%, 50%)"), Some(Color::rgb(0, 255, 0)));
        assert_eq!(color("hsl(0deg 100% 25% / .5)"), Some(Color::rgba(128, 0, 0, 128)));
        assert_eq!(color("hsla(240, 100%, 50%, 1)"), Some(Color::rgb(0, 0, 255)));
        assert_eq!(color("transparent"), Some(Color::TRANSPARENT));
        assert_eq!(color("light-dark(#eee, #222)"), Some(Color::rgb(0xee, 0xee, 0xee)));
        assert_eq!(color("#12"), None);
        assert_eq!(color("notacolor"), None);
    }

    #[test]
    fn legacy_attribute_colors() {
        assert_eq!(parse_legacy_color("#f6f6ef"), Some(Color::rgb(0xf6, 0xf6, 0xef)));
        assert_eq!(parse_legacy_color("ff6600"), Some(Color::rgb(0xff, 0x66, 0x00)));
        assert_eq!(parse_legacy_color("White"), Some(Color::WHITE));
        assert_eq!(parse_legacy_color("chucknorris"), Some(Color::rgb(0xc0, 0, 0)));
    }
}
