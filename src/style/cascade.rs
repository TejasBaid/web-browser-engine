//! Selector matching over the document and the cascade: collects the declarations that
//! apply to each element, orders them by origin, importance, specificity and source
//! order, and computes the resulting [`ComputedStyle`] for every element and for
//! `::before`/`::after` pseudo-elements.

use super::computed::*;
use super::hints::presentational_hints;
use super::properties::{
    ComputeContext, PROPERTY_COUNT, PropertyDeclaration, PropertyId, SpecifiedValue, apply_property,
};
use super::stylesheet::{CssRule, DeclarationBlock, Origin, Stylesheet};
use crate::css::media::{MediaContext, evaluate_media_list};
use crate::css::selector::{PseudoElement, Selector, Simple};
use crate::dom::{Document, NodeData, NodeId};
use crate::util::FxHashMap;
use std::collections::HashMap;
use std::rc::Rc;

/// A rule flattened for matching: one entry per selector in a selector list.
struct IndexedRule {
    selector: Selector,
    block: Rc<DeclarationBlock>,
    origin: Origin,
    order: u32,
}

/// Rules bucketed by the most selective simple selector of their rightmost compound.
#[derive(Default)]
struct RuleIndex {
    rules: Vec<IndexedRule>,
    by_id: FxHashMap<String, Vec<u32>>,
    by_class: FxHashMap<String, Vec<u32>>,
    by_tag: FxHashMap<String, Vec<u32>>,
    universal: Vec<u32>,
}

impl RuleIndex {
    fn build(sheets: &[Rc<Stylesheet>], media: &MediaContext) -> RuleIndex {
        let mut idx = RuleIndex::default();
        fn walk(idx: &mut RuleIndex, rules: &[CssRule], origin: Origin, media: &MediaContext) {
            for r in rules {
                match r {
                    CssRule::Style(sr) => {
                        for sel in &sr.selectors {
                            if sel.pseudo_element == Some(PseudoElement::Unsupported) {
                                continue;
                            }
                            let i = idx.rules.len() as u32;
                            idx.rules.push(IndexedRule {
                                selector: sel.clone(),
                                block: sr.block.clone(),
                                origin,
                                order: i,
                            });
                            idx.insert(i);
                        }
                    }
                    CssRule::Media { query, rules } if evaluate_media_list(query, media) => {
                        walk(idx, rules, origin, media);
                    }
                    _ => {}
                }
            }
        }
        for sheet in sheets {
            walk(&mut idx, &sheet.rules, sheet.origin, media);
        }
        idx
    }

    fn insert(&mut self, i: u32) {
        let subject = self.rules[i as usize].selector.subject();
        let mut id = None;
        let mut class = None;
        let mut tag = None;
        for s in subject {
            match s {
                Simple::Id(v) => id = Some(v.clone()),
                Simple::Class(v) if class.is_none() => class = Some(v.clone()),
                Simple::Type(v) => tag = Some(v.clone()),
                _ => {}
            }
        }
        if let Some(id) = id {
            self.by_id.entry(id).or_default().push(i);
        } else if let Some(c) = class {
            self.by_class.entry(c).or_default().push(i);
        } else if let Some(t) = tag {
            self.by_tag.entry(t).or_default().push(i);
        } else {
            self.universal.push(i);
        }
    }

    /// Indices of rules that match `el`, sorted by (specificity, source order).
    fn matching(&self, doc: &Document, el: NodeId, out: &mut Vec<u32>) {
        out.clear();
        let e = doc.element(el).expect("element");
        let mut candidates: Vec<u32> = Vec::new();
        if let Some(id) = e.id()
            && let Some(v) = self.by_id.get(id)
        {
            candidates.extend_from_slice(v);
        }
        for c in &e.classes {
            if let Some(v) = self.by_class.get(c) {
                candidates.extend_from_slice(v);
            }
        }
        let lower;
        let tag = if e.name.bytes().any(|b| b.is_ascii_uppercase()) {
            lower = e.name.to_ascii_lowercase();
            &lower
        } else {
            &e.name
        };
        if let Some(v) = self.by_tag.get(tag) {
            candidates.extend_from_slice(v);
        }
        candidates.extend_from_slice(&self.universal);
        candidates.sort_unstable();
        candidates.dedup();
        for i in candidates {
            if self.rules[i as usize].selector.matches(doc, el) {
                out.push(i);
            }
        }
        out.sort_by_key(|&i| {
            let r = &self.rules[i as usize];
            (r.selector.specificity, r.order)
        });
    }
}

