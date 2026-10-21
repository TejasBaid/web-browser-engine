//! Table layout: the automatic table layout algorithm (column min/max widths from cell
//! contents, colspan distribution, percentage columns), `table-layout: fixed`, row
//! heights with rowspan, `border-spacing`, captions and cell `vertical-align`.

use super::block::{layout_bfc_root, relative_offset};
use super::box_tree::{BoxContent, Cell_, LayoutBox, TableContent};
use super::fragment::{CollapsedBorder, Fragment, FragmentKind};
use super::intrinsic::intrinsic_content_widths;
use super::{
    BoxGeometry, ContainingBlock, Edges, LayoutContext, Rect, clamp_height, specified_border_box_height,
    specified_border_box_width,
};
use crate::css::color::Color;
use crate::css::values::LengthPercentage;
use crate::style::computed::*;
use std::rc::Rc;

struct GridCell<'a> {
    row: usize, // global row index
    col: usize,
    cell: &'a Cell_,
}

struct Grid<'a> {
    cells: Vec<GridCell<'a>>,
    rows: Vec<(usize, usize)>, // (group index, row index within group)
    ncols: usize,
}

fn build_grid(t: &TableContent) -> Grid<'_> {
    let mut cells = Vec::new();
    let mut rows = Vec::new();
    let mut occupied: Vec<Vec<bool>> = Vec::new();
    for (gi, g) in t.groups.iter().enumerate() {
        for (ri, r) in g.rows.iter().enumerate() {
            let row_idx = rows.len();
            rows.push((gi, ri));
            if occupied.len() <= row_idx {
                occupied.resize(row_idx + 1, Vec::new());
            }
            let mut col = 0;
            for c in &r.cells {
                while occupied[row_idx].get(col).copied().unwrap_or(false) {
                    col += 1;
                }
                // Rowspan is limited to the rows of the group.
                let rowspan = c.rowspan.min(g.rows.len() - ri).max(1);
                for dr in 0..rowspan {
                    let rr = row_idx + dr;
                    if occupied.len() <= rr {
                        occupied.resize(rr + 1, Vec::new());
                    }
                    if occupied[rr].len() < col + c.colspan {
                        occupied[rr].resize(col + c.colspan, false);
                    }
                    for dc in 0..c.colspan {
                        occupied[rr][col + dc] = true;
                    }
                }
                cells.push(GridCell {
                    row: row_idx,
                    col,
                    cell: c,
                });
                col += c.colspan;
            }
        }
    }
    let ncols = cells
        .iter()
        .map(|c| c.col + c.cell.colspan)
        .max()
        .unwrap_or(0)
        .max(t.columns.len());
    Grid { cells, rows, ncols }
}

fn spacing(style: &ComputedStyle) -> (f32, f32) {
    if style.border_collapse == BorderCollapse::Collapse {
        (0.0, 0.0)
    } else {
        style.border_spacing
    }
}

struct Columns {
    min: Vec<f32>,
    max: Vec<f32>,
    pct: Vec<Option<f32>>,
    fixed: Vec<bool>,
}

fn rowspan_of(c: &GridCell, grid: &Grid) -> usize {
    let (g, _) = grid.rows[c.row];
    let group_rows_after = grid.rows[c.row..].iter().take_while(|(gg, _)| *gg == g).count();
    c.cell.rowspan.min(group_rows_after).max(1)
}

fn cell_pct(style: &ComputedStyle) -> Option<f32> {
    match &style.width {
        Size::Length(LengthPercentage::Lin(l)) if l.pct > 0.0 && l.px == 0.0 => Some(l.pct),
        _ => None,
    }
}

