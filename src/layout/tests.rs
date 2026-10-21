//! Layout tests with expected box geometries. Fonts are the bundled Liberation faces,
//! so text metrics are deterministic.

use super::*;
use crate::html::parse_document;
use crate::style::{Origin, Stylesheet, Viewport, compute_styles, user_agent_stylesheet};
use crate::text::FontDb;
use std::rc::Rc;

struct Laid {
    doc: Document,
    tree: LayoutTree,
}

impl Laid {
    fn rect(&self, id: &str) -> Rect {
        let n = self.doc.element_by_id(id).unwrap_or_else(|| panic!("no #{id}"));
        self.tree
            .root
            .find_rect(n)
            .unwrap_or_else(|| panic!("#{id} has no fragment"))
    }

    fn rects(&self, id: &str) -> Vec<Rect> {
        let n = self.doc.element_by_id(id).unwrap();
        self.tree.root.find_all_rects(n)
    }
}

fn layout_html(html: &str, width: f32) -> Laid {
    let doc = parse_document(html);
    let mut sheets = vec![user_agent_stylesheet()];
    for s in doc.elements_by_tag("style") {
        sheets.push(Rc::new(Stylesheet::parse(&doc.text_content(s), Origin::Author, None)));
    }
    let styles = compute_styles(&doc, &sheets, Viewport { width, height: 600.0 });
    let fonts = FontDb::new();
    let res = Resources::default();
    let tree = layout_document(&doc, &styles, &fonts, &res, (width, 600.0));
    Laid { doc, tree }
}

fn assert_rect(r: Rect, x: f32, y: f32, w: f32, h: f32) {
    let ok =
        (r.x - x).abs() < 0.51 && (r.y - y).abs() < 0.51 && (r.width - w).abs() < 0.51 && (r.height - h).abs() < 0.51;
    assert!(
        ok,
        "expected ({x}, {y}, {w}x{h}), got ({}, {}, {}x{})",
        r.x, r.y, r.width, r.height
    );
}

const RESET: &str = "<style>body{margin:0} div{box-sizing:content-box}</style>";

#[test]
fn block_widths_padding_border_and_auto_margins() {
    let l = layout_html(
        &format!(
            "{RESET}<div id=a style='width:200px;padding:10px;border:5px solid;margin:0 auto'></div>
             <div id=b style='width:200px;padding:10px;border:5px solid;box-sizing:border-box;margin-left:auto'></div>
             <div id=c style='margin:0 20px;height:10px'></div>"
        ),
        800.0,
    );
    assert_rect(l.rect("a"), 285.0, 0.0, 230.0, 30.0);
    assert_rect(l.rect("b"), 600.0, 30.0, 200.0, 30.0);
    assert_rect(l.rect("c"), 20.0, 60.0, 760.0, 10.0);
}

#[test]
fn min_max_width_and_height() {
    let l = layout_html(
        &format!(
            "{RESET}<div id=a style='width:50%;max-width:300px;min-height:40px'></div>
             <div id=b style='width:10px;min-width:100px;height:500px;max-height:50px'></div>"
        ),
        800.0,
    );
    assert_rect(l.rect("a"), 0.0, 0.0, 300.0, 40.0);
    assert_rect(l.rect("b"), 0.0, 40.0, 100.0, 50.0);
}

#[test]
fn sibling_and_parent_child_margin_collapsing() {
    let l = layout_html(
        &format!(
            "{RESET}<div id=a style='height:10px;margin-bottom:20px'></div>
             <div id=b style='height:10px;margin-top:30px'></div>
             <div id=p style='margin-top:10px'><div id=c style='margin-top:25px;height:10px'></div></div>
             <div id=e style='margin:15px 0 40px'></div>
             <div id=d style='height:10px;margin-top:-10px'></div>"
        ),
        800.0,
    );
    assert_rect(l.rect("a"), 0.0, 0.0, 800.0, 10.0);
    assert_rect(l.rect("b"), 0.0, 40.0, 800.0, 10.0);
    // Parent's 10px and child's 25px collapse to 25px; both tops coincide.
    assert_rect(l.rect("p"), 0.0, 75.0, 800.0, 10.0);
    assert_rect(l.rect("c"), 0.0, 75.0, 800.0, 10.0);
    // Empty box: its margins collapse through with the next sibling's: max(40) + (-10).
    assert_rect(l.rect("d"), 0.0, 85.0 + 40.0 - 10.0, 800.0, 10.0);
}

