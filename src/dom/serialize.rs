//! Text serializations of the DOM used by tests and the `dump-dom` command.

use super::{AttrNamespace, Document, Namespace, NodeData, NodeId};
use std::fmt::Write;

/// Serializes the children of `root` in the html5lib-tests tree format:
///
/// ```text
/// | <html>
/// |   <head>
/// |   <body>
/// |     class="x"
/// |     "text"
/// ```
pub fn serialize_html5lib(doc: &Document, root: NodeId) -> String {
    let mut out = String::new();
    for c in doc.children(root) {
        write_html5lib(doc, c, 0, &mut out);
    }
    out
}

fn write_html5lib(doc: &Document, id: NodeId, depth: usize, out: &mut String) {
    let indent = "  ".repeat(depth);
    match &doc.node(id).data {
        NodeData::Document | NodeData::Fragment => {}
        NodeData::Doctype {
            name,
            public_id,
            system_id,
        } => {
            if public_id.is_some() || system_id.is_some() {
                let _ = writeln!(
                    out,
                    "| {indent}<!DOCTYPE {name} \"{}\" \"{}\">",
                    public_id.as_deref().unwrap_or(""),
                    system_id.as_deref().unwrap_or("")
                );
            } else {
                let _ = writeln!(out, "| {indent}<!DOCTYPE {name}>");
            }
        }
        NodeData::Comment(c) => {
            let _ = writeln!(out, "| {indent}<!-- {c} -->");
        }
        NodeData::Text(t) => {
            let _ = writeln!(out, "| {indent}\"{t}\"");
        }
        NodeData::Element(e) => {
            let prefix = match e.ns {
                Namespace::Html => "",
                Namespace::Svg => "svg ",
                Namespace::MathMl => "math ",
            };
            let _ = writeln!(out, "| {indent}<{prefix}{}>", e.name);
            let mut attrs: Vec<(String, &str)> = e
                .attrs
                .iter()
                .map(|a| {
                    let name = match a.ns {
                        None => a.name.clone(),
                        Some(AttrNamespace::XLink) => format!("xlink {}", a.name),
                        Some(AttrNamespace::Xml) => format!("xml {}", a.name),
                        Some(AttrNamespace::Xmlns) => format!("xmlns {}", a.name),
                    };
                    (name, a.value.as_str())
                })
                .collect();
            attrs.sort();
            for (name, value) in attrs {
                let _ = writeln!(out, "| {indent}  {name}=\"{value}\"");
            }
            if let Some(contents) = e.template_contents {
                let _ = writeln!(out, "| {indent}  content");
                for c in doc.children(contents) {
                    write_html5lib(doc, c, depth + 2, out);
                }
            }
        }
    }
    for c in doc.children(id) {
        write_html5lib(doc, c, depth + 1, out);
    }
}

/// A compact, human-readable outline of the tree, for `browser dump-dom`.
/// Whitespace-only text nodes are omitted and long text is truncated.
pub fn serialize_outline(doc: &Document) -> String {
    let mut out = String::new();
    write_outline(doc, doc.root(), 0, &mut out);
    out
}

fn write_outline(doc: &Document, id: NodeId, depth: usize, out: &mut String) {
    let indent = "  ".repeat(depth);
    match &doc.node(id).data {
        NodeData::Document => {
            let _ = writeln!(out, "#document ({:?})", doc.quirks_mode);
        }
        NodeData::Fragment => {
            let _ = writeln!(out, "{indent}#fragment");
        }
        NodeData::Doctype { name, .. } => {
            let _ = writeln!(out, "{indent}<!DOCTYPE {name}>");
        }
        NodeData::Comment(c) => {
            let _ = writeln!(out, "{indent}<!--{}-->", truncate(c, 60));
        }
        NodeData::Text(t) => {
            let collapsed: Vec<&str> = t.split_ascii_whitespace().collect();
            if !collapsed.is_empty() {
                let _ = writeln!(out, "{indent}\"{}\"", truncate(&collapsed.join(" "), 80));
            }
        }
        NodeData::Element(e) => {
            let _ = write!(out, "{indent}<{}", e.name);
            for a in &e.attrs {
                let _ = write!(out, " {}=\"{}\"", a.name, truncate(&a.value, 60));
            }
            let _ = writeln!(out, ">");
            if let Some(contents) = e.template_contents {
                write_outline(doc, contents, depth + 1, out);
            }
        }
    }
    let child_depth = if matches!(doc.node(id).data, NodeData::Document) {
        0
    } else {
        depth + 1
    };
    for c in doc.children(id) {
        write_outline(doc, c, child_depth, out);
    }
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let mut t: String = s.chars().take(max).collect();
        t.push('…');
        t
    }
}
