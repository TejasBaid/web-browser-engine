//! HTML tree construction (<https://html.spec.whatwg.org/multipage/parsing.html#tree-construction>).
//!
//! Implements the insertion-mode state machine including implied end tags, head/body
//! inference, the "in table" family of modes with foster parenting, the list of active
//! formatting elements with the adoption agency algorithm, templates, foreign content
//! (SVG and MathML) and fragment parsing. Scripting is treated as disabled.

use super::tokenizer::{Doctype, State, Tag, Token, Tokenizer};
use crate::dom::{AttrNamespace, Attribute, Document, ElementData, Namespace, NodeData, NodeId, QuirksMode};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Mode {
    Initial,
    BeforeHtml,
    BeforeHead,
    InHead,
    InHeadNoscript,
    AfterHead,
    InBody,
    Text,
    InTable,
    InTableText,
    InCaption,
    InColumnGroup,
    InTableBody,
    InRow,
    InCell,
    InTemplate,
    AfterBody,
    InFrameset,
    AfterFrameset,
    AfterAfterBody,
    AfterAfterFrameset,
}

#[derive(Clone, Debug)]
enum Formatting {
    Marker,
    Element(NodeId, Tag),
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Scope {
    Default,
    ListItem,
    Button,
    Table,
}

/// Options controlling the parser.
#[derive(Clone, Debug, Default)]
pub struct ParseOptions {
    /// When true, `<noscript>` contents are parsed as raw text (as a scripting browser
    /// would). The engine does not run scripts, so this defaults to false.
    pub scripting: bool,
}

/// Parses a complete HTML document.
pub fn parse_document(input: &str) -> Document {
    parse_document_with(input, ParseOptions::default())
}

/// Parses a complete HTML document with the given options.
pub fn parse_document_with(input: &str, opts: ParseOptions) -> Document {
    let mut tb = TreeBuilder::new(input, opts);
    tb.run();
    tb.doc
}

/// Parses an HTML fragment as if it were the `innerHTML` of a `context` element
/// (`context_ns`, `context_name`). Returns the document and the node whose children are
/// the parsed fragment.
pub fn parse_fragment(
    input: &str,
    context_ns: Namespace,
    context_name: &str,
    opts: ParseOptions,
) -> (Document, NodeId) {
    let mut tb = TreeBuilder::new(input, opts);
    let ctx = tb
        .doc
        .create_node(NodeData::Element(ElementData::new(context_ns, context_name, vec![])));
    tb.context = Some(ctx);
    if context_ns == Namespace::Html {
        tb.tokenizer.state = match context_name {
            "title" | "textarea" => State::Rcdata,
            "style" | "xmp" | "iframe" | "noembed" | "noframes" => State::Rawtext,
            "script" => State::ScriptData,
            "noscript" if tb.scripting => State::Rawtext,
            "plaintext" => State::Plaintext,
            _ => State::Data,
        };
    }
    let root = tb.doc.create_element("html", vec![]);
    let doc_root = tb.doc.root();
    tb.doc.append_child(doc_root, root);
    tb.open.push(root);
    if context_ns == Namespace::Html && context_name == "template" {
        tb.template_modes.push(Mode::InTemplate);
    }
    tb.reset_insertion_mode();
    if context_ns == Namespace::Html && context_name == "form" {
        tb.form = Some(ctx);
    }
    tb.run();
    (tb.doc, root)
}

struct TreeBuilder {
    doc: Document,
    tokenizer: Tokenizer,
    mode: Mode,
    original_mode: Mode,
    template_modes: Vec<Mode>,
    open: Vec<NodeId>,
    active: Vec<Formatting>,
    head: Option<NodeId>,
    form: Option<NodeId>,
    frameset_ok: bool,
    foster_parenting: bool,
    pending_table_text: Vec<String>,
    context: Option<NodeId>,
    scripting: bool,
    ignore_lf: bool,
    stopped: bool,
    selectedcontent_seen: bool,
}

fn is_ws_char(c: char) -> bool {
    matches!(c, '\t' | '\n' | '\x0C' | '\r' | ' ')
}

fn all_ws(s: &str) -> bool {
    s.chars().all(is_ws_char)
}

/// Splits a character run into homogeneous chunks: whitespace, a single NUL, or other text.
fn split_chunks(s: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let bytes = s.as_bytes();
    let class = |b: u8| -> u8 {
        match b {
            b'\t' | b'\n' | 0x0C | b'\r' | b' ' => 0,
            0 => 1,
            _ => 2,
        }
    };
    let mut start = 0;
    while start < bytes.len() {
        let c = class(bytes[start]);
        let mut end = start + 1;
        if c != 1 {
            while end < bytes.len() && class(bytes[end]) == c {
                end += 1;
            }
        }
        out.push(&s[start..end]);
        start = end;
    }
    out
}

const SPECIAL_HTML: &[&str] = &[
    "address",
    "applet",
    "area",
    "article",
    "aside",
    "base",
    "basefont",
    "bgsound",
    "blockquote",
    "body",
    "br",
    "button",
    "caption",
    "center",
    "col",
    "colgroup",
    "dd",
    "details",
    "dir",
    "div",
    "dl",
    "dt",
    "embed",
    "fieldset",
    "figcaption",
    "figure",
    "footer",
    "form",
    "frame",
    "frameset",
    "h1",
    "h2",
    "h3",
    "h4",
    "h5",
    "h6",
    "head",
    "header",
    "hgroup",
    "hr",
    "html",
    "iframe",
    "img",
    "input",
    "keygen",
    "li",
    "link",
    "listing",
    "main",
    "marquee",
    "menu",
    "meta",
    "nav",
    "noembed",
    "noframes",
    "noscript",
    "object",
    "ol",
    "p",
    "param",
    "plaintext",
    "pre",
    "script",
    "search",
    "section",
    "select",
    "source",
    "style",
    "summary",
    "table",
    "tbody",
    "td",
    "template",
    "textarea",
    "tfoot",
    "th",
    "thead",
    "title",
    "tr",
    "track",
    "ul",
    "wbr",
    "xmp",
];

impl TreeBuilder {
    fn new(input: &str, opts: ParseOptions) -> Self {
        TreeBuilder {
            doc: Document::new(),
            tokenizer: Tokenizer::new(input),
            mode: Mode::Initial,
            original_mode: Mode::Initial,
            template_modes: Vec::new(),
            open: Vec::new(),
            active: Vec::new(),
            head: None,
            form: None,
            frameset_ok: true,
            foster_parenting: false,
            pending_table_text: Vec::new(),
            context: None,
            scripting: opts.scripting,
            ignore_lf: false,
            stopped: false,
            selectedcontent_seen: false,
        }
    }

    fn run(&mut self) {
        while !self.stopped {
            self.tokenizer.allow_cdata = self
                .adjusted_current_node()
                .is_some_and(|n| self.ns(n) != Namespace::Html);
            let tok = self.tokenizer.next_token();
            match tok {
                Token::Characters(s) => {
                    let mut s = s.as_str();
                    if self.ignore_lf {
                        self.ignore_lf = false;
                        if let Some(rest) = s.strip_prefix('\n') {
                            s = rest;
                        }
                    }
                    for chunk in split_chunks(s) {
                        self.dispatch(Token::Characters(chunk.to_string()));
                    }
                }
                Token::Eof => {
                    self.dispatch(Token::Eof);
                    self.stopped = true;
                }
                t => {
                    self.ignore_lf = false;
                    self.dispatch(t);
                }
            }
        }
        // "The end": pop all the nodes off the stack of open elements.
        self.truncate_stack(0);
    }

    // ------------------------------------------------------------ node helpers

    fn ns(&self, id: NodeId) -> Namespace {
        self.doc.element(id).map(|e| e.ns).unwrap_or(Namespace::Html)
    }

    fn name(&self, id: NodeId) -> &str {
        self.doc.element(id).map(|e| e.name.as_str()).unwrap_or("")
    }

    fn is(&self, id: NodeId, name: &str) -> bool {
        self.doc.is_html_element(id, name)
    }

    fn is_one_of(&self, id: NodeId, names: &[&str]) -> bool {
        self.doc
            .element(id)
            .is_some_and(|e| e.ns == Namespace::Html && names.contains(&e.name.as_str()))
    }

    /// Whether the fragment parsing context element is the HTML element `name`.
    fn context_is(&self, name: &str) -> bool {
        self.context.is_some_and(|c| self.is(c, name))
    }

    fn current(&self) -> NodeId {
        *self.open.last().expect("stack of open elements is empty")
    }

    fn adjusted_current_node(&self) -> Option<NodeId> {
        if self.open.len() == 1 && self.context.is_some() {
            self.context
        } else {
            self.open.last().copied()
        }
    }

    fn is_special(&self, id: NodeId) -> bool {
        let Some(e) = self.doc.element(id) else { return false };
        match e.ns {
            Namespace::Html => SPECIAL_HTML.contains(&e.name.as_str()),
            Namespace::MathMl => matches!(e.name.as_str(), "mi" | "mo" | "mn" | "ms" | "mtext" | "annotation-xml"),
            Namespace::Svg => matches!(e.name.as_str(), "foreignObject" | "desc" | "title"),
        }
    }

    fn is_mathml_text_integration_point(&self, id: NodeId) -> bool {
        self.doc.element(id).is_some_and(|e| {
            e.ns == Namespace::MathMl && matches!(e.name.as_str(), "mi" | "mo" | "mn" | "ms" | "mtext")
        })
    }

    fn is_html_integration_point(&self, id: NodeId) -> bool {
        let Some(e) = self.doc.element(id) else { return false };
        match e.ns {
            Namespace::MathMl if e.name == "annotation-xml" => e.attr("encoding").is_some_and(|enc| {
                enc.eq_ignore_ascii_case("text/html") || enc.eq_ignore_ascii_case("application/xhtml+xml")
            }),
            Namespace::Svg => matches!(e.name.as_str(), "foreignObject" | "desc" | "title"),
            _ => false,
        }
    }

    // ------------------------------------------------------------ scopes

