//! CSS parsing per CSS Syntax Module Level 3, §5: tokens are grouped into component
//! values (preserved tokens, functions and simple blocks), rules and declarations.
//! Interpretation of rule preludes and declaration values happens in
//! [`crate::style::stylesheet`] and [`crate::style::properties`].

use super::tokenizer::{Token, tokenize};

/// A component value: a preserved token, a function, or a `{}`/`[]`/`()` block.
#[derive(Clone, Debug, PartialEq)]
pub enum ComponentValue {
    /// Any token other than `{ [ (` and function tokens.
    Token(Token),
    /// `name( args )`
    Function {
        /// Function name as written.
        name: String,
        /// Arguments, including whitespace and comma tokens.
        args: Vec<ComponentValue>,
    },
    /// A simple block opened by `{`, `[` or `(`.
    Block {
        /// The opening character.
        open: char,
        /// Block contents.
        contents: Vec<ComponentValue>,
    },
}

impl ComponentValue {
    /// Whether this is a whitespace token.
    pub fn is_whitespace(&self) -> bool {
        matches!(self, ComponentValue::Token(Token::Whitespace))
    }

    /// The identifier if this is an ident token.
    pub fn ident(&self) -> Option<&str> {
        match self {
            ComponentValue::Token(Token::Ident(s)) => Some(s),
            _ => None,
        }
    }

    /// Serializes back to CSS text.
    pub fn to_css(&self, out: &mut String) {
        match self {
            ComponentValue::Token(t) => out.push_str(&t.to_string()),
            ComponentValue::Function { name, args } => {
                out.push_str(name);
                out.push('(');
                for a in args {
                    a.to_css(out);
                }
                out.push(')');
            }
            ComponentValue::Block { open, contents } => {
                out.push(*open);
                for c in contents {
                    c.to_css(out);
                }
                out.push(match open {
                    '{' => '}',
                    '[' => ']',
                    _ => ')',
                });
            }
        }
    }
}

/// Serializes a list of component values to CSS text.
pub fn to_css(values: &[ComponentValue]) -> String {
    let mut s = String::new();
    for v in values {
        v.to_css(&mut s);
    }
    s
}

/// A generic rule produced by the syntax-level parser.
#[derive(Clone, Debug, PartialEq)]
pub enum Rule {
    /// `prelude { block }`, typically a style rule.
    Qualified {
        /// Prelude (the selector text, as component values).
        prelude: Vec<ComponentValue>,
        /// Block contents.
        block: Vec<ComponentValue>,
    },
    /// `@name prelude;` or `@name prelude { block }`.
    At {
        /// At-keyword name (without `@`), lowercased.
        name: String,
        /// Prelude.
        prelude: Vec<ComponentValue>,
        /// Block contents, if the rule has a block.
        block: Option<Vec<ComponentValue>>,
    },
}

/// A declaration `name: value [!important]`.
#[derive(Clone, Debug, PartialEq)]
pub struct Declaration {
    /// Property name: lowercased, except custom properties (`--x`) which are case-sensitive.
    pub name: String,
    /// Value with surrounding whitespace and `!important` removed.
    pub value: Vec<ComponentValue>,
    /// Whether `!important` was present.
    pub important: bool,
}

struct Parser {
    tokens: Vec<Token>,
    pos: usize,
}

impl Parser {
    fn new(tokens: Vec<Token>) -> Self {
        Parser { tokens, pos: 0 }
    }

    fn next(&mut self) -> Option<Token> {
        let t = self.tokens.get(self.pos).cloned();
        self.pos += 1;
        t
    }

    fn peek(&self) -> Option<&Token> {
        self.tokens.get(self.pos)
    }

    fn consume_rule_list(&mut self, top_level: bool) -> Vec<Rule> {
        let mut rules = Vec::new();
        while let Some(t) = self.peek() {
            match t {
                Token::Whitespace => self.pos += 1,
                Token::Cdo | Token::Cdc if top_level => self.pos += 1,
                Token::AtKeyword(_) => {
                    if let Some(r) = self.consume_at_rule() {
                        rules.push(r);
                    }
                }
                _ => {
                    if let Some(r) = self.consume_qualified_rule() {
                        rules.push(r);
                    }
                }
            }
        }
        rules
    }

