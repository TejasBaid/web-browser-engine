//! CSS Grid layout (a practical subset of CSS Grid Layout Level 1): explicit tracks
//! with `repeat()` (including `auto-fill`/`auto-fit`), `minmax()`, `fr`, intrinsic track
//! sizes, line-based and named-area placement, row/column auto-placement (sparse and
//! dense), implicit tracks, gaps, and item alignment with `justify-self`/`align-self`.
//!
//! The track sizing algorithm follows the spec's structure (initialize base sizes and
//! growth limits from item contributions, distribute spanning items' extra space,
//! maximize tracks, expand flexible tracks, stretch `auto` tracks) without its finer
//! points such as baseline-aligned tracks or subgrid.

use super::block::{layout_bfc_root, replaced_size};
use super::box_tree::{BoxContent, LayoutBox};
use super::flex::FlexResult;
use super::fragment::{Fragment, FragmentKind};
use super::intrinsic::intrinsic_widths;
use super::{BoxGeometry, ContainingBlock, LayoutContext, Rect, specified_border_box_width};
use crate::style::computed::*;
use crate::style::grid::{GridLine, TrackBreadth, TrackListItem, TrackRepeat, TrackSize};
use std::collections::HashMap;

/// A placed grid item: 0-based start lines and spans.
#[derive(Clone, Copy, Debug)]
struct Placement {
    col: usize,
    col_span: usize,
    row: usize,
    row_span: usize,
}

/// One axis of a placement: (start line if definite, span).
type AxisPlacement = (Option<usize>, usize);

/// Named areas: name → (row_start, row_end, col_start, col_end) as 0-based lines.
type Areas = HashMap<String, (usize, usize, usize, usize)>;

fn areas_of(style: &ComputedStyle) -> Areas {
    let mut map: Areas = HashMap::new();
    if let Some(rows) = &style.grid_template_areas {
        for (r, row) in rows.iter().enumerate() {
            for (c, name) in row.iter().enumerate() {
                if name == "." {
                    continue;
                }
                let e = map.entry(name.clone()).or_insert((r, r + 1, c, c + 1));
                e.0 = e.0.min(r);
                e.1 = e.1.max(r + 1);
                e.2 = e.2.min(c);
                e.3 = e.3.max(c + 1);
            }
        }
    }
    map
}

/// The fixed size of a track, if any (used to count `auto-fill` repetitions).
fn fixed_track_size(t: &TrackSize, basis: Option<f32>) -> Option<f32> {
    let fixed = |b: &TrackBreadth| match b {
        TrackBreadth::Length(l) => l.resolve_opt(basis),
        _ => None,
    };
    match t {
        TrackSize::Single(b) => fixed(b),
        TrackSize::MinMax(a, b) => fixed(b).or_else(|| fixed(a)),
        TrackSize::FitContent(l) => l.resolve_opt(basis),
    }
}

/// Expands a track list into individual tracks for an available size.
fn expand_tracks(list: &[TrackListItem], avail: Option<f32>, gap: f32) -> Vec<TrackSize> {
    let mut out = Vec::new();
    // Space used by the non-repeating tracks (for auto-fill counts).
    let fixed_other: f32 = list
        .iter()
        .map(|i| match i {
            TrackListItem::Track(t) => fixed_track_size(t, avail).unwrap_or(0.0) + gap,
            TrackListItem::Repeat(TrackRepeat::Count(n), ts) => {
                *n as f32
                    * ts.iter()
                        .map(|t| fixed_track_size(t, avail).unwrap_or(0.0) + gap)
                        .sum::<f32>()
            }
            _ => 0.0,
        })
        .sum();
    for item in list {
        match item {
            TrackListItem::Track(t) => out.push(t.clone()),
            TrackListItem::Repeat(TrackRepeat::Count(n), ts) => {
                for _ in 0..*n {
                    out.extend(ts.iter().cloned());
                }
            }
            TrackListItem::Repeat(_, ts) => {
                let per: f32 = ts.iter().map(|t| fixed_track_size(t, avail).unwrap_or(0.0) + gap).sum();
                let count = match avail {
                    Some(a) if per > 0.0 => (((a - fixed_other + gap) / per).floor() as usize).max(1),
                    _ => 1,
                };
                for _ in 0..count.min(1000) {
                    out.extend(ts.iter().cloned());
                }
            }
        }
    }
    out
}