    fn is_scope_boundary(&self, id: NodeId, scope: Scope) -> bool {
        let Some(e) = self.doc.element(id) else { return false };
        let n = e.name.as_str();
        match scope {
            Scope::Table => e.ns == Namespace::Html && matches!(n, "html" | "table" | "template"),
            _ => {
                let base = match e.ns {
                    Namespace::Html => matches!(
                        n,
                        "applet"
                            | "caption"
                            | "html"
                            | "table"
                            | "td"
                            | "th"
                            | "marquee"
                            | "object"
                            | "select"
                            | "template"
                    ),
                    Namespace::MathMl => matches!(n, "mi" | "mo" | "mn" | "ms" | "mtext" | "annotation-xml"),
                    Namespace::Svg => matches!(n, "foreignObject" | "desc" | "title"),
                };
                base || match scope {
                    Scope::ListItem => e.ns == Namespace::Html && matches!(n, "ol" | "ul"),
                    Scope::Button => e.ns == Namespace::Html && n == "button",
                    _ => false,
                }
            }
        }
    }

    fn in_scope_by(&self, scope: Scope, pred: impl Fn(&Self, NodeId) -> bool) -> bool {
        for &n in self.open.iter().rev() {
            if pred(self, n) {
                return true;
            }
            if self.is_scope_boundary(n, scope) {
                return false;
            }
        }
        false
    }

    fn in_scope(&self, name: &str, scope: Scope) -> bool {
        self.in_scope_by(scope, |s, n| s.is(n, name))
    }

    fn node_in_scope(&self, node: NodeId, scope: Scope) -> bool {
        self.in_scope_by(scope, |_, n| n == node)
    }

    fn heading_in_scope(&self) -> bool {
        self.in_scope_by(Scope::Default, |s, n| {
            s.is_one_of(n, &["h1", "h2", "h3", "h4", "h5", "h6"])
        })
    }

    // ------------------------------------------------------------ stack ops

    fn pop(&mut self) -> NodeId {
        self.pop_raw().expect("pop on empty stack")
    }

    /// Pops the current node, running the "element popped" hooks.
    fn pop_raw(&mut self) -> Option<NodeId> {
        let n = self.open.pop()?;
        if self.selectedcontent_seen && self.is(n, "option") {
            self.update_selectedcontent(n);
        }
        Some(n)
    }

    fn truncate_stack(&mut self, len: usize) {
        while self.open.len() > len {
            self.pop_raw();
        }
    }

    /// "Update descendant selectedcontent elements for an option": when an option is
    /// popped and it is its select's selected option, its children are cloned into every
    /// `<selectedcontent>` inside that select.
    fn update_selectedcontent(&mut self, option: NodeId) {
        let Some(select) = self.doc.ancestors(option).find(|&a| self.is(a, "select")) else {
            return;
        };
        if self.doc.attr(select, "multiple").is_some() {
            return;
        }
        let mut first = None;
        let mut last_selected = None;
        let mut targets = Vec::new();
        for n in self.doc.descendants(select) {
            if self.is(n, "option") {
                first.get_or_insert(n);
                if self.doc.attr(n, "selected").is_some() {
                    last_selected = Some(n);
                }
            } else if self.is(n, "selectedcontent") {
                targets.push(n);
            }
        }
        if last_selected.or(first) != Some(option) {
            return;
        }
        for target in targets {
            while let Some(c) = self.doc.first_child(target) {
                self.doc.detach(c);
            }
            let children: Vec<NodeId> = self.doc.children(option).collect();
            for c in children {
                let clone = self.doc.clone_subtree(c);
                self.doc.append_child(target, clone);
            }
        }
    }

    fn pop_until(&mut self, name: &str) {
        while let Some(n) = self.pop_raw() {
            if self.is(n, name) {
                break;
            }
        }
    }

    fn pop_until_one_of(&mut self, names: &[&str]) {
        while let Some(n) = self.pop_raw() {
            if self.is_one_of(n, names) {
                break;
            }
        }
    }

    fn pop_until_node(&mut self, node: NodeId) {
        while let Some(n) = self.pop_raw() {
            if n == node {
                break;
            }
        }
    }

    fn remove_from_stack(&mut self, node: NodeId) {
        if let Some(i) = self.open.iter().rposition(|&n| n == node) {
            self.open.remove(i);
        }
    }

    fn generate_implied_end_tags(&mut self, except: Option<&str>) {
        const IMPLIED: &[&str] = &["dd", "dt", "li", "optgroup", "option", "p", "rb", "rp", "rt", "rtc"];
        while let Some(&n) = self.open.last() {
            if self.is_one_of(n, IMPLIED) && except.is_none_or(|e| !self.is(n, e)) {
                self.pop_raw();
            } else {
                break;
            }
        }
    }

    fn generate_all_implied_end_tags_thoroughly(&mut self) {
        const IMPLIED: &[&str] = &[
            "caption", "colgroup", "dd", "dt", "li", "optgroup", "option", "p", "rb", "rp", "rt", "rtc", "tbody", "td",
            "tfoot", "th", "thead", "tr",
        ];
        while let Some(&n) = self.open.last() {
            if self.is_one_of(n, IMPLIED) {
                self.pop_raw();
            } else {
                break;
            }
        }
    }

    fn close_p_element(&mut self) {
        self.generate_implied_end_tags(Some("p"));
        self.pop_until("p");
    }

    fn close_p_if_in_button_scope(&mut self) {
        if self.in_scope("p", Scope::Button) {
            self.close_p_element();
        }
    }

    fn clear_stack_back_to(&mut self, names: &[&str]) {
        while let Some(&n) = self.open.last() {
            if self.is_one_of(n, names) {
                break;
            }
            self.pop_raw();
        }
    }

    fn clear_to_table_context(&mut self) {
        self.clear_stack_back_to(&["table", "template", "html"]);
    }

    fn clear_to_table_body_context(&mut self) {
        self.clear_stack_back_to(&["tbody", "tfoot", "thead", "template", "html"]);
    }

    fn clear_to_table_row_context(&mut self) {
        self.clear_stack_back_to(&["tr", "template", "html"]);
    }

    // ------------------------------------------------------------ insertion

    /// Returns (parent, insert-before reference) for the appropriate place for inserting
    /// a node, applying foster parenting when enabled.
    fn appropriate_place(&self, override_target: Option<NodeId>) -> (NodeId, Option<NodeId>) {
        let target = override_target.unwrap_or_else(|| self.current());
        let (parent, before) =
            if self.foster_parenting && self.is_one_of(target, &["table", "tbody", "tfoot", "thead", "tr"]) {
                let last_template = self.open.iter().rposition(|&n| self.is(n, "template"));
                let last_table = self.open.iter().rposition(|&n| self.is(n, "table"));
                match (last_template, last_table) {
                    (Some(t), tb) if tb.is_none_or(|tb| t > tb) => (self.open[t], None),
                    (_, None) => (self.open[0], None),
                    (_, Some(tb)) => {
                        let table = self.open[tb];
                        match self.doc.parent(table) {
                            Some(p) => (p, Some(table)),
                            None => (self.open[tb - 1], None),
                        }
                    }
                }
            } else {
                (target, None)
            };
        // Inside a template, insert into its contents fragment.
        if before.is_none()
            && let Some(contents) = self.doc.element(parent).and_then(|e| e.template_contents)
        {
            return (contents, None);
        }
        (parent, before)
    }

    fn create_element_for(&mut self, tag: &Tag, ns: Namespace) -> NodeId {
        let attrs = tag
            .attrs
            .iter()
            .map(|(n, v)| Attribute::new(n.clone(), v.clone()))
            .collect();
        let id = self
            .doc
            .create_node(NodeData::Element(ElementData::new(ns, tag.name.clone(), attrs)));
        if ns == Namespace::Html && tag.name == "selectedcontent" {
            self.selectedcontent_seen = true;
        }
        if ns == Namespace::Html && tag.name == "template" {
            let frag = self.doc.create_node(NodeData::Fragment);
            if let Some(e) = self.doc.element_mut(id) {
                e.template_contents = Some(frag);
            }
        }
        id
    }

    fn insert_node_at_appropriate_place(&mut self, node: NodeId) {
        let (parent, before) = self.appropriate_place(None);
        self.doc.insert_before(parent, node, before);
    }

    fn insert_html_element(&mut self, tag: &Tag) -> NodeId {
        self.insert_foreign_element(tag, Namespace::Html)
    }

    fn insert_foreign_element(&mut self, tag: &Tag, ns: Namespace) -> NodeId {
        let el = self.create_element_for(tag, ns);
        self.insert_node_at_appropriate_place(el);
        self.open.push(el);
        el
    }

    fn insert_characters(&mut self, s: &str) {
        let (parent, before) = self.appropriate_place(None);
        if matches!(self.doc.node(parent).data, NodeData::Document) {
            return;
        }
        self.doc.insert_text_before(parent, s, before);
    }

    fn insert_comment(&mut self, text: String, at: Option<NodeId>) {
        let c = self.doc.create_node(NodeData::Comment(text));
        match at {
            Some(parent) => self.doc.append_child(parent, c),
            None => self.insert_node_at_appropriate_place(c),
        }
    }

    // ------------------------------------------------------------ active formatting

    fn push_active(&mut self, node: NodeId, tag: &Tag) {
        // Noah's Ark clause: at most three identical entries after the last marker.
        let mut same = Vec::new();
        for (i, f) in self.active.iter().enumerate().rev() {
            match f {
                Formatting::Marker => break,
                Formatting::Element(n, t) => {
                    let e = self.doc.element(*n).unwrap();
                    if t.name == tag.name && e.ns == Namespace::Html && same_attrs(&t.attrs, &tag.attrs) {
                        same.push(i);
                    }
                }
            }
        }
        if same.len() >= 3 {
            self.active.remove(*same.last().unwrap());
        }
        self.active.push(Formatting::Element(node, tag.clone()));
    }

    fn push_marker(&mut self) {
        self.active.push(Formatting::Marker);
    }

    fn clear_active_to_last_marker(&mut self) {
        while let Some(f) = self.active.pop() {
            if matches!(f, Formatting::Marker) {
                break;
            }
        }
    }

    fn active_index_of(&self, node: NodeId) -> Option<usize> {
        self.active
            .iter()
            .position(|f| matches!(f, Formatting::Element(n, _) if *n == node))
    }

