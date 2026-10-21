//! CSS tokenizer following CSS Syntax Module Level 3, §4
//! (<https://www.w3.org/TR/css-syntax-3/#tokenization>).

use std::fmt;

/// Whether a numeric token was written as an integer or with a fraction/exponent.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NumKind {
    /// No `.` or exponent in the source.
    Integer,
    /// Has a fractional part or exponent.
    Number,
}

/// A CSS token.
#[derive(Clone, Debug, PartialEq)]
pub enum Token {
    /// `foo`
    Ident(String),
    /// `foo(` — the name without the parenthesis.
    Function(String),
    /// `@foo`
    AtKeyword(String),
    /// `#foo`; `is_id` is true when the name would be a valid identifier.
    Hash {
        /// The name after `#`.
        value: String,
        /// Type flag "id".
        is_id: bool,
    },
    /// A quoted string (quotes removed, escapes resolved).
    String(String),
    /// A string interrupted by a newline.
    BadString,
    /// `url(foo)` with an unquoted URL.
    Url(String),
    /// A malformed unquoted URL.
    BadUrl,
    /// Any other single code point.
    Delim(char),
    /// `12`, `-3.5e2`
    Number {
        /// Numeric value.
        value: f32,
        /// Integer or number.
        kind: NumKind,
        /// Whether an explicit sign was written (relevant for `an+b`).
        signed: bool,
    },
    /// `50%`
    Percentage(f32),
    /// `12px`, `1.5em`
    Dimension {
        /// Numeric value.
        value: f32,
        /// Integer or number.
        kind: NumKind,
        /// Unit, as written (compare case-insensitively).
        unit: String,
        /// Whether an explicit sign was written.
        signed: bool,
    },
    /// One or more whitespace characters.
    Whitespace,
    /// `<!--`
    Cdo,
    /// `-->`
    Cdc,
    /// `:`
    Colon,
    /// `;`
    Semicolon,
    /// `,`
    Comma,
    /// `[`
    LeftBracket,
    /// `]`
    RightBracket,
    /// `(`
    LeftParen,
    /// `)`
    RightParen,
    /// `{`
    LeftBrace,
    /// `}`
    RightBrace,
}

impl fmt::Display for Token {
    /// Serializes the token back to CSS text (used for custom properties and debugging).
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Token::Ident(s) => write!(f, "{s}"),
            Token::Function(s) => write!(f, "{s}("),
            Token::AtKeyword(s) => write!(f, "@{s}"),
            Token::Hash { value, .. } => write!(f, "#{value}"),
            Token::String(s) => write!(f, "\"{}\"", s.replace('"', "\\\"")),
            Token::BadString => writeln!(f, "\""),
            Token::Url(s) => write!(f, "url({s})"),
            Token::BadUrl => write!(f, "url()"),
            Token::Delim(c) => write!(f, "{c}"),
            Token::Number { value, signed, .. } => {
                if *signed && *value >= 0.0 {
                    write!(f, "+")?;
                }
                write!(f, "{value}")
            }
            Token::Percentage(v) => write!(f, "{v}%"),
            Token::Dimension {
                value, unit, signed, ..
            } => {
                if *signed && *value >= 0.0 {
                    write!(f, "+")?;
                }
                write!(f, "{value}{unit}")
            }
            Token::Whitespace => write!(f, " "),
            Token::Cdo => write!(f, "<!--"),
            Token::Cdc => write!(f, "-->"),
            Token::Colon => write!(f, ":"),
            Token::Semicolon => write!(f, ";"),
            Token::Comma => write!(f, ","),
            Token::LeftBracket => write!(f, "["),
            Token::RightBracket => write!(f, "]"),
            Token::LeftParen => write!(f, "("),
            Token::RightParen => write!(f, ")"),
            Token::LeftBrace => write!(f, "{{"),
            Token::RightBrace => write!(f, "}}"),
        }
    }
}

/// Tokenizes a complete CSS source string.
pub fn tokenize(input: &str) -> Vec<Token> {
    let mut t = Tokenizer::new(input);
    let mut out = Vec::new();
    while let Some(tok) = t.next_token() {
        out.push(tok);
    }
    out
}

struct Tokenizer {
    chars: Vec<char>,
    pos: usize,
}

fn is_name_start(c: char) -> bool {
    c.is_ascii_alphabetic() || c == '_' || !c.is_ascii()
}

fn is_name(c: char) -> bool {
    is_name_start(c) || c.is_ascii_digit() || c == '-'
}

fn is_ws(c: char) -> bool {
    matches!(c, ' ' | '\t' | '\n')
}

