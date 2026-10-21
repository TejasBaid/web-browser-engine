//! Box tree construction: styled DOM → layout boxes.
//!
//! Block containers hold either block-level children or a single inline formatting
//! context. Inline formatting contexts are stored as a flat item list where inline
//! boxes appear as `Start`/`End` markers; when a block-level box occurs inside an inline
//! box, the open inline boxes are closed and reopened around it (marking the split
//! edges), which implements block-in-inline splitting without a separate continuation
//! tree.

use super::Resources;
use crate::dom::{Document, Namespace, NodeData, NodeId};
use crate::image::Image;
use crate::style::StyledDocument;
use crate::style::computed::*;
use crate::text::ShapedGlyph;
use std::cell::{Cell, RefCell};
use std::rc::Rc;

/// A layout box.
#[derive(Debug)]
pub struct LayoutBox {
    /// Generating element (for pseudo-elements: the originating element).
    pub node: Option<NodeId>,
    /// Computed style.
    pub style: Rc<ComputedStyle>,
    /// What the box contains.
    pub content: BoxContent,
    /// Whether this is an anonymous box.
    pub anonymous: bool,
    /// Cached (min-content, max-content) border-box widths.
    pub(crate) intrinsic: Cell<Option<(f32, f32)>>,
    /// Cached "margins collapse through this box" flag.
    pub(crate) collapse_through: Cell<Option<bool>>,
}

impl LayoutBox {
    pub(crate) fn new(node: Option<NodeId>, style: Rc<ComputedStyle>, content: BoxContent) -> LayoutBox {
        LayoutBox {
            node,
            style,
            content,
            anonymous: false,
            intrinsic: Cell::new(None),
            collapse_through: Cell::new(None),
        }
    }

    fn anonymous(style: Rc<ComputedStyle>, content: BoxContent) -> LayoutBox {
        LayoutBox {
            anonymous: true,
            ..LayoutBox::new(None, style, content)
        }
    }

    /// Whether the box is a float.
    pub fn is_float(&self) -> bool {
        self.style.is_floated()
    }

    /// Whether the box is absolutely or fixed positioned.
    pub fn is_abspos(&self) -> bool {
        self.style.is_out_of_flow_positioned()
    }

    /// Whether the box is in normal flow (neither floated nor absolutely positioned).
    pub fn is_in_flow(&self) -> bool {
        !self.is_float() && !self.is_abspos()
    }

    /// Whether the box establishes an independent formatting context (a new block
    /// formatting context for block containers).
    pub fn establishes_bfc(&self) -> bool {
        let s = &self.style;
        s.is_floated()
            || s.is_out_of_flow_positioned()
            || s.clips_overflow()
            || matches!(
                s.display,
                Display::InlineBlock
                    | Display::FlowRoot
                    | Display::Flex
                    | Display::InlineFlex
                    | Display::Grid
                    | Display::InlineGrid
                    | Display::Table
                    | Display::InlineTable
                    | Display::TableCell
                    | Display::TableCaption
            )
            || !matches!(self.content, BoxContent::Blocks(_) | BoxContent::Inline(_))
    }
}

/// The contents of a box.
#[derive(Debug)]
pub enum BoxContent {
    /// Block-level children (including floats and absolutely positioned boxes).
    Blocks(Vec<LayoutBox>),
    /// An inline formatting context.
    Inline(InlineContent),
    /// Flex items.
    Flex(Vec<LayoutBox>),
    /// Grid items.
    Grid(Vec<LayoutBox>),
    /// Table structure.
    Table(TableContent),
    /// Replaced content (images and placeholder boxes for unsupported embedded content).
    Replaced(Replaced),
}

/// Vector content of a replaced element.
#[derive(Debug, Clone)]
pub enum SvgContent {
    /// An inline `<svg>` element of the document.
    Inline(NodeId),
    /// An SVG file loaded for `<img src>`.
    External(Rc<crate::svg::SvgSource>),
}

/// Replaced element data.
#[derive(Debug, Clone)]
pub struct Replaced {
    /// Decoded image, if any.
    pub image: Option<Rc<Image>>,
    /// Natural size in px, if known.
    pub natural: Option<(f32, f32)>,
    /// SVG content, rasterized at the used size during layout.
    pub svg: Option<SvgContent>,
}

/// An inline formatting context.
#[derive(Debug, Default)]
pub struct InlineContent {
    /// Items in order.
    pub items: Vec<InlineItem>,
    /// Inline boxes referenced by `Start`/`End` items.
    pub boxes: Vec<InlineBoxInfo>,
}

impl InlineContent {
    /// Whether the IFC would generate any non-empty line box.
    pub fn has_content(&self) -> bool {
        self.items.iter().any(|i| match i {
            InlineItem::Text(t) => !t.text.is_empty(),
            InlineItem::Atomic(_) | InlineItem::Break(_) | InlineItem::Marker(_) => true,
            InlineItem::Start(b) | InlineItem::End(b) => {
                let s = &self.boxes[*b].style;
                s.border_width[1] > 0.0 || s.border_width[3] > 0.0 || !s.padding[1].is_zero() || !s.padding[3].is_zero()
            }
            _ => false,
        })
    }
}

/// An inline box (e.g. `<span>`) in an inline formatting context.
#[derive(Debug, Clone)]
pub struct InlineBoxInfo {
    /// Element.
    pub node: Option<NodeId>,
    /// Style.
    pub style: Rc<ComputedStyle>,
    /// Whether this piece has the box's inline-start edge (false after a block split).
    pub start_edge: bool,
    /// Whether this piece has the box's inline-end edge.
    pub end_edge: bool,
}