    fn reconstruct_active_formatting(&mut self) {
        if self.active.is_empty() {
            return;
        }
        let is_open_or_marker = |s: &Self, f: &Formatting| match f {
            Formatting::Marker => true,
            Formatting::Element(n, _) => s.open.contains(n),
        };
        let last = self.active.len() - 1;
        if is_open_or_marker(self, &self.active[last]) {
            return;
        }
        let mut i = last;
        // Rewind.
        while i > 0 {
            if is_open_or_marker(self, &self.active[i - 1]) {
                break;
            }
            i -= 1;
        }
        // Advance + create.
        while i <= last {
            let Formatting::Element(_, tag) = self.active[i].clone() else {
                unreachable!()
            };
            let el = self.insert_html_element(&tag);
            self.active[i] = Formatting::Element(el, tag);
            i += 1;
        }
    }

    /// The adoption agency algorithm. Returns false if the caller should act as
    /// "any other end tag".
    fn adoption_agency(&mut self, subject: &str) -> bool {
        let cur = self.current();
        if self.is(cur, subject) && self.active_index_of(cur).is_none() {
            self.pop();
            return true;
        }
        for _ in 0..8 {
            // Formatting element: last element in the list (after the last marker) named subject.
            let mut fe_idx = None;
            for (i, f) in self.active.iter().enumerate().rev() {
                match f {
                    Formatting::Marker => break,
                    Formatting::Element(_, t) if t.name == subject => {
                        fe_idx = Some(i);
                        break;
                    }
                    _ => {}
                }
            }
            let Some(fe_idx) = fe_idx else { return false };
            let Formatting::Element(fe, fe_tag) = self.active[fe_idx].clone() else {
                unreachable!()
            };
            let Some(fe_stack_idx) = self.open.iter().position(|&n| n == fe) else {
                self.active.remove(fe_idx);
                return true;
            };
            if !self.node_in_scope(fe, Scope::Default) {
                return true;
            }
            // Furthest block: topmost special element below the formatting element.
            let furthest = self.open[fe_stack_idx + 1..]
                .iter()
                .position(|&n| self.is_special(n))
                .map(|i| i + fe_stack_idx + 1);
            let Some(fb_stack_idx) = furthest else {
                self.truncate_stack(fe_stack_idx);
                self.active.remove(fe_idx);
                return true;
            };
            let furthest_block = self.open[fb_stack_idx];
            let common_ancestor = self.open[fe_stack_idx - 1];
            let mut bookmark = fe_idx;
            let mut node_idx = fb_stack_idx;
            let mut last_node = furthest_block;
            let mut inner = 0;
            loop {
                inner += 1;
                node_idx -= 1;
                let mut node = self.open[node_idx];
                if node == fe {
                    break;
                }
                let mut node_active = self.active_index_of(node);
                if inner > 3
                    && let Some(ai) = node_active
                {
                    self.active.remove(ai);
                    if ai < bookmark {
                        bookmark -= 1;
                    }
                    node_active = None;
                }
                let Some(ai) = node_active else {
                    self.open.remove(node_idx);
                    continue;
                };
                // Replace node with a fresh clone.
                let Formatting::Element(_, tag) = self.active[ai].clone() else {
                    unreachable!()
                };
                let new_el = self.create_element_for(&tag, Namespace::Html);
                self.active[ai] = Formatting::Element(new_el, tag);
                self.open[node_idx] = new_el;
                node = new_el;
                if last_node == furthest_block {
                    bookmark = ai + 1;
                }
                self.doc.append_child(node, last_node);
                last_node = node;
            }
            // Insert last_node at the appropriate place with common_ancestor as override target.
            self.doc.detach(last_node);
            let (parent, before) = self.appropriate_place_for_adoption(common_ancestor);
            self.doc.insert_before(parent, last_node, before);
            // New element for the formatting element, adopt furthest block's children.
            let new_el = self.create_element_for(&fe_tag, Namespace::Html);
            self.doc.reparent_children(furthest_block, new_el);
            self.doc.append_child(furthest_block, new_el);
            // Update the list of active formatting elements.
            let fe_now = self.active_index_of(fe).expect("formatting element in list");
            self.active.remove(fe_now);
            if fe_now < bookmark {
                bookmark -= 1;
            }
            let bookmark = bookmark.min(self.active.len());
            self.active.insert(bookmark, Formatting::Element(new_el, fe_tag));
            // Update the stack.
            self.remove_from_stack(fe);
            let fb_pos = self.open.iter().position(|&n| n == furthest_block).unwrap();
            self.open.insert(fb_pos + 1, new_el);
        }
        true
    }

    fn appropriate_place_for_adoption(&self, target: NodeId) -> (NodeId, Option<NodeId>) {
        self.appropriate_place(Some(target))
    }

    // ------------------------------------------------------------ mode reset

    fn reset_insertion_mode(&mut self) {
        for i in (0..self.open.len()).rev() {
            let last = i == 0;
            let mut node = self.open[i];
            if last && let Some(ctx) = self.context {
                node = ctx;
            }
            let e = self.doc.element(node).unwrap();
            if e.ns != Namespace::Html {
                if last {
                    self.mode = Mode::InBody;
                    return;
                }
                continue;
            }
            let name = e.name.clone();
            match name.as_str() {
                "td" | "th" if !last => {
                    self.mode = Mode::InCell;
                    return;
                }
                "tr" => {
                    self.mode = Mode::InRow;
                    return;
                }
                "tbody" | "thead" | "tfoot" => {
                    self.mode = Mode::InTableBody;
                    return;
                }
                "caption" => {
                    self.mode = Mode::InCaption;
                    return;
                }
                "colgroup" => {
                    self.mode = Mode::InColumnGroup;
                    return;
                }
                "table" => {
                    self.mode = Mode::InTable;
                    return;
                }
                "template" => {
                    self.mode = *self.template_modes.last().unwrap_or(&Mode::InTemplate);
                    return;
                }
                "head" if !last => {
                    self.mode = Mode::InHead;
                    return;
                }
                "body" => {
                    self.mode = Mode::InBody;
                    return;
                }
                "frameset" => {
                    self.mode = Mode::InFrameset;
                    return;
                }
                "html" => {
                    self.mode = if self.head.is_none() {
                        Mode::BeforeHead
                    } else {
                        Mode::AfterHead
                    };
                    return;
                }
                _ => {}
            }
            if last {
                self.mode = Mode::InBody;
                return;
            }
        }
        self.mode = Mode::InBody;
    }

    // ------------------------------------------------------------ dispatch

    fn dispatch(&mut self, tok: Token) {
        let use_html_rules = match self.adjusted_current_node() {
            None => true,
            Some(acn) => {
                let ns = self.ns(acn);
                ns == Namespace::Html
                    || (self.is_mathml_text_integration_point(acn)
                        && match &tok {
                            Token::StartTag(t) => t.name != "mglyph" && t.name != "malignmark",
                            Token::Characters(_) => true,
                            _ => false,
                        })
                    || (ns == Namespace::MathMl
                        && self.name(acn) == "annotation-xml"
                        && matches!(&tok, Token::StartTag(t) if t.name == "svg"))
                    || (self.is_html_integration_point(acn)
                        && matches!(&tok, Token::StartTag(_) | Token::Characters(_)))
                    || matches!(tok, Token::Eof)
            }
        };
        if use_html_rules {
            self.process(self.mode, tok);
        } else {
            self.process_foreign(tok);
        }
    }

    fn process(&mut self, mode: Mode, tok: Token) {
        match mode {
            Mode::Initial => self.initial(tok),
            Mode::BeforeHtml => self.before_html(tok),
            Mode::BeforeHead => self.before_head(tok),
            Mode::InHead => self.in_head(tok),
            Mode::InHeadNoscript => self.in_head_noscript(tok),
            Mode::AfterHead => self.after_head(tok),
            Mode::InBody => self.in_body(tok),
            Mode::Text => self.text(tok),
            Mode::InTable => self.in_table(tok),
            Mode::InTableText => self.in_table_text(tok),
            Mode::InCaption => self.in_caption(tok),
            Mode::InColumnGroup => self.in_column_group(tok),
            Mode::InTableBody => self.in_table_body(tok),
            Mode::InRow => self.in_row(tok),
            Mode::InCell => self.in_cell(tok),
            Mode::InTemplate => self.in_template(tok),
            Mode::AfterBody => self.after_body(tok),
            Mode::InFrameset => self.in_frameset(tok),
            Mode::AfterFrameset => self.after_frameset(tok),
            Mode::AfterAfterBody => self.after_after_body(tok),
            Mode::AfterAfterFrameset => self.after_after_frameset(tok),
        }
    }

    fn reprocess(&mut self, mode: Mode, tok: Token) {
        self.mode = mode;
        self.process(mode, tok);
    }

    // ------------------------------------------------------------ modes

    fn initial(&mut self, tok: Token) {
        match tok {
            Token::Characters(ref s) if all_ws(s) => {}
            Token::Comment(c) => self.insert_comment(c, Some(self.doc.root())),
            Token::Doctype(d) => {
                let node = self.doc.create_node(NodeData::Doctype {
                    name: d.name.clone().unwrap_or_default(),
                    public_id: d.public_id.clone(),
                    system_id: d.system_id.clone(),
                });
                let root = self.doc.root();
                self.doc.append_child(root, node);
                self.doc.quirks_mode = quirks_mode_for(&d);
                self.mode = Mode::BeforeHtml;
            }
            t => {
                self.doc.quirks_mode = QuirksMode::Quirks;
                self.reprocess(Mode::BeforeHtml, t);
            }
        }
    }

    fn before_html(&mut self, tok: Token) {
        match tok {
            Token::Doctype(_) => {}
            Token::Comment(c) => self.insert_comment(c, Some(self.doc.root())),
            Token::Characters(ref s) if all_ws(s) => {}
            Token::StartTag(ref t) if t.name == "html" => {
                let el = self.create_element_for(t, Namespace::Html);
                let root = self.doc.root();
                self.doc.append_child(root, el);
                self.open.push(el);
                self.mode = Mode::BeforeHead;
            }
            Token::EndTag(ref t) if !matches!(t.name.as_str(), "head" | "body" | "html" | "br") => {}
            t => {
                let el = self.doc.create_element("html", vec![]);
                let root = self.doc.root();
                self.doc.append_child(root, el);
                self.open.push(el);
                self.reprocess(Mode::BeforeHead, t);
            }
        }
    }

