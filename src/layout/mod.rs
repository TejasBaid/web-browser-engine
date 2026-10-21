//! Layout: turns the styled DOM into a tree of positioned [`Fragment`]s.
//!
//! The pipeline has three stages:
//!
//! 1. [`box_tree`] builds layout boxes from styled elements: anonymous block wrappers,
//!    inline formatting contexts flattened into item lists (which makes block-in-inline
//!    splitting straightforward), list markers, `::before`/`::after`, replaced elements.
//! 2. Formatting contexts lay boxes out: [`block`] (normal flow, margin collapsing,
//!    floats, clearance), [`inline`] (line breaking with real glyph advances, inline
//!    boxes, `vertical-align`), [`flex`] (the CSS Flexbox algorithm) and [`table`].
//!    Absolutely positioned boxes leave placeholders at their static position.
//! 3. [`positioned`] resolves placeholders against their containing blocks once those
//!    have their final size, and applies relative offsets.
//!
//! Fragment coordinates are relative to the parent fragment's border-box origin, so a
//! subtree can be moved in O(1) (flex items, table cells, inline-blocks on a line).

pub mod block;
pub mod box_tree;
pub mod flex;
pub mod floats;
pub mod fragment;
pub mod grid;
pub mod inline;
pub mod intrinsic;
pub mod positioned;
pub mod table;

pub use box_tree::{BoxContent, LayoutBox};
pub use fragment::{Fragment, FragmentKind, TextRun};

use crate::dom::Document;
use crate::image::Image;
use crate::style::StyledDocument;
use crate::style::computed::{BoxSizing, ComputedStyle, LengthOrAuto, Size};
use crate::text::FontDb;
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

/// A point in CSS px.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Point {
    /// x
    pub x: f32,
    /// y
    pub y: f32,
}

impl Point {
    /// Creates a point.
    pub const fn new(x: f32, y: f32) -> Point {
        Point { x, y }
    }
}

/// An axis-aligned rectangle in CSS px.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Rect {
    /// Left edge.
    pub x: f32,
    /// Top edge.
    pub y: f32,
    /// Width.
    pub width: f32,
    /// Height.
    pub height: f32,
}

impl Rect {
    /// Creates a rectangle.
    pub const fn new(x: f32, y: f32, width: f32, height: f32) -> Rect {
        Rect { x, y, width, height }
    }

    /// Right edge.
    pub fn right(&self) -> f32 {
        self.x + self.width
    }

    /// Bottom edge.
    pub fn bottom(&self) -> f32 {
        self.y + self.height
    }

    /// Translated copy.
    pub fn translate(&self, dx: f32, dy: f32) -> Rect {
        Rect::new(self.x + dx, self.y + dy, self.width, self.height)
    }

    /// Shrinks by edge insets.
    pub fn inset(&self, e: &Edges) -> Rect {
        Rect::new(
            self.x + e.left,
            self.y + e.top,
            (self.width - e.left - e.right).max(0.0),
            (self.height - e.top - e.bottom).max(0.0),
        )
    }

    /// Grows by edge outsets.
    pub fn outset(&self, e: &Edges) -> Rect {
        Rect::new(
            self.x - e.left,
            self.y - e.top,
            self.width + e.left + e.right,
            self.height + e.top + e.bottom,
        )
    }

    /// Intersection (may be empty).
    pub fn intersect(&self, o: &Rect) -> Rect {
        let x = self.x.max(o.x);
        let y = self.y.max(o.y);
        let r = self.right().min(o.right());
        let b = self.bottom().min(o.bottom());
        Rect::new(x, y, (r - x).max(0.0), (b - y).max(0.0))
    }

    /// Whether the rectangles overlap.
    pub fn intersects(&self, o: &Rect) -> bool {
        self.x < o.right() && o.x < self.right() && self.y < o.bottom() && o.y < self.bottom()
    }

    /// Whether the point lies inside.
    pub fn contains(&self, x: f32, y: f32) -> bool {
        x >= self.x && x < self.right() && y >= self.y && y < self.bottom()
    }

