//! Media query evaluation (Media Queries Level 4 subset): media types, `and`/`or`/`not`,
//! `min-`/`max-` prefixed features and range syntax for `width`/`height`, plus
//! `orientation`, `aspect-ratio`, `prefers-color-scheme` and a few interaction features.

use super::parser::ComponentValue;
use super::tokenizer::Token;

/// The environment media queries are evaluated against.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MediaContext {
    /// Viewport width in CSS px.
    pub width: f32,
    /// Viewport height in CSS px.
    pub height: f32,
}

/// Evaluates a comma-separated media query list. An empty list matches.
pub fn evaluate_media_list(values: &[ComponentValue], ctx: &MediaContext) -> bool {
    let parts: Vec<&[ComponentValue]> = values
        .split(|v| matches!(v, ComponentValue::Token(Token::Comma)))
        .collect();
    if parts.len() == 1 && parts[0].iter().all(ComponentValue::is_whitespace) {
        return true;
    }
    parts.into_iter().any(|q| evaluate_query(q, ctx).unwrap_or(false))
}

/// Evaluates a single media query; `None` means the query is malformed (treated as `not all`).
fn evaluate_query(q: &[ComponentValue], ctx: &MediaContext) -> Option<bool> {
    let items: Vec<&ComponentValue> = q.iter().filter(|v| !v.is_whitespace()).collect();
    if items.is_empty() {
        return None;
    }
    let mut i = 0;
    let mut negate = false;
    if let Some(id) = items[0].ident() {
        match id.to_ascii_lowercase().as_str() {
            "not" => {
                negate = true;
                i = 1;
            }
            "only" => i = 1,
            _ => {}
        }
    }
    let result;
    if let Some(ty) = items.get(i).and_then(|v| v.ident()) {
        let mut r = matches!(ty.to_ascii_lowercase().as_str(), "all" | "screen");
        i += 1;
        // Remaining: ("and" <condition-without-or>)*
        while i < items.len() {
            if !items[i].ident().is_some_and(|s| s.eq_ignore_ascii_case("and")) {
                return None;
            }
            let block = items.get(i + 1)?;
            r &= evaluate_in_parens(block, ctx)?;
            i += 2;
        }
        result = r;
    } else {
        if negate {
            // `not (cond)` without a media type is a condition, handled below.
            negate = false;
            i = 0;
        }
        result = evaluate_condition(&items[i..], ctx)?;
    }
    Some(result != negate)
}

/// `<media-condition>`: `not <in-parens>` | `<in-parens> [and <in-parens>]*` | `... [or ...]*`.
fn evaluate_condition(items: &[&ComponentValue], ctx: &MediaContext) -> Option<bool> {
    if items.is_empty() {
        return None;
    }
    if items[0].ident().is_some_and(|s| s.eq_ignore_ascii_case("not")) {
        return Some(!evaluate_in_parens(items.get(1)?, ctx)?);
    }
    let mut result = evaluate_in_parens(items[0], ctx)?;
    let mut i = 1;
    let mut op: Option<bool> = None; // Some(true) = and, Some(false) = or
    while i < items.len() {
        let is_and = match items[i].ident().map(|s| s.to_ascii_lowercase()) {
            Some(s) if s == "and" => true,
            Some(s) if s == "or" => false,
            _ => return None,
        };
        if op.is_some_and(|o| o != is_and) {
            return None; // mixing and/or without parens is invalid
        }
        op = Some(is_and);
        let rhs = evaluate_in_parens(items.get(i + 1)?, ctx)?;
        result = if is_and { result && rhs } else { result || rhs };
        i += 2;
    }
    Some(result)
}

fn evaluate_in_parens(v: &ComponentValue, ctx: &MediaContext) -> Option<bool> {
    let ComponentValue::Block { open: '(', contents } = v else {
        return None;
    };
    let items: Vec<&ComponentValue> = contents.iter().filter(|v| !v.is_whitespace()).collect();
    if items.is_empty() {
        return None;
    }
    // Nested condition?
    if matches!(items[0], ComponentValue::Block { .. })
        || items[0].ident().is_some_and(|s| s.eq_ignore_ascii_case("not")) && items.len() == 2
    {
        return evaluate_condition(&items, ctx);
    }
    evaluate_feature(&items, ctx)
}