impl Tokenizer {
    fn new(input: &str) -> Self {
        // Preprocessing: CRLF/CR/FF become LF, NUL becomes U+FFFD.
        let mut chars = Vec::with_capacity(input.len());
        let mut it = input.chars().peekable();
        while let Some(c) = it.next() {
            match c {
                '\r' => {
                    if it.peek() == Some(&'\n') {
                        it.next();
                    }
                    chars.push('\n');
                }
                '\x0C' => chars.push('\n'),
                '\0' => chars.push('\u{FFFD}'),
                c => chars.push(c),
            }
        }
        Tokenizer { chars, pos: 0 }
    }

    #[inline]
    fn peek(&self, n: usize) -> Option<char> {
        self.chars.get(self.pos + n).copied()
    }

    fn starts_valid_escape(&self, n: usize) -> bool {
        self.peek(n) == Some('\\') && self.peek(n + 1).is_some_and(|c| c != '\n')
    }

    fn starts_ident(&self, n: usize) -> bool {
        match self.peek(n) {
            Some('-') => {
                self.peek(n + 1).is_some_and(|c| is_name_start(c) || c == '-') || self.starts_valid_escape(n + 1)
            }
            Some(c) if is_name_start(c) => true,
            Some('\\') => self.starts_valid_escape(n),
            _ => false,
        }
    }

    fn consume_comments(&mut self) {
        while self.peek(0) == Some('/') && self.peek(1) == Some('*') {
            self.pos += 2;
            loop {
                match self.peek(0) {
                    None => return,
                    Some('*') if self.peek(1) == Some('/') => {
                        self.pos += 2;
                        break;
                    }
                    _ => self.pos += 1,
                }
            }
        }
    }

    fn next_token(&mut self) -> Option<Token> {
        self.consume_comments();
        let c = self.peek(0)?;
        self.pos += 1;
        let tok = match c {
            c if is_ws(c) => {
                while self.peek(0).is_some_and(is_ws) {
                    self.pos += 1;
                }
                Token::Whitespace
            }
            '"' | '\'' => self.consume_string(c),
            '#' => {
                if self.peek(0).is_some_and(is_name) || self.starts_valid_escape(0) {
                    let is_id = self.starts_ident(0);
                    Token::Hash {
                        value: self.consume_name(),
                        is_id,
                    }
                } else {
                    Token::Delim('#')
                }
            }
            '(' => Token::LeftParen,
            ')' => Token::RightParen,
            '+' => {
                if self.starts_number_at_prev() {
                    self.pos -= 1;
                    self.consume_numeric()
                } else {
                    Token::Delim('+')
                }
            }
            ',' => Token::Comma,
            '-' => {
                if self.starts_number_at_prev() {
                    self.pos -= 1;
                    self.consume_numeric()
                } else if self.peek(0) == Some('-') && self.peek(1) == Some('>') {
                    self.pos += 2;
                    Token::Cdc
                } else if self.starts_ident_at_prev() {
                    self.pos -= 1;
                    self.consume_ident_like()
                } else {
                    Token::Delim('-')
                }
            }
            '.' => {
                if self.starts_number_at_prev() {
                    self.pos -= 1;
                    self.consume_numeric()
                } else {
                    Token::Delim('.')
                }
            }
            ':' => Token::Colon,
            ';' => Token::Semicolon,
            '<' => {
                if self.peek(0) == Some('!') && self.peek(1) == Some('-') && self.peek(2) == Some('-') {
                    self.pos += 3;
                    Token::Cdo
                } else {
                    Token::Delim('<')
                }
            }
            '@' => {
                if self.starts_ident(0) {
                    Token::AtKeyword(self.consume_name())
                } else {
                    Token::Delim('@')
                }
            }
            '[' => Token::LeftBracket,
            '\\' => {
                if self.starts_ident_at_prev() {
                    self.pos -= 1;
                    self.consume_ident_like()
                } else {
                    Token::Delim('\\')
                }
            }
            ']' => Token::RightBracket,
            '{' => Token::LeftBrace,
            '}' => Token::RightBrace,
            c if c.is_ascii_digit() => {
                self.pos -= 1;
                self.consume_numeric()
            }
            c if is_name_start(c) => {
                self.pos -= 1;
                self.consume_ident_like()
            }
            c => Token::Delim(c),
        };
        Some(tok)
    }

    /// "Starts a number" check for the three code points beginning with the one just consumed.
    fn starts_number_at_prev(&self) -> bool {
        let p = self.pos - 1;
        let at = |i: usize| self.chars.get(p + i).copied();
        let digit = |c: Option<char>| c.is_some_and(|c| c.is_ascii_digit());
        match at(0) {
            Some('+') | Some('-') => digit(at(1)) || (at(1) == Some('.') && digit(at(2))),
            Some('.') => digit(at(1)),
            c => digit(c),
        }
    }

