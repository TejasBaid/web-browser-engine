//! The Document Object Model: an arena-allocated node tree.
//!
//! Nodes live in a single `Vec` owned by [`Document`] and refer to each other through
//! [`NodeId`] indices (parent, first/last child, previous/next sibling). This gives
//! O(1) insertion, removal and re-parenting, which the HTML tree builder needs for
//! foster parenting and the adoption agency algorithm, without reference counting or
//! `unsafe`. Detached nodes simply stay in the arena.

mod serialize;

pub use serialize::{serialize_html5lib, serialize_outline};

use std::fmt;

/// Index of a node inside its [`Document`]'s arena.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct NodeId(pub u32);

impl NodeId {
    /// The arena index as a `usize`.
    #[inline]
    pub fn index(self) -> usize {
        self.0 as usize
    }
}

impl fmt::Debug for NodeId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "#{}", self.0)
    }
}

/// Element namespaces the parser can produce.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Namespace {
    /// `http://www.w3.org/1999/xhtml`
    Html,
    /// `http://www.w3.org/2000/svg`
    Svg,
    /// `http://www.w3.org/1998/Math/MathML`
    MathMl,
}

/// Namespaces of foreign attributes (`xlink:href`, `xml:lang`, `xmlns:xlink`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum AttrNamespace {
    /// The XLink namespace.
    XLink,
    /// The XML namespace.
    Xml,
    /// The XMLNS namespace.
    Xmlns,
}

/// A single attribute on an element.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Attribute {
    /// Namespace for adjusted foreign attributes; `None` for ordinary ones.
    pub ns: Option<AttrNamespace>,
    /// Local name (lowercased for HTML elements).
    pub name: String,
    /// Attribute value.
    pub value: String,
}

impl Attribute {
    /// Creates an attribute without a namespace.
    pub fn new(name: impl Into<String>, value: impl Into<String>) -> Self {
        Attribute {
            ns: None,
            name: name.into(),
            value: value.into(),
        }
    }
}

/// Data carried by element nodes.
#[derive(Clone, Debug)]
pub struct ElementData {
    /// Element namespace.
    pub ns: Namespace,
    /// Local name, lowercase for HTML elements (SVG keeps its camelCase names).
    pub name: String,
    /// Attributes in source order.
    pub attrs: Vec<Attribute>,
    /// Class list, cached from the `class` attribute for selector matching.
    pub classes: Vec<String>,
    /// For `<template>`: the document fragment holding its contents.
    pub template_contents: Option<NodeId>,
}

impl ElementData {
    /// Creates element data and precomputes the class list.
    pub fn new(ns: Namespace, name: impl Into<String>, attrs: Vec<Attribute>) -> Self {
        let mut e = ElementData {
            ns,
            name: name.into(),
            attrs,
            classes: Vec::new(),
            template_contents: None,
        };
        e.refresh_caches();
        e
    }

    fn refresh_caches(&mut self) {
        self.classes = self
            .attr("class")
            .map(|c| c.split_ascii_whitespace().map(str::to_string).collect())
            .unwrap_or_default();
    }

    /// Returns the value of the (non-namespaced) attribute `name`.
    pub fn attr(&self, name: &str) -> Option<&str> {
        self.attrs
            .iter()
            .find(|a| a.ns.is_none() && a.name == name)
            .map(|a| a.value.as_str())
    }

    /// Sets (or replaces) an attribute.
    pub fn set_attr(&mut self, name: &str, value: &str) {
        match self.attrs.iter_mut().find(|a| a.ns.is_none() && a.name == name) {
            Some(a) => a.value = value.to_string(),
            None => self.attrs.push(Attribute::new(name, value)),
        }
        if name == "class" {
            self.refresh_caches();
        }
    }

    /// The `id` attribute, if any.
    pub fn id(&self) -> Option<&str> {
        self.attr("id")
    }

    /// Whether the element's class list contains `class`.
    pub fn has_class(&self, class: &str) -> bool {
        self.classes.iter().any(|c| c == class)
    }

    /// Whether this is the HTML element with local name `name`.
    #[inline]
    pub fn is_html(&self, name: &str) -> bool {
        self.ns == Namespace::Html && self.name == name
    }
}