fn column_widths(ctx: &LayoutContext, t: &TableContent, grid: &Grid, hs: f32, adjust: Option<&[f32]>) -> Columns {
    let n = grid.ncols;
    let mut cols = Columns {
        min: vec![0.0; n],
        max: vec![0.0; n],
        pct: vec![None; n],
        fixed: vec![false; n],
    };
    for (i, w) in t.columns.iter().enumerate() {
        match w {
            Size::Length(LengthPercentage::Lin(l)) if l.pct > 0.0 => cols.pct[i] = Some(l.pct),
            Size::Length(l) => {
                let v = l.resolve(0.0);
                cols.max[i] = cols.max[i].max(v);
                cols.fixed[i] = true;
            }
            _ => {}
        }
    }
    // In the collapsing border model, cells use half of each shared border; `adjust`
    // holds, per cell (in grid order), how much narrower that makes them.
    let cell_index = |c: &GridCell| grid.cells.iter().position(|o| std::ptr::eq(o, c)).unwrap_or(0);
    let measure = |c: &GridCell| -> (f32, f32, Option<f32>, bool) {
        let b = &c.cell.b;
        let (cmin, cmax) = intrinsic_content_widths(ctx, b);
        let shrink = adjust.map(|a| a[cell_index(c)]).unwrap_or(0.0);
        let (cmin, cmax) = ((cmin - shrink).max(0.0), (cmax - shrink).max(0.0));
        let geom = BoxGeometry::resolve(&b.style, 0.0);
        let bp = geom.border_padding().horizontal();
        match specified_border_box_width(&b.style, &b.style.width, None, bp) {
            Some(w) => (cmin, w.max(cmin), None, true),
            None => (cmin, cmax.max(cmin), cell_pct(&b.style), false),
        }
    };
    for c in grid.cells.iter().filter(|c| c.cell.colspan == 1) {
        let (min, max, pct, fixed) = measure(c);
        cols.min[c.col] = cols.min[c.col].max(min);
        if fixed && !cols.fixed[c.col] {
            cols.max[c.col] = max;
            cols.fixed[c.col] = true;
        } else {
            cols.max[c.col] = cols.max[c.col].max(max);
        }
        if let Some(p) = pct {
            cols.pct[c.col] = Some(cols.pct[c.col].unwrap_or(0.0).max(p));
        }
    }
    for i in 0..n {
        cols.max[i] = cols.max[i].max(cols.min[i]);
    }
    // Spanning cells: distribute excess over the spanned columns.
    let mut spanning: Vec<&GridCell> = grid.cells.iter().filter(|c| c.cell.colspan > 1).collect();
    spanning.sort_by_key(|c| c.cell.colspan);
    for c in spanning {
        let (min, max, _, _) = measure(c);
        let range = c.col..(c.col + c.cell.colspan).min(n);
        // Spacing between the spanned columns is part of the cell's width.
        let inner_spacing = hs * (range.len().saturating_sub(1)) as f32;
        let (min, max) = ((min - inner_spacing).max(0.0), (max - inner_spacing).max(0.0));
        let span_min: f32 = cols.min[range.clone()].iter().sum();
        let span_max: f32 = cols.max[range.clone()].iter().sum();
        let k = range.len() as f32;
        if min > span_min {
            let extra = min - span_min;
            for i in range.clone() {
                let share = if span_max > 0.0 {
                    cols.max[i] / span_max
                } else {
                    1.0 / k
                };
                cols.min[i] += extra * share;
            }
        }
        let span_max2: f32 = cols.max[range.clone()].iter().sum();
        if max > span_max2 {
            let extra = max - span_max2;
            for i in range.clone() {
                let share = if span_max2 > 0.0 {
                    cols.max[i] / span_max2
                } else {
                    1.0 / k
                };
                cols.max[i] += extra * share;
            }
        }
        for i in range {
            cols.max[i] = cols.max[i].max(cols.min[i]);
        }
    }
    cols
}