/// A text item. Whitespace has already been processed per `white-space`.
#[derive(Debug)]
pub struct TextItem {
    /// Text (no newlines; forced breaks are separate items).
    pub text: String,
    /// Style of the text (its parent element's style).
    pub style: Rc<ComputedStyle>,
    /// The text node.
    pub node: Option<NodeId>,
    /// Shaping cache.
    pub(crate) shaped: RefCell<Option<Rc<Vec<ShapedGlyph>>>>,
}

/// A list marker item.
#[derive(Debug)]
pub struct MarkerItem {
    /// Marker text, e.g. "• " or "3. ".
    pub text: String,
    /// Style (the list item's).
    pub style: Rc<ComputedStyle>,
    /// `list-style-position: outside`.
    pub outside: bool,
}

/// One item of an inline formatting context.
#[derive(Debug)]
pub enum InlineItem {
    /// Text.
    Text(TextItem),
    /// Start of inline box `boxes[i]`.
    Start(usize),
    /// End of inline box `boxes[i]`.
    End(usize),
    /// An atomic inline (inline-block, inline-flex, inline-table, replaced element).
    Atomic(LayoutBox),
    /// A forced line break (`<br>` or a preserved newline), with the style it came from.
    Break(Rc<ComputedStyle>),
    /// A float.
    Float(LayoutBox),
    /// An absolutely positioned box (placeholder at its static position).
    Abs(LayoutBox),
    /// A list marker.
    Marker(MarkerItem),
}

/// Table structure.
#[derive(Debug, Default)]
pub struct TableContent {
    /// Captions (laid out above the table grid).
    pub captions: Vec<LayoutBox>,
    /// Column widths from `<col>`/`<colgroup>` (`Size::Auto` when unspecified).
    pub columns: Vec<Size>,
    /// Row groups in display order (header, bodies, footer).
    pub groups: Vec<RowGroup>,
}

/// A row group.
#[derive(Debug)]
pub struct RowGroup {
    /// Element (None for anonymous groups).
    pub node: Option<NodeId>,
    /// Style.
    pub style: Rc<ComputedStyle>,
    /// Rows.
    pub rows: Vec<Row>,
}

/// A table row.
#[derive(Debug)]
pub struct Row {
    /// Element.
    pub node: Option<NodeId>,
    /// Style.
    pub style: Rc<ComputedStyle>,
    /// Cells.
    pub cells: Vec<Cell_>,
}

/// A table cell.
#[derive(Debug)]
pub struct Cell_ {
    /// The cell box (a block container).
    pub b: LayoutBox,
    /// Columns spanned.
    pub colspan: usize,
    /// Rows spanned.
    pub rowspan: usize,
}

/// Builds the box tree for the document's root element.
pub fn build_box_tree(doc: &Document, styles: &StyledDocument, resources: &Resources) -> Option<LayoutBox> {
    let root = doc.document_element()?;
    let style = styles.style(root)?.clone();
    let b = Builder {
        doc,
        styles,
        resources,
        counters: RefCell::new(Vec::new()),
    };
    Some(b.element_box(root, style))
}

#[derive(Clone)]
enum Child {
    Element(NodeId, Rc<ComputedStyle>),
    Text(NodeId, Rc<ComputedStyle>),
    /// Generated content: (originating element, pseudo-element style). The content is
    /// resolved when the box is built, after counters have been updated.
    Generated(NodeId, Rc<ComputedStyle>),
}

struct Builder<'a> {
    doc: &'a Document,
    styles: &'a StyledDocument,
    resources: &'a Resources,
    /// Active CSS counter instances, innermost last.
    counters: RefCell<Vec<(String, i64)>>,
}