    fn before_head(&mut self, tok: Token) {
        match tok {
            Token::Characters(ref s) if all_ws(s) => {}
            Token::Comment(c) => self.insert_comment(c, None),
            Token::Doctype(_) => {}
            Token::StartTag(ref t) if t.name == "html" => self.in_body(tok),
            Token::StartTag(ref t) if t.name == "head" => {
                let h = self.insert_html_element(t);
                self.head = Some(h);
                self.mode = Mode::InHead;
            }
            Token::EndTag(ref t) if !matches!(t.name.as_str(), "head" | "body" | "html" | "br") => {}
            t => {
                let h = self.insert_html_element(&tag("head"));
                self.head = Some(h);
                self.reprocess(Mode::InHead, t);
            }
        }
    }

    fn in_head(&mut self, tok: Token) {
        match tok {
            Token::Characters(ref s) if all_ws(s) => self.insert_characters(s),
            Token::Comment(c) => self.insert_comment(c, None),
            Token::Doctype(_) => {}
            Token::StartTag(ref t) => match t.name.as_str() {
                "html" => self.in_body(tok),
                "base" | "basefont" | "bgsound" | "link" | "meta" => {
                    self.insert_html_element(t);
                    self.pop();
                }
                "title" => self.generic_text_element(t, State::Rcdata),
                "noscript" if self.scripting => self.generic_text_element(t, State::Rawtext),
                "noframes" | "style" => self.generic_text_element(t, State::Rawtext),
                "noscript" => {
                    self.insert_html_element(t);
                    self.mode = Mode::InHeadNoscript;
                }
                "script" => {
                    self.insert_html_element(t);
                    self.tokenizer.state = State::ScriptData;
                    self.original_mode = self.mode;
                    self.mode = Mode::Text;
                }
                "template" => {
                    self.insert_html_element(t);
                    self.push_marker();
                    self.frameset_ok = false;
                    self.mode = Mode::InTemplate;
                    self.template_modes.push(Mode::InTemplate);
                }
                "head" => {}
                _ => self.in_head_anything_else(tok),
            },
            Token::EndTag(ref t) => match t.name.as_str() {
                "head" => {
                    self.pop();
                    self.mode = Mode::AfterHead;
                }
                "body" | "html" | "br" => self.in_head_anything_else(tok),
                "template" => self.end_template(),
                _ => {}
            },
            t => self.in_head_anything_else(t),
        }
    }

    fn end_template(&mut self) {
        if !self.open.iter().any(|&n| self.is(n, "template")) {
            return;
        }
        self.generate_all_implied_end_tags_thoroughly();
        self.pop_until("template");
        self.clear_active_to_last_marker();
        self.template_modes.pop();
        self.reset_insertion_mode();
    }

    fn in_head_anything_else(&mut self, tok: Token) {
        self.pop();
        self.reprocess(Mode::AfterHead, tok);
    }

    fn generic_text_element(&mut self, t: &Tag, state: State) {
        self.insert_html_element(t);
        self.tokenizer.state = state;
        self.original_mode = self.mode;
        self.mode = Mode::Text;
    }

    fn in_head_noscript(&mut self, tok: Token) {
        match tok {
            Token::Doctype(_) => {}
            Token::StartTag(ref t) if t.name == "html" => self.in_body(tok),
            Token::EndTag(ref t) if t.name == "noscript" => {
                self.pop();
                self.mode = Mode::InHead;
            }
            Token::Characters(ref s) if all_ws(s) => self.in_head(tok),
            Token::Comment(_) => self.in_head(tok),
            Token::StartTag(ref t)
                if matches!(
                    t.name.as_str(),
                    "basefont" | "bgsound" | "link" | "meta" | "noframes" | "style"
                ) =>
            {
                self.in_head(tok)
            }
            Token::StartTag(ref t) if matches!(t.name.as_str(), "head" | "noscript") => {}
            Token::EndTag(ref t) if t.name != "br" => {}
            t => {
                self.pop();
                self.reprocess(Mode::InHead, t);
            }
        }
    }

    fn after_head(&mut self, tok: Token) {
        match tok {
            Token::Characters(ref s) if all_ws(s) => self.insert_characters(s),
            Token::Comment(c) => self.insert_comment(c, None),
            Token::Doctype(_) => {}
            Token::StartTag(ref t) => match t.name.as_str() {
                "html" => self.in_body(tok),
                "body" => {
                    self.insert_html_element(t);
                    self.frameset_ok = false;
                    self.mode = Mode::InBody;
                }
                "frameset" => {
                    self.insert_html_element(t);
                    self.mode = Mode::InFrameset;
                }
                "base" | "basefont" | "bgsound" | "link" | "meta" | "noframes" | "script" | "style" | "template"
                | "title" => {
                    let head = self.head.expect("head element pointer");
                    self.open.push(head);
                    self.in_head(tok);
                    self.remove_from_stack(head);
                }
                "head" => {}
                _ => self.after_head_anything_else(tok),
            },
            Token::EndTag(ref t) => match t.name.as_str() {
                "template" => self.in_head(tok),
                "body" | "html" | "br" => self.after_head_anything_else(tok),
                _ => {}
            },
            t => self.after_head_anything_else(t),
        }
    }

    fn after_head_anything_else(&mut self, tok: Token) {
        self.insert_html_element(&tag("body"));
        self.reprocess(Mode::InBody, tok);
    }

    fn text(&mut self, tok: Token) {
        match tok {
            Token::Characters(s) => self.insert_characters(&s),
            Token::Eof => {
                self.pop();
                let m = self.original_mode;
                self.reprocess(m, Token::Eof);
            }
            Token::EndTag(_) => {
                self.pop();
                self.mode = self.original_mode;
            }
            _ => {}
        }
    }

    fn in_body(&mut self, tok: Token) {
        match tok {
            Token::Characters(s) => {
                if s == "\0" {
                    return;
                }
                self.reconstruct_active_formatting();
                self.insert_characters(&s);
                if !all_ws(&s) {
                    self.frameset_ok = false;
                }
            }
            Token::Comment(c) => self.insert_comment(c, None),
            Token::Doctype(_) => {}
            Token::StartTag(t) => self.in_body_start_tag(t),
            Token::EndTag(t) => self.in_body_end_tag(t),
            Token::Eof => {
                if !self.template_modes.is_empty() {
                    self.in_template(Token::Eof);
                } else {
                    self.stopped = true;
                }
            }
        }
    }