fn has_auto_fit(list: &[TrackListItem]) -> bool {
    list.iter()
        .any(|i| matches!(i, TrackListItem::Repeat(TrackRepeat::AutoFit, _)))
}

/// Resolves one axis of an item's placement to (start, span); `None` start = auto.
fn resolve_axis(lines: &(GridLine, GridLine), explicit: usize, areas: &Areas, columns: bool) -> (Option<usize>, usize) {
    let named = |name: &str, end: bool| -> Option<usize> {
        let (base, suffix_end) = if let Some(b) = name.strip_suffix("-start") {
            (b, Some(false))
        } else if let Some(b) = name.strip_suffix("-end") {
            (b, Some(true))
        } else {
            (name, None)
        };
        let a = areas.get(base)?;
        let use_end = suffix_end.unwrap_or(end);
        Some(match (columns, use_end) {
            (true, false) => a.2,
            (true, true) => a.3,
            (false, false) => a.0,
            (false, true) => a.1,
        })
    };
    let line_index = |l: &GridLine, end: bool| -> Option<usize> {
        match l {
            GridLine::Line(n) if *n > 0 => Some((*n - 1) as usize),
            GridLine::Line(n) => Some((explicit as i32 + 1 + n).max(0) as usize),
            GridLine::Named(name) => named(name, end),
            _ => None,
        }
    };
    let start = line_index(&lines.0, false);
    let end = line_index(&lines.1, true);
    match (start, end, &lines.0, &lines.1) {
        (Some(s), Some(e), _, _) => {
            let (a, b) = if e < s { (e, s) } else { (s, e) };
            (Some(a), (b - a).max(1))
        }
        (Some(s), None, _, GridLine::Span(n)) => (Some(s), (*n as usize).max(1)),
        (Some(s), None, _, _) => (Some(s), 1),
        (None, Some(e), GridLine::Span(n), _) => {
            let n = (*n as usize).max(1);
            (Some(e.saturating_sub(n)), n)
        }
        (None, Some(e), _, _) => (Some(e.saturating_sub(1)), 1),
        (None, None, GridLine::Span(n), _) | (None, None, _, GridLine::Span(n)) => (None, (*n as usize).max(1)),
        _ => (None, 1),
    }
}

struct Occupancy {
    cols: usize,
    cells: Vec<Vec<bool>>,
}

impl Occupancy {
    fn fits(&self, r: usize, c: usize, rs: usize, cs: usize) -> bool {
        if c + cs > self.cols {
            return false;
        }
        (r..r + rs).all(|rr| (c..c + cs).all(|cc| !self.cells.get(rr).is_some_and(|row| row[cc])))
    }
    fn mark(&mut self, r: usize, c: usize, rs: usize, cs: usize) {
        while self.cells.len() < r + rs {
            self.cells.push(vec![false; self.cols]);
        }
        for row in self.cells.iter_mut().skip(r).take(rs) {
            if row.len() < self.cols {
                row.resize(self.cols, false);
            }
            for cell in row.iter_mut().skip(c).take(cs) {
                *cell = true;
            }
        }
    }
}