impl Builder<'_> {
    /// Children of `el` as box-generating items, flattening `display: contents` and
    /// adding `::before`/`::after`.
    fn children(&self, el: NodeId) -> Vec<Child> {
        let mut out = Vec::new();
        if let Some(s) = self.styles.before(el)
            && let Some(c) = self.generated(el, s)
        {
            out.push(c);
        }
        for c in self.doc.children(el) {
            match &self.doc.node(c).data {
                NodeData::Element(_) => {
                    let Some(s) = self.styles.style(c) else { continue };
                    if s.display == Display::Contents {
                        out.extend(self.children(c));
                    } else {
                        out.push(Child::Element(c, s.clone()));
                    }
                }
                NodeData::Text(_) => {
                    if let Some(s) = self.styles.style(c) {
                        out.push(Child::Text(c, s.clone()));
                    }
                }
                _ => {}
            }
        }
        if let Some(s) = self.styles.after(el)
            && let Some(c) = self.generated(el, s)
        {
            out.push(c);
        }
        out
    }

    fn generated(&self, el: NodeId, style: &Rc<ComputedStyle>) -> Option<Child> {
        style.content.as_ref()?;
        Some(Child::Generated(el, style.clone()))
    }

    /// Resolves a pseudo-element's `content` (after applying its counter operations).
    fn resolve_generated(&self, el: NodeId, style: &ComputedStyle) -> (String, Option<Rc<Image>>) {
        self.counters_enter(style);
        let mut text = String::new();
        let mut image = None;
        for item in style.content.iter().flat_map(|c| c.iter()) {
            match item {
                ContentItem::String(s) => text.push_str(s),
                ContentItem::Attr(a) => text.push_str(self.doc.attr(el, a).unwrap_or("")),
                ContentItem::OpenQuote => text.push('\u{201C}'),
                ContentItem::CloseQuote => text.push('\u{201D}'),
                ContentItem::Url(u) => image = self.resources.image(u),
                ContentItem::Counter(name, ty) => {
                    let v = self.counter_value(name);
                    text.push_str(&format_counter(v, ty));
                }
                ContentItem::Counters(name, sep, ty) => {
                    let values: Vec<String> = self
                        .counters
                        .borrow()
                        .iter()
                        .filter(|(n, _)| n == name)
                        .map(|(_, v)| format_counter(*v, ty))
                        .collect();
                    let joined = if values.is_empty() {
                        format_counter(0, ty)
                    } else {
                        values.join(sep)
                    };
                    text.push_str(&joined);
                }
            }
        }
        (text, image)
    }

    /// Applies `counter-reset`, `counter-set` and `counter-increment` (in that order).
    fn counters_enter(&self, style: &ComputedStyle) {
        let Some(ops) = &style.counter_ops else { return };
        let mut c = self.counters.borrow_mut();
        for (name, v) in &ops.reset {
            c.push((name.clone(), *v as i64));
        }
        for (name, v) in &ops.set {
            match c.iter_mut().rev().find(|(n, _)| n == name) {
                Some(e) => e.1 = *v as i64,
                None => c.push((name.clone(), *v as i64)),
            }
        }
        for (name, v) in &ops.increment {
            match c.iter_mut().rev().find(|(n, _)| n == name) {
                Some(e) => e.1 += *v as i64,
                None => c.push((name.clone(), *v as i64)),
            }
        }
    }

    fn counter_value(&self, name: &str) -> i64 {
        self.counters
            .borrow()
            .iter()
            .rev()
            .find(|(n, _)| n == name)
            .map(|(_, v)| *v)
            .unwrap_or(0)
    }

    /// Counter scopes created inside an element's subtree end with the element.
    fn counters_mark(&self) -> usize {
        self.counters.borrow().len()
    }

    fn counters_restore(&self, mark: usize) {
        self.counters.borrow_mut().truncate(mark);
    }

    /// Builds the box for an element that is block-level or an atomic inline.
    fn element_box(&self, el: NodeId, style: Rc<ComputedStyle>) -> LayoutBox {
        if let Some(r) = self.replaced(el) {
            return LayoutBox::new(Some(el), style, BoxContent::Replaced(r));
        }
        if let Some(form) = self.form_control(el, &style) {
            return form;
        }
        self.counters_enter(&style);
        let mark = self.counters_mark();
        let content = match style.display {
            Display::Flex | Display::InlineFlex => BoxContent::Flex(self.flex_items(el, &style)),
            Display::Grid | Display::InlineGrid => BoxContent::Grid(self.flex_items(el, &style)),
            Display::Table | Display::InlineTable => BoxContent::Table(self.table(el, &style)),
            _ => self.block_container(&style, self.children(el)),
        };
        self.counters_restore(mark);
        let mut b = LayoutBox::new(Some(el), style.clone(), content);
        if style.display == Display::ListItem {
            self.add_marker(el, &mut b);
        }
        b
    }

    fn generated_box(&self, el: NodeId, style: Rc<ComputedStyle>, text: String, image: Option<Rc<Image>>) -> LayoutBox {
        if let Some(img) = image {
            let natural = Some((img.width as f32, img.height as f32));
            return LayoutBox::new(
                Some(el),
                style,
                BoxContent::Replaced(Replaced {
                    image: Some(img),
                    natural,
                    svg: None,
                }),
            );
        }
        let mut ifc = InlineContent::default();
        push_text(&mut ifc, &text, &style, None);
        process_whitespace(&mut ifc);
        LayoutBox::new(Some(el), style, BoxContent::Inline(ifc))
    }

    fn replaced(&self, el: NodeId) -> Option<Replaced> {
        let e = self.doc.element(el)?;
        let attr_px = |n: &str| {
            e.attr(n)
                .and_then(|v| v.trim().trim_end_matches("px").parse::<f32>().ok())
        };
        match (e.ns, e.name.as_str()) {
            (Namespace::Html, "img") => {
                let image = e.attr("src").and_then(|s| self.resources.image(s));
                if image.is_none()
                    && let Some(svg) = e.attr("src").and_then(|s| self.resources.svg(s))
                {
                    let natural = Some(crate::svg::intrinsic_size(&svg.doc, svg.root));
                    return Some(Replaced {
                        image: None,
                        natural,
                        svg: Some(SvgContent::External(svg)),
                    });
                }
                let natural = image.as_ref().map(|i| (i.width as f32, i.height as f32));
                Some(Replaced {
                    image,
                    natural,
                    svg: None,
                })
            }
            (Namespace::Svg, "svg") => Some(Replaced {
                image: None,
                natural: Some(crate::svg::intrinsic_size(self.doc, el)),
                svg: Some(SvgContent::Inline(el)),
            }),
            (Namespace::Html, "video") if e.attr("poster").is_some() => {
                let image = e.attr("poster").and_then(|s| self.resources.image(s));
                let natural = Some((attr_px("width").unwrap_or(300.0), attr_px("height").unwrap_or(150.0)));
                Some(Replaced {
                    image,
                    natural,
                    svg: None,
                })
            }
            (Namespace::Html, "canvas" | "video" | "iframe" | "embed" | "object" | "audio") => Some(Replaced {
                image: None,
                natural: Some((attr_px("width").unwrap_or(300.0), attr_px("height").unwrap_or(150.0))),
                svg: None,
            }),
            (Namespace::Html, "input")
                if matches!(
                    e.attr("type").map(|t| t.to_ascii_lowercase()).as_deref(),
                    Some("checkbox") | Some("radio")
                ) =>
            {
                Some(Replaced {
                    image: None,
                    natural: Some((13.0, 13.0)),
                    svg: None,
                })
            }
            (Namespace::Html, "input") if e.attr("type").is_some_and(|t| t.eq_ignore_ascii_case("image")) => {
                let image = e.attr("src").and_then(|s| self.resources.image(s));
                let natural = image.as_ref().map(|i| (i.width as f32, i.height as f32));
                Some(Replaced {
                    image,
                    natural,
                    svg: None,
                })
            }
            _ => None,
        }
    }

    /// Simplified rendering of text inputs, buttons, selects and textareas: a block
    /// container holding the control's current text.
    fn form_control(&self, el: NodeId, style: &Rc<ComputedStyle>) -> Option<LayoutBox> {
        let e = self.doc.element(el)?;
        if e.ns != Namespace::Html {
            return None;
        }
        let (text, placeholder) = match e.name.as_str() {
            "input" => {
                let ty = e.attr("type").unwrap_or("text").to_ascii_lowercase();
                match ty.as_str() {
                    "submit" => (e.attr("value").unwrap_or("Submit").to_string(), false),
                    "reset" => (e.attr("value").unwrap_or("Reset").to_string(), false),
                    "button" => (e.attr("value").unwrap_or("").to_string(), false),
                    "password" => ("•".repeat(e.attr("value").unwrap_or("").chars().count()), false),
                    _ => match e.attr("value").filter(|v| !v.is_empty()) {
                        Some(v) => (v.to_string(), false),
                        None => (e.attr("placeholder").unwrap_or("").to_string(), true),
                    },
                }
            }
            "textarea" => {
                let t = self.doc.text_content(el);
                if t.is_empty() {
                    (e.attr("placeholder").unwrap_or("").to_string(), true)
                } else {
                    (t, false)
                }
            }
            "select" => {
                let options: Vec<NodeId> = self
                    .doc
                    .descendants(el)
                    .filter(|&n| self.doc.is_html_element(n, "option"))
                    .collect();
                let chosen = options
                    .iter()
                    .copied()
                    .find(|&o| self.doc.attr(o, "selected").is_some())
                    .or_else(|| options.first().copied());
                let t = chosen
                    .map(|o| {
                        self.doc
                            .text_content(o)
                            .split_whitespace()
                            .collect::<Vec<_>>()
                            .join(" ")
                    })
                    .unwrap_or_default();
                (format!("{t} \u{25BE}"), false)
            }
            _ => return None,
        };
        let mut text_style = (**style).clone();
        text_style.display = Display::Inline;
        if placeholder {
            text_style.color = crate::css::color::Color::rgb(0x75, 0x75, 0x75);
        }
        if e.name != "textarea" {
            text_style.white_space = WhiteSpace::Pre;
        }
        let mut ifc = InlineContent::default();
        push_text(&mut ifc, &text, &Rc::new(text_style), None);
        process_whitespace(&mut ifc);
        Some(LayoutBox::new(Some(el), style.clone(), BoxContent::Inline(ifc)))
    }

    fn block_container(&self, style: &Rc<ComputedStyle>, children: Vec<Child>) -> BoxContent {
        let mut cb = ContainerBuilder::new(style.clone());
        for c in children {
            self.add_child(&mut cb, c);
        }
        self.flush_orphans(&mut cb);
        cb.finish()
    }

    fn add_child(&self, cb: &mut ContainerBuilder, child: Child) {
        match child {
            Child::Text(n, style) => {
                if let Some(t) = self.doc.text(n) {
                    if !cb.orphan_table_parts.is_empty() {
                        if t.chars().all(|c| c.is_ascii_whitespace()) {
                            return;
                        }
                        self.flush_orphans(cb);
                    }
                    push_text(&mut cb.ifc, t, &style, Some(n));
                }
            }
            Child::Generated(el, style) => {
                self.flush_orphans(cb);
                let (text, image) = self.resolve_generated(el, &style);
                if style.display.is_inline_level() && style.display == Display::Inline && image.is_none() {
                    let idx = cb.open_inline(Some(el), style.clone());
                    push_text(&mut cb.ifc, &text, &style, None);
                    cb.close_inline(idx);
                } else {
                    let b = self.generated_box(el, style.clone(), text, image);
                    self.add_box(cb, b);
                }
            }
            Child::Element(el, style) => {
                let e = self.doc.element(el).unwrap();
                if e.is_html("br") {
                    cb.ifc.items.push(InlineItem::Break(style));
                    return;
                }
                if e.is_html("wbr") {
                    push_text(&mut cb.ifc, "\u{200B}", &style, None);
                    return;
                }
                if e.is_html("img")
                    && style.display == Display::Inline
                    && !style.is_floated()
                    && !style.is_out_of_flow_positioned()
                {
                    // A failed image with alt text and no explicit size renders its alt text.
                    let image = e.attr("src").and_then(|s| self.resources.image(s));
                    let svg = e.attr("src").and_then(|s| self.resources.svg(s));
                    if image.is_none()
                        && svg.is_none()
                        && style.width.is_auto()
                        && style.height.is_auto()
                        && let Some(alt) = e.attr("alt").filter(|a| !a.trim().is_empty())
                    {
                        let idx = cb.open_inline(Some(el), style.clone());
                        push_text(&mut cb.ifc, alt, &style, None);
                        cb.close_inline(idx);
                        return;
                    }
                }
                let is_plain_inline = style.display == Display::Inline
                    && !style.is_floated()
                    && !style.is_out_of_flow_positioned()
                    && self.replaced(el).is_none()
                    && !matches!(e.name.as_str(), "input" | "select" | "textarea");
                if is_plain_inline {
                    self.flush_orphans(cb);
                    self.counters_enter(&style);
                    let mark = self.counters_mark();
                    let idx = cb.open_inline(Some(el), style.clone());
                    for c in self.children(el) {
                        self.add_child(cb, c);
                    }
                    cb.close_inline(idx);
                    self.counters_restore(mark);
                } else if matches!(
                    style.display,
                    Display::TableRow
                        | Display::TableCell
                        | Display::TableRowGroup
                        | Display::TableHeaderGroup
                        | Display::TableFooterGroup
                        | Display::TableCaption
                        | Display::TableColumn
                        | Display::TableColumnGroup
                ) && !style.is_floated()
                    && !style.is_out_of_flow_positioned()
                {
                    cb.orphan_table_parts.push((el, style));
                } else {
                    let b = self.element_box(el, style);
                    self.add_box(cb, b);
                }
            }
        }
    }

    /// Adds a non-inline box (block-level, atomic inline, float or abspos).
    fn add_box(&self, cb: &mut ContainerBuilder, b: LayoutBox) {
        self.flush_orphans(cb);
        if b.is_abspos() {
            if cb.ifc.items.is_empty() && cb.open.is_empty() && !b.style.display.is_inline_level() {
                cb.push_block(b);
            } else {
                cb.ifc.items.push(InlineItem::Abs(b));
            }
        } else if b.is_float() {
            if cb.ifc_is_blank() && cb.open.is_empty() {
                cb.push_block(b);
            } else {
                cb.ifc.items.push(InlineItem::Float(b));
            }
        } else if b.style.display.is_inline_level() {
            cb.ifc.items.push(InlineItem::Atomic(b));
        } else {
            cb.push_block(b);
        }
    }

    fn flush_orphans(&self, cb: &mut ContainerBuilder) {
        if cb.orphan_table_parts.is_empty() {
            return;
        }
        let parts = std::mem::take(&mut cb.orphan_table_parts);
        let table_style = Rc::new(ComputedStyle::anonymous(&cb.style, Display::Table));
        let mut t = TableContent::default();
        self.table_children(&mut t, parts, &table_style);
        let b = LayoutBox::anonymous(table_style, BoxContent::Table(t));
        cb.push_block(b);
    }

    fn flex_items(&self, el: NodeId, style: &Rc<ComputedStyle>) -> Vec<LayoutBox> {
        let mut items = Vec::new();
        let mut pending_text: Vec<Child> = Vec::new();
        let flush_text = |pending: &mut Vec<Child>, items: &mut Vec<LayoutBox>| {
            if pending.is_empty() {
                return;
            }
            let anon = Rc::new(ComputedStyle::anonymous(style, Display::Block));
            let content = self.block_container(&anon, std::mem::take(pending));
            let has = match &content {
                BoxContent::Inline(i) => i.has_content() || i.items.iter().any(|x| matches!(x, InlineItem::Abs(_))),
                BoxContent::Blocks(b) => !b.is_empty(),
                _ => true,
            };
            if has {
                items.push(LayoutBox::anonymous(anon, content));
            }
        };
        for c in self.children(el) {
            match c {
                Child::Text(..) => pending_text.push(c),
                Child::Element(n, s) => {
                    flush_text(&mut pending_text, &mut items);
                    if self.doc.is_html_element(n, "br") {
                        continue;
                    }
                    items.push(self.element_box(n, s));
                }
                Child::Generated(n, s) => {
                    flush_text(&mut pending_text, &mut items);
                    let (text, image) = self.resolve_generated(n, &s);
                    let mut s2 = (*s).clone();
                    s2.display = s2.display.blockify();
                    items.push(self.generated_box(n, Rc::new(s2), text, image));
                }
            }
        }
        flush_text(&mut pending_text, &mut items);
        items
    }

    fn table(&self, el: NodeId, style: &Rc<ComputedStyle>) -> TableContent {
        let mut t = TableContent::default();
        let parts: Vec<(NodeId, Rc<ComputedStyle>)> = self
            .children(el)
            .into_iter()
            .filter_map(|c| match c {
                Child::Element(n, s) => Some((n, s)),
                _ => None,
            })
            .collect();
        self.table_children(&mut t, parts, style);
        t
    }

    fn table_children(
        &self,
        t: &mut TableContent,
        parts: Vec<(NodeId, Rc<ComputedStyle>)>,
        table_style: &Rc<ComputedStyle>,
    ) {
        let mut head: Vec<RowGroup> = Vec::new();
        let mut body: Vec<RowGroup> = Vec::new();
        let mut foot: Vec<RowGroup> = Vec::new();
        let mut loose_rows: Vec<Row> = Vec::new();
        let mut loose_cells: Vec<Cell_> = Vec::new();
        let anon_group = || Rc::new(ComputedStyle::anonymous(table_style, Display::TableRowGroup));
        let anon_row = || Rc::new(ComputedStyle::anonymous(table_style, Display::TableRow));
        let flush_cells = |cells: &mut Vec<Cell_>, rows: &mut Vec<Row>| {
            if !cells.is_empty() {
                rows.push(Row {
                    node: None,
                    style: anon_row(),
                    cells: std::mem::take(cells),
                });
            }
        };
        let flush_rows = |rows: &mut Vec<Row>, body: &mut Vec<RowGroup>| {
            if !rows.is_empty() {
                body.push(RowGroup {
                    node: None,
                    style: anon_group(),
                    rows: std::mem::take(rows),
                });
            }
        };
        for (n, s) in parts {
            match s.display {
                Display::TableCaption => t.captions.push(self.element_box(n, s)),
                Display::TableColumnGroup => {
                    let cols: Vec<NodeId> = self
                        .doc
                        .element_children(n)
                        .filter(|&c| {
                            self.styles
                                .style(c)
                                .is_some_and(|cs| cs.display == Display::TableColumn)
                        })
                        .collect();
                    if cols.is_empty() {
                        let span = span_attr(self.doc, n);
                        for _ in 0..span {
                            t.columns.push(s.width.clone());
                        }
                    }
                    for c in cols {
                        let cs = self.styles.style(c).unwrap();
                        for _ in 0..span_attr(self.doc, c) {
                            t.columns.push(cs.width.clone());
                        }
                    }
                }
                Display::TableColumn => {
                    for _ in 0..span_attr(self.doc, n) {
                        t.columns.push(s.width.clone());
                    }
                }
                Display::TableHeaderGroup | Display::TableRowGroup | Display::TableFooterGroup => {
                    flush_cells(&mut loose_cells, &mut loose_rows);
                    flush_rows(&mut loose_rows, &mut body);
                    let g = RowGroup {
                        node: Some(n),
                        style: s.clone(),
                        rows: self.rows_of(n, &s),
                    };
                    match s.display {
                        Display::TableHeaderGroup if head.is_empty() => head.push(g),
                        Display::TableFooterGroup if foot.is_empty() => foot.push(g),
                        _ => body.push(g),
                    }
                }
                Display::TableRow => {
                    flush_cells(&mut loose_cells, &mut loose_rows);
                    loose_rows.push(self.row(n, &s));
                }
                Display::TableCell => loose_cells.push(self.cell(n, s)),
                _ => {
                    // Non-table content inside a table: wrap it in an anonymous cell.
                    let cell_style = Rc::new(ComputedStyle::anonymous(table_style, Display::TableCell));
                    let content = self.block_container(&cell_style, vec![Child::Element(n, s)]);
                    loose_cells.push(Cell_ {
                        b: LayoutBox::anonymous(cell_style, content),
                        colspan: 1,
                        rowspan: 1,
                    });
                }
            }
        }
        flush_cells(&mut loose_cells, &mut loose_rows);
        flush_rows(&mut loose_rows, &mut body);
        t.groups.extend(head);
        t.groups.extend(body);
        t.groups.extend(foot);
    }

    fn rows_of(&self, group: NodeId, group_style: &Rc<ComputedStyle>) -> Vec<Row> {
        let mut rows = Vec::new();
        let mut loose: Vec<Cell_> = Vec::new();
        for c in self.children(group) {
            if let Child::Element(n, s) = c {
                match s.display {
                    Display::TableRow => {
                        if !loose.is_empty() {
                            rows.push(Row {
                                node: None,
                                style: Rc::new(ComputedStyle::anonymous(group_style, Display::TableRow)),
                                cells: std::mem::take(&mut loose),
                            });
                        }
                        rows.push(self.row(n, &s));
                    }
                    Display::TableCell => loose.push(self.cell(n, s)),
                    _ => {}
                }
            }
        }
        if !loose.is_empty() {
            rows.push(Row {
                node: None,
                style: Rc::new(ComputedStyle::anonymous(group_style, Display::TableRow)),
                cells: loose,
            });
        }
        rows
    }

    fn row(&self, n: NodeId, s: &Rc<ComputedStyle>) -> Row {
        let cells = self
            .children(n)
            .into_iter()
            .filter_map(|c| match c {
                Child::Element(cn, cs) if cs.display == Display::TableCell => Some(self.cell(cn, cs)),
                _ => None,
            })
            .collect();
        Row {
            node: Some(n),
            style: s.clone(),
            cells,
        }
    }

    fn cell(&self, n: NodeId, s: Rc<ComputedStyle>) -> Cell_ {
        let num = |a: &str| {
            self.doc
                .attr(n, a)
                .and_then(|v| v.trim().parse::<usize>().ok())
                .unwrap_or(1)
                .clamp(1, 1000)
        };
        let (colspan, rowspan) = if self.doc.is_html_element(n, "td") || self.doc.is_html_element(n, "th") {
            (num("colspan"), num("rowspan"))
        } else {
            (1, 1)
        };
        Cell_ {
            b: self.element_box(n, s),
            colspan,
            rowspan,
        }
    }

    /// Inserts the list marker into the first inline formatting context of a list item.
    fn add_marker(&self, el: NodeId, b: &mut LayoutBox) {
        let style = b.style.clone();
        // `list-style-image` is not rendered; the marker falls back to the list style type,
        // as it would for an image that fails to load.
        let text = marker_text(&style.list_style_type, list_ordinal(self.doc, el));
        let Some(text) = text else { return };
        let outside = style.list_style_position == ListStylePosition::Outside;
        let marker = InlineItem::Marker(MarkerItem {
            text,
            style: style.clone(),
            outside,
        });
        fn insert(content: &mut BoxContent, marker: InlineItem, style: &Rc<ComputedStyle>) -> Result<(), InlineItem> {
            match content {
                BoxContent::Inline(ifc) => {
                    ifc.items.insert(0, marker);
                    Ok(())
                }
                BoxContent::Blocks(children) => {
                    let first = children.iter_mut().find(|c| c.is_in_flow());
                    match first {
                        Some(c)
                            if matches!(c.content, BoxContent::Inline(_) | BoxContent::Blocks(_))
                                && !c.establishes_bfc() =>
                        {
                            insert(&mut c.content, marker, style)
                        }
                        _ => {
                            let mut ifc = InlineContent::default();
                            ifc.items.push(marker);
                            let anon = Rc::new(ComputedStyle::anonymous(style, Display::Block));
                            children.insert(0, LayoutBox::anonymous(anon, BoxContent::Inline(ifc)));
                            Ok(())
                        }
                    }
                }
                _ => Err(marker),
            }
        }
        // Replaced, flex, grid and table list items get no marker.
        let _unplaced = insert(&mut b.content, marker, &style);
    }
}

