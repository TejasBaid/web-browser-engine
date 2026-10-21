//! Conformance tests against the official [html5lib-tests] suites, vendored in
//! `tests/html5lib-tests/` (pinned commit, see `scripts/fetch-html5lib-tests.sh`).
//!
//! * **Tokenizer** (`tokenizer/*.test`): every test is run in each of its `initialStates`,
//!   honouring `lastStartTag` and `doubleEscaped`. Only the *token stream* is compared
//!   (adjacent character tokens are coalesced on both sides). Parse errors are **not**
//!   compared: the tokenizer counts errors but does not report error codes.
//!   Tests whose input or output contains a lone surrogate (which cannot be represented
//!   in a Rust `String`) and the XML-violation tests (`xmlViolationTests`, a different
//!   tokenizer mode) are skipped.
//! * **Tree construction** (`tree-construction/*.dat`): document and `#document-fragment`
//!   tests, serialized in the html5lib tree format and compared exactly. `#script-off` tests
//!   run with the scripting flag off, `#script-on` tests with [`ParseOptions::scripting`] set
//!   (which only changes how `<noscript>` is parsed, since no script is ever executed), and
//!   unmarked tests must pass in both modes. The `scripted/` tests, which need
//!   `document.write`, are not vendored.
//!
//! Run with `cargo test --test html5lib -- --nocapture` to see the per-file summary, and
//! with `HTML5LIB_VERBOSE=1` to print every failing test's input, expected and actual output.
//!
//! [html5lib-tests]: https://github.com/html5lib/html5lib-tests

use browser_engine::dom::{Namespace, serialize_html5lib};
use browser_engine::html::{ParseOptions, State, Token, Tokenizer, parse_document_with, parse_fragment};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

/// Regression floors: a little under the counts achieved when the suite was last updated.
const TOKENIZER_FLOOR: usize = 7020;
const TREE_FLOOR: usize = 1785;

fn suite_dir(sub: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/html5lib-tests")
        .join(sub)
}

fn verbose() -> bool {
    std::env::var("HTML5LIB_VERBOSE").is_ok_and(|v| v != "0")
}

fn files_with_ext(dir: &Path, ext: &str) -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = fs::read_dir(dir)
        .unwrap_or_else(|e| panic!("cannot read {}: {e}", dir.display()))
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x == ext))
        .collect();
    v.sort();
    v
}

#[derive(Default, Clone, Copy)]
struct Tally {
    passed: usize,
    total: usize,
    skipped: usize,
}

// ------------------------------------------------------------------ minimal JSON

#[derive(Debug, Clone, PartialEq)]
enum Json {
    Null,
    Bool(bool),
    Num(f64),
    /// A string plus a flag that is set if it contained a lone surrogate escape.
    Str(String, bool),
    Arr(Vec<Json>),
    Obj(Vec<(String, Json)>),
}

impl Json {
    fn get(&self, key: &str) -> Option<&Json> {
        match self {
            Json::Obj(kv) => kv.iter().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }
    fn as_str(&self) -> Option<&str> {
        match self {
            Json::Str(s, _) => Some(s),
            _ => None,
        }
    }
    fn as_arr(&self) -> &[Json] {
        match self {
            Json::Arr(a) => a,
            _ => &[],
        }
    }
    fn lossy(&self) -> bool {
        match self {
            Json::Str(_, l) => *l,
            Json::Arr(a) => a.iter().any(Json::lossy),
            Json::Obj(kv) => kv.iter().any(|(_, v)| v.lossy()),
            _ => false,
        }
    }
}

struct JsonParser<'a> {
    s: &'a [u8],
    i: usize,
}

