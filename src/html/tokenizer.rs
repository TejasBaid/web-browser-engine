//! The HTML tokenizer, a direct implementation of the WHATWG tokenization state machine
//! (<https://html.spec.whatwg.org/multipage/parsing.html#tokenization>).
//!
//! The tokenizer is pull-based: [`Tokenizer::next_token`] runs the state machine until at
//! least one token is ready. The tree builder switches the tokenizer state (RCDATA for
//! `<title>`/`<textarea>`, RAWTEXT for `<style>`, script data, PLAINTEXT) between calls,
//! exactly as the specification's tree construction stage does.
//!
//! Character references are resolved inline with lookahead instead of through the
//! separate character-reference sub-states; the observable behaviour is identical.

use super::entities::{ENTITIES, MAX_ENTITY_LEN};
use std::collections::VecDeque;

/// Tokenizer states that can be entered from outside (by the tree builder or tests).
/// The full set of internal states is private.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum State {
    /// Normal content.
    Data,
    /// Escapable raw text: `<title>`, `<textarea>`.
    Rcdata,
    /// Raw text: `<style>`, `<xmp>`, `<iframe>`, `<noembed>`, `<noframes>`.
    Rawtext,
    /// `<script>` contents.
    ScriptData,
    /// `<plaintext>`: everything until EOF is text.
    Plaintext,
    /// `<![CDATA[` section (only reachable from foreign content).
    CdataSection,
    // ---- internal states ----
    #[doc(hidden)]
    TagOpen,
    #[doc(hidden)]
    EndTagOpen,
    #[doc(hidden)]
    TagName,
    #[doc(hidden)]
    RcdataLessThan,
    #[doc(hidden)]
    RcdataEndTagOpen,
    #[doc(hidden)]
    RcdataEndTagName,
    #[doc(hidden)]
    RawtextLessThan,
    #[doc(hidden)]
    RawtextEndTagOpen,
    #[doc(hidden)]
    RawtextEndTagName,
    #[doc(hidden)]
    ScriptLessThan,
    #[doc(hidden)]
    ScriptEndTagOpen,
    #[doc(hidden)]
    ScriptEndTagName,
    #[doc(hidden)]
    ScriptEscapeStart,
    #[doc(hidden)]
    ScriptEscapeStartDash,
    #[doc(hidden)]
    ScriptEscaped,
    #[doc(hidden)]
    ScriptEscapedDash,
    #[doc(hidden)]
    ScriptEscapedDashDash,
    #[doc(hidden)]
    ScriptEscapedLessThan,
    #[doc(hidden)]
    ScriptEscapedEndTagOpen,
    #[doc(hidden)]
    ScriptEscapedEndTagName,
    #[doc(hidden)]
    ScriptDoubleEscapeStart,
    #[doc(hidden)]
    ScriptDoubleEscaped,
    #[doc(hidden)]
    ScriptDoubleEscapedDash,
    #[doc(hidden)]
    ScriptDoubleEscapedDashDash,
    #[doc(hidden)]
    ScriptDoubleEscapedLessThan,
    #[doc(hidden)]
    ScriptDoubleEscapeEnd,
    #[doc(hidden)]
    BeforeAttrName,
    #[doc(hidden)]
    AttrName,
    #[doc(hidden)]
    AfterAttrName,
    #[doc(hidden)]
    BeforeAttrValue,
    #[doc(hidden)]
    AttrValueDoubleQuoted,
    #[doc(hidden)]
    AttrValueSingleQuoted,
    #[doc(hidden)]
    AttrValueUnquoted,
    #[doc(hidden)]
    AfterAttrValueQuoted,
    #[doc(hidden)]
    SelfClosingStartTag,
    #[doc(hidden)]
    BogusComment,
    #[doc(hidden)]
    MarkupDeclarationOpen,
    #[doc(hidden)]
    CommentStart,
    #[doc(hidden)]
    CommentStartDash,
    #[doc(hidden)]
    Comment,
    #[doc(hidden)]
    CommentLessThan,
    #[doc(hidden)]
    CommentLessThanBang,
    #[doc(hidden)]
    CommentLessThanBangDash,
    #[doc(hidden)]
    CommentLessThanBangDashDash,
    #[doc(hidden)]
    CommentEndDash,
    #[doc(hidden)]
    CommentEnd,
    #[doc(hidden)]
    CommentEndBang,
    #[doc(hidden)]
    Doctype,
    #[doc(hidden)]
    BeforeDoctypeName,
    #[doc(hidden)]
    DoctypeName,
    #[doc(hidden)]
    AfterDoctypeName,
    #[doc(hidden)]
    AfterDoctypePublicKeyword,
    #[doc(hidden)]
    BeforeDoctypePublicId,
    #[doc(hidden)]
    DoctypePublicIdDoubleQuoted,
    #[doc(hidden)]
    DoctypePublicIdSingleQuoted,
    #[doc(hidden)]
    AfterDoctypePublicId,
    #[doc(hidden)]
    BetweenDoctypePublicAndSystemIds,
    #[doc(hidden)]
    AfterDoctypeSystemKeyword,
    #[doc(hidden)]
    BeforeDoctypeSystemId,
    #[doc(hidden)]
    DoctypeSystemIdDoubleQuoted,
    #[doc(hidden)]
    DoctypeSystemIdSingleQuoted,
    #[doc(hidden)]
    AfterDoctypeSystemId,
    #[doc(hidden)]
    BogusDoctype,
    #[doc(hidden)]
    CdataSectionBracket,
    #[doc(hidden)]
    CdataSectionEnd,
}

/// A start or end tag token.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Tag {
    /// Lowercased tag name.
    pub name: String,
    /// Attributes in source order (duplicates already removed).
    pub attrs: Vec<(String, String)>,
    /// Whether the tag ended with `/>`.
    pub self_closing: bool,
}

/// A DOCTYPE token.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Doctype {
    /// Doctype name, lowercased.
    pub name: Option<String>,
    /// Public identifier.
    pub public_id: Option<String>,
    /// System identifier.
    pub system_id: Option<String>,
    /// Set for malformed doctypes; forces quirks mode.
    pub force_quirks: bool,
}