/// The type-specific payload of a node.
#[derive(Clone, Debug)]
pub enum NodeData {
    /// The document root.
    Document,
    /// A document fragment (used for `<template>` contents).
    Fragment,
    /// `<!DOCTYPE name PUBLIC "public" "system">`.
    Doctype {
        /// Doctype name.
        name: String,
        /// Public identifier, if present.
        public_id: Option<String>,
        /// System identifier, if present.
        system_id: Option<String>,
    },
    /// An element.
    Element(ElementData),
    /// A text node.
    Text(String),
    /// A comment.
    Comment(String),
}

/// A node in the arena together with its tree links.
#[derive(Clone, Debug)]
pub struct Node {
    /// Parent node.
    pub parent: Option<NodeId>,
    /// First child.
    pub first_child: Option<NodeId>,
    /// Last child.
    pub last_child: Option<NodeId>,
    /// Previous sibling.
    pub prev_sibling: Option<NodeId>,
    /// Next sibling.
    pub next_sibling: Option<NodeId>,
    /// Node payload.
    pub data: NodeData,
}

/// Document compatibility mode, decided by the doctype.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum QuirksMode {
    /// Standards mode.
    #[default]
    NoQuirks,
    /// Limited quirks ("almost standards") mode.
    LimitedQuirks,
    /// Full quirks mode.
    Quirks,
}

/// An HTML document: the node arena plus document-level state.
#[derive(Clone, Debug)]
pub struct Document {
    nodes: Vec<Node>,
    /// Compatibility mode chosen by the parser.
    pub quirks_mode: QuirksMode,
}

impl Default for Document {
    fn default() -> Self {
        Self::new()
    }
}

impl Document {
    /// The root document node always has id 0.
    pub const ROOT: NodeId = NodeId(0);

    /// Creates an empty document containing only the root node.
    pub fn new() -> Self {
        Document {
            nodes: vec![Node {
                parent: None,
                first_child: None,
                last_child: None,
                prev_sibling: None,
                next_sibling: None,
                data: NodeData::Document,
            }],
            quirks_mode: QuirksMode::NoQuirks,
        }
    }

    /// The document root.
    #[inline]
    pub fn root(&self) -> NodeId {
        Self::ROOT
    }

    /// Number of nodes ever allocated (including detached ones).
    pub fn len(&self) -> usize {
        self.nodes.len()
    }

    /// Whether the document contains only the root node.
    pub fn is_empty(&self) -> bool {
        self.nodes.len() == 1
    }

    /// Borrows a node.
    #[inline]
    pub fn node(&self, id: NodeId) -> &Node {
        &self.nodes[id.index()]
    }

    /// Mutably borrows a node.
    #[inline]
    pub fn node_mut(&mut self, id: NodeId) -> &mut Node {
        &mut self.nodes[id.index()]
    }

    /// Allocates a detached node.
    pub fn create_node(&mut self, data: NodeData) -> NodeId {
        let id = NodeId(self.nodes.len() as u32);
        self.nodes.push(Node {
            parent: None,
            first_child: None,
            last_child: None,
            prev_sibling: None,
            next_sibling: None,
            data,
        });
        id
    }

    /// Allocates a detached HTML element.
    pub fn create_element(&mut self, name: &str, attrs: Vec<Attribute>) -> NodeId {
        self.create_node(NodeData::Element(ElementData::new(Namespace::Html, name, attrs)))
    }

    /// Allocates a detached text node.
    pub fn create_text(&mut self, text: impl Into<String>) -> NodeId {
        self.create_node(NodeData::Text(text.into()))
    }

    /// Element data of `id`, or `None` for non-elements.
    #[inline]
    pub fn element(&self, id: NodeId) -> Option<&ElementData> {
        match &self.nodes[id.index()].data {
            NodeData::Element(e) => Some(e),
            _ => None,
        }
    }

    /// Mutable element data of `id`.
    #[inline]
    pub fn element_mut(&mut self, id: NodeId) -> Option<&mut ElementData> {
        match &mut self.nodes[id.index()].data {
            NodeData::Element(e) => Some(e),
            _ => None,
        }
    }

    /// Text of a text node.
    pub fn text(&self, id: NodeId) -> Option<&str> {
        match &self.nodes[id.index()].data {
            NodeData::Text(t) => Some(t),
            _ => None,
        }
    }