    fn starts_ident_at_prev(&self) -> bool {
        let p = self.pos - 1;
        let c0 = self.chars.get(p).copied();
        let c1 = self.chars.get(p + 1).copied();
        let esc = |i: usize| self.chars.get(i) == Some(&'\\') && self.chars.get(i + 1).is_some_and(|&c| c != '\n');
        match c0 {
            Some('-') => c1.is_some_and(|c| is_name_start(c) || c == '-') || esc(p + 1),
            Some(c) if is_name_start(c) => true,
            Some('\\') => esc(p),
            _ => false,
        }
    }

    fn consume_escape(&mut self) -> char {
        // The backslash has already been consumed.
        match self.peek(0) {
            None => '\u{FFFD}',
            Some(c) if c.is_ascii_hexdigit() => {
                let mut v: u32 = 0;
                let mut n = 0;
                while n < 6 && self.peek(0).is_some_and(|c| c.is_ascii_hexdigit()) {
                    v = v * 16 + self.peek(0).unwrap().to_digit(16).unwrap();
                    self.pos += 1;
                    n += 1;
                }
                if self.peek(0).is_some_and(is_ws) {
                    self.pos += 1;
                }
                if v == 0 || (0xD800..=0xDFFF).contains(&v) || v > 0x10FFFF {
                    '\u{FFFD}'
                } else {
                    char::from_u32(v).unwrap_or('\u{FFFD}')
                }
            }
            Some(c) => {
                self.pos += 1;
                c
            }
        }
    }

    fn consume_name(&mut self) -> String {
        let mut s = String::new();
        loop {
            match self.peek(0) {
                Some(c) if is_name(c) => {
                    s.push(c);
                    self.pos += 1;
                }
                Some('\\') if self.starts_valid_escape(0) => {
                    self.pos += 1;
                    s.push(self.consume_escape());
                }
                _ => return s,
            }
        }
    }

    fn consume_number(&mut self) -> (f32, NumKind, bool) {
        let start = self.pos;
        let mut kind = NumKind::Integer;
        let mut signed = false;
        if matches!(self.peek(0), Some('+') | Some('-')) {
            signed = true;
            self.pos += 1;
        }
        while self.peek(0).is_some_and(|c| c.is_ascii_digit()) {
            self.pos += 1;
        }
        if self.peek(0) == Some('.') && self.peek(1).is_some_and(|c| c.is_ascii_digit()) {
            kind = NumKind::Number;
            self.pos += 1;
            while self.peek(0).is_some_and(|c| c.is_ascii_digit()) {
                self.pos += 1;
            }
        }
        if matches!(self.peek(0), Some('e') | Some('E')) {
            let digit_at = |n: usize| self.peek(n).is_some_and(|c| c.is_ascii_digit());
            if digit_at(1) || (matches!(self.peek(1), Some('+') | Some('-')) && digit_at(2)) {
                kind = NumKind::Number;
                self.pos += 2;
                while self.peek(0).is_some_and(|c| c.is_ascii_digit()) {
                    self.pos += 1;
                }
            }
        }
        let text: String = self.chars[start..self.pos].iter().collect();
        (text.parse::<f32>().unwrap_or(0.0), kind, signed)
    }

    fn consume_numeric(&mut self) -> Token {
        let (value, kind, signed) = self.consume_number();
        if self.starts_ident(0) {
            let unit = self.consume_name();
            Token::Dimension {
                value,
                kind,
                unit,
                signed,
            }
        } else if self.peek(0) == Some('%') {
            self.pos += 1;
            Token::Percentage(value)
        } else {
            Token::Number { value, kind, signed }
        }
    }

    fn consume_ident_like(&mut self) -> Token {
        let name = self.consume_name();
        if name.eq_ignore_ascii_case("url") && self.peek(0) == Some('(') {
            self.pos += 1;
            while self.peek(0).is_some_and(is_ws) && self.peek(1).is_some_and(is_ws) {
                self.pos += 1;
            }
            let quoted = |c: Option<char>| matches!(c, Some('"') | Some('\''));
            if quoted(self.peek(0)) || (self.peek(0).is_some_and(is_ws) && quoted(self.peek(1))) {
                Token::Function(name)
            } else {
                self.consume_url()
            }
        } else if self.peek(0) == Some('(') {
            self.pos += 1;
            Token::Function(name)
        } else {
            Token::Ident(name)
        }
    }

