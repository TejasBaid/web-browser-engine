//! Display list construction from the fragment tree, following the painting order of
//! CSS 2.1 Appendix E: within each stacking context, the root's background and
//! borders, negative z-index children, in-flow block backgrounds, floats, inline
//! content, positioned descendants with `z-index: auto | 0` in tree order, then positive
//! z-index children. Opacity creates a group; `overflow` clips descendants.

use crate::css::color::Color;
use crate::dom::{Document, NodeId};
use crate::image::Image;
use crate::layout::fragment::{Fragment, FragmentKind, TextRun};
use crate::layout::{Edges, LayoutTree, Point, Rect, Resources};
use crate::style::computed::*;
use std::rc::Rc;

/// Corner radii (horizontal, vertical): top-left, top-right, bottom-right, bottom-left.
pub type Radii = [(f32, f32); 4];

/// One drawing command. Coordinates are document coordinates in CSS px.
#[derive(Clone, Debug)]
pub enum DisplayItem {
    /// A solid rectangle, optionally with rounded corners.
    Rect {
        /// Area.
        rect: Rect,
        /// Fill color.
        color: Color,
        /// Corner radii (all zero for square corners).
        radii: Radii,
    },
    /// Box borders.
    Border {
        /// Border box.
        rect: Rect,
        /// Widths.
        widths: Edges,
        /// Colors (top, right, bottom, left).
        colors: [Color; 4],
        /// Styles (top, right, bottom, left).
        styles: [BorderStyle; 4],
        /// Outer radii.
        radii: Radii,
    },
    /// An outer box shadow.
    BoxShadow {
        /// The border box casting the shadow.
        rect: Rect,
        /// Its radii.
        radii: Radii,
        /// Offset x.
        dx: f32,
        /// Offset y.
        dy: f32,
        /// Blur radius.
        blur: f32,
        /// Spread.
        spread: f32,
        /// Color.
        color: Color,
    },
    /// A run of glyphs with its baseline starting at `origin`.
    Text {
        /// Baseline origin.
        origin: Point,
        /// Glyphs.
        run: Rc<TextRun>,
        /// Color.
        color: Color,
    },
    /// An image scaled into `rect` (clipped to `clip`).
    Image {
        /// Destination rectangle.
        rect: Rect,
        /// Clip (the content box for `object-fit`).
        clip: Rect,
        /// Pixels.
        image: Rc<Image>,
    },
    /// An image tiled over an area (backgrounds).
    TiledImage {
        /// Area painted (clip).
        area: Rect,
        /// Origin and size of one tile.
        tile: Rect,
        /// Repeat in x.
        repeat_x: bool,
        /// Repeat in y.
        repeat_y: bool,
        /// Pixels.
        image: Rc<Image>,
    },
    /// Starts clipping to a (possibly rounded) rectangle.
    PushClip {
        /// Clip rectangle.
        rect: Rect,
        /// Corner radii.
        radii: Radii,
    },
    /// Ends the innermost clip.
    PopClip,
    /// Starts a group composited with `opacity`; `bounds` covers everything drawn in it.
    PushOpacity {
        /// Group opacity.
        opacity: f32,
        /// Bounds of the group's content (document coordinates).
        bounds: Rect,
    },
    /// Ends the innermost opacity group.
    PopOpacity,
    /// Starts content that does not scroll (`position: fixed`).
    PushFixed,
    /// Ends fixed content.
    PopFixed,
}

/// A clickable link area.
#[derive(Clone, Debug)]
pub struct HitRegion {
    /// Area in document coordinates (viewport coordinates when `fixed`).
    pub rect: Rect,
    /// The `<a>` element.
    pub link: NodeId,
    /// Whether the area belongs to fixed-position content.
    pub fixed: bool,
}

