//! Reftest regression suite: renders every page in `tests/reftests/` with the engine and
//! compares it against the stored headless-Chrome screenshot in `tests/reftests/ref/`.
//! Each test must stay at or above its floor in `tests/reftests/floors.txt` (set 1.5
//! points below the measured pixel-match score). Chrome is not needed to run this.
//!
//! Regenerate references with `scripts/reftest-chrome.sh` and floors with
//! `cargo run --release --bin reftest -- --update-floors`.

use browser_engine::imgdiff::compare;
use browser_engine::net::url_from_arg;
use browser_engine::page::render_url;
use image::RgbaImage;
use std::path::PathBuf;

#[test]
fn reftests_meet_their_floors() {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/reftests");
    let floors = std::fs::read_to_string(dir.join("floors.txt")).expect("floors.txt");
    let mut failures = Vec::new();
    let mut count = 0;
    for line in floors.lines().filter(|l| !l.starts_with('#') && !l.trim().is_empty()) {
        let mut parts = line.split_whitespace();
        let name = parts.next().unwrap();
        let floor: f64 = parts.next().unwrap().parse().unwrap();
        let url = url_from_arg(dir.join(format!("{name}.html")).to_str().unwrap()).unwrap();
        let (_, _, canvas) = render_url(&url, 800, Some(600)).expect("render");
        let engine = RgbaImage::from_raw(canvas.width, canvas.height, canvas.to_rgba()).unwrap();
        let chrome = image::open(dir.join("ref").join(format!("{name}.png")))
            .unwrap()
            .to_rgba8();
        let score = compare(&engine, &chrome, 16).pixel_match * 100.0;
        if score < floor {
            failures.push(format!("{name}: {score:.2}% < floor {floor:.1}%"));
        }
        count += 1;
    }
    assert!(count >= 40, "expected the full reftest gallery, found {count}");
    assert!(failures.is_empty(), "reftest regressions:\n{}", failures.join("\n"));
}
