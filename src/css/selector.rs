//! Selectors Level 4 (subset): parsing, specificity and matching against the DOM.
//!
//! Complex selectors are stored right-to-left so matching starts at the subject element
//! and walks up/left through the combinators, which lets most candidates fail on the
//! first (rightmost) compound.

use super::parser::{ComponentValue, to_css};
use super::tokenizer::Token;
use crate::dom::{Document, Namespace, NodeData, NodeId};

/// Attribute selector operators.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AttrOp {
    /// `[a]`
    Exists,
    /// `[a=v]`
    Equals,
    /// `[a~=v]` whitespace-separated list contains `v`
    Includes,
    /// `[a|=v]` equals `v` or starts with `v-`
    DashMatch,
    /// `[a^=v]`
    Prefix,
    /// `[a$=v]`
    Suffix,
    /// `[a*=v]`
    Substring,
}

/// Pseudo-classes understood by the engine.
#[derive(Clone, Debug, PartialEq)]
pub enum PseudoClass {
    /// `:first-child`
    FirstChild,
    /// `:last-child`
    LastChild,
    /// `:only-child`
    OnlyChild,
    /// `:first-of-type`
    FirstOfType,
    /// `:last-of-type`
    LastOfType,
    /// `:only-of-type`
    OnlyOfType,
    /// `:nth-child(an+b [of S])`
    NthChild(i32, i32, Option<Box<SelectorList>>),
    /// `:nth-last-child(an+b [of S])`
    NthLastChild(i32, i32, Option<Box<SelectorList>>),
    /// `:nth-of-type(an+b)`
    NthOfType(i32, i32),
    /// `:nth-last-of-type(an+b)`
    NthLastOfType(i32, i32),
    /// `:not(list)`
    Not(Box<SelectorList>),
    /// `:is(list)` / `:matches()` / `:-webkit-any()`
    Is(Box<SelectorList>),
    /// `:where(list)` (zero specificity)
    Where(Box<SelectorList>),
    /// `:root`
    Root,
    /// `:empty`
    Empty,
    /// `:link` / `:any-link`: `a`/`area` with `href`
    Link,
    /// `:checked`
    Checked,
    /// `:disabled`
    Disabled,
    /// `:enabled`
    Enabled,
    /// Dynamic or unsupported-but-valid states that never match in a static render
    /// (`:hover`, `:focus`, `:visited`, `:target`, ...).
    Never,
}

/// Pseudo-elements that generate boxes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum PseudoElement {
    /// `::before`
    Before,
    /// `::after`
    After,
    /// `::marker`, `::first-line`, `::selection`, ...: parsed but never applied.
    Unsupported,
}

/// A simple selector.
#[derive(Clone, Debug, PartialEq)]
pub enum Simple {
    /// `*`
    Universal,
    /// `div` (stored lowercase)
    Type(String),
    /// `#id`
    Id(String),
    /// `.class`
    Class(String),
    /// `[name op value i]`
    Attr {
        /// Attribute name (lowercase).
        name: String,
        /// Operator.
        op: AttrOp,
        /// Comparison value.
        value: String,
        /// The `i` flag: ASCII case-insensitive value comparison.
        case_insensitive: bool,
    },
    /// A pseudo-class.
    Pseudo(PseudoClass),
}

/// Combinators between compound selectors.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Combinator {
    /// whitespace
    Descendant,
    /// `>`
    Child,
    /// `+`
    NextSibling,
    /// `~`
    SubsequentSibling,
}

/// A complex selector such as `ul > li.item:first-child::before`.
#[derive(Clone, Debug, PartialEq)]
pub struct Selector {
    /// Compound selectors from right (subject) to left.
    pub compounds: Vec<Vec<Simple>>,
    /// `combinators[i]` joins `compounds[i]` to `compounds[i + 1]` (its left neighbour).
    pub combinators: Vec<Combinator>,
    /// Pseudo-element on the subject, if any.
    pub pseudo_element: Option<PseudoElement>,
    /// Specificity packed as `a << 20 | b << 10 | c`.
    pub specificity: u32,
}

/// A comma-separated selector list.
pub type SelectorList = Vec<Selector>;

/// Packs specificity components.
pub fn pack_specificity(a: u32, b: u32, c: u32) -> u32 {
    (a.min(1023) << 20) | (b.min(1023) << 10) | c.min(1023)
}