/// (min-content, max-content) border-box widths of a table box.
pub fn table_intrinsic(ctx: &LayoutContext, b: &LayoutBox) -> (f32, f32) {
    let BoxContent::Table(t) = &b.content else {
        return (0.0, 0.0);
    };
    let grid = build_grid(t);
    let (hs, _) = spacing(&b.style);
    let mut geom = BoxGeometry::resolve(&b.style, 0.0);
    let collapsed = (b.style.border_collapse == BorderCollapse::Collapse).then(|| collapse_borders(t, &grid, &b.style));
    let adjust: Option<Vec<f32>> = collapsed.as_ref().map(|c| collapse_adjustments(&grid, c));
    if let Some(c) = &collapsed {
        geom.border = c.table_half;
    }
    let cols = column_widths(ctx, t, &grid, hs, adjust.as_deref());
    let bp = geom.border_padding().horizontal();
    let sp = if grid.ncols > 0 {
        hs * (grid.ncols + 1) as f32
    } else {
        0.0
    };
    let min = cols.min.iter().sum::<f32>() + sp + bp;
    let max = cols.max.iter().sum::<f32>() + sp + bp;
    let cap = t
        .captions
        .iter()
        .map(|c| super::intrinsic::intrinsic_widths(ctx, c).0)
        .fold(0.0, f32::max);
    if let Some(w) = specified_border_box_width(&b.style, &b.style.width, None, bp) {
        let w = w.max(min);
        return (w, w);
    }
    (min.max(cap), max.max(min).max(cap))
}

