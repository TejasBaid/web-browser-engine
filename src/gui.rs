//! The windowed browser (`browser open <url>`), built on `winit` + `softbuffer`.
//!
//! Pages load on a background thread ([`page::load`] produces a thread-safe
//! [`RawPage`]) and are styled, laid out and painted on the UI thread. The toolbar and
//! the link status bubble are drawn by the engine's own painter: they are small
//! display lists of [`DisplayItem`]s rasterized like any page, so the browser chrome
//! dogfoods the same text shaping and rasterization code.
//!
//! Only the visible viewport is rasterized; the page is re-rasterized when it scrolls,
//! re-laid out when the window is resized, and the cached pixels are reused for
//! everything else (hover feedback, toolbar edits).

use crate::css::color::Color;
use crate::layout::fragment::{PositionedGlyph, TextRun};
use crate::layout::{Edges, Point, Rect};
use crate::net::url_from_arg;
use crate::page::{self, LoadOptions, Page, RawPage, Rendered};
use crate::paint::{Canvas, DisplayItem, DisplayList, rasterize};
use crate::style::computed::BorderStyle;
use crate::text::FontDb;
use std::num::NonZeroU32;
use std::rc::Rc;
use std::time::{Duration, Instant};
use url::Url;
use winit::application::ApplicationHandler;
use winit::dpi::{LogicalSize, PhysicalPosition, PhysicalSize};
use winit::event::{ElementState, KeyEvent, MouseButton, MouseScrollDelta, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop, EventLoopProxy};
use winit::keyboard::{Key, ModifiersState, NamedKey};
use winit::window::{CursorIcon, Window, WindowId};

/// Height of the toolbar in CSS px.
const TOOLBAR_H: f32 = 40.0;
/// Pixels scrolled per wheel "line" and per arrow key press.
const LINE_SCROLL: f32 = 40.0;
/// The built-in start page's address.
const START_URL: &str = "about:start";

const TOOLBAR_BG: Color = Color::rgb(0xf2, 0xf2, 0xf5);
const TOOLBAR_LINE: Color = Color::rgb(0xd2, 0xd2, 0xd8);
const BUTTON_HOVER: Color = Color::rgb(0xe2, 0xe2, 0xe8);
const ICON: Color = Color::rgb(0x3a, 0x3a, 0x42);
const ICON_DISABLED: Color = Color::rgb(0xb4, 0xb4, 0xbc);
const FIELD_BORDER: Color = Color::rgb(0xc8, 0xc8, 0xd0);
const FOCUS: Color = Color::rgb(0x3b, 0x82, 0xf6);
const SELECTION: Color = Color::rgb(0xb5, 0xd5, 0xff);
const MUTED: Color = Color::rgb(0x80, 0x80, 0x8a);

/// Events sent from loader threads to the UI thread.
enum UserEvent {
    /// A load finished (`id` identifies the request so stale results are ignored).
    Loaded {
        id: u64,
        url: Url,
        result: Result<RawPage, String>,
    },
}

/// A history entry.
struct HistoryEntry {
    url: Url,
    scroll: f32,
}

/// A load in flight.
struct Loading {
    id: u64,
    url: Url,
    /// Whether the finished load creates a new history entry.
    push: bool,
    /// Scroll position to restore (history navigation and reload).
    restore_scroll: Option<f32>,
    started: Instant,
}

/// Toolbar hit targets.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Target {
    Back,
    Forward,
    Reload,
    Address,
}

/// The address field's editing state.
#[derive(Default)]
struct AddressField {
    text: String,
    /// Caret position in chars.
    caret: usize,
    editing: bool,
    /// Whole text selected (replaced by the next keystroke).
    all_selected: bool,
}

impl AddressField {
    fn byte_index(&self, chars: usize) -> usize {
        self.text
            .char_indices()
            .nth(chars)
            .map(|(i, _)| i)
            .unwrap_or(self.text.len())
    }

    fn char_len(&self) -> usize {
        self.text.chars().count()
    }

    fn insert(&mut self, s: &str) {
        if self.all_selected {
            self.text.clear();
            self.caret = 0;
            self.all_selected = false;
        }
        let at = self.byte_index(self.caret);
        self.text.insert_str(at, s);
        self.caret += s.chars().count();
    }

    fn backspace(&mut self) {
        if self.all_selected {
            self.text.clear();
            self.caret = 0;
            self.all_selected = false;
        } else if self.caret > 0 {
            let at = self.byte_index(self.caret - 1);
            self.text.remove(at);
            self.caret -= 1;
        }
    }

    fn delete(&mut self) {
        if self.all_selected {
            self.backspace();
        } else if self.caret < self.char_len() {
            let at = self.byte_index(self.caret);
            self.text.remove(at);
        }
    }
}

