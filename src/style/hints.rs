//! Presentational hints: legacy HTML attributes (`bgcolor`, `width`, `align`, `<font
//! color>`, `cellpadding`, ...) mapped to CSS declarations. Per the HTML standard they act
//! as author-level declarations with zero specificity that precede all author rules.

use super::properties::{PropertyDeclaration, PropertyId, SpecifiedValue};
use crate::css::color::parse_legacy_color;
use crate::css::values::{LinearLength, SpecifiedLength};
use crate::dom::{Document, NodeId};
use std::rc::Rc;

/// Parses an HTML dimension attribute (`"100"`, `"85%"`, `" 12.5px"`).
pub fn parse_dimension(s: &str) -> Option<SpecifiedLength> {
    let s = s.trim_start();
    let num_end = s
        .char_indices()
        .find(|&(_, c)| !(c.is_ascii_digit() || c == '.'))
        .map(|(i, _)| i)
        .unwrap_or(s.len());
    if num_end == 0 {
        return None;
    }
    let v: f32 = s[..num_end].trim_end_matches('.').parse().ok()?;
    if s[num_end..].starts_with('%') {
        Some(SpecifiedLength::Linear(LinearLength::pct(v)))
    } else {
        Some(SpecifiedLength::px(v))
    }
}

fn non_zero_dimension(s: &str) -> Option<SpecifiedLength> {
    parse_dimension(s).filter(|l| !matches!(l, SpecifiedLength::Linear(x) if x.px == 0.0 && x.pct == 0.0))
}

fn push(out: &mut Vec<PropertyDeclaration>, id: PropertyId, v: SpecifiedValue) {
    out.push(PropertyDeclaration::Longhand(id, v));
}

fn push_len(out: &mut Vec<PropertyDeclaration>, id: PropertyId, l: SpecifiedLength) {
    push(out, id, SpecifiedValue::Length(l));
}

fn ancestor_table(doc: &Document, el: NodeId) -> Option<NodeId> {
    doc.ancestors(el).find(|&a| doc.is_html_element(a, "table"))
}