/// Unpacks specificity into `(a, b, c)`.
pub fn unpack_specificity(s: u32) -> (u32, u32, u32) {
    (s >> 20, (s >> 10) & 1023, s & 1023)
}

// ------------------------------------------------------------------ parsing

/// Parses a selector list from text. Returns `None` if any selector is invalid
/// (per spec, an invalid selector invalidates the whole list and its rule).
pub fn parse_selector_list_str(text: &str) -> Option<SelectorList> {
    parse_selector_list(&super::parser::parse_component_values(text))
}

/// Parses a selector list from component values (a rule prelude).
pub fn parse_selector_list(values: &[ComponentValue]) -> Option<SelectorList> {
    let mut list = Vec::new();
    for part in values.split(|v| matches!(v, ComponentValue::Token(Token::Comma))) {
        list.push(parse_complex(part)?);
    }
    if list.is_empty() { None } else { Some(list) }
}

/// Like [`parse_selector_list`] but drops invalid entries instead of failing (the
/// "forgiving" parsing used by `:is()` and `:where()`).
fn parse_forgiving_list(values: &[ComponentValue]) -> SelectorList {
    values
        .split(|v| matches!(v, ComponentValue::Token(Token::Comma)))
        .filter_map(parse_complex)
        .collect()
}

fn parse_complex(values: &[ComponentValue]) -> Option<Selector> {
    // Left-to-right parse, reversed at the end.
    let mut compounds: Vec<Vec<Simple>> = Vec::new();
    let mut combinators: Vec<Combinator> = Vec::new();
    let mut pseudo_element = None;
    let mut i = 0;
    let n = values.len();
    let skip_ws = |i: &mut usize| {
        while *i < n && values[*i].is_whitespace() {
            *i += 1;
        }
    };
    skip_ws(&mut i);
    if i >= n {
        return None;
    }
    loop {
        if pseudo_element.is_some() {
            return None; // nothing may follow a pseudo-element
        }
        let compound = parse_compound(values, &mut i, &mut pseudo_element)?;
        compounds.push(compound);
        // Combinator or end.
        let had_ws = i < n && values[i].is_whitespace();
        skip_ws(&mut i);
        if i >= n {
            break;
        }
        let comb = match &values[i] {
            ComponentValue::Token(Token::Delim('>')) => {
                i += 1;
                Combinator::Child
            }
            ComponentValue::Token(Token::Delim('+')) => {
                i += 1;
                Combinator::NextSibling
            }
            ComponentValue::Token(Token::Delim('~')) => {
                i += 1;
                Combinator::SubsequentSibling
            }
            _ if had_ws => Combinator::Descendant,
            _ => return None,
        };
        combinators.push(comb);
        skip_ws(&mut i);
        if i >= n {
            return None; // dangling combinator
        }
    }
    compounds.reverse();
    combinators.reverse();
    let mut sel = Selector {
        compounds,
        combinators,
        pseudo_element,
        specificity: 0,
    };
    sel.specificity = specificity(&sel);
    Some(sel)
}

