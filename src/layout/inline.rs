//! Inline formatting contexts: line breaking, inline boxes, atomic inlines, floats
//! inside lines, `text-align`, `text-indent` and `vertical-align`.
//!
//! Text is shaped once per text item; break opportunities split it into segments
//! (a word plus its trailing spaces). The breaker accumulates an unbreakable "chunk"
//! (segments, inline box edges and atomics between two break opportunities) and commits
//! it to the current line if it fits, otherwise it starts a new line. Line boxes are
//! then aligned vertically following CSS 2.1 §10.8 with Blink's rounding of half-leading.

use super::block::{layout_float_unplaced, place_float, relative_offset};
use super::box_tree::{InlineContent, InlineItem, LayoutBox, TextItem};
use super::floats::FloatContext;
use super::fragment::{Fragment, FragmentKind, PositionedGlyph, TextRun};
use super::{ContainingBlock, Edges, LayoutContext, Point, Rect};
use crate::style::computed::*;
use crate::text::{FaceId, FontMetrics, ShapedGlyph};
use std::rc::Rc;

/// Result of inline layout.
pub struct InlineResult {
    /// Line fragments and float fragments, relative to the container's content box.
    pub children: Vec<Fragment>,
    /// Total height of the line boxes.
    pub height: f32,
    /// Baseline of the first line.
    pub first_baseline: Option<f32>,
    /// Baseline of the last line.
    pub last_baseline: Option<f32>,
}

/// Face and size for a style.
pub fn font_for(ctx: &LayoutContext, style: &ComputedStyle) -> FaceId {
    ctx.face_for(style)
}

fn shaped(ctx: &LayoutContext, t: &TextItem) -> Rc<Vec<ShapedGlyph>> {
    if let Some(s) = t.shaped.borrow().as_ref() {
        return s.clone();
    }
    let face = font_for(ctx, &t.style);
    let g = Rc::new(ctx.fonts.shape(
        face,
        t.style.font_size,
        &t.text,
        t.style.letter_spacing,
        t.style.word_spacing,
    ));
    *t.shaped.borrow_mut() = Some(g.clone());
    g
}

/// A piece of a text item between break opportunities.
#[derive(Clone, Copy, Debug)]
struct Segment {
    g0: usize,
    g1: usize,
    width: f32,
    /// Width of trailing (collapsible or hangable) spaces.
    trailing: f32,
    /// Whether a line may break after this segment.
    break_after: bool,
}

fn wraps(ws: WhiteSpace) -> bool {
    !matches!(ws, WhiteSpace::Pre | WhiteSpace::Nowrap)
}

fn is_cjk(c: char) -> bool {
    matches!(c as u32, 0x2E80..=0x9FFF | 0xAC00..=0xD7AF | 0xF900..=0xFAFF | 0xFF00..=0xFFEF | 0x20000..=0x2FA1F)
}

/// Splits a shaped text item into segments at break opportunities.
fn segments(t: &TextItem, glyphs: &[ShapedGlyph]) -> Vec<Segment> {
    let text = &t.text;
    let wrap = wraps(t.style.white_space);
    // Break positions (byte offsets after which a break is allowed).
    let mut breaks: Vec<usize> = Vec::new();
    if wrap {
        let chars: Vec<(usize, char)> = text.char_indices().collect();
        for (k, &(i, c)) in chars.iter().enumerate() {
            let next = chars.get(k + 1).map(|&(_, n)| n);
            let end = i + c.len_utf8();
            let opportunity = t.style.word_break_all && next.is_some()
                || match c {
                    ' ' | '\u{200B}' => next != Some(' '),
                    '-' | '\u{2010}' => {
                        let prev_alnum = k > 0 && chars[k - 1].1.is_alphanumeric();
                        prev_alnum && next.is_some_and(|n| n.is_alphabetic())
                    }
                    '\u{2014}' => next.is_some(), // em dash
                    c if is_cjk(c) => next.is_some(),
                    _ => next.is_some_and(is_cjk),
                };
            if opportunity {
                breaks.push(end);
            }
        }
    }
    let mut out = Vec::new();
    let mut gi = 0;
    let mut seg_start_g = 0;
    let mut width = 0.0;
    let mut trailing = 0.0;
    let mut bi = 0;
    while gi < glyphs.len() {
        let g = glyphs[gi];
        let cluster = g.cluster as usize;
        // Close segments whose break position is at or before this glyph's cluster.
        while bi < breaks.len() && breaks[bi] <= cluster {
            if gi > seg_start_g {
                out.push(Segment {
                    g0: seg_start_g,
                    g1: gi,
                    width,
                    trailing,
                    break_after: true,
                });
                seg_start_g = gi;
                width = 0.0;
                trailing = 0.0;
            }
            bi += 1;
        }
        let is_space = text[cluster..].starts_with(' ');
        width += g.advance;
        if is_space {
            trailing += g.advance;
        } else {
            trailing = 0.0;
        }
        gi += 1;
    }
    if gi > seg_start_g {
        out.push(Segment {
            g0: seg_start_g,
            g1: gi,
            width,
            trailing,
            // The end of a text item is a break opportunity only if it ends in a space
            // that allows wrapping; otherwise the next item decides.
            break_after: wrap && text.ends_with(' '),
        });
    }
    out
}

/// Per inline box metrics used during line layout.
#[derive(Clone)]
struct BoxMetrics {
    metrics: FontMetrics,
    /// Ascent including half-leading (Blink: ascent + floor(half-leading)).
    a_lead: f32,
    /// line-height − a_lead.
    d_lead: f32,
    font_size: f32,
    margin: Edges,
    border: Edges,
    padding: Edges,
}

fn box_metrics(ctx: &LayoutContext, style: &ComputedStyle, cb_width: f32) -> BoxMetrics {
    let face = font_for(ctx, style);
    let m = ctx.fonts.metrics(face, style.font_size);
    let lh = style.line_height_px(m.normal_line_height() / style.font_size.max(0.0001));
    let lh = if style.font_size == 0.0 { 0.0 } else { lh };
    let half = (lh - (m.ascent + m.descent)) / 2.0;
    let a_lead = m.ascent + half.floor();
    let geom = super::BoxGeometry::resolve(style, cb_width);
    BoxMetrics {
        metrics: m,
        a_lead,
        d_lead: lh - a_lead,
        font_size: style.font_size,
        margin: geom.margin_or_zero(),
        border: geom.border,
        padding: geom.padding,
    }
}

