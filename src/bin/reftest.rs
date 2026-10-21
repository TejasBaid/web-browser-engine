//! Reftest runner: renders every page in `tests/reftests/` with the engine at 800x600,
//! compares it with the Chrome reference in `tests/reftests/ref/`, and reports pixel
//! match and SSIM per test.
//!
//! ```text
//! cargo run --release --bin reftest                 # print the score table
//! cargo run --release --bin reftest -- --images     # also write comparison images to docs/images/
//! cargo run --release --bin reftest -- --update-floors  # rewrite tests/reftests/floors.txt
//! cargo run --release --bin reftest -- flex-wrap    # run selected tests
//! ```

use browser_engine::imgdiff::{compare, content_match, diff_image, panel_origins, side_by_side, stack_vertical};
use browser_engine::net::url_from_arg;
use browser_engine::page::render_url;
use browser_engine::text::FontDb;
use image::{Rgba, RgbaImage};
use std::path::{Path, PathBuf};

const WIDTH: u32 = 800;
const HEIGHT: u32 = 600;
/// Per-channel tolerance for the pixel-match score.
pub const TOLERANCE: u8 = 16;

struct Result_ {
    name: String,
    pixel: f64,
    content: f64,
    ssim: f64,
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().map(String::as_str) == Some("--hprobe") {
        hprobe(&args[1..]);
        return;
    }
    if args.first().map(String::as_str) == Some("--probe") {
        probe(&args[1..]);
        return;
    }
    if args.first().map(String::as_str) == Some("--shift") {
        shift(&args[1..]);
        return;
    }
    if args.first().map(String::as_str) == Some("--compose") {
        compose(&args[1..]);
        return;
    }
    if args.first().map(String::as_str) == Some("--zoom") {
        zoom(&args[1..]);
        return;
    }
    let images = args.iter().any(|a| a == "--images");
    let update = args.iter().any(|a| a == "--update-floors");
    let filter: Vec<&String> = args.iter().filter(|a| !a.starts_with("--")).collect();
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let dir = root.join("tests/reftests");
    let mut names: Vec<String> = std::fs::read_dir(&dir)
        .expect("tests/reftests")
        .filter_map(|e| {
            let p = e.ok()?.path();
            (p.extension()? == "html").then(|| p.file_stem().unwrap().to_string_lossy().into_owned())
        })
        .filter(|n| filter.is_empty() || filter.iter().any(|f| n.contains(f.as_str())))
        .collect();
    names.sort();
    let out_dir = root.join("docs/images/reftests");
    if images {
        std::fs::create_dir_all(&out_dir).unwrap();
    }
    let fonts = FontDb::new();
    let mut results = Vec::new();
    let mut panels: Vec<(String, RgbaImage)> = Vec::new();
    for name in &names {
        let page = dir.join(format!("{name}.html"));
        let reference = dir.join("ref").join(format!("{name}.png"));
        let Ok(chrome) = image::open(&reference).map(|i| i.to_rgba8()) else {
            eprintln!("{name}: missing reference {}", reference.display());
            continue;
        };
        let engine = render(&page);
        let score = compare(&engine, &chrome, TOLERANCE);
        results.push(Result_ {
            name: name.clone(),
            pixel: score.pixel_match * 100.0,
            content: content_match(&engine, &chrome, TOLERANCE) * 100.0,
            ssim: score.ssim,
        });
        if images {
            let diff = diff_image(&engine, &chrome, TOLERANCE);
            let mut sbs = side_by_side(&[&engine, &chrome, &diff], 12, 34);
            let origins = panel_origins(&[&engine, &chrome, &diff], 12, 34);
            let labels = [
                format!("engine  —  {name}"),
                "Chrome".to_string(),
                format!(
                    "diff  —  {:.1}% match, SSIM {:.3}",
                    score.pixel_match * 100.0,
                    score.ssim
                ),
            ];
            for (i, (x, _)) in origins.iter().enumerate() {
                draw_label(&fonts, &mut sbs, *x as f32 + 4.0, 23.0, &labels[i]);
            }
            save_png(&sbs, &out_dir.join(format!("{name}.png")));
            panels.push((name.clone(), sbs));
        }
    }
    // Report.
    println!("| Test | Pixel match | Content match | SSIM |");
    println!("|---|---:|---:|---:|");
    for r in &results {
        println!("| `{}` | {:.1}% | {:.1}% | {:.3} |", r.name, r.pixel, r.content, r.ssim);
    }
    if !results.is_empty() {
        let n = results.len() as f64;
        let mean_px = results.iter().map(|r| r.pixel).sum::<f64>() / n;
        let mean_ssim = results.iter().map(|r| r.ssim).sum::<f64>() / n;
        let mean_content = results.iter().map(|r| r.content).sum::<f64>() / n;
        let mut sorted: Vec<f64> = results.iter().map(|r| r.pixel).collect();
        sorted.sort_by(|a, b| a.partial_cmp(b).unwrap());
        let median = sorted[sorted.len() / 2];
        println!(
            "| **mean of {}** | **{:.1}%** | **{:.1}%** | **{:.3}** |",
            results.len(),
            mean_px,
            mean_content,
            mean_ssim
        );
        println!(
            "\nmedian pixel match {median:.1}%, min {:.1}%, max {:.1}%",
            sorted[0],
            sorted[sorted.len() - 1]
        );
    }
    if update {
        let mut s = String::from(
            "# Minimum pixel-match % per reftest (CI regression floors, 1.5 points below the measured score).\n",
        );
        for r in &results {
            s.push_str(&format!("{} {:.1}\n", r.name, (r.pixel - 1.5).max(0.0)));
        }
        std::fs::write(dir.join("floors.txt"), s).unwrap();
        println!("wrote tests/reftests/floors.txt");
    }
    if images && !panels.is_empty() {
        write_grid(&root, &panels);
    }
}