/// Places items (CSS Grid §8.5). Works in "row-major" terms; column flow is handled by
/// transposing before and after. Returns placements and the column/row counts.
fn place_items(
    style: &ComputedStyle,
    items: &[&LayoutBox],
    explicit_cols: usize,
    explicit_rows: usize,
) -> (Vec<Placement>, usize, usize) {
    let areas = areas_of(style);
    let column_flow = style.grid_auto_flow_column;
    let dense = style.grid_auto_flow_dense;
    // Resolve definite positions; "major" axis = rows for row flow.
    let mut resolved: Vec<(AxisPlacement, AxisPlacement)> = items
        .iter()
        .map(|b| {
            let c = resolve_axis(&b.style.grid_column, explicit_cols, &areas, true);
            let r = resolve_axis(&b.style.grid_row, explicit_rows, &areas, false);
            if column_flow { (c, r) } else { (r, c) }
        })
        .collect();
    // Minor-axis track count: explicit, or enough for every item.
    let explicit_minor = if column_flow { explicit_rows } else { explicit_cols };
    let mut minor = explicit_minor.max(1);
    for (_, (start, span)) in &resolved {
        minor = minor.max(start.unwrap_or(0) + span);
    }
    let mut occ = Occupancy {
        cols: minor,
        cells: Vec::new(),
    };
    let mut out = vec![
        Placement {
            col: 0,
            col_span: 1,
            row: 0,
            row_span: 1
        };
        items.len()
    ];
    // 1. Items with a definite major position.
    for (i, ((maj, maj_span), (min, min_span))) in resolved.iter_mut().enumerate() {
        if let Some(m) = *maj {
            let c = match *min {
                Some(c) => c,
                None => (0..=minor - (*min_span).min(minor))
                    .find(|&c| occ.fits(m, c, *maj_span, *min_span))
                    .unwrap_or(0),
            };
            occ.mark(m, c, *maj_span, (*min_span).min(minor - c.min(minor - 1)));
            out[i] = Placement {
                row: m,
                row_span: *maj_span,
                col: c,
                col_span: *min_span,
            };
            *min = Some(c);
        }
    }
    // 2. Auto-placed items in order.
    let (mut cur_r, mut cur_c) = (0usize, 0usize);
    for (i, ((maj, maj_span), (min, min_span))) in resolved.iter().enumerate() {
        if maj.is_some() {
            continue;
        }
        let ms = (*min_span).min(minor);
        if dense {
            cur_r = 0;
            cur_c = 0;
        }
        let (r, c) = match *min {
            Some(c) => {
                // Definite minor position: next row where it fits.
                let mut r = if c < cur_c { cur_r + 1 } else { cur_r };
                if dense {
                    r = 0;
                }
                while !occ.fits(r, c, *maj_span, ms) {
                    r += 1;
                }
                (r, c)
            }
            None => {
                let (mut r, mut c) = (cur_r, cur_c);
                loop {
                    if c + ms > minor {
                        r += 1;
                        c = 0;
                        continue;
                    }
                    if occ.fits(r, c, *maj_span, ms) {
                        break;
                    }
                    c += 1;
                }
                (r, c)
            }
        };
        occ.mark(r, c, *maj_span, ms);
        cur_r = r;
        cur_c = c + ms;
        out[i] = Placement {
            row: r,
            row_span: *maj_span,
            col: c,
            col_span: ms,
        };
    }
    let major = occ
        .cells
        .len()
        .max(if column_flow { explicit_cols } else { explicit_rows });
    if column_flow {
        for p in out.iter_mut() {
            *p = Placement {
                col: p.row,
                col_span: p.row_span,
                row: p.col,
                row_span: p.col_span,
            };
        }
        (out, major, minor)
    } else {
        (out, minor, major)
    }
}