fn parse_compound(
    values: &[ComponentValue],
    i: &mut usize,
    pseudo_element: &mut Option<PseudoElement>,
) -> Option<Vec<Simple>> {
    let mut out = Vec::new();
    let n = values.len();
    // Optional type or universal selector first.
    match values.get(*i) {
        Some(ComponentValue::Token(Token::Ident(name))) => {
            out.push(Simple::Type(name.to_ascii_lowercase()));
            *i += 1;
        }
        Some(ComponentValue::Token(Token::Delim('*'))) => {
            out.push(Simple::Universal);
            *i += 1;
        }
        _ => {}
    }
    // Namespace prefix `ns|type` / `*|type`: ignore the namespace.
    if matches!(values.get(*i), Some(ComponentValue::Token(Token::Delim('|')))) {
        *i += 1;
        out.pop();
        match values.get(*i) {
            Some(ComponentValue::Token(Token::Ident(name))) => out.push(Simple::Type(name.to_ascii_lowercase())),
            Some(ComponentValue::Token(Token::Delim('*'))) => out.push(Simple::Universal),
            _ => return None,
        }
        *i += 1;
    }
    while *i < n {
        match &values[*i] {
            ComponentValue::Token(Token::Hash { value, .. }) => {
                if pseudo_element.is_some() {
                    return None;
                }
                out.push(Simple::Id(value.clone()));
                *i += 1;
            }
            ComponentValue::Token(Token::Delim('.')) => {
                match values.get(*i + 1) {
                    Some(ComponentValue::Token(Token::Ident(c))) => out.push(Simple::Class(c.clone())),
                    _ => return None,
                }
                *i += 2;
            }
            ComponentValue::Block { open: '[', contents } => {
                out.push(parse_attr(contents)?);
                *i += 1;
            }
            ComponentValue::Token(Token::Colon) => {
                *i += 1;
                let double = matches!(values.get(*i), Some(ComponentValue::Token(Token::Colon)));
                if double {
                    *i += 1;
                }
                match values.get(*i) {
                    Some(ComponentValue::Token(Token::Ident(name))) => {
                        let lname = name.to_ascii_lowercase();
                        *i += 1;
                        if double || matches!(lname.as_str(), "before" | "after" | "first-line" | "first-letter") {
                            *pseudo_element = Some(match lname.as_str() {
                                "before" => PseudoElement::Before,
                                "after" => PseudoElement::After,
                                _ => PseudoElement::Unsupported,
                            });
                        } else {
                            out.push(Simple::Pseudo(parse_pseudo_class_ident(&lname)?));
                        }
                    }
                    Some(ComponentValue::Function { name, args }) => {
                        let lname = name.to_ascii_lowercase();
                        *i += 1;
                        if double {
                            // ::part(), ::slotted(), ::highlight() ...
                            *pseudo_element = Some(PseudoElement::Unsupported);
                        } else {
                            out.push(Simple::Pseudo(parse_pseudo_class_fn(&lname, args)?));
                        }
                    }
                    _ => return None,
                }
            }
            _ => break,
        }
    }
    if out.is_empty() && pseudo_element.is_none() {
        return None;
    }
    if out.is_empty() {
        out.push(Simple::Universal);
    }
    Some(out)
}

fn parse_attr(contents: &[ComponentValue]) -> Option<Simple> {
    let toks: Vec<&ComponentValue> = contents.iter().filter(|v| !v.is_whitespace()).collect();
    let mut k = 0;
    // Optional namespace prefix: `[ns|attr]` / `[*|attr]` / `[|attr]`.
    let name = loop {
        match toks.get(k) {
            Some(ComponentValue::Token(Token::Ident(n))) => {
                if matches!(toks.get(k + 1), Some(ComponentValue::Token(Token::Delim('|'))))
                    && matches!(toks.get(k + 2), Some(ComponentValue::Token(Token::Ident(_))))
                {
                    k += 2;
                    continue;
                }
                break n.to_ascii_lowercase();
            }
            Some(ComponentValue::Token(Token::Delim('*'))) | Some(ComponentValue::Token(Token::Delim('|'))) => {
                k += 1;
            }
            _ => return None,
        }
    };
    k += 1;
    if k >= toks.len() {
        return Some(Simple::Attr {
            name,
            op: AttrOp::Exists,
            value: String::new(),
            case_insensitive: false,
        });
    }
    let op = match toks[k] {
        ComponentValue::Token(Token::Delim('=')) => {
            k += 1;
            AttrOp::Equals
        }
        ComponentValue::Token(Token::Delim(c)) => {
            let op = match c {
                '~' => AttrOp::Includes,
                '|' => AttrOp::DashMatch,
                '^' => AttrOp::Prefix,
                '$' => AttrOp::Suffix,
                '*' => AttrOp::Substring,
                _ => return None,
            };
            if !matches!(toks.get(k + 1), Some(ComponentValue::Token(Token::Delim('=')))) {
                return None;
            }
            k += 2;
            op
        }
        _ => return None,
    };
    let value = match toks.get(k) {
        Some(ComponentValue::Token(Token::Ident(v))) | Some(ComponentValue::Token(Token::String(v))) => v.clone(),
        _ => return None,
    };
    k += 1;
    let mut case_insensitive = false;
    if let Some(ComponentValue::Token(Token::Ident(flag))) = toks.get(k) {
        match flag.to_ascii_lowercase().as_str() {
            "i" => case_insensitive = true,
            "s" => {}
            _ => return None,
        }
        k += 1;
    }
    if k != toks.len() {
        return None;
    }
    Some(Simple::Attr {
        name,
        op,
        value,
        case_insensitive,
    })
}

