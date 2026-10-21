//! Block formatting contexts: normal flow, width computation (CSS 2.1 §10.3), margin
//! collapsing, floats and clearance, and boxes that establish independent formatting
//! contexts.
//!
//! **Margin collapsing.** A box's position in its BFC must be final before its children
//! are laid out, because floats inside it are placed in BFC coordinates. So instead of
//! resolving collapsed margins after the fact, the collapsed *top* margin of a box (its
//! own margin plus those of first children that collapse with it, and of empty boxes
//! margins collapse through) is computed up front from styles ([`top_strut`]). The
//! parent then places the child at `cursor + resolve(pending ⊕ top_strut(child))`, and
//! the child lays out its first in-flow children at its content top, having "absorbed"
//! their margins. Bottom margins flow upward as a [`MarginStrut`] returned from layout.

use super::box_tree::{BoxContent, LayoutBox};
use super::floats::{FloatContext, Side};
use super::fragment::{Fragment, FragmentKind};
use super::intrinsic::intrinsic_widths;
use super::{
    BoxGeometry, ContainingBlock, LayoutContext, Point, Rect, clamp_height, clamp_width, specified_border_box_height,
    specified_border_box_width,
};
use crate::style::computed::*;

/// A set of adjoining margins being collapsed: the largest positive and the most
/// negative margin.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct MarginStrut {
    pos: f32,
    neg: f32,
}

impl MarginStrut {
    /// A strut holding one margin.
    pub fn from_margin(m: f32) -> Self {
        let mut s = MarginStrut::default();
        s.add(m);
        s
    }

    /// Adds a margin.
    pub fn add(&mut self, m: f32) {
        if m >= 0.0 {
            self.pos = self.pos.max(m);
        } else {
            self.neg = self.neg.min(m);
        }
    }

    /// Collapses another strut into this one.
    pub fn append(&mut self, o: MarginStrut) {
        self.pos = self.pos.max(o.pos);
        self.neg = self.neg.min(o.neg);
    }

    /// The collapsed margin.
    pub fn resolve(&self) -> f32 {
        self.pos + self.neg
    }
}

/// Result of laying out the contents of a block container.
pub struct FlowResult {
    /// Child fragments positioned relative to the container's content box.
    pub children: Vec<Fragment>,
    /// Content height.
    pub height: f32,
    /// Margins escaping through the bottom (when the container's bottom margin
    /// collapses with its last child's).
    pub bottom_strut: MarginStrut,
    /// First baseline (content-box relative).
    pub first_baseline: Option<f32>,
    /// Last baseline (content-box relative).
    pub last_baseline: Option<f32>,
}

/// Offset applied by `position: relative`.
pub fn relative_offset(style: &ComputedStyle, cb_width: f32, cb_height: Option<f32>) -> (f32, f32) {
    if style.position != Position::Relative {
        return (0.0, 0.0);
    }
    let h = cb_height.unwrap_or(0.0);
    let dx = match (&style.inset[3], &style.inset[1]) {
        (LengthOrAuto::Length(l), _) => l.resolve(cb_width),
        (LengthOrAuto::Auto, LengthOrAuto::Length(r)) => -r.resolve(cb_width),
        _ => 0.0,
    };
    let dy = match (&style.inset[0], &style.inset[2]) {
        (LengthOrAuto::Length(t), _) => t.resolve(h),
        (LengthOrAuto::Auto, LengthOrAuto::Length(b)) => -b.resolve(h),
        _ => 0.0,
    };
    (dx, dy)
}

/// Shrink-to-fit border-box width: `min(max(min-content, available), max-content)`.
pub fn shrink_to_fit(ctx: &LayoutContext, b: &LayoutBox, available: f32) -> f32 {
    let (min, max) = intrinsic_widths(ctx, b);
    min.max(available).min(max)
}