    fn consume_at_rule(&mut self) -> Option<Rule> {
        let Some(Token::AtKeyword(name)) = self.next() else {
            return None;
        };
        let mut prelude = Vec::new();
        loop {
            match self.peek() {
                None => {
                    return Some(Rule::At {
                        name: name.to_ascii_lowercase(),
                        prelude,
                        block: None,
                    });
                }
                Some(Token::Semicolon) => {
                    self.pos += 1;
                    return Some(Rule::At {
                        name: name.to_ascii_lowercase(),
                        prelude,
                        block: None,
                    });
                }
                Some(Token::LeftBrace) => {
                    self.pos += 1;
                    let block = self.consume_block_contents('{');
                    return Some(Rule::At {
                        name: name.to_ascii_lowercase(),
                        prelude,
                        block: Some(block),
                    });
                }
                Some(_) => prelude.push(self.consume_component_value()?),
            }
        }
    }

    fn consume_qualified_rule(&mut self) -> Option<Rule> {
        let mut prelude = Vec::new();
        loop {
            match self.peek() {
                None => return None, // parse error: EOF in prelude, rule dropped
                Some(Token::LeftBrace) => {
                    self.pos += 1;
                    let block = self.consume_block_contents('{');
                    return Some(Rule::Qualified { prelude, block });
                }
                Some(_) => prelude.push(self.consume_component_value()?),
            }
        }
    }

    fn consume_component_value(&mut self) -> Option<ComponentValue> {
        let t = self.next()?;
        Some(match t {
            Token::LeftBrace => ComponentValue::Block {
                open: '{',
                contents: self.consume_block_contents('{'),
            },
            Token::LeftBracket => ComponentValue::Block {
                open: '[',
                contents: self.consume_block_contents('['),
            },
            Token::LeftParen => ComponentValue::Block {
                open: '(',
                contents: self.consume_block_contents('('),
            },
            Token::Function(name) => ComponentValue::Function {
                name,
                args: self.consume_block_contents('('),
            },
            t => ComponentValue::Token(t),
        })
    }

    fn consume_block_contents(&mut self, open: char) -> Vec<ComponentValue> {
        let mut out = Vec::new();
        loop {
            match (self.peek(), open) {
                (None, _) => return out,
                (Some(Token::RightBrace), '{') | (Some(Token::RightBracket), '[') | (Some(Token::RightParen), '(') => {
                    self.pos += 1;
                    return out;
                }
                _ => match self.consume_component_value() {
                    Some(v) => out.push(v),
                    None => return out,
                },
            }
        }
    }
}

/// Parses a stylesheet into a list of rules.
pub fn parse_rules(css: &str) -> Vec<Rule> {
    Parser::new(tokenize(css)).consume_rule_list(true)
}

/// Parses the contents of a block (e.g. `@media` contents) as a nested rule list.
pub fn parse_rule_list(values: &[ComponentValue]) -> Vec<Rule> {
    // Re-tokenizing the serialized block is simple and robust; nested blocks are small.
    Parser::new(tokenize(&to_css(values))).consume_rule_list(false)
}

/// Parses a declaration block (the contents of a style rule's `{}` or a `style=""`
/// attribute). Invalid declarations and nested rules are skipped.
pub fn parse_declaration_list(values: &[ComponentValue]) -> Vec<Declaration> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < values.len() {
        match &values[i] {
            ComponentValue::Token(Token::Whitespace) | ComponentValue::Token(Token::Semicolon) => i += 1,
            ComponentValue::Token(Token::Ident(name)) => {
                let start = i;
                // Collect up to the next top-level semicolon.
                let mut end = i;
                while end < values.len() && values[end] != ComponentValue::Token(Token::Semicolon) {
                    end += 1;
                }
                // A `{}` block before the semicolon means this is a nested rule: skip it.
                if values[start..end]
                    .iter()
                    .any(|v| matches!(v, ComponentValue::Block { open: '{', .. }))
                    && !name.starts_with("--")
                {
                    i = skip_nested_rule(values, start);
                    continue;
                }
                if let Some(d) = parse_one_declaration(name, &values[start + 1..end]) {
                    out.push(d);
                }
                i = end + 1;
            }
            _ => {
                // Something that doesn't start a declaration (nested rule or garbage).
                i = skip_nested_rule(values, i);
            }
        }
    }
    out
}