fn evaluate_feature(items: &[&ComponentValue], ctx: &MediaContext) -> Option<bool> {
    // Plain `(feature)` boolean context.
    if items.len() == 1 {
        let name = items[0].ident()?.to_ascii_lowercase();
        return Some(match name.as_str() {
            "width" | "height" => true,
            "color" | "hover" | "pointer" | "any-hover" | "any-pointer" => true,
            "grid" | "monochrome" | "inverted-colors" | "prefers-reduced-motion" | "prefers-contrast" => false,
            _ => false,
        });
    }
    // `(name: value)`
    if items.len() >= 3 && matches!(items[1], ComponentValue::Token(Token::Colon)) {
        let name = items[0].ident()?.to_ascii_lowercase();
        let value = &items[2..];
        let (prefix, feature) = if let Some(f) = name.strip_prefix("min-") {
            (Some(true), f.to_string())
        } else if let Some(f) = name.strip_prefix("max-") {
            (Some(false), f.to_string())
        } else {
            (None, name.clone())
        };
        let feature = feature.trim_start_matches("-webkit-").to_string();
        return Some(match feature.as_str() {
            "width" | "height" | "device-width" | "device-height" => {
                let actual = if feature.ends_with("width") {
                    ctx.width
                } else {
                    ctx.height
                };
                let v = length_px(value.first()?)?;
                compare(actual, v, prefix)
            }
            "aspect-ratio" | "device-aspect-ratio" => {
                let r = ratio(value)?;
                compare(ctx.width / ctx.height, r, prefix)
            }
            "resolution" | "device-pixel-ratio" => {
                let v = match value.first()? {
                    ComponentValue::Token(Token::Number { value, .. }) => *value,
                    ComponentValue::Token(Token::Dimension { value, unit, .. }) => match unit.as_str() {
                        "dppx" | "x" => *value,
                        "dpi" => value / 96.0,
                        "dpcm" => value * 2.54 / 96.0,
                        _ => return None,
                    },
                    _ => return None,
                };
                compare(1.0, v, prefix)
            }
            "orientation" => {
                let o = value.first()?.ident()?.to_ascii_lowercase();
                let portrait = ctx.height >= ctx.width;
                (o == "portrait") == portrait
            }
            "prefers-color-scheme" => value.first()?.ident()?.eq_ignore_ascii_case("light"),
            "prefers-reduced-motion" | "prefers-reduced-transparency" | "prefers-reduced-data" => {
                value.first()?.ident()?.eq_ignore_ascii_case("no-preference")
            }
            "prefers-contrast" => value.first()?.ident()?.eq_ignore_ascii_case("no-preference"),
            "hover" | "any-hover" => value.first()?.ident()?.eq_ignore_ascii_case("hover"),
            "pointer" | "any-pointer" => value.first()?.ident()?.eq_ignore_ascii_case("fine"),
            "color" => true,
            "display-mode" => value.first()?.ident()?.eq_ignore_ascii_case("browser"),
            "scripting" => value.first()?.ident()?.eq_ignore_ascii_case("none"),
            _ => false,
        });
    }
    // Range syntax: `(width >= 600px)`, `(400px < width <= 800px)`.
    evaluate_range(items, ctx)
}

fn compare(actual: f32, v: f32, prefix: Option<bool>) -> bool {
    match prefix {
        Some(true) => actual >= v,
        Some(false) => actual <= v,
        None => (actual - v).abs() < 0.01,
    }
}

fn ratio(value: &[&ComponentValue]) -> Option<f32> {
    let num = |v: &ComponentValue| match v {
        ComponentValue::Token(Token::Number { value, .. }) => Some(*value),
        _ => None,
    };
    let a = num(value.first()?)?;
    if value.len() >= 3 && matches!(value[1], ComponentValue::Token(Token::Delim('/'))) {
        let b = num(value[2])?;
        Some(a / b)
    } else {
        Some(a)
    }
}

fn length_px(v: &ComponentValue) -> Option<f32> {
    match v {
        ComponentValue::Token(Token::Dimension { value, unit, .. }) => {
            let u = unit.to_ascii_lowercase();
            Some(match u.as_str() {
                "px" => *value,
                "em" | "rem" => value * 16.0,
                "pt" => value * 96.0 / 72.0,
                "pc" => value * 16.0,
                "in" => value * 96.0,
                "cm" => value * 96.0 / 2.54,
                "mm" => value * 96.0 / 25.4,
                _ => return None,
            })
        }
        ComponentValue::Token(Token::Number { value, .. }) if *value == 0.0 => Some(0.0),
        ComponentValue::Function { .. } => {
            // calc()/min()/max() of absolute and font-relative units (1em = 16px).
            let l = crate::css::values::parse_length(v)?;
            let ctx = crate::css::values::LengthContext {
                font_size: 16.0,
                root_font_size: 16.0,
                viewport_width: 0.0,
                viewport_height: 0.0,
            };
            let c = l.compute(&ctx);
            (!c.has_percentage()).then(|| c.resolve(0.0))
        }
        _ => None,
    }
}