/// Used size of a replaced element's border box (CSS 2.1 §10.3.2 / §10.6.2).
pub fn replaced_size(b: &LayoutBox, geom: &BoxGeometry, cb: &ContainingBlock) -> (f32, f32) {
    let s = &b.style;
    let bp = geom.border_padding();
    let natural = match &b.content {
        BoxContent::Replaced(r) => r.natural,
        _ => None,
    };
    let ratio = s
        .aspect_ratio
        .or_else(|| natural.and_then(|(w, h)| if h > 0.0 { Some(w / h) } else { None }));
    let spec_w = specified_border_box_width(s, &s.width, Some(cb.width), bp.horizontal()).map(|w| w - bp.horizontal());
    let spec_h = specified_border_box_height(s, &s.height, cb.height, bp.vertical()).map(|h| h - bp.vertical());
    let (mut w, mut h) = match (spec_w, spec_h) {
        (Some(w), Some(h)) => (w, h),
        (Some(w), None) => (w, ratio.map(|r| w / r).or(natural.map(|n| n.1)).unwrap_or(150.0)),
        (None, Some(h)) => (ratio.map(|r| h * r).or(natural.map(|n| n.0)).unwrap_or(300.0), h),
        (None, None) => natural.unwrap_or((0.0, 0.0)),
    };
    // min/max constraints, preserving the ratio when only one dimension was auto.
    let cw = clamp_width(s, w + bp.horizontal(), Some(cb.width), bp.horizontal()) - bp.horizontal();
    if (cw - w).abs() > 0.01 {
        if spec_h.is_none()
            && let Some(r) = ratio
        {
            h = cw / r;
        }
        w = cw;
    }
    let ch = clamp_height(s, h + bp.vertical(), cb.height, bp.vertical()) - bp.vertical();
    if (ch - h).abs() > 0.01 {
        if spec_w.is_none()
            && let Some(r) = ratio
        {
            w = ch * r;
        }
        h = ch;
    }
    (w + bp.horizontal(), h + bp.vertical())
}

/// Used border-box width of a block-level box in normal flow, given the containing
/// block width and the width available for its margin box.
pub fn block_width(b: &LayoutBox, geom: &BoxGeometry, cb_width: f32, available: f32) -> f32 {
    let s = &b.style;
    let bp = geom.border_padding().horizontal();
    if let BoxContent::Replaced(_) = b.content {
        return replaced_size(
            b,
            geom,
            &ContainingBlock {
                width: cb_width,
                height: None,
            },
        )
        .0;
    }
    let m = geom.margin_or_zero();
    let w = specified_border_box_width(s, &s.width, Some(cb_width), bp).unwrap_or(available - m.left - m.right);
    clamp_width(s, w, Some(cb_width), bp)
}

/// Like [`block_width`] but resolves intrinsic `width` keywords too.
fn block_width_with_intrinsic<'a>(
    ctx: &LayoutContext<'a>,
    b: &'a LayoutBox,
    geom: &BoxGeometry,
    cb_width: f32,
    available: f32,
) -> f32 {
    let s = &b.style;
    let bp = geom.border_padding().horizontal();
    let m = geom.margin_or_zero();
    let w = match &s.width {
        Size::MinContent => Some(intrinsic_widths(ctx, b).0),
        Size::MaxContent => Some(intrinsic_widths(ctx, b).1),
        Size::FitContent => Some(shrink_to_fit(ctx, b, available - m.left - m.right)),
        _ => None,
    };
    match w {
        Some(w) => clamp_width(s, w, Some(cb_width), bp),
        None if matches!(b.content, BoxContent::Table(_)) => {
            // Tables size to their content (handled in table layout).
            let spec = specified_border_box_width(s, &s.width, Some(cb_width), bp);
            match spec {
                Some(w) => clamp_width(s, w, Some(cb_width), bp),
                None => {
                    let (min, max) = intrinsic_widths(ctx, b);
                    min.max((available - m.left - m.right).min(max))
                }
            }
        }
        None => block_width(b, geom, cb_width, available),
    }
}

