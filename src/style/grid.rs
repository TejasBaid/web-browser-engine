//! CSS Grid property values: track lists (`grid-template-columns/rows`), implicit
//! track sizes, line placement (`grid-column`, `grid-row`, `grid-area`) and
//! `grid-template-areas`.
//!
//! Grid declarations are validated at parse time but kept as tokens, and converted to
//! computed values (with font- and viewport-relative lengths resolved) at
//! computed-value time.

use crate::css::parser::ComponentValue;
use crate::css::tokenizer::Token;
use crate::css::values::{LengthContext, LengthPercentage, parse_length};

/// One track sizing function's breadth.
#[derive(Clone, Debug, PartialEq)]
pub enum TrackBreadth {
    /// A length or percentage.
    Length(LengthPercentage),
    /// A flexible `fr` value.
    Fr(f32),
    /// `auto`
    Auto,
    /// `min-content`
    MinContent,
    /// `max-content`
    MaxContent,
}

/// A track sizing function.
#[derive(Clone, Debug, PartialEq)]
pub enum TrackSize {
    /// A single breadth (`100px`, `1fr`, `auto`).
    Single(TrackBreadth),
    /// `minmax(min, max)`
    MinMax(TrackBreadth, TrackBreadth),
    /// `fit-content(limit)`
    FitContent(LengthPercentage),
}

impl TrackSize {
    /// The minimum sizing function.
    pub fn min(&self) -> TrackBreadth {
        match self {
            // `1fr` alone means `minmax(auto, 1fr)`.
            TrackSize::Single(TrackBreadth::Fr(_)) => TrackBreadth::Auto,
            TrackSize::Single(b) => b.clone(),
            TrackSize::MinMax(a, _) => a.clone(),
            TrackSize::FitContent(_) => TrackBreadth::Auto,
        }
    }

    /// The maximum sizing function.
    pub fn max(&self) -> TrackBreadth {
        match self {
            TrackSize::Single(b) => b.clone(),
            TrackSize::MinMax(_, b) => b.clone(),
            TrackSize::FitContent(_) => TrackBreadth::MaxContent,
        }
    }
}

/// `repeat()` counts.
#[derive(Clone, Debug, PartialEq)]
pub enum TrackRepeat {
    /// `repeat(3, ...)`
    Count(u32),
    /// `repeat(auto-fill, ...)`
    AutoFill,
    /// `repeat(auto-fit, ...)`
    AutoFit,
}

/// An entry of a track list.
#[derive(Clone, Debug, PartialEq)]
pub enum TrackListItem {
    /// A single track.
    Track(TrackSize),
    /// `repeat(count, tracks...)`
    Repeat(TrackRepeat, Vec<TrackSize>),
}

/// A grid line reference for `grid-row-start` etc.
#[derive(Clone, Debug, PartialEq, Default)]
pub enum GridLine {
    /// `auto`
    #[default]
    Auto,
    /// A line number (negative numbers count from the end).
    Line(i32),
    /// `span n`
    Span(u32),
    /// A named area or line (`main`).
    Named(String),
}

fn items(values: &[ComponentValue]) -> Vec<&ComponentValue> {
    values.iter().filter(|v| !v.is_whitespace()).collect()
}

fn parse_breadth(v: &ComponentValue, ctx: &LengthContext) -> Option<TrackBreadth> {
    match v {
        ComponentValue::Token(Token::Dimension { value, unit, .. }) if unit.eq_ignore_ascii_case("fr") => {
            (*value >= 0.0).then_some(TrackBreadth::Fr(*value))
        }
        ComponentValue::Token(Token::Ident(s)) => match s.to_ascii_lowercase().as_str() {
            "auto" => Some(TrackBreadth::Auto),
            "min-content" => Some(TrackBreadth::MinContent),
            "max-content" => Some(TrackBreadth::MaxContent),
            _ => None,
        },
        _ => {
            let l = parse_length(v)?;
            if !l.is_non_negative() {
                return None;
            }
            Some(TrackBreadth::Length(l.compute(ctx)))
        }
    }
}