/// Track sizes for one axis. `contribution(item) -> (min, max)` gives an item's outer
/// size contributions in this axis.
fn size_tracks(
    tracks: &[TrackSize],
    avail: Option<f32>,
    gap: f32,
    spans: &[(usize, usize)],
    contributions: &[(f32, f32)],
    stretch_auto: bool,
) -> Vec<f32> {
    let n = tracks.len();
    let mut base = vec![0f32; n];
    let mut limit = vec![f32::INFINITY; n];
    let mut flex = vec![0f32; n];
    for (i, t) in tracks.iter().enumerate() {
        match t.min() {
            TrackBreadth::Length(l) => base[i] = l.resolve_opt(avail).unwrap_or(0.0),
            _ => base[i] = 0.0,
        }
        match t.max() {
            TrackBreadth::Length(l) => limit[i] = l.resolve_opt(avail).unwrap_or(f32::INFINITY),
            TrackBreadth::Fr(f) => {
                flex[i] = f;
                limit[i] = f32::INFINITY;
            }
            _ => limit[i] = 0.0, // intrinsic: grown by contributions below
        }
    }
    let intrinsic_min = |t: &TrackSize| !matches!(t.min(), TrackBreadth::Length(_));
    let intrinsic_max = |t: &TrackSize| {
        matches!(
            t.max(),
            TrackBreadth::Auto | TrackBreadth::MinContent | TrackBreadth::MaxContent
        )
    };
    // Single-span items first, then spanning items by increasing span.
    let mut order: Vec<usize> = (0..spans.len()).collect();
    order.sort_by_key(|&i| spans[i].1);
    for &i in &order {
        let (start, span) = spans[i];
        let (cmin, cmax) = contributions[i];
        let range = start..(start + span).min(n);
        if range.is_empty() {
            continue;
        }
        let gaps = gap * (range.len() - 1) as f32;
        if span == 1 {
            let t = &tracks[start];
            if intrinsic_min(t) {
                base[start] = base[start].max(match t.min() {
                    TrackBreadth::MaxContent => cmax,
                    _ => cmin,
                });
            }
            if intrinsic_max(t) {
                let m = match t.max() {
                    TrackBreadth::MinContent => cmin,
                    _ => cmax,
                };
                limit[start] = limit[start].max(m);
            }
            if let TrackSize::FitContent(l) = t {
                let cap = l.resolve_opt(avail).unwrap_or(cmax);
                limit[start] = cmax.min(cap).max(base[start]);
            }
            continue;
        }
        // Spanning items: distribute any excess over intrinsic tracks (skipping flexible
        // ones when non-flexible tracks are available).
        let targets: Vec<usize> = range
            .clone()
            .filter(|&k| intrinsic_min(&tracks[k]) && flex[k] == 0.0)
            .collect();
        let targets = if targets.is_empty() {
            range.clone().filter(|&k| intrinsic_min(&tracks[k])).collect()
        } else {
            targets
        };
        if targets.is_empty() {
            continue;
        }
        let cur: f32 = range.clone().map(|k| base[k]).sum::<f32>() + gaps;
        if cmin > cur {
            let extra = (cmin - cur) / targets.len() as f32;
            for &k in &targets {
                base[k] += extra;
            }
        }
        let cur_l: f32 = range
            .clone()
            .map(|k| {
                if limit[k].is_finite() {
                    limit[k].max(base[k])
                } else {
                    base[k]
                }
            })
            .sum::<f32>()
            + gaps;
        if cmax > cur_l {
            let extra = (cmax - cur_l) / targets.len() as f32;
            for &k in &targets {
                if limit[k].is_finite() {
                    limit[k] = limit[k].max(base[k]) + extra;
                }
            }
        }
    }
    for i in 0..n {
        if limit[i] < base[i] {
            limit[i] = base[i];
        }
    }
    let gaps_total = gap * n.saturating_sub(1) as f32;
    let mut sizes = base.clone();
    match avail {
        Some(a) => {
            // Maximize tracks up to their growth limits.
            let mut free = a - gaps_total - sizes.iter().sum::<f32>();
            for _ in 0..n + 1 {
                if free <= 0.01 {
                    break;
                }
                let growable: Vec<usize> = (0..n)
                    .filter(|&i| flex[i] == 0.0 && limit[i].is_finite() && sizes[i] < limit[i] - 0.01)
                    .collect();
                if growable.is_empty() {
                    break;
                }
                let share = free / growable.len() as f32;
                for &i in &growable {
                    let add = share.min(limit[i] - sizes[i]);
                    sizes[i] += add;
                    free -= add;
                }
            }
            // Expand flexible tracks.
            if flex.iter().any(|&f| f > 0.0) {
                let mut inflexible = vec![false; n];
                for _ in 0..n + 1 {
                    let leftover = a
                        - gaps_total
                        - (0..n)
                            .filter(|&i| flex[i] == 0.0 || inflexible[i])
                            .map(|i| sizes[i])
                            .sum::<f32>();
                    let flex_sum: f32 = (0..n)
                        .filter(|&i| flex[i] > 0.0 && !inflexible[i])
                        .map(|i| flex[i])
                        .sum();
                    if flex_sum <= 0.0 {
                        break;
                    }
                    let unit = leftover.max(0.0) / flex_sum.max(1.0);
                    let mut changed = false;
                    for i in 0..n {
                        if flex[i] > 0.0 && !inflexible[i] && base[i] > flex[i] * unit {
                            inflexible[i] = true;
                            sizes[i] = base[i];
                            changed = true;
                        }
                    }
                    if !changed {
                        for i in 0..n {
                            if flex[i] > 0.0 && !inflexible[i] {
                                sizes[i] = flex[i] * unit;
                            }
                        }
                        break;
                    }
                }
            } else if stretch_auto {
                let free = a - gaps_total - sizes.iter().sum::<f32>();
                let autos: Vec<usize> = (0..n)
                    .filter(|&i| matches!(tracks[i].max(), TrackBreadth::Auto))
                    .collect();
                if free > 0.0 && !autos.is_empty() {
                    for &i in &autos {
                        sizes[i] += free / autos.len() as f32;
                    }
                }
            }
        }
        None => {
            // Indefinite: intrinsic tracks take their growth limits, flexible tracks
            // their max-content contributions (approximated by the limit).
            for i in 0..n {
                if limit[i].is_finite() {
                    sizes[i] = limit[i];
                }
            }
            let flex_tracks: Vec<usize> = (0..n).filter(|&i| flex[i] > 0.0).collect();
            if !flex_tracks.is_empty() {
                // Size an fr unit so every flexible track reaches its max-content size.
                let mut unit: f32 = 0.0;
                for &i in &flex_tracks {
                    let want = spans
                        .iter()
                        .zip(contributions)
                        .filter(|((s, sp), _)| *sp == 1 && *s == i)
                        .map(|(_, c)| c.1)
                        .fold(base[i], f32::max);
                    unit = unit.max(want / flex[i].max(1.0));
                }
                for &i in &flex_tracks {
                    sizes[i] = (unit * flex[i]).max(base[i]);
                }
            }
        }
    }
    sizes
}

