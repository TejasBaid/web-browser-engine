//! Stylesheet objects: interprets syntax-level rules into style rules (selectors plus
//! typed declarations), `@media`, `@import`, `@font-face` and `@supports`.

use super::properties::{PropertyDeclaration, parse_declaration};
use crate::css::parser::{
    ComponentValue, Declaration, Rule, parse_component_values, parse_declaration_list, parse_inline_style,
    parse_rule_list, parse_rules,
};
use crate::css::selector::{SelectorList, parse_selector_list};
use crate::css::tokenizer::Token;
use std::rc::Rc;

/// Where a stylesheet comes from; determines cascade precedence.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Origin {
    /// The built-in user-agent stylesheet.
    UserAgent,
    /// Page styles (`<style>`, `<link>`, `style=""`).
    Author,
}

/// A parsed declaration block.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct DeclarationBlock {
    /// Longhand/custom declarations in source order with their `!important` flag.
    pub declarations: Vec<(PropertyDeclaration, bool)>,
}

impl DeclarationBlock {
    /// Builds a block from syntax-level declarations, dropping invalid ones.
    pub fn from_declarations(decls: &[Declaration]) -> Self {
        let mut out = Vec::new();
        for d in decls {
            if let Some(longhands) = parse_declaration(&d.name, &d.value) {
                out.extend(longhands.into_iter().map(|l| (l, d.important)));
            }
        }
        DeclarationBlock { declarations: out }
    }

    /// Parses a `style=""` attribute.
    pub fn parse_inline(css: &str) -> Self {
        Self::from_declarations(&parse_inline_style(css))
    }
}

/// A style rule: `selectors { declarations }`.
#[derive(Clone, Debug)]
pub struct StyleRule {
    /// The selector list.
    pub selectors: SelectorList,
    /// Declarations.
    pub block: Rc<DeclarationBlock>,
}

/// An `@font-face` rule (TTF/OTF sources only are usable).
#[derive(Clone, Debug, PartialEq)]
pub struct FontFaceRule {
    /// `font-family` descriptor.
    pub family: String,
    /// `url()` sources, in order.
    pub sources: Vec<String>,
    /// `font-weight` descriptor (single value; ranges use their start).
    pub weight: u16,
    /// Whether `font-style` is italic/oblique.
    pub italic: bool,
}

/// A rule inside a stylesheet.
#[derive(Clone, Debug)]
pub enum CssRule {
    /// A style rule.
    Style(StyleRule),
    /// `@media <query> { rules }` (the query is evaluated at cascade time).
    Media {
        /// Media query list.
        query: Rc<Vec<ComponentValue>>,
        /// Nested rules.
        rules: Vec<CssRule>,
    },
    /// `@import url(...) <media>;` — fetched by the page loader.
    Import {
        /// URL as written.
        url: String,
        /// Media query list (may be empty).
        media: Rc<Vec<ComponentValue>>,
    },
    /// `@font-face`
    FontFace(FontFaceRule),
}

/// A parsed stylesheet.
#[derive(Clone, Debug)]
pub struct Stylesheet {
    /// Top-level rules in source order.
    pub rules: Vec<CssRule>,
    /// Origin of the sheet.
    pub origin: Origin,
    /// URL the sheet was loaded from (for resolving relative URLs), if any.
    pub base_url: Option<String>,
}

impl Stylesheet {
    /// Parses CSS source text.
    pub fn parse(css: &str, origin: Origin, base_url: Option<String>) -> Stylesheet {
        let rules = convert_rules(parse_rules(css), true);
        Stylesheet {
            rules,
            origin,
            base_url,
        }
    }

    /// Number of style rules, including nested ones.
    pub fn style_rule_count(&self) -> usize {
        fn count(rules: &[CssRule]) -> usize {
            rules
                .iter()
                .map(|r| match r {
                    CssRule::Style(_) => 1,
                    CssRule::Media { rules, .. } => count(rules),
                    _ => 0,
                })
                .sum()
        }
        count(&self.rules)
    }

    /// All `@font-face` rules (including inside `@media`, without evaluating it).
    pub fn font_faces(&self) -> Vec<FontFaceRule> {
        fn collect(rules: &[CssRule], out: &mut Vec<FontFaceRule>) {
            for r in rules {
                match r {
                    CssRule::FontFace(f) => out.push(f.clone()),
                    CssRule::Media { rules, .. } => collect(rules, out),
                    _ => {}
                }
            }
        }
        let mut out = Vec::new();
        collect(&self.rules, &mut out);
        out
    }
}