fn parse_track_size_one(v: &ComponentValue, ctx: &LengthContext) -> Option<TrackSize> {
    if let ComponentValue::Function { name, args } = v {
        let lname = name.to_ascii_lowercase();
        if lname == "minmax" {
            let parts: Vec<&[ComponentValue]> = args
                .split(|a| matches!(a, ComponentValue::Token(Token::Comma)))
                .collect();
            if parts.len() != 2 {
                return None;
            }
            let a = items(parts[0]);
            let b = items(parts[1]);
            if a.len() != 1 || b.len() != 1 {
                return None;
            }
            let min = parse_breadth(a[0], ctx)?;
            if matches!(min, TrackBreadth::Fr(_)) {
                return None;
            }
            return Some(TrackSize::MinMax(min, parse_breadth(b[0], ctx)?));
        }
        if lname == "fit-content" {
            let a = items(args);
            if a.len() != 1 {
                return None;
            }
            return Some(TrackSize::FitContent(parse_length(a[0])?.compute(ctx)));
        }
    }
    parse_breadth(v, ctx).map(TrackSize::Single)
}

/// Parses a single track size (for `grid-auto-rows/columns`).
pub fn parse_track_size(values: &[ComponentValue], ctx: &LengthContext) -> Option<TrackSize> {
    let it = items(values);
    if it.len() != 1 {
        return None;
    }
    parse_track_size_one(it[0], ctx)
}

/// Parses a `<track-list>` (or `none`). Line names in brackets are accepted and ignored.
pub fn parse_track_list(values: &[ComponentValue], ctx: &LengthContext) -> Option<Vec<TrackListItem>> {
    let it = items(values);
    if it.len() == 1 && it[0].ident().is_some_and(|s| s.eq_ignore_ascii_case("none")) {
        return Some(Vec::new());
    }
    let mut out = Vec::new();
    for v in it {
        match v {
            ComponentValue::Block { open: '[', .. } => {}
            ComponentValue::Function { name, args } if name.eq_ignore_ascii_case("repeat") => {
                let mut parts = args.splitn(2, |a| matches!(a, ComponentValue::Token(Token::Comma)));
                let count = items(parts.next()?);
                let rest = parts.next()?;
                if count.len() != 1 {
                    return None;
                }
                let rep = match count[0] {
                    ComponentValue::Token(Token::Number { value, .. }) if *value >= 1.0 => {
                        TrackRepeat::Count((*value as u32).min(10_000))
                    }
                    c if c.ident().is_some_and(|s| s.eq_ignore_ascii_case("auto-fill")) => TrackRepeat::AutoFill,
                    c if c.ident().is_some_and(|s| s.eq_ignore_ascii_case("auto-fit")) => TrackRepeat::AutoFit,
                    _ => return None,
                };
                let mut tracks = Vec::new();
                for t in items(rest) {
                    if matches!(t, ComponentValue::Block { open: '[', .. }) {
                        continue;
                    }
                    tracks.push(parse_track_size_one(t, ctx)?);
                }
                if tracks.is_empty() {
                    return None;
                }
                out.push(TrackListItem::Repeat(rep, tracks));
            }
            _ => out.push(TrackListItem::Track(parse_track_size_one(v, ctx)?)),
        }
    }
    if out.is_empty() { None } else { Some(out) }
}

/// Parses a grid line: `auto`, `3`, `-1`, `span 2`, `name`, `span name` (treated as 1).
pub fn parse_grid_line(values: &[ComponentValue]) -> Option<GridLine> {
    let it = items(values);
    match it.as_slice() {
        [v] => match v {
            ComponentValue::Token(Token::Number { value, .. }) if value.fract() == 0.0 && *value != 0.0 => {
                Some(GridLine::Line(*value as i32))
            }
            ComponentValue::Token(Token::Ident(s)) if s.eq_ignore_ascii_case("auto") => Some(GridLine::Auto),
            ComponentValue::Token(Token::Ident(s)) if !s.eq_ignore_ascii_case("span") => {
                Some(GridLine::Named(s.clone()))
            }
            _ => None,
        },
        [a, b] => {
            let span_first = a.ident().is_some_and(|s| s.eq_ignore_ascii_case("span"));
            let span_second = b.ident().is_some_and(|s| s.eq_ignore_ascii_case("span"));
            let other = if span_first {
                b
            } else if span_second {
                a
            } else {
                // `<integer> <name>`: use the integer.
                return match a {
                    ComponentValue::Token(Token::Number { value, .. }) if *value != 0.0 => {
                        Some(GridLine::Line(*value as i32))
                    }
                    _ => None,
                };
            };
            match other {
                ComponentValue::Token(Token::Number { value, .. }) if *value >= 1.0 => {
                    Some(GridLine::Span(*value as u32))
                }
                ComponentValue::Token(Token::Ident(_)) => Some(GridLine::Span(1)),
                _ => None,
            }
        }
        _ => None,
    }
}