    fn consume_string(&mut self, quote: char) -> Token {
        let mut s = String::new();
        loop {
            match self.peek(0) {
                None => return Token::String(s),
                Some(c) if c == quote => {
                    self.pos += 1;
                    return Token::String(s);
                }
                Some('\n') => return Token::BadString,
                Some('\\') => {
                    self.pos += 1;
                    match self.peek(0) {
                        None => {}
                        Some('\n') => self.pos += 1,
                        Some(_) => s.push(self.consume_escape()),
                    }
                }
                Some(c) => {
                    s.push(c);
                    self.pos += 1;
                }
            }
        }
    }

    fn consume_url(&mut self) -> Token {
        let mut s = String::new();
        while self.peek(0).is_some_and(is_ws) {
            self.pos += 1;
        }
        loop {
            match self.peek(0) {
                None => return Token::Url(s),
                Some(')') => {
                    self.pos += 1;
                    return Token::Url(s);
                }
                Some(c) if is_ws(c) => {
                    while self.peek(0).is_some_and(is_ws) {
                        self.pos += 1;
                    }
                    match self.peek(0) {
                        None => return Token::Url(s),
                        Some(')') => {
                            self.pos += 1;
                            return Token::Url(s);
                        }
                        _ => {
                            self.consume_bad_url_remnants();
                            return Token::BadUrl;
                        }
                    }
                }
                Some('"') | Some('\'') | Some('(') => {
                    self.consume_bad_url_remnants();
                    return Token::BadUrl;
                }
                Some(c) if (c as u32) < 0x20 && c != '\t' || c == '\x7F' => {
                    self.consume_bad_url_remnants();
                    return Token::BadUrl;
                }
                Some('\\') => {
                    if self.starts_valid_escape(0) {
                        self.pos += 1;
                        s.push(self.consume_escape());
                    } else {
                        self.consume_bad_url_remnants();
                        return Token::BadUrl;
                    }
                }
                Some(c) => {
                    s.push(c);
                    self.pos += 1;
                }
            }
        }
    }

    fn consume_bad_url_remnants(&mut self) {
        loop {
            match self.peek(0) {
                None => return,
                Some(')') => {
                    self.pos += 1;
                    return;
                }
                Some('\\') if self.starts_valid_escape(0) => {
                    self.pos += 1;
                    self.consume_escape();
                }
                _ => self.pos += 1,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn basic_tokens() {
        let t = tokenize("a.b > #c{color:red;width:-1.5em}");
        assert_eq!(
            t,
            vec![
                Token::Ident("a".into()),
                Token::Delim('.'),
                Token::Ident("b".into()),
                Token::Whitespace,
                Token::Delim('>'),
                Token::Whitespace,
                Token::Hash {
                    value: "c".into(),
                    is_id: true
                },
                Token::LeftBrace,
                Token::Ident("color".into()),
                Token::Colon,
                Token::Ident("red".into()),
                Token::Semicolon,
                Token::Ident("width".into()),
                Token::Colon,
                Token::Dimension {
                    value: -1.5,
                    kind: NumKind::Number,
                    unit: "em".into(),
                    signed: true
                },
                Token::RightBrace,
            ]
        );
    }

    #[test]
    fn numbers_percentages_and_signs() {
        let t = tokenize("50% +3 1e2 .5");
        assert_eq!(t[0], Token::Percentage(50.0));
        assert_eq!(
            t[2],
            Token::Number {
                value: 3.0,
                kind: NumKind::Integer,
                signed: true
            }
        );
        assert_eq!(
            t[4],
            Token::Number {
                value: 100.0,
                kind: NumKind::Number,
                signed: false
            }
        );
        assert_eq!(
            t[6],
            Token::Number {
                value: 0.5,
                kind: NumKind::Number,
                signed: false
            }
        );
    }

    #[test]
    fn strings_urls_comments_escapes() {
        let t = tokenize(r#"/* c */ url( a.png ) url("b.png") 'x\'y' \31 0 --var -->"#);
        assert_eq!(t[0], Token::Whitespace);
        assert_eq!(t[1], Token::Url("a.png".into()));
        assert_eq!(t[3], Token::Function("url".into()));
        assert_eq!(t[4], Token::String("b.png".into()));
        assert!(t.contains(&Token::String("x'y".into())));
        assert!(t.contains(&Token::Ident("10".into())));
        assert!(t.contains(&Token::Ident("--var".into())));
        assert_eq!(t.last(), Some(&Token::Cdc));
    }

    #[test]
    fn bad_url_and_bad_string() {
        let t = tokenize("url(a b) \"x\ny\"");
        assert_eq!(t[0], Token::BadUrl);
        assert!(t.contains(&Token::BadString));
    }
}
