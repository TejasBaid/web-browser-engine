//! Positioned layout: resolves absolutely/fixed positioned boxes (CSS 2.1 §10.3.7 and
//! §10.6.4) against their containing blocks once those have their final size, and
//! applies `position: relative` offsets.
//!
//! Flow layout leaves an [`FragmentKind::AbsPlaceholder`] at each such box's static
//! position. This pass walks the finished fragment tree with absolute coordinates,
//! routes every placeholder to its containing block (nearest positioned ancestor, the
//! initial containing block, or the viewport for `fixed`), lays the box out there and
//! appends the result to that fragment so it moves and paints with it.

use super::block::{layout_bfc_root, replaced_size, shrink_to_fit};
use super::box_tree::{BoxContent, LayoutBox};
use super::fragment::{Fragment, FragmentKind};
use super::{
    BoxGeometry, ContainingBlock, LayoutContext, Point, specified_border_box_height, specified_border_box_width,
};
use crate::style::computed::*;

#[derive(Clone, Copy, PartialEq)]
enum Target {
    /// The viewport (fixed positioning).
    Viewport,
    /// The fragment at this depth on the current path.
    Depth(usize),
}

struct Pending {
    target: Target,
    /// Static position in absolute coordinates.
    static_pos: Point,
    index: usize,
}

/// Resolves all positioned boxes in the tree rooted at the viewport fragment.
pub fn resolve_positioned<'a>(ctx: &LayoutContext<'a>, root: &mut Fragment) {
    let mut stack: Vec<usize> = vec![0]; // the root (ICB) is a containing block
    let mut pending = Vec::new();
    visit(ctx, root, Point::default(), 0, &mut stack, &mut pending, true);
    // Everything left targets the initial containing block or the viewport.
    let left = place_into(ctx, root, Point::default(), pending, &mut stack, 0, true);
    debug_assert!(left.is_empty());
}

fn is_containing_block(f: &Fragment) -> bool {
    matches!(
        f.kind,
        FragmentKind::Box | FragmentKind::Anonymous | FragmentKind::Inline { .. }
    ) && f.style.is_positioned()
}

fn visit<'a>(
    ctx: &LayoutContext<'a>,
    frag: &mut Fragment,
    origin: Point,
    depth: usize,
    stack: &mut Vec<usize>,
    pending: &mut Vec<Pending>,
    is_root: bool,
) {
    let cb_here = !is_root && is_containing_block(frag);
    if cb_here {
        stack.push(depth);
    }
    for child in frag.children.iter_mut() {
        if child.rel_offset != (0.0, 0.0) {
            child.rect.x += child.rel_offset.0;
            child.rect.y += child.rel_offset.1;
            child.rel_offset = (0.0, 0.0);
        }
        let child_origin = Point::new(origin.x + child.rect.x, origin.y + child.rect.y);
        if let FragmentKind::AbsPlaceholder(index) = child.kind {
            let fixed = child.style.position == Position::Fixed;
            let target = if fixed {
                Target::Viewport
            } else {
                Target::Depth(*stack.last().unwrap())
            };
            pending.push(Pending {
                target,
                static_pos: child_origin,
                index,
            });
            continue;
        }
        visit(ctx, child, child_origin, depth + 1, stack, pending, false);
    }
    if cb_here {
        let (mine, rest): (Vec<Pending>, Vec<Pending>) =
            pending.drain(..).partition(|p| p.target == Target::Depth(depth));
        *pending = rest;
        if !mine.is_empty() {
            let leftovers = place_into(ctx, frag, origin, mine, stack, depth, false);
            pending.extend(leftovers);
        }
        stack.pop();
    }
}

/// Lays out `items` with `frag` as their containing block and appends them. Returns
/// placeholders found inside the new boxes that belong to other containing blocks.
fn place_into<'a>(
    ctx: &LayoutContext<'a>,
    frag: &mut Fragment,
    origin: Point,
    items: Vec<Pending>,
    stack: &mut Vec<usize>,
    depth: usize,
    is_root: bool,
) -> Vec<Pending> {
    let mut queue: std::collections::VecDeque<Pending> = items.into();
    let mut leftovers = Vec::new();
    while let Some(p) = queue.pop_front() {
        let b = ctx.abs_boxes.borrow()[p.index];
        let (cb_origin, cb_w, cb_h, border_offset) = if is_root {
            let (vw, vh) = ctx.viewport;
            (Point::default(), vw, vh, Point::default())
        } else {
            let pb = frag.padding_box();
            (
                Point::new(origin.x + pb.x, origin.y + pb.y),
                pb.width,
                pb.height,
                Point::new(pb.x, pb.y),
            )
        };
        let static_rel = Point::new(p.static_pos.x - cb_origin.x, p.static_pos.y - cb_origin.y);
        let mut f = layout_absolute(ctx, b, cb_w, cb_h, static_rel);
        f.rect.x += border_offset.x + f.rel_offset.0;
        f.rect.y += border_offset.y + f.rel_offset.1;
        f.rel_offset = (0.0, 0.0);
        if p.target == Target::Viewport {
            f.fixed = true;
        }
        let f_origin = Point::new(origin.x + f.rect.x, origin.y + f.rect.y);
        let mut inner = Vec::new();
        visit(ctx, &mut f, f_origin, depth + 1, stack, &mut inner, false);
        frag.children.push(f);
        for q in inner {
            if q.target == Target::Depth(depth) || (is_root && q.target == Target::Viewport) {
                queue.push_back(q);
            } else {
                leftovers.push(q);
            }
        }
    }
    leftovers
}

