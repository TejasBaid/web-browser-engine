//! Computed values: the output of the cascade for one element, consumed by layout and
//! painting. Lengths are absolute px, or px plus a percentage that layout resolves.

use crate::css::color::Color;
use crate::css::values::LengthPercentage;
use std::collections::HashMap;
use std::rc::Rc;

#[allow(missing_docs)]
mod kinds {
    /// `display`
    #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
    pub enum Display {
        None,
        Contents,
        Block,
        Inline,
        InlineBlock,
        FlowRoot,
        ListItem,
        Flex,
        InlineFlex,
        Grid,
        InlineGrid,
        Table,
        InlineTable,
        TableRowGroup,
        TableHeaderGroup,
        TableFooterGroup,
        TableRow,
        TableCell,
        TableColumn,
        TableColumnGroup,
        TableCaption,
    }

    /// `position`
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub enum Position {
        Static,
        Relative,
        Absolute,
        Fixed,
        Sticky,
    }

    /// `float`
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub enum Float {
        None,
        Left,
        Right,
    }

    /// `clear`
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub enum Clear {
        None,
        Left,
        Right,
        Both,
    }

    /// `box-sizing`
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub enum BoxSizing {
        ContentBox,
        BorderBox,
    }

    /// Border line styles.
    #[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
    pub enum BorderStyle {
        #[default]
        None,
        Hidden,
        Solid,
        Dashed,
        Dotted,
        Double,
        Groove,
        Ridge,
        Inset,
        Outset,
    }

    /// `overflow-x` / `overflow-y`
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub enum Overflow {
        Visible,
        Hidden,
        Clip,
        Scroll,
        Auto,
    }

    /// `text-align`
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub enum TextAlign {
        Left,
        Right,
        Center,
        Justify,
        /// `-webkit-center` (used by `<center>`): centers text and block children.
        WebkitCenter,
    }

    /// `white-space`
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub enum WhiteSpace {
        Normal,
        Pre,
        Nowrap,
        PreWrap,
        PreLine,
        BreakSpaces,
    }

    /// `font-style`
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub enum FontStyle {
        Normal,
        Italic,
        Oblique,
    }

    /// `text-transform`
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub enum TextTransform {
        None,
        Uppercase,
        Lowercase,
        Capitalize,
    }

    /// `visibility`
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub enum Visibility {
        Visible,
        Hidden,
        Collapse,
    }

    /// `list-style-type`
    #[derive(Clone, Debug, PartialEq, Eq)]
    pub enum ListStyleType {
        None,
        Disc,
        Circle,
        Square,
        Decimal,
        DecimalLeadingZero,
        LowerAlpha,
        UpperAlpha,
        LowerRoman,
        UpperRoman,
        LowerGreek,
        /// A literal string marker, e.g. `list-style-type: "- "`.
        String(String),
    }

    /// `list-style-position`
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub enum ListStylePosition {
        Outside,
        Inside,
    }

    /// `flex-direction`
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub enum FlexDirection {
        Row,
        RowReverse,
        Column,
        ColumnReverse,
    }

    /// `flex-wrap`
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub enum FlexWrap {
        Nowrap,
        Wrap,
        WrapReverse,
    }

    /// `justify-content` (and `align-content` distribution values).
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub enum JustifyContent {
        Normal,
        FlexStart,
        FlexEnd,
        Center,
        SpaceBetween,
        SpaceAround,
        SpaceEvenly,
        Stretch,
    }

    /// `align-items` / `align-self` (with `Auto` only valid for align-self).
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub enum AlignItems {
        Auto,
        Normal,
        Stretch,
        FlexStart,
        FlexEnd,
        Center,
        Baseline,
    }

    /// `vertical-align` keywords.
    #[derive(Clone, Debug, PartialEq)]
    pub enum VerticalAlign {
        Baseline,
        Sub,
        Super,
        TextTop,
        TextBottom,
        Middle,
        Top,
        Bottom,
        Length(super::LengthPercentage),
    }

    /// `border-collapse`
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub enum BorderCollapse {
        Separate,
        Collapse,
    }

    /// `table-layout`
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub enum TableLayout {
        Auto,
        Fixed,
    }

    /// `background-repeat`
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub enum BackgroundRepeat {
        Repeat,
        RepeatX,
        RepeatY,
        NoRepeat,
    }