/// The browser application state.
struct App {
    proxy: EventLoopProxy<UserEvent>,
    initial_size: (u32, u32),
    start: Option<Url>,
    window: Option<Rc<Window>>,
    surface: Option<softbuffer::Surface<Rc<Window>, Rc<Window>>>,
    ui_fonts: FontDb,
    page: Option<Page>,
    rendered: Option<Rendered>,
    /// The page display list scaled to device pixels (cached per render).
    scaled: Option<DisplayList>,
    /// Rasterized viewport (device pixels), valid while `page_dirty` is false.
    page_canvas: Option<Canvas>,
    page_dirty: bool,
    /// Vertical scroll offset in CSS px.
    scroll: f32,
    history: Vec<HistoryEntry>,
    index: usize,
    loading: Option<Loading>,
    next_load_id: u64,
    address: AddressField,
    hover_link: Option<Url>,
    hover_target: Option<Target>,
    cursor: PhysicalPosition<f64>,
    mods: ModifiersState,
    title: String,
}

/// Opens a browser window showing `start` (or the built-in start page) and runs the
/// event loop until the window is closed.
pub fn run(start: Option<Url>, width: u32, height: u32) -> Result<(), String> {
    let event_loop = EventLoop::<UserEvent>::with_user_event()
        .build()
        .map_err(|e| format!("cannot create event loop: {e}"))?;
    event_loop.set_control_flow(ControlFlow::Wait);
    let mut app = App {
        proxy: event_loop.create_proxy(),
        initial_size: (width, height),
        start,
        window: None,
        surface: None,
        ui_fonts: FontDb::new(),
        page: None,
        rendered: None,
        scaled: None,
        page_canvas: None,
        page_dirty: true,
        scroll: 0.0,
        history: Vec::new(),
        index: 0,
        loading: None,
        next_load_id: 1,
        address: AddressField::default(),
        hover_link: None,
        hover_target: None,
        cursor: PhysicalPosition::new(0.0, 0.0),
        mods: ModifiersState::empty(),
        title: "browser-engine".into(),
    };
    event_loop
        .run_app(&mut app)
        .map_err(|e| format!("event loop error: {e}"))
}

