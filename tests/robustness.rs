//! Robustness: every input of the html5lib tree-construction suite (deliberately
//! malformed markup: misnested tags, broken tables, foreign content, framesets...) is
//! pushed through the whole pipeline — style, layout, display list and rasterization —
//! at two viewport sizes. The test passes if nothing panics.

use browser_engine::page::Page;
use url::Url;

fn inputs() -> Vec<String> {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/html5lib-tests/tree-construction");
    let mut out = Vec::new();
    let mut files: Vec<_> = std::fs::read_dir(dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .collect();
    files.sort();
    for f in files.into_iter().filter(|p| p.extension().is_some_and(|e| e == "dat")) {
        let text = std::fs::read_to_string(&f).unwrap();
        let mut cur: Option<String> = None;
        for line in text.split('\n') {
            if line == "#data" {
                if let Some(c) = cur.take() {
                    out.push(c);
                }
                cur = Some(String::new());
            } else if line.starts_with('#') {
                if let Some(c) = cur.take() {
                    out.push(c);
                }
            } else if let Some(c) = cur.as_mut() {
                if !c.is_empty() {
                    c.push('\n');
                }
                c.push_str(line);
            }
        }
    }
    out
}

#[test]
fn malformed_documents_render_without_panicking() {
    let cases = inputs();
    assert!(cases.len() > 1500, "found {} inputs", cases.len());
    let base = Url::parse("https://example.test/").unwrap();
    for (i, html) in cases.iter().enumerate() {
        let page = Page::from_html(html, base.clone());
        for (w, h) in [(320u32, 200u32), (900, 300)] {
            let mut r = page.render(w as f32, h as f32);
            let canvas = page.paint(&mut r, w, h, 0.0);
            assert_eq!(canvas.data.len(), (w * h * 4) as usize, "case {i}");
        }
    }
}
