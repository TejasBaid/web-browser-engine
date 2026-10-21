//! Randomized layout robustness: generates deterministic pseudo-random documents that
//! mix every display type, floats, positioning, flex and grid properties, negative and
//! huge margins, percentages, `calc()` and text, then renders them. The test passes if
//! nothing panics and every fragment has finite geometry.

use browser_engine::page::Page;
use url::Url;

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        // xorshift64*
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }
    fn pick<'a>(&mut self, xs: &[&'a str]) -> &'a str {
        xs[(self.next() % xs.len() as u64) as usize]
    }
    fn chance(&mut self, pct: u64) -> bool {
        self.next() % 100 < pct
    }
}

const DISPLAYS: &[&str] = &[
    "block",
    "inline",
    "inline-block",
    "flex",
    "inline-flex",
    "grid",
    "table",
    "table-row",
    "table-cell",
    "list-item",
    "none",
    "contents",
    "flow-root",
];
const LENGTHS: &[&str] = &[
    "0",
    "10px",
    "-15px",
    "50%",
    "100%",
    "auto",
    "3em",
    "calc(100% - 30px)",
    "1e5px",
    "min(40px, 10%)",
    "2vw",
];
const WORDS: &[&str] = &[
    "lorem",
    "ipsum dolor",
    "sit amet,",
    "consectetur",
    "x",
    "averyveryverylongwordwithoutbreaks",
    " ",
];

fn style(r: &mut Rng) -> String {
    let mut s = format!("display:{};", r.pick(DISPLAYS));
    let props: &[(&str, &[&str])] = &[
        ("width", LENGTHS),
        ("height", LENGTHS),
        ("margin", LENGTHS),
        ("padding", &["0", "5px", "10%", "2em"]),
        ("float", &["left", "right", "none"]),
        ("clear", &["both", "left", "none"]),
        ("position", &["static", "relative", "absolute", "fixed", "sticky"]),
        ("top", LENGTHS),
        ("left", LENGTHS),
        ("border", &["1px solid red", "5px dashed blue", "0", "3px double green"]),
        ("border-radius", &["0", "50%", "10px 20px"]),
        ("flex", &["1", "0 0 auto", "2 1 50px", "none"]),
        ("flex-wrap", &["wrap", "nowrap", "wrap-reverse"]),
        ("flex-direction", &["row", "column", "row-reverse", "column-reverse"]),
        (
            "justify-content",
            &["center", "space-between", "flex-end", "space-evenly"],
        ),
        ("align-items", &["stretch", "center", "baseline", "flex-end"]),
        (
            "grid-template-columns",
            &["1fr 2fr", "repeat(auto-fill, minmax(50px, 1fr))", "100px auto", "none"],
        ),
        ("grid-column", &["span 2", "1 / -1", "auto", "3"]),
        ("gap", &["0", "10px", "5%"]),
        ("overflow", &["visible", "hidden"]),
        ("opacity", &["1", "0.5", "0"]),
        ("z-index", &["auto", "-1", "5"]),
        ("white-space", &["normal", "pre", "nowrap", "pre-wrap"]),
        ("vertical-align", &["baseline", "middle", "top", "sub", "10px"]),
        ("text-align", &["left", "center", "justify", "right"]),
        ("line-height", &["normal", "0", "2", "40px"]),
        ("font-size", &["0", "8px", "2em", "48px"]),
        ("box-sizing", &["border-box", "content-box"]),
        ("min-width", LENGTHS),
        ("max-height", LENGTHS),
        ("box-shadow", &["none", "2px 2px 4px black", "0 0 0 3px rgba(0,0,0,.5)"]),
    ];
    for (name, values) in props {
        if r.chance(20) {
            s.push_str(&format!("{name}:{};", r.pick(values)));
        }
    }
    s
}

fn tree(r: &mut Rng, depth: u32, out: &mut String) {
    let n = 1 + r.next() % 5;
    for _ in 0..n {
        match r.next() % 10 {
            0..=2 => out.push_str(r.pick(WORDS)),
            3 => out.push_str("<br>"),
            4 => out.push_str(&format!("<img width={} height={}>", r.next() % 80, r.next() % 80)),
            _ => {
                let tag = r.pick(&["div", "span", "p", "ul", "li", "table", "td", "b", "a href=x"]);
                let name = tag.split(' ').next().unwrap();
                out.push_str(&format!("<{tag} style=\"{}\">", style(r)));
                if depth < 5 {
                    tree(r, depth + 1, out);
                }
                out.push_str(&format!("</{name}>"));
            }
        }
    }
}

#[test]
fn random_documents_lay_out_and_paint() {
    let base = Url::parse("https://example.test/").unwrap();
    let mut rng = Rng(0x9E37_79B9_7F4A_7C15);
    let cases: u32 = std::env::var("RANDOM_CASES")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(400);
    for case in 0..cases {
        let mut html = String::from("<!doctype html><body>");
        tree(&mut rng, 0, &mut html);
        let page = Page::from_html(&html, base.clone());
        let mut r = page.render(400.0, 300.0);
        r.layout.root.walk(&mut |_, rect| {
            assert!(
                rect.x.is_finite() && rect.y.is_finite() && rect.width.is_finite() && rect.height.is_finite(),
                "case {case}: non-finite geometry {rect:?}\n{html}"
            );
        });
        let c = page.paint(&mut r, 400, 300, 0.0);
        assert_eq!(c.width, 400, "case {case}");
    }
}
