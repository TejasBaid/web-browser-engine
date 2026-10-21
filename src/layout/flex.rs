//! Flexbox layout (CSS Flexible Box Layout Level 1, §9): flex base sizes, line
//! collection, the iterative "resolve flexible lengths" loop with min/max freezing,
//! cross sizing with stretch and baseline alignment, `justify-content`,
//! `align-items`/`align-self`, `align-content`, auto margins, `gap`, `order`, and the
//! reverse directions.

use super::block::{layout_bfc_root, shrink_to_fit};
use super::box_tree::LayoutBox;
use super::fragment::{Fragment, FragmentKind};
use super::intrinsic::{intrinsic_content_widths, intrinsic_widths};
use super::{
    BoxGeometry, ContainingBlock, LayoutContext, Rect, specified_border_box_height, specified_border_box_width,
};
use crate::style::computed::*;

/// Result of flex layout (children relative to the container's content box).
pub struct FlexResult {
    /// Item fragments and placeholders.
    pub children: Vec<Fragment>,
    /// Content height of the container.
    pub height: f32,
    /// First baseline.
    pub first_baseline: Option<f32>,
    /// Last baseline.
    pub last_baseline: Option<f32>,
}

struct Item<'a> {
    b: &'a LayoutBox,
    geom: BoxGeometry,
    /// Main-axis margins (auto = None) and cross-axis margins.
    main_margins: (Option<f32>, Option<f32>),
    cross_margins: (Option<f32>, Option<f32>),
    bp_main: f32,
    bp_cross: f32,
    base: f32,
    hypothetical: f32,
    min_main: f32,
    max_main: f32,
    target: f32,
    frozen: bool,
    cross: f32,
    frag: Option<Fragment>,
    baseline: f32,
    align: AlignItems,
}

impl Item<'_> {
    fn outer_main_margins(&self) -> f32 {
        self.main_margins.0.unwrap_or(0.0) + self.main_margins.1.unwrap_or(0.0)
    }
    fn outer_cross_margins(&self) -> f32 {
        self.cross_margins.0.unwrap_or(0.0) + self.cross_margins.1.unwrap_or(0.0)
    }
}