    /// `object-fit`
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub enum ObjectFit {
        Fill,
        Contain,
        Cover,
        None,
        ScaleDown,
    }

    /// `cursor` (only the distinction the GUI cares about).
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub enum Cursor {
        Auto,
        Default,
        Pointer,
        Text,
    }

    /// `text-decoration-style`
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub enum DecorationStyle {
        Solid,
        Double,
        Dotted,
        Dashed,
        Wavy,
    }
}

pub use kinds::*;

impl Display {
    /// Whether the outer display type is inline-level.
    pub fn is_inline_level(self) -> bool {
        matches!(
            self,
            Display::Inline | Display::InlineBlock | Display::InlineFlex | Display::InlineGrid | Display::InlineTable
        )
    }

    /// The block-level equivalent ("blockification", used for floats, abspos, flex items
    /// and the root element).
    pub fn blockify(self) -> Display {
        match self {
            Display::Inline | Display::InlineBlock => Display::Block,
            Display::InlineFlex => Display::Flex,
            Display::InlineGrid => Display::Grid,
            Display::InlineTable => Display::Table,
            Display::TableRowGroup
            | Display::TableHeaderGroup
            | Display::TableFooterGroup
            | Display::TableRow
            | Display::TableCell
            | Display::TableColumn
            | Display::TableColumnGroup
            | Display::TableCaption => Display::Block,
            d => d,
        }
    }
}

/// `width`/`height`/`min-*`/`flex-basis` style sizes.
#[derive(Clone, Debug, PartialEq)]
pub enum Size {
    /// `auto` (or `none` for max-* properties).
    Auto,
    /// A length or percentage.
    Length(LengthPercentage),
    /// `min-content`
    MinContent,
    /// `max-content`
    MaxContent,
    /// `fit-content`
    FitContent,
}

impl Size {
    /// Whether this is `auto`.
    pub fn is_auto(&self) -> bool {
        matches!(self, Size::Auto)
    }

    /// The length, if this is a length.
    pub fn length(&self) -> Option<&LengthPercentage> {
        match self {
            Size::Length(l) => Some(l),
            _ => None,
        }
    }
}

/// A length-percentage or `auto` (margins, offsets).
#[derive(Clone, Debug, PartialEq)]
pub enum LengthOrAuto {
    /// `auto`
    Auto,
    /// A length.
    Length(LengthPercentage),
}

impl LengthOrAuto {
    /// Whether this is `auto`.
    pub fn is_auto(&self) -> bool {
        matches!(self, LengthOrAuto::Auto)
    }

    /// Resolves, mapping `auto` to `None`.
    pub fn resolve(&self, basis: f32) -> Option<f32> {
        match self {
            LengthOrAuto::Auto => None,
            LengthOrAuto::Length(l) => Some(l.resolve(basis)),
        }
    }
}

/// `line-height`
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum LineHeight {
    /// `normal`: from font metrics.
    Normal,
    /// A unitless multiplier (inherited as the number).
    Number(f32),
    /// An absolute length.
    Px(f32),
}

/// One `box-shadow` layer.
#[derive(Clone, Debug, PartialEq)]
pub struct BoxShadow {
    /// Horizontal offset.
    pub x: f32,
    /// Vertical offset.
    pub y: f32,
    /// Blur radius.
    pub blur: f32,
    /// Spread distance.
    pub spread: f32,
    /// Color.
    pub color: Color,
    /// `inset` shadows are not painted (yet) but are parsed.
    pub inset: bool,
}

/// A propagated text decoration (from this element or an ancestor).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TextDecoration {
    /// Underline.
    pub underline: bool,
    /// Overline.
    pub overline: bool,
    /// Line-through.
    pub line_through: bool,
    /// Decoration color.
    pub color: Color,
    /// Line style.
    pub style: DecorationStyle,
}

/// Items of the `content` property for `::before`/`::after`.
#[derive(Clone, Debug, PartialEq)]
pub enum ContentItem {
    /// A literal string.
    String(String),
    /// `attr(name)`
    Attr(String),
    /// `open-quote`
    OpenQuote,
    /// `close-quote`
    CloseQuote,
    /// `url(...)` image content.
    Url(String),
    /// `counter(name, style)`
    Counter(String, ListStyleType),
    /// `counters(name, separator, style)`
    Counters(String, String, ListStyleType),
}