/// Computed styles for a document.
pub struct StyledDocument {
    styles: Vec<Option<Rc<ComputedStyle>>>,
    before: HashMap<NodeId, Rc<ComputedStyle>>,
    after: HashMap<NodeId, Rc<ComputedStyle>>,
    /// Number of (element, rule) selector matches attempted, for statistics.
    pub elements_styled: usize,
}

impl StyledDocument {
    /// The computed style of an element or text node (text nodes share their parent's
    /// style). `None` for nodes inside `display: none` subtrees.
    pub fn style(&self, node: NodeId) -> Option<&Rc<ComputedStyle>> {
        self.styles.get(node.index()).and_then(|s| s.as_ref())
    }

    /// The style of the `::before` pseudo-element, if it generates a box.
    pub fn before(&self, node: NodeId) -> Option<&Rc<ComputedStyle>> {
        self.before.get(&node)
    }

    /// The style of the `::after` pseudo-element, if it generates a box.
    pub fn after(&self, node: NodeId) -> Option<&Rc<ComputedStyle>> {
        self.after.get(&node)
    }
}

/// Viewport parameters for style computation.
#[derive(Clone, Copy, Debug)]
pub struct Viewport {
    /// Width in CSS px.
    pub width: f32,
    /// Height in CSS px.
    pub height: f32,
}

/// Computes styles for every element in `doc` from `sheets` (in cascade order: the UA
/// sheet first, then author sheets in document order).
pub fn compute_styles(doc: &Document, sheets: &[Rc<Stylesheet>], viewport: Viewport) -> StyledDocument {
    let media = MediaContext {
        width: viewport.width,
        height: viewport.height,
    };
    let index = RuleIndex::build(sheets, &media);
    let mut out = StyledDocument {
        styles: vec![None; doc.len()],
        before: HashMap::new(),
        after: HashMap::new(),
        elements_styled: 0,
    };
    let mut resolver = Resolver {
        doc,
        index: &index,
        viewport,
        root_font_size: 16.0,
        matched: Vec::new(),
        inline_cache: HashMap::new(),
        share_cache: FxHashMap::default(),
    };
    let initial = ComputedStyle::initial();
    if let Some(root) = doc.document_element() {
        resolver.style_subtree(root, &initial, true, &mut out);
    }
    out
}

struct Resolver<'a> {
    doc: &'a Document,
    index: &'a RuleIndex,
    viewport: Viewport,
    root_font_size: f32,
    matched: Vec<u32>,
    inline_cache: HashMap<NodeId, Rc<DeclarationBlock>>,
    share_cache: FxHashMap<(usize, Vec<u32>), Rc<ComputedStyle>>,
}

/// Cascade levels in increasing precedence.
const UA_NORMAL: u8 = 0;
const AUTHOR_NORMAL: u8 = 1;
const AUTHOR_IMPORTANT: u8 = 2;
const UA_IMPORTANT: u8 = 3;

fn level(origin: Origin, important: bool) -> u8 {
    match (origin, important) {
        (Origin::UserAgent, false) => UA_NORMAL,
        (Origin::Author, false) => AUTHOR_NORMAL,
        (Origin::Author, true) => AUTHOR_IMPORTANT,
        (Origin::UserAgent, true) => UA_IMPORTANT,
    }
}