/// Lays out a flex container's items inside its content box `cb`.
pub fn layout_flex<'a>(
    ctx: &LayoutContext<'a>,
    container: &'a LayoutBox,
    items_in: &'a [LayoutBox],
    cb: &ContainingBlock,
    _outer: &ContainingBlock,
) -> FlexResult {
    let s = &container.style;
    let row = matches!(s.flex_direction, FlexDirection::Row | FlexDirection::RowReverse);
    let reverse_main = matches!(
        s.flex_direction,
        FlexDirection::RowReverse | FlexDirection::ColumnReverse
    );
    let wrap = s.flex_wrap != FlexWrap::Nowrap;
    let wrap_reverse = s.flex_wrap == FlexWrap::WrapReverse;
    let container_main = if row { Some(cb.width) } else { cb.height };
    let container_cross = if row { cb.height } else { Some(cb.width) };
    let (main_gap_v, cross_gap_v) = if row {
        (&s.column_gap, &s.row_gap)
    } else {
        (&s.row_gap, &s.column_gap)
    };
    let main_gap = main_gap_v
        .as_ref()
        .map(|g| g.resolve(container_main.unwrap_or(0.0)))
        .unwrap_or(0.0);
    let cross_gap = cross_gap_v
        .as_ref()
        .map(|g| g.resolve(container_cross.unwrap_or(0.0)))
        .unwrap_or(0.0);

    let mut children = Vec::new();
    let mut ordered: Vec<&LayoutBox> = Vec::new();
    for b in items_in {
        if b.is_abspos() {
            let idx = ctx.register_abs(b);
            children.push(Fragment::new(
                FragmentKind::AbsPlaceholder(idx),
                b.node,
                b.style.clone(),
                Rect::new(0.0, 0.0, 0.0, 0.0),
            ));
        } else {
            ordered.push(b);
        }
    }
    ordered.sort_by_key(|b| b.style.order);

    // ---- flex base sizes and hypothetical main sizes
    let mut items: Vec<Item> = ordered
        .into_iter()
        .map(|b| {
            let geom = BoxGeometry::resolve(&b.style, cb.width);
            let bp = geom.border_padding();
            let (main_margins, cross_margins, bp_main, bp_cross) = if row {
                (
                    (geom.margin[3], geom.margin[1]),
                    (geom.margin[0], geom.margin[2]),
                    bp.horizontal(),
                    bp.vertical(),
                )
            } else {
                (
                    (geom.margin[0], geom.margin[2]),
                    (geom.margin[3], geom.margin[1]),
                    bp.vertical(),
                    bp.horizontal(),
                )
            };
            let align = match b.style.align_self {
                AlignItems::Auto => s.align_items,
                a => a,
            };
            let align = if align == AlignItems::Normal {
                AlignItems::Stretch
            } else {
                align
            };
            Item {
                b,
                geom,
                main_margins,
                cross_margins,
                bp_main,
                bp_cross,
                base: 0.0,
                hypothetical: 0.0,
                min_main: 0.0,
                max_main: f32::INFINITY,
                target: 0.0,
                frozen: false,
                cross: 0.0,
                frag: None,
                baseline: 0.0,
                align,
            }
        })
        .collect();

    for it in items.iter_mut() {
        let st = &it.b.style;
        let main_size_prop = if row { &st.width } else { &st.height };
        let spec_main = |size: &Size| -> Option<f32> {
            if row {
                specified_border_box_width(st, size, container_main, it.bp_main)
            } else {
                specified_border_box_height(st, size, container_main, it.bp_main)
            }
        };
        let basis = match &st.flex_basis {
            Size::Length(_) => spec_main(&st.flex_basis),
            Size::Auto => spec_main(main_size_prop),
            _ => None,
        };
        let content_main = |it: &Item<'a>| -> f32 {
            if row {
                intrinsic_widths(ctx, it.b).1
            } else {
                let w = item_cross_for_measure(ctx, it, cb.width);
                let f = layout_bfc_root(ctx, it.b, cb, Some(w), None);
                f.rect.height
            }
        };
        it.base = basis.unwrap_or_else(|| content_main(it));
        // min/max main sizes, including the automatic minimum size.
        let min_prop = if row { &st.min_width } else { &st.min_height };
        let max_prop = if row { &st.max_width } else { &st.max_height };
        it.max_main = spec_main(max_prop).unwrap_or(f32::INFINITY);
        it.min_main = match min_prop {
            Size::Auto if !st.clips_overflow() => {
                let content_min = if row {
                    intrinsic_content_widths(ctx, it.b).0
                } else {
                    let w = item_cross_for_measure(ctx, it, cb.width);
                    layout_bfc_root(ctx, it.b, cb, Some(w), None).rect.height
                };
                let content_min = content_min.min(it.max_main);
                match spec_main(main_size_prop) {
                    Some(spec) => spec.min(content_min),
                    None => content_min,
                }
            }
            _ => spec_main(min_prop).unwrap_or(0.0),
        }
        .max(it.bp_main);
        it.hypothetical = it.base.min(it.max_main).max(it.min_main);
    }

    // ---- collect lines
    let avail_main = container_main.unwrap_or_else(|| {
        let n = items.len();
        items
            .iter()
            .map(|i| i.hypothetical + i.outer_main_margins())
            .sum::<f32>()
            + main_gap * n.saturating_sub(1) as f32
    });
    let mut lines: Vec<Vec<usize>> = Vec::new();
    {
        let mut cur: Vec<usize> = Vec::new();
        let mut used = 0.0;
        for (i, it) in items.iter().enumerate() {
            let outer = it.hypothetical + it.outer_main_margins();
            let add = if cur.is_empty() { outer } else { outer + main_gap };
            if wrap && !cur.is_empty() && used + add > avail_main + 0.01 {
                lines.push(std::mem::take(&mut cur));
                used = 0.0;
                cur.push(i);
                used += outer;
            } else {
                cur.push(i);
                used += add;
            }
        }
        if !cur.is_empty() || lines.is_empty() {
            lines.push(cur);
        }
    }

    // ---- resolve flexible lengths per line
    for line in &lines {
        resolve_flexible_lengths(&mut items, line, avail_main, main_gap);
    }

    // ---- cross sizes
    for it in items.iter_mut() {
        let st = &it.b.style;
        if row {
            let spec_h = specified_border_box_height(st, &st.height, container_cross, it.bp_cross);
            let f = layout_bfc_root(ctx, it.b, cb, Some(it.target), spec_h);
            it.cross = f.rect.height;
            it.baseline = f.first_baseline.unwrap_or(f.rect.height) + it.cross_margins.0.unwrap_or(0.0);
            it.frag = Some(f);
        } else {
            let w = item_cross_for_measure(ctx, it, cb.width);
            let f = layout_bfc_root(ctx, it.b, cb, Some(w), Some(it.target));
            it.cross = f.rect.width;
            it.baseline = f.first_baseline.unwrap_or(f.rect.height);
            it.frag = Some(f);
        }
    }
    let single_line = !wrap;
    let mut line_cross: Vec<f32> = Vec::with_capacity(lines.len());
    let mut line_baseline: Vec<f32> = Vec::with_capacity(lines.len());
    for line in &lines {
        let mut max_outer: f32 = 0.0;
        let mut max_asc: f32 = 0.0;
        let mut max_desc: f32 = 0.0;
        for &i in line {
            let it = &items[i];
            let outer = it.cross + it.outer_cross_margins();
            if it.align == AlignItems::Baseline && row {
                max_asc = max_asc.max(it.baseline);
                max_desc = max_desc.max(outer - it.baseline);
            } else {
                max_outer = max_outer.max(outer);
            }
        }
        line_cross.push(max_outer.max(max_asc + max_desc));
        line_baseline.push(max_asc);
    }
    if single_line && let Some(cc) = container_cross {
        line_cross[0] = cc;
    }
    // ---- align-content
    let total_cross: f32 = line_cross.iter().sum::<f32>() + cross_gap * lines.len().saturating_sub(1) as f32;
    let mut line_offsets = vec![0.0; lines.len()];
    let mut extra_gap = 0.0;
    let mut start_offset = 0.0;
    if !single_line && let Some(cc) = container_cross {
        let free = cc - total_cross;
        let n = lines.len() as f32;
        match s.align_content {
            JustifyContent::Normal | JustifyContent::Stretch => {
                if free > 0.0 {
                    for lc in line_cross.iter_mut() {
                        *lc += free / n;
                    }
                }
            }
            JustifyContent::FlexEnd => start_offset = free,
            JustifyContent::Center => start_offset = free / 2.0,
            JustifyContent::SpaceBetween if n > 1.0 && free > 0.0 => extra_gap = free / (n - 1.0),
            JustifyContent::SpaceAround if free > 0.0 => {
                extra_gap = free / n;
                start_offset = extra_gap / 2.0;
            }
            JustifyContent::SpaceEvenly if free > 0.0 => {
                extra_gap = free / (n + 1.0);
                start_offset = extra_gap;
            }
            _ => {}
        }
    }
    {
        let mut pos = start_offset;
        for (li, off) in line_offsets.iter_mut().enumerate() {
            *off = pos;
            pos += line_cross[li] + cross_gap + extra_gap;
        }
    }
    let content_cross = container_cross
        .unwrap_or_else(|| line_cross.iter().sum::<f32>() + cross_gap * lines.len().saturating_sub(1) as f32);
    if wrap_reverse {
        for (li, off) in line_offsets.iter_mut().enumerate() {
            *off = content_cross - *off - line_cross[li];
        }
    }

    // ---- stretch, then main/cross positioning
    let mut first_baseline = None;
    let mut last_baseline = None;
    let mut used_main_max: f32 = 0.0;
    for (li, line) in lines.iter().enumerate() {
        let lc = line_cross[li];
        for &i in line {
            let it = &mut items[i];
            let st = &it.b.style;
            let cross_auto = if row { st.height.is_auto() } else { st.width.is_auto() };
            let has_auto_cross_margin = it.cross_margins.0.is_none() || it.cross_margins.1.is_none();
            if it.align == AlignItems::Stretch && cross_auto && !has_auto_cross_margin {
                let target = (lc - it.outer_cross_margins()).max(0.0);
                let target = if row {
                    super::clamp_height(st, target, container_cross, it.bp_cross)
                } else {
                    super::clamp_width(st, target, container_cross, it.bp_cross)
                };
                if (target - it.cross).abs() > 0.01 {
                    let f = if row {
                        layout_bfc_root(ctx, it.b, cb, Some(it.target), Some(target))
                    } else {
                        layout_bfc_root(ctx, it.b, cb, Some(target), Some(it.target))
                    };
                    it.cross = target;
                    it.frag = Some(f);
                }
            }
        }
        // Main axis.
        let n = line.len();
        let used: f32 = line
            .iter()
            .map(|&i| items[i].target + items[i].outer_main_margins())
            .sum::<f32>()
            + main_gap * n.saturating_sub(1) as f32;
        used_main_max = used_main_max.max(used);
        let mut free = avail_main - used;
        let auto_margins: usize = line
            .iter()
            .map(|&i| items[i].main_margins.0.is_none() as usize + items[i].main_margins.1.is_none() as usize)
            .sum();
        let auto_share = if free > 0.0 && auto_margins > 0 {
            let a = free / auto_margins as f32;
            free = 0.0;
            a
        } else {
            0.0
        };
        let (mut pos, between) = match s.justify_content {
            JustifyContent::FlexEnd => (free, 0.0),
            JustifyContent::Center => (free / 2.0, 0.0),
            JustifyContent::SpaceBetween => {
                if n > 1 && free > 0.0 {
                    (0.0, free / (n - 1) as f32)
                } else {
                    (0.0, 0.0)
                }
            }
            JustifyContent::SpaceAround => {
                if free > 0.0 {
                    (free / n as f32 / 2.0, free / n as f32)
                } else {
                    (free / 2.0, 0.0)
                }
            }
            JustifyContent::SpaceEvenly => {
                if free > 0.0 {
                    (free / (n + 1) as f32, free / (n + 1) as f32)
                } else {
                    (free / 2.0, 0.0)
                }
            }
            _ => (0.0, 0.0),
        };
        for &i in line {
            let it = &mut items[i];
            let ml = it.main_margins.0.unwrap_or(auto_share);
            let mr = it.main_margins.1.unwrap_or(auto_share);
            pos += ml;
            let main_pos = if reverse_main {
                avail_main - pos - it.target
            } else {
                pos
            };
            pos += it.target + mr + main_gap + between;
            // Cross axis.
            let outer = it.cross + it.outer_cross_margins();
            let free_c = lc - outer;
            let (cm_start, cm_end) = (it.cross_margins.0, it.cross_margins.1);
            // Auto margins absorb free space; otherwise align-self decides.
            let cross_pos = match (cm_start, cm_end) {
                (None, None) => free_c.max(0.0) / 2.0,
                (None, Some(_)) => free_c.max(0.0),
                (Some(st_m), None) => st_m,
                (Some(st_m), Some(_)) => {
                    let mut a = it.align;
                    if wrap_reverse {
                        a = match a {
                            AlignItems::FlexStart => AlignItems::FlexEnd,
                            AlignItems::FlexEnd => AlignItems::FlexStart,
                            x => x,
                        };
                    }
                    st_m + match a {
                        AlignItems::FlexEnd => free_c,
                        AlignItems::Center => free_c / 2.0,
                        AlignItems::Baseline if row => line_baseline[li] - it.baseline,
                        _ => 0.0,
                    }
                }
            };
            let cross_abs = line_offsets[li] + cross_pos;
            let mut f = it.frag.take().unwrap();
            if row {
                f.rect.x = main_pos;
                f.rect.y = cross_abs;
            } else {
                f.rect.x = cross_abs;
                f.rect.y = main_pos;
            }
            f.margin = it.geom.margin_or_zero();
            if first_baseline.is_none() {
                first_baseline = Some(f.rect.y + f.first_baseline.unwrap_or(f.rect.height));
            }
            last_baseline = Some(f.rect.y + f.last_baseline.unwrap_or(f.rect.height));
            children.push(f);
        }
    }
    let height = if row {
        content_cross
    } else {
        container_main.unwrap_or(used_main_max)
    };
    FlexResult {
        children,
        height,
        first_baseline,
        last_baseline,
    }
}

