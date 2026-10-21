//! End-to-end tests of the public pipeline: HTML string → pixels, links and layout.

use browser_engine::css::color::Color;
use browser_engine::layout::dump_layout;
use browser_engine::page::Page;
use url::Url;

fn page(html: &str) -> Page {
    Page::from_html(html, Url::parse("https://example.test/dir/index.html").unwrap())
}

fn pixel(c: &browser_engine::paint::Canvas, x: u32, y: u32) -> [u8; 4] {
    let i = ((y * c.width + x) * 4) as usize;
    [c.data[i], c.data[i + 1], c.data[i + 2], c.data[i + 3]]
}

#[test]
fn renders_boxes_with_the_cascade_applied() {
    let p = page(
        "<!doctype html><style>body{margin:0} .a{background:red} #b{background:rgb(0,0,255)}
         div{height:50px} @media (max-width: 100px) { .a { background: green } }</style>
         <div class=a></div><div id=b class=a></div>",
    );
    let mut r = p.render(200.0, 200.0);
    let c = p.paint(&mut r, 200, 200, 0.0);
    assert_eq!(pixel(&c, 10, 10), [255, 0, 0, 255]);
    assert_eq!(pixel(&c, 10, 60), [0, 0, 255, 255], "id beats class");
    assert_eq!(pixel(&c, 10, 150), [255, 255, 255, 255], "canvas is white");
}

#[test]
fn canvas_takes_the_body_background() {
    let p = page("<body style='background:#123456'>x</body>");
    let r = p.render(100.0, 100.0);
    assert_eq!(r.display_list.background, Color::rgb(0x12, 0x34, 0x56));
}

#[test]
fn links_are_hit_testable_and_resolved() {
    let p =
        page("<!doctype html><body style='margin:0'><a href='../other.html' style='display:block;height:40px'>go</a>");
    let r = p.render(300.0, 200.0);
    let (_, url) = p.link_at(&r, 10.0, 10.0, 0.0).expect("link");
    assert_eq!(url.as_str(), "https://example.test/other.html");
    assert!(p.link_at(&r, 10.0, 100.0, 0.0).is_none());
}

#[test]
fn scrolling_shifts_content_but_not_fixed_boxes() {
    let p = page(
        "<!doctype html><body style='margin:0'><div style='height:2000px;background:lime'></div>
         <div style='position:fixed;top:0;left:0;width:20px;height:20px;background:red'></div>",
    );
    let mut r = p.render(100.0, 100.0);
    assert_eq!(r.layout.document_height, 2000.0);
    let c = p.paint(&mut r, 100, 100, 500.0);
    assert_eq!(pixel(&c, 5, 5), [255, 0, 0, 255]);
    assert_eq!(pixel(&c, 50, 50), [0, 255, 0, 255]);
}

#[test]
fn dump_layout_lists_fragments() {
    let p = page("<!doctype html><p id=x>Hello world</p>");
    let r = p.render(400.0, 300.0);
    let dump = dump_layout(&r.layout, &p.doc);
    assert!(dump.contains("Box <p>"), "{dump}");
    assert!(dump.contains("Text \"Hello world\""), "{dump}");
}
