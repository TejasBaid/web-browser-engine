//! Intrinsic (min-content / max-content) inline sizes, used for shrink-to-fit widths
//! (floats, inline-blocks, absolutely positioned boxes), flex base sizes and table
//! column widths.

use super::box_tree::{BoxContent, LayoutBox};
use super::{BoxGeometry, LayoutContext, specified_border_box_width};
use crate::style::computed::*;

/// Returns the (min-content, max-content) border-box widths of `b`. Cached per box.
pub fn intrinsic_widths(ctx: &LayoutContext, b: &LayoutBox) -> (f32, f32) {
    if let Some(v) = b.intrinsic.get() {
        return v;
    }
    let v = compute(ctx, b);
    b.intrinsic.set(Some(v));
    v
}

/// Like [`intrinsic_widths`] but ignores the box's own `width` property (the "content
/// size suggestion" used for flex items' automatic minimum size). Not cached.
pub fn intrinsic_content_widths(ctx: &LayoutContext, b: &LayoutBox) -> (f32, f32) {
    compute_inner(ctx, b, false)
}

fn compute(ctx: &LayoutContext, b: &LayoutBox) -> (f32, f32) {
    compute_inner(ctx, b, true)
}

fn compute_inner(ctx: &LayoutContext, b: &LayoutBox, honor_width: bool) -> (f32, f32) {
    let s = &b.style;
    let geom = BoxGeometry::resolve(s, 0.0);
    let bp = geom.border_padding().horizontal();
    // A definite (non-percentage) width fixes both sizes.
    if honor_width && let Some(w) = specified_border_box_width(s, &s.width, None, bp) {
        let w = clamp(s, w, bp);
        return (w, w);
    }
    let (min, max) = match &b.content {
        BoxContent::Replaced(_) => {
            let (w, _) = super::block::replaced_size(
                b,
                &geom,
                &super::ContainingBlock {
                    width: 0.0,
                    height: None,
                },
            );
            return (w, w);
        }
        BoxContent::Inline(ifc) => super::inline::inline_intrinsic(ctx, ifc, s),
        BoxContent::Blocks(children) => {
            let mut min: f32 = 0.0;
            let mut max: f32 = 0.0;
            let mut float_line: f32 = 0.0;
            for c in children {
                if c.is_abspos() {
                    continue;
                }
                let (cmin, cmax) = intrinsic_widths(ctx, c);
                let cg = BoxGeometry::resolve(&c.style, 0.0);
                let m = cg.margin_or_zero().horizontal();
                min = min.max(cmin + m);
                if c.is_float() {
                    // Floats can sit side by side.
                    float_line += cmax + m;
                    max = max.max(float_line);
                } else {
                    float_line = 0.0;
                    max = max.max(cmax + m);
                }
            }
            (min, max)
        }
        BoxContent::Flex(items) => {
            let row = matches!(s.flex_direction, FlexDirection::Row | FlexDirection::RowReverse);
            let gap = s.column_gap.as_ref().map(|g| g.resolve(0.0)).unwrap_or(0.0);
            let mut min: f32 = 0.0;
            let mut max: f32 = 0.0;
            let mut n = 0;
            for c in items.iter().filter(|c| !c.is_abspos()) {
                let (cmin, cmax) = intrinsic_widths(ctx, c);
                let cg = BoxGeometry::resolve(&c.style, 0.0);
                let m = cg.margin_or_zero().horizontal();
                if row {
                    if s.flex_wrap == FlexWrap::Nowrap {
                        min += cmin + m;
                    } else {
                        min = min.max(cmin + m);
                    }
                    max += cmax + m;
                } else {
                    min = min.max(cmin + m);
                    max = max.max(cmax + m);
                }
                n += 1;
            }
            if row && n > 1 {
                max += gap * (n - 1) as f32;
                if s.flex_wrap == FlexWrap::Nowrap {
                    min += gap * (n - 1) as f32;
                }
            }
            (min, max)
        }
        BoxContent::Grid(items) => super::grid::grid_intrinsic(ctx, b, items),
        BoxContent::Table(_) => {
            let (min, max) = super::table::table_intrinsic(ctx, b);
            return (min, max);
        }
    };
    let min_w = clamp(s, min + bp, bp);
    let max_w = clamp(s, max.max(min) + bp, bp);
    (min_w, max_w)
}

fn clamp(s: &ComputedStyle, w: f32, bp: f32) -> f32 {
    let max = specified_border_box_width(s, &s.max_width, None, bp).unwrap_or(f32::INFINITY);
    let min = specified_border_box_width(s, &s.min_width, None, bp).unwrap_or(0.0);
    w.min(max).max(min)
}