    /// Smallest rectangle containing both.
    pub fn union(&self, o: &Rect) -> Rect {
        if self.width <= 0.0 && self.height <= 0.0 {
            return *o;
        }
        let x = self.x.min(o.x);
        let y = self.y.min(o.y);
        Rect::new(x, y, self.right().max(o.right()) - x, self.bottom().max(o.bottom()) - y)
    }
}

/// Top/right/bottom/left widths (margins, borders, padding).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Edges {
    /// Top.
    pub top: f32,
    /// Right.
    pub right: f32,
    /// Bottom.
    pub bottom: f32,
    /// Left.
    pub left: f32,
}

impl Edges {
    /// All four edges equal.
    pub const fn all(v: f32) -> Edges {
        Edges {
            top: v,
            right: v,
            bottom: v,
            left: v,
        }
    }

    /// left + right
    pub fn horizontal(&self) -> f32 {
        self.left + self.right
    }

    /// top + bottom
    pub fn vertical(&self) -> f32 {
        self.top + self.bottom
    }

    /// Component-wise sum.
    pub fn add(&self, o: &Edges) -> Edges {
        Edges {
            top: self.top + o.top,
            right: self.right + o.right,
            bottom: self.bottom + o.bottom,
            left: self.left + o.left,
        }
    }
}

/// Resources layout needs besides the DOM: decoded images keyed by absolute URL, and
/// the document base URL for resolving relative ones.
#[derive(Default)]
pub struct Resources {
    /// Decoded images by absolute URL.
    pub images: HashMap<String, Rc<Image>>,
    /// Base URL of the document.
    pub base_url: Option<url::Url>,
    /// SVG images by absolute URL.
    pub svgs: HashMap<String, Rc<crate::svg::SvgSource>>,
}

impl Resources {
    /// Resolves `src` against the base URL.
    pub fn resolve(&self, src: &str) -> Option<String> {
        let src = src.trim();
        match &self.base_url {
            Some(base) => base.join(src).ok().map(|u| u.to_string()),
            None => url::Url::parse(src).ok().map(|u| u.to_string()),
        }
    }

    /// The parsed SVG image for `src`, if it was loaded.
    pub fn svg(&self, src: &str) -> Option<Rc<crate::svg::SvgSource>> {
        let abs = self.resolve(src)?;
        self.svgs.get(&abs).cloned()
    }

    /// The decoded image for `src`, if it was loaded.
    pub fn image(&self, src: &str) -> Option<Rc<Image>> {
        let abs = self.resolve(src)?;
        self.images.get(&abs).cloned()
    }
}

/// Shared state for one layout pass.
pub struct LayoutContext<'a> {
    /// The document.
    pub doc: &'a Document,
    /// Computed styles.
    pub styles: &'a StyledDocument,
    /// Fonts.
    pub fonts: &'a FontDb,
    /// Images.
    pub resources: &'a Resources,
    /// Viewport size.
    pub viewport: (f32, f32),
    /// Absolutely positioned boxes discovered during flow layout, indexed by the
    /// placeholders left in the fragment tree.
    pub(crate) abs_boxes: RefCell<Vec<&'a LayoutBox>>,
    /// Border widths overriding a box's computed ones (collapsed table borders), keyed
    /// by box address.
    border_overrides: RefCell<HashMap<usize, Edges>>,
    /// Font selection memoized by the address of the (shared) family list, which is
    /// stable while the styles live.
    face_cache: RefCell<crate::util::FxHashMap<(usize, u16, bool), crate::text::FaceId>>,
}

impl<'a> LayoutContext<'a> {
    /// Creates a context.
    pub fn new(
        doc: &'a Document,
        styles: &'a StyledDocument,
        fonts: &'a FontDb,
        resources: &'a Resources,
        viewport: (f32, f32),
    ) -> Self {
        LayoutContext {
            doc,
            styles,
            fonts,
            resources,
            viewport,
            abs_boxes: RefCell::new(Vec::new()),
            border_overrides: RefCell::new(HashMap::new()),
            face_cache: RefCell::new(Default::default()),
        }
    }