/// Tokens produced by the tokenizer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Token {
    /// `<!DOCTYPE …>`
    Doctype(Doctype),
    /// `<name …>`
    StartTag(Tag),
    /// `</name>`
    EndTag(Tag),
    /// `<!-- … -->`
    Comment(String),
    /// A run of character data (may contain U+0000 in the data state).
    Characters(String),
    /// End of input.
    Eof,
}

/// The HTML tokenizer.
pub struct Tokenizer {
    input: Vec<char>,
    pos: usize,
    /// Current state. The tree builder sets this after inserting RCDATA/RAWTEXT elements.
    pub state: State,
    /// Set by the tree builder when the adjusted current node is a foreign element, which
    /// enables `<![CDATA[` sections.
    pub allow_cdata: bool,
    tokens: VecDeque<Token>,
    chars: String,
    tag: Tag,
    is_end_tag: bool,
    attr_name: String,
    attr_value: String,
    attr_dup: bool,
    comment: String,
    doctype: Doctype,
    temp: String,
    last_start_tag: Option<String>,
    eof_emitted: bool,
    /// Number of parse errors encountered (the tokenizer never aborts on errors).
    pub errors: usize,
}

const REPLACEMENT: char = '\u{FFFD}';

fn is_ws(c: char) -> bool {
    matches!(c, '\t' | '\n' | '\x0C' | ' ')
}

impl Tokenizer {
    /// Creates a tokenizer for `input`. Newlines are normalized (CRLF and CR become LF).
    pub fn new(input: &str) -> Self {
        let mut chars = Vec::with_capacity(input.len());
        let mut it = input.chars().peekable();
        while let Some(c) = it.next() {
            if c == '\r' {
                if it.peek() == Some(&'\n') {
                    it.next();
                }
                chars.push('\n');
            } else {
                chars.push(c);
            }
        }
        Tokenizer {
            input: chars,
            pos: 0,
            state: State::Data,
            allow_cdata: false,
            tokens: VecDeque::new(),
            chars: String::new(),
            tag: Tag::default(),
            is_end_tag: false,
            attr_name: String::new(),
            attr_value: String::new(),
            attr_dup: false,
            comment: String::new(),
            doctype: Doctype::default(),
            temp: String::new(),
            last_start_tag: None,
            eof_emitted: false,
            errors: 0,
        }
    }

    /// Sets the "last start tag" used to recognise appropriate end tags in RCDATA,
    /// RAWTEXT and script data (used by fragment parsing and the html5lib tests).
    pub fn set_last_start_tag(&mut self, name: Option<&str>) {
        self.last_start_tag = name.map(str::to_string);
    }

    /// Returns the next token. After [`Token::Eof`] has been returned once, it keeps
    /// returning `Eof`.
    pub fn next_token(&mut self) -> Token {
        loop {
            if let Some(t) = self.tokens.pop_front() {
                return t;
            }
            if self.eof_emitted {
                return Token::Eof;
            }
            self.step();
        }
    }

    // ---------------------------------------------------------------- helpers

    #[inline]
    fn next_char(&mut self) -> Option<char> {
        let c = self.input.get(self.pos).copied();
        self.pos += 1;
        c
    }

    /// "Reconsume": steps back over the character just read (including EOF).
    #[inline]
    fn unread(&mut self) {
        self.pos -= 1;
    }

    fn peek_matches(&self, s: &str, case_insensitive: bool) -> bool {
        s.chars()
            .enumerate()
            .all(|(k, expected)| match self.input.get(self.pos + k) {
                Some(&c) => c == expected || (case_insensitive && c.eq_ignore_ascii_case(&expected)),
                None => false,
            })
    }

    fn error(&mut self) {
        self.errors += 1;
    }

    #[inline]
    fn emit_char(&mut self, c: char) {
        self.chars.push(c);
    }

    fn emit_str(&mut self, s: &str) {
        self.chars.push_str(s);
    }

    fn flush_chars(&mut self) {
        if !self.chars.is_empty() {
            let s = std::mem::take(&mut self.chars);
            self.tokens.push_back(Token::Characters(s));
        }
    }

    fn emit(&mut self, t: Token) {
        self.flush_chars();
        if matches!(t, Token::Eof) {
            self.eof_emitted = true;
        }
        self.tokens.push_back(t);
    }

    fn emit_eof(&mut self) {
        self.emit(Token::Eof);
    }

    fn new_tag(&mut self, end: bool) {
        self.tag = Tag::default();
        self.is_end_tag = end;
    }

    fn start_attr(&mut self) {
        self.finish_attr();
        self.attr_dup = false;
    }

    /// Ends the attribute name: checks for duplicates (which are dropped).
    fn end_attr_name(&mut self) {
        self.attr_dup = self.tag.attrs.iter().any(|(n, _)| *n == self.attr_name);
        if self.attr_dup {
            self.error();
        }
    }

    fn finish_attr(&mut self) {
        if self.attr_name.is_empty() && self.attr_value.is_empty() {
            return;
        }
        let name = std::mem::take(&mut self.attr_name);
        let value = std::mem::take(&mut self.attr_value);
        if !self.attr_dup && !self.tag.attrs.iter().any(|(n, _)| *n == name) {
            self.tag.attrs.push((name, value));
        }
        self.attr_dup = false;
    }

    fn emit_tag(&mut self) {
        self.finish_attr();
        let tag = std::mem::take(&mut self.tag);
        if self.is_end_tag {
            if !tag.attrs.is_empty() || tag.self_closing {
                self.error();
            }
            self.emit(Token::EndTag(tag));
        } else {
            self.last_start_tag = Some(tag.name.clone());
            self.emit(Token::StartTag(tag));
        }
    }

    fn is_appropriate_end_tag(&self) -> bool {
        self.last_start_tag.as_deref() == Some(self.tag.name.as_str())
    }

    fn emit_comment(&mut self) {
        let c = std::mem::take(&mut self.comment);
        self.emit(Token::Comment(c));
    }

    fn emit_doctype(&mut self) {
        let d = std::mem::take(&mut self.doctype);
        self.emit(Token::Doctype(d));
    }

    // ------------------------------------------------------ character references