fn parse_pseudo_class_ident(name: &str) -> Option<PseudoClass> {
    Some(match name {
        "first-child" => PseudoClass::FirstChild,
        "last-child" => PseudoClass::LastChild,
        "only-child" => PseudoClass::OnlyChild,
        "first-of-type" => PseudoClass::FirstOfType,
        "last-of-type" => PseudoClass::LastOfType,
        "only-of-type" => PseudoClass::OnlyOfType,
        "root" => PseudoClass::Root,
        "empty" => PseudoClass::Empty,
        "link" | "any-link" => PseudoClass::Link,
        "checked" => PseudoClass::Checked,
        "disabled" => PseudoClass::Disabled,
        "enabled" => PseudoClass::Enabled,
        "defined" => PseudoClass::Is(Box::new(vec![universal()])),
        "visited" | "hover" | "active" | "focus" | "focus-within" | "focus-visible" | "target" | "target-within"
        | "placeholder-shown" | "autofill" | "invalid" | "user-invalid" | "indeterminate" | "fullscreen" | "modal"
        | "popover-open" | "open" | "closed" | "playing" | "paused" | "read-only" | "read-write" | "required"
        | "optional" | "valid" | "user-valid" | "default" | "in-range" | "out-of-range" | "scope" | "host" => {
            PseudoClass::Never
        }
        n if n.starts_with("-webkit-") || n.starts_with("-moz-") || n.starts_with("-ms-") => return None,
        _ => return None,
    })
}

fn universal() -> Selector {
    Selector {
        compounds: vec![vec![Simple::Universal]],
        combinators: vec![],
        pseudo_element: None,
        specificity: 0,
    }
}

fn parse_pseudo_class_fn(name: &str, args: &[ComponentValue]) -> Option<PseudoClass> {
    match name {
        "not" => Some(PseudoClass::Not(Box::new(parse_selector_list(args)?))),
        "is" | "matches" | "-webkit-any" | "any" => Some(PseudoClass::Is(Box::new(parse_forgiving_list(args)))),
        "where" => Some(PseudoClass::Where(Box::new(parse_forgiving_list(args)))),
        "nth-child" | "nth-last-child" => {
            // Split off an optional `of <selector-list>`.
            let of_pos = args
                .iter()
                .position(|v| v.ident().is_some_and(|s| s.eq_ignore_ascii_case("of")));
            let (anb_part, of) = match of_pos {
                Some(p) => (&args[..p], Some(Box::new(parse_selector_list(&args[p + 1..])?))),
                None => (args, None),
            };
            let (a, b) = parse_an_plus_b(&to_css(anb_part))?;
            Some(if name == "nth-child" {
                PseudoClass::NthChild(a, b, of)
            } else {
                PseudoClass::NthLastChild(a, b, of)
            })
        }
        "nth-of-type" => {
            let (a, b) = parse_an_plus_b(&to_css(args))?;
            Some(PseudoClass::NthOfType(a, b))
        }
        "nth-last-of-type" => {
            let (a, b) = parse_an_plus_b(&to_css(args))?;
            Some(PseudoClass::NthLastOfType(a, b))
        }
        "lang" | "dir" | "state" | "host" | "host-context" => Some(PseudoClass::Never),
        _ => None,
    }
}

/// Parses the `An+B` microsyntax (`odd`, `even`, `3`, `-n+2`, `2n + 1`, ...).
pub fn parse_an_plus_b(text: &str) -> Option<(i32, i32)> {
    let s: String = text
        .chars()
        .filter(|c| !c.is_whitespace())
        .collect::<String>()
        .to_ascii_lowercase();
    match s.as_str() {
        "odd" => return Some((2, 1)),
        "even" => return Some((2, 0)),
        "" => return None,
        _ => {}
    }
    if let Some(npos) = s.find('n') {
        let a_part = &s[..npos];
        let a = match a_part {
            "" | "+" => 1,
            "-" => -1,
            p => p.parse::<i32>().ok()?,
        };
        let rest = &s[npos + 1..];
        let b = if rest.is_empty() {
            0
        } else {
            if !(rest.starts_with('+') || rest.starts_with('-')) {
                return None;
            }
            let digits = &rest[1..];
            if digits.is_empty() || !digits.chars().all(|c| c.is_ascii_digit()) {
                return None;
            }
            rest.parse::<i32>().ok()?
        };
        Some((a, b))
    } else {
        let t = s.strip_prefix('+').unwrap_or(&s);
        if t.is_empty() || !t.trim_start_matches('-').chars().all(|c| c.is_ascii_digit()) {
            return None;
        }
        Some((0, s.parse::<i32>().ok()?))
    }
}