    /// Whether `id` is an HTML element named `name`.
    #[inline]
    pub fn is_html_element(&self, id: NodeId, name: &str) -> bool {
        self.element(id).is_some_and(|e| e.is_html(name))
    }

    /// Lowercase tag name of an element (empty for other nodes).
    pub fn tag_name(&self, id: NodeId) -> &str {
        self.element(id).map(|e| e.name.as_str()).unwrap_or("")
    }

    /// Attribute value on an element.
    pub fn attr(&self, id: NodeId, name: &str) -> Option<&str> {
        self.element(id).and_then(|e| e.attr(name))
    }

    /// Parent node.
    #[inline]
    pub fn parent(&self, id: NodeId) -> Option<NodeId> {
        self.nodes[id.index()].parent
    }

    /// Parent, but only if it is an element.
    pub fn parent_element(&self, id: NodeId) -> Option<NodeId> {
        self.parent(id).filter(|&p| self.element(p).is_some())
    }

    /// First child.
    #[inline]
    pub fn first_child(&self, id: NodeId) -> Option<NodeId> {
        self.nodes[id.index()].first_child
    }

    /// Last child.
    #[inline]
    pub fn last_child(&self, id: NodeId) -> Option<NodeId> {
        self.nodes[id.index()].last_child
    }

    /// Next sibling.
    #[inline]
    pub fn next_sibling(&self, id: NodeId) -> Option<NodeId> {
        self.nodes[id.index()].next_sibling
    }

    /// Previous sibling.
    #[inline]
    pub fn prev_sibling(&self, id: NodeId) -> Option<NodeId> {
        self.nodes[id.index()].prev_sibling
    }