/// Horizontal margins for a block-level box of border-box width `w` in a containing
/// block `cb_width` wide (auto margins center; over-constrained boxes drop
/// margin-right). `center` implements `text-align: -webkit-center` on the parent.
fn resolve_horizontal_margins(geom: &BoxGeometry, w: f32, cb_width: f32, center: bool) -> (f32, f32) {
    let free = cb_width - w;
    match (geom.margin[3], geom.margin[1]) {
        (None, None) => {
            let half = (free / 2.0).max(0.0);
            (half, free - half)
        }
        (None, Some(r)) => (free - r, r),
        (Some(l), None) => (l, free - l),
        (Some(l), Some(r)) => {
            if center && l == 0.0 && r == 0.0 && free > 0.0 {
                let half = free / 2.0;
                (half, half)
            } else {
                (l, r)
            }
        }
    }
}

/// Whether adjoining margins collapse through this (empty) box.
pub fn is_collapsible_through(b: &LayoutBox) -> bool {
    if let Some(v) = b.collapse_through.get() {
        return v;
    }
    let v = compute_collapsible_through(b);
    b.collapse_through.set(Some(v));
    v
}

fn compute_collapsible_through(b: &LayoutBox) -> bool {
    if b.establishes_bfc() || !b.is_in_flow() {
        return false;
    }
    let s = &b.style;
    let zero_or_auto = |sz: &Size| match sz {
        Size::Auto => true,
        Size::Length(l) => l.resolve(0.0) == 0.0 && !l.has_percentage(),
        _ => false,
    };
    if !zero_or_auto(&s.height) || !zero_or_auto(&s.min_height) {
        return false;
    }
    if s.border_width[0] > 0.0 || s.border_width[2] > 0.0 || !s.padding[0].is_zero() || !s.padding[2].is_zero() {
        return false;
    }
    match &b.content {
        BoxContent::Inline(ifc) => !ifc.has_content(),
        BoxContent::Blocks(children) => children.iter().filter(|c| c.is_in_flow()).all(is_collapsible_through),
        _ => false,
    }
}

/// The collapsed top margin of a box: its own top margin plus, while margins adjoin,
/// those of its first in-flow descendants (and of empty boxes in between).
pub fn top_strut(b: &LayoutBox, cb_width: f32) -> MarginStrut {
    let s = &b.style;
    let mt = s.margin[0].resolve(cb_width).unwrap_or(0.0);
    let mut strut = MarginStrut::from_margin(mt);
    if b.establishes_bfc() {
        return strut;
    }
    let content_w = cb_width; // approximation for percentage margins of descendants
    if is_collapsible_through(b) {
        if let BoxContent::Blocks(children) = &b.content {
            for c in children.iter().filter(|c| c.is_in_flow()) {
                strut.append(top_strut(c, content_w));
            }
        }
        strut.add(s.margin[2].resolve(cb_width).unwrap_or(0.0));
        return strut;
    }
    if s.border_width[0] == 0.0
        && s.padding[0].is_zero()
        && let BoxContent::Blocks(children) = &b.content
    {
        for c in children.iter().filter(|c| c.is_in_flow()) {
            if c.style.clear != Clear::None {
                break;
            }
            strut.append(top_strut(c, content_w));
            if !is_collapsible_through(c) {
                break;
            }
        }
    }
    strut
}