/// A display list for a page.
#[derive(Clone, Debug, Default)]
pub struct DisplayList {
    /// Drawing commands in paint order.
    pub items: Vec<DisplayItem>,
    /// Canvas background (propagated from the root or body element).
    pub background: Color,
    /// Link areas in paint order (later entries are on top).
    pub links: Vec<HitRegion>,
    /// Document size.
    pub width: f32,
    /// Document height.
    pub height: f32,
}

impl DisplayList {
    /// Returns a copy with every coordinate and font size multiplied by `scale`, for
    /// rendering at a device pixel ratio other than 1 (e.g. 2 on Retina displays).
    pub fn scaled(&self, scale: f32) -> DisplayList {
        if scale == 1.0 {
            return self.clone();
        }
        let r = |x: &Rect| Rect::new(x.x * scale, x.y * scale, x.width * scale, x.height * scale);
        let radii = |rd: &Radii| rd.map(|(h, v)| (h * scale, v * scale));
        let items = self
            .items
            .iter()
            .map(|it| match it {
                DisplayItem::Rect { rect, color, radii: rd } => DisplayItem::Rect {
                    rect: r(rect),
                    color: *color,
                    radii: radii(rd),
                },
                DisplayItem::Border {
                    rect,
                    widths,
                    colors,
                    styles,
                    radii: rd,
                } => DisplayItem::Border {
                    rect: r(rect),
                    widths: Edges {
                        top: widths.top * scale,
                        right: widths.right * scale,
                        bottom: widths.bottom * scale,
                        left: widths.left * scale,
                    },
                    colors: *colors,
                    styles: *styles,
                    radii: radii(rd),
                },
                DisplayItem::BoxShadow {
                    rect,
                    radii: rd,
                    dx,
                    dy,
                    blur,
                    spread,
                    color,
                } => DisplayItem::BoxShadow {
                    rect: r(rect),
                    radii: radii(rd),
                    dx: dx * scale,
                    dy: dy * scale,
                    blur: blur * scale,
                    spread: spread * scale,
                    color: *color,
                },
                DisplayItem::Text { origin, run, color } => {
                    let mut run2 = (**run).clone();
                    run2.font_size *= scale;
                    run2.baseline *= scale;
                    for g in run2.glyphs.iter_mut() {
                        g.x *= scale;
                        g.y *= scale;
                    }
                    DisplayItem::Text {
                        origin: Point::new(origin.x * scale, origin.y * scale),
                        run: Rc::new(run2),
                        color: *color,
                    }
                }
                DisplayItem::Image { rect, clip, image } => DisplayItem::Image {
                    rect: r(rect),
                    clip: r(clip),
                    image: image.clone(),
                },
                DisplayItem::TiledImage {
                    area,
                    tile,
                    repeat_x,
                    repeat_y,
                    image,
                } => DisplayItem::TiledImage {
                    area: r(area),
                    tile: r(tile),
                    repeat_x: *repeat_x,
                    repeat_y: *repeat_y,
                    image: image.clone(),
                },
                DisplayItem::PushClip { rect, radii: rd } => DisplayItem::PushClip {
                    rect: r(rect),
                    radii: radii(rd),
                },
                DisplayItem::PushOpacity { opacity, bounds } => DisplayItem::PushOpacity {
                    opacity: *opacity,
                    bounds: r(bounds),
                },
                other => other.clone(),
            })
            .collect();
        DisplayList {
            items,
            background: self.background,
            links: self.links.clone(),
            width: self.width * scale,
            height: self.height * scale,
        }
    }
}

