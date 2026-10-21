//! Page loading and the rendering pipeline: fetch → parse HTML → collect stylesheets,
//! images and web fonts → style → layout → display list → pixels.
//!
//! Loading is split in two so the GUI can load in a background thread: [`load`]
//! produces a thread-safe [`RawPage`] (the parsed DOM plus raw subresource bytes), and
//! [`Page::from_raw`] turns it into a renderable [`Page`] on the rendering thread.

use crate::css::parser::parse_component_values;
use crate::dom::{Document, NodeId};
use crate::html::parse_document;
use crate::image::Image;
use crate::layout::{LayoutTree, Resources, layout_document};
use crate::net::{self, decode_text};
use crate::paint::{Canvas, DisplayList, build_display_list, rasterize};
use crate::style::stylesheet::CssRule;
use crate::style::{Origin, StyledDocument, Stylesheet, Viewport, compute_styles, user_agent_stylesheet};
use crate::text::FontDb;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::time::{Duration, Instant};
use url::Url;

/// A stylesheet's source text.
#[derive(Clone, Debug)]
pub struct SheetSource {
    /// CSS text.
    pub css: String,
    /// URL for resolving relative URLs inside the sheet.
    pub base: Url,
    /// `media` attribute of the `<link>`/`<style>`, if any.
    pub media: Option<String>,
}

/// A web font fetched for an `@font-face` rule.
#[derive(Clone, Debug)]
pub struct FontSource {
    /// Family name.
    pub family: String,
    /// Weight.
    pub weight: u16,
    /// Italic.
    pub italic: bool,
    /// TTF/OTF bytes.
    pub bytes: Vec<u8>,
}

/// Time spent in each loading stage.
#[derive(Clone, Copy, Debug, Default)]
pub struct LoadTimings {
    /// Fetching the document.
    pub fetch_document: Duration,
    /// HTML tokenizing and tree building.
    pub parse_html: Duration,
    /// Fetching stylesheets, images and fonts.
    pub fetch_subresources: Duration,
}

/// A loaded page before CSS parsing and image decoding; safe to send across threads.
pub struct RawPage {
    /// Final document URL.
    pub url: Url,
    /// The parsed document.
    pub doc: Document,
    /// Author stylesheets in cascade order.
    pub sheets: Vec<SheetSource>,
    /// Image bytes by absolute URL.
    pub images: Vec<(String, Vec<u8>)>,
    /// Web fonts.
    pub fonts: Vec<FontSource>,
    /// Timings.
    pub timings: LoadTimings,
    /// Non-fatal errors (failed subresources).
    pub errors: Vec<String>,
}

/// Options for [`load`].
#[derive(Clone, Copy, Debug)]
pub struct LoadOptions {
    /// Viewport used to discover which background images are needed.
    pub viewport: (f32, f32),
    /// Whether to fetch subresources at all.
    pub subresources: bool,
}

impl Default for LoadOptions {
    fn default() -> Self {
        LoadOptions {
            viewport: (1024.0, 768.0),
            subresources: true,
        }
    }
}

/// Fetches and parses a document and its subresources.
pub fn load(url: &Url, opts: &LoadOptions) -> Result<RawPage, String> {
    let t0 = Instant::now();
    let resp = net::fetch(url)?;
    let fetch_document = t0.elapsed();
    let html = decode_text(&resp.bytes, resp.content_type.as_deref(), true);
    let html = if resp.content_type.as_deref().is_some_and(|c| c.starts_with("image/")) {
        format!("<!doctype html><body style='margin:0'><img src=\"{}\">", resp.url)
    } else if resp
        .content_type
        .as_deref()
        .is_some_and(|c| c.starts_with("text/plain"))
    {
        format!(
            "<!doctype html><pre style='white-space:pre-wrap'>{}</pre>",
            escape_html(&html)
        )
    } else {
        html
    };
    load_html(&html, resp.url, opts, fetch_document)
}

fn escape_html(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;")
}