/// Lays out a box that establishes an independent formatting context (the root
/// element, floats, inline-blocks, flex items, table cells, `overflow` != visible, ...).
/// `width`/`height` override the used border-box size. The returned fragment is at
/// (0, 0); callers position it.
pub fn layout_bfc_root<'a>(
    ctx: &LayoutContext<'a>,
    b: &'a LayoutBox,
    cb: &ContainingBlock,
    width: Option<f32>,
    height: Option<f32>,
) -> Fragment {
    if let BoxContent::Table(_) = b.content {
        return super::table::layout_table(ctx, b, cb, width, height);
    }
    let style = &b.style;
    let mut geom = BoxGeometry::resolve(style, cb.width);
    if let Some(border) = ctx.border_override(b) {
        geom.border = border;
    }
    let bp = geom.border_padding();
    let replaced = if let BoxContent::Replaced(_) = b.content {
        Some(replaced_size(b, &geom, cb))
    } else {
        None
    };
    let w = width
        .or(replaced.map(|r| r.0))
        .unwrap_or_else(|| block_width_with_intrinsic(ctx, b, &geom, cb.width, cb.width));
    let content_w = (w - bp.horizontal()).max(0.0);
    let spec_h = height.or(replaced.map(|r| r.1)).or_else(|| {
        specified_border_box_height(style, &style.height, cb.height, bp.vertical())
            .map(|h| clamp_height(style, h, cb.height, bp.vertical()))
    });
    let child_cb = ContainingBlock {
        width: content_w,
        height: spec_h.map(|h| (h - bp.vertical()).max(0.0)),
    };
    let (mut children, content_h, first_bl, last_bl) = match &b.content {
        BoxContent::Blocks(_) | BoxContent::Inline(_) => {
            let mut floats = FloatContext::new();
            let r = layout_flow_contents(ctx, b, &child_cb, &mut floats, Point::default(), false, false);
            let h = r.height.max(floats.max_bottom());
            (r.children, h, r.first_baseline, r.last_baseline)
        }
        BoxContent::Flex(items) => {
            let r = super::flex::layout_flex(ctx, b, items, &child_cb, cb);
            (r.children, r.height, r.first_baseline, r.last_baseline)
        }
        BoxContent::Grid(items) => {
            let r = super::grid::layout_grid(ctx, b, items, &child_cb);
            (r.children, r.height, r.first_baseline, r.last_baseline)
        }
        BoxContent::Replaced(r) => {
            let ch = spec_h.unwrap_or(0.0) - bp.vertical();
            // Vector content is rasterized at twice the used size (crisp at 2x DPR).
            let image = match &r.svg {
                Some(svg) if r.image.is_none() => {
                    let (pw, ph) = ((content_w * 2.0).round() as u32, (ch.max(0.0) * 2.0).round() as u32);
                    match svg {
                        super::box_tree::SvgContent::Inline(n) => {
                            crate::svg::render_element(ctx.doc, *n, pw, ph, style.color)
                        }
                        super::box_tree::SvgContent::External(s) => crate::svg::render(s, pw, ph, style.color),
                    }
                }
                _ => r.image.clone(),
            };
            let frag = Fragment::new(
                FragmentKind::Image(image),
                b.node,
                b.style.clone(),
                Rect::new(0.0, 0.0, content_w, ch.max(0.0)),
            );
            (vec![frag], ch.max(0.0), None, None)
        }
        BoxContent::Table(_) => unreachable!(),
    };
    let h = match spec_h {
        Some(h) => h,
        None => clamp_height(style, content_h + bp.vertical(), cb.height, bp.vertical()),
    };
    let off = Point::new(bp.left, bp.top);
    for c in children.iter_mut() {
        c.rect.x += off.x;
        c.rect.y += off.y;
    }
    let mut frag = Fragment::new(
        if b.anonymous {
            FragmentKind::Anonymous
        } else {
            FragmentKind::Box
        },
        b.node,
        b.style.clone(),
        Rect::new(0.0, 0.0, w, h),
    );
    frag.border = geom.border;
    frag.padding = geom.padding;
    frag.margin = geom.margin_or_zero();
    frag.children = children;
    let clips = style.clips_overflow();
    frag.first_baseline = first_bl.map(|v| v + off.y);
    frag.last_baseline = if clips { None } else { last_bl.map(|v| v + off.y) };
    frag.rel_offset = relative_offset(style, cb.width, cb.height);
    add_translation(&mut frag);
    frag
}

/// Adds a `transform` translation (percentages of the box's own size) to the
/// fragment's paint offset.
pub fn add_translation(frag: &mut Fragment) {
    if let Some(t) = &frag.style.transform {
        frag.rel_offset.0 += t.x.resolve(frag.rect.width);
        frag.rel_offset.1 += t.y.resolve(frag.rect.height);
    }
}