    fn in_body_start_tag(&mut self, t: Tag) {
        let name = t.name.as_str();
        match name {
            "html" => {
                if self.open.iter().any(|&n| self.is(n, "template")) {
                    return;
                }
                let html = self.open[0];
                self.add_missing_attrs(html, &t);
            }
            "base" | "basefont" | "bgsound" | "link" | "meta" | "noframes" | "script" | "style" | "template"
            | "title" => self.in_head(Token::StartTag(t)),
            "body" => {
                if self.open.len() == 1
                    || !self.is(self.open[1], "body")
                    || self.open.iter().any(|&n| self.is(n, "template"))
                {
                    return;
                }
                self.frameset_ok = false;
                let body = self.open[1];
                self.add_missing_attrs(body, &t);
            }
            "frameset" => {
                if self.open.len() == 1 || !self.is(self.open[1], "body") || !self.frameset_ok {
                    return;
                }
                let body = self.open[1];
                self.doc.detach(body);
                self.truncate_stack(1);
                self.insert_html_element(&t);
                self.mode = Mode::InFrameset;
            }
            "address" | "article" | "aside" | "blockquote" | "center" | "details" | "dialog" | "dir" | "div" | "dl"
            | "fieldset" | "figcaption" | "figure" | "footer" | "header" | "hgroup" | "main" | "menu" | "nav"
            | "ol" | "p" | "search" | "section" | "summary" | "ul" => {
                self.close_p_if_in_button_scope();
                self.insert_html_element(&t);
            }
            "h1" | "h2" | "h3" | "h4" | "h5" | "h6" => {
                self.close_p_if_in_button_scope();
                let cur = self.current();
                if self.is_one_of(cur, &["h1", "h2", "h3", "h4", "h5", "h6"]) {
                    self.pop();
                }
                self.insert_html_element(&t);
            }
            "pre" | "listing" => {
                self.close_p_if_in_button_scope();
                self.insert_html_element(&t);
                self.ignore_lf = true;
                self.frameset_ok = false;
            }
            "form" => {
                let has_template = self.open.iter().any(|&n| self.is(n, "template"));
                if self.form.is_some() && !has_template {
                    return;
                }
                self.close_p_if_in_button_scope();
                let f = self.insert_html_element(&t);
                if !has_template {
                    self.form = Some(f);
                }
            }
            "li" | "dd" | "dt" => {
                self.frameset_ok = false;
                let targets: &[&str] = if name == "li" { &["li"] } else { &["dd", "dt"] };
                for i in (0..self.open.len()).rev() {
                    let node = self.open[i];
                    if self.is_one_of(node, targets) {
                        let nn = self.name(node).to_string();
                        self.generate_implied_end_tags(Some(&nn));
                        self.pop_until(&nn);
                        break;
                    }
                    if self.is_special(node) && !self.is_one_of(node, &["address", "div", "p"]) {
                        break;
                    }
                }
                self.close_p_if_in_button_scope();
                self.insert_html_element(&t);
            }
            "plaintext" => {
                self.close_p_if_in_button_scope();
                self.insert_html_element(&t);
                self.tokenizer.state = State::Plaintext;
            }
            "button" => {
                if self.in_scope("button", Scope::Default) {
                    self.generate_implied_end_tags(None);
                    self.pop_until("button");
                }
                self.reconstruct_active_formatting();
                self.insert_html_element(&t);
                self.frameset_ok = false;
            }
            "a" => {
                let mut existing = None;
                for f in self.active.iter().rev() {
                    match f {
                        Formatting::Marker => break,
                        Formatting::Element(n, tg) if tg.name == "a" => {
                            existing = Some(*n);
                            break;
                        }
                        _ => {}
                    }
                }
                if let Some(a) = existing {
                    self.adoption_agency("a");
                    if let Some(i) = self.active_index_of(a) {
                        self.active.remove(i);
                    }
                    self.remove_from_stack(a);
                }
                self.reconstruct_active_formatting();
                let el = self.insert_html_element(&t);
                self.push_active(el, &t);
            }
            "b" | "big" | "code" | "em" | "font" | "i" | "s" | "small" | "strike" | "strong" | "tt" | "u" => {
                self.reconstruct_active_formatting();
                let el = self.insert_html_element(&t);
                self.push_active(el, &t);
            }
            "nobr" => {
                self.reconstruct_active_formatting();
                if self.in_scope("nobr", Scope::Default) {
                    if !self.adoption_agency("nobr") {
                        self.any_other_end_tag("nobr");
                    }
                    self.reconstruct_active_formatting();
                }
                let el = self.insert_html_element(&t);
                self.push_active(el, &t);
            }
            "applet" | "marquee" | "object" => {
                self.reconstruct_active_formatting();
                self.insert_html_element(&t);
                self.push_marker();
                self.frameset_ok = false;
            }
            "table" => {
                if self.doc.quirks_mode != QuirksMode::Quirks {
                    self.close_p_if_in_button_scope();
                }
                self.insert_html_element(&t);
                self.frameset_ok = false;
                self.mode = Mode::InTable;
            }
            "area" | "br" | "embed" | "img" | "keygen" | "wbr" => {
                self.reconstruct_active_formatting();
                self.insert_html_element(&t);
                self.pop();
                self.frameset_ok = false;
            }
            "input" => {
                if self.context_is("select") {
                    return;
                }
                if self.in_scope("select", Scope::Default) {
                    self.pop_until("select");
                }
                self.reconstruct_active_formatting();
                self.insert_html_element(&t);
                self.pop();
                let hidden = t
                    .attrs
                    .iter()
                    .any(|(n, v)| n == "type" && v.eq_ignore_ascii_case("hidden"));
                if !hidden {
                    self.frameset_ok = false;
                }
            }
            "param" | "source" | "track" => {
                self.insert_html_element(&t);
                self.pop();
            }
            "hr" => {
                self.close_p_if_in_button_scope();
                if self.in_scope("select", Scope::Default) {
                    self.generate_implied_end_tags(None);
                }
                self.insert_html_element(&t);
                self.pop();
                self.frameset_ok = false;
            }
            "image" => {
                let mut t = t;
                t.name = "img".to_string();
                self.in_body_start_tag(t);
            }
            "textarea" => {
                self.insert_html_element(&t);
                self.ignore_lf = true;
                self.tokenizer.state = State::Rcdata;
                self.original_mode = self.mode;
                self.frameset_ok = false;
                self.mode = Mode::Text;
            }
            "xmp" => {
                self.close_p_if_in_button_scope();
                self.reconstruct_active_formatting();
                self.frameset_ok = false;
                self.generic_text_element(&t, State::Rawtext);
            }
            "iframe" => {
                self.frameset_ok = false;
                self.generic_text_element(&t, State::Rawtext);
            }
            "noembed" => self.generic_text_element(&t, State::Rawtext),
            "noscript" if self.scripting => self.generic_text_element(&t, State::Rawtext),
            "select" => {
                if self.context_is("select") {
                    return;
                }
                if self.in_scope("select", Scope::Default) {
                    self.pop_until("select");
                    return;
                }
                self.reconstruct_active_formatting();
                self.insert_html_element(&t);
                self.frameset_ok = false;
            }
            "option" | "optgroup" => {
                if self.in_scope("select", Scope::Default) {
                    self.generate_implied_end_tags(if name == "option" { Some("optgroup") } else { None });
                } else if self.is(self.current(), "option") {
                    self.pop();
                }
                self.reconstruct_active_formatting();
                self.insert_html_element(&t);
            }
            "rb" | "rtc" => {
                if self.in_scope("ruby", Scope::Default) {
                    self.generate_implied_end_tags(None);
                }
                self.insert_html_element(&t);
            }
            "rp" | "rt" => {
                if self.in_scope("ruby", Scope::Default) {
                    self.generate_implied_end_tags(Some("rtc"));
                }
                self.insert_html_element(&t);
            }
            "math" | "svg" => {
                self.reconstruct_active_formatting();
                let ns = if name == "math" {
                    Namespace::MathMl
                } else {
                    Namespace::Svg
                };
                let mut t = t;
                adjust_foreign_attrs(&mut t, ns);
                self.insert_foreign_element_adjusted(&t, ns);
                if t.self_closing {
                    self.pop();
                }
            }
            "caption" | "col" | "colgroup" | "frame" | "head" | "tbody" | "td" | "tfoot" | "th" | "thead" | "tr" => {}
            _ => {
                self.reconstruct_active_formatting();
                self.insert_html_element(&t);
            }
        }
    }

    fn add_missing_attrs(&mut self, node: NodeId, t: &Tag) {
        if let Some(e) = self.doc.element_mut(node) {
            for (n, v) in &t.attrs {
                if e.attr(n).is_none() {
                    e.set_attr(n, v);
                }
            }
        }
    }

    fn in_body_end_tag(&mut self, t: Tag) {
        let name = t.name.as_str();
        match name {
            "template" => self.in_head(Token::EndTag(t)),
            "body" | "html" => {
                if !self.in_scope("body", Scope::Default) {
                    return;
                }
                self.mode = Mode::AfterBody;
                if name == "html" {
                    self.after_body(Token::EndTag(t));
                }
            }
            "address" | "article" | "aside" | "blockquote" | "button" | "center" | "details" | "dialog" | "dir"
            | "div" | "dl" | "fieldset" | "figcaption" | "figure" | "footer" | "header" | "hgroup" | "listing"
            | "main" | "menu" | "nav" | "ol" | "pre" | "search" | "section" | "select" | "summary" | "ul" => {
                if !self.in_scope(name, Scope::Default) {
                    return;
                }
                self.generate_implied_end_tags(None);
                self.pop_until(name);
            }
            "form" => {
                if !self.open.iter().any(|&n| self.is(n, "template")) {
                    let node = self.form.take();
                    let Some(node) = node else { return };
                    if !self.node_in_scope(node, Scope::Default) {
                        return;
                    }
                    self.generate_implied_end_tags(None);
                    self.remove_from_stack(node);
                } else {
                    if !self.in_scope("form", Scope::Default) {
                        return;
                    }
                    self.generate_implied_end_tags(None);
                    self.pop_until("form");
                }
            }
            "p" => {
                if !self.in_scope("p", Scope::Button) {
                    self.insert_html_element(&tag("p"));
                }
                self.close_p_element();
            }
            "li" => {
                if !self.in_scope("li", Scope::ListItem) {
                    return;
                }
                self.generate_implied_end_tags(Some("li"));
                self.pop_until("li");
            }
            "dd" | "dt" => {
                if !self.in_scope(name, Scope::Default) {
                    return;
                }
                self.generate_implied_end_tags(Some(name));
                self.pop_until(name);
            }
            "h1" | "h2" | "h3" | "h4" | "h5" | "h6" => {
                if !self.heading_in_scope() {
                    return;
                }
                self.generate_implied_end_tags(None);
                self.pop_until_one_of(&["h1", "h2", "h3", "h4", "h5", "h6"]);
            }
            "a" | "b" | "big" | "code" | "em" | "font" | "i" | "nobr" | "s" | "small" | "strike" | "strong" | "tt"
            | "u" => {
                if !self.adoption_agency(name) {
                    self.any_other_end_tag(name);
                }
            }
            "applet" | "marquee" | "object" => {
                if !self.in_scope(name, Scope::Default) {
                    return;
                }
                self.generate_implied_end_tags(None);
                self.pop_until(name);
                self.clear_active_to_last_marker();
            }
            "br" => {
                self.in_body_start_tag(tag("br"));
            }
            _ => self.any_other_end_tag(name),
        }
    }

    fn any_other_end_tag(&mut self, name: &str) {
        for i in (0..self.open.len()).rev() {
            let node = self.open[i];
            if self.is(node, name) {
                self.generate_implied_end_tags(Some(name));
                self.pop_until_node(node);
                return;
            }
            if self.is_special(node) {
                return;
            }
        }
    }

    fn in_table(&mut self, tok: Token) {
        match tok {
            Token::Characters(_)
                if self.is_one_of(self.current(), &["table", "tbody", "template", "tfoot", "thead", "tr"]) =>
            {
                self.pending_table_text.clear();
                self.original_mode = self.mode;
                self.mode = Mode::InTableText;
                self.in_table_text(tok);
            }
            Token::Comment(c) => self.insert_comment(c, None),
            Token::Doctype(_) => {}
            Token::StartTag(ref t) => match t.name.as_str() {
                "caption" => {
                    self.clear_to_table_context();
                    self.push_marker();
                    self.insert_html_element(t);
                    self.mode = Mode::InCaption;
                }
                "colgroup" => {
                    self.clear_to_table_context();
                    self.insert_html_element(t);
                    self.mode = Mode::InColumnGroup;
                }
                "col" => {
                    self.clear_to_table_context();
                    self.insert_html_element(&tag("colgroup"));
                    self.reprocess(Mode::InColumnGroup, tok);
                }
                "tbody" | "tfoot" | "thead" => {
                    self.clear_to_table_context();
                    self.insert_html_element(t);
                    self.mode = Mode::InTableBody;
                }
                "td" | "th" | "tr" => {
                    self.clear_to_table_context();
                    self.insert_html_element(&tag("tbody"));
                    self.reprocess(Mode::InTableBody, tok);
                }
                "table" => {
                    if !self.in_scope("table", Scope::Table) {
                        return;
                    }
                    self.pop_until("table");
                    self.reset_insertion_mode();
                    let m = self.mode;
                    self.process(m, tok);
                }
                "style" | "script" | "template" => self.in_head(tok),
                "input"
                    if t.attrs
                        .iter()
                        .any(|(n, v)| n == "type" && v.eq_ignore_ascii_case("hidden")) =>
                {
                    self.insert_html_element(t);
                    self.pop();
                }
                "form" => {
                    if self.form.is_some() || self.open.iter().any(|&n| self.is(n, "template")) {
                        return;
                    }
                    let f = self.insert_html_element(t);
                    self.form = Some(f);
                    self.pop();
                }
                _ => self.in_table_anything_else(tok),
            },
            Token::EndTag(ref t) => match t.name.as_str() {
                "table" => {
                    if !self.in_scope("table", Scope::Table) {
                        return;
                    }
                    self.pop_until("table");
                    self.reset_insertion_mode();
                }
                "body" | "caption" | "col" | "colgroup" | "html" | "tbody" | "td" | "tfoot" | "th" | "thead" | "tr" => {
                }
                "template" => self.in_head(tok),
                _ => self.in_table_anything_else(tok),
            },
            Token::Eof => self.in_body(tok),
            t => self.in_table_anything_else(t),
        }
    }

