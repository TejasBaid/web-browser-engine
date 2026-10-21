//! CSS properties: the longhand table, declaration parsing (including shorthand
//! expansion and `var()` deferral) and computation of specified values into
//! [`ComputedStyle`] fields.

use super::computed::*;
use crate::css::color::{Color, parse_color};
use crate::css::parser::{ComponentValue, parse_component_values, to_css};
use crate::css::tokenizer::Token;
use crate::css::values::{LengthContext, LengthPercentage, SpecifiedLength, parse_length, parse_number};
use std::collections::HashMap;
use std::rc::Rc;

macro_rules! properties {
    ($($variant:ident => $name:literal, $inherited:literal;)*) => {
        /// Every longhand property the engine understands.
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
        #[allow(missing_docs)]
        pub enum PropertyId { $($variant,)* }

        impl PropertyId {
            /// All longhands, in declaration order.
            pub const ALL: &'static [PropertyId] = &[$(PropertyId::$variant,)*];

            /// The CSS name.
            pub fn name(self) -> &'static str {
                match self { $(PropertyId::$variant => $name,)* }
            }

            /// Whether the property is inherited by default.
            pub fn inherited(self) -> bool {
                match self { $(PropertyId::$variant => $inherited,)* }
            }

            /// Looks up a longhand by CSS name.
            pub fn from_name(name: &str) -> Option<PropertyId> {
                match name { $($name => Some(PropertyId::$variant),)* _ => None }
            }
        }
    };
}

properties! {
    Color => "color", true;
    FontFamily => "font-family", true;
    FontSize => "font-size", true;
    FontWeight => "font-weight", true;
    FontStyle => "font-style", true;
    LineHeight => "line-height", true;
    TextAlign => "text-align", true;
    TextIndent => "text-indent", true;
    TextTransform => "text-transform", true;
    WhiteSpace => "white-space", true;
    LetterSpacing => "letter-spacing", true;
    WordSpacing => "word-spacing", true;
    ListStyleType => "list-style-type", true;
    ListStylePosition => "list-style-position", true;
    ListStyleImage => "list-style-image", true;
    Visibility => "visibility", true;
    BorderCollapse => "border-collapse", true;
    BorderSpacing => "border-spacing", true;
    Cursor => "cursor", true;
    CaptionSide => "caption-side", true;
    OverflowWrap => "overflow-wrap", true;
    WordBreak => "word-break", true;
    Display => "display", false;
    Position => "position", false;
    Float => "float", false;
    Clear => "clear", false;
    Top => "top", false;
    Right => "right", false;
    Bottom => "bottom", false;
    Left => "left", false;
    Width => "width", false;
    Height => "height", false;
    MinWidth => "min-width", false;
    MinHeight => "min-height", false;
    MaxWidth => "max-width", false;
    MaxHeight => "max-height", false;
    MarginTop => "margin-top", false;
    MarginRight => "margin-right", false;
    MarginBottom => "margin-bottom", false;
    MarginLeft => "margin-left", false;
    PaddingTop => "padding-top", false;
    PaddingRight => "padding-right", false;
    PaddingBottom => "padding-bottom", false;
    PaddingLeft => "padding-left", false;
    BorderTopWidth => "border-top-width", false;
    BorderRightWidth => "border-right-width", false;
    BorderBottomWidth => "border-bottom-width", false;
    BorderLeftWidth => "border-left-width", false;
    BorderTopStyle => "border-top-style", false;
    BorderRightStyle => "border-right-style", false;
    BorderBottomStyle => "border-bottom-style", false;
    BorderLeftStyle => "border-left-style", false;
    BorderTopColor => "border-top-color", false;
    BorderRightColor => "border-right-color", false;
    BorderBottomColor => "border-bottom-color", false;
    BorderLeftColor => "border-left-color", false;
    BorderTopLeftRadius => "border-top-left-radius", false;
    BorderTopRightRadius => "border-top-right-radius", false;
    BorderBottomRightRadius => "border-bottom-right-radius", false;
    BorderBottomLeftRadius => "border-bottom-left-radius", false;
    BoxSizing => "box-sizing", false;
    BackgroundColor => "background-color", false;
    BackgroundImage => "background-image", false;
    BackgroundRepeat => "background-repeat", false;
    BackgroundPosition => "background-position", false;
    BackgroundSize => "background-size", false;
    OverflowX => "overflow-x", false;
    OverflowY => "overflow-y", false;
    Opacity => "opacity", false;
    ZIndex => "z-index", false;
    VerticalAlign => "vertical-align", false;
    TextDecorationLine => "text-decoration-line", false;
    TextDecorationColor => "text-decoration-color", false;
    TextDecorationStyle => "text-decoration-style", false;
    FlexDirection => "flex-direction", false;
    FlexWrap => "flex-wrap", false;
    JustifyContent => "justify-content", false;
    AlignItems => "align-items", false;
    AlignSelf => "align-self", false;
    AlignContent => "align-content", false;
    FlexGrow => "flex-grow", false;
    FlexShrink => "flex-shrink", false;
    FlexBasis => "flex-basis", false;
    Order => "order", false;
    RowGap => "row-gap", false;
    ColumnGap => "column-gap", false;
    TableLayout => "table-layout", false;
    Content => "content", false;
    BoxShadow => "box-shadow", false;
    OutlineWidth => "outline-width", false;
    OutlineStyle => "outline-style", false;
    OutlineColor => "outline-color", false;
    OutlineOffset => "outline-offset", false;
    ObjectFit => "object-fit", false;
    AspectRatio => "aspect-ratio", false;
    MaskImage => "mask-image", false;
    GridTemplateColumns => "grid-template-columns", false;
    GridTemplateRows => "grid-template-rows", false;
    GridAutoColumns => "grid-auto-columns", false;
    GridAutoRows => "grid-auto-rows", false;
    GridAutoFlow => "grid-auto-flow", false;
    GridColumnStart => "grid-column-start", false;
    GridColumnEnd => "grid-column-end", false;
    GridRowStart => "grid-row-start", false;
    GridRowEnd => "grid-row-end", false;
    GridTemplateAreas => "grid-template-areas", false;
    JustifyItems => "justify-items", false;
    JustifySelf => "justify-self", false;
    Transform => "transform", false;
    TextOverflow => "text-overflow", false;
    CounterReset => "counter-reset", false;
    CounterIncrement => "counter-increment", false;
    CounterSet => "counter-set", false;
}

/// Number of longhands.
pub const PROPERTY_COUNT: usize = PropertyId::ALL.len();

/// A shadow before computation.
#[derive(Clone, Debug, PartialEq)]
pub struct SpecifiedShadow {
    /// Offsets, blur and spread.
    pub lengths: [SpecifiedLength; 4],
    /// `None` means currentcolor.
    pub color: Option<Color>,
    /// `inset`
    pub inset: bool,
}