/// Parses `html` (with base URL `url`) and fetches its subresources.
pub fn load_html(html: &str, url: Url, opts: &LoadOptions, fetch_document: Duration) -> Result<RawPage, String> {
    let t1 = Instant::now();
    let doc = parse_document(html);
    let parse_html = t1.elapsed();
    let t2 = Instant::now();
    let base = document_base(&doc, &url);
    let mut errors = Vec::new();
    let mut sheets = Vec::new();
    if opts.subresources {
        collect_stylesheets(&doc, &base, &mut sheets, &mut errors);
    } else {
        for n in doc.elements_by_tag("style") {
            sheets.push(SheetSource {
                css: doc.text_content(n),
                base: base.clone(),
                media: doc.attr(n, "media").map(str::to_string),
            });
        }
    }
    let mut images = Vec::new();
    let mut fonts = Vec::new();
    if opts.subresources {
        // Build stylesheets and styles here to discover images and fonts actually used.
        let parsed = build_sheets_for(&sheets, doc.quirks_mode == crate::dom::QuirksMode::Quirks);
        let styles = compute_styles(
            &doc,
            &parsed,
            Viewport {
                width: opts.viewport.0,
                height: opts.viewport.1,
            },
        );
        let image_urls = discover_images(&doc, &styles, &base, &parsed);
        let font_jobs = discover_fonts(&parsed);
        let (imgs, fnts, errs) = fetch_parallel(image_urls, font_jobs);
        images = imgs;
        fonts = fnts;
        errors.extend(errs);
    }
    Ok(RawPage {
        url,
        doc,
        sheets,
        images,
        fonts,
        timings: LoadTimings {
            fetch_document,
            parse_html,
            fetch_subresources: t2.elapsed(),
        },
        errors,
    })
}

fn document_base(doc: &Document, url: &Url) -> Url {
    doc.elements_by_tag("base")
        .into_iter()
        .find_map(|b| doc.attr(b, "href"))
        .and_then(|h| url.join(h).ok())
        .unwrap_or_else(|| url.clone())
}

fn collect_stylesheets(doc: &Document, base: &Url, out: &mut Vec<SheetSource>, errors: &mut Vec<String>) {
    let mut seen = HashSet::new();
    for n in doc.descendants(doc.root()) {
        let Some(e) = doc.element(n) else { continue };
        if e.ns != crate::dom::Namespace::Html {
            continue;
        }
        if e.name == "style" {
            let css = doc.text_content(n);
            push_with_imports(
                css,
                base.clone(),
                e.attr("media").map(str::to_string),
                out,
                errors,
                &mut seen,
                0,
            );
        } else if e.name == "link"
            && e.attr("rel")
                .is_some_and(|r| r.split_ascii_whitespace().any(|t| t.eq_ignore_ascii_case("stylesheet")))
            && !e
                .attr("rel")
                .is_some_and(|r| r.to_ascii_lowercase().contains("alternate"))
            && e.attr("disabled").is_none()
            && let Some(href) = e.attr("href")
            && let Ok(u) = base.join(href)
        {
            match net::fetch(&u) {
                Ok(r) => {
                    let css = decode_text(&r.bytes, r.content_type.as_deref(), false);
                    push_with_imports(
                        css,
                        r.url,
                        e.attr("media").map(str::to_string),
                        out,
                        errors,
                        &mut seen,
                        0,
                    );
                }
                Err(err) => errors.push(err),
            }
        }
    }
}

fn push_with_imports(
    css: String,
    base: Url,
    media: Option<String>,
    out: &mut Vec<SheetSource>,
    errors: &mut Vec<String>,
    seen: &mut HashSet<String>,
    depth: usize,
) {
    if depth < 4 {
        let sheet = Stylesheet::parse(&css, Origin::Author, None);
        for r in &sheet.rules {
            if let CssRule::Import { url, media: m } = r
                && let Ok(u) = base.join(url)
                && seen.insert(u.to_string())
            {
                match net::fetch(&u) {
                    Ok(resp) => {
                        let text = decode_text(&resp.bytes, resp.content_type.as_deref(), false);
                        let mtext = crate::css::parser::to_css(m);
                        let mq = if mtext.trim().is_empty() {
                            media.clone()
                        } else {
                            Some(mtext)
                        };
                        push_with_imports(text, resp.url, mq, out, errors, seen, depth + 1);
                    }
                    Err(e) => errors.push(e),
                }
            }
        }
    }
    out.push(SheetSource { css, base, media });
}