impl JsonParser<'_> {
    fn parse(text: &str) -> Json {
        let mut p = JsonParser {
            s: text.as_bytes(),
            i: 0,
        };
        let v = p.value();
        p.ws();
        assert_eq!(p.i, p.s.len(), "trailing JSON data");
        v
    }
    fn ws(&mut self) {
        while self.i < self.s.len() && self.s[self.i].is_ascii_whitespace() {
            self.i += 1;
        }
    }
    fn value(&mut self) -> Json {
        self.ws();
        match self.s[self.i] {
            b'{' => {
                self.i += 1;
                let mut kv = Vec::new();
                loop {
                    self.ws();
                    if self.s[self.i] == b'}' {
                        self.i += 1;
                        break;
                    }
                    let Json::Str(k, _) = self.value() else {
                        panic!("object key")
                    };
                    self.ws();
                    assert_eq!(self.s[self.i], b':');
                    self.i += 1;
                    let v = self.value();
                    kv.push((k, v));
                    self.ws();
                    if self.s[self.i] == b',' {
                        self.i += 1;
                    }
                }
                Json::Obj(kv)
            }
            b'[' => {
                self.i += 1;
                let mut a = Vec::new();
                loop {
                    self.ws();
                    if self.s[self.i] == b']' {
                        self.i += 1;
                        break;
                    }
                    a.push(self.value());
                    self.ws();
                    if self.s[self.i] == b',' {
                        self.i += 1;
                    }
                }
                Json::Arr(a)
            }
            b'"' => {
                let (s, lossy) = self.string();
                Json::Str(s, lossy)
            }
            b't' => {
                self.i += 4;
                Json::Bool(true)
            }
            b'f' => {
                self.i += 5;
                Json::Bool(false)
            }
            b'n' => {
                self.i += 4;
                Json::Null
            }
            _ => {
                let start = self.i;
                while self.i < self.s.len() && matches!(self.s[self.i], b'-' | b'+' | b'.' | b'e' | b'E' | b'0'..=b'9')
                {
                    self.i += 1;
                }
                let t = std::str::from_utf8(&self.s[start..self.i]).unwrap();
                Json::Num(t.parse().unwrap_or_else(|_| panic!("bad number {t:?}")))
            }
        }
    }
    fn hex4(&mut self) -> u32 {
        let h = std::str::from_utf8(&self.s[self.i..self.i + 4]).unwrap();
        self.i += 4;
        u32::from_str_radix(h, 16).unwrap()
    }
    fn string(&mut self) -> (String, bool) {
        self.i += 1;
        let mut out = String::new();
        let mut lossy = false;
        loop {
            let b = self.s[self.i];
            match b {
                b'"' => {
                    self.i += 1;
                    return (out, lossy);
                }
                b'\\' => {
                    self.i += 1;
                    let e = self.s[self.i];
                    self.i += 1;
                    match e {
                        b'n' => out.push('\n'),
                        b't' => out.push('\t'),
                        b'r' => out.push('\r'),
                        b'b' => out.push('\u{8}'),
                        b'f' => out.push('\u{c}'),
                        b'/' => out.push('/'),
                        b'\\' => out.push('\\'),
                        b'"' => out.push('"'),
                        b'u' => {
                            let cp = self.hex4();
                            push_code_unit(&mut out, &mut lossy, cp, || {
                                if self.s.get(self.i) == Some(&b'\\') && self.s.get(self.i + 1) == Some(&b'u') {
                                    let save = self.i;
                                    self.i += 2;
                                    let lo = self.hex4();
                                    if (0xDC00..0xE000).contains(&lo) {
                                        return Some(lo);
                                    }
                                    self.i = save;
                                }
                                None
                            });
                        }
                        _ => panic!("bad escape"),
                    }
                }
                _ => {
                    // Copy one UTF-8 encoded character.
                    let len = utf8_len(b);
                    out.push_str(std::str::from_utf8(&self.s[self.i..self.i + len]).unwrap());
                    self.i += len;
                }
            }
        }
    }
}

fn utf8_len(b: u8) -> usize {
    match b {
        0x00..=0x7F => 1,
        0xC0..=0xDF => 2,
        0xE0..=0xEF => 3,
        _ => 4,
    }
}