#[derive(Debug)]
enum LineKind {
    Text {
        item: usize,
        g0: usize,
        g1: usize,
    },
    Start(usize),
    End(usize),
    Atomic {
        item: usize,
    },
    Marker {
        item: usize,
    },
    Abs {
        item: usize,
        idx: usize,
    },
    Break,
    /// `text-overflow: ellipsis` marker, styled like text item `item`.
    Ellipsis {
        item: usize,
    },
}

#[derive(Debug)]
struct LineItem {
    kind: LineKind,
    width: f32,
}

struct Breaker<'c, 'a> {
    ctx: &'c LayoutContext<'a>,
    ifc: &'a InlineContent,
    container: &'a LayoutBox,
    cb: &'c ContainingBlock,
    floats: &'c mut FloatContext,
    origin: Point,
    boxes: Vec<BoxMetrics>,
    root: BoxMetrics,
    glyphs: Vec<Option<Rc<Vec<ShapedGlyph>>>>,
    atomics: Vec<Option<Fragment>>,
    // line state
    line: Vec<LineItem>,
    line_width: f32,
    chunk: Vec<LineItem>,
    chunk_width: f32,
    chunk_trailing: f32,
    y: f32,
    first_line: bool,
    open: Vec<usize>,
    open_at_line_start: Vec<usize>,
    pending_floats: Vec<usize>,
    out: Vec<Fragment>,
    first_baseline: Option<f32>,
    last_baseline: Option<f32>,
}

/// Lays out an inline formatting context.
pub fn layout_inline<'a>(
    ctx: &LayoutContext<'a>,
    container: &'a LayoutBox,
    ifc: &'a InlineContent,
    cb: &ContainingBlock,
    floats: &mut FloatContext,
    origin: Point,
) -> InlineResult {
    let boxes = ifc.boxes.iter().map(|b| box_metrics(ctx, &b.style, cb.width)).collect();
    let root = box_metrics(ctx, &container.style, cb.width);
    let mut br = Breaker {
        ctx,
        ifc,
        container,
        cb,
        floats,
        origin,
        boxes,
        root,
        glyphs: vec![None; ifc.items.len()],
        atomics: (0..ifc.items.len()).map(|_| None).collect(),
        line: Vec::new(),
        line_width: 0.0,
        chunk: Vec::new(),
        chunk_width: 0.0,
        chunk_trailing: 0.0,
        y: 0.0,
        first_line: true,
        open: Vec::new(),
        open_at_line_start: Vec::new(),
        pending_floats: Vec::new(),
        out: Vec::new(),
        first_baseline: None,
        last_baseline: None,
    };
    br.run();
    InlineResult {
        children: br.out,
        height: br.y,
        first_baseline: br.first_baseline,
        last_baseline: br.last_baseline,
    }
}