    fn in_table_anything_else(&mut self, tok: Token) {
        self.foster_parenting = true;
        self.in_body(tok);
        self.foster_parenting = false;
    }

    fn in_table_text(&mut self, tok: Token) {
        match tok {
            Token::Characters(s) if s == "\0" => {}
            Token::Characters(s) => self.pending_table_text.push(s),
            t => {
                let pending = std::mem::take(&mut self.pending_table_text);
                if pending.iter().any(|s| !all_ws(s)) {
                    for s in pending {
                        self.in_table_anything_else(Token::Characters(s));
                    }
                } else {
                    for s in pending {
                        self.insert_characters(&s);
                    }
                }
                let m = self.original_mode;
                self.reprocess(m, t);
            }
        }
    }

    fn in_caption(&mut self, tok: Token) {
        let closes = match &tok {
            Token::StartTag(t) => matches!(
                t.name.as_str(),
                "caption" | "col" | "colgroup" | "tbody" | "td" | "tfoot" | "th" | "thead" | "tr"
            ),
            Token::EndTag(t) => t.name == "table",
            _ => false,
        };
        match tok {
            Token::EndTag(ref t) if t.name == "caption" => {
                if !self.in_scope("caption", Scope::Table) {
                    return;
                }
                self.generate_implied_end_tags(None);
                self.pop_until("caption");
                self.clear_active_to_last_marker();
                self.mode = Mode::InTable;
            }
            t if closes => {
                if !self.in_scope("caption", Scope::Table) {
                    return;
                }
                self.generate_implied_end_tags(None);
                self.pop_until("caption");
                self.clear_active_to_last_marker();
                self.reprocess(Mode::InTable, t);
            }
            Token::EndTag(ref t)
                if matches!(
                    t.name.as_str(),
                    "body" | "col" | "colgroup" | "html" | "tbody" | "td" | "tfoot" | "th" | "thead" | "tr"
                ) => {}
            t => self.in_body(t),
        }
    }

    fn in_column_group(&mut self, tok: Token) {
        match tok {
            Token::Characters(ref s) if all_ws(s) => self.insert_characters(s),
            Token::Comment(c) => self.insert_comment(c, None),
            Token::Doctype(_) => {}
            Token::StartTag(ref t) if t.name == "html" => self.in_body(tok),
            Token::StartTag(ref t) if t.name == "col" => {
                self.insert_html_element(t);
                self.pop();
            }
            Token::EndTag(ref t) if t.name == "colgroup" => {
                if !self.is(self.current(), "colgroup") {
                    return;
                }
                self.pop();
                self.mode = Mode::InTable;
            }
            Token::EndTag(ref t) if t.name == "col" => {}
            Token::StartTag(ref t) if t.name == "template" => self.in_head(tok),
            Token::EndTag(ref t) if t.name == "template" => self.in_head(tok),
            Token::Eof => self.in_body(tok),
            t => {
                if !self.is(self.current(), "colgroup") {
                    return;
                }
                self.pop();
                self.reprocess(Mode::InTable, t);
            }
        }
    }

    fn in_table_body(&mut self, tok: Token) {
        match tok {
            Token::StartTag(ref t) if t.name == "tr" => {
                self.clear_to_table_body_context();
                self.insert_html_element(t);
                self.mode = Mode::InRow;
            }
            Token::StartTag(ref t) if t.name == "th" || t.name == "td" => {
                self.clear_to_table_body_context();
                self.insert_html_element(&tag("tr"));
                self.reprocess(Mode::InRow, tok);
            }
            Token::EndTag(ref t) if matches!(t.name.as_str(), "tbody" | "tfoot" | "thead") => {
                if !self.in_scope(&t.name, Scope::Table) {
                    return;
                }
                self.clear_to_table_body_context();
                self.pop();
                self.mode = Mode::InTable;
            }
            Token::StartTag(ref t)
                if matches!(
                    t.name.as_str(),
                    "caption" | "col" | "colgroup" | "tbody" | "tfoot" | "thead"
                ) =>
            {
                self.table_body_close_and_reprocess(tok)
            }
            Token::EndTag(ref t) if t.name == "table" => self.table_body_close_and_reprocess(tok),
            Token::EndTag(ref t)
                if matches!(
                    t.name.as_str(),
                    "body" | "caption" | "col" | "colgroup" | "html" | "td" | "th" | "tr"
                ) => {}
            t => self.in_table(t),
        }
    }

    fn table_body_close_and_reprocess(&mut self, tok: Token) {
        if !(self.in_scope("tbody", Scope::Table)
            || self.in_scope("thead", Scope::Table)
            || self.in_scope("tfoot", Scope::Table))
        {
            return;
        }
        self.clear_to_table_body_context();
        self.pop();
        self.reprocess(Mode::InTable, tok);
    }

    fn in_row(&mut self, tok: Token) {
        match tok {
            Token::StartTag(ref t) if t.name == "th" || t.name == "td" => {
                self.clear_to_table_row_context();
                self.insert_html_element(t);
                self.mode = Mode::InCell;
                self.push_marker();
            }
            Token::EndTag(ref t) if t.name == "tr" => {
                if !self.in_scope("tr", Scope::Table) {
                    return;
                }
                self.clear_to_table_row_context();
                self.pop();
                self.mode = Mode::InTableBody;
            }
            Token::StartTag(ref t)
                if matches!(
                    t.name.as_str(),
                    "caption" | "col" | "colgroup" | "tbody" | "tfoot" | "thead" | "tr"
                ) =>
            {
                self.row_close_and_reprocess(tok)
            }
            Token::EndTag(ref t) if t.name == "table" => self.row_close_and_reprocess(tok),
            Token::EndTag(ref t) if matches!(t.name.as_str(), "tbody" | "tfoot" | "thead") => {
                if !self.in_scope(&t.name, Scope::Table) {
                    return;
                }
                if !self.in_scope("tr", Scope::Table) {
                    return;
                }
                self.clear_to_table_row_context();
                self.pop();
                self.reprocess(Mode::InTableBody, tok);
            }
            Token::EndTag(ref t)
                if matches!(
                    t.name.as_str(),
                    "body" | "caption" | "col" | "colgroup" | "html" | "td" | "th"
                ) => {}
            t => self.in_table(t),
        }
    }

    fn row_close_and_reprocess(&mut self, tok: Token) {
        if !self.in_scope("tr", Scope::Table) {
            return;
        }
        self.clear_to_table_row_context();
        self.pop();
        self.reprocess(Mode::InTableBody, tok);
    }

    fn close_cell(&mut self) {
        self.generate_implied_end_tags(None);
        self.pop_until_one_of(&["td", "th"]);
        self.clear_active_to_last_marker();
        self.mode = Mode::InRow;
    }

    fn in_cell(&mut self, tok: Token) {
        match tok {
            Token::EndTag(ref t) if t.name == "td" || t.name == "th" => {
                if !self.in_scope(&t.name, Scope::Table) {
                    return;
                }
                self.generate_implied_end_tags(None);
                self.pop_until(&t.name.clone());
                self.clear_active_to_last_marker();
                self.mode = Mode::InRow;
            }
            Token::StartTag(ref t)
                if matches!(
                    t.name.as_str(),
                    "caption" | "col" | "colgroup" | "tbody" | "td" | "tfoot" | "th" | "thead" | "tr"
                ) =>
            {
                if !self.in_scope("td", Scope::Table) && !self.in_scope("th", Scope::Table) {
                    return;
                }
                self.close_cell();
                self.reprocess(Mode::InRow, tok);
            }
            Token::EndTag(ref t) if matches!(t.name.as_str(), "body" | "caption" | "col" | "colgroup" | "html") => {}
            Token::EndTag(ref t) if matches!(t.name.as_str(), "table" | "tbody" | "tfoot" | "thead" | "tr") => {
                if !self.in_scope(&t.name, Scope::Table) {
                    return;
                }
                self.close_cell();
                self.reprocess(Mode::InRow, tok);
            }
            t => self.in_body(t),
        }
    }

    fn in_template(&mut self, tok: Token) {
        match tok {
            Token::Characters(_) | Token::Comment(_) | Token::Doctype(_) => self.in_body(tok),
            Token::StartTag(ref t) => {
                let new_mode = match t.name.as_str() {
                    "base" | "basefont" | "bgsound" | "link" | "meta" | "noframes" | "script" | "style"
                    | "template" | "title" => {
                        self.in_head(tok);
                        return;
                    }
                    "caption" | "colgroup" | "tbody" | "tfoot" | "thead" => Mode::InTable,
                    "col" => Mode::InColumnGroup,
                    "tr" => Mode::InTableBody,
                    "td" | "th" => Mode::InRow,
                    _ => Mode::InBody,
                };
                self.template_modes.pop();
                self.template_modes.push(new_mode);
                self.reprocess(new_mode, tok);
            }
            Token::EndTag(ref t) if t.name == "template" => self.in_head(tok),
            Token::EndTag(_) => {}
            Token::Eof => {
                if !self.open.iter().any(|&n| self.is(n, "template")) {
                    self.stopped = true;
                    return;
                }
                self.pop_until("template");
                self.clear_active_to_last_marker();
                self.template_modes.pop();
                self.reset_insertion_mode();
                let m = self.mode;
                self.process(m, Token::Eof);
            }
        }
    }

    fn after_body(&mut self, tok: Token) {
        match tok {
            Token::Characters(ref s) if all_ws(s) => self.in_body(tok),
            Token::Comment(c) => {
                let html = self.open[0];
                self.insert_comment(c, Some(html));
            }
            Token::Doctype(_) => {}
            Token::StartTag(ref t) if t.name == "html" => self.in_body(tok),
            Token::EndTag(ref t) if t.name == "html" => {
                if self.context.is_some() {
                    return;
                }
                self.mode = Mode::AfterAfterBody;
            }
            Token::Eof => self.stopped = true,
            t => self.reprocess(Mode::InBody, t),
        }
    }