#[derive(Clone, Copy)]
enum Op {
    Lt,
    Le,
    Gt,
    Ge,
    Eq,
}

fn parse_op(items: &[&ComponentValue], i: &mut usize) -> Option<Op> {
    let d = |k: usize| match items.get(k) {
        Some(ComponentValue::Token(Token::Delim(c))) => Some(*c),
        _ => None,
    };
    let first = d(*i)?;
    let eq_next = d(*i + 1) == Some('=');
    let op = match (first, eq_next) {
        ('<', true) => Op::Le,
        ('<', false) => Op::Lt,
        ('>', true) => Op::Ge,
        ('>', false) => Op::Gt,
        ('=', _) => Op::Eq,
        _ => return None,
    };
    *i += if eq_next && first != '=' { 2 } else { 1 };
    Some(op)
}

fn apply(a: f32, op: Op, b: f32) -> bool {
    match op {
        Op::Lt => a < b,
        Op::Le => a <= b,
        Op::Gt => a > b,
        Op::Ge => a >= b,
        Op::Eq => (a - b).abs() < 0.01,
    }
}

fn evaluate_range(items: &[&ComponentValue], ctx: &MediaContext) -> Option<bool> {
    let feature_value = |v: &ComponentValue| -> Option<f32> {
        match v.ident()?.to_ascii_lowercase().as_str() {
            "width" => Some(ctx.width),
            "height" => Some(ctx.height),
            _ => None,
        }
    };
    let mut i = 0;
    if let Some(actual) = feature_value(items[0]) {
        // feature op value
        i += 1;
        let op = parse_op(items, &mut i)?;
        let v = length_px(items.get(i)?)?;
        return Some(apply(actual, op, v));
    }
    // value op feature [op value]
    let v1 = length_px(items[0])?;
    i += 1;
    let op1 = parse_op(items, &mut i)?;
    let actual = feature_value(items.get(i)?)?;
    i += 1;
    let mut ok = apply(v1, op1, actual);
    if i < items.len() {
        let op2 = parse_op(items, &mut i)?;
        let v2 = length_px(items.get(i)?)?;
        ok &= apply(actual, op2, v2);
    }
    Some(ok)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::css::parser::parse_component_values;

    fn eval(q: &str, w: f32) -> bool {
        evaluate_media_list(
            &parse_component_values(q),
            &MediaContext {
                width: w,
                height: 600.0,
            },
        )
    }

    #[test]
    fn width_queries() {
        assert!(eval("", 800.0));
        assert!(eval("screen", 800.0));
        assert!(!eval("print", 800.0));
        assert!(eval("(min-width: 600px)", 800.0));
        assert!(!eval("(min-width: 600px)", 500.0));
        assert!(eval("screen and (max-width: 50em)", 800.0));
        assert!(!eval("screen and (max-width: 40em)", 800.0));
        assert!(eval("(width >= 600px)", 600.0));
        assert!(eval("(400px < width <= 800px)", 800.0));
        assert!(!eval("(400px < width <= 800px)", 801.0));
        assert!(eval("print, (min-width: 100px)", 800.0));
        assert!(eval("not print", 800.0));
        assert!(!eval("not screen and (min-width: 100px)", 800.0));
        assert!(eval("(orientation: landscape)", 800.0));
        assert!(eval("(prefers-color-scheme: light)", 800.0));
        assert!(eval("(min-width: 100px) and (max-width: 900px)", 800.0));
        assert!(eval("(max-width: 100px) or (min-width: 700px)", 800.0));
        assert!(eval("not (max-width: 100px)", 800.0));
        assert!(eval("screen and (max-width: calc(1120px - 1px))", 1024.0));
        assert!(!eval("screen and (max-width: calc(1120px - 1px))", 1120.0));
        assert!(!eval("garbage (", 800.0));
    }
}