// --------------------------------------------------------------- specificity

fn specificity(sel: &Selector) -> u32 {
    let (mut a, mut b, mut c) = (0u32, 0u32, 0u32);
    for compound in &sel.compounds {
        for s in compound {
            let (x, y, z) = simple_specificity(s);
            a += x;
            b += y;
            c += z;
        }
    }
    if matches!(
        sel.pseudo_element,
        Some(PseudoElement::Before) | Some(PseudoElement::After) | Some(PseudoElement::Unsupported)
    ) {
        c += 1;
    }
    pack_specificity(a, b, c)
}

fn max_list_specificity(list: &SelectorList) -> (u32, u32, u32) {
    list.iter()
        .map(|s| s.specificity)
        .max()
        .map(unpack_specificity)
        .unwrap_or((0, 0, 0))
}

fn simple_specificity(s: &Simple) -> (u32, u32, u32) {
    match s {
        Simple::Universal => (0, 0, 0),
        Simple::Type(_) => (0, 0, 1),
        Simple::Id(_) => (1, 0, 0),
        Simple::Class(_) | Simple::Attr { .. } => (0, 1, 0),
        Simple::Pseudo(p) => match p {
            PseudoClass::Not(l) | PseudoClass::Is(l) => max_list_specificity(l),
            PseudoClass::Where(_) => (0, 0, 0),
            PseudoClass::NthChild(_, _, Some(l)) | PseudoClass::NthLastChild(_, _, Some(l)) => {
                let (a, b, c) = max_list_specificity(l);
                (a, b + 1, c)
            }
            _ => (0, 1, 0),
        },
    }
}

// ------------------------------------------------------------------ matching

impl Selector {
    /// Whether the selector's subject matches `el` (ignoring any pseudo-element).
    pub fn matches(&self, doc: &Document, el: NodeId) -> bool {
        matches_from(self, 0, doc, el)
    }

    /// The rightmost compound, used to bucket rules by id/class/tag.
    pub fn subject(&self) -> &[Simple] {
        &self.compounds[0]
    }
}

/// Whether any selector in the list matches `el`.
pub fn list_matches(list: &SelectorList, doc: &Document, el: NodeId) -> bool {
    list.iter().any(|s| s.pseudo_element.is_none() && s.matches(doc, el))
}

fn matches_from(sel: &Selector, idx: usize, doc: &Document, el: NodeId) -> bool {
    if !matches_compound(&sel.compounds[idx], doc, el) {
        return false;
    }
    if idx + 1 == sel.compounds.len() {
        return true;
    }
    match sel.combinators[idx] {
        Combinator::Child => doc
            .parent_element(el)
            .is_some_and(|p| matches_from(sel, idx + 1, doc, p)),
        Combinator::Descendant => {
            let mut cur = doc.parent_element(el);
            while let Some(p) = cur {
                if matches_from(sel, idx + 1, doc, p) {
                    return true;
                }
                cur = doc.parent_element(p);
            }
            false
        }
        Combinator::NextSibling => doc
            .prev_element_sibling(el)
            .is_some_and(|s| matches_from(sel, idx + 1, doc, s)),
        Combinator::SubsequentSibling => {
            let mut cur = doc.prev_element_sibling(el);
            while let Some(s) = cur {
                if matches_from(sel, idx + 1, doc, s) {
                    return true;
                }
                cur = doc.prev_element_sibling(s);
            }
            false
        }
    }
}

fn matches_compound(compound: &[Simple], doc: &Document, el: NodeId) -> bool {
    compound.iter().all(|s| matches_simple(s, doc, el))
}