    /// The font face for a style (memoized per layout pass).
    pub fn face_for(&self, style: &ComputedStyle) -> crate::text::FaceId {
        let italic = style.font_style != crate::style::computed::FontStyle::Normal;
        let key = (style.font_family.as_ptr() as usize, style.font_weight, italic);
        if let Some(&f) = self.face_cache.borrow().get(&key) {
            return f;
        }
        let f = self.fonts.select(&style.font_family, style.font_weight, italic);
        self.face_cache.borrow_mut().insert(key, f);
        f
    }

    /// Makes `b` use `border` instead of its computed border widths.
    pub(crate) fn set_border_override(&self, b: &LayoutBox, border: Edges) {
        self.border_overrides
            .borrow_mut()
            .insert(b as *const LayoutBox as usize, border);
    }

    /// The border override for `b`, if any.
    pub(crate) fn border_override(&self, b: &LayoutBox) -> Option<Edges> {
        self.border_overrides
            .borrow()
            .get(&(b as *const LayoutBox as usize))
            .copied()
    }

    /// Registers an absolutely positioned box and returns its placeholder index.
    pub(crate) fn register_abs(&self, b: &'a LayoutBox) -> usize {
        let mut v = self.abs_boxes.borrow_mut();
        v.push(b);
        v.len() - 1
    }
}

/// The result of laying out a document.
pub struct LayoutTree {
    /// The root fragment, covering the initial containing block (the viewport) and
    /// containing the root element's fragment plus fixed-position boxes.
    pub root: Fragment,
    /// Total document height (for scrolling).
    pub document_height: f32,
    /// Total document width.
    pub document_width: f32,
}

/// Lays out a styled document at the given viewport size.
pub fn layout_document(
    doc: &Document,
    styles: &StyledDocument,
    fonts: &FontDb,
    resources: &Resources,
    viewport: (f32, f32),
) -> LayoutTree {
    let root_box = box_tree::build_box_tree(doc, styles, resources);
    let ctx = LayoutContext::new(doc, styles, fonts, resources, viewport);
    layout_box_tree(&ctx, root_box.as_ref())
}

/// Lays out an already-built box tree.
pub fn layout_box_tree<'a>(ctx: &LayoutContext<'a>, root_box: Option<&'a LayoutBox>) -> LayoutTree {
    let (vw, vh) = ctx.viewport;
    let mut viewport_frag = Fragment::new(
        FragmentKind::Viewport,
        None,
        ComputedStyle::initial(),
        Rect::new(0.0, 0.0, vw, vh),
    );
    let mut doc_h = vh;
    let mut doc_w = vw;
    if let Some(rb) = root_box {
        let cb = ContainingBlock {
            width: vw,
            height: Some(vh),
        };
        let geom = BoxGeometry::resolve(&rb.style, vw);
        let margin_left = geom.margin_or_zero().left;
        let margin_top = geom.margin_or_zero().top;
        let border_box_width = block::block_width(rb, &geom, vw, vw - margin_left - geom.margin_or_zero().right);
        let mut frag = block::layout_bfc_root(ctx, rb, &cb, Some(border_box_width), None);
        frag.rect.x = margin_left;
        frag.rect.y = margin_top;
        frag.margin = geom.margin_or_zero();
        doc_h = doc_h.max(frag.rect.bottom() + geom.margin_or_zero().bottom);
        viewport_frag.children.push(frag);
    }
    positioned::resolve_positioned(ctx, &mut viewport_frag);
    let overflow = viewport_frag.scrollable_overflow();
    doc_h = doc_h.max(overflow.bottom());
    doc_w = doc_w.max(overflow.right());
    viewport_frag.rect.height = doc_h;
    LayoutTree {
        root: viewport_frag,
        document_height: doc_h,
        document_width: doc_w,
    }
}

/// The containing block for a box's children: the content box size.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ContainingBlock {
    /// Content width.
    pub width: f32,
    /// Content height, if definite (needed for percentage heights).
    pub height: Option<f32>,
}