#[test]
fn padding_prevents_parent_child_collapse() {
    let l = layout_html(
        &format!(
            "{RESET}<div id=p style='padding-top:1px;margin-top:10px'><div id=c style='margin-top:20px;height:5px'></div></div>"
        ),
        800.0,
    );
    assert_rect(l.rect("p"), 0.0, 10.0, 800.0, 26.0);
    assert_rect(l.rect("c"), 0.0, 31.0, 800.0, 5.0);
}

#[test]
fn body_margin_collapses_with_first_paragraph() {
    let l = layout_html("<p id=p>Hello</p>", 800.0);
    // p's 16px top margin collapses with body's 8px.
    let r = l.rect("p");
    assert_rect(Rect::new(r.x, r.y, r.width, 0.0), 8.0, 16.0, 784.0, 0.0);
}

#[test]
fn inline_text_wraps_at_container_width() {
    let l = layout_html(
        &format!("{RESET}<div id=d style='width:100px;font:20px/30px \"Liberation Mono\"'>aaaa bbbb cccc</div>"),
        800.0,
    );
    // Mono advance = 12.0px at 20px: "aaaa " (60px) + "bbbb" (48px) > 100px, so one word per line.
    assert_rect(l.rect("d"), 0.0, 0.0, 100.0, 90.0);
}

#[test]
fn line_height_and_baseline() {
    let l = layout_html(
        &format!(
            "{RESET}<div id=d style='font:16px \"Liberation Sans\"'>x</div><div id=e style='font:16px/2 \"Liberation Sans\"'>x</div>"
        ),
        800.0,
    );
    // normal line-height for Liberation Sans 16px = 14 + 3 + 1 = 18 (Chrome rounding).
    assert_rect(l.rect("d"), 0.0, 0.0, 800.0, 18.0);
    assert_rect(l.rect("e"), 0.0, 18.0, 800.0, 32.0);
}

#[test]
fn inline_box_with_padding_spans_lines() {
    let l = layout_html(
        &format!(
            "{RESET}<div style='width:130px;font:20px/30px \"Liberation Mono\"'>aa <span id=s style='padding:0 5px;border-left:3px solid'>bbbb cccc</span></div>"
        ),
        800.0,
    );
    let rs = l.rects("s");
    assert_eq!(rs.len(), 2, "split into two line fragments: {rs:?}");
    // First fragment starts after "aa " (36px) and has the left border+padding.
    assert!((rs[0].x - 36.0).abs() < 0.5, "{rs:?}");
    assert!((rs[1].x - 0.0).abs() < 0.5, "{rs:?}");
    assert!(rs[1].y > rs[0].y);
}

#[test]
fn floats_and_text_wrapping_around_them() {
    let l = layout_html(
        &format!(
            "{RESET}<div style='width:300px'><div id=f style='float:left;width:100px;height:50px'></div>
             <div id=r style='float:right;width:50px;height:20px'></div><p id=p style='margin:0'>text</p>
             <div id=c style='clear:both;height:5px'></div></div>"
        ),
        800.0,
    );
    assert_rect(l.rect("f"), 0.0, 0.0, 100.0, 50.0);
    assert_rect(l.rect("r"), 250.0, 0.0, 50.0, 20.0);
    // The paragraph box itself is full width; its line box is shortened by the floats.
    assert_rect(l.rect("c"), 0.0, 50.0, 300.0, 5.0);
    let mut line_x = None;
    l.tree.root.walk(&mut |f, r| {
        if f.kind == FragmentKind::Line && line_x.is_none() && f.style.display == crate::style::computed::Display::Block
        {
            line_x = Some(r.x);
        }
    });
    assert_eq!(line_x, Some(100.0));
}

#[test]
fn overflow_hidden_block_avoids_floats() {
    let l = layout_html(
        &format!(
            "{RESET}<div id=f style='float:left;width:100px;height:50px'></div><div id=b style='overflow:hidden;height:20px'></div>"
        ),
        400.0,
    );
    assert_rect(l.rect("b"), 100.0, 0.0, 300.0, 20.0);
}