/// Parses sheet sources into stylesheets (UA sheet first).
pub fn build_sheets(sources: &[SheetSource]) -> Vec<Rc<Stylesheet>> {
    build_sheets_for(sources, false)
}

/// Like [`build_sheets`], adding the quirks-mode UA rules when `quirks` is set.
pub fn build_sheets_for(sources: &[SheetSource], quirks: bool) -> Vec<Rc<Stylesheet>> {
    let mut v = vec![user_agent_stylesheet()];
    if quirks {
        v.push(crate::style::quirks_stylesheet());
    }
    for s in sources {
        let mut sheet = Stylesheet::parse(&s.css, Origin::Author, Some(s.base.to_string()));
        resolve_sheet_urls(&mut sheet.rules, &s.base);
        if let Some(m) = &s.media
            && !m.trim().is_empty()
            && !m.trim().eq_ignore_ascii_case("all")
        {
            sheet.rules = vec![CssRule::Media {
                query: Rc::new(parse_component_values(m)),
                rules: std::mem::take(&mut sheet.rules),
            }];
        }
        v.push(Rc::new(sheet));
    }
    v
}

/// Makes `url()` values in declarations absolute so computed styles carry absolute URLs.
fn resolve_sheet_urls(rules: &mut [CssRule], base: &Url) {
    use crate::style::properties::{PropertyDeclaration, SpecifiedValue};
    for r in rules.iter_mut() {
        match r {
            CssRule::Style(sr) => {
                let needs = sr
                    .block
                    .declarations
                    .iter()
                    .any(|(d, _)| matches!(d, PropertyDeclaration::Longhand(_, SpecifiedValue::Url(_))));
                if needs {
                    let mut block = (*sr.block).clone();
                    for (d, _) in block.declarations.iter_mut() {
                        if let PropertyDeclaration::Longhand(_, SpecifiedValue::Url(u)) = d
                            && let Ok(abs) = base.join(u)
                        {
                            *u = Rc::from(abs.as_str());
                        }
                    }
                    sr.block = Rc::new(block);
                }
            }
            CssRule::Media { rules, .. } => resolve_sheet_urls(rules, base),
            CssRule::FontFace(ff) => {
                for s in ff.sources.iter_mut() {
                    if let Ok(abs) = base.join(s) {
                        *s = abs.to_string();
                    }
                }
            }
            CssRule::Import { .. } => {}
        }
    }
}

fn discover_images(doc: &Document, styles: &StyledDocument, base: &Url, _sheets: &[Rc<Stylesheet>]) -> Vec<String> {
    let mut urls: Vec<String> = Vec::new();
    let mut seen = HashSet::new();
    let mut add = |u: Option<Url>| {
        if let Some(u) = u
            && matches!(u.scheme(), "http" | "https" | "file" | "data")
            && seen.insert(u.to_string())
        {
            urls.push(u.to_string());
        }
    };
    for n in doc.descendants(doc.root()) {
        let Some(e) = doc.element(n) else { continue };
        if styles.style(n).is_none() {
            continue; // display: none subtree
        }
        if e.is_html("img") || (e.is_html("input") && e.attr("type").is_some_and(|t| t.eq_ignore_ascii_case("image"))) {
            let src = e
                .attr("src")
                .filter(|s| !s.trim().is_empty())
                .map(str::to_string)
                .or_else(|| {
                    e.attr("srcset")
                        .and_then(|ss| ss.split(',').next())
                        .and_then(|c| c.split_whitespace().next())
                        .map(str::to_string)
                });
            add(src.and_then(|s| base.join(s.trim()).ok()));
        }
        if e.is_html("video")
            && let Some(p) = e.attr("poster")
        {
            add(base.join(p.trim()).ok());
        }
        if let Some(s) = styles.style(n) {
            if let Some(bg) = &s.background_image {
                add(Url::parse(bg).ok().or_else(|| base.join(bg).ok()));
            }
            if let Some(li) = &s.list_style_image {
                add(Url::parse(li).ok().or_else(|| base.join(li).ok()));
            }
        }
        for ps in [styles.before(n), styles.after(n)].into_iter().flatten() {
            if let Some(items) = &ps.content {
                for it in items.iter() {
                    if let crate::style::computed::ContentItem::Url(u) = it {
                        add(Url::parse(u).ok().or_else(|| base.join(u).ok()));
                    }
                }
            }
        }
    }
    urls
}