/// Lays out an in-flow block container that does not establish a BFC. Returns the
/// fragment (positioned at `x`, `y` in the parent's content box) and the strut of
/// margins escaping through its bottom.
#[allow(clippy::too_many_arguments)]
fn layout_in_flow_block<'a>(
    ctx: &LayoutContext<'a>,
    b: &'a LayoutBox,
    parent_cb: &ContainingBlock,
    floats: &mut FloatContext,
    parent_origin: Point,
    x: f32,
    y: f32,
    w: f32,
) -> (Fragment, MarginStrut) {
    let style = &b.style;
    let geom = BoxGeometry::resolve(style, parent_cb.width);
    let bp = geom.border_padding();
    let content_w = (w - bp.horizontal()).max(0.0);
    let spec_h = specified_border_box_height(style, &style.height, parent_cb.height, bp.vertical())
        .map(|h| clamp_height(style, h, parent_cb.height, bp.vertical()));
    let child_cb = ContainingBlock {
        width: content_w,
        height: spec_h.map(|h| (h - bp.vertical()).max(0.0)),
    };
    let origin = Point::new(parent_origin.x + x + bp.left, parent_origin.y + y + bp.top);
    let collapses_top = geom.border.top == 0.0 && geom.padding.top == 0.0;
    let min_h_zero = match &style.min_height {
        Size::Auto => true,
        Size::Length(l) => l.resolve(0.0) <= 0.0,
        _ => false,
    };
    let collapses_bottom = geom.border.bottom == 0.0 && geom.padding.bottom == 0.0 && spec_h.is_none() && min_h_zero;
    let r = layout_flow_contents(ctx, b, &child_cb, floats, origin, collapses_top, collapses_bottom);
    let h = match spec_h {
        Some(h) => h,
        None => clamp_height(style, r.height + bp.vertical(), parent_cb.height, bp.vertical()),
    };
    let mb = geom.margin[2].unwrap_or(0.0);
    let bottom = if collapses_bottom && (h - (r.height + bp.vertical())).abs() < 0.01 {
        let mut s = r.bottom_strut;
        s.add(mb);
        s
    } else {
        MarginStrut::from_margin(mb)
    };
    let mut children = r.children;
    for c in children.iter_mut() {
        c.rect.x += bp.left;
        c.rect.y += bp.top;
    }
    let mut frag = Fragment::new(
        if b.anonymous {
            FragmentKind::Anonymous
        } else {
            FragmentKind::Box
        },
        b.node,
        b.style.clone(),
        Rect::new(x, y, w, h),
    );
    frag.border = geom.border;
    frag.padding = geom.padding;
    frag.margin = geom.margin_or_zero();
    frag.children = children;
    frag.first_baseline = r.first_baseline.map(|v| v + bp.top);
    frag.last_baseline = r.last_baseline.map(|v| v + bp.top);
    frag.rel_offset = relative_offset(style, parent_cb.width, parent_cb.height);
    add_translation(&mut frag);
    (frag, bottom)
}

/// Lays out a float and places it in `floats`. `y` is the earliest local y (content-box
/// coordinates of the container whose origin in BFC coordinates is `origin`).
pub fn layout_float<'a>(
    ctx: &LayoutContext<'a>,
    b: &'a LayoutBox,
    cb: &ContainingBlock,
    floats: &mut FloatContext,
    origin: Point,
    y: f32,
) -> Fragment {
    let mut frag = layout_float_unplaced(ctx, b, cb);
    place_float(&mut frag, b, cb, floats, origin, y);
    frag
}

/// Lays out a float without placing it (its rect is at 0,0; margins are in `margin`).
pub fn layout_float_unplaced<'a>(ctx: &LayoutContext<'a>, b: &'a LayoutBox, cb: &ContainingBlock) -> Fragment {
    let geom = BoxGeometry::resolve(&b.style, cb.width);
    let m = geom.margin_or_zero();
    let bp = geom.border_padding().horizontal();
    let width = if let BoxContent::Replaced(_) = b.content {
        replaced_size(b, &geom, cb).0
    } else if let BoxContent::Table(_) = b.content {
        block_width_with_intrinsic(ctx, b, &geom, cb.width, cb.width)
    } else {
        match specified_border_box_width(&b.style, &b.style.width, Some(cb.width), bp) {
            Some(w) => clamp_width(&b.style, w, Some(cb.width), bp),
            None => clamp_width(
                &b.style,
                shrink_to_fit(ctx, b, cb.width - m.left - m.right),
                Some(cb.width),
                bp,
            ),
        }
    };
    let mut frag = layout_bfc_root(ctx, b, cb, Some(width), None);
    frag.margin = m;
    frag
}