/// `counter-reset`, `counter-increment` and `counter-set` of an element.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct CounterOps {
    /// `counter-reset` (name, value).
    pub reset: Vec<(String, i32)>,
    /// `counter-increment` (name, amount).
    pub increment: Vec<(String, i32)>,
    /// `counter-set` (name, value).
    pub set: Vec<(String, i32)>,
}

/// `background-size`
#[derive(Clone, Debug, PartialEq)]
pub enum BackgroundSize {
    /// `auto` / explicit `w h` (each may be auto).
    Explicit(LengthOrAuto, LengthOrAuto),
    /// `cover`
    Cover,
    /// `contain`
    Contain,
}

/// The computed style of an element (or pseudo-element / anonymous box).
#[derive(Clone, Debug, PartialEq)]
pub struct ComputedStyle {
    // ---- inherited ----
    /// `color`
    pub color: Color,
    /// `font-family` list (lowercased generic names and family names as written).
    pub font_family: Rc<[String]>,
    /// `font-size` in px.
    pub font_size: f32,
    /// `font-weight` (100..=900).
    pub font_weight: u16,
    /// `font-style`
    pub font_style: FontStyle,
    /// `line-height`
    pub line_height: LineHeight,
    /// `text-align`
    pub text_align: TextAlign,
    /// `text-indent`
    pub text_indent: LengthPercentage,
    /// `text-transform`
    pub text_transform: TextTransform,
    /// `white-space`
    pub white_space: WhiteSpace,
    /// `letter-spacing` in px.
    pub letter_spacing: f32,
    /// `word-spacing` in px.
    pub word_spacing: f32,
    /// `list-style-type`
    pub list_style_type: ListStyleType,
    /// `list-style-position`
    pub list_style_position: ListStylePosition,
    /// `list-style-image`
    pub list_style_image: Option<String>,
    /// `visibility`
    pub visibility: Visibility,
    /// `border-collapse`
    pub border_collapse: BorderCollapse,
    /// `border-spacing` (horizontal, vertical) in px.
    pub border_spacing: (f32, f32),
    /// `cursor`
    pub cursor: Cursor,
    /// Text decorations in effect (propagated from ancestors plus this element's own).
    pub text_decorations: Rc<[TextDecoration]>,
    /// `caption-side: bottom`.
    pub caption_side_bottom: bool,
    /// `overflow-wrap: break-word | anywhere` (or `word-break: break-word`): words that
    /// don't fit on an empty line may be broken anywhere.
    pub overflow_wrap_anywhere: bool,
    /// `word-break: break-all`: lines may break between any two characters.
    pub word_break_all: bool,
    /// Custom properties (`--name`), inherited.
    pub custom_properties: Option<Rc<HashMap<String, Rc<str>>>>,