/// Lays out an absolutely positioned box in a containing block of `cb_w` x `cb_h`
/// (padding box). `static_pos` is relative to the padding box. Returns a fragment
/// positioned relative to the padding box.
pub fn layout_absolute<'a>(
    ctx: &LayoutContext<'a>,
    b: &'a LayoutBox,
    cb_w: f32,
    cb_h: f32,
    static_pos: Point,
) -> Fragment {
    let s = &b.style;
    let geom = BoxGeometry::resolve(s, cb_w);
    let bp = geom.border_padding();
    let left = s.inset[3].resolve(cb_w);
    let right = s.inset[1].resolve(cb_w);
    let top = s.inset[0].resolve(cb_h);
    let bottom = s.inset[2].resolve(cb_h);
    let cb = ContainingBlock {
        width: cb_w,
        height: Some(cb_h),
    };
    let replaced = matches!(b.content, BoxContent::Replaced(_)).then(|| replaced_size(b, &geom, &cb));
    // ---- horizontal
    let ml0 = geom.margin[3].unwrap_or(0.0);
    let mr0 = geom.margin[1].unwrap_or(0.0);
    let spec_w = replaced
        .map(|r| r.0)
        .or_else(|| specified_border_box_width(s, &s.width, Some(cb_w), bp.horizontal()));
    let left_v = left.unwrap_or(if right.is_none() { static_pos.x } else { 0.0 });
    let w = match spec_w {
        Some(w) => w,
        None => {
            if let (Some(l), Some(r)) = (left, right) {
                cb_w - l - r - ml0 - mr0
            } else {
                let avail = cb_w
                    - left.unwrap_or(if right.is_none() { static_pos.x } else { 0.0 })
                    - right.unwrap_or(0.0)
                    - ml0
                    - mr0;
                match &s.width {
                    Size::MinContent => super::intrinsic::intrinsic_widths(ctx, b).0,
                    Size::MaxContent => super::intrinsic::intrinsic_widths(ctx, b).1,
                    _ => shrink_to_fit(ctx, b, avail.max(0.0)),
                }
            }
        }
    };
    let w = super::clamp_width(s, w, Some(cb_w), bp.horizontal());
    let (ml, mr) = match (left, right) {
        (Some(l), Some(r)) => {
            let free = cb_w - l - r - w;
            match (geom.margin[3], geom.margin[1]) {
                (None, None) => {
                    if free >= 0.0 {
                        (free / 2.0, free / 2.0)
                    } else {
                        (0.0, free)
                    }
                }
                (None, Some(mr)) => (free - mr, mr),
                (Some(ml), None) => (ml, free - ml),
                (Some(ml), Some(mr)) => (ml, mr),
            }
        }
        _ => (ml0, mr0),
    };
    let x = match (left, right) {
        (Some(l), _) => l + ml,
        (None, Some(r)) => cb_w - r - mr - w,
        (None, None) => left_v + ml,
    };
    // ---- vertical
    let mt0 = geom.margin[0].unwrap_or(0.0);
    let mb0 = geom.margin[2].unwrap_or(0.0);
    let spec_h = replaced
        .map(|r| r.1)
        .or_else(|| specified_border_box_height(s, &s.height, Some(cb_h), bp.vertical()));
    let h = match (spec_h, top, bottom) {
        (Some(h), _, _) => Some(h),
        (None, Some(t), Some(bm)) => Some((cb_h - t - bm - mt0 - mb0).max(bp.vertical())),
        _ => None,
    };
    let h = h.map(|h| super::clamp_height(s, h, Some(cb_h), bp.vertical()));
    let mut frag = layout_bfc_root(ctx, b, &cb, Some(w), h);
    let fh = frag.rect.height;
    let (mt, mb) = match (top, bottom, spec_h) {
        (Some(t), Some(bm), Some(_)) => {
            let free = cb_h - t - bm - fh;
            match (geom.margin[0], geom.margin[2]) {
                (None, None) => (free / 2.0, free / 2.0),
                (None, Some(mb)) => (free - mb, mb),
                (Some(mt), None) => (mt, free - mt),
                (Some(mt), Some(mb)) => (mt, mb),
            }
        }
        _ => (mt0, mb0),
    };
    let y = match (top, bottom) {
        (Some(t), _) => t + mt,
        (None, Some(bm)) => cb_h - bm - mb - fh,
        (None, None) => static_pos.y + mt,
    };
    frag.rect.x = x;
    frag.rect.y = y;
    frag.margin = super::Edges {
        top: mt,
        right: mr,
        bottom: mb,
        left: ml,
    };
    frag
}