fn discover_fonts(sheets: &[Rc<Stylesheet>]) -> Vec<(String, u16, bool, String)> {
    let mut jobs = Vec::new();
    for s in sheets.iter().filter(|s| s.origin == Origin::Author) {
        for ff in s.font_faces() {
            if let Some(src) = ff.sources.iter().find(|u| {
                let l = u.to_ascii_lowercase();
                let path = l.split(['?', '#']).next().unwrap_or("");
                path.ends_with(".ttf") || path.ends_with(".otf")
            }) {
                jobs.push((ff.family.clone(), ff.weight, ff.italic, src.clone()));
            }
        }
    }
    jobs
}

type FetchResults = (Vec<(String, Vec<u8>)>, Vec<FontSource>, Vec<String>);

fn fetch_parallel(images: Vec<String>, fonts: Vec<(String, u16, bool, String)>) -> FetchResults {
    let jobs: Vec<(usize, String)> = images
        .iter()
        .cloned()
        .enumerate()
        .chain(fonts.iter().enumerate().map(|(i, f)| (images.len() + i, f.3.clone())))
        .collect();
    let threads = 8.min(jobs.len().max(1));
    let results: Vec<(usize, Result<Vec<u8>, String>)> = std::thread::scope(|scope| {
        let chunks: Vec<Vec<(usize, String)>> = (0..threads)
            .map(|t| jobs.iter().skip(t).step_by(threads).cloned().collect())
            .collect();
        let handles: Vec<_> = chunks
            .into_iter()
            .map(|chunk| {
                scope.spawn(move || {
                    chunk
                        .into_iter()
                        .map(|(i, u)| {
                            let r = Url::parse(&u)
                                .map_err(|e| e.to_string())
                                .and_then(|url| net::fetch(&url))
                                .map(|r| r.bytes);
                            (i, r)
                        })
                        .collect::<Vec<_>>()
                })
            })
            .collect();
        handles.into_iter().flat_map(|h| h.join().unwrap_or_default()).collect()
    });
    let mut imgs = Vec::new();
    let mut fnts = Vec::new();
    let mut errs = Vec::new();
    for (i, r) in results {
        match r {
            Ok(bytes) if i < images.len() => imgs.push((images[i].clone(), bytes)),
            Ok(bytes) => {
                let f = &fonts[i - images.len()];
                fnts.push(FontSource {
                    family: f.0.clone(),
                    weight: f.1,
                    italic: f.2,
                    bytes,
                });
            }
            Err(e) => errs.push(e),
        }
    }
    (imgs, fnts, errs)
}

/// Time spent rendering.
#[derive(Clone, Copy, Debug, Default)]
pub struct RenderTimings {
    /// CSS parsing (done once per page).
    pub parse_css: Duration,
    /// Selector matching and the cascade.
    pub style: Duration,
    /// Box tree construction and layout.
    pub layout: Duration,
    /// Display list construction.
    pub display_list: Duration,
    /// Rasterization.
    pub raster: Duration,
}

/// A page ready to render.
pub struct Page {
    /// Document URL.
    pub url: Url,
    /// The DOM.
    pub doc: Document,
    /// Stylesheets (UA first).
    pub sheets: Vec<Rc<Stylesheet>>,
    /// Images and base URL.
    pub resources: Resources,
    /// Fonts (bundled plus web fonts).
    pub fonts: FontDb,
    /// Load timings.
    pub load_timings: LoadTimings,
    /// Time to parse the stylesheets.
    pub parse_css_time: Duration,
    /// Non-fatal load errors.
    pub errors: Vec<String>,
}

/// The output of rendering a page at one viewport size.
pub struct Rendered {
    /// Computed styles.
    pub styles: StyledDocument,
    /// Layout.
    pub layout: LayoutTree,
    /// Display list.
    pub display_list: DisplayList,
    /// Timings (raster is filled in by [`Page::paint`]).
    pub timings: RenderTimings,
}