fn render(page: &Path) -> RgbaImage {
    let url = url_from_arg(page.to_str().unwrap()).unwrap();
    let (_, _, canvas) = render_url(&url, WIDTH, Some(HEIGHT)).expect("render");
    RgbaImage::from_raw(canvas.width, canvas.height, canvas.to_rgba()).unwrap()
}

fn save_png(img: &RgbaImage, path: &Path) {
    // Flat RGB with maximum compression keeps the gallery small.
    use image::ImageEncoder;
    use image::codecs::png::{CompressionType, FilterType, PngEncoder};
    let rgb = image::DynamicImage::ImageRgba8(img.clone()).to_rgb8();
    let file = std::fs::File::create(path).unwrap();
    let enc = PngEncoder::new_with_quality(
        std::io::BufWriter::new(file),
        CompressionType::Best,
        FilterType::Adaptive,
    );
    enc.write_image(rgb.as_raw(), rgb.width(), rgb.height(), image::ExtendedColorType::Rgb8)
        .unwrap();
}

/// Draws a label with the engine's own text rasterizer.
fn draw_label(fonts: &FontDb, img: &mut RgbaImage, x: f32, baseline: f32, text: &str) {
    let fam: std::rc::Rc<[String]> = std::rc::Rc::from(vec!["Liberation Sans".to_string()]);
    let face = fonts.select(&fam, 700, false);
    let size = 15.0;
    let mut pen = x;
    for g in fonts.shape(face, size, text, 0.0, 0.0) {
        let r = fonts.rasterize(g.face, g.glyph, size, 0);
        let gx = pen.round() as i32 + r.left;
        let gy = baseline as i32 + r.top;
        for yy in 0..r.height as i32 {
            for xx in 0..r.width as i32 {
                let (px, py) = (gx + xx, gy + yy);
                if px < 0 || py < 0 || px >= img.width() as i32 || py >= img.height() as i32 {
                    continue;
                }
                let c = r.coverage[(yy as u32 * r.width + xx as u32) as usize] as f32 / 255.0;
                let p = img.get_pixel_mut(px as u32, py as u32);
                for k in 0..3 {
                    p[k] = (p[k] as f32 * (1.0 - c) + 40.0 * c) as u8;
                }
            }
        }
        pen += g.advance;
    }
}