fn span_attr(doc: &Document, n: NodeId) -> usize {
    doc.attr(n, "span")
        .and_then(|v| v.trim().parse::<usize>().ok())
        .unwrap_or(1)
        .clamp(1, 1000)
}

/// The ordinal of a list item within its list (honouring `start`, `reversed` and
/// `value`).
pub fn list_ordinal(doc: &Document, el: NodeId) -> i64 {
    let Some(parent) = doc.parent(el) else { return 1 };
    let reversed = doc.is_html_element(parent, "ol") && doc.attr(parent, "reversed").is_some();
    let items: Vec<NodeId> = doc
        .element_children(parent)
        .filter(|&c| doc.is_html_element(c, "li"))
        .collect();
    let start = doc
        .attr(parent, "start")
        .and_then(|s| s.trim().parse::<i64>().ok())
        .unwrap_or(if reversed { items.len() as i64 } else { 1 });
    let mut n = if reversed { start + 1 } else { start - 1 };
    for &li in &items {
        match doc.attr(li, "value").and_then(|v| v.trim().parse::<i64>().ok()) {
            Some(v) => n = v,
            None => n += if reversed { -1 } else { 1 },
        }
        if li == el {
            return n;
        }
    }
    n
}

/// Formats a list marker.
pub fn marker_text(ty: &ListStyleType, n: i64) -> Option<String> {
    Some(match ty {
        ListStyleType::None => return None,
        ListStyleType::Disc => "\u{2022} ".to_string(),
        ListStyleType::Circle => "\u{25E6} ".to_string(),
        ListStyleType::Square => "\u{25AA} ".to_string(),
        ListStyleType::Decimal => format!("{n}. "),
        ListStyleType::DecimalLeadingZero => format!("{n:02}. "),
        ListStyleType::LowerAlpha => format!("{}. ", alpha(n, b'a')),
        ListStyleType::UpperAlpha => format!("{}. ", alpha(n, b'A')),
        ListStyleType::LowerRoman => format!("{}. ", roman(n).to_lowercase()),
        ListStyleType::UpperRoman => format!("{}. ", roman(n)),
        ListStyleType::LowerGreek => {
            let letters = "αβγδεζηθικλμνξοπρστυφχψω";
            let v: Vec<char> = letters.chars().collect();
            if n >= 1 {
                format!("{}. ", v[((n - 1) as usize) % v.len()])
            } else {
                format!("{n}. ")
            }
        }
        ListStyleType::String(s) => s.clone(),
    })
}