fn matches_simple(s: &Simple, doc: &Document, el: NodeId) -> bool {
    let Some(e) = doc.element(el) else { return false };
    match s {
        Simple::Universal => true,
        Simple::Type(t) => e.name.eq_ignore_ascii_case(t),
        Simple::Id(id) => e.id() == Some(id.as_str()),
        Simple::Class(c) => e.has_class(c),
        Simple::Attr {
            name,
            op,
            value,
            case_insensitive,
        } => {
            let Some(actual) = e.attrs.iter().find(|a| a.name == *name).map(|a| a.value.as_str()) else {
                return false;
            };
            let ci = *case_insensitive || (e.ns == Namespace::Html && is_case_insensitive_attr(name));
            attr_matches(*op, actual, value, ci)
        }
        Simple::Pseudo(p) => matches_pseudo(p, doc, el),
    }
}

/// HTML attributes whose values are matched case-insensitively by selectors.
fn is_case_insensitive_attr(name: &str) -> bool {
    matches!(
        name,
        "type"
            | "align"
            | "valign"
            | "dir"
            | "lang"
            | "method"
            | "rel"
            | "scope"
            | "shape"
            | "target"
            | "nowrap"
            | "checked"
            | "disabled"
            | "selected"
            | "multiple"
            | "readonly"
            | "bgcolor"
            | "color"
            | "face"
            | "frame"
            | "rules"
            | "media"
            | "charset"
            | "clear"
            | "valuetype"
            | "axis"
            | "language"
            | "link"
            | "text"
            | "vlink"
            | "alink"
    )
}

fn attr_matches(op: AttrOp, actual: &str, value: &str, ci: bool) -> bool {
    let (actual, value) = if ci {
        (actual.to_ascii_lowercase(), value.to_ascii_lowercase())
    } else {
        (actual.to_string(), value.to_string())
    };
    match op {
        AttrOp::Exists => true,
        AttrOp::Equals => actual == value,
        AttrOp::Includes => !value.is_empty() && actual.split_ascii_whitespace().any(|w| w == value),
        AttrOp::DashMatch => actual == value || actual.starts_with(&format!("{value}-")),
        AttrOp::Prefix => !value.is_empty() && actual.starts_with(&value),
        AttrOp::Suffix => !value.is_empty() && actual.ends_with(&value),
        AttrOp::Substring => !value.is_empty() && actual.contains(&value),
    }
}

fn nth_matches(a: i32, b: i32, index: i32) -> bool {
    // index is 1-based; is there n >= 0 with a*n + b == index?
    if a == 0 {
        index == b
    } else {
        let diff = index - b;
        diff % a == 0 && diff / a >= 0
    }
}

fn same_type(doc: &Document, a: NodeId, b: NodeId) -> bool {
    match (doc.element(a), doc.element(b)) {
        (Some(x), Some(y)) => x.name == y.name && x.ns == y.ns,
        _ => false,
    }
}