/// Border-box cross size (width) used to measure a column-flex item.
fn item_cross_for_measure<'a>(ctx: &LayoutContext<'a>, it: &Item<'a>, container_width: f32) -> f32 {
    let st = &it.b.style;
    let margins = it.geom.margin_or_zero().horizontal();
    if let Some(w) = specified_border_box_width(st, &st.width, Some(container_width), it.bp_cross) {
        return super::clamp_width(st, w, Some(container_width), it.bp_cross);
    }
    let stretch = it.align == AlignItems::Stretch && it.geom.margin[1].is_some() && it.geom.margin[3].is_some();
    let w = if stretch {
        container_width - margins
    } else {
        shrink_to_fit(ctx, it.b, container_width - margins)
    };
    super::clamp_width(st, w, Some(container_width), it.bp_cross)
}

/// CSS Flexbox §9.7 "Resolving Flexible Lengths" for one line.
fn resolve_flexible_lengths(items: &mut [Item], line: &[usize], avail_main: f32, gap: f32) {
    let n = line.len();
    let gaps = gap * n.saturating_sub(1) as f32;
    let sum_hyp: f32 = line
        .iter()
        .map(|&i| items[i].hypothetical + items[i].outer_main_margins())
        .sum();
    let growing = avail_main - gaps - sum_hyp > 0.0;
    for &i in line {
        let it = &mut items[i];
        it.target = it.hypothetical;
        let factor = if growing {
            it.b.style.flex_grow
        } else {
            it.b.style.flex_shrink
        };
        it.frozen = factor == 0.0 || (growing && it.base > it.hypothetical) || (!growing && it.base < it.hypothetical);
    }
    let initial_free = avail_main
        - gaps
        - line
            .iter()
            .map(|&i| {
                let it = &items[i];
                (if it.frozen { it.target } else { it.base }) + it.outer_main_margins()
            })
            .sum::<f32>();
    for _ in 0..(n + 2) {
        if line.iter().all(|&i| items[i].frozen) {
            break;
        }
        let mut remaining = avail_main
            - gaps
            - line
                .iter()
                .map(|&i| {
                    let it = &items[i];
                    (if it.frozen { it.target } else { it.base }) + it.outer_main_margins()
                })
                .sum::<f32>();
        let sum_factors: f32 = line
            .iter()
            .filter(|&&i| !items[i].frozen)
            .map(|&i| {
                if growing {
                    items[i].b.style.flex_grow
                } else {
                    items[i].b.style.flex_shrink
                }
            })
            .sum();
        if sum_factors < 1.0 {
            let scaled = initial_free * sum_factors;
            if scaled.abs() < remaining.abs() {
                remaining = scaled;
            }
        }
        if growing {
            for &i in line {
                let it = &mut items[i];
                if !it.frozen && sum_factors > 0.0 {
                    it.target = it.base + remaining * it.b.style.flex_grow / sum_factors;
                }
            }
        } else {
            let sum_scaled: f32 = line
                .iter()
                .filter(|&&i| !items[i].frozen)
                .map(|&i| items[i].b.style.flex_shrink * (items[i].base - items[i].bp_main).max(0.0))
                .sum();
            for &i in line {
                let it = &mut items[i];
                if !it.frozen {
                    let scaled = it.b.style.flex_shrink * (it.base - it.bp_main).max(0.0);
                    it.target = if sum_scaled > 0.0 {
                        it.base + remaining * scaled / sum_scaled
                    } else {
                        it.base
                    };
                }
            }
        }
        // Clamp and freeze.
        let mut total_violation = 0.0;
        let mut violations = vec![0.0f32; items.len()];
        for &i in line {
            let it = &mut items[i];
            if it.frozen {
                continue;
            }
            let clamped = it.target.min(it.max_main).max(it.min_main);
            violations[i] = clamped - it.target;
            total_violation += clamped - it.target;
            it.target = clamped;
        }
        for &i in line {
            let it = &mut items[i];
            if it.frozen {
                continue;
            }
            if total_violation.abs() < 0.001
                || (total_violation > 0.0 && violations[i] > 0.0)
                || (total_violation < 0.0 && violations[i] < 0.0)
            {
                it.frozen = true;
            }
        }
    }
}