/// Formats a counter value in a list style (without the marker suffix).
pub fn format_counter(n: i64, ty: &ListStyleType) -> String {
    match ty {
        ListStyleType::None => String::new(),
        ListStyleType::Disc => "\u{2022}".to_string(),
        ListStyleType::Circle => "\u{25E6}".to_string(),
        ListStyleType::Square => "\u{25AA}".to_string(),
        ListStyleType::String(s) => s.clone(),
        other => marker_text(other, n)
            .map(|m| m.trim_end_matches(' ').trim_end_matches('.').to_string())
            .unwrap_or_default(),
    }
}

fn alpha(mut n: i64, base: u8) -> String {
    if n <= 0 {
        return n.to_string();
    }
    let mut s = Vec::new();
    while n > 0 {
        n -= 1;
        s.push((base + (n % 26) as u8) as char);
        n /= 26;
    }
    s.iter().rev().collect()
}

fn roman(n: i64) -> String {
    if !(1..=3999).contains(&n) {
        return n.to_string();
    }
    let table = [
        (1000, "M"),
        (900, "CM"),
        (500, "D"),
        (400, "CD"),
        (100, "C"),
        (90, "XC"),
        (50, "L"),
        (40, "XL"),
        (10, "X"),
        (9, "IX"),
        (5, "V"),
        (4, "IV"),
        (1, "I"),
    ];
    let mut n = n;
    let mut s = String::new();
    for (v, r) in table {
        while n >= v {
            s.push_str(r);
            n -= v;
        }
    }
    s
}