impl<'a> Breaker<'_, 'a> {
    fn container_wraps(&self) -> bool {
        wraps(self.container.style.white_space)
    }

    fn run(&mut self) {
        let wrap_root = self.container_wraps();
        for (idx, item) in self.ifc.items.iter().enumerate() {
            match item {
                InlineItem::Text(t) => {
                    let g = shaped(self.ctx, t);
                    let segs = segments(t, &g);
                    self.glyphs[idx] = Some(g);
                    for s in segs {
                        self.push_chunk(
                            LineItem {
                                kind: LineKind::Text {
                                    item: idx,
                                    g0: s.g0,
                                    g1: s.g1,
                                },
                                width: s.width,
                            },
                            s.trailing,
                        );
                        if s.break_after {
                            self.commit();
                        }
                    }
                }
                InlineItem::Start(b) => {
                    let m = &self.boxes[*b];
                    let info = &self.ifc.boxes[*b];
                    let w = if info.start_edge {
                        m.margin.left + m.border.left + m.padding.left
                    } else {
                        0.0
                    };
                    self.open.push(*b);
                    self.push_chunk(
                        LineItem {
                            kind: LineKind::Start(*b),
                            width: w,
                        },
                        0.0,
                    );
                }
                InlineItem::End(b) => {
                    let m = &self.boxes[*b];
                    let info = &self.ifc.boxes[*b];
                    let w = if info.end_edge {
                        m.margin.right + m.border.right + m.padding.right
                    } else {
                        0.0
                    };
                    if let Some(p) = self.open.iter().rposition(|x| x == b) {
                        self.open.remove(p);
                    }
                    // Trailing spaces before an end tag still hang at the line end.
                    let trailing = self.chunk_trailing;
                    self.push_chunk(
                        LineItem {
                            kind: LineKind::End(*b),
                            width: w,
                        },
                        0.0,
                    );
                    self.chunk_trailing = trailing;
                }
                InlineItem::Atomic(b) => {
                    let frag = layout_float_unplaced(self.ctx, b, self.cb);
                    let w = frag.rect.width + frag.margin.horizontal();
                    self.atomics[idx] = Some(frag);
                    if wrap_root {
                        self.commit();
                    }
                    self.push_chunk(
                        LineItem {
                            kind: LineKind::Atomic { item: idx },
                            width: w,
                        },
                        0.0,
                    );
                    if wrap_root {
                        self.commit();
                    }
                }
                InlineItem::Break(_) => {
                    self.commit();
                    self.line.push(LineItem {
                        kind: LineKind::Break,
                        width: 0.0,
                    });
                    self.finish_line(true);
                }
                InlineItem::Float(b) => {
                    self.handle_float(idx, b);
                }
                InlineItem::Abs(b) => {
                    let pidx = self.ctx.register_abs(b);
                    self.push_chunk(
                        LineItem {
                            kind: LineKind::Abs { item: idx, idx: pidx },
                            width: 0.0,
                        },
                        0.0,
                    );
                }
                InlineItem::Marker(m) => {
                    let face = font_for(self.ctx, &m.style);
                    let w = if m.outside {
                        0.0
                    } else {
                        self.ctx.fonts.measure(face, m.style.font_size, &m.text)
                    };
                    self.push_chunk(
                        LineItem {
                            kind: LineKind::Marker { item: idx },
                            width: w,
                        },
                        0.0,
                    );
                }
            }
        }
        self.commit();
        if !self.line.is_empty() {
            self.finish_line(true);
        }
        // Floats that never found a line.
        let pending = std::mem::take(&mut self.pending_floats);
        for idx in pending {
            self.place_pending_float(idx);
        }
    }

    fn push_chunk(&mut self, item: LineItem, trailing: f32) {
        self.chunk_width += item.width;
        let is_text = matches!(item.kind, LineKind::Text { .. });
        self.chunk.push(item);
        if is_text {
            self.chunk_trailing = trailing;
        } else if trailing == 0.0 {
            self.chunk_trailing = 0.0;
        }
    }

    /// Available (left, right) for a line at the current y, in content-box coordinates.
    fn line_band(&self) -> (f32, f32) {
        let est = self.root.a_lead + self.root.d_lead;
        let (l, r) = self.floats.available(
            self.origin.y + self.y,
            est.max(1.0),
            self.origin.x,
            self.origin.x + self.cb.width,
        );
        let mut l = l - self.origin.x;
        let r = r - self.origin.x;
        if self.first_line {
            l += self.container.style.text_indent.resolve(self.cb.width);
        }
        (l, r)
    }

    fn line_has_content(&self) -> bool {
        self.line.iter().any(|i| match i.kind {
            LineKind::Text { g0, g1, .. } => g1 > g0,
            LineKind::Atomic { .. } | LineKind::Marker { .. } => true,
            _ => i.width > 0.0,
        })
    }

    fn commit(&mut self) {
        if self.chunk.is_empty() {
            return;
        }
        loop {
            let (l, r) = self.line_band();
            let avail = r - l;
            let fits = self.line_width + self.chunk_width - self.chunk_trailing <= avail + 0.01;
            if fits || !self.line_has_content() {
                if !fits && !self.line_has_content() && avail < self.cb.width - 0.01 {
                    // Doesn't fit beside floats: move below the next float edge.
                    if let Some(next) = self.floats.next_bottom_below(self.origin.y + self.y) {
                        self.y = next - self.origin.y;
                        continue;
                    }
                }
                if !fits && !self.line_has_content() && self.split_overflowing_chunk(avail) {
                    // The head of the chunk went on its own line; retry with the rest.
                    continue;
                }
                if !self.line_has_content() {
                    self.strip_leading_spaces();
                }
                let chunk = std::mem::take(&mut self.chunk);
                for it in chunk {
                    self.line_width += it.width;
                    self.append_line_item(it);
                }
                self.chunk_width = 0.0;
                self.chunk_trailing = 0.0;
                return;
            }
            self.finish_line(false);
        }
    }

    /// `overflow-wrap: anywhere | break-word`: breaks an unbreakable chunk that doesn't
    /// fit on an empty line at the last glyph that fits. Returns whether it split.
    fn split_overflowing_chunk(&mut self, avail: f32) -> bool {
        let can_split = self.chunk.iter().any(|it| match it.kind {
            LineKind::Text { item, .. } => {
                matches!(&self.ifc.items[item], InlineItem::Text(t) if t.style.overflow_wrap_anywhere)
            }
            _ => false,
        });
        if !can_split {
            return false;
        }
        self.strip_leading_spaces();
        let chunk = std::mem::take(&mut self.chunk);
        let mut head: Vec<LineItem> = Vec::new();
        let mut rest: Vec<LineItem> = Vec::new();
        let mut used = 0.0;
        let mut split = false;
        for it in chunk {
            if split {
                rest.push(it);
                continue;
            }
            if let LineKind::Text { item, g0, g1 } = it.kind
                && let InlineItem::Text(t) = &self.ifc.items[item]
                && t.style.overflow_wrap_anywhere
                && used + it.width > avail
            {
                let glyphs = self.glyphs[item].as_ref().unwrap();
                let mut k = g0;
                let mut w = 0.0;
                while k < g1 && used + w + glyphs[k].advance <= avail + 0.01 {
                    w += glyphs[k].advance;
                    k += 1;
                }
                if k == g0 && head.is_empty() && k < g1 {
                    // Always make progress: at least one glyph per line.
                    w = glyphs[k].advance;
                    k += 1;
                }
                if k > g0 {
                    head.push(LineItem {
                        kind: LineKind::Text { item, g0, g1: k },
                        width: w,
                    });
                }
                if k < g1 {
                    rest.push(LineItem {
                        kind: LineKind::Text { item, g0: k, g1 },
                        width: it.width - w,
                    });
                    split = true;
                }
                used += w;
                continue;
            }
            used += it.width;
            head.push(it);
        }
        if !split {
            // Nothing to break: put everything back and let the line overflow.
            head.extend(rest);
            self.chunk = head;
            return false;
        }
        for it in head {
            self.line_width += it.width;
            self.append_line_item(it);
        }
        self.chunk_width = rest.iter().map(|i| i.width).sum();
        self.chunk_trailing = 0.0;
        self.chunk = rest;
        self.finish_line(false);
        true
    }

    fn append_line_item(&mut self, it: LineItem) {
        // Merge adjacent pieces of the same text item.
        if let LineKind::Text { item, g0, g1 } = it.kind
            && let Some(LineItem {
                kind: LineKind::Text { item: pi, g1: pg1, .. },
                width,
            }) = self.line.last_mut()
            && *pi == item
            && *pg1 == g0
        {
            *pg1 = g1;
            *width += it.width;
            return;
        }
        self.line.push(it);
    }

    /// Removes collapsible spaces at the start of a line from the pending chunk.
    fn strip_leading_spaces(&mut self) {
        for it in self.chunk.iter_mut() {
            match &mut it.kind {
                LineKind::Text { item, g0, g1 } => {
                    let InlineItem::Text(t) = &self.ifc.items[*item] else {
                        break;
                    };
                    if !matches!(
                        t.style.white_space,
                        WhiteSpace::Normal | WhiteSpace::Nowrap | WhiteSpace::PreLine
                    ) {
                        return;
                    }
                    let glyphs = self.glyphs[*item].as_ref().unwrap();
                    while *g0 < *g1 && t.text[glyphs[*g0].cluster as usize..].starts_with(' ') {
                        it.width -= glyphs[*g0].advance;
                        self.chunk_width -= glyphs[*g0].advance;
                        *g0 += 1;
                    }
                    if *g0 < *g1 {
                        return;
                    }
                }
                LineKind::Start(_) | LineKind::End(_) | LineKind::Abs { .. } => {}
                _ => return,
            }
        }
    }

    fn handle_float(&mut self, idx: usize, b: &'a LayoutBox) {
        let mut frag = layout_float_unplaced(self.ctx, b, self.cb);
        let w = frag.rect.width + frag.margin.horizontal();
        let (l, r) = self.line_band();
        let used = self.line_width + self.chunk_width;
        if self.pending_floats.is_empty()
            && (used + w <= r - l + 0.01 || (!self.line_has_content() && self.chunk.is_empty()))
        {
            place_float(&mut frag, b, self.cb, self.floats, self.origin, self.y);
            self.out.push(frag);
        } else {
            self.atomics[idx] = Some(frag);
            self.pending_floats.push(idx);
        }
    }

    fn place_pending_float(&mut self, idx: usize) {
        let InlineItem::Float(b) = &self.ifc.items[idx] else {
            return;
        };
        let Some(mut frag) = self.atomics[idx].take() else {
            return;
        };
        place_float(&mut frag, b, self.cb, self.floats, self.origin, self.y);
        self.out.push(frag);
    }

    fn trim_trailing_spaces(&mut self) {
        for i in (0..self.line.len()).rev() {
            let (item, g0, mut g1) = match self.line[i].kind {
                LineKind::Text { item, g0, g1 } => (item, g0, g1),
                LineKind::End(_) | LineKind::Start(_) | LineKind::Abs { .. } | LineKind::Break => continue,
                _ => return,
            };
            let InlineItem::Text(t) = &self.ifc.items[item] else {
                return;
            };
            if matches!(t.style.white_space, WhiteSpace::Pre | WhiteSpace::BreakSpaces) {
                return;
            }
            let glyphs = self.glyphs[item].as_ref().unwrap();
            let mut removed = 0.0;
            while g1 > g0 && t.text[glyphs[g1 - 1].cluster as usize..].starts_with(' ') {
                removed += glyphs[g1 - 1].advance;
                g1 -= 1;
            }
            self.line[i].width -= removed;
            self.line_width -= removed;
            if let LineKind::Text { g1: ref mut end, .. } = self.line[i].kind {
                *end = g1;
            }
            if g1 > g0 {
                return;
            }
        }
    }

    fn finish_line(&mut self, forced: bool) {
        self.trim_trailing_spaces();
        self.apply_text_overflow();
        let has_content = self.line_has_content() || self.line.iter().any(|i| matches!(i.kind, LineKind::Break));
        let has_abs = self.line.iter().any(|i| matches!(i.kind, LineKind::Abs { .. }));
        let (l, r) = self.line_band();
        if has_content || has_abs {
            let frag = self.build_line(l, r, forced, has_content);
            let h = frag.rect.height;
            if has_content {
                let bl = frag.first_baseline.unwrap_or(0.0) + frag.rect.y;
                self.first_baseline.get_or_insert(bl);
                self.last_baseline = Some(bl);
                self.first_line = false;
            }
            self.out.push(frag);
            self.y += h;
        }
        // Track inline boxes still open for the next line.
        self.open_at_line_start = self.open_after_line();
        self.line.clear();
        self.line_width = 0.0;
        self.floats.raise_floor(self.origin.y + self.y.min(f32::MAX));
        let pending = std::mem::take(&mut self.pending_floats);
        for idx in pending {
            self.place_pending_float(idx);
        }
    }

    /// `text-overflow: ellipsis` on a clipping container: truncates text that overflows
    /// the line and appends "…".
    fn apply_text_overflow(&mut self) {
        let s = &self.container.style;
        if !s.text_overflow_ellipsis || s.overflow_x == Overflow::Visible {
            return;
        }
        let (l, r) = self.line_band();
        let avail = r - l;
        if self.line_width <= avail + 0.01 {
            return;
        }
        // Style the ellipsis like the last text item on the line.
        let Some(text_item) = self.line.iter().rev().find_map(|i| match i.kind {
            LineKind::Text { item, .. } => Some(item),
            _ => None,
        }) else {
            return;
        };
        let InlineItem::Text(t) = &self.ifc.items[text_item] else {
            return;
        };
        let face = font_for(self.ctx, &t.style);
        let ell_w = self.ctx.fonts.measure(face, t.style.font_size, "\u{2026}");
        let limit = (avail - ell_w).max(0.0);
        let mut used = 0.0;
        let mut cut = self.line.len();
        for (idx, it) in self.line.iter_mut().enumerate() {
            if used + it.width <= limit + 0.01 {
                used += it.width;
                continue;
            }
            if let LineKind::Text { item, g0, g1 } = &mut it.kind {
                let glyphs = self.glyphs[*item].as_ref().unwrap();
                let mut w = 0.0;
                let mut k = *g0;
                while k < *g1 && used + w + glyphs[k].advance <= limit + 0.01 {
                    w += glyphs[k].advance;
                    k += 1;
                }
                *g1 = k;
                it.width = w;
                used += w;
                cut = idx + 1;
            } else {
                cut = idx;
            }
            break;
        }
        // Keep inline box ends so the inline structure stays balanced.
        let tail: Vec<LineItem> = self
            .line
            .drain(cut..)
            .filter(|i| matches!(i.kind, LineKind::End(_)))
            .collect();
        self.line.push(LineItem {
            kind: LineKind::Ellipsis { item: text_item },
            width: ell_w,
        });
        self.line.extend(tail);
        self.line_width = used + ell_w;
    }

    fn open_after_line(&self) -> Vec<usize> {
        let mut stack = self.open_at_line_start.clone();
        for it in &self.line {
            match it.kind {
                LineKind::Start(b) => stack.push(b),
                LineKind::End(b) => {
                    if let Some(p) = stack.iter().rposition(|&x| x == b) {
                        stack.remove(p);
                    }
                }
                _ => {}
            }
        }
        stack
    }

    fn parent_map(&self) -> Vec<Option<usize>> {
        // Parent box of each line item (None = root inline box), and of each box.
        let mut parents = Vec::with_capacity(self.line.len());
        let mut stack = self.open_at_line_start.clone();
        for it in &self.line {
            match it.kind {
                LineKind::Start(b) => {
                    parents.push(stack.last().copied());
                    stack.push(b);
                }
                LineKind::End(b) => {
                    if let Some(p) = stack.iter().rposition(|&x| x == b) {
                        stack.remove(p);
                    }
                    parents.push(stack.last().copied());
                }
                _ => parents.push(stack.last().copied()),
            }
        }
        parents
    }

    /// Builds the line fragment: vertical alignment, horizontal alignment and the nested
    /// inline fragment tree.
    fn build_line(&mut self, left: f32, right: f32, forced: bool, has_content: bool) -> Fragment {
        let n_boxes = self.boxes.len();
        // Parent of each inline box present on this line.
        let item_parents = self.parent_map();
        let mut box_parent: Vec<Option<Option<usize>>> = vec![None; n_boxes];
        for &b in &self.open_at_line_start {
            // Continuations: parent is the previous entry in the open stack.
            let pos = self.open_at_line_start.iter().position(|&x| x == b).unwrap();
            box_parent[b] = Some(if pos == 0 {
                None
            } else {
                Some(self.open_at_line_start[pos - 1])
            });
        }
        for (i, it) in self.line.iter().enumerate() {
            if let LineKind::Start(b) = it.kind {
                box_parent[b] = Some(item_parents[i]);
            }
        }
        // Baseline offsets relative to the root baseline (positive = down).
        let mut off: Vec<Option<f32>> = vec![None; n_boxes];
        let mut deferred: Vec<usize> = Vec::new(); // top/bottom-aligned boxes
        let present: Vec<usize> = (0..n_boxes).filter(|&b| box_parent[b].is_some()).collect();
        let root_metrics = self.root.metrics;
        let root_fs = self.root.font_size;
        fn compute_off(
            b: usize,
            br: &Breaker,
            box_parent: &[Option<Option<usize>>],
            off: &mut Vec<Option<f32>>,
            deferred: &mut Vec<usize>,
            root: (FontMetrics, f32),
        ) -> f32 {
            if let Some(v) = off[b] {
                return v;
            }
            let parent = box_parent[b].flatten();
            let (p_off, pm, pfs) = match parent {
                Some(p) => (
                    compute_off(p, br, box_parent, off, deferred, root),
                    br.boxes[p].metrics,
                    br.boxes[p].font_size,
                ),
                None => (0.0, root.0, root.1),
            };
            let m = &br.boxes[b];
            let style = &br.ifc.boxes[b].style;
            let v = match &style.vertical_align {
                VerticalAlign::Baseline => p_off,
                VerticalAlign::Sub => p_off + (pfs / 5.0 + 1.0),
                VerticalAlign::Super => p_off - (pfs / 3.0 + 1.0),
                VerticalAlign::TextTop => p_off - pm.ascent + m.metrics.ascent,
                VerticalAlign::TextBottom => p_off + pm.descent - m.metrics.descent,
                VerticalAlign::Middle => p_off - pm.x_height / 2.0 + (m.metrics.ascent - m.metrics.descent) / 2.0,
                VerticalAlign::Length(l) => p_off - l.resolve(m.a_lead + m.d_lead),
                VerticalAlign::Top | VerticalAlign::Bottom => {
                    deferred.push(b);
                    p_off
                }
            };
            off[b] = Some(v);
            v
        }
        for &b in &present {
            compute_off(b, self, &box_parent, &mut off, &mut deferred, (root_metrics, root_fs));
        }
        // Atomic and marker contributions.
        // Quirks mode "line height calculation quirk": inline boxes (including the root
        // inline box) with no text and no vertical borders/padding don't affect the line.
        let quirks = self.ctx.doc.quirks_mode != crate::dom::QuirksMode::NoQuirks;
        let mut direct_text = vec![false; n_boxes];
        let mut root_direct = false;
        for (i, it) in self.line.iter().enumerate() {
            let has = match it.kind {
                LineKind::Text { g0, g1, .. } => g1 > g0,
                LineKind::Marker { .. } | LineKind::Break => true,
                _ => false,
            };
            if has {
                match item_parents[i] {
                    Some(b) => direct_text[b] = true,
                    None => root_direct = true,
                }
            }
        }
        let contributes = |b: usize| -> bool {
            if !quirks || direct_text[b] {
                return true;
            }
            let m = &self.boxes[b];
            m.border.vertical() + m.padding.vertical() > 0.0
        };
        let (mut top, mut bottom) = if !quirks || root_direct {
            (-self.root.a_lead, self.root.d_lead)
        } else {
            (f32::INFINITY, f32::NEG_INFINITY)
        };
        let in_deferred = |b: Option<usize>, box_parent: &[Option<Option<usize>>], deferred: &[usize]| {
            let mut cur = b;
            while let Some(x) = cur {
                if deferred.contains(&x) {
                    return true;
                }
                cur = box_parent[x].flatten();
            }
            false
        };
        for &b in &present {
            if in_deferred(Some(b), &box_parent, &deferred) || !contributes(b) {
                continue;
            }
            let m = &self.boxes[b];
            let o = off[b].unwrap();
            top = top.min(o - m.a_lead);
            bottom = bottom.max(o + m.d_lead);
        }
        // Atomic boxes: (top relative to root baseline) computed per item.
        let mut atomic_top: Vec<Option<f32>> = vec![None; self.line.len()];
        let mut deferred_atomics: Vec<usize> = Vec::new();
        for (i, it) in self.line.iter().enumerate() {
            let LineKind::Atomic { item } = it.kind else { continue };
            let frag = self.atomics[item].as_ref().unwrap();
            let parent = item_parents[i];
            let (p_off, pm, pfs) = match parent {
                Some(p) => (off[p].unwrap_or(0.0), self.boxes[p].metrics, self.boxes[p].font_size),
                None => (0.0, root_metrics, root_fs),
            };
            let h = frag.rect.height + frag.margin.vertical();
            let baseline = atomic_baseline(frag);
            let t = match &frag.style.vertical_align {
                VerticalAlign::Baseline => p_off - baseline,
                VerticalAlign::Sub => p_off + (pfs / 5.0 + 1.0) - baseline,
                VerticalAlign::Super => p_off - (pfs / 3.0 + 1.0) - baseline,
                VerticalAlign::TextTop => p_off - pm.ascent,
                VerticalAlign::TextBottom => p_off + pm.descent - h,
                VerticalAlign::Middle => p_off - pm.x_height / 2.0 - h / 2.0,
                VerticalAlign::Length(l) => p_off - baseline - l.resolve(h),
                VerticalAlign::Top | VerticalAlign::Bottom => {
                    deferred_atomics.push(i);
                    0.0
                }
            };
            atomic_top[i] = Some(t);
            if !deferred_atomics.contains(&i) && !in_deferred(parent, &box_parent, &deferred) {
                top = top.min(t);
                bottom = bottom.max(t + h);
            }
        }
        // Top/bottom-aligned boxes and atomics.
        if !top.is_finite() || !bottom.is_finite() {
            top = 0.0;
            bottom = 0.0;
        }
        for &b in &deferred {
            let m = &self.boxes[b];
            let h = m.a_lead + m.d_lead;
            let style = &self.ifc.boxes[b].style;
            if style.vertical_align == VerticalAlign::Top {
                bottom = bottom.max(top + h);
            } else {
                top = top.min(bottom - h);
            }
        }
        for &i in &deferred_atomics {
            let LineKind::Atomic { item } = self.line[i].kind else {
                continue;
            };
            let frag = self.atomics[item].as_ref().unwrap();
            let h = frag.rect.height + frag.margin.vertical();
            if frag.style.vertical_align == VerticalAlign::Top {
                bottom = bottom.max(top + h);
            } else {
                top = top.min(bottom - h);
            }
        }
        for &b in &deferred {
            let m = &self.boxes[b];
            let style = &self.ifc.boxes[b].style;
            let new_off = if style.vertical_align == VerticalAlign::Top {
                top + m.a_lead
            } else {
                bottom - m.d_lead
            };
            let delta = new_off - off[b].unwrap();
            // Shift the box and its descendants.
            for &d in &present {
                let mut cur = Some(d);
                while let Some(x) = cur {
                    if x == b {
                        off[d] = Some(off[d].unwrap() + delta);
                        break;
                    }
                    cur = box_parent[x].flatten();
                }
            }
        }
        for &i in &deferred_atomics {
            let LineKind::Atomic { item } = self.line[i].kind else {
                continue;
            };
            let frag = self.atomics[item].as_ref().unwrap();
            let h = frag.rect.height + frag.margin.vertical();
            atomic_top[i] = Some(if frag.style.vertical_align == VerticalAlign::Top {
                top
            } else {
                bottom - h
            });
        }
        if !top.is_finite() || !bottom.is_finite() {
            top = 0.0;
            bottom = 0.0;
        }
        let (line_height, baseline) = if has_content { (bottom - top, -top) } else { (0.0, 0.0) };

        // Horizontal alignment.
        let avail = right - left;
        let free = (avail - self.line_width).max(0.0);
        let align = self.container.style.text_align;
        let mut justify_extra = 0.0;
        let mut x = match align {
            TextAlign::Right => free,
            TextAlign::Center | TextAlign::WebkitCenter => free / 2.0,
            _ => 0.0,
        };
        if align == TextAlign::Justify && !forced && free > 0.0 {
            let spaces = self.count_spaces();
            if spaces > 0 {
                justify_extra = free / spaces as f32;
            }
        }
        let mut line_frag = Fragment::new(
            FragmentKind::Line,
            None,
            self.container.style.clone(),
            Rect::new(left, self.y, avail.max(0.0), line_height),
        );
        line_frag.first_baseline = Some(baseline);
        line_frag.last_baseline = Some(baseline);

        // Build nested fragments.
        struct Open {
            b: usize,
            frag: Fragment,
            y: f32,
        }
        let mut stack: Vec<Open> = Vec::new();
        let make_inline = |br: &Self, b: usize, x: f32, start: bool, baseline: f32, off: &[Option<f32>]| -> Open {
            let m = &br.boxes[b];
            let info = &br.ifc.boxes[b];
            let o = off[b].unwrap_or(0.0);
            let y = baseline + o - m.metrics.ascent - m.padding.top - m.border.top;
            let h = m.metrics.ascent + m.metrics.descent + m.padding.vertical() + m.border.vertical();
            let mut f = Fragment::new(
                FragmentKind::Inline { start, end: false },
                info.node,
                info.style.clone(),
                Rect::new(x, y, 0.0, h),
            );
            f.border = Edges {
                top: m.border.top,
                bottom: m.border.bottom,
                left: if start { m.border.left } else { 0.0 },
                right: 0.0,
            };
            f.padding = Edges {
                top: m.padding.top,
                bottom: m.padding.bottom,
                left: if start { m.padding.left } else { 0.0 },
                right: 0.0,
            };
            f.rel_offset = relative_offset(&info.style, br.cb.width, br.cb.height);
            Open { b, frag: f, y }
        };
        fn attach(stack: &mut [Open], line: &mut Fragment, mut child: Fragment) {
            match stack.last_mut() {
                Some(parent) => {
                    child.rect.x -= parent.frag.rect.x;
                    child.rect.y -= parent.y;
                    parent.frag.children.push(child);
                }
                None => line.children.push(child),
            }
        }
        for &b in &self.open_at_line_start.clone() {
            let o = make_inline(self, b, x, false, baseline, &off);
            stack.push(o);
        }
        let line_items = std::mem::take(&mut self.line);
        for (i, it) in line_items.iter().enumerate() {
            match it.kind {
                LineKind::Start(b) => {
                    let start = self.ifc.boxes[b].start_edge;
                    let m = self.boxes[b].clone();
                    if start {
                        x += m.margin.left;
                    }
                    let o = make_inline(self, b, x, start, baseline, &off);
                    stack.push(o);
                    if start {
                        x += m.border.left + m.padding.left;
                    }
                }
                LineKind::End(b) => {
                    let end = self.ifc.boxes[b].end_edge;
                    let m = self.boxes[b].clone();
                    if end {
                        x += m.padding.right + m.border.right;
                    }
                    if let Some(pos) = stack.iter().rposition(|o| o.b == b) {
                        // Close any boxes opened inside that were not closed (malformed).
                        while stack.len() > pos + 1 {
                            let mut inner = stack.pop().unwrap();
                            inner.frag.rect.width = x - inner.frag.rect.x;
                            attach(&mut stack, &mut line_frag, inner.frag);
                        }
                        let mut o = stack.pop().unwrap();
                        o.frag.rect.width = x - o.frag.rect.x;
                        if end {
                            o.frag.kind = FragmentKind::Inline {
                                start: matches!(o.frag.kind, FragmentKind::Inline { start: true, .. }),
                                end: true,
                            };
                            o.frag.border.right = m.border.right;
                            o.frag.padding.right = m.padding.right;
                        }
                        attach(&mut stack, &mut line_frag, o.frag);
                    }
                    if end {
                        x += m.margin.right;
                    }
                }
                LineKind::Text { item, g0, g1 } => {
                    if g1 <= g0 {
                        continue;
                    }
                    let InlineItem::Text(t) = &self.ifc.items[item] else {
                        continue;
                    };
                    let glyphs = self.glyphs[item].as_ref().unwrap();
                    let face = font_for(self.ctx, &t.style);
                    let metrics = self.ctx.fonts.metrics(face, t.style.font_size);
                    let p_off = item_parents[i].and_then(|p| off[p]).unwrap_or(0.0);
                    let mut pen = 0.0;
                    let mut pg = Vec::with_capacity(g1 - g0);
                    for g in &glyphs[g0..g1] {
                        pg.push(PositionedGlyph {
                            face: g.face,
                            glyph: g.glyph,
                            x: pen + g.x_offset,
                            y: -g.y_offset,
                        });
                        pen += g.advance;
                        if justify_extra > 0.0 && t.text[g.cluster as usize..].starts_with(' ') {
                            pen += justify_extra;
                        }
                    }
                    let start_byte = glyphs[g0].cluster as usize;
                    let end_byte = glyphs.get(g1).map(|g| g.cluster as usize).unwrap_or(t.text.len());
                    let run = TextRun {
                        glyphs: pg,
                        font_size: t.style.font_size,
                        baseline: metrics.ascent,
                        metrics,
                        text: t.text[start_byte.min(end_byte)..end_byte.max(start_byte)].to_string(),
                    };
                    let y = baseline + p_off - metrics.ascent;
                    let frag = Fragment::new(
                        FragmentKind::Text(Rc::new(run)),
                        t.node,
                        t.style.clone(),
                        Rect::new(x, y, pen, metrics.ascent + metrics.descent),
                    );
                    x += pen;
                    attach(&mut stack, &mut line_frag, frag);
                }
                LineKind::Atomic { item } => {
                    let mut frag = self.atomics[item].take().unwrap();
                    let t = atomic_top[i].unwrap_or(0.0);
                    frag.rect.x = x + frag.margin.left;
                    frag.rect.y = baseline + t + frag.margin.top;
                    x += it.width;
                    attach(&mut stack, &mut line_frag, frag);
                }
                LineKind::Marker { item } => {
                    let InlineItem::Marker(mk) = &self.ifc.items[item] else {
                        continue;
                    };
                    let face = font_for(self.ctx, &mk.style);
                    let metrics = self.ctx.fonts.metrics(face, mk.style.font_size);
                    let glyphs = self.ctx.fonts.shape(face, mk.style.font_size, &mk.text, 0.0, 0.0);
                    let mut pen = 0.0;
                    let pg: Vec<PositionedGlyph> = glyphs
                        .iter()
                        .map(|g| {
                            let p = PositionedGlyph {
                                face: g.face,
                                glyph: g.glyph,
                                x: pen + g.x_offset,
                                y: -g.y_offset,
                            };
                            pen += g.advance;
                            p
                        })
                        .collect();
                    let run = TextRun {
                        glyphs: pg,
                        font_size: mk.style.font_size,
                        baseline: metrics.ascent,
                        metrics,
                        text: mk.text.clone(),
                    };
                    let mx = if mk.outside { -left - pen } else { x };
                    let frag = Fragment::new(
                        FragmentKind::Marker(Rc::new(run)),
                        None,
                        mk.style.clone(),
                        Rect::new(mx, baseline - metrics.ascent, pen, metrics.ascent + metrics.descent),
                    );
                    if !mk.outside {
                        x += pen;
                    }
                    line_frag.children.push(frag);
                }
                LineKind::Abs { item, idx } => {
                    let InlineItem::Abs(b) = &self.ifc.items[item] else {
                        continue;
                    };
                    let frag = Fragment::new(
                        FragmentKind::AbsPlaceholder(idx),
                        b.node,
                        b.style.clone(),
                        Rect::new(x, 0.0, 0.0, 0.0),
                    );
                    line_frag.children.push(frag);
                }
                LineKind::Break => {}
                LineKind::Ellipsis { item } => {
                    let InlineItem::Text(t) = &self.ifc.items[item] else {
                        continue;
                    };
                    let face = font_for(self.ctx, &t.style);
                    let metrics = self.ctx.fonts.metrics(face, t.style.font_size);
                    let glyphs = self.ctx.fonts.shape(face, t.style.font_size, "\u{2026}", 0.0, 0.0);
                    let mut pen = 0.0;
                    let pg: Vec<PositionedGlyph> = glyphs
                        .iter()
                        .map(|g| {
                            let p = PositionedGlyph {
                                face: g.face,
                                glyph: g.glyph,
                                x: pen + g.x_offset,
                                y: -g.y_offset,
                            };
                            pen += g.advance;
                            p
                        })
                        .collect();
                    let p_off = item_parents[i].and_then(|p| off[p]).unwrap_or(0.0);
                    let run = TextRun {
                        glyphs: pg,
                        font_size: t.style.font_size,
                        baseline: metrics.ascent,
                        metrics,
                        text: "\u{2026}".to_string(),
                    };
                    let frag = Fragment::new(
                        FragmentKind::Text(Rc::new(run)),
                        t.node,
                        t.style.clone(),
                        Rect::new(
                            x,
                            baseline + p_off - metrics.ascent,
                            pen,
                            metrics.ascent + metrics.descent,
                        ),
                    );
                    x += pen;
                    attach(&mut stack, &mut line_frag, frag);
                }
            }
        }
        while let Some(mut o) = stack.pop() {
            o.frag.rect.width = x - o.frag.rect.x;
            attach(&mut stack, &mut line_frag, o.frag);
        }
        self.line = line_items;
        line_frag
    }

    fn count_spaces(&self) -> usize {
        let mut n = 0;
        for it in &self.line {
            if let LineKind::Text { item, g0, g1 } = it.kind {
                let InlineItem::Text(t) = &self.ifc.items[item] else {
                    continue;
                };
                let glyphs = self.glyphs[item].as_ref().unwrap();
                n += glyphs[g0..g1]
                    .iter()
                    .filter(|g| t.text[g.cluster as usize..].starts_with(' '))
                    .count();
            }
        }
        n
    }
}