fn matches_pseudo(p: &PseudoClass, doc: &Document, el: NodeId) -> bool {
    match p {
        PseudoClass::FirstChild => doc.prev_element_sibling(el).is_none() && has_element_parent_or_root(doc, el),
        PseudoClass::LastChild => doc.next_element_sibling(el).is_none() && has_element_parent_or_root(doc, el),
        PseudoClass::OnlyChild => doc.prev_element_sibling(el).is_none() && doc.next_element_sibling(el).is_none(),
        PseudoClass::FirstOfType => !siblings_before(doc, el).any(|s| same_type(doc, s, el)),
        PseudoClass::LastOfType => !siblings_after(doc, el).any(|s| same_type(doc, s, el)),
        PseudoClass::OnlyOfType => {
            !siblings_before(doc, el).any(|s| same_type(doc, s, el))
                && !siblings_after(doc, el).any(|s| same_type(doc, s, el))
        }
        PseudoClass::NthChild(a, b, of) => {
            if let Some(of) = of
                && !list_matches(of, doc, el)
            {
                return false;
            }
            let idx = 1 + siblings_before(doc, el)
                .filter(|&s| of.as_ref().is_none_or(|l| list_matches(l, doc, s)))
                .count() as i32;
            nth_matches(*a, *b, idx)
        }
        PseudoClass::NthLastChild(a, b, of) => {
            if let Some(of) = of
                && !list_matches(of, doc, el)
            {
                return false;
            }
            let idx = 1 + siblings_after(doc, el)
                .filter(|&s| of.as_ref().is_none_or(|l| list_matches(l, doc, s)))
                .count() as i32;
            nth_matches(*a, *b, idx)
        }
        PseudoClass::NthOfType(a, b) => {
            let idx = 1 + siblings_before(doc, el).filter(|&s| same_type(doc, s, el)).count() as i32;
            nth_matches(*a, *b, idx)
        }
        PseudoClass::NthLastOfType(a, b) => {
            let idx = 1 + siblings_after(doc, el).filter(|&s| same_type(doc, s, el)).count() as i32;
            nth_matches(*a, *b, idx)
        }
        PseudoClass::Not(list) => !list_matches(list, doc, el),
        PseudoClass::Is(list) | PseudoClass::Where(list) => list_matches(list, doc, el),
        PseudoClass::Root => doc.parent(el) == Some(doc.root()),
        PseudoClass::Empty => doc.children(el).all(|c| match &doc.node(c).data {
            NodeData::Element(_) => false,
            NodeData::Text(t) => t.is_empty(),
            _ => true,
        }),
        PseudoClass::Link => {
            let e = doc.element(el).unwrap();
            (e.is_html("a") || e.is_html("area")) && e.attr("href").is_some()
        }
        PseudoClass::Checked => {
            let e = doc.element(el).unwrap();
            ((e.is_html("input")) && e.attr("checked").is_some())
                || (e.is_html("option") && e.attr("selected").is_some())
        }
        PseudoClass::Disabled | PseudoClass::Enabled => {
            let e = doc.element(el).unwrap();
            let form_control = matches!(
                e.name.as_str(),
                "input" | "button" | "select" | "textarea" | "option" | "optgroup" | "fieldset"
            ) && e.ns == Namespace::Html;
            let disabled = form_control && e.attr("disabled").is_some();
            if matches!(p, PseudoClass::Disabled) {
                disabled
            } else {
                form_control && !disabled
            }
        }
        PseudoClass::Never => false,
    }
}

fn has_element_parent_or_root(doc: &Document, el: NodeId) -> bool {
    doc.parent(el).is_some()
}

fn siblings_before(doc: &Document, el: NodeId) -> impl Iterator<Item = NodeId> + '_ {
    std::iter::successors(doc.prev_element_sibling(el), move |&s| doc.prev_element_sibling(s))
}