    /// Called just after consuming `&`. Resolves a character reference at the current
    /// position, appending the result to the current attribute value or emitting it.
    fn consume_char_ref(&mut self, in_attr: bool) {
        let out = self.char_ref(in_attr);
        if in_attr {
            self.attr_value.push_str(&out);
        } else {
            self.emit_str(&out);
        }
    }

    fn char_ref(&mut self, in_attr: bool) -> String {
        match self.input.get(self.pos).copied() {
            Some(c) if c.is_ascii_alphanumeric() => self.named_char_ref(in_attr),
            Some('#') => {
                self.pos += 1;
                self.numeric_char_ref()
            }
            _ => "&".to_string(),
        }
    }

    fn named_char_ref(&mut self, in_attr: bool) -> String {
        let start = self.pos;
        let avail = (self.input.len() - start).min(MAX_ENTITY_LEN);
        let mut candidate = String::with_capacity(MAX_ENTITY_LEN);
        let mut best: Option<(usize, &'static str)> = None;
        for i in 0..avail {
            let c = self.input[start + i];
            if !(c.is_ascii_alphanumeric() || c == ';') {
                break;
            }
            candidate.push(c);
            // Binary search for an exact match on the prefix we have so far.
            if let Ok(idx) = ENTITIES.binary_search_by(|(n, _)| n.as_bytes().cmp(candidate.as_bytes())) {
                best = Some((i + 1, ENTITIES[idx].1));
            }
            // Stop early once no entity starts with the candidate.
            let pp = ENTITIES.partition_point(|(n, _)| n.as_bytes() < candidate.as_bytes());
            if pp >= ENTITIES.len() || !ENTITIES[pp].0.starts_with(candidate.as_str()) {
                break;
            }
            if c == ';' {
                break;
            }
        }
        match best {
            Some((len, value)) => {
                let last = self.input[start + len - 1];
                let next = self.input.get(start + len).copied();
                if in_attr && last != ';' && next.is_some_and(|n| n == '=' || n.is_ascii_alphanumeric()) {
                    // Historical quirk: `&amp` followed by `=`/alnum in an attribute is literal.
                    return "&".to_string();
                }
                if last != ';' {
                    self.error();
                }
                self.pos = start + len;
                value.to_string()
            }
            None => {
                // Ambiguous ampersand: the alphanumerics are re-tokenized as ordinary text.
                let mut i = start;
                while i < self.input.len() && self.input[i].is_ascii_alphanumeric() {
                    i += 1;
                }
                if self.input.get(i) == Some(&';') {
                    self.error();
                }
                "&".to_string()
            }
        }
    }

    fn numeric_char_ref(&mut self) -> String {
        // We are just past "&#".
        let hex = matches!(self.input.get(self.pos), Some('x') | Some('X'));
        let digits_start = if hex { self.pos + 1 } else { self.pos };
        let radix = if hex { 16 } else { 10 };
        let mut i = digits_start;
        let mut value: u32 = 0;
        while let Some(&c) = self.input.get(i) {
            match c.to_digit(radix) {
                Some(d) => {
                    value = value.saturating_mul(radix).saturating_add(d);
                    i += 1;
                }
                None => break,
            }
        }
        if i == digits_start {
            // absence-of-digits-in-numeric-character-reference: flush "&#" / "&#x".
            self.error();
            return if hex {
                let x = self.input[self.pos];
                self.pos += 1;
                format!("&#{x}")
            } else {
                "&#".to_string()
            };
        }
        if self.input.get(i) == Some(&';') {
            i += 1;
        } else {
            self.error();
        }
        self.pos = i;
        let c = match value {
            0 => {
                self.error();
                REPLACEMENT
            }
            v if v > 0x10FFFF => {
                self.error();
                REPLACEMENT
            }
            0xD800..=0xDFFF => {
                self.error();
                REPLACEMENT
            }
            v => {
                if (0xFDD0..=0xFDEF).contains(&v) || (v & 0xFFFE) == 0xFFFE {
                    self.error();
                }
                if v == 0x0D || (v < 0x20 && !matches!(v, 0x09 | 0x0A | 0x0C)) || (0x7F..=0x9F).contains(&v) {
                    self.error();
                }
                match windows_1252_replacement(v) {
                    Some(r) => r,
                    None => char::from_u32(v).unwrap_or(REPLACEMENT),
                }
            }
        };
        c.to_string()
    }

    // ------------------------------------------------------------ state machine

    /// Runs one step of the state machine (consumes at most a handful of characters).
    #[allow(clippy::cognitive_complexity)]
    fn step(&mut self) {
        use State::*;
        match self.state {
            Data => loop {
                match self.next_char() {
                    Some('&') => {
                        self.consume_char_ref(false);
                    }
                    Some('<') => {
                        self.state = TagOpen;
                        return;
                    }
                    Some('\0') => {
                        self.error();
                        self.emit_char('\0');
                    }
                    Some(c) => self.emit_char(c),
                    None => {
                        self.emit_eof();
                        return;
                    }
                }
            },
            Rcdata => loop {
                match self.next_char() {
                    Some('&') => self.consume_char_ref(false),
                    Some('<') => {
                        self.state = RcdataLessThan;
                        return;
                    }
                    Some('\0') => {
                        self.error();
                        self.emit_char(REPLACEMENT);
                    }
                    Some(c) => self.emit_char(c),
                    None => {
                        self.emit_eof();
                        return;
                    }
                }
            },
            Rawtext | ScriptData | Plaintext => {
                let st = self.state;
                loop {
                    match self.next_char() {
                        Some('<') if st == Rawtext => {
                            self.state = RawtextLessThan;
                            return;
                        }
                        Some('<') if st == ScriptData => {
                            self.state = ScriptLessThan;
                            return;
                        }
                        Some('\0') => {
                            self.error();
                            self.emit_char(REPLACEMENT);
                        }
                        Some(c) => self.emit_char(c),
                        None => {
                            self.emit_eof();
                            return;
                        }
                    }
                }
            }
            TagOpen => match self.next_char() {
                Some('!') => self.state = MarkupDeclarationOpen,
                Some('/') => self.state = EndTagOpen,
                Some(c) if c.is_ascii_alphabetic() => {
                    self.new_tag(false);
                    self.unread();
                    self.state = TagName;
                }
                Some('?') => {
                    self.error();
                    self.comment.clear();
                    self.unread();
                    self.state = BogusComment;
                }
                None => {
                    self.error();
                    self.emit_char('<');
                    self.emit_eof();
                }
                Some(_) => {
                    self.error();
                    self.emit_char('<');
                    self.unread();
                    self.state = Data;
                }
            },
            EndTagOpen => match self.next_char() {
                Some(c) if c.is_ascii_alphabetic() => {
                    self.new_tag(true);
                    self.unread();
                    self.state = TagName;
                }
                Some('>') => {
                    self.error();
                    self.state = Data;
                }
                None => {
                    self.error();
                    self.emit_str("</");
                    self.emit_eof();
                }
                Some(_) => {
                    self.error();
                    self.comment.clear();
                    self.unread();
                    self.state = BogusComment;
                }
            },
            TagName => loop {
                match self.next_char() {
                    Some(c) if is_ws(c) => {
                        self.state = BeforeAttrName;
                        return;
                    }
                    Some('/') => {
                        self.state = SelfClosingStartTag;
                        return;
                    }
                    Some('>') => {
                        self.state = Data;
                        self.emit_tag();
                        return;
                    }
                    Some('\0') => {
                        self.error();
                        self.tag.name.push(REPLACEMENT);
                    }
                    Some(c) => self.tag.name.push(c.to_ascii_lowercase()),
                    None => {
                        self.error();
                        self.emit_eof();
                        return;
                    }
                }
            },
            RcdataLessThan | RawtextLessThan => {
                let (open, text) = if self.state == RcdataLessThan {
                    (RcdataEndTagOpen, Rcdata)
                } else {
                    (RawtextEndTagOpen, Rawtext)
                };
                match self.next_char() {
                    Some('/') => {
                        self.temp.clear();
                        self.state = open;
                    }
                    _ => {
                        self.emit_char('<');
                        self.unread();
                        self.state = text;
                    }
                }
            }
            RcdataEndTagOpen | RawtextEndTagOpen | ScriptEndTagOpen | ScriptEscapedEndTagOpen => {
                let (name, text) = match self.state {
                    RcdataEndTagOpen => (RcdataEndTagName, Rcdata),
                    RawtextEndTagOpen => (RawtextEndTagName, Rawtext),
                    ScriptEndTagOpen => (ScriptEndTagName, ScriptData),
                    _ => (ScriptEscapedEndTagName, ScriptEscaped),
                };
                match self.next_char() {
                    Some(c) if c.is_ascii_alphabetic() => {
                        self.new_tag(true);
                        self.unread();
                        self.state = name;
                    }
                    _ => {
                        self.emit_str("</");
                        self.unread();
                        self.state = text;
                    }
                }
            }
            RcdataEndTagName | RawtextEndTagName | ScriptEndTagName | ScriptEscapedEndTagName => {
                let text = match self.state {
                    RcdataEndTagName => Rcdata,
                    RawtextEndTagName => Rawtext,
                    ScriptEndTagName => ScriptData,
                    _ => ScriptEscaped,
                };
                loop {
                    let c = self.next_char();
                    match c {
                        Some(c) if is_ws(c) && self.is_appropriate_end_tag() => {
                            self.state = BeforeAttrName;
                            return;
                        }
                        Some('/') if self.is_appropriate_end_tag() => {
                            self.state = SelfClosingStartTag;
                            return;
                        }
                        Some('>') if self.is_appropriate_end_tag() => {
                            self.state = Data;
                            self.emit_tag();
                            return;
                        }
                        Some(c) if c.is_ascii_alphabetic() => {
                            self.tag.name.push(c.to_ascii_lowercase());
                            self.temp.push(c);
                        }
                        _ => {
                            self.emit_str("</");
                            let t = std::mem::take(&mut self.temp);
                            self.emit_str(&t);
                            self.unread();
                            self.state = text;
                            return;
                        }
                    }
                }
            }
            ScriptLessThan => match self.next_char() {
                Some('/') => {
                    self.temp.clear();
                    self.state = ScriptEndTagOpen;
                }
                Some('!') => {
                    self.state = ScriptEscapeStart;
                    self.emit_str("<!");
                }
                _ => {
                    self.emit_char('<');
                    self.unread();
                    self.state = ScriptData;
                }
            },
            ScriptEscapeStart | ScriptEscapeStartDash => {
                let next = if self.state == ScriptEscapeStart {
                    ScriptEscapeStartDash
                } else {
                    ScriptEscapedDashDash
                };
                match self.next_char() {
                    Some('-') => {
                        self.state = next;
                        self.emit_char('-');
                    }
                    _ => {
                        self.unread();
                        self.state = ScriptData;
                    }
                }
            }
            ScriptEscaped => match self.next_char() {
                Some('-') => {
                    self.state = ScriptEscapedDash;
                    self.emit_char('-');
                }
                Some('<') => self.state = ScriptEscapedLessThan,
                Some('\0') => {
                    self.error();
                    self.emit_char(REPLACEMENT);
                }
                Some(c) => self.emit_char(c),
                None => {
                    self.error();
                    self.emit_eof();
                }
            },
            ScriptEscapedDash | ScriptEscapedDashDash => {
                let dashdash = self.state == ScriptEscapedDashDash;
                match self.next_char() {
                    Some('-') => {
                        self.state = ScriptEscapedDashDash;
                        self.emit_char('-');
                    }
                    Some('<') => self.state = ScriptEscapedLessThan,
                    Some('>') if dashdash => {
                        self.state = ScriptData;
                        self.emit_char('>');
                    }
                    Some('\0') => {
                        self.error();
                        self.state = ScriptEscaped;
                        self.emit_char(REPLACEMENT);
                    }
                    Some(c) => {
                        self.state = ScriptEscaped;
                        self.emit_char(c);
                    }
                    None => {
                        self.error();
                        self.emit_eof();
                    }
                }
            }
            ScriptEscapedLessThan => match self.next_char() {
                Some('/') => {
                    self.temp.clear();
                    self.state = ScriptEscapedEndTagOpen;
                }
                Some(c) if c.is_ascii_alphabetic() => {
                    self.temp.clear();
                    self.emit_char('<');
                    self.unread();
                    self.state = ScriptDoubleEscapeStart;
                }
                _ => {
                    self.emit_char('<');
                    self.unread();
                    self.state = ScriptEscaped;
                }
            },
            ScriptDoubleEscapeStart | ScriptDoubleEscapeEnd => {
                let (if_script, otherwise) = if self.state == ScriptDoubleEscapeStart {
                    (ScriptDoubleEscaped, ScriptEscaped)
                } else {
                    (ScriptEscaped, ScriptDoubleEscaped)
                };
                match self.next_char() {
                    Some(c) if is_ws(c) || c == '/' || c == '>' => {
                        self.state = if self.temp == "script" { if_script } else { otherwise };
                        self.emit_char(c);
                    }
                    Some(c) if c.is_ascii_alphabetic() => {
                        self.temp.push(c.to_ascii_lowercase());
                        self.emit_char(c);
                    }
                    _ => {
                        self.unread();
                        self.state = otherwise;
                    }
                }
            }
            ScriptDoubleEscaped => match self.next_char() {
                Some('-') => {
                    self.state = ScriptDoubleEscapedDash;
                    self.emit_char('-');
                }
                Some('<') => {
                    self.state = ScriptDoubleEscapedLessThan;
                    self.emit_char('<');
                }
                Some('\0') => {
                    self.error();
                    self.emit_char(REPLACEMENT);
                }
                Some(c) => self.emit_char(c),
                None => {
                    self.error();
                    self.emit_eof();
                }
            },
            ScriptDoubleEscapedDash | ScriptDoubleEscapedDashDash => {
                let dashdash = self.state == ScriptDoubleEscapedDashDash;
                match self.next_char() {
                    Some('-') => {
                        self.state = ScriptDoubleEscapedDashDash;
                        self.emit_char('-');
                    }
                    Some('<') => {
                        self.state = ScriptDoubleEscapedLessThan;
                        self.emit_char('<');
                    }
                    Some('>') if dashdash => {
                        self.state = ScriptData;
                        self.emit_char('>');
                    }
                    Some('\0') => {
                        self.error();
                        self.state = ScriptDoubleEscaped;
                        self.emit_char(REPLACEMENT);
                    }
                    Some(c) => {
                        self.state = ScriptDoubleEscaped;
                        self.emit_char(c);
                    }
                    None => {
                        self.error();
                        self.emit_eof();
                    }
                }
            }
            ScriptDoubleEscapedLessThan => match self.next_char() {
                Some('/') => {
                    self.temp.clear();
                    self.state = ScriptDoubleEscapeEnd;
                    self.emit_char('/');
                }
                _ => {
                    self.unread();
                    self.state = ScriptDoubleEscaped;
                }
            },
            BeforeAttrName => match self.next_char() {
                Some(c) if is_ws(c) => {}
                Some('/') | Some('>') | None => {
                    self.unread();
                    self.state = AfterAttrName;
                }
                Some('=') => {
                    self.error();
                    self.start_attr();
                    self.attr_name.push('=');
                    self.state = AttrName;
                }
                Some(_) => {
                    self.start_attr();
                    self.unread();
                    self.state = AttrName;
                }
            },
            AttrName => loop {
                match self.next_char() {
                    Some(c) if is_ws(c) || c == '/' || c == '>' => {
                        self.unread();
                        self.end_attr_name();
                        self.state = AfterAttrName;
                        return;
                    }
                    None => {
                        self.unread();
                        self.end_attr_name();
                        self.state = AfterAttrName;
                        return;
                    }
                    Some('=') => {
                        self.end_attr_name();
                        self.state = BeforeAttrValue;
                        return;
                    }
                    Some('\0') => {
                        self.error();
                        self.attr_name.push(REPLACEMENT);
                    }
                    Some(c) => {
                        if matches!(c, '"' | '\'' | '<') {
                            self.error();
                        }
                        self.attr_name.push(c.to_ascii_lowercase());
                    }
                }
            },
            AfterAttrName => match self.next_char() {
                Some(c) if is_ws(c) => {}
                Some('/') => self.state = SelfClosingStartTag,
                Some('=') => self.state = BeforeAttrValue,
                Some('>') => {
                    self.state = Data;
                    self.emit_tag();
                }
                None => {
                    self.error();
                    self.emit_eof();
                }
                Some(_) => {
                    self.start_attr();
                    self.unread();
                    self.state = AttrName;
                }
            },
            BeforeAttrValue => match self.next_char() {
                Some(c) if is_ws(c) => {}
                Some('"') => self.state = AttrValueDoubleQuoted,
                Some('\'') => self.state = AttrValueSingleQuoted,
                Some('>') => {
                    self.error();
                    self.state = Data;
                    self.emit_tag();
                }
                _ => {
                    self.unread();
                    self.state = AttrValueUnquoted;
                }
            },
            AttrValueDoubleQuoted | AttrValueSingleQuoted => {
                let quote = if self.state == AttrValueDoubleQuoted { '"' } else { '\'' };
                loop {
                    match self.next_char() {
                        Some(c) if c == quote => {
                            self.state = AfterAttrValueQuoted;
                            return;
                        }
                        Some('&') => self.consume_char_ref(true),
                        Some('\0') => {
                            self.error();
                            self.attr_value.push(REPLACEMENT);
                        }
                        Some(c) => self.attr_value.push(c),
                        None => {
                            self.error();
                            self.emit_eof();
                            return;
                        }
                    }
                }
            }
            AttrValueUnquoted => loop {
                match self.next_char() {
                    Some(c) if is_ws(c) => {
                        self.state = BeforeAttrName;
                        return;
                    }
                    Some('&') => self.consume_char_ref(true),
                    Some('>') => {
                        self.state = Data;
                        self.emit_tag();
                        return;
                    }
                    Some('\0') => {
                        self.error();
                        self.attr_value.push(REPLACEMENT);
                    }
                    Some(c) => {
                        if matches!(c, '"' | '\'' | '<' | '=' | '`') {
                            self.error();
                        }
                        self.attr_value.push(c);
                    }
                    None => {
                        self.error();
                        self.emit_eof();
                        return;
                    }
                }
            },
            AfterAttrValueQuoted => match self.next_char() {
                Some(c) if is_ws(c) => self.state = BeforeAttrName,
                Some('/') => self.state = SelfClosingStartTag,
                Some('>') => {
                    self.state = Data;
                    self.emit_tag();
                }
                None => {
                    self.error();
                    self.emit_eof();
                }
                Some(_) => {
                    self.error();
                    self.unread();
                    self.state = BeforeAttrName;
                }
            },
            SelfClosingStartTag => match self.next_char() {
                Some('>') => {
                    self.tag.self_closing = true;
                    self.state = Data;
                    self.emit_tag();
                }
                None => {
                    self.error();
                    self.emit_eof();
                }
                Some(_) => {
                    self.error();
                    self.unread();
                    self.state = BeforeAttrName;
                }
            },
            BogusComment => loop {
                match self.next_char() {
                    Some('>') => {
                        self.state = Data;
                        self.emit_comment();
                        return;
                    }
                    None => {
                        self.emit_comment();
                        self.emit_eof();
                        return;
                    }
                    Some('\0') => {
                        self.error();
                        self.comment.push(REPLACEMENT);
                    }
                    Some(c) => self.comment.push(c),
                }
            },
            MarkupDeclarationOpen => {
                if self.peek_matches("--", false) {
                    self.pos += 2;
                    self.comment.clear();
                    self.state = CommentStart;
                } else if self.peek_matches("DOCTYPE", true) {
                    self.pos += 7;
                    self.state = Doctype;
                } else if self.peek_matches("[CDATA[", false) {
                    self.pos += 7;
                    if self.allow_cdata {
                        self.state = CdataSection;
                    } else {
                        self.error();
                        self.comment = "[CDATA[".to_string();
                        self.state = BogusComment;
                    }
                } else {
                    self.error();
                    self.comment.clear();
                    self.state = BogusComment;
                }
            }
            CommentStart => match self.next_char() {
                Some('-') => self.state = CommentStartDash,
                Some('>') => {
                    self.error();
                    self.state = Data;
                    self.emit_comment();
                }
                _ => {
                    self.unread();
                    self.state = Comment;
                }
            },
            CommentStartDash => match self.next_char() {
                Some('-') => self.state = CommentEnd,
                Some('>') => {
                    self.error();
                    self.state = Data;
                    self.emit_comment();
                }
                None => {
                    self.error();
                    self.emit_comment();
                    self.emit_eof();
                }
                Some(_) => {
                    self.comment.push('-');
                    self.unread();
                    self.state = Comment;
                }
            },
            Comment => loop {
                match self.next_char() {
                    Some('<') => {
                        self.comment.push('<');
                        self.state = CommentLessThan;
                        return;
                    }
                    Some('-') => {
                        self.state = CommentEndDash;
                        return;
                    }
                    Some('\0') => {
                        self.error();
                        self.comment.push(REPLACEMENT);
                    }
                    Some(c) => self.comment.push(c),
                    None => {
                        self.error();
                        self.emit_comment();
                        self.emit_eof();
                        return;
                    }
                }
            },
            CommentLessThan => match self.next_char() {
                Some('!') => {
                    self.comment.push('!');
                    self.state = CommentLessThanBang;
                }
                Some('<') => self.comment.push('<'),
                _ => {
                    self.unread();
                    self.state = Comment;
                }
            },
            CommentLessThanBang => match self.next_char() {
                Some('-') => self.state = CommentLessThanBangDash,
                _ => {
                    self.unread();
                    self.state = Comment;
                }
            },
            CommentLessThanBangDash => match self.next_char() {
                Some('-') => self.state = CommentLessThanBangDashDash,
                _ => {
                    self.unread();
                    self.state = CommentEndDash;
                }
            },
            CommentLessThanBangDashDash => {
                match self.next_char() {
                    Some('>') | None => {}
                    Some(_) => self.error(),
                }
                self.unread();
                self.state = CommentEnd;
            }
            CommentEndDash => match self.next_char() {
                Some('-') => self.state = CommentEnd,
                None => {
                    self.error();
                    self.emit_comment();
                    self.emit_eof();
                }
                Some(_) => {
                    self.comment.push('-');
                    self.unread();
                    self.state = Comment;
                }
            },
            CommentEnd => match self.next_char() {
                Some('>') => {
                    self.state = Data;
                    self.emit_comment();
                }
                Some('!') => self.state = CommentEndBang,
                Some('-') => self.comment.push('-'),
                None => {
                    self.error();
                    self.emit_comment();
                    self.emit_eof();
                }
                Some(_) => {
                    self.comment.push_str("--");
                    self.unread();
                    self.state = Comment;
                }
            },
            CommentEndBang => match self.next_char() {
                Some('-') => {
                    self.comment.push_str("--!");
                    self.state = CommentEndDash;
                }
                Some('>') => {
                    self.error();
                    self.state = Data;
                    self.emit_comment();
                }
                None => {
                    self.error();
                    self.emit_comment();
                    self.emit_eof();
                }
                Some(_) => {
                    self.comment.push_str("--!");
                    self.unread();
                    self.state = Comment;
                }
            },
            Doctype => match self.next_char() {
                Some(c) if is_ws(c) => self.state = BeforeDoctypeName,
                Some('>') => {
                    self.unread();
                    self.state = BeforeDoctypeName;
                }
                None => {
                    self.error();
                    self.doctype = self::Doctype {
                        force_quirks: true,
                        ..Default::default()
                    };
                    self.emit_doctype();
                    self.emit_eof();
                }
                Some(_) => {
                    self.error();
                    self.unread();
                    self.state = BeforeDoctypeName;
                }
            },
            BeforeDoctypeName => match self.next_char() {
                Some(c) if is_ws(c) => {}
                Some('\0') => {
                    self.error();
                    self.doctype = self::Doctype {
                        name: Some(REPLACEMENT.to_string()),
                        ..Default::default()
                    };
                    self.state = DoctypeName;
                }
                Some('>') => {
                    self.error();
                    self.doctype = self::Doctype {
                        force_quirks: true,
                        ..Default::default()
                    };
                    self.state = Data;
                    self.emit_doctype();
                }
                None => {
                    self.error();
                    self.doctype = self::Doctype {
                        force_quirks: true,
                        ..Default::default()
                    };
                    self.emit_doctype();
                    self.emit_eof();
                }
                Some(c) => {
                    self.doctype = self::Doctype {
                        name: Some(c.to_ascii_lowercase().to_string()),
                        ..Default::default()
                    };
                    self.state = DoctypeName;
                }
            },
            DoctypeName => loop {
                match self.next_char() {
                    Some(c) if is_ws(c) => {
                        self.state = AfterDoctypeName;
                        return;
                    }
                    Some('>') => {
                        self.state = Data;
                        self.emit_doctype();
                        return;
                    }
                    Some('\0') => {
                        self.error();
                        self.doctype.name.get_or_insert_default().push(REPLACEMENT);
                    }
                    Some(c) => self.doctype.name.get_or_insert_default().push(c.to_ascii_lowercase()),
                    None => {
                        self.error();
                        self.doctype.force_quirks = true;
                        self.emit_doctype();
                        self.emit_eof();
                        return;
                    }
                }
            },
            AfterDoctypeName => match self.next_char() {
                Some(c) if is_ws(c) => {}
                Some('>') => {
                    self.state = Data;
                    self.emit_doctype();
                }
                None => {
                    self.error();
                    self.doctype.force_quirks = true;
                    self.emit_doctype();
                    self.emit_eof();
                }
                Some(_) => {
                    self.unread();
                    if self.peek_matches("PUBLIC", true) {
                        self.pos += 6;
                        self.state = AfterDoctypePublicKeyword;
                    } else if self.peek_matches("SYSTEM", true) {
                        self.pos += 6;
                        self.state = AfterDoctypeSystemKeyword;
                    } else {
                        self.error();
                        self.doctype.force_quirks = true;
                        self.state = BogusDoctype;
                    }
                }
            },
            AfterDoctypePublicKeyword | AfterDoctypeSystemKeyword => {
                let public = self.state == AfterDoctypePublicKeyword;
                match self.next_char() {
                    Some(c) if is_ws(c) => {
                        self.state = if public {
                            BeforeDoctypePublicId
                        } else {
                            BeforeDoctypeSystemId
                        };
                    }
                    Some(q @ ('"' | '\'')) => {
                        self.error();
                        self.start_doctype_id(public, q);
                    }
                    Some('>') => {
                        self.error();
                        self.doctype.force_quirks = true;
                        self.state = Data;
                        self.emit_doctype();
                    }
                    None => {
                        self.error();
                        self.doctype.force_quirks = true;
                        self.emit_doctype();
                        self.emit_eof();
                    }
                    Some(_) => {
                        self.error();
                        self.doctype.force_quirks = true;
                        self.unread();
                        self.state = BogusDoctype;
                    }
                }
            }
            BeforeDoctypePublicId | BeforeDoctypeSystemId => {
                let public = self.state == BeforeDoctypePublicId;
                match self.next_char() {
                    Some(c) if is_ws(c) => {}
                    Some(q @ ('"' | '\'')) => self.start_doctype_id(public, q),
                    Some('>') => {
                        self.error();
                        self.doctype.force_quirks = true;
                        self.state = Data;
                        self.emit_doctype();
                    }
                    None => {
                        self.error();
                        self.doctype.force_quirks = true;
                        self.emit_doctype();
                        self.emit_eof();
                    }
                    Some(_) => {
                        self.error();
                        self.doctype.force_quirks = true;
                        self.unread();
                        self.state = BogusDoctype;
                    }
                }
            }
            DoctypePublicIdDoubleQuoted
            | DoctypePublicIdSingleQuoted
            | DoctypeSystemIdDoubleQuoted
            | DoctypeSystemIdSingleQuoted => {
                let (public, quote) = match self.state {
                    DoctypePublicIdDoubleQuoted => (true, '"'),
                    DoctypePublicIdSingleQuoted => (true, '\''),
                    DoctypeSystemIdDoubleQuoted => (false, '"'),
                    _ => (false, '\''),
                };
                loop {
                    let c = self.next_char();
                    match c {
                        Some(c) if c == quote => {
                            self.state = if public {
                                AfterDoctypePublicId
                            } else {
                                AfterDoctypeSystemId
                            };
                            return;
                        }
                        Some('>') => {
                            self.error();
                            self.doctype.force_quirks = true;
                            self.state = Data;
                            self.emit_doctype();
                            return;
                        }
                        None => {
                            self.error();
                            self.doctype.force_quirks = true;
                            self.emit_doctype();
                            self.emit_eof();
                            return;
                        }
                        Some(c) => {
                            let c = if c == '\0' {
                                self.error();
                                REPLACEMENT
                            } else {
                                c
                            };
                            let id = if public {
                                &mut self.doctype.public_id
                            } else {
                                &mut self.doctype.system_id
                            };
                            id.get_or_insert_default().push(c);
                        }
                    }
                }
            }
            AfterDoctypePublicId | BetweenDoctypePublicAndSystemIds => {
                let after = self.state == AfterDoctypePublicId;
                match self.next_char() {
                    Some(c) if is_ws(c) => {
                        self.state = BetweenDoctypePublicAndSystemIds;
                    }
                    Some('>') => {
                        self.state = Data;
                        self.emit_doctype();
                    }
                    Some(q @ ('"' | '\'')) => {
                        if after {
                            self.error();
                        }
                        self.start_doctype_id(false, q);
                    }
                    None => {
                        self.error();
                        self.doctype.force_quirks = true;
                        self.emit_doctype();
                        self.emit_eof();
                    }
                    Some(_) => {
                        self.error();
                        self.doctype.force_quirks = true;
                        self.unread();
                        self.state = BogusDoctype;
                    }
                }
            }
            AfterDoctypeSystemId => match self.next_char() {
                Some(c) if is_ws(c) => {}
                Some('>') => {
                    self.state = Data;
                    self.emit_doctype();
                }
                None => {
                    self.error();
                    self.doctype.force_quirks = true;
                    self.emit_doctype();
                    self.emit_eof();
                }
                Some(_) => {
                    // unexpected-character-after-doctype-system-identifier: no force-quirks.
                    self.error();
                    self.unread();
                    self.state = BogusDoctype;
                }
            },
            BogusDoctype => loop {
                match self.next_char() {
                    Some('>') => {
                        self.state = Data;
                        self.emit_doctype();
                        return;
                    }
                    Some('\0') => self.error(),
                    Some(_) => {}
                    None => {
                        self.emit_doctype();
                        self.emit_eof();
                        return;
                    }
                }
            },
            CdataSection => loop {
                match self.next_char() {
                    Some(']') => {
                        self.state = CdataSectionBracket;
                        return;
                    }
                    Some(c) => self.emit_char(c),
                    None => {
                        self.error();
                        self.emit_eof();
                        return;
                    }
                }
            },
            CdataSectionBracket => match self.next_char() {
                Some(']') => self.state = CdataSectionEnd,
                _ => {
                    self.emit_char(']');
                    self.unread();
                    self.state = CdataSection;
                }
            },
            CdataSectionEnd => match self.next_char() {
                Some(']') => self.emit_char(']'),
                Some('>') => self.state = Data,
                _ => {
                    self.emit_str("]]");
                    self.unread();
                    self.state = CdataSection;
                }
            },
        }
    }

    fn start_doctype_id(&mut self, public: bool, quote: char) {
        use State::*;
        if public {
            self.doctype.public_id = Some(String::new());
            self.state = if quote == '"' {
                DoctypePublicIdDoubleQuoted
            } else {
                DoctypePublicIdSingleQuoted
            };
        } else {
            self.doctype.system_id = Some(String::new());
            self.state = if quote == '"' {
                DoctypeSystemIdDoubleQuoted
            } else {
                DoctypeSystemIdSingleQuoted
            };
        }
    }
}

/// Numeric references in the C1 range map through windows-1252, as the spec requires.
fn windows_1252_replacement(v: u32) -> Option<char> {
    let c = match v {
        0x80 => '\u{20AC}',
        0x82 => '\u{201A}',
        0x83 => '\u{0192}',
        0x84 => '\u{201E}',
        0x85 => '\u{2026}',
        0x86 => '\u{2020}',
        0x87 => '\u{2021}',
        0x88 => '\u{02C6}',
        0x89 => '\u{2030}',
        0x8A => '\u{0160}',
        0x8B => '\u{2039}',
        0x8C => '\u{0152}',
        0x8E => '\u{017D}',
        0x91 => '\u{2018}',
        0x92 => '\u{2019}',
        0x93 => '\u{201C}',
        0x94 => '\u{201D}',
        0x95 => '\u{2022}',
        0x96 => '\u{2013}',
        0x97 => '\u{2014}',
        0x98 => '\u{02DC}',
        0x99 => '\u{2122}',
        0x9A => '\u{0161}',
        0x9B => '\u{203A}',
        0x9C => '\u{0153}',
        0x9E => '\u{017E}',
        0x9F => '\u{0178}',
        _ => return None,
    };
    Some(c)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tokens(input: &str) -> Vec<Token> {
        let mut t = Tokenizer::new(input);
        let mut out = Vec::new();
        loop {
            let tok = t.next_token();
            if tok == Token::Eof {
                break;
            }
            out.push(tok);
        }
        out
    }

    fn start(name: &str, attrs: &[(&str, &str)], self_closing: bool) -> Token {
        Token::StartTag(Tag {
            name: name.into(),
            attrs: attrs.iter().map(|(a, b)| (a.to_string(), b.to_string())).collect(),
            self_closing,
        })
    }

    #[test]
    fn tags_and_attributes() {
        let t = tokens(r#"<DIV Class="a" id='b' data-x=c checked/>hi</div>"#);
        assert_eq!(
            t,
            vec![
                start(
                    "div",
                    &[("class", "a"), ("id", "b"), ("data-x", "c"), ("checked", "")],
                    true
                ),
                Token::Characters("hi".into()),
                Token::EndTag(Tag {
                    name: "div".into(),
                    ..Default::default()
                }),
            ]
        );
        let t = tokens("<br/><img a=1 a=2>");
        assert_eq!(t, vec![start("br", &[], true), start("img", &[("a", "1")], false)]);
    }

    #[test]
    fn comments_and_doctype() {
        let t = tokens("<!DOCTYPE html><!-- x -- y --><!--->");
        assert_eq!(
            t,
            vec![
                Token::Doctype(Doctype {
                    name: Some("html".into()),
                    ..Default::default()
                }),
                Token::Comment(" x -- y ".into()),
                Token::Comment("".into()),
            ]
        );
        let t = tokens(r#"<!doctype html PUBLIC "-//W3C//DTD HTML 4.01//EN" 'x'>"#);
        assert_eq!(
            t,
            vec![Token::Doctype(Doctype {
                name: Some("html".into()),
                public_id: Some("-//W3C//DTD HTML 4.01//EN".into()),
                system_id: Some("x".into()),
                force_quirks: false,
            })]
        );
    }

    #[test]
    fn character_references() {
        assert_eq!(
            tokens("&amp;&lt;&#65;&#x42;&notin;&notit;"),
            vec![Token::Characters("&<AB∉¬it;".into())]
        );
        assert_eq!(
            tokens("&#x80;&#0;&bogus;&"),
            vec![Token::Characters("€\u{FFFD}&bogus;&".into())]
        );
        // In attributes, a legacy entity followed by '=' or alnum is not decoded.
        assert_eq!(
            tokens(r#"<a href="?x=1&copy=2&copy;">"#),
            vec![start("a", &[("href", "?x=1&copy=2©")], false)]
        );
    }

    #[test]
    fn rawtext_states() {
        let mut t = Tokenizer::new("a<b></style>c");
        t.state = State::Rawtext;
        t.set_last_start_tag(Some("style"));
        assert_eq!(t.next_token(), Token::Characters("a<b>".into()));
        assert!(matches!(t.next_token(), Token::EndTag(ref tag) if tag.name == "style"));
        assert_eq!(t.next_token(), Token::Characters("c".into()));
    }

    #[test]
    fn newline_normalization() {
        assert_eq!(tokens("a\r\nb\rc"), vec![Token::Characters("a\nb\nc".into())]);
    }
}