/// Builds the display list for a laid-out document.
pub fn build_display_list(tree: &LayoutTree, doc: &Document, resources: &Resources) -> DisplayList {
    let mut b = Builder {
        doc,
        resources,
        items: Vec::new(),
        links: Vec::new(),
        skip_bg: None,
        fixed_depth: 0,
    };
    // Canvas background: the root element's, or the body's if the root has none.
    let mut background = Color::WHITE;
    if let Some(root_frag) = tree.root.children.iter().find(|f| f.node == doc.document_element()) {
        if !root_frag.style.background_color.is_transparent() || root_frag.style.background_image.is_some() {
            background = blend_over_white(root_frag.style.background_color);
            b.skip_bg = Some(root_frag.node.unwrap());
            if root_frag.style.background_image.is_some() {
                b.skip_bg = None;
            }
        } else if let Some(body) = doc.body()
            && let Some(bf) = find_child_frag(root_frag, body)
            && !bf.style.background_color.is_transparent()
        {
            background = blend_over_white(bf.style.background_color);
            b.skip_bg = Some(body);
        }
    }
    b.paint_stacking_context(&tree.root, Point::default(), None);
    DisplayList {
        items: b.items,
        background,
        links: b.links,
        width: tree.document_width,
        height: tree.document_height,
    }
}

fn blend_over_white(c: Color) -> Color {
    let a = c.a as f32 / 255.0;
    let mix = |v: u8| (v as f32 * a + 255.0 * (1.0 - a)).round() as u8;
    Color::rgb(mix(c.r), mix(c.g), mix(c.b))
}

fn find_child_frag(f: &Fragment, node: NodeId) -> Option<&Fragment> {
    f.children
        .iter()
        .find(|c| c.node == Some(node) && matches!(c.kind, FragmentKind::Box))
}

struct Builder<'a> {
    doc: &'a Document,
    resources: &'a Resources,
    items: Vec<DisplayItem>,
    links: Vec<HitRegion>,
    /// Element whose background was propagated to the canvas.
    skip_bg: Option<NodeId>,
    fixed_depth: usize,
}

/// A positioned descendant or nested stacking context collected for z-ordering.
struct Layered<'f> {
    frag: &'f Fragment,
    origin: Point,
    clip: Option<Rect>,
    link: Option<NodeId>,
}

fn creates_stacking_context(f: &Fragment) -> bool {
    matches!(
        f.kind,
        FragmentKind::Box | FragmentKind::Anonymous | FragmentKind::Inline { .. }
    ) && (f.style.creates_stacking_context() || f.fixed)
}

fn is_positioned_layer(f: &Fragment) -> bool {
    matches!(
        f.kind,
        FragmentKind::Box | FragmentKind::Anonymous | FragmentKind::Inline { .. }
    ) && (f.style.is_positioned() || f.style.creates_stacking_context() || f.fixed)
}

fn resolve_radii(style: &ComputedStyle, rect: &Rect) -> Radii {
    let mut r = [(0.0, 0.0); 4];
    for (i, (h, v)) in style.border_radius.iter().enumerate() {
        r[i] = (h.resolve(rect.width).max(0.0), v.resolve(rect.height).max(0.0));
    }
    // Scale down overlapping radii (CSS Backgrounds §5.5).
    let mut f: f32 = 1.0;
    let check = |sum: f32, len: f32, f: &mut f32| {
        if sum > len && sum > 0.0 {
            *f = f.min(len / sum);
        }
    };
    check(r[0].0 + r[1].0, rect.width, &mut f);
    check(r[3].0 + r[2].0, rect.width, &mut f);
    check(r[0].1 + r[3].1, rect.height, &mut f);
    check(r[1].1 + r[2].1, rect.height, &mut f);
    if f < 1.0 {
        for c in r.iter_mut() {
            c.0 *= f;
            c.1 *= f;
        }
    }
    r
}

fn link_of(doc: &Document, node: Option<NodeId>) -> Option<NodeId> {
    let n = node?;
    let e = doc.element(n)?;
    if (e.is_html("a") || e.is_html("area")) && e.attr("href").is_some() {
        Some(n)
    } else {
        None
    }
}

impl<'a> Builder<'a> {
    fn push(&mut self, item: DisplayItem) {
        self.items.push(item);
    }

    fn add_link(&mut self, rect: Rect, link: Option<NodeId>, fixed: bool) {
        if let Some(l) = link
            && rect.width > 0.0
            && rect.height > 0.0
        {
            self.links.push(HitRegion {
                rect,
                link: l,
                fixed: fixed || self.fixed_depth > 0,
            });
        }
    }