/// Appends a UTF-16 code unit `cp`, combining it with a following low surrogate supplied
/// by `next_low`. Lone surrogates become U+FFFD and set `lossy`.
fn push_code_unit(out: &mut String, lossy: &mut bool, cp: u32, mut next_low: impl FnMut() -> Option<u32>) {
    if (0xD800..0xDC00).contains(&cp) {
        if let Some(lo) = next_low() {
            let c = 0x10000 + ((cp - 0xD800) << 10) + (lo - 0xDC00);
            out.push(char::from_u32(c).unwrap());
            return;
        }
        *lossy = true;
        out.push('\u{FFFD}');
    } else if (0xDC00..0xE000).contains(&cp) {
        *lossy = true;
        out.push('\u{FFFD}');
    } else {
        out.push(char::from_u32(cp).unwrap());
    }
}

/// Undoes the `doubleEscaped` encoding: `\uXXXX` sequences in an already-decoded string.
fn unescape_double(s: &str, lossy: &mut bool) -> String {
    let chars: Vec<char> = s.chars().collect();
    let mut out = String::new();
    let mut i = 0;
    let read_hex = |i: usize| -> Option<u32> {
        if i + 6 <= chars.len() && chars[i] == '\\' && chars[i + 1] == 'u' {
            let h: String = chars[i + 2..i + 6].iter().collect();
            u32::from_str_radix(&h, 16).ok()
        } else {
            None
        }
    };
    while i < chars.len() {
        if let Some(cp) = read_hex(i) {
            i += 6;
            let mut j = i;
            push_code_unit(&mut out, lossy, cp, || {
                let lo = read_hex(j).filter(|lo| (0xDC00..0xE000).contains(lo));
                if lo.is_some() {
                    j += 6;
                }
                lo
            });
            i = j;
        } else {
            out.push(chars[i]);
            i += 1;
        }
    }
    out
}

// ------------------------------------------------------------------ tokenizer suite

/// A token in the html5lib JSON shape, used for comparison.
#[derive(Debug, Clone, PartialEq)]
enum Expected {
    Doctype(Option<String>, Option<String>, Option<String>, bool),
    Start(String, Vec<(String, String)>, bool),
    End(String),
    Comment(String),
    Chars(String),
}

fn coalesce(tokens: Vec<Expected>) -> Vec<Expected> {
    let mut out: Vec<Expected> = Vec::new();
    for t in tokens {
        if let (Expected::Chars(s), Some(Expected::Chars(prev))) = (&t, out.last_mut()) {
            prev.push_str(s);
            continue;
        }
        out.push(t);
    }
    out
}

fn expected_tokens(output: &Json, double: bool, lossy: &mut bool) -> Vec<Expected> {
    let fix = |s: &str, lossy: &mut bool| {
        if double {
            unescape_double(s, lossy)
        } else {
            s.to_string()
        }
    };
    let opt = |j: &Json, lossy: &mut bool| j.as_str().map(|s| fix(s, lossy));
    let mut v = Vec::new();
    for t in output.as_arr() {
        let a = t.as_arr();
        let kind = a[0].as_str().unwrap();
        v.push(match kind {
            "DOCTYPE" => Expected::Doctype(
                opt(&a[1], lossy),
                opt(&a[2], lossy),
                opt(&a[3], lossy),
                a[4] == Json::Bool(true),
            ),
            "StartTag" => {
                let attrs = match &a[2] {
                    Json::Obj(kv) => kv
                        .iter()
                        .map(|(k, v)| (fix(k, lossy), fix(v.as_str().unwrap(), lossy)))
                        .collect(),
                    _ => Vec::new(),
                };
                let sc = a.get(3) == Some(&Json::Bool(true));
                Expected::Start(fix(a[1].as_str().unwrap(), lossy), attrs, sc)
            }
            "EndTag" => Expected::End(fix(a[1].as_str().unwrap(), lossy)),
            "Comment" => Expected::Comment(fix(a[1].as_str().unwrap(), lossy)),
            "Character" => Expected::Chars(fix(a[1].as_str().unwrap(), lossy)),
            other => panic!("unknown token kind {other}"),
        });
    }
    coalesce(v)
}