/// Distance from an atomic inline's top margin edge to its baseline.
fn atomic_baseline(frag: &Fragment) -> f32 {
    let h = frag.rect.height + frag.margin.vertical();
    let clips = frag.style.clips_overflow();
    let bl = match frag.style.display {
        Display::InlineBlock if !clips => frag.last_baseline,
        Display::InlineFlex | Display::InlineGrid | Display::InlineTable => frag.first_baseline,
        _ => None,
    };
    match bl {
        Some(b) => frag.margin.top + b,
        None => h,
    }
}

/// Min-content and max-content widths of an inline formatting context (content-box).
pub fn inline_intrinsic(ctx: &LayoutContext, ifc: &InlineContent, container: &ComputedStyle) -> (f32, f32) {
    let mut min: f32 = 0.0;
    let mut max: f32 = 0.0;
    let mut line: f32 = container.text_indent.resolve(0.0).max(0.0);
    let mut word: f32 = 0.0; // current unbreakable chunk
    let mut word_trailing: f32 = 0.0;
    let cbw = 0.0;
    let finish_word = |word: &mut f32, trailing: &mut f32, min: &mut f32| {
        *min = min.max(*word - *trailing);
        *word = 0.0;
        *trailing = 0.0;
    };
    for item in &ifc.items {
        match item {
            InlineItem::Text(t) => {
                let g = shaped(ctx, t);
                for s in segments(t, &g) {
                    word += s.width;
                    word_trailing = s.trailing;
                    line += s.width;
                    if s.break_after {
                        finish_word(&mut word, &mut word_trailing, &mut min);
                    }
                }
            }
            InlineItem::Start(b) | InlineItem::End(b) => {
                let m = box_metrics(ctx, &ifc.boxes[*b].style, cbw);
                let info = &ifc.boxes[*b];
                let w = if matches!(item, InlineItem::Start(_)) {
                    if info.start_edge {
                        m.margin.left + m.border.left + m.padding.left
                    } else {
                        0.0
                    }
                } else if info.end_edge {
                    m.margin.right + m.border.right + m.padding.right
                } else {
                    0.0
                };
                word += w;
                line += w;
            }
            InlineItem::Atomic(b) => {
                let (amin, amax) = super::intrinsic::intrinsic_widths(ctx, b);
                let geom = super::BoxGeometry::resolve(&b.style, 0.0);
                let m = geom.margin_or_zero().horizontal();
                if wraps(container.white_space) {
                    finish_word(&mut word, &mut word_trailing, &mut min);
                    min = min.max(amin + m);
                } else {
                    word += amin + m;
                }
                line += amax + m;
            }
            InlineItem::Float(b) => {
                let (fmin, fmax) = super::intrinsic::intrinsic_widths(ctx, b);
                let geom = super::BoxGeometry::resolve(&b.style, 0.0);
                let m = geom.margin_or_zero().horizontal();
                min = min.max(fmin + m);
                line += fmax + m;
            }
            InlineItem::Break(_) => {
                finish_word(&mut word, &mut word_trailing, &mut min);
                max = max.max(line);
                line = 0.0;
            }
            InlineItem::Marker(mk) => {
                if !mk.outside {
                    let face = font_for(ctx, &mk.style);
                    let w = ctx.fonts.measure(face, mk.style.font_size, &mk.text);
                    word += w;
                    line += w;
                }
            }
            InlineItem::Abs(_) => {}
        }
    }
    finish_word(&mut word, &mut word_trailing, &mut min);
    max = max.max(line);
    // Trailing spaces on the last line don't count.
    (min, max.max(min))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    fn item(text: &str, ws: WhiteSpace) -> TextItem {
        let mut s = (*ComputedStyle::initial()).clone();
        s.white_space = ws;
        TextItem {
            text: text.to_string(),
            style: Rc::new(s),
            node: None,
            shaped: RefCell::new(None),
        }
    }

    fn fake_glyphs(text: &str) -> Vec<ShapedGlyph> {
        text.char_indices()
            .map(|(i, _)| ShapedGlyph {
                face: FaceId(0),
                glyph: 1,
                cluster: i as u32,
                advance: 10.0,
                x_offset: 0.0,
                y_offset: 0.0,
            })
            .collect()
    }

    #[test]
    fn segments_break_after_spaces_and_hyphens() {
        let t = item("hello  big well-known world", WhiteSpace::Normal);
        let segs = segments(&t, &fake_glyphs(&t.text));
        let widths: Vec<f32> = segs.iter().map(|s| s.width).collect();
        // "hello  " | "big " | "well-" | "known " | "world"
        assert_eq!(widths, vec![70.0, 40.0, 50.0, 60.0, 50.0]);
        assert_eq!(segs[0].trailing, 20.0);
        assert!(segs[0].break_after && !segs[4].break_after);
    }

    #[test]
    fn nowrap_has_no_breaks() {
        let t = item("a b c", WhiteSpace::Nowrap);
        let segs = segments(&t, &fake_glyphs(&t.text));
        assert_eq!(segs.len(), 1);
        assert!(!segs[0].break_after);
    }
}