fn track_offsets(sizes: &[f32], gap: f32, start: f32, extra_gap: f32) -> Vec<f32> {
    let mut v = Vec::with_capacity(sizes.len() + 1);
    let mut x = start;
    for s in sizes {
        v.push(x);
        x += s + gap + extra_gap;
    }
    v.push(x - gap - extra_gap);
    v
}

/// Distribution offsets for `justify-content`/`align-content` on the grid.
fn content_distribution(mode: JustifyContent, free: f32, n: usize) -> (f32, f32) {
    if free <= 0.0 || n == 0 {
        return (0.0, 0.0);
    }
    match mode {
        JustifyContent::FlexEnd => (free, 0.0),
        JustifyContent::Center => (free / 2.0, 0.0),
        JustifyContent::SpaceBetween if n > 1 => (0.0, free / (n - 1) as f32),
        JustifyContent::SpaceAround => (free / n as f32 / 2.0, free / n as f32),
        JustifyContent::SpaceEvenly => (free / (n + 1) as f32, free / (n + 1) as f32),
        _ => (0.0, 0.0),
    }
}

struct Prepared<'a> {
    items: Vec<&'a LayoutBox>,
    placements: Vec<Placement>,
    col_tracks: Vec<TrackSize>,
    row_tracks: Vec<TrackSize>,
    col_gap: f32,
    row_gap: f32,
}

