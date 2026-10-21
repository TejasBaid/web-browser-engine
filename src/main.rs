//! `browser`: command-line front end for the engine.

use browser_engine::dom::serialize_outline;
use browser_engine::layout::dump_layout;
use browser_engine::net::url_from_arg;
use browser_engine::page::{LoadOptions, Page, render_url};
use clap::{Parser, Subcommand};
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::{Duration, Instant};

#[derive(Parser)]
#[command(
    name = "browser",
    version,
    about = "A web browser engine written from scratch in Rust"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Render a page to a PNG file (headless).
    Render {
        /// URL or file path.
        url: String,
        /// Viewport width in CSS px.
        #[arg(long, default_value_t = 1024)]
        width: u32,
        /// Viewport height; the image is this tall unless --full-page is given.
        #[arg(long, default_value_t = 768)]
        height: u32,
        /// Capture the whole document height instead of one viewport.
        #[arg(long)]
        full_page: bool,
        /// Output PNG path.
        #[arg(long, short, default_value = "page.png")]
        out: PathBuf,
        /// Print per-stage timings.
        #[arg(long)]
        timings: bool,
    },
    /// Print the parsed DOM tree.
    DumpDom {
        /// URL or file path.
        url: String,
    },
    /// Print the fragment (layout) tree with absolute coordinates.
    DumpLayout {
        /// URL or file path.
        url: String,
        /// Viewport width in CSS px.
        #[arg(long, default_value_t = 1024)]
        width: u32,
        /// Viewport height in CSS px.
        #[arg(long, default_value_t = 768)]
        height: u32,
    },
    /// Measure parse/style/layout/paint time (median of several runs).
    Bench {
        /// URL or file path.
        url: String,
        /// Viewport width.
        #[arg(long, default_value_t = 1024)]
        width: u32,
        /// Viewport height (the painted area).
        #[arg(long, default_value_t = 768)]
        height: u32,
        /// Number of measured runs.
        #[arg(long, default_value_t = 5)]
        runs: usize,
    },
    /// Open a window and browse.
    #[cfg(feature = "gui")]
    Open {
        /// URL or file path (defaults to a start page).
        url: Option<String>,
        /// Initial window width.
        #[arg(long, default_value_t = 1100)]
        width: u32,
        /// Initial window height.
        #[arg(long, default_value_t = 800)]
        height: u32,
    },
}

fn ms(d: Duration) -> f64 {
    d.as_secs_f64() * 1000.0
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    match run(cli) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}

fn run(cli: Cli) -> Result<(), String> {
    match cli.command {
        Command::Render {
            url,
            width,
            height,
            full_page,
            out,
            timings,
        } => {
            let u = url_from_arg(&url)?;
            let start = Instant::now();
            let (page, rendered, canvas) = render_url(&u, width, if full_page { None } else { Some(height) })?;
            canvas.save_png(&out)?;
            for e in &page.errors {
                eprintln!("warning: {e}");
            }
            let t = &rendered.timings;
            let lt = &page.load_timings;
            println!(
                "rendered {} ({}x{} px, document height {:.0}px) -> {}",
                u,
                canvas.width,
                canvas.height,
                rendered.layout.document_height,
                out.display()
            );
            if timings {
                println!(
                    "fetch {:.1} ms | parse HTML {:.1} ms | subresources {:.1} ms | parse CSS {:.1} ms | style {:.1} ms | layout {:.1} ms | display list {:.1} ms | raster {:.1} ms | total {:.1} ms",
                    ms(lt.fetch_document),
                    ms(lt.parse_html),
                    ms(lt.fetch_subresources),
                    ms(t.parse_css),
                    ms(t.style),
                    ms(t.layout),
                    ms(t.display_list),
                    ms(t.raster),
                    ms(start.elapsed())
                );
            }
            Ok(())
        }
        Command::DumpDom { url } => {
            let u = url_from_arg(&url)?;
            let page = Page::load(
                &u,
                &LoadOptions {
                    subresources: false,
                    ..Default::default()
                },
            )?;
            print!("{}", serialize_outline(&page.doc));
            Ok(())
        }
        Command::DumpLayout { url, width, height } => {
            let u = url_from_arg(&url)?;
            let page = Page::load(
                &u,
                &LoadOptions {
                    viewport: (width as f32, height as f32),
                    subresources: true,
                },
            )?;
            let r = page.render(width as f32, height as f32);
            print!("{}", dump_layout(&r.layout, &page.doc));
            Ok(())
        }
        Command::Bench {
            url,
            width,
            height,
            runs,
        } => bench(&url, width, height, runs),
        #[cfg(feature = "gui")]
        Command::Open { url, width, height } => {
            let start = match url {
                Some(u) => Some(url_from_arg(&u)?),
                None => None,
            };
            browser_engine::gui::run(start, width, height)
        }
    }
}

fn median(mut v: Vec<f64>) -> f64 {
    v.sort_by(|a, b| a.partial_cmp(b).unwrap());
    v[v.len() / 2]
}

fn bench(url: &str, width: u32, height: u32, runs: usize) -> Result<(), String> {
    let u = url_from_arg(url)?;
    let opts = LoadOptions {
        viewport: (width as f32, height as f32),
        subresources: true,
    };
    let resp = browser_engine::net::fetch(&u)?;
    let html_len = resp.bytes.len();
    let html = browser_engine::net::decode_text(&resp.bytes, resp.content_type.as_deref(), true);
    let raw = browser_engine::page::load_html(&html, resp.url, &opts, Duration::ZERO)?;
    let sources = raw.sheets.clone();
    let page = Page::from_raw(raw);
    let elements = page
        .doc
        .descendants(page.doc.root())
        .filter(|&n| page.doc.element(n).is_some())
        .count();
    let rules: usize = page
        .sheets
        .iter()
        .filter(|s| s.origin == browser_engine::style::Origin::Author)
        .map(|s| s.style_rule_count())
        .sum();
    let (mut parse, mut css, mut style, mut layout, mut dl, mut raster) =
        (vec![], vec![], vec![], vec![], vec![], vec![]);
    let mut doc_h = 0.0;
    let mut items = 0;
    for _ in 0..runs.max(1) {
        let t = Instant::now();
        let d = browser_engine::html::parse_document(&html);
        parse.push(ms(t.elapsed()));
        drop(d);
        let t = Instant::now();
        let sheets = browser_engine::page::build_sheets(&sources);
        css.push(ms(t.elapsed()));
        drop(sheets);
        let mut r = page.render(width as f32, height as f32);
        let _ = page.paint(&mut r, width, height, 0.0);
        style.push(ms(r.timings.style));
        layout.push(ms(r.timings.layout));
        dl.push(ms(r.timings.display_list));
        raster.push(ms(r.timings.raster));
        doc_h = r.layout.document_height;
        items = r.display_list.items.len();
    }
    println!("{u}");
    println!(
        "  {:.1} MB HTML, {} elements, {} author style rules, document height {:.0}px, {} display items",
        html_len as f64 / 1e6,
        elements,
        rules,
        doc_h,
        items
    );
    println!(
        "  median of {} runs: parse HTML {:.1} ms | parse CSS {:.1} ms | style {:.1} ms | layout {:.1} ms | display list {:.1} ms | raster {}x{} {:.1} ms",
        runs,
        median(parse),
        median(css),
        median(style),
        median(layout),
        median(dl),
        width,
        height,
        median(raster)
    );
    Ok(())
}