fn apply_text_transform(text: &str, tt: TextTransform) -> String {
    match tt {
        TextTransform::None => text.to_string(),
        TextTransform::Uppercase => text.to_uppercase(),
        TextTransform::Lowercase => text.to_lowercase(),
        TextTransform::Capitalize => {
            let mut out = String::with_capacity(text.len());
            let mut at_start = true;
            for c in text.chars() {
                if at_start && c.is_alphanumeric() {
                    out.extend(c.to_uppercase());
                    at_start = false;
                } else {
                    if c.is_whitespace() {
                        at_start = true;
                    }
                    out.push(c);
                }
            }
            out
        }
    }
}

/// Appends a raw text item (whitespace processing happens when the IFC is finished).
fn push_text(ifc: &mut InlineContent, text: &str, style: &Rc<ComputedStyle>, node: Option<NodeId>) {
    if text.is_empty() {
        return;
    }
    let text = apply_text_transform(text, style.text_transform);
    let preserves_newlines = matches!(
        style.white_space,
        WhiteSpace::Pre | WhiteSpace::PreWrap | WhiteSpace::PreLine | WhiteSpace::BreakSpaces
    );
    if preserves_newlines && text.contains('\n') {
        for (i, line) in text.split('\n').enumerate() {
            if i > 0 {
                ifc.items.push(InlineItem::Break(style.clone()));
            }
            if !line.is_empty() {
                ifc.items.push(InlineItem::Text(TextItem {
                    text: line.to_string(),
                    style: style.clone(),
                    node,
                    shaped: RefCell::new(None),
                }));
            }
        }
    } else {
        ifc.items.push(InlineItem::Text(TextItem {
            text,
            style: style.clone(),
            node,
            shaped: RefCell::new(None),
        }));
    }
}