/// The last row (below the label header) that contains a non-white, non-background pixel.
fn last_content_row(img: &RgbaImage) -> u32 {
    for y in (40..img.height()).rev() {
        let row_has_content = (0..img.width()).any(|x| {
            let p = img.get_pixel(x, y);
            let white = p[0] > 250 && p[1] > 250 && p[2] > 250;
            !white && *p != browser_engine::imgdiff::PANEL_BACKGROUND
        });
        if row_has_content {
            return y;
        }
    }
    img.height()
}

/// A showcase grid of downscaled comparisons (two per row).
fn write_grid(root: &Path, panels: &[(String, RgbaImage)]) {
    let showcase = [
        "mixed-cards",
        "mixed-article",
        "mixed-navbar",
        "flex-justify-content",
        "grid",
        "gradients",
        "floats",
        "border-radius",
        "z-index",
        "table-collapse",
        "inline-boxes",
        "text-styles",
    ];
    let mut rows = Vec::new();
    let chosen: Vec<&RgbaImage> = showcase
        .iter()
        .filter_map(|n| panels.iter().find(|(p, _)| p == n).map(|(_, i)| i))
        .collect();
    for pair in chosen.chunks(2) {
        // Crop the empty bottom of the panels (same height for both in a row).
        let content_bottom = pair.iter().map(|i| last_content_row(i)).max().unwrap_or(0);
        let crop_h = (content_bottom + 24).min(pair[0].height());
        let scaled: Vec<RgbaImage> = pair
            .iter()
            .map(|i| {
                let c = image::imageops::crop_imm(*i, 0, 0, i.width(), crop_h.min(i.height())).to_image();
                image::imageops::resize(&c, c.width() / 2, c.height() / 2, image::imageops::FilterType::Triangle)
            })
            .collect();
        let refs: Vec<&RgbaImage> = scaled.iter().collect();
        rows.push(side_by_side(&refs, 16, 0));
    }
    let row_refs: Vec<&RgbaImage> = rows.iter().collect();
    let mut grid = stack_vertical(&row_refs, 16);
    // White background margin.
    for p in grid.pixels_mut() {
        if p[3] == 0 {
            *p = Rgba([255, 255, 255, 255]);
        }
    }
    save_png(&grid, &root.join("docs/images/reftest-grid.png"));
    println!("wrote docs/images/reftest-grid.png");
}

/// `--zoom <name> <x> <y> <w> <h> <out.png>`: writes an 8x enlarged engine | Chrome crop.
fn zoom(args: &[String]) {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let dir = root.join("tests/reftests");
    let name = &args[0];
    let n: Vec<u32> = args[1..5].iter().map(|v| v.parse().unwrap()).collect();
    let engine = render(&dir.join(format!("{name}.html")));
    let chrome = image::open(dir.join("ref").join(format!("{name}.png")))
        .unwrap()
        .to_rgba8();
    let crop = |img: &RgbaImage| {
        let c = image::imageops::crop_imm(img, n[0], n[1], n[2], n[3]).to_image();
        image::imageops::resize(&c, n[2] * 8, n[3] * 8, image::imageops::FilterType::Nearest)
    };
    let (a, b) = (crop(&engine), crop(&chrome));
    let out = side_by_side(&[&a, &b], 8, 0);
    save_png(&out, Path::new(&args[5]));
}