    /// Paints `f` as a stacking context root (or a positioned "pseudo" stacking
    /// context, which we treat the same way).
    fn paint_stacking_context(&mut self, f: &Fragment, origin: Point, link: Option<NodeId>) {
        if f.style.has_mask {
            return; // masks are unsupported: draw nothing rather than a solid box
        }
        let rect = f.rect.translate(origin.x, origin.y);
        let link = link_of(self.doc, f.node).or(link);
        if f.fixed {
            self.fixed_depth += 1;
            self.push(DisplayItem::PushFixed);
        }
        let opacity = f.style.opacity;
        if opacity < 1.0 && f.kind != FragmentKind::Viewport {
            // Conservative bounds: the subtree's overflow, grown for shadows and outlines.
            let bounds = f
                .scrollable_overflow()
                .translate(origin.x, origin.y)
                .outset(&Edges::all(64.0));
            self.push(DisplayItem::PushOpacity { opacity, bounds });
        }
        if f.kind != FragmentKind::Viewport {
            if matches!(f.kind, FragmentKind::Inline { .. }) {
                self.paint_inline_box(f, origin, link);
            } else {
                self.paint_box_decorations(f, rect);
                self.add_link(rect, link, f.fixed);
            }
        }
        // Collect layered descendants.
        let mut layers = Vec::new();
        let own_clip = if f.style.clips_overflow() && f.kind != FragmentKind::Viewport {
            Some(f.padding_box().translate(rect.x, rect.y))
        } else {
            None
        };
        for c in &f.children {
            self.collect_layers(c, Point::new(rect.x, rect.y), own_clip, link, &mut layers);
        }
        let mut negative: Vec<&Layered> = layers
            .iter()
            .filter(|l| l.frag.style.z_index.unwrap_or(0) < 0 && creates_stacking_context(l.frag))
            .collect();
        negative.sort_by_key(|l| l.frag.style.z_index.unwrap_or(0));
        for l in negative {
            self.paint_layer(l);
        }
        // Normal flow, clipped by this box's overflow.
        let clip_items = self.push_clip_for(f, rect);
        self.paint_flow(f, rect, link, Phase::Blocks);
        self.paint_flow(f, rect, link, Phase::Floats);
        if matches!(f.kind, FragmentKind::Inline { .. }) {
            // An inline stacking context: its contents are already inline content.
        }
        self.paint_flow(f, rect, link, Phase::Inline);
        if clip_items {
            self.push(DisplayItem::PopClip);
        }
        // z-index: auto / 0 positioned descendants in tree order, then positive.
        for l in layers.iter().filter(|l| {
            let z = l.frag.style.z_index.unwrap_or(0);
            z == 0 || (z < 0 && !creates_stacking_context(l.frag))
        }) {
            self.paint_layer(l);
        }
        let mut positive: Vec<&Layered> = layers
            .iter()
            .filter(|l| l.frag.style.z_index.unwrap_or(0) > 0)
            .collect();
        positive.sort_by_key(|l| l.frag.style.z_index.unwrap_or(0));
        for l in positive {
            self.paint_layer(l);
        }
        if opacity < 1.0 && f.kind != FragmentKind::Viewport {
            self.push(DisplayItem::PopOpacity);
        }
        if f.fixed {
            self.push(DisplayItem::PopFixed);
            self.fixed_depth -= 1;
        }
    }

    fn paint_layer(&mut self, l: &Layered) {
        if let Some(c) = l.clip {
            self.push(DisplayItem::PushClip {
                rect: c,
                radii: [(0.0, 0.0); 4],
            });
        }
        self.paint_stacking_context(l.frag, l.origin, l.link);
        if l.clip.is_some() {
            self.push(DisplayItem::PopClip);
        }
    }