/// Places an already laid out float fragment.
pub fn place_float(
    frag: &mut Fragment,
    b: &LayoutBox,
    cb: &ContainingBlock,
    floats: &mut FloatContext,
    origin: Point,
    y: f32,
) {
    let m = frag.margin;
    let side = if b.style.float == Float::Right {
        Side::Right
    } else {
        Side::Left
    };
    let mut min_y = origin.y + y;
    if b.style.clear != Clear::None {
        let (l, r) = clear_sides(b.style.clear);
        min_y = min_y.max(floats.clearance_y(l, r));
    }
    let (fx, fy) = floats.place(
        frag.rect.width + m.horizontal(),
        frag.rect.height + m.vertical(),
        side,
        min_y,
        origin.x,
        origin.x + cb.width,
    );
    frag.rect.x = fx - origin.x + m.left;
    frag.rect.y = fy - origin.y + m.top;
}

fn clear_sides(c: Clear) -> (bool, bool) {
    match c {
        Clear::Left => (true, false),
        Clear::Right => (false, true),
        Clear::Both => (true, true),
        Clear::None => (false, false),
    }
}

/// Lays out the contents of a block container (`b`) whose content box origin is at
/// `origin` in BFC coordinates.
pub fn layout_flow_contents<'a>(
    ctx: &LayoutContext<'a>,
    b: &'a LayoutBox,
    cb: &ContainingBlock,
    floats: &mut FloatContext,
    origin: Point,
    absorb_first: bool,
    collapse_bottom: bool,
) -> FlowResult {
    match &b.content {
        BoxContent::Inline(ifc) => {
            let r = super::inline::layout_inline(ctx, b, ifc, cb, floats, origin);
            FlowResult {
                children: r.children,
                height: r.height,
                bottom_strut: MarginStrut::default(),
                first_baseline: r.first_baseline,
                last_baseline: r.last_baseline,
            }
        }
        BoxContent::Blocks(children) => {
            layout_block_children(ctx, b, children, cb, floats, origin, absorb_first, collapse_bottom)
        }
        _ => FlowResult {
            children: Vec::new(),
            height: 0.0,
            bottom_strut: MarginStrut::default(),
            first_baseline: None,
            last_baseline: None,
        },
    }
}