/// Returns the presentational-hint declarations for element `el`.
pub fn presentational_hints(doc: &Document, el: NodeId) -> Vec<PropertyDeclaration> {
    use PropertyId as P;
    use SpecifiedValue as V;
    let mut out = Vec::new();
    let Some(e) = doc.element(el) else { return out };
    if e.ns != crate::dom::Namespace::Html {
        return out;
    }
    let name = e.name.as_str();
    let attr = |n: &str| e.attr(n);

    // bgcolor / background
    if matches!(
        name,
        "body" | "table" | "tr" | "td" | "th" | "thead" | "tbody" | "tfoot"
    ) && let Some(c) = attr("bgcolor").and_then(parse_legacy_color)
    {
        push(&mut out, P::BackgroundColor, V::Color(c));
    }
    if matches!(name, "body" | "table" | "td" | "th")
        && let Some(bg) = attr("background").filter(|s| !s.trim().is_empty())
    {
        push(&mut out, P::BackgroundImage, V::Url(Rc::from(bg.trim())));
    }
    // width / height
    if matches!(
        name,
        "table" | "td" | "th" | "col" | "hr" | "iframe" | "embed" | "object" | "video" | "canvas" | "pre"
    ) && let Some(w) = attr("width").and_then(non_zero_dimension)
    {
        push_len(&mut out, P::Width, w);
    }
    if matches!(
        name,
        "table" | "td" | "th" | "tr" | "iframe" | "embed" | "object" | "video" | "canvas"
    ) && let Some(h) = attr("height").and_then(non_zero_dimension)
    {
        push_len(&mut out, P::Height, h);
    }
    if name == "img" {
        if let Some(w) = attr("width").and_then(parse_dimension) {
            push_len(&mut out, P::Width, w);
        }
        if let Some(h) = attr("height").and_then(parse_dimension) {
            push_len(&mut out, P::Height, h);
        }
        if let Some(b) = attr("border").and_then(parse_dimension) {
            for id in [
                P::BorderTopWidth,
                P::BorderRightWidth,
                P::BorderBottomWidth,
                P::BorderLeftWidth,
            ] {
                push_len(&mut out, id, b.clone());
            }
            for id in [
                P::BorderTopStyle,
                P::BorderRightStyle,
                P::BorderBottomStyle,
                P::BorderLeftStyle,
            ] {
                push(&mut out, id, V::Keyword("solid"));
            }
        }
        if let Some(h) = attr("hspace").and_then(parse_dimension) {
            push_len(&mut out, P::MarginLeft, h.clone());
            push_len(&mut out, P::MarginRight, h);
        }
        if let Some(v) = attr("vspace").and_then(parse_dimension) {
            push_len(&mut out, P::MarginTop, v.clone());
            push_len(&mut out, P::MarginBottom, v);
        }
    }
    // align
    if let Some(align) = attr("align").map(|a| a.trim().to_ascii_lowercase()) {
        match name {
            "table" => match align.as_str() {
                "center" => {
                    push(&mut out, P::MarginLeft, V::Keyword("auto"));
                    push(&mut out, P::MarginRight, V::Keyword("auto"));
                }
                "left" => push(&mut out, P::Float, V::Keyword("left")),
                "right" => push(&mut out, P::Float, V::Keyword("right")),
                _ => {}
            },
            "img" | "iframe" | "object" | "embed" => match align.as_str() {
                "left" => push(&mut out, P::Float, V::Keyword("left")),
                "right" => push(&mut out, P::Float, V::Keyword("right")),
                "middle" | "center" | "absmiddle" => push(&mut out, P::VerticalAlign, V::Keyword("middle")),
                "top" | "texttop" => push(&mut out, P::VerticalAlign, V::Keyword("top")),
                "bottom" | "baseline" | "absbottom" => push(&mut out, P::VerticalAlign, V::Keyword("baseline")),
                _ => {}
            },
            "hr" => match align.as_str() {
                "left" => push(&mut out, P::MarginLeft, V::Length(SpecifiedLength::px(0.0))),
                "right" => push(&mut out, P::MarginRight, V::Length(SpecifiedLength::px(0.0))),
                _ => {}
            },
            "div" | "p" | "h1" | "h2" | "h3" | "h4" | "h5" | "h6" | "td" | "th" | "tr" | "thead" | "tbody"
            | "tfoot" | "caption" | "legend" => {
                let k = match align.as_str() {
                    "center" | "middle" => Some("-webkit-center"),
                    "left" => Some("left"),
                    "right" => Some("right"),
                    "justify" => Some("justify"),
                    _ => None,
                };
                if let Some(k) = k {
                    let k = if matches!(name, "p" | "h1" | "h2" | "h3" | "h4" | "h5" | "h6" | "caption")
                        && k == "-webkit-center"
                    {
                        "center"
                    } else {
                        k
                    };
                    push(&mut out, P::TextAlign, V::Keyword(k));
                }
            }
            _ => {}
        }
    }
    // valign
    if matches!(name, "td" | "th" | "tr" | "thead" | "tbody" | "tfoot")
        && let Some(v) = attr("valign")
    {
        let k = match v.trim().to_ascii_lowercase().as_str() {
            "top" => Some("top"),
            "middle" | "center" => Some("middle"),
            "bottom" => Some("bottom"),
            "baseline" => Some("baseline"),
            _ => None,
        };
        if let Some(k) = k {
            push(&mut out, P::VerticalAlign, V::Keyword(k));
        }
    }
    // Tables: border, cellspacing; cells: cellpadding and border from the table.
    if name == "table" {
        if let Some(b) = attr("border") {
            let w = parse_dimension(b).unwrap_or(SpecifiedLength::px(1.0));
            let zero = matches!(&w, SpecifiedLength::Linear(x) if x.px == 0.0);
            if !zero {
                for id in [
                    P::BorderTopWidth,
                    P::BorderRightWidth,
                    P::BorderBottomWidth,
                    P::BorderLeftWidth,
                ] {
                    push_len(&mut out, id, w.clone());
                }
                for id in [
                    P::BorderTopStyle,
                    P::BorderRightStyle,
                    P::BorderBottomStyle,
                    P::BorderLeftStyle,
                ] {
                    push(&mut out, id, V::Keyword("outset"));
                }
            }
        }
        if let Some(cs) = attr("cellspacing").and_then(parse_dimension) {
            push(
                &mut out,
                P::BorderSpacing,
                V::Pair(Box::new(V::Length(cs.clone())), Box::new(V::Length(cs))),
            );
        }
    }
    if matches!(name, "td" | "th")
        && let Some(table) = ancestor_table(doc, el)
    {
        if let Some(cp) = doc.attr(table, "cellpadding").and_then(parse_dimension) {
            for id in [P::PaddingTop, P::PaddingRight, P::PaddingBottom, P::PaddingLeft] {
                push_len(&mut out, id, cp.clone());
            }
        }
        if let Some(b) = doc.attr(table, "border") {
            let nonzero = parse_dimension(b).is_none_or(|l| !matches!(l, SpecifiedLength::Linear(x) if x.px == 0.0));
            if nonzero {
                for id in [
                    P::BorderTopWidth,
                    P::BorderRightWidth,
                    P::BorderBottomWidth,
                    P::BorderLeftWidth,
                ] {
                    push_len(&mut out, id, SpecifiedLength::px(1.0));
                }
                for id in [
                    P::BorderTopStyle,
                    P::BorderRightStyle,
                    P::BorderBottomStyle,
                    P::BorderLeftStyle,
                ] {
                    push(&mut out, id, V::Keyword("inset"));
                }
            }
        }
        if attr("nowrap").is_some() {
            push(&mut out, P::WhiteSpace, V::Keyword("nowrap"));
        }
    }
    // <font>
    if name == "font" {
        if let Some(c) = attr("color").and_then(parse_legacy_color) {
            push(&mut out, P::Color, V::Color(c));
        }
        if let Some(face) = attr("face") {
            let fams: Vec<String> = face
                .split(',')
                .map(|f| f.trim().to_string())
                .filter(|f| !f.is_empty())
                .collect();
            if !fams.is_empty() {
                push(&mut out, P::FontFamily, V::FontFamily(Rc::from(fams)));
            }
        }
        if let Some(size) = attr("size").and_then(legacy_font_size) {
            push(&mut out, P::FontSize, V::Keyword(size));
        }
    }
    if name == "body"
        && let Some(c) = attr("text").and_then(parse_legacy_color)
    {
        push(&mut out, P::Color, V::Color(c));
    }
    if name == "hr" {
        if let Some(c) = attr("color").and_then(parse_legacy_color) {
            push(&mut out, P::BorderTopColor, V::Color(c));
            push(&mut out, P::BorderRightColor, V::Color(c));
            push(&mut out, P::BorderBottomColor, V::Color(c));
            push(&mut out, P::BorderLeftColor, V::Color(c));
            push(&mut out, P::BackgroundColor, V::Color(c));
        }
        if attr("noshade").is_some() {
            for id in [
                P::BorderTopStyle,
                P::BorderRightStyle,
                P::BorderBottomStyle,
                P::BorderLeftStyle,
            ] {
                push(&mut out, id, V::Keyword("solid"));
            }
        }
        if let Some(sz) = attr("size").and_then(parse_dimension)
            && let SpecifiedLength::Linear(l) = &sz
            && l.px > 1.0
        {
            push_len(&mut out, P::Height, SpecifiedLength::px(l.px - 2.0));
        }
    }
    out
}