    /// Finds positioned descendants / stacking contexts of a stacking context, without
    /// descending into them.
    fn collect_layers<'f>(
        &self,
        f: &'f Fragment,
        origin: Point,
        clip: Option<Rect>,
        link: Option<NodeId>,
        out: &mut Vec<Layered<'f>>,
    ) {
        if is_positioned_layer(f) {
            out.push(Layered {
                frag: f,
                origin,
                clip: if f.fixed { None } else { clip },
                link,
            });
            return;
        }
        let rect = f.rect.translate(origin.x, origin.y);
        let link = link_of(self.doc, f.node).or(link);
        let clip = if f.style.clips_overflow() && matches!(f.kind, FragmentKind::Box | FragmentKind::Anonymous) {
            let pb = f.padding_box().translate(rect.x, rect.y);
            Some(match clip {
                Some(c) => c.intersect(&pb),
                None => pb,
            })
        } else {
            clip
        };
        for c in &f.children {
            self.collect_layers(c, Point::new(rect.x, rect.y), clip, link, out);
        }
    }

    fn push_clip_for(&mut self, f: &Fragment, rect: Rect) -> bool {
        if f.style.clips_overflow() && matches!(f.kind, FragmentKind::Box | FragmentKind::Anonymous) {
            let pb = f.padding_box().translate(rect.x, rect.y);
            let radii = resolve_radii(&f.style, &rect);
            // Inner radii for the padding box.
            let inner: Radii = [
                (
                    (radii[0].0 - f.border.left).max(0.0),
                    (radii[0].1 - f.border.top).max(0.0),
                ),
                (
                    (radii[1].0 - f.border.right).max(0.0),
                    (radii[1].1 - f.border.top).max(0.0),
                ),
                (
                    (radii[2].0 - f.border.right).max(0.0),
                    (radii[2].1 - f.border.bottom).max(0.0),
                ),
                (
                    (radii[3].0 - f.border.left).max(0.0),
                    (radii[3].1 - f.border.bottom).max(0.0),
                ),
            ];
            self.push(DisplayItem::PushClip { rect: pb, radii: inner });
            true
        } else {
            false
        }
    }

    /// Walks the in-flow descendants of `f` for one painting phase.
    fn paint_flow(&mut self, f: &Fragment, rect: Rect, link: Option<NodeId>, phase: Phase) {
        for c in &f.children {
            self.paint_flow_child(c, Point::new(rect.x, rect.y), link, phase);
        }
    }

    fn paint_flow_child(&mut self, c: &Fragment, origin: Point, link: Option<NodeId>, phase: Phase) {
        if is_positioned_layer(c) || c.style.has_mask {
            return; // painted with the layers
        }
        let rect = c.rect.translate(origin.x, origin.y);
        let link = link_of(self.doc, c.node).or(link);
        match &c.kind {
            FragmentKind::Box | FragmentKind::Anonymous => {
                if c.style.is_floated() {
                    if phase == Phase::Floats {
                        self.paint_atomic(c, origin, link);
                    }
                    return;
                }
                if phase == Phase::Blocks {
                    self.paint_box_decorations(c, rect);
                    self.add_link(rect, link, false);
                }
                let clipped = self.push_clip_for(c, rect);
                for ch in &c.children {
                    self.paint_flow_child(ch, Point::new(rect.x, rect.y), link, phase);
                }
                if clipped {
                    self.push(DisplayItem::PopClip);
                }
            }
            FragmentKind::Line => {
                if phase == Phase::Inline {
                    for ch in &c.children {
                        self.paint_inline_content(ch, Point::new(rect.x, rect.y), link);
                    }
                } else if phase == Phase::Floats {
                    // Floats are not inside lines (they are siblings), nothing to do.
                }
            }
            FragmentKind::Image(img) => {
                if phase == Phase::Inline {
                    self.paint_image(c, rect, img.as_ref());
                }
            }
            FragmentKind::Text(_) | FragmentKind::Marker(_) | FragmentKind::Inline { .. } => {
                if phase == Phase::Inline {
                    self.paint_inline_content(c, origin, link);
                }
            }
            FragmentKind::AbsPlaceholder(_) | FragmentKind::Viewport => {}
        }
    }

    /// Paints content inside a line box.
    fn paint_inline_content(&mut self, c: &Fragment, origin: Point, link: Option<NodeId>) {
        if is_positioned_layer(c) || c.style.has_mask {
            return;
        }
        let rect = c.rect.translate(origin.x, origin.y);
        let link = link_of(self.doc, c.node).or(link);
        match &c.kind {
            FragmentKind::Inline { .. } => {
                self.paint_inline_box(c, origin, link);
            }
            FragmentKind::Text(run) => {
                if c.style.visibility == Visibility::Visible {
                    self.paint_text(c, rect, run, c.style.color);
                }
                self.add_link(rect, link, false);
            }
            FragmentKind::Marker(run) => {
                if c.style.visibility == Visibility::Visible {
                    self.push(DisplayItem::Text {
                        origin: Point::new(rect.x, rect.y + run.baseline),
                        run: run.clone(),
                        color: c.style.color,
                    });
                }
            }
            FragmentKind::Box | FragmentKind::Anonymous => {
                // Atomic inline (inline-block, replaced, inline-flex/table).
                self.paint_atomic(c, origin, link);
            }
            FragmentKind::Image(img) => self.paint_image(c, rect, img.as_ref()),
            _ => {}
        }
    }

    /// Paints an inline box fragment's background/borders and then its contents.
    fn paint_inline_box(&mut self, c: &Fragment, origin: Point, link: Option<NodeId>) {
        let rect = c.rect.translate(origin.x, origin.y);
        if c.style.visibility == Visibility::Visible {
            let bg = c.style.background_color;
            if !bg.is_transparent() {
                self.push(DisplayItem::Rect {
                    rect,
                    color: bg,
                    radii: [(0.0, 0.0); 4],
                });
            }
            if c.border.horizontal() + c.border.vertical() > 0.0 {
                self.push(DisplayItem::Border {
                    rect,
                    widths: c.border,
                    colors: c.style.border_color,
                    styles: c.style.border_style,
                    radii: [(0.0, 0.0); 4],
                });
            }
        }
        self.add_link(rect, link, false);
        for ch in &c.children {
            self.paint_inline_content(ch, Point::new(rect.x, rect.y), link);
        }
    }

    /// Paints a float or atomic inline as if it created a stacking context.
    fn paint_atomic(&mut self, c: &Fragment, origin: Point, link: Option<NodeId>) {
        self.paint_stacking_context(c, origin, link);
    }

    fn paint_text(&mut self, c: &Fragment, rect: Rect, run: &Rc<TextRun>, color: Color) {
        let baseline = rect.y + run.baseline;
        self.push(DisplayItem::Text {
            origin: Point::new(rect.x, baseline),
            run: run.clone(),
            color,
        });
        for d in c.style.text_decorations.iter() {
            let m = &run.metrics;
            let thickness = m.underline_thickness.max(1.0).round().max(1.0);
            let mut lines = Vec::new();
            if d.underline {
                // Skia reports the underline top as position + thickness / 2; Blink rounds it.
                lines.push(baseline + (m.underline_offset + m.underline_thickness / 2.0).round().max(1.0));
            }
            if d.overline {
                lines.push(baseline - m.ascent);
            }
            if d.line_through {
                lines.push(baseline - m.x_height / 2.0 - thickness / 2.0 + thickness / 2.0);
            }
            for y in lines {
                let y = y.round();
                match d.style {
                    DecorationStyle::Dotted | DecorationStyle::Dashed => {
                        let dash = if d.style == DecorationStyle::Dotted {
                            thickness
                        } else {
                            thickness * 3.0
                        };
                        let mut x = rect.x;
                        while x < rect.right() {
                            let w = dash.min(rect.right() - x);
                            self.push(DisplayItem::Rect {
                                rect: Rect::new(x, y, w, thickness),
                                color: d.color,
                                radii: [(0.0, 0.0); 4],
                            });
                            x += dash * 2.0;
                        }
                    }
                    DecorationStyle::Double => {
                        for dy in [0.0, thickness * 2.0] {
                            self.push(DisplayItem::Rect {
                                rect: Rect::new(rect.x, y + dy, rect.width, thickness),
                                color: d.color,
                                radii: [(0.0, 0.0); 4],
                            });
                        }
                    }
                    _ => self.push(DisplayItem::Rect {
                        rect: Rect::new(rect.x, y, rect.width, thickness),
                        color: d.color,
                        radii: [(0.0, 0.0); 4],
                    }),
                }
            }
        }
    }

    fn paint_image(&mut self, c: &Fragment, rect: Rect, img: Option<&Rc<Image>>) {
        let Some(img) = img else { return };
        if c.style.visibility != Visibility::Visible || rect.width <= 0.0 || rect.height <= 0.0 {
            return;
        }
        let (iw, ih) = (img.width as f32, img.height as f32);
        let dest = match c.style.object_fit {
            ObjectFit::Fill => rect,
            fit => {
                let sx = rect.width / iw;
                let sy = rect.height / ih;
                let s = match fit {
                    ObjectFit::Contain => sx.min(sy),
                    ObjectFit::Cover => sx.max(sy),
                    ObjectFit::None => 1.0,
                    ObjectFit::ScaleDown => sx.min(sy).min(1.0),
                    ObjectFit::Fill => 1.0,
                };
                let w = iw * s;
                let h = ih * s;
                Rect::new(rect.x + (rect.width - w) / 2.0, rect.y + (rect.height - h) / 2.0, w, h)
            }
        };
        self.push(DisplayItem::Image {
            rect: dest,
            clip: rect,
            image: img.clone(),
        });
    }

    /// Box shadow, background and borders of a block-level/atomic box.
    fn paint_box_decorations(&mut self, f: &Fragment, rect: Rect) {
        let s = &f.style;
        if s.visibility != Visibility::Visible {
            return;
        }
        let radii = resolve_radii(s, &rect);
        for sh in s.box_shadow.iter().rev() {
            if sh.inset || sh.color.is_transparent() {
                continue;
            }
            self.push(DisplayItem::BoxShadow {
                rect,
                radii,
                dx: sh.x,
                dy: sh.y,
                blur: sh.blur,
                spread: sh.spread,
                color: sh.color,
            });
        }
        let skip = self.skip_bg.is_some() && f.node == self.skip_bg && matches!(f.kind, FragmentKind::Box);
        if !skip && !s.background_color.is_transparent() {
            self.push(DisplayItem::Rect {
                rect,
                color: s.background_color,
                radii,
            });
        }
        if let Some(url) = &s.background_image {
            if let Some(img) = self.resources.image(url) {
                let size = (img.width as f32, img.height as f32);
                self.paint_background_image(f, rect, radii, size, BgSource::Raster(img));
            } else if let Some(svg) = self.resources.svg(url) {
                let size = crate::svg::intrinsic_size(&svg.doc, svg.root);
                self.paint_background_image(f, rect, radii, size, BgSource::Svg(svg));
            }
        } else if let Some(g) = &s.background_gradient {
            // Gradients have no intrinsic size: they fill the painting area.
            let area = f.padding_box();
            self.paint_background_image(f, rect, radii, (area.width, area.height), BgSource::Gradient(g.clone()));
        }
        if let Some(cb) = &f.collapsed_border {
            let w = cb.widths;
            if w.top + w.right + w.bottom + w.left > 0.0 {
                // Centred on the border-box edges: half of each border lies outside.
                let half = Edges {
                    top: w.top / 2.0,
                    right: w.right / 2.0,
                    bottom: w.bottom / 2.0,
                    left: w.left / 2.0,
                };
                self.push(DisplayItem::Border {
                    rect: rect.outset(&half),
                    widths: w,
                    colors: cb.colors,
                    styles: cb.styles,
                    radii: [(0.0, 0.0); 4],
                });
            }
        } else if f.border.top + f.border.right + f.border.bottom + f.border.left > 0.0 {
            let w = f.border;
            self.push(DisplayItem::Border {
                rect,
                widths: w,
                colors: s.border_color,
                styles: s.border_style,
                radii,
            });
        }
        if s.outline_width > 0.0 && s.outline_style != BorderStyle::None {
            let o = s.outline_width + s.outline_offset;
            let orect = rect.outset(&Edges::all(o));
            let ow = s.outline_width;
            self.push(DisplayItem::Border {
                rect: orect,
                widths: Edges::all(ow),
                colors: [s.outline_color; 4],
                styles: [s.outline_style; 4],
                radii: [(0.0, 0.0); 4],
            });
        }
    }

    fn paint_background_image(&mut self, f: &Fragment, rect: Rect, radii: Radii, size: (f32, f32), src: BgSource) {
        let s = &f.style;
        let area = f.padding_box().translate(rect.x, rect.y);
        let (iw, ih) = size;
        if iw <= 0.0 || ih <= 0.0 {
            return;
        }
        let (tw, th) = match &s.background_size {
            BackgroundSize::Cover => {
                let k = (area.width / iw).max(area.height / ih);
                (iw * k, ih * k)
            }
            BackgroundSize::Contain => {
                let k = (area.width / iw).min(area.height / ih);
                (iw * k, ih * k)
            }
            BackgroundSize::Explicit(w, h) => match (w.resolve(area.width), h.resolve(area.height)) {
                (Some(w), Some(h)) => (w, h),
                (Some(w), None) => (w, w * ih / iw),
                (None, Some(h)) => (h * iw / ih, h),
                (None, None) => (iw, ih),
            },
        };
        if tw <= 0.0 || th <= 0.0 {
            return;
        }
        let img = match src {
            BgSource::Raster(img) => img,
            // Vector backgrounds are rasterized at twice the tile size.
            BgSource::Svg(svg) => {
                match crate::svg::render(&svg, (tw * 2.0).ceil() as u32, (th * 2.0).ceil() as u32, s.color) {
                    Some(img) => img,
                    None => return,
                }
            }
            // Large gradient tiles are rendered at reduced resolution and scaled.
            BgSource::Gradient(g) => {
                let (pw, ph) = (tw.ceil().clamp(1.0, 1024.0) as u32, th.ceil().clamp(1.0, 1024.0) as u32);
                match crate::css::gradient::render_gradient(&g, tw, th, pw, ph) {
                    Some(img) => img,
                    None => return,
                }
            }
        };
        let px = s.background_position.0.resolve(area.width - tw);
        let py = s.background_position.1.resolve(area.height - th);
        let (rx, ry) = match s.background_repeat {
            BackgroundRepeat::Repeat => (true, true),
            BackgroundRepeat::RepeatX => (true, false),
            BackgroundRepeat::RepeatY => (false, true),
            BackgroundRepeat::NoRepeat => (false, false),
        };
        let clip_rounded = radii.iter().any(|r| r.0 > 0.0);
        if clip_rounded {
            self.push(DisplayItem::PushClip { rect, radii });
        }
        self.push(DisplayItem::TiledImage {
            area: rect,
            tile: Rect::new(area.x + px, area.y + py, tw, th),
            repeat_x: rx,
            repeat_y: ry,
            image: img,
        });
        if clip_rounded {
            self.push(DisplayItem::PopClip);
        }
    }
}

/// The source of a background image.
enum BgSource {
    Raster(Rc<Image>),
    Svg(Rc<crate::svg::SvgSource>),
    Gradient(Rc<crate::css::gradient::Gradient>),
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Phase {
    Blocks,
    Floats,
    Inline,
}