    /// Iterates over the children of `id`.
    pub fn children(&self, id: NodeId) -> Children<'_> {
        Children {
            doc: self,
            next: self.first_child(id),
        }
    }

    /// Iterates over element children of `id`.
    pub fn element_children(&self, id: NodeId) -> impl Iterator<Item = NodeId> + '_ {
        self.children(id).filter(move |&c| self.element(c).is_some())
    }

    /// Iterates over the ancestors of `id`, nearest first (excluding `id`).
    pub fn ancestors(&self, id: NodeId) -> Ancestors<'_> {
        Ancestors {
            doc: self,
            next: self.parent(id),
        }
    }

    /// Pre-order traversal of `id` and all its descendants.
    pub fn descendants(&self, id: NodeId) -> Descendants<'_> {
        Descendants {
            doc: self,
            root: id,
            next: Some(id),
        }
    }

    /// The previous sibling that is an element.
    pub fn prev_element_sibling(&self, id: NodeId) -> Option<NodeId> {
        let mut cur = self.prev_sibling(id);
        while let Some(c) = cur {
            if self.element(c).is_some() {
                return Some(c);
            }
            cur = self.prev_sibling(c);
        }
        None
    }

    /// The next sibling that is an element.
    pub fn next_element_sibling(&self, id: NodeId) -> Option<NodeId> {
        let mut cur = self.next_sibling(id);
        while let Some(c) = cur {
            if self.element(c).is_some() {
                return Some(c);
            }
            cur = self.next_sibling(c);
        }
        None
    }

    /// Detaches `id` from its parent (no-op if already detached).
    pub fn detach(&mut self, id: NodeId) {
        let Node {
            parent,
            prev_sibling: prev,
            next_sibling: next,
            ..
        } = *self.node(id);
        let Some(parent) = parent else { return };
        match prev {
            Some(p) => self.node_mut(p).next_sibling = next,
            None => self.node_mut(parent).first_child = next,
        }
        match next {
            Some(n) => self.node_mut(n).prev_sibling = prev,
            None => self.node_mut(parent).last_child = prev,
        }
        let n = self.node_mut(id);
        n.parent = None;
        n.prev_sibling = None;
        n.next_sibling = None;
    }

    /// Appends `child` as the last child of `parent`, detaching it first.
    pub fn append_child(&mut self, parent: NodeId, child: NodeId) {
        self.detach(child);
        let last = self.node(parent).last_child;
        {
            let c = self.node_mut(child);
            c.parent = Some(parent);
            c.prev_sibling = last;
        }
        match last {
            Some(l) => self.node_mut(l).next_sibling = Some(child),
            None => self.node_mut(parent).first_child = Some(child),
        }
        self.node_mut(parent).last_child = Some(child);
    }

    /// Inserts `child` into `parent` before `reference` (or appends if `None`).
    pub fn insert_before(&mut self, parent: NodeId, child: NodeId, reference: Option<NodeId>) {
        let Some(reference) = reference else {
            self.append_child(parent, child);
            return;
        };
        self.detach(child);
        let prev = self.node(reference).prev_sibling;
        {
            let c = self.node_mut(child);
            c.parent = Some(parent);
            c.prev_sibling = prev;
            c.next_sibling = Some(reference);
        }
        self.node_mut(reference).prev_sibling = Some(child);
        match prev {
            Some(p) => self.node_mut(p).next_sibling = Some(child),
            None => self.node_mut(parent).first_child = Some(child),
        }
    }

    /// Appends text to `parent`, merging with a trailing text node if there is one.
    pub fn append_text(&mut self, parent: NodeId, text: &str) {
        if let Some(last) = self.last_child(parent)
            && let NodeData::Text(t) = &mut self.node_mut(last).data
        {
            t.push_str(text);
            return;
        }
        let t = self.create_text(text);
        self.append_child(parent, t);
    }

    /// Inserts text before `reference`, merging with the preceding text node if possible.
    pub fn insert_text_before(&mut self, parent: NodeId, text: &str, reference: Option<NodeId>) {
        let Some(r) = reference else {
            self.append_text(parent, text);
            return;
        };
        if let Some(prev) = self.prev_sibling(r)
            && let NodeData::Text(t) = &mut self.node_mut(prev).data
        {
            t.push_str(text);
            return;
        }
        let t = self.create_text(text);
        self.insert_before(parent, t, Some(r));
    }

    /// Moves all children of `from` to the end of `to`.
    pub fn reparent_children(&mut self, from: NodeId, to: NodeId) {
        while let Some(c) = self.first_child(from) {
            self.append_child(to, c);
        }
    }

    /// Deep-clones `id` and its descendants (including template contents) into new,
    /// detached nodes and returns the clone's id.
    pub fn clone_subtree(&mut self, id: NodeId) -> NodeId {
        let mut data = self.node(id).data.clone();
        if let NodeData::Element(e) = &mut data
            && let Some(contents) = e.template_contents
        {
            e.template_contents = Some(self.clone_subtree(contents));
        }
        let clone = self.create_node(data);
        let children: Vec<NodeId> = self.children(id).collect();
        for c in children {
            let cc = self.clone_subtree(c);
            self.append_child(clone, cc);
        }
        clone
    }

    /// Concatenated text of all descendant text nodes.
    pub fn text_content(&self, id: NodeId) -> String {
        let mut s = String::new();
        for n in self.descendants(id) {
            if let NodeData::Text(t) = &self.node(n).data {
                s.push_str(t);
            }
        }
        s
    }

    /// The `<html>` element, if present.
    pub fn document_element(&self) -> Option<NodeId> {
        self.element_children(Self::ROOT).next()
    }

    /// The first `<body>` (or `<frameset>`) child of the document element.
    pub fn body(&self) -> Option<NodeId> {
        let html = self.document_element()?;
        self.element_children(html)
            .find(|&c| self.is_html_element(c, "body") || self.is_html_element(c, "frameset"))
    }

    /// The first `<head>` child of the document element.
    pub fn head(&self) -> Option<NodeId> {
        let html = self.document_element()?;
        self.element_children(html).find(|&c| self.is_html_element(c, "head"))
    }

    /// The text of the first `<title>` element, whitespace-collapsed.
    pub fn title(&self) -> Option<String> {
        let t = self
            .descendants(Self::ROOT)
            .find(|&n| self.is_html_element(n, "title"))?;
        Some(
            self.text_content(t)
                .split_ascii_whitespace()
                .collect::<Vec<_>>()
                .join(" "),
        )
    }

    /// Finds the first element (in tree order) with the given `id` attribute.
    pub fn element_by_id(&self, id: &str) -> Option<NodeId> {
        self.descendants(Self::ROOT)
            .find(|&n| self.element(n).and_then(|e| e.id()) == Some(id))
    }

    /// All elements with the given HTML tag name, in tree order.
    pub fn elements_by_tag(&self, name: &str) -> Vec<NodeId> {
        self.descendants(Self::ROOT)
            .filter(|&n| self.is_html_element(n, name))
            .collect()
    }
}