/// The HTML of the built-in start page.
fn start_page_html() -> String {
    let root = env!("CARGO_MANIFEST_DIR");
    let file = |name: &str| {
        Url::from_file_path(format!("{root}/tests/reftests/{name}.html"))
            .map(|u| u.to_string())
            .unwrap_or_default()
    };
    format!(
        r#"<!doctype html><html><head><title>browser-engine</title><style>
        body {{ font-family: "Liberation Sans", sans-serif; margin: 0; background: #fafafb; color: #1f2430; }}
        .wrap {{ max-width: 640px; margin: 56px auto; padding: 0 24px; }}
        h1 {{ font-size: 34px; margin: 0 0 6px; letter-spacing: -0.5px; }}
        p.lead {{ color: #5b6170; margin: 0 0 28px; font-size: 16px; line-height: 1.5; }}
        h2 {{ font-size: 13px; text-transform: uppercase; letter-spacing: 1px; color: #8a8f9c; margin: 28px 0 10px; }}
        ul {{ list-style: none; padding: 0; margin: 0; }}
        li {{ margin: 0 0 8px; }}
        a {{ display: block; padding: 12px 16px; background: #fff; border: 1px solid #e2e4ea; border-radius: 8px;
             color: #1f2430; text-decoration: none; }}
        a span {{ color: #8a8f9c; font-size: 13px; margin-left: 8px; }}
        kbd {{ font-family: "Liberation Mono", monospace; font-size: 12px; background: #eceef2; border-radius: 4px; padding: 1px 5px; }}
        .keys {{ color: #5b6170; font-size: 14px; line-height: 1.9; }}
        </style></head><body><div class="wrap">
        <h1>browser-engine</h1>
        <p class="lead">A web browser engine written from scratch in Rust: HTML parser, CSS cascade,
        block/inline/flex/table layout and a software rasterizer. Everything below, including the toolbar, is
        drawn by the engine itself.</p>
        <h2>On the web</h2>
        <ul>
        <li><a href="https://example.com/">example.com <span>the simplest page there is</span></a></li>
        <li><a href="https://news.ycombinator.com/">Hacker News <span>table layout</span></a></li>
        <li><a href="https://en.wikipedia.org/wiki/Web_browser_engine">Wikipedia: Browser engine <span>a long article</span></a></li>
        </ul>
        <h2>Local test pages</h2>
        <ul>
        <li><a href="{cards}">Card layout <span>flexbox, border-radius, shadows</span></a></li>
        <li><a href="{article}">Article <span>floats, typography</span></a></li>
        <li><a href="{navbar}">Navigation bar <span>flex alignment</span></a></li>
        </ul>
        <h2>Keyboard</h2>
        <div class="keys"><kbd>Cmd+L</kbd> address bar &nbsp; <kbd>Cmd+[</kbd> <kbd>Cmd+]</kbd> back / forward &nbsp;
        <kbd>Cmd+R</kbd> reload &nbsp; <kbd>Space</kbd> page down &nbsp; <kbd>Cmd+W</kbd> close</div>
        </div></body></html>"#,
        cards = file("mixed-cards"),
        article = file("mixed-article"),
        navbar = file("mixed-navbar"),
    )
}

/// The HTML of the error page shown when a load fails.
fn error_page_html(url: &Url, error: &str) -> String {
    let esc = |s: &str| s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;");
    format!(
        r#"<!doctype html><html><head><title>Can't open page</title><style>
        body {{ font-family: "Liberation Sans", sans-serif; margin: 0; background: #fafafb; color: #1f2430; }}
        .box {{ max-width: 560px; margin: 80px auto; padding: 0 24px; }}
        h1 {{ font-size: 24px; margin: 0 0 12px; }}
        p {{ color: #5b6170; line-height: 1.5; }}
        code {{ display: block; font-family: "Liberation Mono", monospace; font-size: 13px; background: #f0f1f4;
                border: 1px solid #e2e4ea; border-radius: 6px; padding: 12px; white-space: pre-wrap; color: #9b2c2c; }}
        </style></head><body><div class="box"><h1>This page couldn't be loaded</h1>
        <p>{}</p><code>{}</code></div></body></html>"#,
        esc(url.as_str()),
        esc(error)
    )
}

/// Builds a text run with the UI fonts. Returns the run and its advance width.
fn text_run(fonts: &FontDb, text: &str, size: f32, bold: bool, mono: bool) -> (Rc<TextRun>, f32) {
    let family = if mono { "Liberation Mono" } else { "Liberation Sans" };
    let families: Rc<[String]> = Rc::from(vec![family.to_string()]);
    let face = fonts.select(&families, if bold { 700 } else { 400 }, false);
    let metrics = fonts.metrics(face, size);
    let mut pen = 0.0;
    let glyphs = fonts
        .shape(face, size, text, 0.0, 0.0)
        .iter()
        .map(|g| {
            let p = PositionedGlyph {
                face: g.face,
                glyph: g.glyph,
                x: pen + g.x_offset,
                y: -g.y_offset,
            };
            pen += g.advance;
            p
        })
        .collect();
    let run = TextRun {
        glyphs,
        font_size: size,
        baseline: metrics.ascent,
        metrics,
        text: text.to_string(),
    };
    (Rc::new(run), pen)
}

/// Small helpers for building chrome display lists.
struct Ui<'a> {
    fonts: &'a FontDb,
    items: Vec<DisplayItem>,
}

impl Ui<'_> {
    fn rect(&mut self, r: Rect, color: Color, radius: f32) {
        self.items.push(DisplayItem::Rect {
            rect: r,
            color,
            radii: [(radius, radius); 4],
        });
    }

    fn border(&mut self, r: Rect, width: f32, color: Color, radius: f32) {
        self.items.push(DisplayItem::Border {
            rect: r,
            widths: Edges::all(width),
            colors: [color; 4],
            styles: [BorderStyle::Solid; 4],
            radii: [(radius, radius); 4],
        });
    }

    /// Draws text with its baseline vertically centred in `[y, y + h)`; returns the width.
    fn text(&mut self, x: f32, y: f32, h: f32, text: &str, size: f32, color: Color) -> f32 {
        let (run, w) = text_run(self.fonts, text, size, false, false);
        let m = run.metrics;
        let baseline = (y + (h - (m.ascent + m.descent)) / 2.0 + m.ascent).round();
        self.items.push(DisplayItem::Text {
            origin: Point::new(x, baseline),
            run,
            color,
        });
        w
    }

    fn measure(&self, text: &str, size: f32) -> f32 {
        text_run(self.fonts, text, size, false, false).1
    }
}

impl App {
    fn dpr(&self) -> f32 {
        self.window.as_ref().map(|w| w.scale_factor() as f32).unwrap_or(1.0)
    }

    /// Window size in CSS px.
    fn css_size(&self) -> (f32, f32) {
        let Some(w) = &self.window else { return (800.0, 600.0) };
        let s = w.inner_size();
        let d = self.dpr();
        (s.width as f32 / d, s.height as f32 / d)
    }

    /// The page viewport size in CSS px.
    fn viewport(&self) -> (f32, f32) {
        let (w, h) = self.css_size();
        (w.max(1.0), (h - TOOLBAR_H).max(1.0))
    }

    fn max_scroll(&self) -> f32 {
        let doc_h = self.rendered.as_ref().map(|r| r.layout.document_height).unwrap_or(0.0);
        (doc_h - self.viewport().1).max(0.0)
    }

    fn set_scroll(&mut self, y: f32) {
        let y = y.clamp(0.0, self.max_scroll()).round();
        if y != self.scroll {
            self.scroll = y;
            self.page_dirty = true;
            self.refresh_hover();
            self.redraw();
        }
    }

    fn redraw(&self) {
        if let Some(w) = &self.window {
            w.request_redraw();
        }
    }

    fn current_url(&self) -> Option<&Url> {
        self.history.get(self.index).map(|e| &e.url)
    }

    // ------------------------------------------------------------- navigation

    /// Navigates to `url`, creating a history entry.
    fn navigate(&mut self, url: Url) {
        // Same-document fragment navigation just scrolls.
        if let (Some(cur), Some(frag)) = (self.current_url(), url.fragment())
            && strip_fragment(cur) == strip_fragment(&url)
            && self.page.is_some()
        {
            let frag = frag.to_string();
            if let Some(e) = self.history.get_mut(self.index) {
                e.scroll = self.scroll;
            }
            self.history.truncate(self.index + 1);
            self.history.push(HistoryEntry {
                url: url.clone(),
                scroll: 0.0,
            });
            self.index = self.history.len() - 1;
            self.address.text = url.to_string();
            self.scroll_to_fragment(&frag);
            self.redraw();
            return;
        }
        if let Some(e) = self.history.get_mut(self.index) {
            e.scroll = self.scroll;
        }
        self.start_load(url, true, None);
    }

    fn go(&mut self, delta: isize) {
        let target = self.index as isize + delta;
        if target < 0 || target as usize >= self.history.len() {
            return;
        }
        if let Some(e) = self.history.get_mut(self.index) {
            e.scroll = self.scroll;
        }
        self.index = target as usize;
        let entry = &self.history[self.index];
        let (url, scroll) = (entry.url.clone(), entry.scroll);
        // Entries that differ only by fragment share the loaded document.
        let same_doc = self.page.as_ref().is_some_and(|p| {
            strip_fragment(&p.url) == strip_fragment(&url) || url.as_str() == START_URL && p.url.as_str() == START_URL
        });
        if same_doc && self.loading.is_none() {
            self.address.text = url.to_string();
            self.set_scroll(scroll);
            self.redraw();
        } else {
            self.start_load(url, false, Some(scroll));
        }
    }

    fn reload(&mut self) {
        if let Some(url) = self.current_url().cloned() {
            let scroll = self.scroll;
            self.start_load(url, false, Some(scroll));
        }
    }

    /// Starts loading `url` on a background thread.
    fn start_load(&mut self, url: Url, push: bool, restore_scroll: Option<f32>) {
        let id = self.next_load_id;
        self.next_load_id += 1;
        self.address.text = url.to_string();
        self.address.editing = false;
        self.loading = Some(Loading {
            id,
            url: url.clone(),
            push,
            restore_scroll,
            started: Instant::now(),
        });
        if url.as_str() == START_URL {
            // Built in: no thread needed.
            let page = Page::from_html(&start_page_html(), url.clone());
            self.finish_load(id, url, Ok(page));
            return;
        }
        let proxy = self.proxy.clone();
        let opts = LoadOptions {
            viewport: self.viewport(),
            subresources: true,
        };
        std::thread::spawn(move || {
            let result = page::load(&url, &opts);
            let _ = proxy.send_event(UserEvent::Loaded { id, url, result });
        });
        self.redraw();
    }

    /// Installs a loaded (or failed) page.
    fn finish_load(&mut self, id: u64, url: Url, result: Result<Page, String>) {
        let Some(loading) = self.loading.take_if(|l| l.id == id) else {
            return; // superseded by a newer navigation
        };
        let page = match result {
            Ok(p) => p,
            Err(e) => Page::from_html(&error_page_html(&url, &e), url.clone()),
        };
        let final_url = if url.fragment().is_some() && page.url.fragment().is_none() {
            let mut u = page.url.clone();
            u.set_fragment(url.fragment());
            u
        } else {
            page.url.clone()
        };
        if loading.push || self.history.is_empty() {
            if !self.history.is_empty() {
                self.history.truncate(self.index + 1);
            }
            self.history.push(HistoryEntry {
                url: final_url.clone(),
                scroll: 0.0,
            });
            self.index = self.history.len() - 1;
        } else if let Some(e) = self.history.get_mut(self.index) {
            e.url = final_url.clone();
        }
        self.address.text = final_url.to_string();
        self.title = page
            .title()
            .filter(|t| !t.is_empty())
            .unwrap_or_else(|| final_url.to_string());
        if let Some(w) = &self.window {
            w.set_title(&self.title);
        }
        self.page = Some(page);
        self.relayout();
        self.scroll = 0.0;
        match (loading.restore_scroll, final_url.fragment()) {
            (Some(s), _) => self.set_scroll(s),
            (None, Some(frag)) => {
                let frag = frag.to_string();
                self.scroll_to_fragment(&frag);
            }
            _ => {}
        }
        self.page_dirty = true;
        self.refresh_hover();
        self.redraw();
    }

    fn scroll_to_fragment(&mut self, frag: &str) {
        let (Some(page), Some(r)) = (&self.page, &self.rendered) else {
            return;
        };
        let node = page.doc.element_by_id(frag).or_else(|| {
            page.doc
                .elements_by_tag("a")
                .into_iter()
                .find(|&a| page.doc.attr(a, "name") == Some(frag))
        });
        if let Some(rect) = node.and_then(|n| r.layout.root.find_rect(n)) {
            self.set_scroll(rect.y);
        } else if frag.is_empty() || frag == "top" {
            self.set_scroll(0.0);
        }
    }

    /// Re-runs style, layout and display list construction for the current viewport.
    fn relayout(&mut self) {
        let Some(page) = &self.page else { return };
        let (w, h) = self.viewport();
        let rendered = page.render(w, h);
        self.scaled = Some(rendered.display_list.scaled(self.dpr()));
        self.rendered = Some(rendered);
        self.page_dirty = true;
        let max = self.max_scroll();
        if self.scroll > max {
            self.scroll = max;
        }
    }

    // ------------------------------------------------------------- input

    /// Cursor position in CSS px.
    fn cursor_css(&self) -> (f32, f32) {
        let d = self.dpr();
        (self.cursor.x as f32 / d, self.cursor.y as f32 / d)
    }

    fn toolbar_target(&self, x: f32, y: f32) -> Option<Target> {
        if !(0.0..TOOLBAR_H).contains(&y) {
            return None;
        }
        let (w, _) = self.css_size();
        if (6.0..34.0).contains(&y) {
            if (8.0..36.0).contains(&x) {
                return Some(Target::Back);
            }
            if (40.0..68.0).contains(&x) {
                return Some(Target::Forward);
            }
            if (72.0..100.0).contains(&x) {
                return Some(Target::Reload);
            }
            if x >= 110.0 && x < w - 12.0 {
                return Some(Target::Address);
            }
        }
        None
    }

    /// Recomputes the hovered link / toolbar button and the cursor shape.
    fn refresh_hover(&mut self) {
        let (x, y) = self.cursor_css();
        let target = self.toolbar_target(x, y);
        let link = if y >= TOOLBAR_H {
            match (&self.page, &self.rendered) {
                (Some(p), Some(r)) => {
                    let doc_y = y - TOOLBAR_H + self.scroll;
                    p.link_at(r, x, doc_y, self.scroll).map(|(_, u)| u)
                }
                _ => None,
            }
        } else {
            None
        };
        if link != self.hover_link || target != self.hover_target {
            self.hover_link = link;
            self.hover_target = target;
            if let Some(w) = &self.window {
                let icon = if self.hover_link.is_some()
                    || matches!(target, Some(Target::Back | Target::Forward | Target::Reload))
                {
                    CursorIcon::Pointer
                } else if target == Some(Target::Address) {
                    CursorIcon::Text
                } else {
                    CursorIcon::Default
                };
                w.set_cursor(icon);
            }
            self.redraw();
        }
    }

    fn click(&mut self) {
        let (x, y) = self.cursor_css();
        match self.toolbar_target(x, y) {
            Some(Target::Back) => self.go(-1),
            Some(Target::Forward) => self.go(1),
            Some(Target::Reload) => self.reload(),
            Some(Target::Address) => self.focus_address(),
            None => {
                if self.address.editing {
                    self.address.editing = false;
                    self.restore_address();
                    self.redraw();
                }
                if y >= TOOLBAR_H
                    && let Some(url) = self.hover_link.clone()
                {
                    self.navigate(url);
                }
            }
        }
    }

    fn focus_address(&mut self) {
        self.address.editing = true;
        self.address.all_selected = true;
        self.address.caret = self.address.char_len();
        self.redraw();
    }

    fn restore_address(&mut self) {
        self.address.text = self
            .loading
            .as_ref()
            .map(|l| l.url.to_string())
            .or_else(|| self.current_url().map(|u| u.to_string()))
            .unwrap_or_default();
        self.address.all_selected = false;
    }

    fn handle_key(&mut self, event_loop: &ActiveEventLoop, ev: &KeyEvent) {
        if ev.state != ElementState::Pressed {
            return;
        }
        let cmd = self.mods.super_key() || self.mods.control_key();
        let alt = self.mods.alt_key();
        let shift = self.mods.shift_key();
        // Global shortcuts.
        if cmd {
            match &ev.logical_key {
                Key::Character(c) => match c.as_str() {
                    "l" | "L" => return self.focus_address(),
                    "r" | "R" => return self.reload(),
                    "[" => return self.go(-1),
                    "]" => return self.go(1),
                    "w" | "W" | "q" | "Q" => return event_loop.exit(),
                    "a" | "A" if self.address.editing => {
                        self.address.all_selected = true;
                        self.address.caret = self.address.char_len();
                        return self.redraw();
                    }
                    _ => return,
                },
                Key::Named(NamedKey::ArrowUp) if !self.address.editing => return self.set_scroll(0.0),
                Key::Named(NamedKey::ArrowDown) if !self.address.editing => return self.set_scroll(f32::MAX),
                Key::Named(NamedKey::ArrowLeft) if !self.address.editing => return self.go(-1),
                Key::Named(NamedKey::ArrowRight) if !self.address.editing => return self.go(1),
                _ => return,
            }
        }
        if alt && !self.address.editing {
            match &ev.logical_key {
                Key::Named(NamedKey::ArrowLeft) => return self.go(-1),
                Key::Named(NamedKey::ArrowRight) => return self.go(1),
                _ => {}
            }
        }
        if self.address.editing {
            self.edit_address(ev);
            return;
        }
        let page_step = (self.viewport().1 - LINE_SCROLL).max(LINE_SCROLL);
        match &ev.logical_key {
            Key::Named(NamedKey::ArrowDown) => self.set_scroll(self.scroll + LINE_SCROLL),
            Key::Named(NamedKey::ArrowUp) => self.set_scroll(self.scroll - LINE_SCROLL),
            Key::Named(NamedKey::PageDown) => self.set_scroll(self.scroll + page_step),
            Key::Named(NamedKey::PageUp) => self.set_scroll(self.scroll - page_step),
            Key::Named(NamedKey::Space) if shift => self.set_scroll(self.scroll - page_step),
            Key::Named(NamedKey::Space) => self.set_scroll(self.scroll + page_step),
            Key::Named(NamedKey::Home) => self.set_scroll(0.0),
            Key::Named(NamedKey::End) => self.set_scroll(f32::MAX),
            Key::Named(NamedKey::Backspace) => self.go(-1),
            _ => {}
        }
    }

    fn edit_address(&mut self, ev: &KeyEvent) {
        let a = &mut self.address;
        match &ev.logical_key {
            Key::Named(NamedKey::Enter) => {
                let text = a.text.trim().to_string();
                a.editing = false;
                a.all_selected = false;
                if text.is_empty() {
                    self.restore_address();
                } else if text == START_URL {
                    if let Ok(u) = Url::parse(START_URL) {
                        self.navigate(u);
                    }
                } else {
                    match url_from_arg(&text) {
                        Ok(u) => self.navigate(u),
                        Err(_) => self.restore_address(),
                    }
                }
            }
            Key::Named(NamedKey::Escape) => {
                a.editing = false;
                self.restore_address();
            }
            Key::Named(NamedKey::Backspace) => a.backspace(),
            Key::Named(NamedKey::Delete) => a.delete(),
            Key::Named(NamedKey::ArrowLeft) => {
                if a.all_selected {
                    a.all_selected = false;
                    a.caret = 0;
                } else {
                    a.caret = a.caret.saturating_sub(1);
                }
            }
            Key::Named(NamedKey::ArrowRight) => {
                a.all_selected = false;
                a.caret = (a.caret + 1).min(a.char_len());
            }
            Key::Named(NamedKey::Home) => {
                a.all_selected = false;
                a.caret = 0;
            }
            Key::Named(NamedKey::End) => {
                a.all_selected = false;
                a.caret = a.char_len();
            }
            _ => {
                if let Some(t) = &ev.text {
                    let clean: String = t.chars().filter(|c| !c.is_control()).collect();
                    if !clean.is_empty() {
                        a.insert(&clean);
                    }
                }
            }
        }
        self.redraw();
    }

    // ------------------------------------------------------------- drawing

    /// Builds the toolbar display list in CSS px.
    fn toolbar_list(&self, width: f32) -> DisplayList {
        let mut ui = Ui {
            fonts: &self.ui_fonts,
            items: Vec::new(),
        };
        ui.rect(Rect::new(0.0, 0.0, width, TOOLBAR_H), TOOLBAR_BG, 0.0);
        ui.rect(Rect::new(0.0, TOOLBAR_H - 1.0, width, 1.0), TOOLBAR_LINE, 0.0);
        let can_back = self.index > 0;
        let can_forward = self.index + 1 < self.history.len();
        let buttons = [
            (Target::Back, 8.0, "\u{2190}", can_back),
            (Target::Forward, 40.0, "\u{2192}", can_forward),
            (Target::Reload, 72.0, "", true),
        ];
        for (target, x, label, enabled) in buttons {
            let r = Rect::new(x, 6.0, 28.0, 28.0);
            if enabled && self.hover_target == Some(target) {
                ui.rect(r, BUTTON_HOVER, 6.0);
            }
            let color = if enabled { ICON } else { ICON_DISABLED };
            if target == Target::Reload {
                // A ring with a gap and an arrowhead, drawn with the engine's own primitives.
                let ring = Rect::new(x + 7.5, 13.5, 13.0, 13.0);
                ui.border(ring, 1.8, color, 6.5);
                let bg = if self.hover_target == Some(target) {
                    BUTTON_HOVER
                } else {
                    TOOLBAR_BG
                };
                ui.rect(Rect::new(x + 15.0, 12.0, 7.0, 6.0), bg, 0.0);
                ui.text(x + 15.2, 9.0, 12.0, "\u{25BA}", 8.0, color);
            } else {
                let w = ui.measure(label, 18.0);
                ui.text(x + (28.0 - w) / 2.0, 5.0, 27.0, label, 18.0, color);
            }
        }
        // Address field.
        let field = Rect::new(110.0, 6.0, (width - 122.0).max(40.0), 28.0);
        ui.rect(field, Color::WHITE, 7.0);
        if self.address.editing {
            ui.border(field, 2.0, FOCUS, 7.0);
        } else {
            ui.border(field, 1.0, FIELD_BORDER, 7.0);
        }
        let text_x = field.x + 12.0;
        ui.items.push(DisplayItem::PushClip {
            rect: Rect::new(field.x + 4.0, field.y, field.width - 8.0, field.height),
            radii: [(0.0, 0.0); 4],
        });
        let size = 14.0;
        if self.address.editing {
            let full = ui.measure(&self.address.text, size);
            if self.address.all_selected && !self.address.text.is_empty() {
                ui.rect(Rect::new(text_x - 1.0, field.y + 6.0, full + 2.0, 16.0), SELECTION, 2.0);
            }
            ui.text(text_x, field.y, field.height, &self.address.text, size, ICON);
            if !self.address.all_selected {
                let before: String = self.address.text.chars().take(self.address.caret).collect();
                let cx = text_x + ui.measure(&before, size);
                ui.rect(Rect::new(cx.round(), field.y + 6.0, 1.5, 16.0), FOCUS, 0.0);
            }
        } else if self.address.text.is_empty() {
            ui.text(text_x, field.y, field.height, "Search or enter address", size, MUTED);
        } else {
            // Show the scheme muted, like most browsers.
            let text = &self.address.text;
            let split = text.find("://").map(|i| i + 3).unwrap_or(0);
            let w = ui.text(text_x, field.y, field.height, &text[..split], size, MUTED);
            ui.text(text_x + w, field.y, field.height, &text[split..], size, ICON);
        }
        ui.items.push(DisplayItem::PopClip);
        if let Some(l) = &self.loading {
            // Indeterminate progress bar along the bottom edge of the toolbar.
            let t = l.started.elapsed().as_secs_f32();
            let span = width * 0.3;
            let x = ((t * 0.9).fract() * (width + span)) - span;
            ui.rect(Rect::new(x, TOOLBAR_H - 2.0, span, 2.0), FOCUS, 0.0);
            let lw = ui.measure("Loading\u{2026}", 12.0);
            let fx = field.x + field.width - lw - 12.0;
            ui.rect(
                Rect::new(fx - 8.0, field.y + 2.0, lw + 16.0, field.height - 4.0),
                Color::WHITE,
                0.0,
            );
            ui.text(fx, field.y, field.height, "Loading\u{2026}", 12.0, MUTED);
        }
        DisplayList {
            items: ui.items,
            background: TOOLBAR_BG,
            width,
            height: TOOLBAR_H,
            ..Default::default()
        }
    }

    /// Builds the status bubble shown when hovering a link (CSS px, at the origin).
    fn status_list(&self, text: &str) -> DisplayList {
        let mut ui = Ui {
            fonts: &self.ui_fonts,
            items: Vec::new(),
        };
        let w = ui.measure(text, 12.0) + 16.0;
        let h = 22.0;
        ui.rect(Rect::new(0.0, 0.0, w, h), Color::rgb(0xf8, 0xf8, 0xfa), 0.0);
        ui.border(Rect::new(0.0, 0.0, w, h), 1.0, FIELD_BORDER, 0.0);
        ui.text(8.0, 0.0, h, text, 12.0, ICON);
        DisplayList {
            items: ui.items,
            background: Color::rgb(0xf8, 0xf8, 0xfa),
            width: w,
            height: h,
            ..Default::default()
        }
    }

    fn draw(&mut self) {
        let Some(window) = self.window.clone() else { return };
        let size = window.inner_size();
        let (Some(w), Some(h)) = (NonZeroU32::new(size.width), NonZeroU32::new(size.height)) else {
            return;
        };
        let dpr = self.dpr();
        let toolbar_px = ((TOOLBAR_H * dpr).round() as u32).min(size.height);
        let page_px_h = size.height - toolbar_px;
        // Page viewport (cached between frames).
        let needs_page = self.page_dirty
            || self
                .page_canvas
                .as_ref()
                .is_none_or(|c| c.width != size.width || c.height != page_px_h);
        if needs_page && page_px_h > 0 {
            let canvas = match (&self.page, &self.scaled) {
                (Some(p), Some(list)) => rasterize(
                    list,
                    &p.fonts,
                    size.width,
                    page_px_h,
                    Point::new(0.0, (self.scroll * dpr).round()),
                ),
                _ => {
                    let mut c = Canvas::new(size.width, page_px_h);
                    c.clear(Color::WHITE);
                    c
                }
            };
            self.page_canvas = Some(canvas);
            self.page_dirty = false;
        }
        let (css_w, _) = self.css_size();
        let toolbar = rasterize(
            &self.toolbar_list(css_w).scaled(dpr),
            &self.ui_fonts,
            size.width,
            toolbar_px,
            Point::default(),
        );
        let status = self.hover_link.as_ref().map(|u| {
            let mut text = u.to_string();
            if text.chars().count() > 90 {
                text = text.chars().take(89).collect::<String>() + "\u{2026}";
            }
            let list = self.status_list(&text);
            let bw = ((list.width * dpr).ceil() as u32).min(size.width);
            let bh = ((list.height * dpr).ceil() as u32).min(page_px_h);
            rasterize(&list.scaled(dpr), &self.ui_fonts, bw, bh, Point::default())
        });
        let Some(surface) = self.surface.as_mut() else { return };
        if surface.resize(w, h).is_err() {
            return;
        }
        let Ok(mut buffer) = surface.buffer_mut() else { return };
        let width = size.width as usize;
        let blit = |buf: &mut [u32], c: &Canvas, x0: usize, y0: usize| {
            for y in 0..c.height as usize {
                let dy = y0 + y;
                if dy >= size.height as usize {
                    break;
                }
                let row = &c.data[y * c.width as usize * 4..(y + 1) * c.width as usize * 4];
                let dst = &mut buf[dy * width + x0..dy * width + (x0 + c.width as usize).min(width)];
                for (d, px) in dst.iter_mut().zip(row.as_chunks::<4>().0) {
                    *d = (px[0] as u32) << 16 | (px[1] as u32) << 8 | px[2] as u32;
                }
            }
        };
        blit(&mut buffer, &toolbar, 0, 0);
        if let Some(page) = &self.page_canvas {
            blit(&mut buffer, page, 0, toolbar_px as usize);
        }
        if let Some(s) = &status {
            let margin = (4.0 * dpr) as usize;
            let y0 = (size.height as usize).saturating_sub(s.height as usize + margin);
            blit(&mut buffer, s, margin, y0);
        }
        let _ = buffer.present();
    }
}

fn strip_fragment(u: &Url) -> String {
    let mut u = u.clone();
    u.set_fragment(None);
    u.to_string()
}

impl ApplicationHandler<UserEvent> for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_some() {
            return;
        }
        let attrs = Window::default_attributes()
            .with_title("browser-engine")
            .with_inner_size(LogicalSize::new(self.initial_size.0, self.initial_size.1))
            .with_min_inner_size(LogicalSize::new(320, 200));
        let window = match event_loop.create_window(attrs) {
            Ok(w) => Rc::new(w),
            Err(e) => {
                eprintln!("error: cannot create window: {e}");
                event_loop.exit();
                return;
            }
        };
        let surface =
            softbuffer::Context::new(window.clone()).and_then(|ctx| softbuffer::Surface::new(&ctx, window.clone()));
        match surface {
            Ok(s) => self.surface = Some(s),
            Err(e) => {
                eprintln!("error: cannot create drawing surface: {e}");
                event_loop.exit();
                return;
            }
        }
        self.window = Some(window);
        let start = self.start.take().or_else(|| Url::parse(START_URL).ok());
        if let Some(u) = start {
            self.start_load(u, true, None);
        }
    }

    fn user_event(&mut self, _event_loop: &ActiveEventLoop, event: UserEvent) {
        match event {
            UserEvent::Loaded { id, url, result } => {
                if self.loading.as_ref().is_some_and(|l| l.id == id) {
                    let page = result.map(Page::from_raw);
                    self.finish_load(id, url, page);
                }
            }
        }
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        match event {
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::RedrawRequested => self.draw(),
            WindowEvent::Resized(PhysicalSize { .. }) | WindowEvent::ScaleFactorChanged { .. } => {
                self.relayout();
                self.redraw();
            }
            WindowEvent::ModifiersChanged(m) => self.mods = m.state(),
            WindowEvent::CursorMoved { position, .. } => {
                self.cursor = position;
                self.refresh_hover();
            }
            WindowEvent::CursorLeft { .. } => {
                if self.hover_link.take().is_some() || self.hover_target.take().is_some() {
                    self.redraw();
                }
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } => self.click(),
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Back,
                ..
            } => self.go(-1),
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Forward,
                ..
            } => self.go(1),
            WindowEvent::MouseWheel { delta, .. } => {
                let dy = match delta {
                    MouseScrollDelta::LineDelta(_, y) => -y * LINE_SCROLL,
                    MouseScrollDelta::PixelDelta(p) => -(p.y as f32) / self.dpr(),
                };
                self.set_scroll(self.scroll + dy);
            }
            WindowEvent::KeyboardInput { event, .. } => self.handle_key(event_loop, &event),
            _ => {}
        }
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        if self.loading.is_some() {
            // Animate the progress bar while loading.
            self.redraw();
            event_loop.set_control_flow(ControlFlow::WaitUntil(Instant::now() + Duration::from_millis(33)));
        } else {
            event_loop.set_control_flow(ControlFlow::Wait);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn address_field_editing() {
        let mut a = AddressField {
            text: "héllo".into(),
            caret: 5,
            editing: true,
            all_selected: false,
        };
        a.backspace();
        assert_eq!(a.text, "héll");
        a.caret = 1;
        a.insert("X");
        assert_eq!(a.text, "hXéll");
        a.delete();
        assert_eq!(a.text, "hXll");
        a.all_selected = true;
        a.insert("new");
        assert_eq!((a.text.as_str(), a.caret), ("new", 3));
    }

    #[test]
    fn built_in_pages_render() {
        let url = Url::parse(START_URL).unwrap();
        let page = Page::from_html(&start_page_html(), url.clone());
        let r = page.render(900.0, 700.0);
        assert!(r.layout.document_height > 300.0);
        assert!(!r.display_list.links.is_empty(), "start page has clickable links");
        let err = Page::from_html(&error_page_html(&url, "boom <x>"), url);
        assert!(err.doc.text_content(err.doc.root()).contains("boom <x>"));
    }

    #[test]
    fn fragments_are_ignored_when_comparing_documents() {
        let a = Url::parse("https://a.test/p#x").unwrap();
        let b = Url::parse("https://a.test/p#y").unwrap();
        assert_eq!(strip_fragment(&a), strip_fragment(&b));
    }
}