fn siblings_after(doc: &Document, el: NodeId) -> impl Iterator<Item = NodeId> + '_ {
    std::iter::successors(doc.next_element_sibling(el), move |&s| doc.next_element_sibling(s))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::html::parse_document;

    fn select(doc: &Document, sel: &str) -> Vec<String> {
        let list = parse_selector_list_str(sel).unwrap_or_else(|| panic!("invalid selector {sel}"));
        doc.descendants(doc.root())
            .filter(|&n| doc.element(n).is_some() && list_matches(&list, doc, n))
            .map(|n| {
                let e = doc.element(n).unwrap();
                e.id().map(|s| s.to_string()).unwrap_or_else(|| e.name.clone())
            })
            .collect()
    }

    const PAGE: &str = r#"<!doctype html><body>
        <ul id=list>
          <li id=a class="x first" data-k="alpha beta">
          <li id=b class=x lang=en-US>
          <li id=c data-k="gamma">
          <li id=d class="y">
        </ul>
        <p id=p1>p</p><div id=d1><p id=p2><span id=s1>s</span></p></div><p id=p3></p>
        <a id=l1 href="https://example.com/x.pdf">x</a><a id=l2>no href</a>"#;

    #[test]
    fn simple_and_compound() {
        let d = parse_document(PAGE);
        assert_eq!(select(&d, "li.x"), ["a", "b"]);
        assert_eq!(select(&d, "#c"), ["c"]);
        assert_eq!(select(&d, ".x.first"), ["a"]);
        assert_eq!(select(&d, "LI#d.y"), ["d"]);
        assert!(select(&d, "#D").is_empty(), "ids are case-sensitive");
        assert_eq!(select(&d, "*#p1"), ["p1"]);
    }

    #[test]
    fn attribute_operators() {
        let d = parse_document(PAGE);
        assert_eq!(select(&d, "[data-k]"), ["a", "c"]);
        assert_eq!(select(&d, "[data-k=gamma]"), ["c"]);
        assert_eq!(select(&d, "[data-k~=beta]"), ["a"]);
        assert_eq!(select(&d, "[lang|=en]"), ["b"]);
        assert_eq!(select(&d, "[href^='https:']"), ["l1"]);
        assert_eq!(select(&d, "[href$=\".pdf\"]"), ["l1"]);
        assert_eq!(select(&d, "[href*=example]"), ["l1"]);
        assert_eq!(select(&d, "[data-k=GAMMA i]"), ["c"]);
        assert!(select(&d, "[data-k=GAMMA]").is_empty());
    }

    #[test]
    fn combinators() {
        let d = parse_document(PAGE);
        assert_eq!(select(&d, "body p"), ["p1", "p2", "p3"]);
        assert_eq!(select(&d, "body > p"), ["p1", "p3"]);
        assert_eq!(select(&d, "div span"), ["s1"]);
        assert_eq!(select(&d, "#a + li"), ["b"]);
        assert_eq!(select(&d, "#b ~ li"), ["c", "d"]);
        assert_eq!(select(&d, "ul li + li + li"), ["c", "d"]);
        assert_eq!(select(&d, "p + div > p > span"), ["s1"]);
    }

    #[test]
    fn structural_pseudo_classes() {
        let d = parse_document(PAGE);
        assert_eq!(select(&d, "li:first-child"), ["a"]);
        assert_eq!(select(&d, "li:last-child"), ["d"]);
        assert_eq!(select(&d, "li:nth-child(2n)"), ["b", "d"]);
        assert_eq!(select(&d, "li:nth-child(odd)"), ["a", "c"]);
        assert_eq!(select(&d, "li:nth-child(-n+2)"), ["a", "b"]);
        assert_eq!(select(&d, "li:nth-child(3)"), ["c"]);
        assert_eq!(select(&d, "li:nth-last-child(1)"), ["d"]);
        assert_eq!(select(&d, "li:not(.x)"), ["c", "d"]);
        assert_eq!(select(&d, "li:not(.x, #d)"), ["c"]);
        assert_eq!(select(&d, "li:nth-child(even of .x)"), ["b"]);
        assert_eq!(select(&d, "p:first-of-type"), ["p1", "p2"]);
        assert_eq!(select(&d, "p:empty"), ["p3"]);
        assert_eq!(select(&d, ":is(#a, #c)"), ["a", "c"]);
        assert_eq!(select(&d, "a:link"), ["l1"]);
        assert_eq!(select(&d, "html:root"), ["html"]);
    }

    #[test]
    fn specificity_values() {
        let s = |t: &str| unpack_specificity(parse_selector_list_str(t).unwrap()[0].specificity);
        assert_eq!(s("*"), (0, 0, 0));
        assert_eq!(s("li"), (0, 0, 1));
        assert_eq!(s("ul li"), (0, 0, 2));
        assert_eq!(s("ul ol+li"), (0, 0, 3));
        assert_eq!(s("h1 + *[rel=up]"), (0, 1, 1));
        assert_eq!(s("ul ol li.red"), (0, 1, 3));
        assert_eq!(s("li.red.level"), (0, 2, 1));
        assert_eq!(s("#x34y"), (1, 0, 0));
        assert_eq!(s("#s12:not(FOO)"), (1, 0, 1));
        assert_eq!(s(".foo :is(.bar, #baz)"), (1, 1, 0));
        assert_eq!(s(":where(#a) p"), (0, 0, 1));
        assert_eq!(s("p::before"), (0, 0, 2));
        assert_eq!(s("li:nth-child(2n of .x)"), (0, 2, 1));
    }

    #[test]
    fn invalid_selectors_are_rejected() {
        for bad in [
            "",
            "a >",
            "> a",
            "a..b",
            "p:unknown",
            "[a=]",
            "a::before b",
            ":nth-child(x)",
            "div,",
        ] {
            assert!(parse_selector_list_str(bad).is_none(), "{bad:?} should be invalid");
        }
    }

    #[test]
    fn an_plus_b() {
        assert_eq!(parse_an_plus_b("odd"), Some((2, 1)));
        assert_eq!(parse_an_plus_b("-n + 3"), Some((-1, 3)));
        assert_eq!(parse_an_plus_b("n"), Some((1, 0)));
        assert_eq!(parse_an_plus_b("+5"), Some((0, 5)));
        assert_eq!(parse_an_plus_b("3n-2"), Some((3, -2)));
        assert_eq!(parse_an_plus_b("2n+"), None);
    }
}