/// Applies `white-space` processing across the whole IFC: collapses spaces (also across
/// element boundaries), converts newlines/tabs, and removes leading spaces.
pub fn process_whitespace(ifc: &mut InlineContent) {
    let mut prev_space = true; // leading collapsible spaces at the start are removed
    for item in ifc.items.iter_mut() {
        match item {
            InlineItem::Text(t) => {
                let ws = t.style.white_space;
                match ws {
                    WhiteSpace::Normal | WhiteSpace::Nowrap | WhiteSpace::PreLine => {
                        let mut out = String::with_capacity(t.text.len());
                        for c in t.text.chars() {
                            if matches!(c, ' ' | '\t' | '\n' | '\r' | '\x0C') {
                                if !prev_space {
                                    out.push(' ');
                                    prev_space = true;
                                }
                            } else {
                                out.push(c);
                                prev_space = false;
                            }
                        }
                        t.text = out;
                    }
                    WhiteSpace::Pre | WhiteSpace::PreWrap | WhiteSpace::BreakSpaces => {
                        if t.text.contains('\t') {
                            let mut out = String::new();
                            let mut col = 0;
                            for c in t.text.chars() {
                                if c == '\t' {
                                    let n = 8 - col % 8;
                                    out.extend(std::iter::repeat_n(' ', n));
                                    col += n;
                                } else {
                                    out.push(c);
                                    col += 1;
                                }
                            }
                            t.text = out;
                        }
                        prev_space = false;
                    }
                }
            }
            InlineItem::Atomic(_) => prev_space = false,
            InlineItem::Marker(m) if !m.outside => prev_space = false,
            InlineItem::Break(_) => prev_space = true,
            _ => {}
        }
    }
    ifc.items
        .retain(|i| !matches!(i, InlineItem::Text(t) if t.text.is_empty()));
}