fn skip_nested_rule(values: &[ComponentValue], mut i: usize) -> usize {
    while i < values.len() {
        match &values[i] {
            ComponentValue::Block { open: '{', .. } => return i + 1,
            ComponentValue::Token(Token::Semicolon) => return i + 1,
            _ => i += 1,
        }
    }
    i
}

fn parse_one_declaration(name: &str, rest: &[ComponentValue]) -> Option<Declaration> {
    let mut j = 0;
    while j < rest.len() && rest[j].is_whitespace() {
        j += 1;
    }
    if j >= rest.len() || rest[j] != ComponentValue::Token(Token::Colon) {
        return None;
    }
    let mut value: Vec<ComponentValue> = rest[j + 1..].to_vec();
    trim_whitespace(&mut value);
    let mut important = false;
    // Detect a trailing `! important`.
    let n = value.len();
    if n >= 2
        && let Some(id) = value[n - 1].ident()
        && id.eq_ignore_ascii_case("important")
    {
        let mut k = n - 2;
        while k > 0 && value[k].is_whitespace() {
            k -= 1;
        }
        if value[k] == ComponentValue::Token(Token::Delim('!')) {
            important = true;
            value.truncate(k);
            trim_whitespace(&mut value);
        }
    }
    let name = if name.starts_with("--") {
        name.to_string()
    } else {
        name.to_ascii_lowercase()
    };
    Some(Declaration { name, value, important })
}

/// Removes leading and trailing whitespace tokens.
pub fn trim_whitespace(v: &mut Vec<ComponentValue>) {
    while v.last().is_some_and(ComponentValue::is_whitespace) {
        v.pop();
    }
    let lead = v.iter().take_while(|c| c.is_whitespace()).count();
    v.drain(..lead);
}

/// Parses a `style=""` attribute value.
pub fn parse_inline_style(css: &str) -> Vec<Declaration> {
    let mut p = Parser::new(tokenize(css));
    let mut values = Vec::new();
    while p.peek().is_some() {
        match p.consume_component_value() {
            Some(v) => values.push(v),
            None => break,
        }
    }
    parse_declaration_list(&values)
}

/// Parses a string into a list of component values (for values and preludes).
pub fn parse_component_values(css: &str) -> Vec<ComponentValue> {
    let mut p = Parser::new(tokenize(css));
    let mut values = Vec::new();
    while p.peek().is_some() {
        match p.consume_component_value() {
            Some(v) => values.push(v),
            None => break,
        }
    }
    values
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rules_and_declarations() {
        let rules = parse_rules("a { color: red !important; width: 10px } @media (min-width: 5px) { b { x: y } }");
        assert_eq!(rules.len(), 2);
        let Rule::Qualified { prelude, block } = &rules[0] else {
            panic!()
        };
        assert_eq!(to_css(prelude).trim(), "a");
        let decls = parse_declaration_list(block);
        assert_eq!(decls.len(), 2);
        assert_eq!(decls[0].name, "color");
        assert!(decls[0].important);
        assert_eq!(to_css(&decls[0].value), "red");
        assert!(!decls[1].important);
        let Rule::At { name, block, .. } = &rules[1] else {
            panic!()
        };
        assert_eq!(name, "media");
        let inner = parse_rule_list(block.as_ref().unwrap());
        assert_eq!(inner.len(), 1);
    }

    #[test]
    fn error_recovery() {
        // A bad declaration is skipped without losing the following ones.
        let d = parse_inline_style("color red; ; width: 1px; :x; height:2px");
        let names: Vec<_> = d.iter().map(|d| d.name.as_str()).collect();
        assert_eq!(names, ["width", "height"]);
        // An unterminated rule at EOF is dropped; earlier ones survive.
        let rules = parse_rules("a{b:c} d{e:f} g");
        assert_eq!(rules.len(), 2);
    }

    #[test]
    fn nested_rules_are_skipped() {
        let rules = parse_rules(".a { color: red; &:hover { color: blue } width: 1px }");
        let Rule::Qualified { block, .. } = &rules[0] else {
            panic!()
        };
        let names: Vec<_> = parse_declaration_list(block).into_iter().map(|d| d.name).collect();
        assert_eq!(names, ["color", "width"]);
    }

    #[test]
    fn custom_property_names_keep_case() {
        let d = parse_inline_style("--Main-Color: #fff; COLOR: var(--Main-Color)");
        assert_eq!(d[0].name, "--Main-Color");
        assert_eq!(d[1].name, "color");
    }
}