impl Resolver<'_> {
    fn style_subtree(&mut self, el: NodeId, parent: &Rc<ComputedStyle>, is_root: bool, out: &mut StyledDocument) {
        // Match once, then split by pseudo-element.
        let mut matched = std::mem::take(&mut self.matched);
        self.index.matching(self.doc, el, &mut matched);
        let rules = &self.index.rules;
        let pe_of = |i: &u32| rules[*i as usize].selector.pseudo_element;
        let before: Vec<u32> = matched
            .iter()
            .copied()
            .filter(|i| pe_of(i) == Some(PseudoElement::Before))
            .collect();
        let after: Vec<u32> = matched
            .iter()
            .copied()
            .filter(|i| pe_of(i) == Some(PseudoElement::After))
            .collect();
        matched.retain(|i| pe_of(i).is_none());
        let hints = presentational_hints(self.doc, el);
        let inline = self.inline_style(el);
        // Style sharing: elements with the same parent style and matched rules, and no
        // element-specific declarations, get identical computed styles.
        let shareable = !is_root && hints.is_empty() && inline.is_none();
        let key = (Rc::as_ptr(parent) as usize, matched.clone());
        let cached = if shareable {
            self.share_cache.get(&key).cloned()
        } else {
            None
        };
        let style = match cached {
            Some(s) => s,
            None => {
                let s = Rc::new(self.cascade(&matched, &hints, inline.as_deref(), parent, Some(el), is_root));
                if shareable {
                    if self.share_cache.len() > 50_000 {
                        self.share_cache.clear();
                    }
                    self.share_cache.insert(key, s.clone());
                }
                s
            }
        };
        self.matched = matched;
        out.elements_styled += 1;
        if is_root {
            self.root_font_size = style.font_size;
        }
        if style.display == Display::None {
            return;
        }
        out.styles[el.index()] = Some(style.clone());
        for (list, map) in [(before, &mut out.before), (after, &mut out.after)] {
            if list.is_empty() {
                continue;
            }
            let ps = self.cascade(&list, &[], None, &style, None, false);
            if ps.display != Display::None && ps.content.is_some() {
                map.insert(el, Rc::new(ps));
            }
        }
        let mut child = self.doc.first_child(el);
        while let Some(c) = child {
            match &self.doc.node(c).data {
                NodeData::Element(_) => self.style_subtree(c, &style, false, out),
                NodeData::Text(_) => out.styles[c.index()] = Some(style.clone()),
                _ => {}
            }
            child = self.doc.next_sibling(c);
        }
        // <template> contents are inert and never rendered.
    }

    fn inline_style(&mut self, el: NodeId) -> Option<Rc<DeclarationBlock>> {
        let css = self.doc.attr(el, "style")?;
        Some(
            self.inline_cache
                .entry(el)
                .or_insert_with(|| Rc::new(DeclarationBlock::parse_inline(css)))
                .clone(),
        )
    }

    /// Runs the cascade for one element and computes its style.
    fn cascade(
        &self,
        matched: &[u32],
        hints: &[PropertyDeclaration],
        inline: Option<&DeclarationBlock>,
        parent: &ComputedStyle,
        el: Option<NodeId>,
        is_root: bool,
    ) -> ComputedStyle {
        let mut winners: [Option<&SpecifiedValue>; PROPERTY_COUNT] = [None; PROPERTY_COUNT];
        let mut customs: Vec<(&Rc<str>, &Rc<str>)> = Vec::new();
        // Applies declarations of one cascade level, in increasing precedence.
        let rules = &self.index.rules;
        for lvl in [UA_NORMAL, AUTHOR_NORMAL, AUTHOR_IMPORTANT, UA_IMPORTANT] {
            if lvl == AUTHOR_NORMAL {
                for d in hints {
                    record(d, &mut winners, &mut customs);
                }
            }
            for &i in matched {
                let r = &rules[i as usize];
                for (d, important) in &r.block.declarations {
                    if level(r.origin, *important) == lvl {
                        record(d, &mut winners, &mut customs);
                    }
                }
            }
            if let Some(block) = inline {
                for (d, important) in &block.declarations {
                    if level(Origin::Author, *important) == lvl {
                        record(d, &mut winners, &mut customs);
                    }
                }
            }
        }

        let mut s = ComputedStyle::inherit_from(parent);
        if !customs.is_empty() {
            let mut map = parent.custom_properties.as_deref().cloned().unwrap_or_default();
            for (k, v) in customs {
                map.insert(k.to_string(), v.clone());
            }
            s.custom_properties = Some(Rc::new(map));
        }
        let ctx = ComputeContext {
            parent,
            root_font_size: if is_root { 16.0 } else { self.root_font_size },
            viewport_width: self.viewport.width,
            viewport_height: self.viewport.height,
        };
        const EARLY: [PropertyId; 3] = [PropertyId::FontFamily, PropertyId::FontSize, PropertyId::Color];
        // font-size depends on font-family (monospace quirk) even when only the family
        // changes, so recompute an inherited size when the family is set.
        if winners[PropertyId::FontFamily as usize].is_some() && winners[PropertyId::FontSize as usize].is_none() {
            apply_property(
                PropertyId::FontFamily,
                winners[PropertyId::FontFamily as usize].unwrap(),
                &mut s,
                &ctx,
            );
            let rel = SpecifiedValue::Length(crate::css::values::SpecifiedLength::Linear(
                crate::css::values::LinearLength {
                    em: 1.0,
                    ..Default::default()
                },
            ));
            apply_property(PropertyId::FontSize, &rel, &mut s, &ctx);
        }
        for id in EARLY {
            if let Some(v) = winners[id as usize] {
                apply_property(id, v, &mut s, &ctx);
            }
        }
        // currentcolor defaults.
        s.border_color = [s.color; 4];
        s.outline_color = s.color;
        for &id in PropertyId::ALL {
            if EARLY.contains(&id) {
                continue;
            }
            if let Some(v) = winners[id as usize] {
                apply_property(id, v, &mut s, &ctx);
            }
        }
        self.fixup(&mut s, parent, el, is_root);
        s
    }

    /// Post-cascade adjustments: border widths for `none` styles, blockification,
    /// text decoration propagation.
    fn fixup(&self, s: &mut ComputedStyle, parent: &ComputedStyle, el: Option<NodeId>, is_root: bool) {
        for i in 0..4 {
            if matches!(s.border_style[i], BorderStyle::None | BorderStyle::Hidden) {
                s.border_width[i] = 0.0;
            }
        }
        if s.outline_style == BorderStyle::None {
            s.outline_width = 0.0;
        }
        if s.is_out_of_flow_positioned() {
            s.float = Float::None;
        }
        let parent_is_flex = matches!(
            parent.display,
            Display::Flex | Display::InlineFlex | Display::Grid | Display::InlineGrid
        );
        if is_root || s.float != Float::None || s.is_out_of_flow_positioned() || (parent_is_flex && el.is_some()) {
            s.display = s.display.blockify();
        }
        if is_root && s.display == Display::Contents {
            s.display = Display::Block;
        }
        // Text decorations propagate to in-flow inline content, but not into atomic
        // inlines, floats or absolutely positioned boxes.
        let atomic = matches!(
            s.display,
            Display::InlineBlock | Display::InlineTable | Display::InlineFlex | Display::InlineGrid
        ) || s.float != Float::None
            || s.is_out_of_flow_positioned();
        let (u, o, l) = s.text_decoration_line;
        if atomic || u || o || l {
            let mut list: Vec<TextDecoration> = if atomic {
                Vec::new()
            } else {
                parent.text_decorations.to_vec()
            };
            if u || o || l {
                list.push(TextDecoration {
                    underline: u,
                    overline: o,
                    line_through: l,
                    color: s.text_decoration_color.unwrap_or(s.color),
                    style: s.text_decoration_style,
                });
            }
            s.text_decorations = Rc::from(list);
        }
    }
}