    fn in_frameset(&mut self, tok: Token) {
        match tok {
            Token::Characters(ref s) => {
                let ws: String = s.chars().filter(|&c| is_ws_char(c)).collect();
                if !ws.is_empty() {
                    self.insert_characters(&ws);
                }
            }
            Token::Comment(c) => self.insert_comment(c, None),
            Token::Doctype(_) => {}
            Token::StartTag(ref t) => match t.name.as_str() {
                "html" => self.in_body(tok),
                "frameset" => {
                    self.insert_html_element(t);
                }
                "frame" => {
                    self.insert_html_element(t);
                    self.pop();
                }
                "noframes" => self.in_head(tok),
                _ => {}
            },
            Token::EndTag(ref t) if t.name == "frameset" => {
                if self.open.len() == 1 {
                    return;
                }
                self.pop();
                if self.context.is_none() && !self.is(self.current(), "frameset") {
                    self.mode = Mode::AfterFrameset;
                }
            }
            Token::EndTag(_) => {}
            Token::Eof => self.stopped = true,
        }
    }

    fn after_frameset(&mut self, tok: Token) {
        match tok {
            Token::Characters(ref s) => {
                let ws: String = s.chars().filter(|&c| is_ws_char(c)).collect();
                if !ws.is_empty() {
                    self.insert_characters(&ws);
                }
            }
            Token::Comment(c) => self.insert_comment(c, None),
            Token::StartTag(ref t) if t.name == "html" => self.in_body(tok),
            Token::StartTag(ref t) if t.name == "noframes" => self.in_head(tok),
            Token::EndTag(ref t) if t.name == "html" => self.mode = Mode::AfterAfterFrameset,
            Token::Eof => self.stopped = true,
            _ => {}
        }
    }

    fn after_after_body(&mut self, tok: Token) {
        match tok {
            Token::Comment(c) => self.insert_comment(c, Some(self.doc.root())),
            Token::Doctype(_) => self.in_body(tok),
            Token::Characters(ref s) if all_ws(s) => self.in_body(tok),
            Token::StartTag(ref t) if t.name == "html" => self.in_body(tok),
            Token::Eof => self.stopped = true,
            t => self.reprocess(Mode::InBody, t),
        }
    }

    fn after_after_frameset(&mut self, tok: Token) {
        match tok {
            Token::Comment(c) => self.insert_comment(c, Some(self.doc.root())),
            Token::Doctype(_) => self.in_body(tok),
            Token::Characters(ref s) if all_ws(s) => self.in_body(tok),
            Token::StartTag(ref t) if t.name == "html" => self.in_body(tok),
            Token::StartTag(ref t) if t.name == "noframes" => self.in_head(tok),
            Token::Eof => self.stopped = true,
            _ => {}
        }
    }

    // ------------------------------------------------------------ foreign content

    fn insert_foreign_element_adjusted(&mut self, t: &Tag, ns: Namespace) -> NodeId {
        // Attributes on `t` are already adjusted; build the element with namespaced attrs.
        let attrs = t.attrs.iter().map(|(n, v)| foreign_attr(n, v)).collect();
        let el = self
            .doc
            .create_node(NodeData::Element(ElementData::new(ns, t.name.clone(), attrs)));
        self.insert_node_at_appropriate_place(el);
        self.open.push(el);
        el
    }

    fn process_foreign(&mut self, tok: Token) {
        match tok {
            Token::Characters(s) => {
                if s == "\0" {
                    self.insert_characters("\u{FFFD}");
                } else {
                    self.insert_characters(&s);
                    if !all_ws(&s) {
                        self.frameset_ok = false;
                    }
                }
            }
            Token::Comment(c) => self.insert_comment(c, None),
            Token::Doctype(_) => {}
            Token::StartTag(t) => {
                let breakout = matches!(
                    t.name.as_str(),
                    "b" | "big"
                        | "blockquote"
                        | "body"
                        | "br"
                        | "center"
                        | "code"
                        | "dd"
                        | "div"
                        | "dl"
                        | "dt"
                        | "em"
                        | "embed"
                        | "h1"
                        | "h2"
                        | "h3"
                        | "h4"
                        | "h5"
                        | "h6"
                        | "head"
                        | "hr"
                        | "i"
                        | "img"
                        | "li"
                        | "listing"
                        | "menu"
                        | "meta"
                        | "nobr"
                        | "ol"
                        | "p"
                        | "pre"
                        | "ruby"
                        | "s"
                        | "small"
                        | "span"
                        | "strong"
                        | "strike"
                        | "sub"
                        | "sup"
                        | "table"
                        | "tt"
                        | "u"
                        | "ul"
                        | "var"
                ) || (t.name == "font"
                    && t.attrs
                        .iter()
                        .any(|(n, _)| matches!(n.as_str(), "color" | "face" | "size")));
                if breakout {
                    self.pop_until_html_or_integration_point();
                    let m = self.mode;
                    self.process(m, Token::StartTag(t));
                    return;
                }
                let acn = self.adjusted_current_node().unwrap();
                let ns = self.ns(acn);
                let mut t = t;
                if ns == Namespace::Svg {
                    t.name = adjust_svg_tag_name(&t.name).to_string();
                }
                adjust_foreign_attrs(&mut t, ns);
                self.insert_foreign_element_adjusted(&t, ns);
                if t.self_closing {
                    self.pop();
                }
            }
            Token::EndTag(t) => {
                if t.name == "br" || t.name == "p" {
                    self.pop_until_html_or_integration_point();
                    let m = self.mode;
                    self.process(m, Token::EndTag(t));
                    return;
                }
                let cur = self.current();
                if t.name == "script" && self.ns(cur) == Namespace::Svg && self.name(cur) == "script" {
                    self.pop();
                    return;
                }
                let mut i = self.open.len() - 1;
                loop {
                    let node = self.open[i];
                    if i == 0 {
                        return;
                    }
                    if self.name(node).eq_ignore_ascii_case(&t.name) {
                        self.truncate_stack(i);
                        return;
                    }
                    i -= 1;
                    if self.ns(self.open[i]) == Namespace::Html {
                        let m = self.mode;
                        self.process(m, Token::EndTag(t));
                        return;
                    }
                }
            }
            Token::Eof => {
                let m = self.mode;
                self.process(m, Token::Eof);
            }
        }
    }

    fn pop_until_html_or_integration_point(&mut self) {
        while let Some(&n) = self.open.last() {
            if self.ns(n) == Namespace::Html
                || self.is_mathml_text_integration_point(n)
                || self.is_html_integration_point(n)
            {
                break;
            }
            self.pop_raw();
        }
    }
}

fn tag(name: &str) -> Tag {
    Tag {
        name: name.to_string(),
        ..Default::default()
    }
}

fn same_attrs(a: &[(String, String)], b: &[(String, String)]) -> bool {
    a.len() == b.len() && a.iter().all(|x| b.contains(x))
}

fn foreign_attr(name: &str, value: &str) -> Attribute {
    let (ns, local) = match name {
        "xlink:actuate" | "xlink:arcrole" | "xlink:href" | "xlink:role" | "xlink:show" | "xlink:title"
        | "xlink:type" => (Some(AttrNamespace::XLink), &name[6..]),
        "xml:lang" | "xml:space" => (Some(AttrNamespace::Xml), &name[4..]),
        "xmlns" => (Some(AttrNamespace::Xmlns), "xmlns"),
        "xmlns:xlink" => (Some(AttrNamespace::Xmlns), "xlink"),
        _ => (None, name),
    };
    Attribute {
        ns,
        name: local.to_string(),
        value: value.to_string(),
    }
}

fn adjust_foreign_attrs(t: &mut Tag, ns: Namespace) {
    for (name, _) in t.attrs.iter_mut() {
        match ns {
            Namespace::MathMl if name == "definitionurl" => *name = "definitionURL".to_string(),
            Namespace::Svg => {
                if let Some(adj) = adjust_svg_attr(name) {
                    *name = adj.to_string();
                }
            }
            _ => {}
        }
    }
}

fn adjust_svg_attr(name: &str) -> Option<&'static str> {
    Some(match name {
        "attributename" => "attributeName",
        "attributetype" => "attributeType",
        "basefrequency" => "baseFrequency",
        "baseprofile" => "baseProfile",
        "calcmode" => "calcMode",
        "clippathunits" => "clipPathUnits",
        "diffuseconstant" => "diffuseConstant",
        "edgemode" => "edgeMode",
        "filterunits" => "filterUnits",
        "glyphref" => "glyphRef",
        "gradienttransform" => "gradientTransform",
        "gradientunits" => "gradientUnits",
        "kernelmatrix" => "kernelMatrix",
        "kernelunitlength" => "kernelUnitLength",
        "keypoints" => "keyPoints",
        "keysplines" => "keySplines",
        "keytimes" => "keyTimes",
        "lengthadjust" => "lengthAdjust",
        "limitingconeangle" => "limitingConeAngle",
        "markerheight" => "markerHeight",
        "markerunits" => "markerUnits",
        "markerwidth" => "markerWidth",
        "maskcontentunits" => "maskContentUnits",
        "maskunits" => "maskUnits",
        "numoctaves" => "numOctaves",
        "pathlength" => "pathLength",
        "patterncontentunits" => "patternContentUnits",
        "patterntransform" => "patternTransform",
        "patternunits" => "patternUnits",
        "pointsatx" => "pointsAtX",
        "pointsaty" => "pointsAtY",
        "pointsatz" => "pointsAtZ",
        "preservealpha" => "preserveAlpha",
        "preserveaspectratio" => "preserveAspectRatio",
        "primitiveunits" => "primitiveUnits",
        "refx" => "refX",
        "refy" => "refY",
        "repeatcount" => "repeatCount",
        "repeatdur" => "repeatDur",
        "requiredextensions" => "requiredExtensions",
        "requiredfeatures" => "requiredFeatures",
        "specularconstant" => "specularConstant",
        "specularexponent" => "specularExponent",
        "spreadmethod" => "spreadMethod",
        "startoffset" => "startOffset",
        "stddeviation" => "stdDeviation",
        "stitchtiles" => "stitchTiles",
        "surfacescale" => "surfaceScale",
        "systemlanguage" => "systemLanguage",
        "tablevalues" => "tableValues",
        "targetx" => "targetX",
        "targety" => "targetY",
        "textlength" => "textLength",
        "viewbox" => "viewBox",
        "viewtarget" => "viewTarget",
        "xchannelselector" => "xChannelSelector",
        "ychannelselector" => "yChannelSelector",
        "zoomandpan" => "zoomAndPan",
        _ => return None,
    })
}