    // ---- not inherited ----
    /// `display`
    pub display: Display,
    /// `position`
    pub position: Position,
    /// `float`
    pub float: Float,
    /// `clear`
    pub clear: Clear,
    /// `top`, `right`, `bottom`, `left`
    pub inset: [LengthOrAuto; 4],
    /// `width`
    pub width: Size,
    /// `height`
    pub height: Size,
    /// `min-width`
    pub min_width: Size,
    /// `min-height`
    pub min_height: Size,
    /// `max-width` (`Auto` means `none`)
    pub max_width: Size,
    /// `max-height` (`Auto` means `none`)
    pub max_height: Size,
    /// Margins: top, right, bottom, left.
    pub margin: [LengthOrAuto; 4],
    /// Padding: top, right, bottom, left.
    pub padding: [LengthPercentage; 4],
    /// Border widths in px (zero when the style is none/hidden): top, right, bottom, left.
    pub border_width: [f32; 4],
    /// Border styles.
    pub border_style: [BorderStyle; 4],
    /// Border colors.
    pub border_color: [Color; 4],
    /// Corner radii (horizontal, vertical): top-left, top-right, bottom-right, bottom-left.
    pub border_radius: [(LengthPercentage, LengthPercentage); 4],
    /// `box-sizing`
    pub box_sizing: BoxSizing,
    /// `background-color`
    pub background_color: Color,
    /// `background-image` (first `url()` layer only).
    pub background_image: Option<String>,
    /// `background-image` when the topmost layer is a gradient.
    pub background_gradient: Option<Rc<crate::css::gradient::Gradient>>,
    /// `background-repeat`
    pub background_repeat: BackgroundRepeat,
    /// `background-position` (x, y)
    pub background_position: (LengthPercentage, LengthPercentage),
    /// `background-size`
    pub background_size: BackgroundSize,
    /// `overflow-x`
    pub overflow_x: Overflow,
    /// `overflow-y`
    pub overflow_y: Overflow,
    /// `opacity`
    pub opacity: f32,
    /// `z-index` (`None` = auto)
    pub z_index: Option<i32>,
    /// `vertical-align`
    pub vertical_align: VerticalAlign,
    /// `text-decoration-line` of this element: (underline, overline, line-through).
    pub text_decoration_line: (bool, bool, bool),
    /// `text-decoration-color` (`None` = currentcolor)
    pub text_decoration_color: Option<Color>,
    /// `text-decoration-style`
    pub text_decoration_style: DecorationStyle,
    /// `flex-direction`
    pub flex_direction: FlexDirection,
    /// `flex-wrap`
    pub flex_wrap: FlexWrap,
    /// `justify-content`
    pub justify_content: JustifyContent,
    /// `align-items`
    pub align_items: AlignItems,
    /// `align-self`
    pub align_self: AlignItems,
    /// `align-content`
    pub align_content: JustifyContent,
    /// `flex-grow`
    pub flex_grow: f32,
    /// `flex-shrink`
    pub flex_shrink: f32,
    /// `flex-basis` (`Auto`, or `MaxContent` for `content`)
    pub flex_basis: Size,
    /// `order`
    pub order: i32,
    /// `row-gap` (`None` = normal)
    pub row_gap: Option<LengthPercentage>,
    /// `column-gap` (`None` = normal)
    pub column_gap: Option<LengthPercentage>,
    /// `table-layout`
    pub table_layout: TableLayout,
    /// `content` for pseudo-elements (`None` = normal/none).
    pub content: Option<Rc<[ContentItem]>>,
    /// `box-shadow`
    pub box_shadow: Rc<[BoxShadow]>,
    /// `outline-width` in px (zero when style is none)
    pub outline_width: f32,
    /// `outline-style`
    pub outline_style: BorderStyle,
    /// `outline-color`
    pub outline_color: Color,
    /// `outline-offset` in px
    pub outline_offset: f32,
    /// `object-fit`
    pub object_fit: ObjectFit,
    /// `aspect-ratio` (width / height), `None` = auto.
    pub aspect_ratio: Option<f32>,
    /// Whether `mask-image` is set (masks are unsupported; such boxes are not painted).
    pub has_mask: bool,
    /// `grid-template-columns`
    pub grid_template_columns: Rc<[crate::style::grid::TrackListItem]>,
    /// `grid-template-rows`
    pub grid_template_rows: Rc<[crate::style::grid::TrackListItem]>,
    /// `grid-auto-columns`
    pub grid_auto_columns: crate::style::grid::TrackSize,
    /// `grid-auto-rows`
    pub grid_auto_rows: crate::style::grid::TrackSize,
    /// `grid-auto-flow: column`
    pub grid_auto_flow_column: bool,
    /// `grid-auto-flow: dense`
    pub grid_auto_flow_dense: bool,
    /// `grid-column-start` / `grid-column-end`
    pub grid_column: (crate::style::grid::GridLine, crate::style::grid::GridLine),
    /// `grid-row-start` / `grid-row-end`
    pub grid_row: (crate::style::grid::GridLine, crate::style::grid::GridLine),
    /// `grid-template-areas`
    pub grid_template_areas: Option<Rc<Vec<Vec<String>>>>,
    /// `justify-items`
    pub justify_items: AlignItems,
    /// `justify-self`
    pub justify_self: AlignItems,
    /// `transform` (translation only; `None` = none).
    pub transform: Option<Rc<Transform2d>>,
    /// `text-overflow: ellipsis`
    pub text_overflow_ellipsis: bool,
    /// Counter operations (`None` when none are specified).
    pub counter_ops: Option<Rc<CounterOps>>,
}

/// The supported part of a `transform`: a translation (percentages refer to the box's
/// own size). `other` records that unsupported functions (rotate, scale, skew) were
/// present and ignored.
#[derive(Clone, Debug, PartialEq)]
pub struct Transform2d {
    /// Horizontal translation.
    pub x: LengthPercentage,
    /// Vertical translation.
    pub y: LengthPercentage,
    /// Whether unsupported transform functions were dropped.
    pub other: bool,
}