fn convert_rules(rules: Vec<Rule>, top_level: bool) -> Vec<CssRule> {
    let mut out = Vec::new();
    let mut imports_allowed = top_level;
    for rule in rules {
        match rule {
            Rule::Qualified { prelude, block } => {
                imports_allowed = false;
                // An invalid selector drops the whole rule.
                let Some(selectors) = parse_selector_list(&prelude) else {
                    continue;
                };
                let block = DeclarationBlock::from_declarations(&parse_declaration_list(&block));
                out.push(CssRule::Style(StyleRule {
                    selectors,
                    block: Rc::new(block),
                }));
            }
            Rule::At { name, prelude, block } => match (name.as_str(), block) {
                ("import", None) if imports_allowed => {
                    let items: Vec<&ComponentValue> = prelude.iter().filter(|v| !v.is_whitespace()).collect();
                    let url = match items.first() {
                        Some(ComponentValue::Token(Token::Url(u))) => Some(u.clone()),
                        Some(ComponentValue::Token(Token::String(s))) => Some(s.clone()),
                        Some(ComponentValue::Function { name, args }) if name.eq_ignore_ascii_case("url") => {
                            args.iter().find_map(|a| match a {
                                ComponentValue::Token(Token::String(s)) => Some(s.clone()),
                                _ => None,
                            })
                        }
                        _ => None,
                    };
                    if let Some(url) = url {
                        let first = prelude.iter().position(|v| !v.is_whitespace()).unwrap_or(0);
                        let media: Vec<ComponentValue> = prelude[first + 1..]
                            .iter()
                            .filter(|v| {
                                // Skip `layer`/`supports()` import conditions.
                                !matches!(v, ComponentValue::Function { name, .. } if name.eq_ignore_ascii_case("supports") || name.eq_ignore_ascii_case("layer"))
                                    && v.ident().is_none_or(|s| !s.eq_ignore_ascii_case("layer"))
                            })
                            .cloned()
                            .collect();
                        out.push(CssRule::Import {
                            url,
                            media: Rc::new(media),
                        });
                    }
                }
                ("charset", _) | ("namespace", _) => {}
                ("media", Some(block)) => {
                    imports_allowed = false;
                    out.push(CssRule::Media {
                        query: Rc::new(prelude),
                        rules: convert_rules(parse_rule_list(&block), false),
                    });
                }
                ("supports", Some(block)) => {
                    imports_allowed = false;
                    if evaluate_supports(&prelude) {
                        out.extend(convert_rules(parse_rule_list(&block), false));
                    }
                }
                ("layer", Some(block)) | ("container", Some(block)) | ("scope", Some(block)) => {
                    // Cascade layers/containers/scopes are flattened (approximation).
                    out.extend(convert_rules(parse_rule_list(&block), false));
                }
                ("font-face", Some(block)) => {
                    if let Some(ff) = parse_font_face(&block) {
                        out.push(CssRule::FontFace(ff));
                    }
                }
                _ => {}
            },
        }
    }
    out
}

fn parse_font_face(block: &[ComponentValue]) -> Option<FontFaceRule> {
    let decls = parse_declaration_list(block);
    let mut family = None;
    let mut sources = Vec::new();
    let mut weight = 400;
    let mut italic = false;
    for d in decls {
        match d.name.as_str() {
            "font-family" => {
                family = d.value.iter().find_map(|v| match v {
                    ComponentValue::Token(Token::String(s)) => Some(s.clone()),
                    ComponentValue::Token(Token::Ident(s)) => Some(s.clone()),
                    _ => None,
                });
            }
            "src" => {
                for part in d.value.split(|v| matches!(v, ComponentValue::Token(Token::Comma))) {
                    for v in part {
                        match v {
                            ComponentValue::Token(Token::Url(u)) => sources.push(u.clone()),
                            ComponentValue::Function { name, args } if name.eq_ignore_ascii_case("url") => {
                                if let Some(s) = args.iter().find_map(|a| match a {
                                    ComponentValue::Token(Token::String(s)) => Some(s.clone()),
                                    _ => None,
                                }) {
                                    sources.push(s);
                                }
                            }
                            _ => {}
                        }
                    }
                }
            }
            "font-weight" => {
                for v in &d.value {
                    match v {
                        ComponentValue::Token(Token::Number { value, .. }) => {
                            weight = *value as u16;
                            break;
                        }
                        ComponentValue::Token(Token::Ident(s)) if s.eq_ignore_ascii_case("bold") => {
                            weight = 700;
                            break;
                        }
                        _ => {}
                    }
                }
            }
            "font-style" => {
                italic = d.value.iter().any(|v| {
                    v.ident()
                        .is_some_and(|s| s.eq_ignore_ascii_case("italic") || s.eq_ignore_ascii_case("oblique"))
                });
            }
            _ => {}
        }
    }
    Some(FontFaceRule {
        family: family?,
        sources,
        weight,
        italic,
    })
}