impl Page {
    /// Builds a page from loaded data (parses CSS, decodes images, registers fonts).
    pub fn from_raw(raw: RawPage) -> Page {
        let t = Instant::now();
        let sheets = build_sheets_for(&raw.sheets, raw.doc.quirks_mode == crate::dom::QuirksMode::Quirks);
        let parse_css_time = t.elapsed();
        let mut images = HashMap::new();
        let mut svgs = HashMap::new();
        for (u, bytes) in &raw.images {
            if let Some(img) = Image::decode(bytes) {
                images.insert(u.clone(), img);
            } else if let Ok(text) = std::str::from_utf8(bytes)
                && text[..text.len().min(4096)].contains("<svg")
                && let Some(svg) = crate::svg::parse_svg(text)
            {
                svgs.insert(u.clone(), Rc::new(svg));
            }
        }
        let mut fonts = FontDb::new();
        for f in raw.fonts {
            fonts.add_web_font(f.bytes, &f.family, f.weight, f.italic);
        }
        let base = document_base(&raw.doc, &raw.url);
        Page {
            url: raw.url,
            doc: raw.doc,
            sheets,
            resources: Resources {
                images,
                base_url: Some(base),
                svgs,
            },
            fonts,
            load_timings: raw.timings,
            parse_css_time,
            errors: raw.errors,
        }
    }

    /// Loads a page from a URL (blocking).
    pub fn load(url: &Url, opts: &LoadOptions) -> Result<Page, String> {
        Ok(Page::from_raw(load(url, opts)?))
    }

    /// Builds a page from an HTML string without fetching anything.
    pub fn from_html(html: &str, base: Url) -> Page {
        let opts = LoadOptions {
            subresources: false,
            ..Default::default()
        };
        Page::from_raw(load_html(html, base, &opts, Duration::ZERO).expect("parsing never fails"))
    }

    /// The document title.
    pub fn title(&self) -> Option<String> {
        self.doc.title()
    }

    /// Styles, lays out and builds the display list for a viewport.
    pub fn render(&self, width: f32, height: f32) -> Rendered {
        let t0 = Instant::now();
        let styles = compute_styles(&self.doc, &self.sheets, Viewport { width, height });
        let t1 = Instant::now();
        let layout = layout_document(&self.doc, &styles, &self.fonts, &self.resources, (width, height));
        let t2 = Instant::now();
        let display_list = build_display_list(&layout, &self.doc, &self.resources);
        let t3 = Instant::now();
        Rendered {
            styles,
            layout,
            display_list,
            timings: RenderTimings {
                parse_css: self.parse_css_time,
                style: t1 - t0,
                layout: t2 - t1,
                display_list: t3 - t2,
                raster: Duration::ZERO,
            },
        }
    }

    /// Rasterizes a rendered page into a `width` x `height` canvas scrolled to `scroll_y`.
    pub fn paint(&self, r: &mut Rendered, width: u32, height: u32, scroll_y: f32) -> Canvas {
        let t = Instant::now();
        let c = rasterize(
            &r.display_list,
            &self.fonts,
            width,
            height,
            crate::layout::Point::new(0.0, scroll_y),
        );
        r.timings.raster = t.elapsed();
        c
    }

    /// Finds the `href` of the link at document point (x, y), resolved to an absolute URL.
    pub fn link_at(&self, r: &Rendered, x: f32, y: f32, scroll_y: f32) -> Option<(NodeId, Url)> {
        for h in r.display_list.links.iter().rev() {
            let (px, py) = if h.fixed { (x, y - scroll_y) } else { (x, y) };
            if h.rect.contains(px, py) {
                let href = self.doc.attr(h.link, "href")?;
                let base = self.resources.base_url.as_ref()?;
                return base.join(href.trim()).ok().map(|u| (h.link, u));
            }
        }
        None
    }
}

/// Renders a URL to a canvas in one call (used by the CLI and tests).
pub fn render_url(url: &Url, width: u32, height: Option<u32>) -> Result<(Page, Rendered, Canvas), String> {
    let vh = height.unwrap_or(768) as f32;
    let opts = LoadOptions {
        viewport: (width as f32, vh),
        subresources: true,
    };
    let page = Page::load(url, &opts)?;
    let mut rendered = page.render(width as f32, vh);
    let h = height.unwrap_or_else(|| (rendered.layout.document_height.ceil() as u32).clamp(1, 32_000));
    let canvas = page.paint(&mut rendered, width, h, 0.0);
    Ok((page, rendered, canvas))
}