thread_local! {
    static INITIAL: Rc<ComputedStyle> = Rc::new(ComputedStyle::build_initial());
}

impl ComputedStyle {
    /// The initial values of every property (shared instance).
    pub fn initial() -> Rc<ComputedStyle> {
        INITIAL.with(Rc::clone)
    }

    fn build_initial() -> ComputedStyle {
        let zero = LengthPercentage::px(0.0);
        let len0 = || LengthOrAuto::Length(LengthPercentage::px(0.0));
        ComputedStyle {
            color: Color::BLACK,
            font_family: Rc::from(vec!["serif".to_string()]),
            font_size: 16.0,
            font_weight: 400,
            font_style: FontStyle::Normal,
            line_height: LineHeight::Normal,
            text_align: TextAlign::Left,
            text_indent: zero.clone(),
            text_transform: TextTransform::None,
            white_space: WhiteSpace::Normal,
            letter_spacing: 0.0,
            word_spacing: 0.0,
            list_style_type: ListStyleType::Disc,
            list_style_position: ListStylePosition::Outside,
            list_style_image: None,
            visibility: Visibility::Visible,
            border_collapse: BorderCollapse::Separate,
            border_spacing: (0.0, 0.0),
            cursor: Cursor::Auto,
            text_decorations: Rc::from(Vec::new()),
            custom_properties: None,
            caption_side_bottom: false,
            overflow_wrap_anywhere: false,
            word_break_all: false,
            display: Display::Inline,
            position: Position::Static,
            float: Float::None,
            clear: Clear::None,
            inset: [
                LengthOrAuto::Auto,
                LengthOrAuto::Auto,
                LengthOrAuto::Auto,
                LengthOrAuto::Auto,
            ],
            width: Size::Auto,
            height: Size::Auto,
            min_width: Size::Auto,
            min_height: Size::Auto,
            max_width: Size::Auto,
            max_height: Size::Auto,
            margin: [len0(), len0(), len0(), len0()],
            padding: [zero.clone(), zero.clone(), zero.clone(), zero.clone()],
            border_width: [0.0; 4],
            border_style: [BorderStyle::None; 4],
            border_color: [Color::BLACK; 4],
            border_radius: [
                (zero.clone(), zero.clone()),
                (zero.clone(), zero.clone()),
                (zero.clone(), zero.clone()),
                (zero.clone(), zero.clone()),
            ],
            box_sizing: BoxSizing::ContentBox,
            background_color: Color::TRANSPARENT,
            background_image: None,
            background_gradient: None,
            background_repeat: BackgroundRepeat::Repeat,
            background_position: (zero.clone(), zero.clone()),
            background_size: BackgroundSize::Explicit(LengthOrAuto::Auto, LengthOrAuto::Auto),
            overflow_x: Overflow::Visible,
            overflow_y: Overflow::Visible,
            opacity: 1.0,
            z_index: None,
            vertical_align: VerticalAlign::Baseline,
            text_decoration_line: (false, false, false),
            text_decoration_color: None,
            text_decoration_style: DecorationStyle::Solid,
            flex_direction: FlexDirection::Row,
            flex_wrap: FlexWrap::Nowrap,
            justify_content: JustifyContent::Normal,
            align_items: AlignItems::Normal,
            align_self: AlignItems::Auto,
            align_content: JustifyContent::Normal,
            flex_grow: 0.0,
            flex_shrink: 1.0,
            flex_basis: Size::Auto,
            order: 0,
            row_gap: None,
            column_gap: None,
            table_layout: TableLayout::Auto,
            content: None,
            box_shadow: Rc::from(Vec::new()),
            outline_width: 0.0,
            outline_style: BorderStyle::None,
            outline_color: Color::BLACK,
            outline_offset: 0.0,
            object_fit: ObjectFit::Fill,
            aspect_ratio: None,
            has_mask: false,
            grid_template_columns: Rc::from(Vec::new()),
            grid_template_rows: Rc::from(Vec::new()),
            grid_auto_columns: crate::style::grid::TrackSize::Single(crate::style::grid::TrackBreadth::Auto),
            grid_auto_rows: crate::style::grid::TrackSize::Single(crate::style::grid::TrackBreadth::Auto),
            grid_auto_flow_column: false,
            grid_auto_flow_dense: false,
            grid_column: Default::default(),
            grid_row: Default::default(),
            grid_template_areas: None,
            justify_items: AlignItems::Normal,
            justify_self: AlignItems::Auto,
            transform: None,
            text_overflow_ellipsis: false,
            counter_ops: None,
        }
    }