/// Lays out a table box. Returns the table's fragment (wrapped together with its
/// captions in an anonymous fragment when it has captions).
#[allow(clippy::needless_range_loop)]
pub fn layout_table<'a>(
    ctx: &LayoutContext<'a>,
    b: &'a LayoutBox,
    cb: &ContainingBlock,
    width: Option<f32>,
    height: Option<f32>,
) -> Fragment {
    let BoxContent::Table(t) = &b.content else {
        unreachable!()
    };
    let style = &b.style;
    let mut geom = BoxGeometry::resolve(style, cb.width);
    let grid = build_grid(t);
    let (hs, vs) = spacing(style);
    let collapsed = (style.border_collapse == BorderCollapse::Collapse).then(|| collapse_borders(t, &grid, style));
    if let Some(c) = &collapsed {
        // The table's own border is drawn by the edge cells; it contributes half its
        // collapsed width to the table box.
        geom.border = c.table_half;
        for (cell, half) in grid.cells.iter().zip(&c.half) {
            ctx.set_border_override(&cell.cell.b, *half);
        }
    }
    let bp = geom.border_padding();
    let adjust: Option<Vec<f32>> = collapsed.as_ref().map(|c| collapse_adjustments(&grid, c));
    let cols = column_widths(ctx, t, &grid, hs, adjust.as_deref());
    let n = grid.ncols;
    let sp_total = if n > 0 { hs * (n + 1) as f32 } else { 0.0 };
    let min_table = cols.min.iter().sum::<f32>() + sp_total + bp.horizontal();
    let max_table = cols.max.iter().sum::<f32>() + sp_total + bp.horizontal();
    let m = geom.margin_or_zero();
    let used_width = match width {
        Some(w) => w.max(min_table),
        None => match specified_border_box_width(style, &style.width, Some(cb.width), bp.horizontal()) {
            Some(w) => w.max(min_table),
            None => max_table.min(cb.width - m.horizontal()).max(min_table),
        },
    };
    // ---- distribute column widths
    let avail = (used_width - sp_total - bp.horizontal()).max(0.0);
    let mut widths = vec![0.0f32; n];
    if style.table_layout == TableLayout::Fixed && width.is_some()
        || style.table_layout == TableLayout::Fixed && !style.width.is_auto()
    {
        // Fixed layout: widths from the first row / <col>, rest shared equally.
        let mut assigned = 0.0;
        let mut unassigned = Vec::new();
        for i in 0..n {
            if cols.fixed[i] {
                widths[i] = cols.max[i];
                assigned += widths[i];
            } else if let Some(p) = cols.pct[i] {
                widths[i] = avail * p / 100.0;
                assigned += widths[i];
            } else {
                unassigned.push(i);
            }
        }
        let rest = (avail - assigned).max(0.0);
        for &i in &unassigned {
            widths[i] = rest / unassigned.len() as f32;
        }
    } else {
        let mut remaining = avail;
        let mut auto_cols = Vec::new();
        for i in 0..n {
            if let Some(p) = cols.pct[i] {
                widths[i] = (avail * p / 100.0).max(cols.min[i]);
                remaining -= widths[i];
            } else {
                auto_cols.push(i);
            }
        }
        let sum_min: f32 = auto_cols.iter().map(|&i| cols.min[i]).sum();
        let sum_max: f32 = auto_cols.iter().map(|&i| cols.max[i]).sum();
        if remaining >= sum_max {
            let extra = remaining - sum_max;
            // Prefer growing non-fixed columns.
            let flexible: Vec<usize> = auto_cols.iter().copied().filter(|&i| !cols.fixed[i]).collect();
            let targets = if flexible.is_empty() {
                auto_cols.clone()
            } else {
                flexible
            };
            let tmax: f32 = targets.iter().map(|&i| cols.max[i]).sum();
            for &i in &auto_cols {
                widths[i] = cols.max[i];
            }
            for &i in &targets {
                widths[i] += if tmax > 0.0 {
                    extra * cols.max[i] / tmax
                } else {
                    extra / targets.len() as f32
                };
            }
        } else if remaining >= sum_min && sum_max > sum_min {
            let ratio = (remaining - sum_min) / (sum_max - sum_min);
            for &i in &auto_cols {
                widths[i] = cols.min[i] + (cols.max[i] - cols.min[i]) * ratio;
            }
        } else {
            for &i in &auto_cols {
                widths[i] = cols.min[i];
            }
        }
    }
    let col_x: Vec<f32> = {
        let mut xs = Vec::with_capacity(n + 1);
        let mut x = hs;
        for w in &widths {
            xs.push(x);
            x += w + hs;
        }
        xs.push(x);
        xs
    };
    let span_width = |col: usize, span: usize| -> f32 {
        let end = (col + span).min(n);
        widths[col..end].iter().sum::<f32>() + hs * (end - col).saturating_sub(1) as f32
    };

    // ---- lay out cells, compute row heights
    let nrows = grid.rows.len();
    let content_w = used_width - bp.horizontal();
    let cell_cb = ContainingBlock {
        width: content_w,
        height: None,
    };
    let mut cell_frags: Vec<Option<Fragment>> = Vec::with_capacity(grid.cells.len());
    let mut natural_heights: Vec<f32> = Vec::with_capacity(grid.cells.len());
    let mut row_h = vec![0.0f32; nrows];
    let mut row_baseline = vec![0.0f32; nrows];
    let mut row_below = vec![0.0f32; nrows];
    for (gi_row, &(g, r)) in grid.rows.iter().enumerate() {
        let row = &t.groups[g].rows[r];
        if let Some(h) = specified_border_box_height(&row.style, &row.style.height, None, 0.0) {
            row_h[gi_row] = h;
        }
    }
    for c in &grid.cells {
        let w = span_width(c.col, c.cell.colspan);
        let mut frag = layout_bfc_root(ctx, &c.cell.b, &cell_cb, Some(w), None);
        if let Some(col) = &collapsed {
            let ci = cell_frags.len();
            frag.collapsed_border = Some(Box::new(col.paint[ci].clone()));
        }
        // The content's own extent (ignoring a specified height), for vertical-align.
        let content_bottom = frag
            .children
            .iter()
            .map(|ch| ch.rect.bottom())
            .fold(frag.border.top + frag.padding.top, f32::max);
        natural_heights.push((content_bottom + frag.padding.bottom + frag.border.bottom).min(frag.rect.height));
        let cs = &c.cell.b.style;
        // A specified cell height acts as a minimum.
        if let Some(h) =
            specified_border_box_height(cs, &cs.height, None, frag.border.vertical() + frag.padding.vertical())
            && h > frag.rect.height
        {
            frag.rect.height = h;
        }
        if rowspan_of(c, &grid) == 1 {
            if cs.vertical_align == VerticalAlign::Baseline {
                let bl = frag
                    .first_baseline
                    .unwrap_or(frag.rect.height - frag.border.bottom - frag.padding.bottom);
                row_baseline[c.row] = row_baseline[c.row].max(bl);
                row_below[c.row] = row_below[c.row].max(frag.rect.height - bl);
            }
            row_h[c.row] = row_h[c.row].max(frag.rect.height);
        }
        cell_frags.push(Some(frag));
    }
    for i in 0..nrows {
        row_h[i] = row_h[i].max(row_baseline[i] + row_below[i]);
    }
    // Rowspans: make the spanned rows tall enough.
    for (ci, c) in grid.cells.iter().enumerate() {
        let span = rowspan_of(c, &grid);
        if span > 1 {
            let h = cell_frags[ci].as_ref().unwrap().rect.height;
            let end = (c.row + span).min(nrows);
            let cur: f32 = row_h[c.row..end].iter().sum::<f32>() + vs * (end - c.row - 1) as f32;
            if h > cur {
                row_h[end - 1] += h - cur;
            }
        }
    }
    // Specified table height: distribute extra to rows.
    let grid_h = row_h.iter().sum::<f32>() + if nrows > 0 { vs * (nrows + 1) as f32 } else { 0.0 };
    let spec_h = height.or_else(|| specified_border_box_height(style, &style.height, cb.height, bp.vertical()));
    if let Some(h) = spec_h {
        let extra = h - bp.vertical() - grid_h;
        if extra > 0.0 && nrows > 0 {
            let total: f32 = row_h.iter().sum();
            for rh in row_h.iter_mut() {
                *rh += if total > 0.0 {
                    extra * *rh / total
                } else {
                    extra / nrows as f32
                };
            }
        }
    }
    let mut row_y = Vec::with_capacity(nrows);
    {
        let mut y = vs;
        for h in &row_h {
            row_y.push(y);
            y += h + vs;
        }
    }
    let grid_height = row_h.iter().sum::<f32>() + if nrows > 0 { vs * (nrows + 1) as f32 } else { 0.0 };
    let table_h = clamp_height(
        style,
        (grid_height + bp.vertical()).max(spec_h.unwrap_or(0.0)),
        cb.height,
        bp.vertical(),
    );

    // ---- build fragments: groups > rows > cells
    let mut group_frags: Vec<Fragment> = Vec::new();
    let mut first_baseline = None;
    let inner_w = col_x[n] - 0.0;
    for (gi, g) in t.groups.iter().enumerate() {
        let rows_in_group: Vec<usize> = (0..nrows).filter(|&r| grid.rows[r].0 == gi).collect();
        if rows_in_group.is_empty() {
            continue;
        }
        let gy = row_y[rows_in_group[0]];
        let last = *rows_in_group.last().unwrap();
        let gh = row_y[last] + row_h[last] - gy;
        let mut gf = Fragment::new(
            if g.node.is_some() {
                FragmentKind::Box
            } else {
                FragmentKind::Anonymous
            },
            g.node,
            g.style.clone(),
            Rect::new(bp.left + hs, bp.top + gy, (inner_w - 2.0 * hs).max(0.0), gh),
        );
        for &r in &rows_in_group {
            let (_, ri) = grid.rows[r];
            let row = &g.rows[ri];
            let mut rf = Fragment::new(
                if row.node.is_some() {
                    FragmentKind::Box
                } else {
                    FragmentKind::Anonymous
                },
                row.node,
                row.style.clone(),
                Rect::new(0.0, row_y[r] - gy, gf.rect.width, row_h[r]),
            );
            for (ci, c) in grid.cells.iter().enumerate() {
                if c.row != r {
                    continue;
                }
                let span = rowspan_of(c, &grid);
                let end = (r + span).min(nrows);
                let h = row_y[end - 1] + row_h[end - 1] - row_y[r];
                let mut f = cell_frags[ci].take().unwrap();
                let natural = natural_heights[ci];
                let content_bottom = natural;
                let dy = match f.style.vertical_align {
                    VerticalAlign::Top => 0.0,
                    VerticalAlign::Bottom => h - content_bottom,
                    VerticalAlign::Baseline if span == 1 => {
                        let bl = f.first_baseline.unwrap_or(natural - f.border.bottom - f.padding.bottom);
                        row_baseline[r] - bl
                    }
                    VerticalAlign::Middle | VerticalAlign::Baseline => (h - content_bottom) / 2.0,
                    _ => 0.0,
                };
                if dy != 0.0 {
                    for ch in f.children.iter_mut() {
                        ch.rect.y += dy;
                    }
                    f.first_baseline = f.first_baseline.map(|b| b + dy);
                    f.last_baseline = f.last_baseline.map(|b| b + dy);
                }
                f.rect.height = h;
                f.rect.x = col_x[c.col] - hs;
                f.rect.y = 0.0;
                if first_baseline.is_none() && r == 0 {
                    first_baseline = f.first_baseline.map(|b| b + bp.top + row_y[r]);
                }
                rf.children.push(f);
            }
            gf.children.push(rf);
        }
        group_frags.push(gf);
    }
    let mut table = Fragment::new(
        if b.anonymous {
            FragmentKind::Anonymous
        } else {
            FragmentKind::Box
        },
        b.node,
        b.style.clone(),
        Rect::new(0.0, 0.0, used_width, table_h),
    );
    table.border = geom.border;
    table.padding = geom.padding;
    if collapsed.is_some() {
        // Edge cells paint the table's border.
        table.collapsed_border = Some(Box::new(CollapsedBorder::default()));
    }
    table.margin = m;
    table.children = group_frags;
    table.first_baseline = first_baseline.or(Some(table_h));
    table.last_baseline = table.first_baseline;
    table.rel_offset = relative_offset(style, cb.width, cb.height);
    if t.captions.is_empty() {
        return table;
    }
    // Captions go above (or below, with `caption-side: bottom`) the table inside an
    // anonymous wrapper.
    let mut wrapper = Fragment::new(
        FragmentKind::Anonymous,
        None,
        Rc::new(ComputedStyle::anonymous(style, Display::Block)),
        Rect::new(0.0, 0.0, used_width, 0.0),
    );
    let cap_cb = ContainingBlock {
        width: used_width,
        height: None,
    };
    let layout_caption = |c: &'a LayoutBox, y: &mut f32, out: &mut Vec<Fragment>| {
        let cg = BoxGeometry::resolve(&c.style, used_width);
        let cm = cg.margin_or_zero();
        let mut f = layout_bfc_root(ctx, c, &cap_cb, Some(used_width - cm.horizontal()), None);
        *y += cm.top;
        f.rect.x = cm.left;
        f.rect.y = *y;
        *y += f.rect.height + cm.bottom;
        out.push(f);
    };
    let mut y = 0.0;
    let mut caps = Vec::new();
    for c in t.captions.iter().filter(|c| !c.style.caption_side_bottom) {
        layout_caption(c, &mut y, &mut caps);
    }
    table.rect.y = y;
    table.rel_offset = (0.0, 0.0);
    wrapper.first_baseline = table.first_baseline.map(|b| b + y);
    wrapper.last_baseline = wrapper.first_baseline;
    y += table.rect.height;
    wrapper.children = caps;
    wrapper.children.push(table);
    let mut bottom_caps = Vec::new();
    for c in t.captions.iter().filter(|c| c.style.caption_side_bottom) {
        layout_caption(c, &mut y, &mut bottom_caps);
    }
    wrapper.children.extend(bottom_caps);
    wrapper.rect.height = y;
    wrapper.margin = m;
    wrapper.rel_offset = relative_offset(style, cb.width, cb.height);
    wrapper
}