/// Iterator over a node's children.
pub struct Children<'a> {
    doc: &'a Document,
    next: Option<NodeId>,
}

impl Iterator for Children<'_> {
    type Item = NodeId;
    fn next(&mut self) -> Option<NodeId> {
        let cur = self.next?;
        self.next = self.doc.next_sibling(cur);
        Some(cur)
    }
}

/// Iterator over a node's ancestors.
pub struct Ancestors<'a> {
    doc: &'a Document,
    next: Option<NodeId>,
}

impl Iterator for Ancestors<'_> {
    type Item = NodeId;
    fn next(&mut self) -> Option<NodeId> {
        let cur = self.next?;
        self.next = self.doc.parent(cur);
        Some(cur)
    }
}

/// Pre-order iterator over a subtree.
pub struct Descendants<'a> {
    doc: &'a Document,
    root: NodeId,
    next: Option<NodeId>,
}

impl Iterator for Descendants<'_> {
    type Item = NodeId;
    fn next(&mut self) -> Option<NodeId> {
        let cur = self.next?;
        self.next = if let Some(c) = self.doc.first_child(cur) {
            Some(c)
        } else {
            let mut n = cur;
            loop {
                if n == self.root {
                    break None;
                }
                if let Some(s) = self.doc.next_sibling(n) {
                    break Some(s);
                }
                match self.doc.parent(n) {
                    Some(p) => n = p,
                    None => break None,
                }
            }
        };
        Some(cur)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(doc: &Document, parent: NodeId) -> Vec<String> {
        doc.children(parent).map(|c| doc.tag_name(c).to_string()).collect()
    }

    #[test]
    fn append_insert_detach() {
        let mut d = Document::new();
        let root = d.root();
        let a = d.create_element("a", vec![]);
        let b = d.create_element("b", vec![]);
        let c = d.create_element("c", vec![]);
        d.append_child(root, a);
        d.append_child(root, c);
        d.insert_before(root, b, Some(c));
        assert_eq!(names(&d, root), ["a", "b", "c"]);
        d.detach(b);
        assert_eq!(names(&d, root), ["a", "c"]);
        assert_eq!(d.parent(b), None);
        d.append_child(a, b);
        d.append_child(root, b); // moving re-links correctly
        assert_eq!(names(&d, root), ["a", "c", "b"]);
        assert_eq!(d.first_child(a), None);
        assert_eq!(d.prev_sibling(b), Some(c));
    }

    #[test]
    fn clone_subtree_is_deep_and_detached() {
        let mut d = Document::new();
        let root = d.root();
        let p = d.create_element("p", vec![Attribute::new("id", "x")]);
        let b = d.create_element("b", vec![]);
        d.append_child(root, p);
        d.append_child(p, b);
        d.append_text(b, "hi");
        let c = d.clone_subtree(p);
        assert_ne!(c, p);
        assert_eq!(d.parent(c), None);
        assert_eq!(d.attr(c, "id"), Some("x"));
        assert_eq!(d.text_content(c), "hi");
        let cb = d.first_child(c).unwrap();
        assert_ne!(cb, b);
        assert_eq!(d.tag_name(cb), "b");
    }

    #[test]
    fn traversal_orders() {
        let mut d = Document::new();
        let root = d.root();
        let html = d.create_element("html", vec![]);
        let body = d.create_element("body", vec![]);
        let p = d.create_element("p", vec![Attribute::new("class", " x  y ")]);
        d.append_child(root, html);
        d.append_child(html, body);
        d.append_child(body, p);
        d.append_text(p, "hello ");
        d.append_text(p, "world");
        assert_eq!(d.children(p).count(), 1, "adjacent text merges");
        assert_eq!(d.text_content(root), "hello world");
        let order: Vec<_> = d.descendants(root).map(|n| d.tag_name(n).to_string()).collect();
        assert_eq!(order, ["", "html", "body", "p", ""]);
        assert_eq!(d.ancestors(p).count(), 3);
        assert!(d.element(p).unwrap().has_class("y"));
        assert_eq!(d.body(), Some(body));
    }
}