/// `--probe <name> <x0> <x1> <y0> <y1>`: per row, the summed darkness over [x0, x1),
/// engine vs Chrome (for finding vertical offsets).
fn probe(args: &[String]) {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let dir = root.join("tests/reftests");
    let name = &args[0];
    let n: Vec<u32> = args[1..5].iter().map(|v| v.parse().unwrap()).collect();
    let engine = render(&dir.join(format!("{name}.html")));
    let chrome = image::open(dir.join("ref").join(format!("{name}.png")))
        .unwrap()
        .to_rgba8();
    let dark = |img: &RgbaImage, y: u32| -> u32 {
        (n[0]..n[1]).map(|x| 255 - img.get_pixel(x, y)[1] as u32).sum::<u32>() / 255
    };
    for y in n[2]..n[3] {
        println!("y={y:4}  engine {:5}  chrome {:5}", dark(&engine, y), dark(&chrome, y));
    }
}

/// `--compose <engine.png> <chrome.png> <out.png> <title> [max_height]`: a labelled
/// engine | Chrome side-by-side for real-site screenshots.
fn compose(args: &[String]) {
    let fonts = FontDb::new();
    let max_h: u32 = args.get(4).and_then(|v| v.parse().ok()).unwrap_or(u32::MAX);
    let load = |p: &str| {
        let i = image::open(p).unwrap().to_rgba8();
        let h = i.height().min(max_h);
        image::imageops::crop_imm(&i, 0, 0, i.width(), h).to_image()
    };
    let a = load(&args[0]);
    let b = load(&args[1]);
    let mut out = side_by_side(&[&a, &b], 12, 34);
    let origins = panel_origins(&[&a, &b], 12, 34);
    draw_label(
        &fonts,
        &mut out,
        origins[0].0 as f32 + 4.0,
        23.0,
        &format!("engine  —  {}", args[3]),
    );
    draw_label(&fonts, &mut out, origins[1].0 as f32 + 4.0, 23.0, "Chrome");
    save_png(&out, Path::new(&args[2]));
}

/// `--shift <name> <x> <y> <w> <h>`: finds the (dx, dy) offset of the engine image that
/// best matches Chrome in a region (sum of absolute differences), to spot systematic
/// sub-pixel offsets.
fn shift(args: &[String]) {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let dir = root.join("tests/reftests");
    let name = &args[0];
    let n: Vec<i32> = args[1..5].iter().map(|v| v.parse().unwrap()).collect();
    let engine = render(&dir.join(format!("{name}.html")));
    let chrome = image::open(dir.join("ref").join(format!("{name}.png")))
        .unwrap()
        .to_rgba8();
    let mut results = Vec::new();
    for dy in -2..=2 {
        for dx in -2..=2 {
            let mut sad = 0u64;
            for y in n[1]..n[1] + n[3] {
                for x in n[0]..n[0] + n[2] {
                    let e = engine.get_pixel((x + dx) as u32, (y + dy) as u32)[1] as i64;
                    let c = chrome.get_pixel(x as u32, y as u32)[1] as i64;
                    sad += (e - c).unsigned_abs();
                }
            }
            results.push((sad, dx, dy));
        }
    }
    results.sort();
    for (sad, dx, dy) in results.iter().take(5) {
        println!("dx={dx:+} dy={dy:+}  SAD={sad}");
    }
}

/// `--hprobe <name> <y> <x0> <x1>`: the green channel along a pixel row, engine vs Chrome.
fn hprobe(args: &[String]) {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let dir = root.join("tests/reftests");
    let name = &args[0];
    let n: Vec<u32> = args[1..4].iter().map(|v| v.parse().unwrap()).collect();
    let engine = render(&dir.join(format!("{name}.html")));
    let chrome = image::open(dir.join("ref").join(format!("{name}.png")))
        .unwrap()
        .to_rgba8();
    for x in n[1]..n[2] {
        println!(
            "x={x:4}  engine {:3}  chrome {:3}",
            engine.get_pixel(x, n[0])[1],
            chrome.get_pixel(x, n[0])[1]
        );
    }
}