/// Maps `<font size>` (1..7, or relative `+n`/`-n` from 3) to a font-size keyword.
fn legacy_font_size(s: &str) -> Option<&'static str> {
    let s = s.trim();
    let (rel, digits) = match s.as_bytes().first()? {
        b'+' => (1, &s[1..]),
        b'-' => (-1, &s[1..]),
        _ => (0, s),
    };
    let n: i32 = digits
        .chars()
        .take_while(|c| c.is_ascii_digit())
        .collect::<String>()
        .parse()
        .ok()?;
    let v = match rel {
        1 => 3 + n,
        -1 => 3 - n,
        _ => n,
    }
    .clamp(1, 7);
    Some(match v {
        1 => "x-small",
        2 => "small",
        3 => "medium",
        4 => "large",
        5 => "x-large",
        6 => "xx-large",
        _ => "xxx-large",
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dimensions() {
        assert_eq!(
            parse_dimension("85%"),
            Some(SpecifiedLength::Linear(LinearLength::pct(85.0)))
        );
        assert_eq!(parse_dimension(" 18"), Some(SpecifiedLength::px(18.0)));
        assert_eq!(parse_dimension("12px"), Some(SpecifiedLength::px(12.0)));
        assert_eq!(parse_dimension("abc"), None);
    }

    #[test]
    fn font_sizes() {
        assert_eq!(legacy_font_size("1"), Some("x-small"));
        assert_eq!(legacy_font_size("+2"), Some("x-large"));
        assert_eq!(legacy_font_size("-1"), Some("small"));
        assert_eq!(legacy_font_size("9"), Some("xxx-large"));
    }
}