/// How much narrower each cell's border box is with collapsed (half) borders.
fn collapse_adjustments(grid: &Grid, c: &Collapsed) -> Vec<f32> {
    grid.cells
        .iter()
        .zip(&c.half)
        .map(|(cell, half)| {
            let s = &cell.cell.b.style;
            (s.border_width[1] + s.border_width[3]) - half.horizontal()
        })
        .collect()
}

/// Resolved borders in the collapsing border model.
struct Collapsed {
    /// Per cell (grid order): the half widths used for layout.
    half: Vec<Edges>,
    /// Per cell: the full borders to paint, centred on the cell's edges.
    paint: Vec<CollapsedBorder>,
    /// Half of the widest collapsed border on each outer edge of the table.
    table_half: Edges,
}

/// Collapsing border conflict resolution (CSS 2.1 §17.6.2.1, simplified): for each
/// edge, the wider of the two adjoining borders wins (`hidden` suppresses, `none`
/// contributes nothing); on outer edges the table's own border competes too.
fn collapse_borders(t: &TableContent, grid: &Grid, table: &ComputedStyle) -> Collapsed {
    let _ = t;
    let nrows = grid.rows.len();
    let ncols = grid.ncols;
    let mut owner = vec![vec![usize::MAX; ncols]; nrows];
    for (i, c) in grid.cells.iter().enumerate() {
        let span = rowspan_of(c, grid);
        for row in owner.iter_mut().skip(c.row).take(span) {
            for slot in row.iter_mut().skip(c.col).take(c.cell.colspan) {
                *slot = i;
            }
        }
    }
    type Side = (f32, Color, BorderStyle);
    let side_of = |s: &ComputedStyle, k: usize| -> Side {
        let st = s.border_style[k];
        let w = if matches!(st, BorderStyle::None | BorderStyle::Hidden) {
            0.0
        } else {
            s.border_width[k]
        };
        (w, s.border_color[k], st)
    };
    let pick = |a: Side, b: Side| -> Side {
        if a.2 == BorderStyle::Hidden || b.2 == BorderStyle::Hidden {
            return (0.0, a.1, BorderStyle::Hidden);
        }
        if b.0 > a.0 { b } else { a }
    };
    let mut half = Vec::with_capacity(grid.cells.len());
    let mut paint = Vec::with_capacity(grid.cells.len());
    let mut table_half = Edges::default();
    for c in &grid.cells {
        let s = &c.cell.b.style;
        let span = rowspan_of(c, grid);
        let (r0, r1, c0, c1) = (c.row, c.row + span, c.col, c.col + c.cell.colspan);
        let neighbor = |r: usize, col: usize, k: usize| -> Option<Side> {
            let i = *owner.get(r)?.get(col)?;
            (i != usize::MAX).then(|| side_of(&grid.cells[i].cell.b.style, k))
        };
        let top = if r0 == 0 {
            pick(side_of(s, 0), side_of(table, 0))
        } else {
            neighbor(r0 - 1, c0, 2).map_or(side_of(s, 0), |n| pick(side_of(s, 0), n))
        };
        let bottom = if r1 >= nrows {
            pick(side_of(s, 2), side_of(table, 2))
        } else {
            neighbor(r1, c0, 0).map_or(side_of(s, 2), |n| pick(side_of(s, 2), n))
        };
        let left = if c0 == 0 {
            pick(side_of(s, 3), side_of(table, 3))
        } else {
            neighbor(r0, c0 - 1, 1).map_or(side_of(s, 3), |n| pick(side_of(s, 3), n))
        };
        let right = if c1 >= ncols {
            pick(side_of(s, 1), side_of(table, 1))
        } else {
            neighbor(r0, c1, 3).map_or(side_of(s, 1), |n| pick(side_of(s, 1), n))
        };
        if r0 == 0 {
            table_half.top = table_half.top.max(top.0 / 2.0);
        }
        if r1 >= nrows {
            table_half.bottom = table_half.bottom.max(bottom.0 / 2.0);
        }
        if c0 == 0 {
            table_half.left = table_half.left.max(left.0 / 2.0);
        }
        if c1 >= ncols {
            table_half.right = table_half.right.max(right.0 / 2.0);
        }
        half.push(Edges {
            top: top.0 / 2.0,
            right: right.0 / 2.0,
            bottom: bottom.0 / 2.0,
            left: left.0 / 2.0,
        });
        paint.push(CollapsedBorder {
            widths: Edges {
                top: top.0,
                right: right.0,
                bottom: bottom.0,
                left: left.0,
            },
            colors: [top.1, right.1, bottom.1, left.1],
            styles: [top.2, right.2, bottom.2, left.2],
        });
    }
    Collapsed {
        half,
        paint,
        table_half,
    }
}