#[test]
fn relative_absolute_and_fixed_positioning() {
    let l = layout_html(
        &format!(
            "{RESET}<div id=rel style='position:relative;left:10px;top:5px;margin-top:20px;height:100px;padding:10px'>
               <div id=abs style='position:absolute;right:0;bottom:0;width:30px;height:20px'></div>
               <div id=st style='position:absolute;width:5px;height:5px'></div>
             </div>
             <div id=fix style='position:fixed;left:0;right:0;bottom:0;height:10px'></div>"
        ),
        800.0,
    );
    assert_rect(l.rect("rel"), 10.0, 25.0, 800.0, 120.0);
    // abspos: bottom-right of rel's padding box (rel is 800x120 at (10,25)).
    assert_rect(l.rect("abs"), 10.0 + 800.0 - 30.0, 25.0 + 120.0 - 20.0, 30.0, 20.0);
    // Static position: rel's content box origin.
    assert_rect(l.rect("st"), 20.0, 35.0, 5.0, 5.0);
    assert_rect(l.rect("fix"), 0.0, 590.0, 800.0, 10.0);
}

#[test]
fn display_none_inline_block_and_list_items() {
    let l = layout_html(
        &format!(
            "{RESET}<div id=h style='display:none;height:50px'></div>
             <span id=ib style='display:inline-block;width:40px;height:30px'></span>
             <ul style='margin:0;padding-left:40px'><li id=li>item</li></ul>"
        ),
        800.0,
    );
    assert!(l.tree.root.find_rect(l.doc.element_by_id("h").unwrap()).is_none());
    let ib = l.rect("ib");
    assert_eq!((ib.width, ib.height), (40.0, 30.0));
    let li = l.rect("li");
    assert_eq!(li.x, 40.0);
    let mut marker = false;
    l.tree.root.walk(&mut |f, r| {
        if let FragmentKind::Marker(t) = &f.kind {
            marker = true;
            assert!(t.text.starts_with('\u{2022}'));
            assert!(r.right() <= 40.5, "outside marker sits left of the content: {r:?}");
        }
    });
    assert!(marker);
}

#[test]
fn flex_row_grow_shrink_and_justify() {
    let l = layout_html(
        &format!(
            "{RESET}<div style='display:flex;width:600px'>
               <div id=a style='flex:1;height:10px'></div><div id=b style='flex:2;height:20px'></div><div id=c style='width:100px'></div>
             </div>
             <div style='display:flex;width:300px;justify-content:space-between'>
               <div id=d style='width:50px;height:5px'></div><div id=e style='width:50px'></div><div id=f style='width:50px'></div>
             </div>
             <div style='display:flex;width:200px'><div id=g style='width:150px;flex-shrink:1'></div><div id=h style='width:150px;flex-shrink:3'></div></div>"
        ),
        800.0,
    );
    assert_rect(l.rect("a"), 0.0, 0.0, 500.0 / 3.0, 10.0);
    assert_rect(l.rect("b"), 500.0 / 3.0, 0.0, 1000.0 / 3.0, 20.0);
    assert_rect(l.rect("c"), 500.0, 0.0, 100.0, 20.0);
    assert_rect(l.rect("d"), 0.0, 20.0, 50.0, 5.0);
    assert_rect(l.rect("e"), 125.0, 20.0, 50.0, 5.0);
    assert_rect(l.rect("f"), 250.0, 20.0, 50.0, 5.0);
    // Overflow 100px shrinks proportionally to shrink * base: 1:3.
    assert!((l.rect("g").width - 125.0).abs() < 0.5);
    assert!((l.rect("h").width - 75.0).abs() < 0.5);
}

#[test]
fn flex_column_wrap_gap_and_align() {
    let l = layout_html(
        &format!(
            "{RESET}<div style='display:flex;flex-wrap:wrap;gap:10px;width:250px;align-items:center'>
               <div id=a style='width:100px;height:20px'></div><div id=b style='width:100px;height:40px'></div><div id=c style='width:100px;height:10px'></div>
             </div>
             <div id=col style='display:flex;flex-direction:column;height:100px;justify-content:flex-end;align-items:flex-end;width:300px'>
               <div id=d style='width:50px;height:30px'></div>
             </div>"
        ),
        800.0,
    );
    assert_rect(l.rect("a"), 0.0, 10.0, 100.0, 20.0);
    assert_rect(l.rect("b"), 110.0, 0.0, 100.0, 40.0);
    assert_rect(l.rect("c"), 0.0, 50.0, 100.0, 10.0);
    let col = l.rect("col");
    assert_rect(l.rect("d"), 250.0, col.y + 70.0, 50.0, 30.0);
}