/// A parsed (specified) value of a longhand.
#[derive(Clone, Debug, PartialEq)]
pub enum SpecifiedValue {
    /// `inherit`
    Inherit,
    /// `initial`
    Initial,
    /// `unset` (also used for `revert` and for invalid-at-computed-value-time).
    Unset,
    /// A keyword from the property's grammar.
    Keyword(&'static str),
    /// A length-percentage.
    Length(SpecifiedLength),
    /// A number.
    Number(f32),
    /// An integer.
    Integer(i32),
    /// A color.
    Color(Color),
    /// `currentcolor`
    CurrentColor,
    /// A URL (unresolved).
    Url(Rc<str>),
    /// A font family list.
    FontFamily(Rc<[String]>),
    /// A literal string (e.g. `list-style-type: "-"`).
    Str(Rc<str>),
    /// Box shadows.
    Shadows(Rc<[SpecifiedShadow]>),
    /// `content` items.
    Content(Rc<[ContentItem]>),
    /// Two values (border radii, border-spacing, background-position, background-size).
    Pair(Box<SpecifiedValue>, Box<SpecifiedValue>),
    /// Validated tokens, converted at computed-value time (grid properties).
    Tokens(Rc<[ComponentValue]>),
    /// Text decoration line flags (underline, overline, line-through).
    DecorationLine(bool, bool, bool),
    /// A value containing `var()`: substituted and parsed at computed-value time.
    /// `shorthand` names the shorthand the declaration came from, if any.
    Unparsed {
        /// The raw CSS text of the value.
        css: Rc<str>,
        /// Originating shorthand name.
        shorthand: Option<&'static str>,
    },
}

/// One parsed declaration: a longhand (or custom property) and its value.
#[derive(Clone, Debug, PartialEq)]
pub enum PropertyDeclaration {
    /// A longhand.
    Longhand(PropertyId, SpecifiedValue),
    /// A custom property `--name: value` (value kept as CSS text).
    Custom(Rc<str>, Rc<str>),
}

// ---------------------------------------------------------------- parse helpers

fn non_ws(values: &[ComponentValue]) -> Vec<&ComponentValue> {
    values.iter().filter(|v| !v.is_whitespace()).collect()
}

fn single(values: &[ComponentValue]) -> Option<&ComponentValue> {
    let v = non_ws(values);
    if v.len() == 1 { Some(v[0]) } else { None }
}

fn keyword_of(v: &ComponentValue, allowed: &[&'static str]) -> Option<&'static str> {
    let id = v.ident()?;
    allowed.iter().copied().find(|k| k.eq_ignore_ascii_case(id))
}

fn kw(values: &[ComponentValue], allowed: &[&'static str]) -> Option<SpecifiedValue> {
    keyword_of(single(values)?, allowed).map(SpecifiedValue::Keyword)
}

fn length_value(v: &ComponentValue, non_negative: bool) -> Option<SpecifiedLength> {
    let l = parse_length(v)?;
    if non_negative && !l.is_non_negative() {
        return None;
    }
    Some(l)
}

fn len_or_kw(values: &[ComponentValue], allowed: &[&'static str], non_negative: bool) -> Option<SpecifiedValue> {
    let v = single(values)?;
    if let Some(k) = keyword_of(v, allowed) {
        return Some(SpecifiedValue::Keyword(k));
    }
    length_value(v, non_negative).map(SpecifiedValue::Length)
}

fn color_value(v: &ComponentValue) -> Option<SpecifiedValue> {
    if v.ident().is_some_and(|s| s.eq_ignore_ascii_case("currentcolor")) {
        return Some(SpecifiedValue::CurrentColor);
    }
    parse_color(v).map(SpecifiedValue::Color)
}

fn url_value(v: &ComponentValue) -> Option<Rc<str>> {
    match v {
        ComponentValue::Token(Token::Url(u)) => Some(Rc::from(u.as_str())),
        ComponentValue::Function { name, args } if name.eq_ignore_ascii_case("url") => {
            non_ws(args).into_iter().find_map(|a| match a {
                ComponentValue::Token(Token::String(s)) => Some(Rc::from(s.as_str())),
                _ => None,
            })
        }
        _ => None,
    }
}

fn border_width_value(v: &ComponentValue) -> Option<SpecifiedValue> {
    if let Some(k) = keyword_of(v, &["thin", "medium", "thick"]) {
        return Some(SpecifiedValue::Keyword(k));
    }
    length_value(v, true)
        .filter(|l| !matches!(l, SpecifiedLength::Linear(x) if x.pct != 0.0))
        .map(SpecifiedValue::Length)
}

const BORDER_STYLES: &[&str] = &[
    "none", "hidden", "solid", "dashed", "dotted", "double", "groove", "ridge", "inset", "outset",
];

const SIZE_KW: &[&str] = &[
    "auto",
    "min-content",
    "max-content",
    "fit-content",
    "-webkit-fill-available",
    "stretch",
];
const MAX_SIZE_KW: &[&str] = &[
    "none",
    "min-content",
    "max-content",
    "fit-content",
    "-webkit-fill-available",
    "stretch",
];

fn contains_var(values: &[ComponentValue]) -> bool {
    values.iter().any(|v| match v {
        ComponentValue::Function { name, args } => name.eq_ignore_ascii_case("var") || contains_var(args),
        ComponentValue::Block { contents, .. } => contains_var(contents),
        _ => false,
    })
}

fn css_wide(values: &[ComponentValue]) -> Option<SpecifiedValue> {
    let id = single(values)?.ident()?.to_ascii_lowercase();
    match id.as_str() {
        "inherit" => Some(SpecifiedValue::Inherit),
        "initial" => Some(SpecifiedValue::Initial),
        "unset" | "revert" | "revert-layer" => Some(SpecifiedValue::Unset),
        _ => None,
    }
}

// ---------------------------------------------------------------- longhands

/// Parses the value of a longhand. Returns `None` if the value is invalid.
pub fn parse_longhand(id: PropertyId, values: &[ComponentValue]) -> Option<SpecifiedValue> {
    use PropertyId as P;
    use SpecifiedValue as V;
    match id {
        P::Color
        | P::BackgroundColor
        | P::BorderTopColor
        | P::BorderRightColor
        | P::BorderBottomColor
        | P::BorderLeftColor
        | P::TextDecorationColor
        | P::OutlineColor => color_value(single(values)?),
        P::FontFamily => parse_font_family(values),
        P::FontSize => len_or_kw(
            values,
            &[
                "xx-small",
                "x-small",
                "small",
                "medium",
                "large",
                "x-large",
                "xx-large",
                "xxx-large",
                "smaller",
                "larger",
                "math",
            ],
            true,
        ),
        P::FontWeight => {
            let v = single(values)?;
            if let Some(k) = keyword_of(v, &["normal", "bold", "bolder", "lighter"]) {
                return Some(V::Keyword(k));
            }
            let n = parse_number(v)?;
            (1.0..=1000.0).contains(&n).then_some(V::Number(n))
        }
        P::FontStyle => {
            let v = non_ws(values);
            keyword_of(v.first()?, &["normal", "italic", "oblique"]).map(V::Keyword)
        }
        P::LineHeight => {
            let v = single(values)?;
            if let Some(k) = keyword_of(v, &["normal"]) {
                return Some(V::Keyword(k));
            }
            if let Some(n) = parse_number(v) {
                return (n >= 0.0).then_some(V::Number(n));
            }
            length_value(v, true).map(V::Length)
        }
        P::TextAlign => kw(
            values,
            &[
                "left",
                "right",
                "center",
                "justify",
                "start",
                "end",
                "-webkit-center",
                "-moz-center",
                "-webkit-left",
                "-webkit-right",
                "match-parent",
            ],
        ),
        P::TextIndent => single(values).and_then(|v| length_value(v, false)).map(V::Length),
        P::TextTransform => kw(values, &["none", "uppercase", "lowercase", "capitalize", "full-width"]),
        P::WhiteSpace => {
            // Also accept the CSS Text 4 longhand combos that map onto the classic values.
            let v = non_ws(values);
            if v.len() == 2 {
                let a = v[0].ident()?.to_ascii_lowercase();
                let b = v[1].ident()?.to_ascii_lowercase();
                return match (a.as_str(), b.as_str()) {
                    ("preserve", "nowrap") => Some(V::Keyword("pre")),
                    ("collapse", "nowrap") | ("nowrap", "collapse") => Some(V::Keyword("nowrap")),
                    ("preserve", "wrap") => Some(V::Keyword("pre-wrap")),
                    _ => None,
                };
            }
            kw(
                values,
                &["normal", "pre", "nowrap", "pre-wrap", "pre-line", "break-spaces"],
            )
        }
        P::LetterSpacing | P::WordSpacing => len_or_kw(values, &["normal"], false),
        P::ListStyleType => {
            let v = single(values)?;
            if let ComponentValue::Token(Token::String(s)) = v {
                return Some(V::Str(Rc::from(s.as_str())));
            }
            keyword_of(v, LIST_STYLE_TYPES).map(V::Keyword).or_else(|| {
                // Unknown counter styles fall back to decimal, like browsers do.
                v.ident().map(|_| V::Keyword("decimal"))
            })
        }
        P::ListStylePosition => kw(values, &["inside", "outside"]),
        P::ListStyleImage => {
            let v = single(values)?;
            if keyword_of(v, &["none"]).is_some() {
                return Some(V::Keyword("none"));
            }
            url_value(v).map(V::Url)
        }
        P::Visibility => kw(values, &["visible", "hidden", "collapse"]),
        P::BorderCollapse => kw(values, &["separate", "collapse"]),
        P::BorderSpacing => {
            let v = non_ws(values);
            match v.len() {
                1 => {
                    let l = length_value(v[0], true)?;
                    Some(V::Pair(Box::new(V::Length(l.clone())), Box::new(V::Length(l))))
                }
                2 => Some(V::Pair(
                    Box::new(V::Length(length_value(v[0], true)?)),
                    Box::new(V::Length(length_value(v[1], true)?)),
                )),
                _ => None,
            }
        }
        P::Cursor => {
            // `cursor: url(x), pointer` — use the last keyword.
            let last = non_ws(values).into_iter().rev().find(|v| v.ident().is_some())?;
            Some(V::Keyword(keyword_of(last, CURSORS).unwrap_or("auto")))
        }
        P::Display => parse_display(values),
        P::Position => kw(
            values,
            &["static", "relative", "absolute", "fixed", "sticky", "-webkit-sticky"],
        ),
        P::Float => kw(values, &["none", "left", "right", "inline-start", "inline-end"]),
        P::Clear => kw(values, &["none", "left", "right", "both", "inline-start", "inline-end"]),
        P::Top | P::Right | P::Bottom | P::Left | P::MarginTop | P::MarginRight | P::MarginBottom | P::MarginLeft => {
            len_or_kw(values, &["auto"], false)
        }
        P::Width | P::Height | P::MinWidth | P::MinHeight => len_or_kw(values, SIZE_KW, true),
        P::MaxWidth | P::MaxHeight => len_or_kw(values, MAX_SIZE_KW, true),
        P::PaddingTop | P::PaddingRight | P::PaddingBottom | P::PaddingLeft => {
            single(values).and_then(|v| length_value(v, true)).map(V::Length)
        }
        P::BorderTopWidth | P::BorderRightWidth | P::BorderBottomWidth | P::BorderLeftWidth | P::OutlineWidth => {
            border_width_value(single(values)?)
        }
        P::BorderTopStyle | P::BorderRightStyle | P::BorderBottomStyle | P::BorderLeftStyle => {
            kw(values, BORDER_STYLES)
        }
        P::OutlineStyle => kw(
            values,
            &[
                "none", "auto", "solid", "dashed", "dotted", "double", "groove", "ridge", "inset", "outset",
            ],
        ),
        P::OutlineOffset => single(values).and_then(|v| length_value(v, false)).map(V::Length),
        P::BorderTopLeftRadius | P::BorderTopRightRadius | P::BorderBottomRightRadius | P::BorderBottomLeftRadius => {
            let v = non_ws(values);
            match v.len() {
                1 => {
                    let l = length_value(v[0], true)?;
                    Some(V::Pair(Box::new(V::Length(l.clone())), Box::new(V::Length(l))))
                }
                2 => Some(V::Pair(
                    Box::new(V::Length(length_value(v[0], true)?)),
                    Box::new(V::Length(length_value(v[1], true)?)),
                )),
                _ => None,
            }
        }
        P::BoxSizing => kw(values, &["content-box", "border-box"]),
        P::BackgroundImage => {
            // Use the topmost layer that is a url() or a gradient (one image per box).
            let mut found = None;
            for layer in values.split(|v| matches!(v, ComponentValue::Token(Token::Comma))) {
                let layer = non_ws(layer);
                if layer.len() != 1 {
                    if layer.is_empty() {
                        return None;
                    }
                    continue;
                }
                if let Some(u) = url_value(layer[0]) {
                    found.get_or_insert(V::Url(u));
                } else if crate::css::gradient::is_gradient(layer[0]) {
                    found.get_or_insert(V::Tokens(Rc::from(vec![layer[0].clone()])));
                } else if keyword_of(layer[0], &["none"]).is_none()
                    && !matches!(layer[0], ComponentValue::Function { .. })
                {
                    return None;
                }
            }
            Some(found.unwrap_or(V::Keyword("none")))
        }
        P::BackgroundRepeat => {
            let first_layer = values
                .split(|v| matches!(v, ComponentValue::Token(Token::Comma)))
                .next()?;
            let v = non_ws(first_layer);
            match v.len() {
                1 => {
                    keyword_of(v[0], &["repeat", "repeat-x", "repeat-y", "no-repeat", "space", "round"]).map(V::Keyword)
                }
                2 => {
                    let a = keyword_of(v[0], &["repeat", "no-repeat", "space", "round"])?;
                    let b = keyword_of(v[1], &["repeat", "no-repeat", "space", "round"])?;
                    Some(V::Keyword(match (a, b) {
                        ("no-repeat", "no-repeat") => "no-repeat",
                        ("no-repeat", _) => "repeat-y",
                        (_, "no-repeat") => "repeat-x",
                        _ => "repeat",
                    }))
                }
                _ => None,
            }
        }
        P::BackgroundPosition => {
            let first_layer = values
                .split(|v| matches!(v, ComponentValue::Token(Token::Comma)))
                .next()?;
            parse_position(&non_ws(first_layer))
        }
        P::BackgroundSize => {
            let first_layer = values
                .split(|v| matches!(v, ComponentValue::Token(Token::Comma)))
                .next()?;
            parse_bg_size(&non_ws(first_layer))
        }
        P::OverflowX | P::OverflowY => kw(values, &["visible", "hidden", "clip", "scroll", "auto", "overlay"]),
        P::Opacity => {
            let v = single(values)?;
            match v {
                ComponentValue::Token(Token::Percentage(p)) => Some(V::Number(p / 100.0)),
                _ => parse_number(v).map(V::Number),
            }
        }
        P::ZIndex => {
            let v = single(values)?;
            if keyword_of(v, &["auto"]).is_some() {
                return Some(V::Keyword("auto"));
            }
            match v {
                ComponentValue::Token(Token::Number {
                    value,
                    kind: crate::css::tokenizer::NumKind::Integer,
                    ..
                }) => Some(V::Integer(*value as i32)),
                _ => None,
            }
        }
        P::VerticalAlign => len_or_kw(
            values,
            &[
                "baseline",
                "sub",
                "super",
                "text-top",
                "text-bottom",
                "middle",
                "top",
                "bottom",
            ],
            false,
        ),
        P::TextDecorationLine => parse_decoration_line(&non_ws(values)),
        P::TextDecorationStyle => kw(values, &["solid", "double", "dotted", "dashed", "wavy"]),
        P::FlexDirection => kw(values, &["row", "row-reverse", "column", "column-reverse"]),
        P::FlexWrap => kw(values, &["nowrap", "wrap", "wrap-reverse"]),
        P::JustifyContent | P::AlignContent => parse_content_distribution(values),
        P::AlignItems | P::AlignSelf => parse_self_alignment(values, id == P::AlignSelf),
        P::FlexGrow | P::FlexShrink => {
            let n = parse_number(single(values)?)?;
            (n >= 0.0).then_some(V::Number(n))
        }
        P::FlexBasis => len_or_kw(
            values,
            &["auto", "content", "min-content", "max-content", "fit-content"],
            true,
        ),
        P::Order => match single(values)? {
            ComponentValue::Token(Token::Number { value, .. }) => Some(V::Integer(*value as i32)),
            _ => None,
        },
        P::RowGap | P::ColumnGap => len_or_kw(values, &["normal"], true),
        P::TableLayout => kw(values, &["auto", "fixed"]),
        P::Content => parse_content(values),
        P::BoxShadow => parse_box_shadow(values),
        P::ObjectFit => kw(values, &["fill", "contain", "cover", "none", "scale-down"]),
        P::CaptionSide => kw(values, &["top", "bottom"]),
        P::OverflowWrap => kw(values, &["normal", "break-word", "anywhere"]),
        P::WordBreak => kw(values, &["normal", "break-all", "keep-all", "break-word"]),
        P::GridTemplateColumns | P::GridTemplateRows => {
            crate::style::grid::parse_track_list(values, &DUMMY_LENGTHS).map(|_| V::Tokens(Rc::from(values)))
        }
        P::GridAutoColumns | P::GridAutoRows => {
            crate::style::grid::parse_track_size(values, &DUMMY_LENGTHS).map(|_| V::Tokens(Rc::from(values)))
        }
        P::GridColumnStart | P::GridColumnEnd | P::GridRowStart | P::GridRowEnd => {
            crate::style::grid::parse_grid_line(values).map(|_| V::Tokens(Rc::from(values)))
        }
        P::GridTemplateAreas => crate::style::grid::parse_areas(values).map(|_| V::Tokens(Rc::from(values))),
        P::TextOverflow => kw(values, &["clip", "ellipsis"]),
        P::CounterReset | P::CounterIncrement | P::CounterSet => {
            parse_counter_list(values, if id == P::CounterIncrement { 1 } else { 0 })
                .map(|_| V::Tokens(Rc::from(values)))
        }
        P::Transform => {
            let v = non_ws(values);
            if v.len() == 1 && keyword_of(v[0], &["none"]).is_some() {
                return Some(V::Keyword("none"));
            }
            v.iter()
                .all(|x| matches!(x, ComponentValue::Function { .. }))
                .then(|| V::Tokens(Rc::from(values)))
        }
        P::GridAutoFlow => {
            let v = non_ws(values);
            let ok =
                !v.is_empty() && v.len() <= 2 && v.iter().all(|x| keyword_of(x, &["row", "column", "dense"]).is_some());
            ok.then(|| V::Tokens(Rc::from(values)))
        }
        P::JustifyItems | P::JustifySelf => {
            let v = non_ws(values);
            let last = v.last()?;
            let k = keyword_of(
                last,
                &[
                    "auto",
                    "normal",
                    "stretch",
                    "start",
                    "end",
                    "center",
                    "left",
                    "right",
                    "flex-start",
                    "flex-end",
                    "self-start",
                    "self-end",
                    "baseline",
                    "legacy",
                ],
            )?;
            Some(V::Keyword(k))
        }
        P::MaskImage => {
            // Masks are not implemented; record only whether one is present.
            let v = non_ws(values);
            if v.len() == 1 && keyword_of(v[0], &["none"]).is_some() {
                Some(V::Keyword("none"))
            } else if v.is_empty() {
                None
            } else {
                Some(V::Keyword("mask"))
            }
        }
        P::AspectRatio => {
            let v = non_ws(values);
            let nums: Vec<f32> = v.iter().filter_map(|x| parse_number(x)).collect();
            if v.iter().any(|x| keyword_of(x, &["auto"]).is_some()) && nums.is_empty() {
                return Some(V::Keyword("auto"));
            }
            match nums.as_slice() {
                [a] if *a > 0.0 => Some(V::Number(*a)),
                [a, b] if *a > 0.0 && *b > 0.0 => Some(V::Number(a / b)),
                _ => None,
            }
        }
    }
}

const LIST_STYLE_TYPES: &[&str] = &[
    "none",
    "disc",
    "circle",
    "square",
    "decimal",
    "decimal-leading-zero",
    "lower-alpha",
    "upper-alpha",
    "lower-latin",
    "upper-latin",
    "lower-roman",
    "upper-roman",
    "lower-greek",
    "disclosure-open",
    "disclosure-closed",
];

const CURSORS: &[&str] = &[
    "auto",
    "default",
    "pointer",
    "text",
    "none",
    "help",
    "wait",
    "move",
    "crosshair",
    "not-allowed",
];

fn parse_display(values: &[ComponentValue]) -> Option<SpecifiedValue> {
    let v = non_ws(values);
    let idents: Option<Vec<String>> = v.iter().map(|x| x.ident().map(|s| s.to_ascii_lowercase())).collect();
    let idents = idents?;
    let key: Vec<&str> = idents.iter().map(String::as_str).collect();
    let k = match key.as_slice() {
        ["none"] => "none",
        ["contents"] => "contents",
        ["block"] | ["block", "flow"] => "block",
        ["inline"] | ["inline", "flow"] => "inline",
        ["inline-block"] | ["inline", "flow-root"] => "inline-block",
        ["flow-root"] | ["block", "flow-root"] => "flow-root",
        ["list-item"] | ["block", "list-item"] | ["list-item", "block"] | ["block", "flow", "list-item"] => "list-item",
        ["flex"] | ["block", "flex"] | ["-webkit-box"] | ["-webkit-flex"] | ["-ms-flexbox"] => "flex",
        ["inline-flex"] | ["inline", "flex"] | ["-webkit-inline-box"] | ["-webkit-inline-flex"] => "inline-flex",
        ["grid"] | ["block", "grid"] => "grid",
        ["inline-grid"] | ["inline", "grid"] => "inline-grid",
        ["table"] | ["block", "table"] => "table",
        ["inline-table"] | ["inline", "table"] => "inline-table",
        ["table-row-group"] => "table-row-group",
        ["table-header-group"] => "table-header-group",
        ["table-footer-group"] => "table-footer-group",
        ["table-row"] => "table-row",
        ["table-cell"] => "table-cell",
        ["table-column"] => "table-column",
        ["table-column-group"] => "table-column-group",
        ["table-caption"] => "table-caption",
        ["ruby"] | ["ruby-text"] | ["ruby-base"] => "inline",
        ["run-in"] => "block",
        _ => return None,
    };
    Some(SpecifiedValue::Keyword(k))
}

fn parse_font_family(values: &[ComponentValue]) -> Option<SpecifiedValue> {
    let mut families = Vec::new();
    for part in values.split(|v| matches!(v, ComponentValue::Token(Token::Comma))) {
        let items = non_ws(part);
        if items.is_empty() {
            return None;
        }
        if let [ComponentValue::Token(Token::String(s))] = items.as_slice() {
            families.push(s.clone());
            continue;
        }
        let words: Option<Vec<&str>> = items.iter().map(|i| i.ident()).collect();
        let words = words?;
        let name = words.join(" ");
        let lower = name.to_ascii_lowercase();
        if matches!(
            lower.as_str(),
            "serif"
                | "sans-serif"
                | "monospace"
                | "cursive"
                | "fantasy"
                | "system-ui"
                | "ui-sans-serif"
                | "ui-serif"
                | "ui-monospace"
                | "ui-rounded"
                | "math"
                | "emoji"
        ) {
            families.push(lower);
        } else {
            families.push(name);
        }
    }
    if families.is_empty() {
        return None;
    }
    Some(SpecifiedValue::FontFamily(Rc::from(families)))
}

fn parse_decoration_line(v: &[&ComponentValue]) -> Option<SpecifiedValue> {
    if v.len() == 1 && keyword_of(v[0], &["none"]).is_some() {
        return Some(SpecifiedValue::DecorationLine(false, false, false));
    }
    let (mut u, mut o, mut l) = (false, false, false);
    for x in v {
        match keyword_of(x, &["underline", "overline", "line-through", "blink"])? {
            "underline" => u = true,
            "overline" => o = true,
            "line-through" => l = true,
            _ => {}
        }
    }
    if v.is_empty() {
        return None;
    }
    Some(SpecifiedValue::DecorationLine(u, o, l))
}

fn parse_content_distribution(values: &[ComponentValue]) -> Option<SpecifiedValue> {
    let v = non_ws(values);
    // Ignore overflow-position prefixes (`safe center`).
    let last = v.last()?;
    if v.len() > 2 {
        return None;
    }
    if v.len() == 2 && keyword_of(v[0], &["safe", "unsafe"]).is_none() {
        return None;
    }
    keyword_of(
        last,
        &[
            "normal",
            "flex-start",
            "flex-end",
            "center",
            "space-between",
            "space-around",
            "space-evenly",
            "stretch",
            "start",
            "end",
            "left",
            "right",
        ],
    )
    .map(SpecifiedValue::Keyword)
}

fn parse_self_alignment(values: &[ComponentValue], allow_auto: bool) -> Option<SpecifiedValue> {
    let v = non_ws(values);
    let last = v.last()?;
    if v.len() > 2 {
        return None;
    }
    if v.len() == 2 && keyword_of(v[0], &["safe", "unsafe", "first", "last"]).is_none() {
        return None;
    }
    let k = keyword_of(
        last,
        &[
            "auto",
            "normal",
            "stretch",
            "flex-start",
            "flex-end",
            "center",
            "baseline",
            "start",
            "end",
            "self-start",
            "self-end",
            "left",
            "right",
        ],
    )?;
    if k == "auto" && !allow_auto {
        return None;
    }
    Some(SpecifiedValue::Keyword(k))
}

fn parse_content(values: &[ComponentValue]) -> Option<SpecifiedValue> {
    let v = non_ws(values);
    if v.len() == 1
        && let Some(k) = keyword_of(v[0], &["normal", "none"])
    {
        return Some(SpecifiedValue::Keyword(k));
    }
    let mut items = Vec::new();
    for x in v {
        match x {
            ComponentValue::Token(Token::String(s)) => items.push(ContentItem::String(s.clone())),
            ComponentValue::Token(Token::Delim('/')) => break, // alt text follows
            ComponentValue::Function { name, args } if name.eq_ignore_ascii_case("attr") => {
                let a = non_ws(args).first()?.ident()?.to_ascii_lowercase();
                items.push(ContentItem::Attr(a));
            }
            ComponentValue::Function { name, args } if name.eq_ignore_ascii_case("counter") => {
                let parts: Vec<Vec<&ComponentValue>> = args
                    .split(|a| matches!(a, ComponentValue::Token(Token::Comma)))
                    .map(non_ws)
                    .collect();
                let cname = parts.first()?.first()?.ident()?.to_string();
                let style = counter_style(parts.get(1).and_then(|p| p.first().copied()));
                items.push(ContentItem::Counter(cname, style));
            }
            ComponentValue::Function { name, args } if name.eq_ignore_ascii_case("counters") => {
                let parts: Vec<Vec<&ComponentValue>> = args
                    .split(|a| matches!(a, ComponentValue::Token(Token::Comma)))
                    .map(non_ws)
                    .collect();
                let cname = parts.first()?.first()?.ident()?.to_string();
                let sep = match parts.get(1)?.first()? {
                    ComponentValue::Token(Token::String(s)) => s.clone(),
                    _ => return None,
                };
                let style = counter_style(parts.get(2).and_then(|p| p.first().copied()));
                items.push(ContentItem::Counters(cname, sep, style));
            }
            ComponentValue::Token(Token::Ident(i)) => match i.to_ascii_lowercase().as_str() {
                "open-quote" => items.push(ContentItem::OpenQuote),
                "close-quote" => items.push(ContentItem::CloseQuote),
                "no-open-quote" | "no-close-quote" => {}
                _ => return None,
            },
            other => {
                if let Some(u) = url_value(other) {
                    items.push(ContentItem::Url(u.to_string()));
                } else if matches!(other, ComponentValue::Function { .. }) {
                    // Gradients/image-set: ignore.
                } else {
                    return None;
                }
            }
        }
    }
    Some(SpecifiedValue::Content(Rc::from(items)))
}

fn parse_box_shadow(values: &[ComponentValue]) -> Option<SpecifiedValue> {
    let v = non_ws(values);
    if v.len() == 1 && keyword_of(v[0], &["none"]).is_some() {
        return Some(SpecifiedValue::Shadows(Rc::from(Vec::new())));
    }
    let mut shadows = Vec::new();
    for layer in values.split(|v| matches!(v, ComponentValue::Token(Token::Comma))) {
        let mut lengths = Vec::new();
        let mut color = None;
        let mut inset = false;
        for x in non_ws(layer) {
            if keyword_of(x, &["inset"]).is_some() {
                inset = true;
            } else if let Some(l) = length_value(x, false) {
                lengths.push(l);
            } else {
                let c = color_value(x)?;
                color = match c {
                    SpecifiedValue::Color(c) => Some(c),
                    _ => None,
                };
            }
        }
        if !(2..=4).contains(&lengths.len()) {
            return None;
        }
        while lengths.len() < 4 {
            lengths.push(SpecifiedLength::px(0.0));
        }
        let lengths: [SpecifiedLength; 4] = lengths.try_into().ok()?;
        shadows.push(SpecifiedShadow { lengths, color, inset });
    }
    Some(SpecifiedValue::Shadows(Rc::from(shadows)))
}

fn position_component(v: &ComponentValue) -> Option<(Option<bool>, SpecifiedLength)> {
    // Returns (axis: Some(true)=x, Some(false)=y, None=either, value).
    if let Some(k) = keyword_of(v, &["left", "right", "top", "bottom", "center"]) {
        let pct = |p: f32| SpecifiedLength::Linear(crate::css::values::LinearLength::pct(p));
        return Some(match k {
            "left" => (Some(true), pct(0.0)),
            "right" => (Some(true), pct(100.0)),
            "top" => (Some(false), pct(0.0)),
            "bottom" => (Some(false), pct(100.0)),
            _ => (None, pct(50.0)),
        });
    }
    length_value(v, false).map(|l| (None, l))
}

fn parse_position(v: &[&ComponentValue]) -> Option<SpecifiedValue> {
    let pair = |x: SpecifiedLength, y: SpecifiedLength| {
        SpecifiedValue::Pair(Box::new(SpecifiedValue::Length(x)), Box::new(SpecifiedValue::Length(y)))
    };
    let center = || SpecifiedLength::Linear(crate::css::values::LinearLength::pct(50.0));
    match v.len() {
        1 => {
            let (axis, l) = position_component(v[0])?;
            Some(match axis {
                Some(false) => pair(center(), l),
                _ => pair(l, center()),
            })
        }
        2 => {
            let (a1, l1) = position_component(v[0])?;
            let (a2, l2) = position_component(v[1])?;
            if a1 == Some(false) || a2 == Some(true) {
                Some(pair(l2, l1))
            } else {
                Some(pair(l1, l2))
            }
        }
        _ => {
            // 3/4-value syntax with edge offsets: approximate using the keywords only.
            let comps: Vec<(Option<bool>, SpecifiedLength)> = v.iter().filter_map(|c| position_component(c)).collect();
            let x = comps
                .iter()
                .find(|c| c.0 == Some(true))
                .map(|c| c.1.clone())
                .unwrap_or_else(center);
            let y = comps
                .iter()
                .find(|c| c.0 == Some(false))
                .map(|c| c.1.clone())
                .unwrap_or_else(center);
            Some(pair(x, y))
        }
    }
}

fn parse_bg_size(v: &[&ComponentValue]) -> Option<SpecifiedValue> {
    match v.len() {
        1 => {
            if let Some(k) = keyword_of(v[0], &["cover", "contain", "auto"]) {
                return Some(SpecifiedValue::Keyword(k));
            }
            let l = length_value(v[0], true)?;
            Some(SpecifiedValue::Pair(
                Box::new(SpecifiedValue::Length(l)),
                Box::new(SpecifiedValue::Keyword("auto")),
            ))
        }
        2 => {
            let one = |x: &ComponentValue| -> Option<SpecifiedValue> {
                if keyword_of(x, &["auto"]).is_some() {
                    Some(SpecifiedValue::Keyword("auto"))
                } else {
                    length_value(x, true).map(SpecifiedValue::Length)
                }
            };
            Some(SpecifiedValue::Pair(Box::new(one(v[0])?), Box::new(one(v[1])?)))
        }
        _ => None,
    }
}

// ---------------------------------------------------------------- shorthands

/// Parses one declaration into longhand declarations. Shorthands are expanded; values
/// containing `var()` are deferred. Returns `None` if the declaration is invalid.
pub fn parse_declaration(name: &str, values: &[ComponentValue]) -> Option<Vec<PropertyDeclaration>> {
    if name.starts_with("--") {
        let css = to_css(values);
        return Some(vec![PropertyDeclaration::Custom(Rc::from(name), Rc::from(css.trim()))]);
    }
    let name = normalize_name(name);
    let longhands = shorthand_longhands(name);
    if let Some(wide) = css_wide(values) {
        return match (PropertyId::from_name(name), longhands) {
            (Some(id), _) => Some(vec![PropertyDeclaration::Longhand(id, wide)]),
            (None, Some(ls)) => Some(
                ls.iter()
                    .map(|&id| PropertyDeclaration::Longhand(id, wide.clone()))
                    .collect(),
            ),
            _ => None,
        };
    }
    if contains_var(values) {
        let css: Rc<str> = Rc::from(to_css(values).as_str());
        return match (PropertyId::from_name(name), longhands) {
            (Some(id), _) => Some(vec![PropertyDeclaration::Longhand(
                id,
                SpecifiedValue::Unparsed { css, shorthand: None },
            )]),
            (None, Some(ls)) => {
                let sh = SHORTHAND_NAMES.iter().copied().find(|s| *s == name);
                Some(
                    ls.iter()
                        .map(|&id| {
                            PropertyDeclaration::Longhand(
                                id,
                                SpecifiedValue::Unparsed {
                                    css: css.clone(),
                                    shorthand: sh,
                                },
                            )
                        })
                        .collect(),
                )
            }
            _ => None,
        };
    }
    if let Some(id) = PropertyId::from_name(name) {
        let v = parse_longhand(id, values)?;
        return Some(vec![PropertyDeclaration::Longhand(id, v)]);
    }
    let expanded = expand_shorthand(name, values)?;
    Some(
        expanded
            .into_iter()
            .map(|(id, v)| PropertyDeclaration::Longhand(id, v))
            .collect(),
    )
}

/// Maps logical (LTR, horizontal-tb) and legacy aliases onto physical property names.
fn normalize_name(name: &str) -> &str {
    match name {
        "margin-inline-start" => "margin-left",
        "margin-inline-end" => "margin-right",
        "margin-block-start" => "margin-top",
        "margin-block-end" => "margin-bottom",
        "padding-inline-start" => "padding-left",
        "padding-inline-end" => "padding-right",
        "padding-block-start" => "padding-top",
        "padding-block-end" => "padding-bottom",
        "inset-inline-start" => "left",
        "inset-inline-end" => "right",
        "inset-block-start" => "top",
        "inset-block-end" => "bottom",
        "inline-size" => "width",
        "block-size" => "height",
        "min-inline-size" => "min-width",
        "min-block-size" => "min-height",
        "max-inline-size" => "max-width",
        "max-block-size" => "max-height",
        "border-inline-start" => "border-left",
        "border-inline-end" => "border-right",
        "border-block-start" => "border-top",
        "border-block-end" => "border-bottom",
        "border-inline-start-width" => "border-left-width",
        "border-inline-end-width" => "border-right-width",
        "border-inline-start-color" => "border-left-color",
        "border-inline-end-color" => "border-right-color",
        "border-inline-start-style" => "border-left-style",
        "border-inline-end-style" => "border-right-style",
        "border-start-start-radius" => "border-top-left-radius",
        "border-start-end-radius" => "border-top-right-radius",
        "border-end-end-radius" => "border-bottom-right-radius",
        "border-end-start-radius" => "border-bottom-left-radius",
        "grid-gap" => "gap",
        "grid-row-gap" => "row-gap",
        "grid-column-gap" => "column-gap",
        "word-wrap" => "overflow-wrap",
        "-webkit-box-sizing" => "box-sizing",
        "-webkit-flex-direction" => "flex-direction",
        "-webkit-flex-wrap" => "flex-wrap",
        "-webkit-justify-content" => "justify-content",
        "-webkit-align-items" => "align-items",
        "-webkit-flex" => "flex",
        "-webkit-box-shadow" => "box-shadow",
        "-webkit-border-radius" | "-moz-border-radius" => "border-radius",
        "-webkit-mask-image" | "mask" | "-webkit-mask" => "mask-image",
        other => other,
    }
}

const SHORTHAND_NAMES: &[&str] = &[
    "margin",
    "padding",
    "border",
    "border-top",
    "border-right",
    "border-bottom",
    "border-left",
    "border-width",
    "border-style",
    "border-color",
    "border-radius",
    "background",
    "font",
    "flex",
    "flex-flow",
    "gap",
    "list-style",
    "text-decoration",
    "overflow",
    "inset",
    "outline",
    "margin-inline",
    "margin-block",
    "padding-inline",
    "padding-block",
    "inset-inline",
    "inset-block",
    "border-inline",
    "border-block",
    "place-items",
    "place-content",
    "place-self",
    "background-position",
    "grid-column",
    "grid-row",
    "grid-area",
    "grid-template",
];

fn shorthand_longhands(name: &str) -> Option<&'static [PropertyId]> {
    use PropertyId as P;
    Some(match name {
        "margin" => &[P::MarginTop, P::MarginRight, P::MarginBottom, P::MarginLeft],
        "padding" => &[P::PaddingTop, P::PaddingRight, P::PaddingBottom, P::PaddingLeft],
        "border-width" => &[
            P::BorderTopWidth,
            P::BorderRightWidth,
            P::BorderBottomWidth,
            P::BorderLeftWidth,
        ],
        "border-style" => &[
            P::BorderTopStyle,
            P::BorderRightStyle,
            P::BorderBottomStyle,
            P::BorderLeftStyle,
        ],
        "border-color" => &[
            P::BorderTopColor,
            P::BorderRightColor,
            P::BorderBottomColor,
            P::BorderLeftColor,
        ],
        "border-radius" => &[
            P::BorderTopLeftRadius,
            P::BorderTopRightRadius,
            P::BorderBottomRightRadius,
            P::BorderBottomLeftRadius,
        ],
        "border" => &[
            P::BorderTopWidth,
            P::BorderRightWidth,
            P::BorderBottomWidth,
            P::BorderLeftWidth,
            P::BorderTopStyle,
            P::BorderRightStyle,
            P::BorderBottomStyle,
            P::BorderLeftStyle,
            P::BorderTopColor,
            P::BorderRightColor,
            P::BorderBottomColor,
            P::BorderLeftColor,
        ],
        "border-top" => &[P::BorderTopWidth, P::BorderTopStyle, P::BorderTopColor],
        "border-right" => &[P::BorderRightWidth, P::BorderRightStyle, P::BorderRightColor],
        "border-bottom" => &[P::BorderBottomWidth, P::BorderBottomStyle, P::BorderBottomColor],
        "border-left" => &[P::BorderLeftWidth, P::BorderLeftStyle, P::BorderLeftColor],
        "border-inline" => &[
            P::BorderLeftWidth,
            P::BorderLeftStyle,
            P::BorderLeftColor,
            P::BorderRightWidth,
            P::BorderRightStyle,
            P::BorderRightColor,
        ],
        "border-block" => &[
            P::BorderTopWidth,
            P::BorderTopStyle,
            P::BorderTopColor,
            P::BorderBottomWidth,
            P::BorderBottomStyle,
            P::BorderBottomColor,
        ],
        "background" => &[
            P::BackgroundColor,
            P::BackgroundImage,
            P::BackgroundRepeat,
            P::BackgroundPosition,
            P::BackgroundSize,
        ],
        "font" => &[P::FontStyle, P::FontWeight, P::FontSize, P::LineHeight, P::FontFamily],
        "flex" => &[P::FlexGrow, P::FlexShrink, P::FlexBasis],
        "flex-flow" => &[P::FlexDirection, P::FlexWrap],
        "gap" => &[P::RowGap, P::ColumnGap],
        "list-style" => &[P::ListStyleType, P::ListStylePosition, P::ListStyleImage],
        "text-decoration" => &[P::TextDecorationLine, P::TextDecorationStyle, P::TextDecorationColor],
        "overflow" => &[P::OverflowX, P::OverflowY],
        "inset" => &[P::Top, P::Right, P::Bottom, P::Left],
        "outline" => &[P::OutlineWidth, P::OutlineStyle, P::OutlineColor],
        "margin-inline" => &[P::MarginLeft, P::MarginRight],
        "margin-block" => &[P::MarginTop, P::MarginBottom],
        "padding-inline" => &[P::PaddingLeft, P::PaddingRight],
        "padding-block" => &[P::PaddingTop, P::PaddingBottom],
        "inset-inline" => &[P::Left, P::Right],
        "inset-block" => &[P::Top, P::Bottom],
        "place-items" => &[P::AlignItems, P::JustifyItems],
        "place-content" => &[P::AlignContent, P::JustifyContent],
        "place-self" => &[P::AlignSelf, P::JustifySelf],
        "grid-column" => &[P::GridColumnStart, P::GridColumnEnd],
        "grid-row" => &[P::GridRowStart, P::GridRowEnd],
        "grid-area" => &[P::GridRowStart, P::GridColumnStart, P::GridRowEnd, P::GridColumnEnd],
        "grid-template" => &[P::GridTemplateRows, P::GridTemplateColumns, P::GridTemplateAreas],
        _ => return None,
    })
}

/// Expands 1-4 values into top/right/bottom/left.
fn four_sides<T: Clone>(v: Vec<T>) -> Option<[T; 4]> {
    Some(match v.len() {
        1 => [v[0].clone(), v[0].clone(), v[0].clone(), v[0].clone()],
        2 => [v[0].clone(), v[1].clone(), v[0].clone(), v[1].clone()],
        3 => [v[0].clone(), v[1].clone(), v[2].clone(), v[1].clone()],
        4 => [v[0].clone(), v[1].clone(), v[2].clone(), v[3].clone()],
        _ => return None,
    })
}

fn sides(
    ids: &[PropertyId; 4],
    values: &[ComponentValue],
    each: impl Fn(&ComponentValue) -> Option<SpecifiedValue>,
) -> Option<Vec<(PropertyId, SpecifiedValue)>> {
    let parsed: Option<Vec<SpecifiedValue>> = non_ws(values).into_iter().map(each).collect();
    let s = four_sides(parsed?)?;
    Some(ids.iter().copied().zip(s).collect())
}

fn two_sides(
    ids: [PropertyId; 2],
    values: &[ComponentValue],
    each: impl Fn(&ComponentValue) -> Option<SpecifiedValue>,
) -> Option<Vec<(PropertyId, SpecifiedValue)>> {
    let parsed: Option<Vec<SpecifiedValue>> = non_ws(values).into_iter().map(each).collect();
    let p = parsed?;
    let (a, b) = match p.len() {
        1 => (p[0].clone(), p[0].clone()),
        2 => (p[0].clone(), p[1].clone()),
        _ => return None,
    };
    Some(vec![(ids[0], a), (ids[1], b)])
}

fn margin_value(v: &ComponentValue) -> Option<SpecifiedValue> {
    if keyword_of(v, &["auto"]).is_some() {
        return Some(SpecifiedValue::Keyword("auto"));
    }
    length_value(v, false).map(SpecifiedValue::Length)
}

fn padding_value(v: &ComponentValue) -> Option<SpecifiedValue> {
    length_value(v, true).map(SpecifiedValue::Length)
}

/// Parses `<width> || <style> || <color>` for border/outline shorthands.
fn border_triplet(
    values: &[ComponentValue],
    outline: bool,
) -> Option<(SpecifiedValue, SpecifiedValue, SpecifiedValue)> {
    let mut width = None;
    let mut style = None;
    let mut color = None;
    let items = non_ws(values);
    if items.is_empty() {
        return None;
    }
    for x in items {
        if width.is_none()
            && let Some(w) = border_width_value(x)
        {
            width = Some(w);
            continue;
        }
        let styles: &[&'static str] = if outline {
            &[
                "none", "auto", "solid", "dashed", "dotted", "double", "groove", "ridge", "inset", "outset",
            ]
        } else {
            BORDER_STYLES
        };
        if style.is_none()
            && let Some(s) = keyword_of(x, styles)
        {
            style = Some(SpecifiedValue::Keyword(s));
            continue;
        }
        if color.is_none()
            && let Some(c) = color_value(x)
        {
            color = Some(c);
            continue;
        }
        return None;
    }
    Some((
        width.unwrap_or(SpecifiedValue::Keyword("medium")),
        style.unwrap_or(SpecifiedValue::Keyword("none")),
        color.unwrap_or(SpecifiedValue::CurrentColor),
    ))
}

/// Expands a shorthand into longhands.
pub fn expand_shorthand(name: &str, values: &[ComponentValue]) -> Option<Vec<(PropertyId, SpecifiedValue)>> {
    use PropertyId as P;
    use SpecifiedValue as V;
    let name = normalize_name(name);
    match name {
        "margin" => sides(
            &[P::MarginTop, P::MarginRight, P::MarginBottom, P::MarginLeft],
            values,
            margin_value,
        ),
        "padding" => sides(
            &[P::PaddingTop, P::PaddingRight, P::PaddingBottom, P::PaddingLeft],
            values,
            padding_value,
        ),
        "inset" => sides(&[P::Top, P::Right, P::Bottom, P::Left], values, margin_value),
        "margin-inline" => two_sides([P::MarginLeft, P::MarginRight], values, margin_value),
        "margin-block" => two_sides([P::MarginTop, P::MarginBottom], values, margin_value),
        "padding-inline" => two_sides([P::PaddingLeft, P::PaddingRight], values, padding_value),
        "padding-block" => two_sides([P::PaddingTop, P::PaddingBottom], values, padding_value),
        "inset-inline" => two_sides([P::Left, P::Right], values, margin_value),
        "inset-block" => two_sides([P::Top, P::Bottom], values, margin_value),
        "border-width" => sides(
            &[
                P::BorderTopWidth,
                P::BorderRightWidth,
                P::BorderBottomWidth,
                P::BorderLeftWidth,
            ],
            values,
            border_width_value,
        ),
        "border-style" => sides(
            &[
                P::BorderTopStyle,
                P::BorderRightStyle,
                P::BorderBottomStyle,
                P::BorderLeftStyle,
            ],
            values,
            |v| keyword_of(v, BORDER_STYLES).map(V::Keyword),
        ),
        "border-color" => sides(
            &[
                P::BorderTopColor,
                P::BorderRightColor,
                P::BorderBottomColor,
                P::BorderLeftColor,
            ],
            values,
            color_value,
        ),
        "border" | "border-top" | "border-right" | "border-bottom" | "border-left" | "border-inline"
        | "border-block" => {
            let (w, s, c) = border_triplet(values, false)?;
            let sides_idx: &[usize] = match name {
                "border" => &[0, 1, 2, 3],
                "border-top" => &[0],
                "border-right" => &[1],
                "border-bottom" => &[2],
                "border-left" => &[3],
                "border-inline" => &[1, 3],
                _ => &[0, 2],
            };
            let widths = [
                P::BorderTopWidth,
                P::BorderRightWidth,
                P::BorderBottomWidth,
                P::BorderLeftWidth,
            ];
            let styles = [
                P::BorderTopStyle,
                P::BorderRightStyle,
                P::BorderBottomStyle,
                P::BorderLeftStyle,
            ];
            let colors = [
                P::BorderTopColor,
                P::BorderRightColor,
                P::BorderBottomColor,
                P::BorderLeftColor,
            ];
            let mut out = Vec::new();
            for &i in sides_idx {
                out.push((widths[i], w.clone()));
                out.push((styles[i], s.clone()));
                out.push((colors[i], c.clone()));
            }
            Some(out)
        }
        "outline" => {
            let (w, s, c) = border_triplet(values, true)?;
            Some(vec![(P::OutlineWidth, w), (P::OutlineStyle, s), (P::OutlineColor, c)])
        }
        "border-radius" => {
            let all: Vec<&ComponentValue> = non_ws(values);
            let slash = all
                .iter()
                .position(|v| matches!(v, ComponentValue::Token(Token::Delim('/'))));
            let (h, vv) = match slash {
                Some(p) => (&all[..p], &all[p + 1..]),
                None => (&all[..], &all[..]),
            };
            let hl: Option<Vec<SpecifiedLength>> = h.iter().map(|x| length_value(x, true)).collect();
            let vl: Option<Vec<SpecifiedLength>> = vv.iter().map(|x| length_value(x, true)).collect();
            let hs = four_sides(hl?)?;
            let vs = four_sides(vl?)?;
            let ids = [
                P::BorderTopLeftRadius,
                P::BorderTopRightRadius,
                P::BorderBottomRightRadius,
                P::BorderBottomLeftRadius,
            ];
            Some(
                ids.iter()
                    .enumerate()
                    .map(|(i, &id)| {
                        (
                            id,
                            V::Pair(Box::new(V::Length(hs[i].clone())), Box::new(V::Length(vs[i].clone()))),
                        )
                    })
                    .collect(),
            )
        }
        "overflow" => {
            let v = non_ws(values);
            let o = |x: &ComponentValue| keyword_of(x, &["visible", "hidden", "clip", "scroll", "auto", "overlay"]);
            match v.len() {
                1 => {
                    let k = o(v[0])?;
                    Some(vec![(P::OverflowX, V::Keyword(k)), (P::OverflowY, V::Keyword(k))])
                }
                2 => Some(vec![
                    (P::OverflowX, V::Keyword(o(v[0])?)),
                    (P::OverflowY, V::Keyword(o(v[1])?)),
                ]),
                _ => None,
            }
        }
        "gap" => {
            let v = non_ws(values);
            let g = |x: &ComponentValue| -> Option<SpecifiedValue> {
                if keyword_of(x, &["normal"]).is_some() {
                    Some(V::Keyword("normal"))
                } else {
                    length_value(x, true).map(V::Length)
                }
            };
            match v.len() {
                1 => {
                    let a = g(v[0])?;
                    Some(vec![(P::RowGap, a.clone()), (P::ColumnGap, a)])
                }
                2 => Some(vec![(P::RowGap, g(v[0])?), (P::ColumnGap, g(v[1])?)]),
                _ => None,
            }
        }
        "flex-flow" => {
            let mut dir = V::Keyword("row");
            let mut wrap = V::Keyword("nowrap");
            for x in non_ws(values) {
                if let Some(k) = keyword_of(x, &["row", "row-reverse", "column", "column-reverse"]) {
                    dir = V::Keyword(k);
                } else {
                    let k = keyword_of(x, &["nowrap", "wrap", "wrap-reverse"])?;
                    wrap = V::Keyword(k);
                }
            }
            Some(vec![(P::FlexDirection, dir), (P::FlexWrap, wrap)])
        }
        "flex" => parse_flex_shorthand(values),
        "list-style" => {
            let mut ty = None;
            let mut pos = None;
            let mut img = None;
            let mut nones = 0;
            for x in non_ws(values) {
                if keyword_of(x, &["none"]).is_some() {
                    nones += 1;
                } else if let Some(k) = keyword_of(x, &["inside", "outside"]) {
                    pos = Some(V::Keyword(k));
                } else if let Some(u) = url_value(x) {
                    img = Some(V::Url(u));
                } else if let Some(k) = keyword_of(x, LIST_STYLE_TYPES) {
                    ty = Some(V::Keyword(k));
                } else if let ComponentValue::Token(Token::String(s)) = x {
                    ty = Some(V::Str(Rc::from(s.as_str())));
                } else {
                    return None;
                }
            }
            if nones > 0 {
                if ty.is_none() {
                    ty = Some(V::Keyword("none"));
                } else if img.is_none() {
                    img = Some(V::Keyword("none"));
                }
            }
            Some(vec![
                (P::ListStyleType, ty.unwrap_or(V::Keyword("disc"))),
                (P::ListStylePosition, pos.unwrap_or(V::Keyword("outside"))),
                (P::ListStyleImage, img.unwrap_or(V::Keyword("none"))),
            ])
        }
        "text-decoration" => {
            let mut lines = Vec::new();
            let mut style = V::Keyword("solid");
            let mut color = V::CurrentColor;
            for x in non_ws(values) {
                if keyword_of(x, &["underline", "overline", "line-through", "blink", "none"]).is_some() {
                    lines.push(x);
                } else if let Some(k) = keyword_of(x, &["solid", "double", "dotted", "dashed", "wavy"]) {
                    style = V::Keyword(k);
                } else if let Some(c) = color_value(x) {
                    color = c;
                } else if length_value(x, false).is_some() || keyword_of(x, &["auto", "from-font"]).is_some() {
                    // text-decoration-thickness: ignored
                } else {
                    return None;
                }
            }
            let line = if lines.is_empty() {
                V::DecorationLine(false, false, false)
            } else {
                parse_decoration_line(&lines)?
            };
            Some(vec![
                (P::TextDecorationLine, line),
                (P::TextDecorationStyle, style),
                (P::TextDecorationColor, color),
            ])
        }
        "background" => parse_background_shorthand(values),
        "font" => parse_font_shorthand(values),
        "place-items" | "place-self" => {
            let it = non_ws(values);
            let (align_id, justify_id, allow_auto) = if name == "place-items" {
                (P::AlignItems, P::JustifyItems, false)
            } else {
                (P::AlignSelf, P::JustifySelf, true)
            };
            let a = parse_self_alignment(std::slice::from_ref(*it.first()?), allow_auto)?;
            let j_src = it.get(1).copied().unwrap_or(it[0]);
            let j = parse_longhand(justify_id, std::slice::from_ref(j_src))?;
            Some(vec![(align_id, a), (justify_id, j)])
        }
        "grid-column" | "grid-row" => {
            let parts = crate::style::grid::split_slash(values);
            let (start_id, end_id) = if name == "grid-column" {
                (P::GridColumnStart, P::GridColumnEnd)
            } else {
                (P::GridRowStart, P::GridRowEnd)
            };
            let start = parts.first()?;
            crate::style::grid::parse_grid_line(start)?;
            let end: Rc<[ComponentValue]> = match parts.get(1) {
                Some(e) => {
                    crate::style::grid::parse_grid_line(e)?;
                    Rc::from(*e)
                }
                None => grid_default_end(start),
            };
            Some(vec![(start_id, V::Tokens(Rc::from(*start))), (end_id, V::Tokens(end))])
        }
        "grid-area" => {
            let parts = crate::style::grid::split_slash(values);
            if parts.is_empty() || parts.len() > 4 {
                return None;
            }
            for p in &parts {
                crate::style::grid::parse_grid_line(p)?;
            }
            let row_start: Rc<[ComponentValue]> = Rc::from(parts[0]);
            let col_start: Rc<[ComponentValue]> = parts
                .get(1)
                .map(|p| Rc::from(*p))
                .unwrap_or_else(|| grid_default_end(parts[0]));
            let row_end: Rc<[ComponentValue]> = parts
                .get(2)
                .map(|p| Rc::from(*p))
                .unwrap_or_else(|| grid_default_end(parts[0]));
            let col_end: Rc<[ComponentValue]> = parts
                .get(3)
                .map(|p| Rc::from(*p))
                .unwrap_or_else(|| grid_default_end(&col_start));
            Some(vec![
                (P::GridRowStart, V::Tokens(row_start)),
                (P::GridColumnStart, V::Tokens(col_start)),
                (P::GridRowEnd, V::Tokens(row_end)),
                (P::GridColumnEnd, V::Tokens(col_end)),
            ])
        }
        "grid-template" => {
            let it = non_ws(values);
            if it.len() == 1 && keyword_of(it[0], &["none"]).is_some() {
                let none: Rc<[ComponentValue]> = Rc::from(values);
                return Some(vec![
                    (P::GridTemplateRows, V::Tokens(none.clone())),
                    (P::GridTemplateColumns, V::Tokens(none.clone())),
                    (P::GridTemplateAreas, V::Tokens(none)),
                ]);
            }
            let parts = crate::style::grid::split_slash(values);
            if parts.len() != 2 {
                return None;
            }
            crate::style::grid::parse_track_list(parts[0], &DUMMY_LENGTHS)?;
            crate::style::grid::parse_track_list(parts[1], &DUMMY_LENGTHS)?;
            let none: Rc<[ComponentValue]> = Rc::from(vec![ComponentValue::Token(Token::Ident("none".into()))]);
            Some(vec![
                (P::GridTemplateRows, V::Tokens(Rc::from(parts[0]))),
                (P::GridTemplateColumns, V::Tokens(Rc::from(parts[1]))),
                (P::GridTemplateAreas, V::Tokens(none)),
            ])
        }
        "place-content" => {
            let v = non_ws(values);
            let a = parse_content_distribution(std::slice::from_ref(v.first()?))?;
            let b = match v.get(1) {
                Some(x) => parse_content_distribution(std::slice::from_ref(*x))?,
                None => a.clone(),
            };
            Some(vec![(P::AlignContent, a), (P::JustifyContent, b)])
        }
        _ => None,
    }
}

fn parse_flex_shorthand(values: &[ComponentValue]) -> Option<Vec<(PropertyId, SpecifiedValue)>> {
    use PropertyId as P;
    use SpecifiedValue as V;
    let v = non_ws(values);
    let out = |g: f32, s: f32, b: SpecifiedValue| {
        Some(vec![
            (P::FlexGrow, V::Number(g)),
            (P::FlexShrink, V::Number(s)),
            (P::FlexBasis, b),
        ])
    };
    let zero_pct = || V::Length(SpecifiedLength::Linear(crate::css::values::LinearLength::pct(0.0)));
    if v.len() == 1
        && let Some(k) = keyword_of(v[0], &["none", "auto", "initial"])
    {
        return match k {
            "none" => out(0.0, 0.0, V::Keyword("auto")),
            "auto" => out(1.0, 1.0, V::Keyword("auto")),
            _ => out(0.0, 1.0, V::Keyword("auto")),
        };
    }
    let mut nums = Vec::new();
    let mut basis = None;
    for x in &v {
        if let Some(n) = parse_number(x) {
            if nums.len() == 2 && n == 0.0 && basis.is_none() {
                // A unitless zero after grow and shrink is the basis.
                basis = Some(V::Length(SpecifiedLength::px(0.0)));
                continue;
            }
            if basis.is_some() && nums.len() == 2 {
                return None;
            }
            nums.push(n);
        } else if let Some(k) = keyword_of(x, &["auto", "content"]) {
            basis = Some(V::Keyword(k));
        } else {
            let l = length_value(x, true)?;
            basis = Some(V::Length(l));
        }
    }
    if nums.len() > 2 || (nums.is_empty() && basis.is_none()) {
        return None;
    }
    let g = nums.first().copied().unwrap_or(1.0);
    let s = nums.get(1).copied().unwrap_or(1.0);
    out(g, s, basis.unwrap_or_else(zero_pct))
}

fn parse_background_shorthand(values: &[ComponentValue]) -> Option<Vec<(PropertyId, SpecifiedValue)>> {
    use PropertyId as P;
    use SpecifiedValue as V;
    let layers: Vec<&[ComponentValue]> = values
        .split(|v| matches!(v, ComponentValue::Token(Token::Comma)))
        .collect();
    let mut color = V::Color(Color::TRANSPARENT);
    let mut image = V::Keyword("none");
    let mut repeat = V::Keyword("repeat");
    let mut position: Option<SpecifiedValue> = None;
    let mut size = V::Keyword("auto");
    for (li, layer) in layers.iter().enumerate() {
        let items = non_ws(layer);
        let mut pos_items: Vec<&ComponentValue> = Vec::new();
        let mut size_items: Vec<&ComponentValue> = Vec::new();
        let mut after_slash = false;
        let mut repeats: Vec<&'static str> = Vec::new();
        for x in items {
            if matches!(x, ComponentValue::Token(Token::Delim('/'))) {
                after_slash = true;
                continue;
            }
            if after_slash {
                size_items.push(x);
                continue;
            }
            if let Some(u) = url_value(x) {
                if li == 0 {
                    image = V::Url(u);
                }
            } else if keyword_of(x, &["none"]).is_some() {
            } else if let Some(k) = keyword_of(x, &["repeat", "repeat-x", "repeat-y", "no-repeat", "space", "round"]) {
                repeats.push(k);
            } else if keyword_of(
                x,
                &[
                    "scroll",
                    "fixed",
                    "local",
                    "border-box",
                    "padding-box",
                    "content-box",
                    "text",
                ],
            )
            .is_some()
            {
            } else if keyword_of(x, &["left", "right", "top", "bottom", "center"]).is_some()
                || length_value(x, false).is_some()
            {
                pos_items.push(x);
            } else if let Some(c) = color_value(x) {
                if li == layers.len() - 1 {
                    color = c;
                }
            } else if crate::css::gradient::is_gradient(x) {
                if li == 0 && !matches!(image, V::Url(_)) {
                    image = V::Tokens(Rc::from(vec![x.clone()]));
                }
            } else if matches!(x, ComponentValue::Function { .. }) {
                // Other image functions (image-set(), cross-fade()) are not supported.
            } else {
                return None;
            }
        }
        if li == 0 {
            if !repeats.is_empty() {
                let r: Vec<ComponentValue> = repeats
                    .iter()
                    .map(|k| ComponentValue::Token(Token::Ident(k.to_string())))
                    .flat_map(|t| [t, ComponentValue::Token(Token::Whitespace)])
                    .collect();
                repeat = parse_longhand(P::BackgroundRepeat, &r)?;
            }
            if !pos_items.is_empty() {
                position = Some(parse_position(&pos_items)?);
            }
            if !size_items.is_empty() {
                size = parse_bg_size(&size_items)?;
            }
        }
    }
    let position = position.unwrap_or_else(|| {
        let z = || V::Length(SpecifiedLength::px(0.0));
        V::Pair(Box::new(z()), Box::new(z()))
    });
    Some(vec![
        (P::BackgroundColor, color),
        (P::BackgroundImage, image),
        (P::BackgroundRepeat, repeat),
        (P::BackgroundPosition, position),
        (P::BackgroundSize, size),
    ])
}

fn parse_font_shorthand(values: &[ComponentValue]) -> Option<Vec<(PropertyId, SpecifiedValue)>> {
    use PropertyId as P;
    use SpecifiedValue as V;
    let items: Vec<&ComponentValue> = non_ws(values);
    if items.len() == 1
        && keyword_of(
            items[0],
            &["caption", "icon", "menu", "message-box", "small-caption", "status-bar"],
        )
        .is_some()
    {
        return Some(vec![
            (P::FontStyle, V::Keyword("normal")),
            (P::FontWeight, V::Keyword("normal")),
            (P::FontSize, V::Length(SpecifiedLength::px(13.0))),
            (P::LineHeight, V::Keyword("normal")),
            (P::FontFamily, V::FontFamily(Rc::from(vec!["sans-serif".to_string()]))),
        ]);
    }
    let mut style = V::Keyword("normal");
    let mut weight = V::Keyword("normal");
    let mut i = 0;
    // Optional style / variant / weight / stretch, in any order.
    while i < items.len() {
        let x = items[i];
        if let Some(k) = keyword_of(x, &["italic", "oblique"]) {
            style = V::Keyword(k);
        } else if let Some(k) = keyword_of(x, &["bold", "bolder", "lighter"]) {
            weight = V::Keyword(k);
        } else if keyword_of(
            x,
            &[
                "normal",
                "small-caps",
                "condensed",
                "expanded",
                "semi-condensed",
                "semi-expanded",
                "ultra-condensed",
                "extra-condensed",
                "extra-expanded",
                "ultra-expanded",
            ],
        )
        .is_some()
        {
        } else if let ComponentValue::Token(Token::Number { value, .. }) = x {
            weight = V::Number(*value);
        } else {
            break;
        }
        i += 1;
    }
    // Size [/ line-height]
    let size_cv = items.get(i)?;
    let size = parse_longhand(P::FontSize, std::slice::from_ref(*size_cv))?;
    i += 1;
    let mut line_height = V::Keyword("normal");
    if matches!(items.get(i), Some(ComponentValue::Token(Token::Delim('/')))) {
        line_height = parse_longhand(P::LineHeight, std::slice::from_ref(*items.get(i + 1)?))?;
        i += 2;
    }
    // Family: the rest of the original values after the size/line-height.
    let rest_start = {
        // Find the position in `values` of items[i].
        let target = *items.get(i)?;
        values.iter().position(|v| std::ptr::eq(v, target))?
    };
    let family = parse_font_family(&values[rest_start..])?;
    Some(vec![
        (P::FontStyle, style),
        (P::FontWeight, weight),
        (P::FontSize, size),
        (P::LineHeight, line_height),
        (P::FontFamily, family),
    ])
}

/// Extracts the translation of a transform list: `translate()`, `translateX/Y()`,
/// `translate3d()` and the translation part of `matrix()`. Rotation, scaling and
/// skewing are not supported and are dropped.
fn parse_translation(values: &[ComponentValue], ctx: &LengthContext) -> Transform2d {
    let mut x = LengthPercentage::px(0.0);
    let mut y = LengthPercentage::px(0.0);
    let mut other = false;
    let add = |a: &LengthPercentage, b: &LengthPercentage| -> LengthPercentage {
        match (a, b) {
            (LengthPercentage::Lin(p), LengthPercentage::Lin(q)) => LengthPercentage::Lin(crate::css::values::Lin {
                px: p.px + q.px,
                pct: p.pct + q.pct,
            }),
            (_, q) => q.clone(),
        }
    };
    for v in values {
        let ComponentValue::Function { name, args } = v else {
            continue;
        };
        let parts: Vec<Vec<ComponentValue>> = args
            .split(|a| matches!(a, ComponentValue::Token(Token::Comma)))
            .map(|p| p.iter().filter(|c| !c.is_whitespace()).cloned().collect())
            .collect();
        let len = |i: usize| -> Option<LengthPercentage> {
            parts
                .get(i)
                .and_then(|p| p.first())
                .and_then(parse_length)
                .map(|l| l.compute(ctx))
        };
        match name.to_ascii_lowercase().as_str() {
            "translate" | "translate3d" => {
                if let Some(tx) = len(0) {
                    x = add(&x, &tx);
                }
                if let Some(ty) = len(1) {
                    y = add(&y, &ty);
                }
            }
            "translatex" => {
                if let Some(tx) = len(0) {
                    x = add(&x, &tx);
                }
            }
            "translatey" => {
                if let Some(ty) = len(0) {
                    y = add(&y, &ty);
                }
            }
            "matrix" => {
                let nums: Vec<f32> = parts.iter().filter_map(|p| p.first().and_then(parse_number)).collect();
                if nums.len() == 6 {
                    x = add(&x, &LengthPercentage::px(nums[4]));
                    y = add(&y, &LengthPercentage::px(nums[5]));
                    other |= (nums[0] - 1.0).abs() > 1e-3
                        || nums[1].abs() > 1e-3
                        || nums[2].abs() > 1e-3
                        || (nums[3] - 1.0).abs() > 1e-3;
                }
            }
            "translatez" | "perspective" => {}
            "scale" | "scalex" | "scaley" | "scale3d" => {
                other |= parts
                    .iter()
                    .filter_map(|p| p.first().and_then(parse_number))
                    .any(|n| (n - 1.0).abs() > 1e-3);
            }
            _ => other = true,
        }
    }
    Transform2d { x, y, other }
}

/// Parses `none | [<custom-ident> <integer>?]+` with `default` as the implied value.
fn parse_counter_list(values: &[ComponentValue], default: i32) -> Option<Vec<(String, i32)>> {
    let v = non_ws(values);
    if v.len() == 1 && keyword_of(v[0], &["none"]).is_some() {
        return Some(Vec::new());
    }
    let mut out: Vec<(String, i32)> = Vec::new();
    for x in v {
        match x {
            ComponentValue::Token(Token::Ident(name)) => out.push((name.clone(), default)),
            ComponentValue::Token(Token::Number { value, .. }) => out.last_mut()?.1 = *value as i32,
            _ => return None,
        }
    }
    (!out.is_empty()).then_some(out)
}

/// Parses the list style argument of `counter()`/`counters()`.
fn counter_style(v: Option<&ComponentValue>) -> ListStyleType {
    match v.and_then(|x| x.ident()).map(|s| s.to_ascii_lowercase()).as_deref() {
        Some("none") => ListStyleType::None,
        Some("disc") => ListStyleType::Disc,
        Some("circle") => ListStyleType::Circle,
        Some("square") => ListStyleType::Square,
        Some("decimal-leading-zero") => ListStyleType::DecimalLeadingZero,
        Some("lower-alpha") | Some("lower-latin") => ListStyleType::LowerAlpha,
        Some("upper-alpha") | Some("upper-latin") => ListStyleType::UpperAlpha,
        Some("lower-roman") => ListStyleType::LowerRoman,
        Some("upper-roman") => ListStyleType::UpperRoman,
        Some("lower-greek") => ListStyleType::LowerGreek,
        _ => ListStyleType::Decimal,
    }
}

/// A length context used only to validate grid track lists at parse time.
const DUMMY_LENGTHS: LengthContext = LengthContext {
    font_size: 16.0,
    root_font_size: 16.0,
    viewport_width: 1000.0,
    viewport_height: 1000.0,
};

/// The omitted end line of `grid-column`/`grid-row`/`grid-area`: the same name for a
/// named start line, otherwise `auto`.
fn grid_default_end(start: &[ComponentValue]) -> Rc<[ComponentValue]> {
    match crate::style::grid::parse_grid_line(start) {
        Some(crate::style::grid::GridLine::Named(_)) => Rc::from(start),
        _ => Rc::from(vec![ComponentValue::Token(Token::Ident("auto".into()))]),
    }
}

// ---------------------------------------------------------------- var()

/// Substitutes `var()` references in `css` using `customs`. Returns `None` if a
/// referenced property is missing and has no fallback.
pub fn substitute_vars(css: &str, customs: Option<&HashMap<String, Rc<str>>>, depth: u32) -> Option<String> {
    if depth > 16 {
        return None;
    }
    let values = parse_component_values(css);
    let mut out = String::new();
    substitute_into(&values, customs, depth, &mut out)?;
    Some(out)
}

fn substitute_into(
    values: &[ComponentValue],
    customs: Option<&HashMap<String, Rc<str>>>,
    depth: u32,
    out: &mut String,
) -> Option<()> {
    for v in values {
        match v {
            ComponentValue::Function { name, args } if name.eq_ignore_ascii_case("var") => {
                let mut parts = args.splitn(2, |a| matches!(a, ComponentValue::Token(Token::Comma)));
                let name_part = parts.next()?;
                let fallback = parts.next();
                let var_name = non_ws(name_part).first()?.ident()?.to_string();
                match customs.and_then(|c| c.get(&var_name)) {
                    Some(val) if !val.trim().is_empty() || fallback.is_none() => {
                        out.push_str(substitute_vars(val, customs, depth + 1)?.trim());
                    }
                    _ => {
                        let fb = fallback?;
                        out.push_str(substitute_vars(&to_css(fb), customs, depth + 1)?.trim());
                    }
                }
            }
            ComponentValue::Function { name, args } => {
                out.push_str(name);
                out.push('(');
                substitute_into(args, customs, depth, out)?;
                out.push(')');
            }
            ComponentValue::Block { open, contents } => {
                out.push(*open);
                substitute_into(contents, customs, depth, out)?;
                out.push(match open {
                    '{' => '}',
                    '[' => ']',
                    _ => ')',
                });
            }
            other => other.to_css(out),
        }
    }
    Some(())
}

/// Resolves a deferred (`var()`-containing) value for `id` at computed-value time.
pub fn resolve_unparsed(
    id: PropertyId,
    css: &str,
    shorthand: Option<&str>,
    customs: Option<&HashMap<String, Rc<str>>>,
) -> Option<SpecifiedValue> {
    let text = substitute_vars(css, customs, 0)?;
    let values = parse_component_values(&text);
    if let Some(w) = css_wide(&values) {
        return Some(w);
    }
    match shorthand {
        None => parse_longhand(id, &values),
        Some(sh) => expand_shorthand(sh, &values)?
            .into_iter()
            .find(|(i, _)| *i == id)
            .map(|(_, v)| v),
    }
}

// ---------------------------------------------------------------- computation

/// Context for computing values of one element.
pub struct ComputeContext<'a> {
    /// The parent's computed style (initial style for the root).
    pub parent: &'a ComputedStyle,
    /// Root element font size (px), for `rem`.
    pub root_font_size: f32,
    /// Viewport width (px).
    pub viewport_width: f32,
    /// Viewport height (px).
    pub viewport_height: f32,
}

impl ComputeContext<'_> {
    fn lengths(&self, font_size: f32) -> LengthContext {
        LengthContext {
            font_size,
            root_font_size: self.root_font_size,
            viewport_width: self.viewport_width,
            viewport_height: self.viewport_height,
        }
    }
}

fn is_mono_only(f: &[String]) -> bool {
    f.len() == 1 && f[0] == "monospace"
}

/// Applies one specified value to `s`. `s` must already hold inherited/initial values,
/// and `font-size`/`color` must have been applied before other properties.
pub fn apply_property(id: PropertyId, value: &SpecifiedValue, s: &mut ComputedStyle, ctx: &ComputeContext) {
    use PropertyId as P;
    use SpecifiedValue as V;
    match value {
        V::Inherit => return copy_property(id, s, ctx.parent),
        V::Initial => return copy_property(id, s, &ComputedStyle::initial()),
        V::Unset => {
            if id.inherited() {
                return copy_property(id, s, ctx.parent);
            }
            return copy_property(id, s, &ComputedStyle::initial());
        }
        V::Unparsed { css, shorthand } => {
            let resolved =
                resolve_unparsed(id, css, *shorthand, s.custom_properties.as_deref()).unwrap_or(SpecifiedValue::Unset);
            if !matches!(resolved, V::Unparsed { .. }) {
                apply_property(id, &resolved, s, ctx);
            }
            return;
        }
        _ => {}
    }
    let lctx = ctx.lengths(s.font_size);
    let len = |v: &SpecifiedValue| -> LengthPercentage {
        match v {
            V::Length(l) => l.compute(&lctx),
            V::Number(n) => LengthPercentage::px(*n),
            _ => LengthPercentage::px(0.0),
        }
    };
    let px = |v: &SpecifiedValue| len(v).resolve(0.0);
    let color = |v: &SpecifiedValue, current: Color| match v {
        V::Color(c) => *c,
        _ => current,
    };
    let kwd = |v: &SpecifiedValue| match v {
        V::Keyword(k) => *k,
        _ => "",
    };
    let size = |v: &SpecifiedValue| -> Size {
        match v {
            V::Length(_) => Size::Length(len(v)),
            V::Keyword("min-content") => Size::MinContent,
            V::Keyword("max-content") => Size::MaxContent,
            V::Keyword("fit-content") | V::Keyword("-webkit-fill-available") | V::Keyword("stretch") => {
                if matches!(v, V::Keyword("fit-content")) {
                    Size::FitContent
                } else {
                    Size::Auto
                }
            }
            _ => Size::Auto,
        }
    };
    let len_or_auto = |v: &SpecifiedValue| -> LengthOrAuto {
        match v {
            V::Length(_) => LengthOrAuto::Length(len(v)),
            _ => LengthOrAuto::Auto,
        }
    };
    let border_width = |v: &SpecifiedValue| -> f32 {
        let w = match v {
            V::Keyword("thin") => 1.0,
            V::Keyword("medium") => 3.0,
            V::Keyword("thick") => 5.0,
            _ => px(v),
        };
        // Chrome floors border widths >= 1px to whole device pixels.
        if w >= 1.0 {
            w.floor()
        } else if w > 0.0 {
            1.0
        } else {
            0.0
        }
    };
    let border_style = |v: &SpecifiedValue| match kwd(v) {
        "hidden" => BorderStyle::Hidden,
        "solid" => BorderStyle::Solid,
        "dashed" => BorderStyle::Dashed,
        "dotted" => BorderStyle::Dotted,
        "double" => BorderStyle::Double,
        "groove" => BorderStyle::Groove,
        "ridge" => BorderStyle::Ridge,
        "inset" => BorderStyle::Inset,
        "outset" => BorderStyle::Outset,
        "auto" => BorderStyle::Solid,
        _ => BorderStyle::None,
    };
    let radius = |v: &SpecifiedValue| match v {
        V::Pair(a, b) => (len(a), len(b)),
        _ => (LengthPercentage::px(0.0), LengthPercentage::px(0.0)),
    };
    let align = |v: &SpecifiedValue| match kwd(v) {
        "auto" => AlignItems::Auto,
        "stretch" => AlignItems::Stretch,
        "flex-start" | "start" | "self-start" | "left" => AlignItems::FlexStart,
        "flex-end" | "end" | "self-end" | "right" => AlignItems::FlexEnd,
        "center" => AlignItems::Center,
        "baseline" => AlignItems::Baseline,
        _ => AlignItems::Normal,
    };
    let distribution = |v: &SpecifiedValue| match kwd(v) {
        "flex-start" | "start" | "left" => JustifyContent::FlexStart,
        "flex-end" | "end" | "right" => JustifyContent::FlexEnd,
        "center" => JustifyContent::Center,
        "space-between" => JustifyContent::SpaceBetween,
        "space-around" => JustifyContent::SpaceAround,
        "space-evenly" => JustifyContent::SpaceEvenly,
        "stretch" => JustifyContent::Stretch,
        _ => JustifyContent::Normal,
    };
    let overflow = |v: &SpecifiedValue| match kwd(v) {
        "hidden" => Overflow::Hidden,
        "clip" => Overflow::Clip,
        "scroll" => Overflow::Scroll,
        "auto" | "overlay" => Overflow::Auto,
        _ => Overflow::Visible,
    };

    match id {
        P::Color => s.color = color(value, ctx.parent.color),
        P::FontFamily => {
            if let V::FontFamily(f) = value {
                s.font_family = f.clone();
            }
        }
        P::FontSize => {
            // Monospace quirk: generic `monospace` has a 13px default instead of 16px.
            let factor = match (is_mono_only(&s.font_family), is_mono_only(&ctx.parent.font_family)) {
                (true, false) => 13.0 / 16.0,
                (false, true) => 16.0 / 13.0,
                _ => 1.0,
            };
            let base = ctx.parent.font_size * factor;
            let medium = 16.0 * if is_mono_only(&s.font_family) { 13.0 / 16.0 } else { 1.0 };
            s.font_size = match value {
                V::Keyword(k) => match *k {
                    "xx-small" => medium * 9.0 / 16.0,
                    "x-small" => medium * 10.0 / 16.0,
                    "small" => medium * 13.0 / 16.0,
                    "medium" => medium,
                    "large" => medium * 18.0 / 16.0,
                    "x-large" => medium * 24.0 / 16.0,
                    "xx-large" => medium * 2.0,
                    "xxx-large" => medium * 3.0,
                    "smaller" => base / 1.2,
                    "larger" => base * 1.2,
                    _ => base,
                },
                V::Length(l) => {
                    let lc = ctx.lengths(base);
                    l.compute(&lc).resolve(base)
                }
                _ => base,
            }
            .max(0.0);
        }
        P::FontWeight => {
            let pw = ctx.parent.font_weight;
            s.font_weight = match value {
                V::Keyword("bold") => 700,
                V::Keyword("bolder") => {
                    if pw < 350 {
                        400
                    } else if pw < 550 {
                        700
                    } else {
                        900
                    }
                }
                V::Keyword("lighter") => {
                    if pw < 550 {
                        100
                    } else if pw < 750 {
                        400
                    } else {
                        700
                    }
                }
                V::Number(n) => n.round().clamp(1.0, 1000.0) as u16,
                _ => 400,
            }
        }
        P::FontStyle => {
            s.font_style = match kwd(value) {
                "italic" => FontStyle::Italic,
                "oblique" => FontStyle::Oblique,
                _ => FontStyle::Normal,
            }
        }
        P::LineHeight => {
            s.line_height = match value {
                V::Number(n) => LineHeight::Number(*n),
                V::Length(_) => LineHeight::Px(len(value).resolve(s.font_size)),
                _ => LineHeight::Normal,
            }
        }
        P::TextAlign => {
            s.text_align = match kwd(value) {
                "right" | "end" | "-webkit-right" => TextAlign::Right,
                "center" => TextAlign::Center,
                "justify" => TextAlign::Justify,
                "-webkit-center" | "-moz-center" => TextAlign::WebkitCenter,
                "match-parent" => ctx.parent.text_align,
                _ => TextAlign::Left,
            }
        }
        P::TextIndent => s.text_indent = len(value),
        P::TextTransform => {
            s.text_transform = match kwd(value) {
                "uppercase" => TextTransform::Uppercase,
                "lowercase" => TextTransform::Lowercase,
                "capitalize" => TextTransform::Capitalize,
                _ => TextTransform::None,
            }
        }
        P::WhiteSpace => {
            s.white_space = match kwd(value) {
                "pre" => WhiteSpace::Pre,
                "nowrap" => WhiteSpace::Nowrap,
                "pre-wrap" => WhiteSpace::PreWrap,
                "pre-line" => WhiteSpace::PreLine,
                "break-spaces" => WhiteSpace::BreakSpaces,
                _ => WhiteSpace::Normal,
            }
        }
        P::LetterSpacing => s.letter_spacing = if matches!(value, V::Length(_)) { px(value) } else { 0.0 },
        P::WordSpacing => s.word_spacing = if matches!(value, V::Length(_)) { px(value) } else { 0.0 },
        P::ListStyleType => {
            s.list_style_type = match value {
                V::Str(t) => ListStyleType::String(t.to_string()),
                _ => match kwd(value) {
                    "none" => ListStyleType::None,
                    "circle" => ListStyleType::Circle,
                    "square" => ListStyleType::Square,
                    "decimal" => ListStyleType::Decimal,
                    "decimal-leading-zero" => ListStyleType::DecimalLeadingZero,
                    "lower-alpha" | "lower-latin" => ListStyleType::LowerAlpha,
                    "upper-alpha" | "upper-latin" => ListStyleType::UpperAlpha,
                    "lower-roman" => ListStyleType::LowerRoman,
                    "upper-roman" => ListStyleType::UpperRoman,
                    "lower-greek" => ListStyleType::LowerGreek,
                    "disclosure-open" => ListStyleType::String("▾ ".to_string()),
                    "disclosure-closed" => ListStyleType::String("▸ ".to_string()),
                    _ => ListStyleType::Disc,
                },
            }
        }
        P::ListStylePosition => {
            s.list_style_position = if kwd(value) == "inside" {
                ListStylePosition::Inside
            } else {
                ListStylePosition::Outside
            }
        }
        P::ListStyleImage => {
            s.list_style_image = match value {
                V::Url(u) => Some(u.to_string()),
                _ => None,
            }
        }
        P::Visibility => {
            s.visibility = match kwd(value) {
                "hidden" => Visibility::Hidden,
                "collapse" => Visibility::Collapse,
                _ => Visibility::Visible,
            }
        }
        P::BorderCollapse => {
            s.border_collapse = if kwd(value) == "collapse" {
                BorderCollapse::Collapse
            } else {
                BorderCollapse::Separate
            }
        }
        P::BorderSpacing => {
            if let V::Pair(a, b) = value {
                s.border_spacing = (px(a), px(b));
            }
        }
        P::Cursor => {
            s.cursor = match kwd(value) {
                "pointer" => Cursor::Pointer,
                "text" => Cursor::Text,
                "default" => Cursor::Default,
                _ => Cursor::Auto,
            }
        }
        P::Display => {
            s.display = match kwd(value) {
                "none" => Display::None,
                "contents" => Display::Contents,
                "block" => Display::Block,
                "inline-block" => Display::InlineBlock,
                "flow-root" => Display::FlowRoot,
                "list-item" => Display::ListItem,
                "flex" => Display::Flex,
                "inline-flex" => Display::InlineFlex,
                "grid" => Display::Grid,
                "inline-grid" => Display::InlineGrid,
                "table" => Display::Table,
                "inline-table" => Display::InlineTable,
                "table-row-group" => Display::TableRowGroup,
                "table-header-group" => Display::TableHeaderGroup,
                "table-footer-group" => Display::TableFooterGroup,
                "table-row" => Display::TableRow,
                "table-cell" => Display::TableCell,
                "table-column" => Display::TableColumn,
                "table-column-group" => Display::TableColumnGroup,
                "table-caption" => Display::TableCaption,
                _ => Display::Inline,
            }
        }
        P::Position => {
            s.position = match kwd(value) {
                "relative" => Position::Relative,
                "absolute" => Position::Absolute,
                "fixed" => Position::Fixed,
                "sticky" | "-webkit-sticky" => Position::Sticky,
                _ => Position::Static,
            }
        }
        P::Float => {
            s.float = match kwd(value) {
                "left" | "inline-start" => Float::Left,
                "right" | "inline-end" => Float::Right,
                _ => Float::None,
            }
        }
        P::Clear => {
            s.clear = match kwd(value) {
                "left" | "inline-start" => Clear::Left,
                "right" | "inline-end" => Clear::Right,
                "both" => Clear::Both,
                _ => Clear::None,
            }
        }
        P::Top => s.inset[0] = len_or_auto(value),
        P::Right => s.inset[1] = len_or_auto(value),
        P::Bottom => s.inset[2] = len_or_auto(value),
        P::Left => s.inset[3] = len_or_auto(value),
        P::Width => s.width = size(value),
        P::Height => s.height = size(value),
        P::MinWidth => s.min_width = size(value),
        P::MinHeight => s.min_height = size(value),
        P::MaxWidth => s.max_width = size(value),
        P::MaxHeight => s.max_height = size(value),
        P::MarginTop => s.margin[0] = len_or_auto(value),
        P::MarginRight => s.margin[1] = len_or_auto(value),
        P::MarginBottom => s.margin[2] = len_or_auto(value),
        P::MarginLeft => s.margin[3] = len_or_auto(value),
        P::PaddingTop => s.padding[0] = len(value),
        P::PaddingRight => s.padding[1] = len(value),
        P::PaddingBottom => s.padding[2] = len(value),
        P::PaddingLeft => s.padding[3] = len(value),
        P::BorderTopWidth => s.border_width[0] = border_width(value),
        P::BorderRightWidth => s.border_width[1] = border_width(value),
        P::BorderBottomWidth => s.border_width[2] = border_width(value),
        P::BorderLeftWidth => s.border_width[3] = border_width(value),
        P::BorderTopStyle => s.border_style[0] = border_style(value),
        P::BorderRightStyle => s.border_style[1] = border_style(value),
        P::BorderBottomStyle => s.border_style[2] = border_style(value),
        P::BorderLeftStyle => s.border_style[3] = border_style(value),
        P::BorderTopColor => s.border_color[0] = color(value, s.color),
        P::BorderRightColor => s.border_color[1] = color(value, s.color),
        P::BorderBottomColor => s.border_color[2] = color(value, s.color),
        P::BorderLeftColor => s.border_color[3] = color(value, s.color),
        P::BorderTopLeftRadius => s.border_radius[0] = radius(value),
        P::BorderTopRightRadius => s.border_radius[1] = radius(value),
        P::BorderBottomRightRadius => s.border_radius[2] = radius(value),
        P::BorderBottomLeftRadius => s.border_radius[3] = radius(value),
        P::BoxSizing => {
            s.box_sizing = if kwd(value) == "border-box" {
                BoxSizing::BorderBox
            } else {
                BoxSizing::ContentBox
            }
        }
        P::BackgroundColor => s.background_color = color(value, s.color),
        P::BackgroundImage => {
            s.background_image = match value {
                V::Url(u) => Some(u.to_string()),
                _ => None,
            };
            s.background_gradient = match value {
                V::Tokens(t) => t
                    .first()
                    .and_then(|g| crate::css::gradient::parse_gradient(g, &lctx, s.color))
                    .map(Rc::new),
                _ => None,
            };
        }
        P::BackgroundRepeat => {
            s.background_repeat = match kwd(value) {
                "repeat-x" => BackgroundRepeat::RepeatX,
                "repeat-y" => BackgroundRepeat::RepeatY,
                "no-repeat" => BackgroundRepeat::NoRepeat,
                _ => BackgroundRepeat::Repeat,
            }
        }
        P::BackgroundPosition => {
            if let V::Pair(a, b) = value {
                s.background_position = (len(a), len(b));
            }
        }
        P::BackgroundSize => {
            s.background_size = match value {
                V::Keyword("cover") => BackgroundSize::Cover,
                V::Keyword("contain") => BackgroundSize::Contain,
                V::Pair(a, b) => BackgroundSize::Explicit(len_or_auto(a), len_or_auto(b)),
                _ => BackgroundSize::Explicit(LengthOrAuto::Auto, LengthOrAuto::Auto),
            }
        }
        P::OverflowX => s.overflow_x = overflow(value),
        P::OverflowY => s.overflow_y = overflow(value),
        P::Opacity => {
            if let V::Number(n) = value {
                s.opacity = n.clamp(0.0, 1.0);
            }
        }
        P::ZIndex => {
            s.z_index = match value {
                V::Integer(i) => Some(*i),
                _ => None,
            }
        }
        P::VerticalAlign => {
            s.vertical_align = match value {
                V::Length(_) => VerticalAlign::Length(len(value)),
                _ => match kwd(value) {
                    "sub" => VerticalAlign::Sub,
                    "super" => VerticalAlign::Super,
                    "text-top" => VerticalAlign::TextTop,
                    "text-bottom" => VerticalAlign::TextBottom,
                    "middle" => VerticalAlign::Middle,
                    "top" => VerticalAlign::Top,
                    "bottom" => VerticalAlign::Bottom,
                    _ => VerticalAlign::Baseline,
                },
            }
        }
        P::TextDecorationLine => {
            if let V::DecorationLine(u, o, l) = value {
                s.text_decoration_line = (*u, *o, *l);
            }
        }
        P::TextDecorationColor => {
            s.text_decoration_color = match value {
                V::Color(c) => Some(*c),
                _ => None,
            }
        }
        P::TextDecorationStyle => {
            s.text_decoration_style = match kwd(value) {
                "double" => DecorationStyle::Double,
                "dotted" => DecorationStyle::Dotted,
                "dashed" => DecorationStyle::Dashed,
                "wavy" => DecorationStyle::Wavy,
                _ => DecorationStyle::Solid,
            }
        }
        P::FlexDirection => {
            s.flex_direction = match kwd(value) {
                "row-reverse" => FlexDirection::RowReverse,
                "column" => FlexDirection::Column,
                "column-reverse" => FlexDirection::ColumnReverse,
                _ => FlexDirection::Row,
            }
        }
        P::FlexWrap => {
            s.flex_wrap = match kwd(value) {
                "wrap" => FlexWrap::Wrap,
                "wrap-reverse" => FlexWrap::WrapReverse,
                _ => FlexWrap::Nowrap,
            }
        }
        P::JustifyContent => s.justify_content = distribution(value),
        P::AlignContent => s.align_content = distribution(value),
        P::AlignItems => s.align_items = align(value),
        P::AlignSelf => s.align_self = align(value),
        P::FlexGrow => {
            if let V::Number(n) = value {
                s.flex_grow = *n;
            }
        }
        P::FlexShrink => {
            if let V::Number(n) = value {
                s.flex_shrink = *n;
            }
        }
        P::FlexBasis => {
            s.flex_basis = match value {
                V::Keyword("content") => Size::MaxContent,
                _ => size(value),
            }
        }
        P::Order => {
            if let V::Integer(i) = value {
                s.order = *i;
            }
        }
        P::RowGap => s.row_gap = matches!(value, V::Length(_)).then(|| len(value)),
        P::ColumnGap => s.column_gap = matches!(value, V::Length(_)).then(|| len(value)),
        P::TableLayout => {
            s.table_layout = if kwd(value) == "fixed" {
                TableLayout::Fixed
            } else {
                TableLayout::Auto
            }
        }
        P::Content => {
            s.content = match value {
                V::Content(items) => Some(items.clone()),
                _ => None,
            }
        }
        P::BoxShadow => {
            if let V::Shadows(list) = value {
                s.box_shadow = list
                    .iter()
                    .map(|sh| BoxShadow {
                        x: sh.lengths[0].compute(&lctx).resolve(0.0),
                        y: sh.lengths[1].compute(&lctx).resolve(0.0),
                        blur: sh.lengths[2].compute(&lctx).resolve(0.0).max(0.0),
                        spread: sh.lengths[3].compute(&lctx).resolve(0.0),
                        color: sh.color.unwrap_or(s.color),
                        inset: sh.inset,
                    })
                    .collect();
            }
        }
        P::OutlineWidth => s.outline_width = border_width(value),
        P::OutlineStyle => s.outline_style = border_style(value),
        P::OutlineColor => s.outline_color = color(value, s.color),
        P::OutlineOffset => s.outline_offset = px(value),
        P::ObjectFit => {
            s.object_fit = match kwd(value) {
                "contain" => ObjectFit::Contain,
                "cover" => ObjectFit::Cover,
                "none" => ObjectFit::None,
                "scale-down" => ObjectFit::ScaleDown,
                _ => ObjectFit::Fill,
            }
        }
        P::CaptionSide => s.caption_side_bottom = kwd(value) == "bottom",
        P::OverflowWrap => s.overflow_wrap_anywhere = kwd(value) != "normal",
        P::WordBreak => {
            s.word_break_all = kwd(value) == "break-all";
            if kwd(value) == "break-word" {
                s.overflow_wrap_anywhere = true;
            }
        }
        P::GridTemplateColumns | P::GridTemplateRows => {
            if let V::Tokens(t) = value {
                let list: std::rc::Rc<[crate::style::grid::TrackListItem]> =
                    Rc::from(crate::style::grid::parse_track_list(t, &lctx).unwrap_or_default());
                if id == P::GridTemplateColumns {
                    s.grid_template_columns = list;
                } else {
                    s.grid_template_rows = list;
                }
            }
        }
        P::GridAutoColumns | P::GridAutoRows => {
            if let V::Tokens(t) = value
                && let Some(ts) = crate::style::grid::parse_track_size(t, &lctx)
            {
                if id == P::GridAutoColumns {
                    s.grid_auto_columns = ts;
                } else {
                    s.grid_auto_rows = ts;
                }
            }
        }
        P::GridColumnStart | P::GridColumnEnd | P::GridRowStart | P::GridRowEnd => {
            if let V::Tokens(t) = value {
                let line = crate::style::grid::parse_grid_line(t).unwrap_or_default();
                match id {
                    P::GridColumnStart => s.grid_column.0 = line,
                    P::GridColumnEnd => s.grid_column.1 = line,
                    P::GridRowStart => s.grid_row.0 = line,
                    _ => s.grid_row.1 = line,
                }
            }
        }
        P::GridTemplateAreas => {
            if let V::Tokens(t) = value {
                s.grid_template_areas = crate::style::grid::parse_areas(t)
                    .filter(|a| !a.is_empty())
                    .map(Rc::new);
            }
        }
        P::GridAutoFlow => {
            if let V::Tokens(t) = value {
                let words: Vec<String> = t
                    .iter()
                    .filter_map(|v| v.ident().map(|s| s.to_ascii_lowercase()))
                    .collect();
                s.grid_auto_flow_column = words.iter().any(|w| w == "column");
                s.grid_auto_flow_dense = words.iter().any(|w| w == "dense");
            }
        }
        P::Transform => {
            s.transform = match value {
                V::Tokens(t) => Some(Rc::new(parse_translation(t, &lctx))),
                _ => None,
            }
        }
        P::JustifyItems => s.justify_items = align(value),
        P::TextOverflow => s.text_overflow_ellipsis = kwd(value) == "ellipsis",
        P::CounterReset | P::CounterIncrement | P::CounterSet => {
            let list = match value {
                V::Tokens(t) => {
                    parse_counter_list(t, if id == P::CounterIncrement { 1 } else { 0 }).unwrap_or_default()
                }
                _ => Vec::new(),
            };
            let mut ops = s.counter_ops.as_deref().cloned().unwrap_or_default();
            match id {
                P::CounterReset => ops.reset = list,
                P::CounterIncrement => ops.increment = list,
                _ => ops.set = list,
            }
            s.counter_ops = Some(Rc::new(ops));
        }
        P::JustifySelf => s.justify_self = align(value),
        P::MaskImage => s.has_mask = kwd(value) == "mask",
        P::AspectRatio => {
            s.aspect_ratio = match value {
                V::Number(n) => Some(*n),
                _ => None,
            }
        }
    }
}

/// Copies property `id`'s computed value from `src` (used for `inherit`/`initial`).
pub fn copy_property(id: PropertyId, dst: &mut ComputedStyle, src: &ComputedStyle) {
    use PropertyId as P;
    match id {
        P::Color => dst.color = src.color,
        P::FontFamily => dst.font_family = src.font_family.clone(),
        P::FontSize => dst.font_size = src.font_size,
        P::FontWeight => dst.font_weight = src.font_weight,
        P::FontStyle => dst.font_style = src.font_style,
        P::LineHeight => dst.line_height = src.line_height,
        P::TextAlign => dst.text_align = src.text_align,
        P::TextIndent => dst.text_indent = src.text_indent.clone(),
        P::TextTransform => dst.text_transform = src.text_transform,
        P::WhiteSpace => dst.white_space = src.white_space,
        P::LetterSpacing => dst.letter_spacing = src.letter_spacing,
        P::WordSpacing => dst.word_spacing = src.word_spacing,
        P::ListStyleType => dst.list_style_type = src.list_style_type.clone(),
        P::ListStylePosition => dst.list_style_position = src.list_style_position,
        P::ListStyleImage => dst.list_style_image = src.list_style_image.clone(),
        P::Visibility => dst.visibility = src.visibility,
        P::BorderCollapse => dst.border_collapse = src.border_collapse,
        P::BorderSpacing => dst.border_spacing = src.border_spacing,
        P::Cursor => dst.cursor = src.cursor,
        P::Display => dst.display = src.display,
        P::Position => dst.position = src.position,
        P::Float => dst.float = src.float,
        P::Clear => dst.clear = src.clear,
        P::Top => dst.inset[0] = src.inset[0].clone(),
        P::Right => dst.inset[1] = src.inset[1].clone(),
        P::Bottom => dst.inset[2] = src.inset[2].clone(),
        P::Left => dst.inset[3] = src.inset[3].clone(),
        P::Width => dst.width = src.width.clone(),
        P::Height => dst.height = src.height.clone(),
        P::MinWidth => dst.min_width = src.min_width.clone(),
        P::MinHeight => dst.min_height = src.min_height.clone(),
        P::MaxWidth => dst.max_width = src.max_width.clone(),
        P::MaxHeight => dst.max_height = src.max_height.clone(),
        P::MarginTop => dst.margin[0] = src.margin[0].clone(),
        P::MarginRight => dst.margin[1] = src.margin[1].clone(),
        P::MarginBottom => dst.margin[2] = src.margin[2].clone(),
        P::MarginLeft => dst.margin[3] = src.margin[3].clone(),
        P::PaddingTop => dst.padding[0] = src.padding[0].clone(),
        P::PaddingRight => dst.padding[1] = src.padding[1].clone(),
        P::PaddingBottom => dst.padding[2] = src.padding[2].clone(),
        P::PaddingLeft => dst.padding[3] = src.padding[3].clone(),
        P::BorderTopWidth => dst.border_width[0] = src.border_width[0],
        P::BorderRightWidth => dst.border_width[1] = src.border_width[1],
        P::BorderBottomWidth => dst.border_width[2] = src.border_width[2],
        P::BorderLeftWidth => dst.border_width[3] = src.border_width[3],
        P::BorderTopStyle => dst.border_style[0] = src.border_style[0],
        P::BorderRightStyle => dst.border_style[1] = src.border_style[1],
        P::BorderBottomStyle => dst.border_style[2] = src.border_style[2],
        P::BorderLeftStyle => dst.border_style[3] = src.border_style[3],
        P::BorderTopColor => dst.border_color[0] = src.border_color[0],
        P::BorderRightColor => dst.border_color[1] = src.border_color[1],
        P::BorderBottomColor => dst.border_color[2] = src.border_color[2],
        P::BorderLeftColor => dst.border_color[3] = src.border_color[3],
        P::BorderTopLeftRadius => dst.border_radius[0] = src.border_radius[0].clone(),
        P::BorderTopRightRadius => dst.border_radius[1] = src.border_radius[1].clone(),
        P::BorderBottomRightRadius => dst.border_radius[2] = src.border_radius[2].clone(),
        P::BorderBottomLeftRadius => dst.border_radius[3] = src.border_radius[3].clone(),
        P::BoxSizing => dst.box_sizing = src.box_sizing,
        P::BackgroundColor => dst.background_color = src.background_color,
        P::BackgroundImage => {
            dst.background_image = src.background_image.clone();
            dst.background_gradient = src.background_gradient.clone();
        }
        P::BackgroundRepeat => dst.background_repeat = src.background_repeat,
        P::BackgroundPosition => dst.background_position = src.background_position.clone(),
        P::BackgroundSize => dst.background_size = src.background_size.clone(),
        P::OverflowX => dst.overflow_x = src.overflow_x,
        P::OverflowY => dst.overflow_y = src.overflow_y,
        P::Opacity => dst.opacity = src.opacity,
        P::ZIndex => dst.z_index = src.z_index,
        P::VerticalAlign => dst.vertical_align = src.vertical_align.clone(),
        P::TextDecorationLine => dst.text_decoration_line = src.text_decoration_line,
        P::TextDecorationColor => dst.text_decoration_color = src.text_decoration_color,
        P::TextDecorationStyle => dst.text_decoration_style = src.text_decoration_style,
        P::FlexDirection => dst.flex_direction = src.flex_direction,
        P::FlexWrap => dst.flex_wrap = src.flex_wrap,
        P::JustifyContent => dst.justify_content = src.justify_content,
        P::AlignItems => dst.align_items = src.align_items,
        P::AlignSelf => dst.align_self = src.align_self,
        P::AlignContent => dst.align_content = src.align_content,
        P::FlexGrow => dst.flex_grow = src.flex_grow,
        P::FlexShrink => dst.flex_shrink = src.flex_shrink,
        P::FlexBasis => dst.flex_basis = src.flex_basis.clone(),
        P::Order => dst.order = src.order,
        P::RowGap => dst.row_gap = src.row_gap.clone(),
        P::ColumnGap => dst.column_gap = src.column_gap.clone(),
        P::TableLayout => dst.table_layout = src.table_layout,
        P::Content => dst.content = src.content.clone(),
        P::BoxShadow => dst.box_shadow = src.box_shadow.clone(),
        P::OutlineWidth => dst.outline_width = src.outline_width,
        P::OutlineStyle => dst.outline_style = src.outline_style,
        P::OutlineColor => dst.outline_color = src.outline_color,
        P::OutlineOffset => dst.outline_offset = src.outline_offset,
        P::ObjectFit => dst.object_fit = src.object_fit,
        P::AspectRatio => dst.aspect_ratio = src.aspect_ratio,
        P::CaptionSide => dst.caption_side_bottom = src.caption_side_bottom,
        P::OverflowWrap => dst.overflow_wrap_anywhere = src.overflow_wrap_anywhere,
        P::WordBreak => dst.word_break_all = src.word_break_all,
        P::GridTemplateColumns => dst.grid_template_columns = src.grid_template_columns.clone(),
        P::GridTemplateRows => dst.grid_template_rows = src.grid_template_rows.clone(),
        P::GridAutoColumns => dst.grid_auto_columns = src.grid_auto_columns.clone(),
        P::GridAutoRows => dst.grid_auto_rows = src.grid_auto_rows.clone(),
        P::GridAutoFlow => {
            dst.grid_auto_flow_column = src.grid_auto_flow_column;
            dst.grid_auto_flow_dense = src.grid_auto_flow_dense;
        }
        P::GridColumnStart => dst.grid_column.0 = src.grid_column.0.clone(),
        P::GridColumnEnd => dst.grid_column.1 = src.grid_column.1.clone(),
        P::GridRowStart => dst.grid_row.0 = src.grid_row.0.clone(),
        P::GridRowEnd => dst.grid_row.1 = src.grid_row.1.clone(),
        P::GridTemplateAreas => dst.grid_template_areas = src.grid_template_areas.clone(),
        P::JustifyItems => dst.justify_items = src.justify_items,
        P::Transform => dst.transform = src.transform.clone(),
        P::TextOverflow => dst.text_overflow_ellipsis = src.text_overflow_ellipsis,
        P::CounterReset | P::CounterIncrement | P::CounterSet => {
            let from = src.counter_ops.as_deref().cloned().unwrap_or_default();
            let mut ops = dst.counter_ops.as_deref().cloned().unwrap_or_default();
            match id {
                P::CounterReset => ops.reset = from.reset,
                P::CounterIncrement => ops.increment = from.increment,
                _ => ops.set = from.set,
            }
            dst.counter_ops = Some(Rc::new(ops));
        }
        P::JustifySelf => dst.justify_self = src.justify_self,
        P::MaskImage => dst.has_mask = src.has_mask,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn decl(name: &str, value: &str) -> Option<Vec<PropertyDeclaration>> {
        parse_declaration(name, &parse_component_values(value))
    }

    fn longhands(name: &str, value: &str) -> Vec<(PropertyId, SpecifiedValue)> {
        decl(name, value)
            .unwrap_or_else(|| panic!("{name}: {value} invalid"))
            .into_iter()
            .filter_map(|d| match d {
                PropertyDeclaration::Longhand(id, v) => Some((id, v)),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn invalid_values_are_rejected() {
        assert!(decl("width", "-10px").is_none());
        assert!(decl("padding-left", "-1px").is_none());
        assert!(decl("display", "blockish").is_none());
        assert!(decl("color", "12px").is_none());
        assert!(decl("margin", "1px 2px 3px 4px 5px").is_none());
        assert!(decl("not-a-property", "1px").is_none());
        assert!(decl("margin-left", "-10px").is_some());
    }

    #[test]
    fn margin_and_border_shorthands() {
        let m = longhands("margin", "1px auto");
        assert_eq!(m.len(), 4);
        assert_eq!(m[1], (PropertyId::MarginRight, SpecifiedValue::Keyword("auto")));
        let b = longhands("border", "2px dashed red");
        assert_eq!(b.len(), 12);
        assert!(b.contains(&(PropertyId::BorderLeftStyle, SpecifiedValue::Keyword("dashed"))));
        assert!(b.contains(&(PropertyId::BorderTopColor, SpecifiedValue::Color(Color::rgb(255, 0, 0)))));
        let b = longhands("border-top", "solid");
        assert!(b.contains(&(PropertyId::BorderTopWidth, SpecifiedValue::Keyword("medium"))));
        assert!(b.contains(&(PropertyId::BorderTopColor, SpecifiedValue::CurrentColor)));
    }

    #[test]
    fn flex_shorthand() {
        let f = longhands("flex", "1");
        assert_eq!(f[0], (PropertyId::FlexGrow, SpecifiedValue::Number(1.0)));
        assert_eq!(f[1], (PropertyId::FlexShrink, SpecifiedValue::Number(1.0)));
        let f = longhands("flex", "none");
        assert_eq!(f[1], (PropertyId::FlexShrink, SpecifiedValue::Number(0.0)));
        let f = longhands("flex", "1 1 0");
        assert_eq!(
            f[2],
            (PropertyId::FlexBasis, SpecifiedValue::Length(SpecifiedLength::px(0.0)))
        );
        let f = longhands("flex", "2 3 100px");
        assert_eq!(
            f[2],
            (
                PropertyId::FlexBasis,
                SpecifiedValue::Length(SpecifiedLength::px(100.0))
            )
        );
    }

    #[test]
    fn font_shorthand() {
        let f = longhands("font", "italic bold 12px/1.5 \"Liberation Sans\", sans-serif");
        assert!(f.contains(&(PropertyId::FontStyle, SpecifiedValue::Keyword("italic"))));
        assert!(f.contains(&(PropertyId::FontWeight, SpecifiedValue::Keyword("bold"))));
        assert!(f.contains(&(PropertyId::LineHeight, SpecifiedValue::Number(1.5))));
        let fam = f.iter().find(|(id, _)| *id == PropertyId::FontFamily).unwrap();
        assert_eq!(
            fam.1,
            SpecifiedValue::FontFamily(Rc::from(vec!["Liberation Sans".to_string(), "sans-serif".to_string()]))
        );
        assert!(decl("font", "bold").is_none(), "size and family are required");
    }

    #[test]
    fn background_shorthand() {
        let b = longhands("background", "#fff url(a.png) no-repeat center / cover");
        assert!(b.contains(&(PropertyId::BackgroundColor, SpecifiedValue::Color(Color::WHITE))));
        assert!(b.contains(&(PropertyId::BackgroundImage, SpecifiedValue::Url(Rc::from("a.png")))));
        assert!(b.contains(&(PropertyId::BackgroundRepeat, SpecifiedValue::Keyword("no-repeat"))));
        assert!(b.contains(&(PropertyId::BackgroundSize, SpecifiedValue::Keyword("cover"))));
    }

    #[test]
    fn var_substitution() {
        let mut customs = HashMap::new();
        customs.insert("--gap".to_string(), Rc::from("4px"));
        customs.insert("--pair".to_string(), Rc::from("var(--gap) 8px"));
        let c = Some(&customs);
        assert_eq!(substitute_vars("calc(var(--gap) * 2)", c, 0).unwrap(), "calc(4px * 2)");
        assert_eq!(substitute_vars("var(--missing, red)", c, 0).unwrap(), "red");
        assert!(substitute_vars("var(--missing)", c, 0).is_none());
        let v = resolve_unparsed(PropertyId::MarginRight, "var(--pair)", Some("margin"), c).unwrap();
        assert_eq!(v, SpecifiedValue::Length(SpecifiedLength::px(8.0)));
    }
}