fn prepare<'a>(
    container: &'a LayoutBox,
    items_in: &'a [LayoutBox],
    width: Option<f32>,
    height: Option<f32>,
) -> Prepared<'a> {
    let s = &container.style;
    let col_gap = s
        .column_gap
        .as_ref()
        .map(|g| g.resolve(width.unwrap_or(0.0)))
        .unwrap_or(0.0);
    let row_gap = s
        .row_gap
        .as_ref()
        .map(|g| g.resolve(height.unwrap_or(0.0)))
        .unwrap_or(0.0);
    let mut items: Vec<&LayoutBox> = items_in.iter().filter(|b| !b.is_abspos()).collect();
    items.sort_by_key(|b| b.style.order);
    let mut explicit_cols = expand_tracks(&s.grid_template_columns, width, col_gap);
    let mut explicit_rows = expand_tracks(&s.grid_template_rows, height, row_gap);
    if let Some(a) = &s.grid_template_areas {
        let w = a.first().map(|r| r.len()).unwrap_or(0);
        while explicit_cols.len() < w {
            explicit_cols.push(s.grid_auto_columns.clone());
        }
        while explicit_rows.len() < a.len() {
            explicit_rows.push(s.grid_auto_rows.clone());
        }
    }
    let (placements, ncols, nrows) = place_items(s, &items, explicit_cols.len(), explicit_rows.len());
    let mut col_tracks = explicit_cols;
    while col_tracks.len() < ncols {
        col_tracks.push(s.grid_auto_columns.clone());
    }
    let mut row_tracks = explicit_rows;
    while row_tracks.len() < nrows {
        row_tracks.push(s.grid_auto_rows.clone());
    }
    // auto-fit: empty repeated tracks collapse to zero.
    if has_auto_fit(&s.grid_template_columns) {
        for (i, t) in col_tracks.iter_mut().enumerate() {
            let used = placements.iter().any(|p| p.col <= i && i < p.col + p.col_span);
            if !used {
                *t = TrackSize::Single(TrackBreadth::Length(crate::css::values::LengthPercentage::px(0.0)));
            }
        }
    }
    Prepared {
        items,
        placements,
        col_tracks,
        row_tracks,
        col_gap,
        row_gap,
    }
}

fn width_contributions(ctx: &LayoutContext, items: &[&LayoutBox], cb_width: f32) -> Vec<(f32, f32)> {
    items
        .iter()
        .map(|b| {
            let (min, max) = intrinsic_widths(ctx, b);
            let m = BoxGeometry::resolve(&b.style, cb_width).margin_or_zero().horizontal();
            (min + m, max + m)
        })
        .collect()
}

/// (min-content, max-content) widths of a grid container's content box.
pub fn grid_intrinsic(ctx: &LayoutContext, container: &LayoutBox, items: &[LayoutBox]) -> (f32, f32) {
    let p = prepare(container, items, None, None);
    let spans: Vec<(usize, usize)> = p.placements.iter().map(|pl| (pl.col, pl.col_span)).collect();
    let contrib = width_contributions(ctx, &p.items, 0.0);
    let gaps = p.col_gap * p.col_tracks.len().saturating_sub(1) as f32;
    let min_contrib: Vec<(f32, f32)> = contrib.iter().map(|c| (c.0, c.0)).collect();
    let min = size_tracks(&p.col_tracks, None, p.col_gap, &spans, &min_contrib, false)
        .iter()
        .sum::<f32>()
        + gaps;
    let max = size_tracks(&p.col_tracks, None, p.col_gap, &spans, &contrib, false)
        .iter()
        .sum::<f32>()
        + gaps;
    (min, max.max(min))
}