#[test]
fn flex_stretch_and_order() {
    let l = layout_html(
        &format!(
            "{RESET}<div style='display:flex;height:80px'><div id=a style='width:10px;order:2'></div><div id=b style='width:20px'></div></div>"
        ),
        800.0,
    );
    assert_rect(l.rect("b"), 0.0, 0.0, 20.0, 80.0);
    assert_rect(l.rect("a"), 20.0, 0.0, 10.0, 80.0);
}

#[test]
fn table_columns_and_spacing() {
    let l = layout_html(
        &format!(
            "{RESET}<table id=t style='border-spacing:4px'><tr><td id=a style='width:100px;padding:0;height:20px'></td><td id=b style='width:50px;padding:0'></td></tr>
             <tr><td id=c colspan=2 style='padding:0;height:10px'></td></tr></table>"
        ),
        800.0,
    );
    assert_rect(
        l.rect("t"),
        0.0,
        0.0,
        4.0 + 100.0 + 4.0 + 50.0 + 4.0,
        4.0 + 20.0 + 4.0 + 10.0 + 4.0,
    );
    assert_rect(l.rect("a"), 4.0, 4.0, 100.0, 20.0);
    assert_rect(l.rect("b"), 108.0, 4.0, 50.0, 20.0);
    assert_rect(l.rect("c"), 4.0, 28.0, 154.0, 10.0);
}

#[test]
fn table_percentage_width_distributes_extra() {
    let l = layout_html(
        &format!(
            "{RESET}<table width=400 cellspacing=0 cellpadding=0><tr><td id=a>x</td><td id=b>yyyyyyyyyy</td></tr></table>"
        ),
        800.0,
    );
    let a = l.rect("a");
    let b = l.rect("b");
    assert!((a.width + b.width - 400.0).abs() < 0.5, "{a:?} {b:?}");
    assert!(b.width > a.width);
}

#[test]
fn images_use_intrinsic_or_attribute_sizes() {
    let l = layout_html(
        &format!("{RESET}<img id=a width=40 height=30><img id=b style='width:20px;height:10px;display:block'>"),
        800.0,
    );
    let a = l.rect("a");
    assert_eq!((a.width, a.height), (40.0, 30.0));
    let b = l.rect("b");
    assert_eq!((b.width, b.height), (20.0, 10.0));
}

#[test]
fn vertical_align_middle_and_top() {
    let l = layout_html(
        &format!(
            "{RESET}<div id=d style='font:16px/20px \"Liberation Sans\"'>x<span id=big style='display:inline-block;width:10px;height:60px;vertical-align:top'></span><span id=m style='display:inline-block;width:10px;height:10px;vertical-align:middle'></span></div>"
        ),
        800.0,
    );
    assert_rect(l.rect("big"), l.rect("big").x, 0.0, 10.0, 60.0);
    assert_eq!(l.rect("d").height, 60.0);
}

#[test]
fn block_in_inline_splits_the_inline() {
    let l = layout_html(
        &format!("{RESET}<span id=s>before<div id=d>block</div>after</span>"),
        800.0,
    );
    let d = l.rect("d");
    assert!(d.y > 0.0 && d.width == 800.0);
    assert_eq!(l.rects("s").len(), 2);
}

#[test]
fn grid_tracks_placement_and_areas() {
    let l = layout_html(
        &format!(
            "{RESET}<div style='display:grid;grid-template-columns:100px 1fr 2fr;gap:10px;width:400px'>
               <div id=a style='height:20px'></div><div id=b></div><div id=c></div>
               <div id=d style='grid-column:1 / span 2;height:30px'></div><div id=e style='grid-column:-2'></div>
             </div>
             <div style='display:grid;grid-template-areas:\"head head\" \"nav main\";grid-template-columns:50px 1fr;grid-template-rows:40px 60px;width:300px'>
               <div id=main style='grid-area:main'></div><div id=head style='grid-area:head'></div><div id=nav style='grid-area:nav'></div>
             </div>"
        ),
        800.0,
    );
    // 400 - 100 - 2*10 = 280 split 1:2.
    assert_rect(l.rect("a"), 0.0, 0.0, 100.0, 20.0);
    assert_rect(l.rect("b"), 110.0, 0.0, 280.0 / 3.0, 20.0);
    assert_rect(l.rect("c"), 110.0 + 280.0 / 3.0 + 10.0, 0.0, 560.0 / 3.0, 20.0);
    // Second row: d spans columns 1-2, e goes in the last column.
    assert_rect(l.rect("d"), 0.0, 30.0, 110.0 + 280.0 / 3.0, 30.0);
    assert_rect(l.rect("e"), 110.0 + 280.0 / 3.0 + 10.0, 30.0, 560.0 / 3.0, 30.0);
    let top = l.rect("head").y;
    assert_rect(l.rect("head"), 0.0, top, 300.0, 40.0);
    assert_rect(l.rect("nav"), 0.0, top + 40.0, 50.0, 60.0);
    assert_rect(l.rect("main"), 50.0, top + 40.0, 250.0, 60.0);
}