/// Parses `grid-template-areas` strings into rows of cell names (`.` = empty).
pub fn parse_areas(values: &[ComponentValue]) -> Option<Vec<Vec<String>>> {
    let it = items(values);
    if it.len() == 1 && it[0].ident().is_some_and(|s| s.eq_ignore_ascii_case("none")) {
        return Some(Vec::new());
    }
    let mut rows = Vec::new();
    for v in it {
        let ComponentValue::Token(Token::String(s)) = v else {
            return None;
        };
        let row: Vec<String> = s
            .split_ascii_whitespace()
            .map(|c| {
                if c.chars().all(|ch| ch == '.') {
                    ".".to_string()
                } else {
                    c.to_string()
                }
            })
            .collect();
        if row.is_empty() {
            return None;
        }
        rows.push(row);
    }
    let w = rows.first()?.len();
    if rows.iter().any(|r| r.len() != w) {
        return None;
    }
    Some(rows)
}

/// Splits shorthand values at top-level `/` delimiters.
pub fn split_slash(values: &[ComponentValue]) -> Vec<&[ComponentValue]> {
    values
        .split(|v| matches!(v, ComponentValue::Token(Token::Delim('/'))))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::css::parser::parse_component_values;

    fn ctx() -> LengthContext {
        LengthContext {
            font_size: 10.0,
            root_font_size: 10.0,
            viewport_width: 800.0,
            viewport_height: 600.0,
        }
    }

    #[test]
    fn track_lists() {
        let t = parse_track_list(
            &parse_component_values("[a] 100px 1fr minmax(2em, auto) repeat(2, 50%)"),
            &ctx(),
        )
        .unwrap();
        assert_eq!(t.len(), 4);
        assert_eq!(
            t[0],
            TrackListItem::Track(TrackSize::Single(TrackBreadth::Length(LengthPercentage::px(100.0))))
        );
        assert_eq!(t[1], TrackListItem::Track(TrackSize::Single(TrackBreadth::Fr(1.0))));
        assert_eq!(
            t[2],
            TrackListItem::Track(TrackSize::MinMax(
                TrackBreadth::Length(LengthPercentage::px(20.0)),
                TrackBreadth::Auto
            ))
        );
        assert!(matches!(t[3], TrackListItem::Repeat(TrackRepeat::Count(2), _)));
        let t = parse_track_list(&parse_component_values("repeat(auto-fill, minmax(120px, 1fr))"), &ctx()).unwrap();
        assert!(matches!(t[0], TrackListItem::Repeat(TrackRepeat::AutoFill, _)));
        assert!(parse_track_list(&parse_component_values("minmax(1fr, 2fr)"), &ctx()).is_none());
        assert!(parse_track_list(&parse_component_values("bogus"), &ctx()).is_none());
    }

    #[test]
    fn lines_and_areas() {
        assert_eq!(
            parse_grid_line(&parse_component_values("span 2")),
            Some(GridLine::Span(2))
        );
        assert_eq!(parse_grid_line(&parse_component_values("-1")), Some(GridLine::Line(-1)));
        assert_eq!(
            parse_grid_line(&parse_component_values("main")),
            Some(GridLine::Named("main".into()))
        );
        assert_eq!(parse_grid_line(&parse_component_values("0")), None);
        let a = parse_areas(&parse_component_values("\"head head\" \"nav main\" \". foot\"")).unwrap();
        assert_eq!(a[1], vec!["nav", "main"]);
        assert_eq!(a[2][0], ".");
        assert!(parse_areas(&parse_component_values("\"a b\" \"c\"")).is_none());
    }
}