struct ContainerBuilder {
    style: Rc<ComputedStyle>,
    blocks: Vec<LayoutBox>,
    ifc: InlineContent,
    /// Indices into `ifc.boxes` of currently open inline boxes (outermost first).
    open: Vec<usize>,
    orphan_table_parts: Vec<(NodeId, Rc<ComputedStyle>)>,
}

impl ContainerBuilder {
    fn new(style: Rc<ComputedStyle>) -> Self {
        ContainerBuilder {
            style,
            blocks: Vec::new(),
            ifc: InlineContent::default(),
            open: Vec::new(),
            orphan_table_parts: Vec::new(),
        }
    }

    fn open_inline(&mut self, node: Option<NodeId>, style: Rc<ComputedStyle>) -> usize {
        let idx = self.ifc.boxes.len();
        self.ifc.boxes.push(InlineBoxInfo {
            node,
            style,
            start_edge: true,
            end_edge: true,
        });
        self.ifc.items.push(InlineItem::Start(idx));
        self.open.push(idx);
        self.open.len() - 1
    }

    fn close_inline(&mut self, depth: usize) {
        // `depth` identifies the open-stack slot; after splits the box index changes.
        let idx = self.open[depth];
        self.ifc.items.push(InlineItem::End(idx));
        self.open.truncate(depth);
    }

    /// Whether the current IFC holds only collapsible whitespace (so far).
    fn ifc_is_blank(&self) -> bool {
        self.ifc.items.iter().all(|i| match i {
            InlineItem::Text(t) => {
                t.text.chars().all(|c| matches!(c, ' ' | '\t' | '\n' | '\r' | '\x0C'))
                    && matches!(
                        t.style.white_space,
                        WhiteSpace::Normal | WhiteSpace::Nowrap | WhiteSpace::PreLine
                    )
            }
            InlineItem::Float(_) | InlineItem::Abs(_) => true,
            _ => false,
        })
    }

    fn push_block(&mut self, b: LayoutBox) {
        if self.open.is_empty() {
            self.flush_ifc();
            self.blocks.push(b);
            return;
        }
        // Block-in-inline: close the open inline boxes (without end edges), emit the
        // block, then reopen them (without start edges).
        let open = self.open.clone();
        for &idx in open.iter().rev() {
            self.ifc.boxes[idx].end_edge = false;
            self.ifc.items.push(InlineItem::End(idx));
        }
        let templates: Vec<InlineBoxInfo> = open.iter().map(|&i| self.ifc.boxes[i].clone()).collect();
        self.flush_ifc();
        self.blocks.push(b);
        let mut new_open = Vec::with_capacity(open.len());
        for mut info in templates {
            info.start_edge = false;
            info.end_edge = true;
            let idx = self.ifc.boxes.len();
            self.ifc.boxes.push(info);
            self.ifc.items.push(InlineItem::Start(idx));
            new_open.push(idx);
        }
        self.open = new_open;
    }

    fn flush_ifc(&mut self) {
        if self.ifc.items.is_empty() {
            return;
        }
        // The next IFC starts with an empty box table; `push_block` re-creates the
        // entries for inline boxes that are still open.
        let mut ifc = std::mem::take(&mut self.ifc);
        process_whitespace(&mut ifc);
        if ifc.has_content() {
            let anon = Rc::new(ComputedStyle::anonymous(&self.style, Display::Block));
            self.blocks.push(LayoutBox::anonymous(anon, BoxContent::Inline(ifc)));
        } else {
            // Blank IFC: keep its out-of-flow boxes as block-level children.
            for item in ifc.items {
                match item {
                    InlineItem::Float(b) | InlineItem::Abs(b) => self.blocks.push(b),
                    _ => {}
                }
            }
        }
    }

    fn finish(mut self) -> BoxContent {
        if self.blocks.is_empty() {
            let mut ifc = std::mem::take(&mut self.ifc);
            process_whitespace(&mut ifc);
            return BoxContent::Inline(ifc);
        }
        self.flush_ifc();
        BoxContent::Blocks(self.blocks)
    }
}