    /// A style whose inherited properties come from `parent` and whose other properties
    /// have their initial values (the starting point of the cascade for a child).
    pub fn inherit_from(parent: &ComputedStyle) -> ComputedStyle {
        let mut s = (*Self::initial()).clone();
        s.color = parent.color;
        s.font_family = parent.font_family.clone();
        s.font_size = parent.font_size;
        s.font_weight = parent.font_weight;
        s.font_style = parent.font_style;
        s.line_height = parent.line_height;
        s.text_align = parent.text_align;
        s.text_indent = parent.text_indent.clone();
        s.text_transform = parent.text_transform;
        s.white_space = parent.white_space;
        s.letter_spacing = parent.letter_spacing;
        s.word_spacing = parent.word_spacing;
        s.list_style_type = parent.list_style_type.clone();
        s.list_style_position = parent.list_style_position;
        s.list_style_image = parent.list_style_image.clone();
        s.visibility = parent.visibility;
        s.border_collapse = parent.border_collapse;
        s.border_spacing = parent.border_spacing;
        s.cursor = parent.cursor;
        s.text_decorations = parent.text_decorations.clone();
        s.custom_properties = parent.custom_properties.clone();
        s.caption_side_bottom = parent.caption_side_bottom;
        s.overflow_wrap_anywhere = parent.overflow_wrap_anywhere;
        s.word_break_all = parent.word_break_all;
        s
    }

    /// An anonymous box style (anonymous block wrappers, table wrappers): inherits from
    /// `parent` with the given display.
    pub fn anonymous(parent: &ComputedStyle, display: Display) -> ComputedStyle {
        let mut s = Self::inherit_from(parent);
        s.display = display;
        s
    }

    /// The used line height in px for this element's font, given the font's
    /// `normal` line height ratio.
    pub fn line_height_px(&self, normal_ratio: f32) -> f32 {
        match self.line_height {
            LineHeight::Normal => self.font_size * normal_ratio,
            LineHeight::Number(n) => self.font_size * n,
            LineHeight::Px(p) => p,
        }
    }

    /// Whether this box is absolutely positioned (absolute or fixed).
    pub fn is_out_of_flow_positioned(&self) -> bool {
        matches!(self.position, Position::Absolute | Position::Fixed)
    }

    /// Whether this box is positioned (anything but static).
    pub fn is_positioned(&self) -> bool {
        self.position != Position::Static
    }

    /// Whether the box is floated (and not absolutely positioned).
    pub fn is_floated(&self) -> bool {
        self.float != Float::None && !self.is_out_of_flow_positioned()
    }

    /// Whether overflow clips content in either axis.
    pub fn clips_overflow(&self) -> bool {
        self.overflow_x != Overflow::Visible || self.overflow_y != Overflow::Visible
    }

    /// Whether this element establishes a stacking context.
    pub fn creates_stacking_context(&self) -> bool {
        self.opacity < 1.0
            || self.transform.is_some()
            || (self.z_index.is_some() && self.is_positioned())
            || matches!(self.position, Position::Fixed | Position::Sticky)
    }

    /// Whether any corner has a non-zero radius.
    pub fn has_border_radius(&self) -> bool {
        self.border_radius.iter().any(|(h, v)| !h.is_zero() && !v.is_zero())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inheritance_resets_non_inherited() {
        let mut parent = (*ComputedStyle::initial()).clone();
        parent.color = Color::rgb(1, 2, 3);
        parent.display = Display::Block;
        parent.background_color = Color::WHITE;
        parent.font_size = 20.0;
        let child = ComputedStyle::inherit_from(&parent);
        assert_eq!(child.color, Color::rgb(1, 2, 3));
        assert_eq!(child.font_size, 20.0);
        assert_eq!(child.display, Display::Inline);
        assert_eq!(child.background_color, Color::TRANSPARENT);
    }

    #[test]
    fn blockification() {
        assert_eq!(Display::Inline.blockify(), Display::Block);
        assert_eq!(Display::InlineFlex.blockify(), Display::Flex);
        assert_eq!(Display::ListItem.blockify(), Display::ListItem);
    }
}