/// Evaluates an `@supports` condition: `(prop: value)`, `not`, `and`, `or`, `selector()`.
pub fn evaluate_supports(prelude: &[ComponentValue]) -> bool {
    let items: Vec<&ComponentValue> = prelude.iter().filter(|v| !v.is_whitespace()).collect();
    eval_supports_items(&items).unwrap_or(false)
}

fn eval_supports_items(items: &[&ComponentValue]) -> Option<bool> {
    if items.is_empty() {
        return None;
    }
    if items[0].ident().is_some_and(|s| s.eq_ignore_ascii_case("not")) {
        return Some(!eval_supports_in_parens(items.get(1)?)?);
    }
    let mut result = eval_supports_in_parens(items[0])?;
    let mut i = 1;
    while i < items.len() {
        let op = items[i].ident()?.to_ascii_lowercase();
        let rhs = eval_supports_in_parens(items.get(i + 1)?)?;
        result = match op.as_str() {
            "and" => result && rhs,
            "or" => result || rhs,
            _ => return None,
        };
        i += 2;
    }
    Some(result)
}

fn eval_supports_in_parens(v: &ComponentValue) -> Option<bool> {
    match v {
        ComponentValue::Block { open: '(', contents } => {
            let items: Vec<&ComponentValue> = contents.iter().filter(|v| !v.is_whitespace()).collect();
            if matches!(items.first(), Some(ComponentValue::Block { .. }))
                || items
                    .first()
                    .and_then(|i| i.ident())
                    .is_some_and(|s| s.eq_ignore_ascii_case("not"))
            {
                return eval_supports_items(&items);
            }
            // A declaration test.
            let colon = contents
                .iter()
                .position(|c| matches!(c, ComponentValue::Token(Token::Colon)))?;
            let name = contents[..colon].iter().find_map(|c| c.ident())?.to_ascii_lowercase();
            let mut value = contents[colon + 1..].to_vec();
            crate::css::parser::trim_whitespace(&mut value);
            Some(parse_declaration(&name, &value).is_some())
        }
        ComponentValue::Function { name, args } if name.eq_ignore_ascii_case("selector") => {
            Some(parse_selector_list(args).is_some())
        }
        _ => Some(false),
    }
}

/// Parses a stand-alone selector/declaration pair, for tests and tools.
pub fn parse_rule_text(css: &str) -> Option<StyleRule> {
    let sheet = Stylesheet::parse(css, Origin::Author, None);
    sheet.rules.into_iter().find_map(|r| match r {
        CssRule::Style(s) => Some(s),
        _ => None,
    })
}

/// Parses a media query list from text.
pub fn parse_media_text(text: &str) -> Vec<ComponentValue> {
    parse_component_values(text)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::style::properties::PropertyId;

    #[test]
    fn parses_rules_media_imports_and_font_faces() {
        let css = r#"
            @charset "utf-8";
            @import url("a.css") screen;
            @import "b.css";
            p, .x { color: red; margin: 0 auto }
            p:bogus { color: blue }
            @media (max-width: 600px) { p { color: green } }
            @supports (display: flex) { .f { display: flex } }
            @supports (display: nonsense) { .g { display: block } }
            @font-face { font-family: "Test Sans"; src: url(t.ttf) format("truetype"); font-weight: bold }
        "#;
        let sheet = Stylesheet::parse(css, Origin::Author, None);
        let kinds: Vec<&str> = sheet
            .rules
            .iter()
            .map(|r| match r {
                CssRule::Style(_) => "style",
                CssRule::Media { .. } => "media",
                CssRule::Import { .. } => "import",
                CssRule::FontFace(_) => "font-face",
            })
            .collect();
        assert_eq!(kinds, ["import", "import", "style", "media", "style", "font-face"]);
        let CssRule::Style(r) = &sheet.rules[2] else { panic!() };
        assert_eq!(r.selectors.len(), 2);
        // color + 4 margin longhands
        assert_eq!(r.block.declarations.len(), 5);
        let ff = &sheet.font_faces()[0];
        assert_eq!(ff.family, "Test Sans");
        assert_eq!(ff.sources, ["t.ttf"]);
        assert_eq!(ff.weight, 700);
    }

    #[test]
    fn invalid_declarations_do_not_shadow_valid_ones() {
        let r = parse_rule_text("p { width: 10px; width: -5px; }").unwrap();
        assert_eq!(r.block.declarations.len(), 1);
        assert!(matches!(
            r.block.declarations[0].0,
            PropertyDeclaration::Longhand(PropertyId::Width, _)
        ));
    }
}