fn run_tokenizer(input: &str, state: State, last_start_tag: Option<&str>) -> Vec<Expected> {
    let mut t = Tokenizer::new(input);
    t.state = state;
    t.set_last_start_tag(last_start_tag);
    let mut v = Vec::new();
    loop {
        match t.next_token() {
            Token::Eof => break,
            Token::Doctype(d) => v.push(Expected::Doctype(d.name, d.public_id, d.system_id, !d.force_quirks)),
            Token::StartTag(tag) => v.push(Expected::Start(tag.name, tag.attrs, tag.self_closing)),
            Token::EndTag(tag) => v.push(Expected::End(tag.name)),
            Token::Comment(c) => v.push(Expected::Comment(c)),
            Token::Characters(s) => v.push(Expected::Chars(s)),
        }
    }
    coalesce(v)
}

fn state_from_name(name: &str) -> State {
    match name {
        "Data state" => State::Data,
        "PLAINTEXT state" => State::Plaintext,
        "RCDATA state" => State::Rcdata,
        "RAWTEXT state" => State::Rawtext,
        "Script data state" => State::ScriptData,
        "CDATA section state" => State::CdataSection,
        other => panic!("unknown initial state {other}"),
    }
}

fn tokenizer_suite() -> (BTreeMap<String, Tally>, Tally) {
    let mut per_file = BTreeMap::new();
    let mut total = Tally::default();
    for path in files_with_ext(&suite_dir("tokenizer"), "test") {
        let json = JsonParser::parse(&fs::read_to_string(&path).unwrap());
        let mut tally = Tally::default();
        if let Some(Json::Arr(skipped)) = json.get("xmlViolationTests") {
            tally.skipped += skipped.len();
        }
        for test in json.get("tests").map(Json::as_arr).unwrap_or(&[]) {
            let desc = test.get("description").and_then(Json::as_str).unwrap_or("");
            let double = test.get("doubleEscaped") == Some(&Json::Bool(true));
            let mut lossy = test.get("input").unwrap().lossy() || test.get("output").unwrap().lossy();
            let raw_input = test.get("input").and_then(Json::as_str).unwrap();
            let input = if double {
                unescape_double(raw_input, &mut lossy)
            } else {
                raw_input.to_string()
            };
            let expected = expected_tokens(test.get("output").unwrap(), double, &mut lossy);
            let states: Vec<State> = match test.get("initialStates") {
                Some(Json::Arr(a)) => a.iter().map(|s| state_from_name(s.as_str().unwrap())).collect(),
                _ => vec![State::Data],
            };
            let last = test.get("lastStartTag").and_then(Json::as_str);
            for state in states {
                if lossy {
                    tally.skipped += 1;
                    continue;
                }
                tally.total += 1;
                let actual = run_tokenizer(&input, state, last);
                if actual == expected {
                    tally.passed += 1;
                } else if verbose() {
                    println!(
                        "TOKENIZER FAIL {} :: {desc} [{state:?}]\n  input:    {input:?}\n  expected: {expected:?}\n  actual:   {actual:?}",
                        path.file_name().unwrap().to_string_lossy()
                    );
                }
            }
        }
        total.passed += tally.passed;
        total.total += tally.total;
        total.skipped += tally.skipped;
        per_file.insert(path.file_name().unwrap().to_string_lossy().into_owned(), tally);
    }
    (per_file, total)
}

// ------------------------------------------------------------------ tree construction suite

struct TreeTest {
    data: String,
    fragment: Option<String>,
    script: Option<bool>,
    document: String,
}

fn parse_dat(text: &str) -> Vec<TreeTest> {
    let mut tests = Vec::new();
    let lines: Vec<&str> = text.split('\n').collect();
    let mut i = 0;
    while i < lines.len() {
        if lines[i] != "#data" {
            i += 1;
            continue;
        }
        i += 1;
        let mut data = Vec::new();
        while i < lines.len() && lines[i] != "#errors" {
            data.push(lines[i]);
            i += 1;
        }
        let mut fragment = None;
        let mut script = None;
        let mut document = Vec::new();
        while i < lines.len() && lines[i] != "#document" {
            match lines[i] {
                "#document-fragment" => {
                    fragment = Some(lines[i + 1].to_string());
                    i += 1;
                }
                "#script-on" => script = Some(true),
                "#script-off" => script = Some(false),
                _ => {}
            }
            i += 1;
        }
        i += 1;
        while i < lines.len() && lines[i] != "#data" {
            document.push(lines[i]);
            i += 1;
        }
        while document.last().is_some_and(|l| l.is_empty()) {
            document.pop();
        }
        let mut doc = document.join("\n");
        if !doc.is_empty() {
            doc.push('\n');
        }
        tests.push(TreeTest {
            data: data.join("\n"),
            fragment,
            script,
            document: doc,
        });
    }
    tests
}