/// Lays out a grid container's items inside its content box.
pub fn layout_grid<'a>(
    ctx: &LayoutContext<'a>,
    container: &'a LayoutBox,
    items_in: &'a [LayoutBox],
    cb: &ContainingBlock,
) -> FlexResult {
    let s = &container.style;
    let mut children = Vec::new();
    for b in items_in.iter().filter(|b| b.is_abspos()) {
        let idx = ctx.register_abs(b);
        children.push(Fragment::new(
            FragmentKind::AbsPlaceholder(idx),
            b.node,
            b.style.clone(),
            Rect::new(0.0, 0.0, 0.0, 0.0),
        ));
    }
    let p = prepare(container, items_in, Some(cb.width), cb.height);
    // ---- columns
    let col_spans: Vec<(usize, usize)> = p.placements.iter().map(|pl| (pl.col, pl.col_span)).collect();
    let contrib = width_contributions(ctx, &p.items, cb.width);
    let stretch_cols = matches!(s.justify_content, JustifyContent::Normal | JustifyContent::Stretch);
    let col_sizes = size_tracks(
        &p.col_tracks,
        Some(cb.width),
        p.col_gap,
        &col_spans,
        &contrib,
        stretch_cols,
    );
    let col_used: f32 = col_sizes.iter().sum::<f32>() + p.col_gap * col_sizes.len().saturating_sub(1) as f32;
    let (cstart, cextra) = content_distribution(s.justify_content, cb.width - col_used, col_sizes.len());
    let col_off = track_offsets(&col_sizes, p.col_gap, cstart, cextra);
    let area_w = |pl: &Placement| -> f32 {
        let end = (pl.col + pl.col_span).min(col_sizes.len());
        let start = pl.col.min(end);
        col_sizes[start..end].iter().sum::<f32>() + (p.col_gap + cextra) * (end - start).saturating_sub(1) as f32
    };
    // ---- lay out items at their column widths to get row contributions
    let justify = |b: &LayoutBox| -> AlignItems {
        let j = match b.style.justify_self {
            AlignItems::Auto => s.justify_items,
            j => j,
        };
        match j {
            AlignItems::Normal if matches!(b.content, BoxContent::Replaced(_)) => AlignItems::FlexStart,
            AlignItems::Normal | AlignItems::Auto => AlignItems::Stretch,
            j => j,
        }
    };
    let align = |b: &LayoutBox| -> AlignItems {
        let a = match b.style.align_self {
            AlignItems::Auto => s.align_items,
            a => a,
        };
        match a {
            AlignItems::Normal if matches!(b.content, BoxContent::Replaced(_)) => AlignItems::FlexStart,
            AlignItems::Normal | AlignItems::Auto => AlignItems::Stretch,
            a => a,
        }
    };
    let item_width = |b: &LayoutBox, aw: f32| -> f32 {
        let geom = BoxGeometry::resolve(&b.style, aw);
        let m = geom.margin_or_zero().horizontal();
        let bp = geom.border_padding().horizontal();
        if let BoxContent::Replaced(_) = b.content {
            return replaced_size(
                b,
                &geom,
                &ContainingBlock {
                    width: aw,
                    height: None,
                },
            )
            .0;
        }
        if let Some(w) = specified_border_box_width(&b.style, &b.style.width, Some(aw), bp) {
            return super::clamp_width(&b.style, w, Some(aw), bp);
        }
        let avail = (aw - m).max(0.0);
        let w = if justify(b) == AlignItems::Stretch {
            avail
        } else {
            let (min, max) = intrinsic_widths(ctx, b);
            min.max(avail.min(max))
        };
        super::clamp_width(&b.style, w, Some(aw), bp)
    };
    let mut frags: Vec<Fragment> = Vec::with_capacity(p.items.len());
    let mut row_contrib = Vec::with_capacity(p.items.len());
    for (b, pl) in p.items.iter().zip(&p.placements) {
        let aw = area_w(pl);
        let w = item_width(b, aw);
        let icb = ContainingBlock {
            width: aw,
            height: None,
        };
        let f = layout_bfc_root(ctx, b, &icb, Some(w), None);
        let m = BoxGeometry::resolve(&b.style, aw).margin_or_zero().vertical();
        let h = f.rect.height + m;
        row_contrib.push((h, h));
        frags.push(f);
    }
    // ---- rows
    let row_spans: Vec<(usize, usize)> = p.placements.iter().map(|pl| (pl.row, pl.row_span)).collect();
    let stretch_rows = matches!(s.align_content, JustifyContent::Normal | JustifyContent::Stretch);
    let row_sizes = size_tracks(
        &p.row_tracks,
        cb.height,
        p.row_gap,
        &row_spans,
        &row_contrib,
        stretch_rows,
    );
    let row_used: f32 = row_sizes.iter().sum::<f32>() + p.row_gap * row_sizes.len().saturating_sub(1) as f32;
    let (rstart, rextra) = match cb.height {
        Some(h) => content_distribution(s.align_content, h - row_used, row_sizes.len()),
        None => (0.0, 0.0),
    };
    let row_off = track_offsets(&row_sizes, p.row_gap, rstart, rextra);
    // ---- final placement and alignment
    let mut first_baseline = None;
    let mut last_baseline = None;
    for ((b, pl), mut f) in p.items.iter().zip(&p.placements).zip(frags) {
        let aw = area_w(pl);
        let rend = (pl.row + pl.row_span).min(row_sizes.len());
        let rstart_i = pl.row.min(rend);
        let ah = row_sizes[rstart_i..rend].iter().sum::<f32>()
            + (p.row_gap + rextra) * (rend - rstart_i).saturating_sub(1) as f32;
        let geom = BoxGeometry::resolve(&b.style, aw);
        let m = geom.margin_or_zero();
        let a = align(b);
        if a == AlignItems::Stretch && b.style.height.is_auto() && !matches!(b.content, BoxContent::Replaced(_)) {
            let target = super::clamp_height(
                &b.style,
                (ah - m.vertical()).max(0.0),
                Some(ah),
                geom.border_padding().vertical(),
            );
            if (target - f.rect.height).abs() > 0.01 {
                let icb = ContainingBlock {
                    width: aw,
                    height: Some(ah),
                };
                f = layout_bfc_root(ctx, b, &icb, Some(f.rect.width), Some(target));
            }
        }
        let free_x = aw - f.rect.width - m.horizontal();
        let dx = match justify(b) {
            AlignItems::FlexEnd => free_x,
            AlignItems::Center => free_x / 2.0,
            _ => 0.0,
        };
        let free_y = ah - f.rect.height - m.vertical();
        let dy = match a {
            AlignItems::FlexEnd => free_y,
            AlignItems::Center => free_y / 2.0,
            _ => 0.0,
        };
        f.rect.x = col_off[pl.col.min(col_sizes.len())] + m.left + dx;
        f.rect.y = row_off[pl.row.min(row_sizes.len())] + m.top + dy;
        f.margin = m;
        if first_baseline.is_none() && pl.row == 0 {
            first_baseline = Some(f.rect.y + f.first_baseline.unwrap_or(f.rect.height));
        }
        last_baseline = Some(f.rect.y + f.last_baseline.unwrap_or(f.rect.height));
        children.push(f);
    }
    let height = row_off.last().copied().unwrap_or(0.0).max(0.0);
    FlexResult {
        children,
        height,
        first_baseline,
        last_baseline,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fr_and_fixed_tracks() {
        let px = |v| TrackSize::Single(TrackBreadth::Length(crate::css::values::LengthPercentage::px(v)));
        let fr = |v| TrackSize::Single(TrackBreadth::Fr(v));
        let sizes = size_tracks(&[px(100.0), fr(1.0), fr(2.0)], Some(400.0), 0.0, &[], &[], true);
        assert_eq!(sizes, vec![100.0, 100.0, 200.0]);
        // An fr track never shrinks below its min-content contribution.
        let sizes = size_tracks(&[fr(1.0), fr(1.0)], Some(100.0), 0.0, &[(0, 1)], &[(80.0, 120.0)], true);
        assert_eq!(sizes, vec![80.0, 20.0]);
        let auto = TrackSize::Single(TrackBreadth::Auto);
        let sizes = size_tracks(
            &[auto.clone(), auto],
            Some(300.0),
            10.0,
            &[(0, 1), (1, 1)],
            &[(10.0, 50.0), (10.0, 90.0)],
            true,
        );
        assert!(
            (sizes[0] + sizes[1] - 290.0).abs() < 0.01,
            "auto tracks stretch: {sizes:?}"
        );
    }
}