#[allow(clippy::too_many_arguments)]
fn layout_block_children<'a>(
    ctx: &LayoutContext<'a>,
    parent: &'a LayoutBox,
    children: &'a [LayoutBox],
    cb: &ContainingBlock,
    floats: &mut FloatContext,
    origin: Point,
    absorb_first: bool,
    collapse_bottom: bool,
) -> FlowResult {
    let mut out = Vec::with_capacity(children.len());
    let mut cursor = 0.0f32;
    let mut strut = MarginStrut::default();
    let mut absorbing = absorb_first;
    let mut first_baseline = None;
    let mut last_baseline = None;
    let center = parent.style.text_align == TextAlign::WebkitCenter;
    for child in children {
        if child.is_abspos() {
            let idx = ctx.register_abs(child);
            let y = if absorbing { cursor } else { cursor + strut.resolve() };
            out.push(Fragment::new(
                FragmentKind::AbsPlaceholder(idx),
                child.node,
                child.style.clone(),
                Rect::new(0.0, y, 0.0, 0.0),
            ));
            continue;
        }
        if child.is_float() {
            let y = if absorbing { cursor } else { cursor + strut.resolve() };
            out.push(layout_float(ctx, child, cb, floats, origin, y));
            continue;
        }
        let geom = BoxGeometry::resolve(&child.style, cb.width);
        let chain = top_strut(child, cb.width);
        let mut y = if absorbing {
            cursor
        } else {
            let mut s = strut;
            s.append(chain);
            cursor + s.resolve()
        };
        if child.style.clear != Clear::None && !floats.is_empty() {
            let (l, r) = clear_sides(child.style.clear);
            let clear_y = floats.clearance_y(l, r) - origin.y;
            if clear_y > y {
                y = clear_y;
                absorbing = false;
            }
        }
        let collapsible = is_collapsible_through(child);
        let (frag, bottom) = if child.establishes_bfc() {
            layout_bfc_child(ctx, child, &geom, cb, floats, origin, &mut y, center)
        } else {
            let w = block_width_with_intrinsic(ctx, child, &geom, cb.width, cb.width);
            let (ml, _) = resolve_horizontal_margins(&geom, w, cb.width, center);
            layout_in_flow_block(ctx, child, cb, floats, origin, ml, y, w)
        };
        if collapsible {
            if !absorbing {
                strut.append(chain);
            }
        } else {
            cursor = frag.rect.y + frag.rect.height;
            strut = bottom;
            absorbing = false;
        }
        if first_baseline.is_none()
            && let Some(fb) = frag.first_baseline
        {
            first_baseline = Some(frag.rect.y + fb);
        }
        if let Some(lb) = frag.last_baseline {
            last_baseline = Some(frag.rect.y + lb);
        }
        out.push(frag);
    }
    let height = if collapse_bottom || absorbing {
        cursor
    } else {
        cursor + strut.resolve()
    };
    FlowResult {
        children: out,
        height: height.max(0.0),
        bottom_strut: strut,
        first_baseline,
        last_baseline,
    }
}

/// Places a child that establishes its own formatting context; such boxes must not
/// overlap floats in the parent BFC, so they are narrowed or moved down.
#[allow(clippy::too_many_arguments)]
fn layout_bfc_child<'a>(
    ctx: &LayoutContext<'a>,
    child: &'a LayoutBox,
    geom: &BoxGeometry,
    cb: &ContainingBlock,
    floats: &FloatContext,
    origin: Point,
    y: &mut f32,
    center: bool,
) -> (Fragment, MarginStrut) {
    let m = geom.margin_or_zero();
    let width_auto =
        child.style.width.is_auto() && !matches!(child.content, BoxContent::Replaced(_) | BoxContent::Table(_));
    let mut frag;
    loop {
        let (l, r) = if floats.is_empty() {
            (origin.x, origin.x + cb.width)
        } else {
            floats.available(origin.y + *y, 1.0, origin.x, origin.x + cb.width)
        };
        let avail = r - l;
        let narrowed = avail < cb.width - 0.01;
        let w = if width_auto && narrowed {
            clamp_width(
                &child.style,
                avail - m.left - m.right,
                Some(cb.width),
                geom.border_padding().horizontal(),
            )
        } else {
            block_width_with_intrinsic(ctx, child, geom, cb.width, cb.width)
        };
        frag = layout_bfc_root(ctx, child, cb, Some(w), None);
        if narrowed {
            // Check the whole height of the box against floats.
            let (l2, r2) = floats.available(
                origin.y + *y,
                frag.rect.height + m.vertical(),
                origin.x,
                origin.x + cb.width,
            );
            if w + m.left + m.right <= r2 - l2 + 0.01 || floats.next_bottom_below(origin.y + *y).is_none() {
                frag.rect.x = l2 - origin.x + m.left;
                break;
            }
            *y = floats.next_bottom_below(origin.y + *y).unwrap() - origin.y;
            continue;
        }
        let (ml, _) = resolve_horizontal_margins(geom, w, cb.width, center);
        frag.rect.x = ml;
        break;
    }
    frag.rect.y = *y;
    frag.margin = m;
    let bottom = MarginStrut::from_margin(geom.margin[2].unwrap_or(0.0));
    (frag, bottom)
}