fn record<'a>(
    d: &'a PropertyDeclaration,
    winners: &mut [Option<&'a SpecifiedValue>; PROPERTY_COUNT],
    customs: &mut Vec<(&'a Rc<str>, &'a Rc<str>)>,
) {
    match d {
        PropertyDeclaration::Longhand(id, v) => winners[*id as usize] = Some(v),
        PropertyDeclaration::Custom(name, value) => customs.push((name, value)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::css::color::Color;
    use crate::html::parse_document;

    fn styled(html: &str, css: &str) -> (Document, StyledDocument) {
        let doc = parse_document(html);
        let sheets = vec![
            Rc::new(Stylesheet::parse(super::super::UA_CSS, Origin::UserAgent, None)),
            Rc::new(Stylesheet::parse(css, Origin::Author, None)),
        ];
        let s = compute_styles(
            &doc,
            &sheets,
            Viewport {
                width: 800.0,
                height: 600.0,
            },
        );
        (doc, s)
    }

    fn by_id<'a>(doc: &Document, s: &'a StyledDocument, id: &str) -> &'a ComputedStyle {
        s.style(doc.element_by_id(id).unwrap()).unwrap()
    }

    #[test]
    fn specificity_and_order() {
        let (d, s) = styled(
            "<p id=a class=c>x</p>",
            "#a { color: red } p.c { color: blue } p { color: green } .c { color: yellow }",
        );
        assert_eq!(by_id(&d, &s, "a").color, Color::rgb(255, 0, 0));
        let (d, s) = styled("<p id=a class=c>x</p>", ".c { color: blue } .c { color: green }");
        assert_eq!(by_id(&d, &s, "a").color, Color::rgb(0, 128, 0));
    }

    #[test]
    fn important_and_inline() {
        let (d, s) = styled(
            "<p id=a style='color: blue'>x</p><p id=b style='color: blue !important'>y</p>",
            "#a { color: red } p { color: green !important } #b { color: red !important }",
        );
        assert_eq!(
            by_id(&d, &s, "a").color,
            Color::rgb(0, 128, 0),
            "!important beats inline"
        );
        assert_eq!(
            by_id(&d, &s, "b").color,
            Color::rgb(0, 0, 255),
            "inline !important wins"
        );
    }

    #[test]
    fn inheritance_and_keywords() {
        let (d, s) = styled(
            "<div id=p><span id=c>x</span><span id=i>y</span><span id=u>z</span></div>",
            "#p { color: red; border: 1px solid blue; font-size: 20px } #c { font-size: 2em }
             #i { border: inherit } #u { color: initial }",
        );
        assert_eq!(by_id(&d, &s, "c").color, Color::rgb(255, 0, 0));
        assert_eq!(by_id(&d, &s, "c").font_size, 40.0);
        assert_eq!(by_id(&d, &s, "c").border_width, [0.0; 4], "border is not inherited");
        assert_eq!(by_id(&d, &s, "i").border_width, [1.0; 4]);
        assert_eq!(by_id(&d, &s, "u").color, Color::BLACK);
    }

    #[test]
    fn ua_defaults() {
        let (d, s) = styled("<h1 id=h>t</h1><p id=p>x</p><a id=a href=#>l</a>", "");
        let h = by_id(&d, &s, "h");
        assert_eq!(h.display, Display::Block);
        assert_eq!(h.font_size, 32.0);
        assert_eq!(h.font_weight, 700);
        assert_eq!(by_id(&d, &s, "a").color, Color::rgb(0, 0, 0xee));
        assert!(by_id(&d, &s, "a").text_decorations[0].underline);
        let body = s.style(d.body().unwrap()).unwrap();
        assert_eq!(
            body.margin[0],
            LengthOrAuto::Length(crate::css::values::LengthPercentage::px(8.0))
        );
    }

    #[test]
    fn units_media_and_vars() {
        let (d, s) = styled(
            "<div id=a></div><div id=b></div><div id=c></div>",
            ":root { --w: 25vw; font-size: 10px } #a { width: var(--w); padding: 2rem }
             @media (min-width: 700px) { #b { width: 50% } } @media (max-width: 700px) { #b { width: 1px } }
             #c { width: calc(100% - 3em); height: var(--missing, 7px) }",
        );
        let a = by_id(&d, &s, "a");
        assert_eq!(a.width.length().unwrap().resolve(0.0), 200.0);
        assert_eq!(a.padding[0].resolve(0.0), 20.0);
        assert_eq!(by_id(&d, &s, "b").width.length().unwrap().resolve(100.0), 50.0);
        let c = by_id(&d, &s, "c");
        assert_eq!(c.width.length().unwrap().resolve(100.0), 70.0);
        assert_eq!(c.height.length().unwrap().resolve(0.0), 7.0);
    }

    #[test]
    fn pseudo_elements_and_display_none() {
        let (d, s) = styled(
            "<p id=a>x</p><div id=h style='display:none'><span id=inner>y</span></div>",
            "#a::before { content: '>'; color: red } #a::after { color: blue }",
        );
        let a = d.element_by_id("a").unwrap();
        assert!(s.before(a).is_some());
        assert!(s.after(a).is_none(), "no content, no box");
        assert!(s.style(d.element_by_id("inner").unwrap()).is_none());
    }

    #[test]
    fn blockification_of_floats_and_flex_items() {
        let (d, s) = styled(
            "<span id=f style='float:left'>x</span><div style='display:flex'><span id=i>y</span></div>",
            "",
        );
        assert_eq!(by_id(&d, &s, "f").display, Display::Block);
        assert_eq!(by_id(&d, &s, "i").display, Display::Block);
    }

    #[test]
    fn presentational_hints_lose_to_author_css() {
        let (d, s) = styled(
            "<table id=t bgcolor=red width=50%><tr><td id=c>x</td></tr></table>",
            "#c { background-color: blue }",
        );
        let t = by_id(&d, &s, "t");
        assert_eq!(t.background_color, Color::rgb(255, 0, 0));
        assert_eq!(by_id(&d, &s, "c").background_color, Color::rgb(0, 0, 255));
    }
}