#[test]
fn grid_auto_fill_and_alignment() {
    let l = layout_html(
        &format!(
            "{RESET}<div style='display:grid;grid-template-columns:repeat(auto-fill, 100px);width:350px;justify-items:center;grid-auto-rows:50px'>
               <div id=a style='width:20px;height:10px'></div><div></div><div></div><div id=d style='align-self:end;height:10px'></div>
             </div>"
        ),
        800.0,
    );
    // Three 100px columns fit; the fourth item wraps to row 2.
    assert_rect(l.rect("a"), 40.0, 0.0, 20.0, 10.0);
    let d = l.rect("d");
    assert_eq!((d.y, d.height), (90.0, 10.0));
}

#[test]
fn overflow_wrap_and_word_break_split_long_words() {
    let l = layout_html(
        &format!(
            "{RESET}<div id=n style='width:60px;font:20px/30px \"Liberation Mono\"'>abcdefghij</div>
             <div id=o style='width:60px;font:20px/30px \"Liberation Mono\";overflow-wrap:anywhere'>abcdefghij</div>
             <div id=w style='width:60px;font:20px/30px \"Liberation Mono\";word-break:break-all'>ab abcdefgh</div>"
        ),
        800.0,
    );
    // 12px per glyph: 10 glyphs overflow a 60px line by default, but break into 5+5 with overflow-wrap.
    assert_eq!(l.rect("n").height, 30.0);
    assert_eq!(l.rect("o").height, 60.0);
    // break-all fills lines greedily character by character: "ab ab" / "cdefg" / "h".
    assert_eq!(l.rect("w").height, 90.0);
}

#[test]
fn translate_transforms_offset_boxes() {
    let l = layout_html(
        &format!(
            "{RESET}<div style='position:relative;width:400px;height:200px'>
               <div id=c style='position:absolute;left:50%;top:50%;width:100px;height:40px;transform:translate(-50%, -50%)'></div>
             </div>
             <div id=t style='width:10px;height:10px;transform:translateX(30px) translateY(5px) rotate(10deg)'></div>"
        ),
        800.0,
    );
    assert_rect(l.rect("c"), 150.0, 80.0, 100.0, 40.0);
    assert_rect(l.rect("t"), 30.0, 205.0, 10.0, 10.0);
}

#[test]
fn text_overflow_ellipsis_truncates_lines() {
    let l = layout_html(
        &format!(
            "{RESET}<div id=e style='width:100px;white-space:nowrap;overflow:hidden;text-overflow:ellipsis;font:20px \"Liberation Mono\"'>abcdefghijkl</div>"
        ),
        800.0,
    );
    let mut texts = Vec::new();
    l.tree.root.walk(&mut |f, r| {
        if let FragmentKind::Text(t) = &f.kind {
            texts.push((t.text.clone(), r.right()));
        }
    });
    // 12px glyphs: 7 fit before the 12px ellipsis in 100px.
    assert_eq!(texts[0].0, "abcdefg");
    assert_eq!(texts[1].0, "\u{2026}");
    assert!(texts[1].1 <= 100.5, "{texts:?}");
}

#[test]
fn css_counters_number_generated_content() {
    let l = layout_html(
        "<style>body{counter-reset:sec} h2{counter-increment:sec; counter-reset:sub} h2::before{content:counter(sec) \". \"}
         h3{counter-increment:sub} h3::before{content:counters(sec, \"-\") \".\" counter(sub, lower-alpha) \" \"}</style>
         <h2>A</h2><h3>x</h3><h3>y</h3><h2>B</h2><h3>z</h3>",
        800.0,
    );
    let mut texts = Vec::new();
    l.tree.root.walk(&mut |f, _| {
        if let FragmentKind::Text(t) = &f.kind {
            texts.push(t.text.clone());
        }
    });
    let joined = texts.join("|");
    for expected in ["1. ", "1.a ", "1.b ", "2. ", "2.a "] {
        assert!(joined.contains(expected), "missing {expected:?} in {joined}");
    }
}