fn adjust_svg_tag_name(name: &str) -> &str {
    match name {
        "altglyph" => "altGlyph",
        "altglyphdef" => "altGlyphDef",
        "altglyphitem" => "altGlyphItem",
        "animatecolor" => "animateColor",
        "animatemotion" => "animateMotion",
        "animatetransform" => "animateTransform",
        "clippath" => "clipPath",
        "feblend" => "feBlend",
        "fecolormatrix" => "feColorMatrix",
        "fecomponenttransfer" => "feComponentTransfer",
        "fecomposite" => "feComposite",
        "feconvolvematrix" => "feConvolveMatrix",
        "fediffuselighting" => "feDiffuseLighting",
        "fedisplacementmap" => "feDisplacementMap",
        "fedistantlight" => "feDistantLight",
        "fedropshadow" => "feDropShadow",
        "feflood" => "feFlood",
        "fefunca" => "feFuncA",
        "fefuncb" => "feFuncB",
        "fefuncg" => "feFuncG",
        "fefuncr" => "feFuncR",
        "fegaussianblur" => "feGaussianBlur",
        "feimage" => "feImage",
        "femerge" => "feMerge",
        "femergenode" => "feMergeNode",
        "femorphology" => "feMorphology",
        "feoffset" => "feOffset",
        "fepointlight" => "fePointLight",
        "fespecularlighting" => "feSpecularLighting",
        "fespotlight" => "feSpotLight",
        "fetile" => "feTile",
        "feturbulence" => "feTurbulence",
        "foreignobject" => "foreignObject",
        "glyphref" => "glyphRef",
        "lineargradient" => "linearGradient",
        "radialgradient" => "radialGradient",
        "textpath" => "textPath",
        other => other,
    }
}

/// Decides the document mode from the DOCTYPE token
/// (<https://html.spec.whatwg.org/multipage/parsing.html#the-initial-insertion-mode>).
fn quirks_mode_for(d: &Doctype) -> QuirksMode {
    const QUIRKY_PREFIXES: &[&str] = &[
        "+//silmaril//dtd html pro v0r11 19970101//",
        "-//as//dtd html 3.0 aswedit + extensions//",
        "-//advasoft ltd//dtd html 3.0 aswedit + extensions//",
        "-//ietf//dtd html 2.0 level 1//",
        "-//ietf//dtd html 2.0 level 2//",
        "-//ietf//dtd html 2.0 strict level 1//",
        "-//ietf//dtd html 2.0 strict level 2//",
        "-//ietf//dtd html 2.0 strict//",
        "-//ietf//dtd html 2.0//",
        "-//ietf//dtd html 2.1e//",
        "-//ietf//dtd html 3.0//",
        "-//ietf//dtd html 3.2 final//",
        "-//ietf//dtd html 3.2//",
        "-//ietf//dtd html 3//",
        "-//ietf//dtd html level 0//",
        "-//ietf//dtd html level 1//",
        "-//ietf//dtd html level 2//",
        "-//ietf//dtd html level 3//",
        "-//ietf//dtd html strict level 0//",
        "-//ietf//dtd html strict level 1//",
        "-//ietf//dtd html strict level 2//",
        "-//ietf//dtd html strict level 3//",
        "-//ietf//dtd html strict//",
        "-//ietf//dtd html//",
        "-//metrius//dtd metrius presentational//",
        "-//microsoft//dtd internet explorer 2.0 html strict//",
        "-//microsoft//dtd internet explorer 2.0 html//",
        "-//microsoft//dtd internet explorer 2.0 tables//",
        "-//microsoft//dtd internet explorer 3.0 html strict//",
        "-//microsoft//dtd internet explorer 3.0 html//",
        "-//microsoft//dtd internet explorer 3.0 tables//",
        "-//netscape comm. corp.//dtd html//",
        "-//netscape comm. corp.//dtd strict html//",
        "-//o'reilly and associates//dtd html 2.0//",
        "-//o'reilly and associates//dtd html extended 1.0//",
        "-//o'reilly and associates//dtd html extended relaxed 1.0//",
        "-//sq//dtd html 2.0 hotmetal + extensions//",
        "-//softquad software//dtd hotmetal pro 6.0::19990601::extensions to html 4.0//",
        "-//softquad//dtd hotmetal pro 4.0::19971010::extensions to html 4.0//",
        "-//spyglass//dtd html 2.0 extended//",
        "-//sun microsystems corp.//dtd hotjava html//",
        "-//sun microsystems corp.//dtd hotjava strict html//",
        "-//w3c//dtd html 3 1995-03-24//",
        "-//w3c//dtd html 3.2 draft//",
        "-//w3c//dtd html 3.2 final//",
        "-//w3c//dtd html 3.2//",
        "-//w3c//dtd html 3.2s draft//",
        "-//w3c//dtd html 4.0 frameset//",
        "-//w3c//dtd html 4.0 transitional//",
        "-//w3c//dtd html experimental 19960712//",
        "-//w3c//dtd html experimental 970421//",
        "-//w3c//dtd w3 html//",
        "-//w3o//dtd w3 html 3.0//",
        "-//webtechs//dtd mozilla html 2.0//",
        "-//webtechs//dtd mozilla html//",
    ];
    if d.force_quirks || d.name.as_deref() != Some("html") {
        return QuirksMode::Quirks;
    }
    let public = d.public_id.as_deref().map(str::to_ascii_lowercase);
    let system = d.system_id.as_deref().map(str::to_ascii_lowercase);
    if let Some(p) = &public {
        if p == "-//w3o//dtd w3 html strict 3.0//en//" || p == "-/w3c/dtd html 4.0 transitional/en" || p == "html" {
            return QuirksMode::Quirks;
        }
        if QUIRKY_PREFIXES.iter().any(|q| p.starts_with(q)) {
            return QuirksMode::Quirks;
        }
        let html401 =
            p.starts_with("-//w3c//dtd html 4.01 frameset//") || p.starts_with("-//w3c//dtd html 4.01 transitional//");
        if system.is_none() && html401 {
            return QuirksMode::Quirks;
        }
        if p.starts_with("-//w3c//dtd xhtml 1.0 frameset//") || p.starts_with("-//w3c//dtd xhtml 1.0 transitional//") {
            return QuirksMode::LimitedQuirks;
        }
        if system.is_some() && html401 {
            return QuirksMode::LimitedQuirks;
        }
    }
    if system.as_deref() == Some("http://www.ibm.com/data/dtd/v11/ibmxhtml1-transitional.dtd") {
        return QuirksMode::Quirks;
    }
    QuirksMode::NoQuirks
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dom::serialize_html5lib;

    fn tree(html: &str) -> String {
        let doc = parse_document(html);
        serialize_html5lib(&doc, doc.root())
    }

    #[test]
    fn infers_html_head_body() {
        assert_eq!(
            tree("<title>t</title>hi"),
            "| <html>\n|   <head>\n|     <title>\n|       \"t\"\n|   <body>\n|     \"hi\"\n"
        );
    }

    #[test]
    fn auto_closes_paragraphs_and_list_items() {
        let t = tree("<!doctype html><p>a<p>b<ul><li>1<li>2</ul>");
        assert!(
            t.contains("|     <p>\n|       \"a\"\n|     <p>\n|       \"b\"\n|     <ul>"),
            "{t}"
        );
        assert!(
            t.contains("|       <li>\n|         \"1\"\n|       <li>\n|         \"2\""),
            "{t}"
        );
    }

    #[test]
    fn adoption_agency_misnested_formatting() {
        let t = tree("<!doctype html><p>1<b>2<i>3</b>4</i>5</p>");
        let expected = "\
| <!DOCTYPE html>
| <html>
|   <head>
|   <body>
|     <p>
|       \"1\"
|       <b>
|         \"2\"
|         <i>
|           \"3\"
|       <i>
|         \"4\"
|       \"5\"
";
        assert_eq!(t, expected);
    }

    #[test]
    fn foster_parents_text_out_of_tables() {
        let t = tree("<!doctype html><table>x<tr><td>y</table>");
        assert!(
            t.contains("|     \"x\"\n|     <table>\n|       <tbody>\n|         <tr>\n|           <td>"),
            "{t}"
        );
    }

    #[test]
    fn quirks_mode_detection() {
        assert_eq!(parse_document("<!DOCTYPE html>").quirks_mode, QuirksMode::NoQuirks);
        assert_eq!(parse_document("<p>no doctype").quirks_mode, QuirksMode::Quirks);
        assert_eq!(
            parse_document(r#"<!DOCTYPE HTML PUBLIC "-//W3C//DTD HTML 4.01 Transitional//EN">"#).quirks_mode,
            QuirksMode::Quirks
        );
        assert_eq!(
            parse_document(
                r#"<!DOCTYPE html PUBLIC "-//W3C//DTD XHTML 1.0 Transitional//EN" "http://www.w3.org/TR/xhtml1/DTD/xhtml1-transitional.dtd">"#
            )
            .quirks_mode,
            QuirksMode::LimitedQuirks
        );
    }

    #[test]
    fn svg_foreign_content() {
        let t = tree("<!doctype html><svg viewbox='0 0 1 1'><foreignobject><p>x</p></foreignobject></svg>");
        assert!(t.contains("<svg svg>"), "{t}");
        assert!(t.contains("viewBox=\"0 0 1 1\""), "{t}");
        assert!(t.contains("<svg foreignObject>"), "{t}");
        assert!(t.contains("<p>"), "{t}");
    }

    #[test]
    fn customizable_select_content() {
        // Since the 2025 "customizable <select>" parser changes, select keeps arbitrary
        // content and a selectedcontent element mirrors the selected option.
        let t = tree(
            "<!doctype html><select><button><selectedcontent></button><option>A<option selected><b>B</b></select>",
        );
        let expected = "\
| <!DOCTYPE html>
| <html>
|   <head>
|   <body>
|     <select>
|       <button>
|         <selectedcontent>
|           <b>
|             \"B\"
|       <option>
|         \"A\"
|       <option>
|         selected=\"\"
|         <b>
|           \"B\"
";
        assert_eq!(t, expected);
    }

    #[test]
    fn fragment_parsing() {
        let (doc, root) = parse_fragment("<td>x</td>", Namespace::Html, "tr", ParseOptions::default());
        assert_eq!(serialize_html5lib(&doc, root), "| <td>\n|   \"x\"\n");
    }
}