/// Resolved margins (with `auto` kept as `None`), borders and padding for a box.
#[derive(Clone, Copy, Debug, Default)]
pub struct BoxGeometry {
    /// Margins; `None` = auto.
    pub margin: [Option<f32>; 4],
    /// Border widths.
    pub border: Edges,
    /// Padding.
    pub padding: Edges,
}

impl BoxGeometry {
    /// Resolves geometry against the containing block width (percent margins and
    /// padding, including vertical ones, refer to the width).
    pub fn resolve(style: &ComputedStyle, cb_width: f32) -> BoxGeometry {
        let m = |l: &LengthOrAuto| l.resolve(cb_width);
        BoxGeometry {
            margin: [
                m(&style.margin[0]),
                m(&style.margin[1]),
                m(&style.margin[2]),
                m(&style.margin[3]),
            ],
            border: Edges {
                top: style.border_width[0],
                right: style.border_width[1],
                bottom: style.border_width[2],
                left: style.border_width[3],
            },
            padding: Edges {
                top: style.padding[0].resolve(cb_width).max(0.0),
                right: style.padding[1].resolve(cb_width).max(0.0),
                bottom: style.padding[2].resolve(cb_width).max(0.0),
                left: style.padding[3].resolve(cb_width).max(0.0),
            },
        }
    }

    /// Margins with `auto` treated as zero.
    pub fn margin_or_zero(&self) -> Edges {
        Edges {
            top: self.margin[0].unwrap_or(0.0),
            right: self.margin[1].unwrap_or(0.0),
            bottom: self.margin[2].unwrap_or(0.0),
            left: self.margin[3].unwrap_or(0.0),
        }
    }

    /// Border plus padding.
    pub fn border_padding(&self) -> Edges {
        self.border.add(&self.padding)
    }
}

/// Resolves a `width`-like size to a border-box width, or `None` for `auto` /
/// unresolvable percentages. Intrinsic keywords are resolved by the caller.
pub fn specified_border_box_width(style: &ComputedStyle, size: &Size, cb_width: Option<f32>, bp: f32) -> Option<f32> {
    let Size::Length(l) = size else { return None };
    let v = l.resolve_opt(cb_width)?;
    Some(match style.box_sizing {
        BoxSizing::ContentBox => v.max(0.0) + bp,
        BoxSizing::BorderBox => v.max(bp),
    })
}

/// Like [`specified_border_box_width`] for heights (percentages need a definite
/// containing block height).
pub fn specified_border_box_height(style: &ComputedStyle, size: &Size, cb_height: Option<f32>, bp: f32) -> Option<f32> {
    specified_border_box_width(style, size, cb_height, bp)
}

/// Clamps a border-box width by `min-width`/`max-width`.
pub fn clamp_width(style: &ComputedStyle, w: f32, cb_width: Option<f32>, bp: f32) -> f32 {
    let max = specified_border_box_width(style, &style.max_width, cb_width, bp).unwrap_or(f32::INFINITY);
    let min = specified_border_box_width(style, &style.min_width, cb_width, bp).unwrap_or(0.0);
    w.min(max).max(min).max(bp)
}

/// Clamps a border-box height by `min-height`/`max-height`.
pub fn clamp_height(style: &ComputedStyle, h: f32, cb_height: Option<f32>, bp: f32) -> f32 {
    let max = specified_border_box_height(style, &style.max_height, cb_height, bp).unwrap_or(f32::INFINITY);
    let min = specified_border_box_height(style, &style.min_height, cb_height, bp).unwrap_or(0.0);
    h.min(max).max(min).max(bp)
}

/// Prints the fragment tree with absolute coordinates, for `browser dump-layout`.
pub fn dump_layout(tree: &LayoutTree, doc: &Document) -> String {
    let mut out = String::new();
    fragment::dump(&tree.root, doc, 0.0, 0.0, 0, &mut out);
    out
}

#[cfg(test)]
mod tests;