fn run_tree_test(t: &TreeTest, scripting: bool) -> String {
    let opts = ParseOptions { scripting };
    match &t.fragment {
        Some(ctx) => {
            let (ns, name) = if let Some(n) = ctx.strip_prefix("svg ") {
                (Namespace::Svg, n)
            } else if let Some(n) = ctx.strip_prefix("math ") {
                (Namespace::MathMl, n)
            } else {
                (Namespace::Html, ctx.as_str())
            };
            let (doc, root) = parse_fragment(&t.data, ns, name, opts);
            serialize_html5lib(&doc, root)
        }
        None => {
            let doc = parse_document_with(&t.data, opts);
            serialize_html5lib(&doc, doc.root())
        }
    }
}

fn tree_suite() -> (BTreeMap<String, Tally>, Tally) {
    let mut per_file = BTreeMap::new();
    let mut total = Tally::default();
    for path in files_with_ext(&suite_dir("tree-construction"), "dat") {
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        let mut tally = Tally::default();
        for t in parse_dat(&fs::read_to_string(&path).unwrap()) {
            tally.total += 1;
            // Unmarked tests must pass in both scripting modes.
            let modes: &[bool] = match t.script {
                Some(on) => {
                    if on {
                        &[true]
                    } else {
                        &[false]
                    }
                }
                None => &[false, true],
            };
            let mut actual = String::new();
            for &scripting in modes {
                actual = std::panic::catch_unwind(|| run_tree_test(&t, scripting))
                    .unwrap_or_else(|_| "<panic>\n".to_string());
                if actual != t.document {
                    break;
                }
            }
            if actual == t.document {
                tally.passed += 1;
            } else if verbose() {
                println!(
                    "TREE FAIL {name}{}\n--- input\n{}\n--- expected\n{}--- actual\n{}",
                    t.fragment
                        .as_ref()
                        .map(|f| format!(" (fragment: {f})"))
                        .unwrap_or_default(),
                    t.data,
                    t.document,
                    actual
                );
            }
        }
        total.passed += tally.passed;
        total.total += tally.total;
        total.skipped += tally.skipped;
        per_file.insert(name, tally);
    }
    (per_file, total)
}

fn print_table(title: &str, per_file: &BTreeMap<String, Tally>, total: Tally) {
    println!("\n{title}");
    for (name, t) in per_file {
        println!("  {name:<44} {:>5} / {:<5} skipped {}", t.passed, t.total, t.skipped);
    }
    println!(
        "  {:<44} {:>5} / {:<5} skipped {}  ({:.2}%)",
        "TOTAL",
        total.passed,
        total.total,
        total.skipped,
        100.0 * total.passed as f64 / total.total.max(1) as f64
    );
}

#[test]
fn html5lib_conformance() {
    // Silence the default panic hook while tree tests run under catch_unwind.
    let hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(|_| {}));
    let (tok_files, tok) = tokenizer_suite();
    let (tree_files, tree) = tree_suite();
    std::panic::set_hook(hook);

    print_table("html5lib tokenizer tests", &tok_files, tok);
    print_table("html5lib tree-construction tests", &tree_files, tree);
    println!(
        "HTML5LIB tokenizer {}/{} tree {}/{} (tree skipped {})",
        tok.passed, tok.total, tree.passed, tree.total, tree.skipped
    );
    assert!(
        tok.passed >= TOKENIZER_FLOOR,
        "tokenizer conformance regressed: {} < {TOKENIZER_FLOOR}",
        tok.passed
    );
    assert!(
        tree.passed >= TREE_FLOOR,
        "tree-construction conformance regressed: {} < {TREE_FLOOR}",
        tree.passed
    );
}
