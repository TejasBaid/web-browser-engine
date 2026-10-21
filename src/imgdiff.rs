//! Image comparison utilities for the reftest suite.
//!
//! Everything here works on [`image::RgbaImage`] and is independent of the rest of the
//! engine, so the same code scores engine renders against Chrome screenshots, renders the
//! diff visualisations and composes the side-by-side gallery images.
//!
//! Two scores are provided:
//! - [`pixel_match`]: the fraction of pixels whose largest per-channel difference is within
//!   a tolerance. Easy to interpret, harsh on sub-pixel text differences.
//! - [`ssim`]: mean structural similarity on luma, which is more forgiving of small
//!   anti-aliasing differences and more sensitive to structural (layout) differences.

use image::{Rgba, RgbaImage};

/// Both similarity scores for a pair of images.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Score {
    /// Fraction of matching pixels in `[0, 1]` (see [`pixel_match`]).
    pub pixel_match: f64,
    /// Mean SSIM in `[-1, 1]`, in practice `[0, 1]` (see [`ssim`]).
    pub ssim: f64,
}

/// Computes both [`pixel_match`] (with `tolerance`) and [`ssim`].
pub fn compare(a: &RgbaImage, b: &RgbaImage, tolerance: u8) -> Score {
    Score {
        pixel_match: pixel_match(a, b, tolerance),
        ssim: ssim(a, b),
    }
}

/// Largest absolute per-channel (RGBA) difference between two pixels.
#[inline]
pub fn channel_diff(p: &Rgba<u8>, q: &Rgba<u8>) -> u8 {
    p.0.iter()
        .zip(q.0.iter())
        .map(|(x, y)| x.abs_diff(*y))
        .max()
        .unwrap_or(0)
}

/// Fraction of pixels whose maximum per-channel difference is `<= tolerance`.
///
/// Images of different sizes are compared over the union of their areas; pixels that
/// exist in only one of the images count as mismatches. Two empty images match fully.
pub fn pixel_match(a: &RgbaImage, b: &RgbaImage, tolerance: u8) -> f64 {
    let (w, h) = (a.width().max(b.width()), a.height().max(b.height()));
    let total = u64::from(w) * u64::from(h);
    if total == 0 {
        return 1.0;
    }
    let (ow, oh) = (a.width().min(b.width()), a.height().min(b.height()));
    let mut matching = 0u64;
    for y in 0..oh {
        for x in 0..ow {
            if channel_diff(a.get_pixel(x, y), b.get_pixel(x, y)) <= tolerance {
                matching += 1;
            }
        }
    }
    matching as f64 / total as f64
}

/// Like [`pixel_match`], but only over "content" pixels: those that are not pure white
/// in at least one of the two images. This removes the inflating effect of large empty
/// page backgrounds. Returns 1.0 if neither image has content.
pub fn content_match(a: &RgbaImage, b: &RgbaImage, tolerance: u8) -> f64 {
    let (ow, oh) = (a.width().min(b.width()), a.height().min(b.height()));
    let white = Rgba([255, 255, 255, 255]);
    let (mut total, mut matching) = (0u64, 0u64);
    for y in 0..oh {
        for x in 0..ow {
            let (p, q) = (a.get_pixel(x, y), b.get_pixel(x, y));
            if *p == white && *q == white {
                continue;
            }
            total += 1;
            if channel_diff(p, q) <= tolerance {
                matching += 1;
            }
        }
    }
    if total == 0 {
        1.0
    } else {
        matching as f64 / total as f64
    }
}

/// Rec. 601 luma of a pixel after compositing it over opaque white.
#[inline]
pub fn luma(p: &Rgba<u8>) -> f64 {
    let [r, g, b, a] = p.0;
    let alpha = f64::from(a) / 255.0;
    let over_white = |c: u8| f64::from(c) * alpha + 255.0 * (1.0 - alpha);
    0.299 * over_white(r) + 0.587 * over_white(g) + 0.114 * over_white(b)
}

/// Side length of the square SSIM window.
pub const SSIM_WINDOW: u32 = 8;
/// Step between neighbouring SSIM windows (windows overlap by half).
pub const SSIM_STRIDE: u32 = 4;

/// Mean structural similarity (SSIM, Wang et al. 2004) over 8x8 luma windows.
///
/// Windows are placed every [`SSIM_STRIDE`] pixels and use uniform weights with the
/// standard constants `C1 = (0.01 * 255)^2` and `C2 = (0.03 * 255)^2`. Images of different
/// sizes are padded to the union size with opaque white. Identical images score 1.0.
pub fn ssim(a: &RgbaImage, b: &RgbaImage) -> f64 {
    let (w, h) = (a.width().max(b.width()), a.height().max(b.height()));
    if w == 0 || h == 0 {
        return 1.0;
    }
    let plane = |img: &RgbaImage| -> Vec<f64> {
        let mut v = vec![255.0; (w * h) as usize];
        for (x, y, p) in img.enumerate_pixels() {
            v[(y * w + x) as usize] = luma(p);
        }
        v
    };
    let (la, lb) = (plane(a), plane(b));
    const C1: f64 = (0.01 * 255.0) * (0.01 * 255.0);
    const C2: f64 = (0.03 * 255.0) * (0.03 * 255.0);
    let win_w = SSIM_WINDOW.min(w);
    let win_h = SSIM_WINDOW.min(h);
    let n = f64::from(win_w * win_h);
    let starts = |len: u32, win: u32| -> Vec<u32> {
        let mut s: Vec<u32> = (0..=len - win).step_by(SSIM_STRIDE as usize).collect();
        if *s.last().unwrap() != len - win {
            s.push(len - win);
        }
        s
    };
    let (xs, ys) = (starts(w, win_w), starts(h, win_h));
    let mut sum = 0.0;
    let mut count = 0usize;
    for &y0 in &ys {
        for &x0 in &xs {
            // Two passes (means, then centred moments) keep identical windows exact.
            let (mut sa, mut sb) = (0.0, 0.0);
            for y in y0..y0 + win_h {
                let row = (y * w) as usize;
                for x in x0..x0 + win_w {
                    sa += la[row + x as usize];
                    sb += lb[row + x as usize];
                }
            }
            let (ma, mb) = (sa / n, sb / n);
            let (mut va, mut vb, mut cov) = (0.0, 0.0, 0.0);
            for y in y0..y0 + win_h {
                let row = (y * w) as usize;
                for x in x0..x0 + win_w {
                    let (p, q) = (la[row + x as usize] - ma, lb[row + x as usize] - mb);
                    va += p * p;
                    vb += q * q;
                    cov += p * q;
                }
            }
            let (va, vb, cov) = (va / n, vb / n, cov / n);
            sum += ((2.0 * ma * mb + C1) * (2.0 * cov + C2)) / ((ma * ma + mb * mb + C1) * (va + vb + C2));
            count += 1;
        }
    }
    sum / count as f64
}

/// Visualises the differences between `a` and `b`.
///
/// The result has the union size of both images. Pixels within `tolerance` are drawn as a
/// faded grayscale version of `a`; mismatching pixels are red, brighter for larger
/// differences; pixels present in only one image are pure red.
pub fn diff_image(a: &RgbaImage, b: &RgbaImage, tolerance: u8) -> RgbaImage {
    let (w, h) = (a.width().max(b.width()), a.height().max(b.height()));
    let mut out = RgbaImage::new(w, h);
    for y in 0..h {
        for x in 0..w {
            let pa = (x < a.width() && y < a.height()).then(|| a.get_pixel(x, y));
            let pb = (x < b.width() && y < b.height()).then(|| b.get_pixel(x, y));
            let px = match (pa, pb) {
                (Some(p), Some(q)) => {
                    let d = channel_diff(p, q);
                    if d <= tolerance {
                        let g = (192.0 + luma(p) / 4.0).round() as u8;
                        Rgba([g, g, g, 255])
                    } else {
                        let r = 96 + (u16::from(d) * 159 / 255) as u8;
                        Rgba([r, 0, 0, 255])
                    }
                }
                _ => Rgba([255, 0, 0, 255]),
            };
            out.put_pixel(x, y, px);
        }
    }
    out
}

/// Background color used by [`side_by_side`] for gaps and headers.
pub const PANEL_BACKGROUND: Rgba<u8> = Rgba([238, 238, 242, 255]);

/// Top-left corner of each panel's image in the canvas produced by [`side_by_side`] with
/// the same arguments. The header area for panel `i` spans `header_height` pixels directly
/// above `origins[i]`, so callers can draw labels there.
pub fn panel_origins(images: &[&RgbaImage], gap: u32, header_height: u32) -> Vec<(u32, u32)> {
    let mut x = gap;
    images
        .iter()
        .map(|img| {
            let origin = (x, gap + header_height);
            x += img.width() + gap;
            origin
        })
        .collect()
}

/// Composes `images` left to right, separated (and surrounded) by `gap` pixels of
/// [`PANEL_BACKGROUND`], with `header_height` blank pixels reserved above every panel for
/// labels. Shorter images are top-aligned.
pub fn side_by_side(images: &[&RgbaImage], gap: u32, header_height: u32) -> RgbaImage {
    let width = gap + images.iter().map(|i| i.width() + gap).sum::<u32>();
    let tallest = images.iter().map(|i| i.height()).max().unwrap_or(0);
    let height = gap + header_height + tallest + gap;
    let mut canvas = RgbaImage::from_pixel(width, height, PANEL_BACKGROUND);
    for (img, (x, y)) in images.iter().zip(panel_origins(images, gap, header_height)) {
        image::imageops::replace(&mut canvas, *img, i64::from(x), i64::from(y));
    }
    canvas
}

/// Stacks images vertically (left-aligned) with `gap` pixels of [`PANEL_BACKGROUND`]
/// between and around them; useful for turning several side-by-side rows into a grid.
pub fn stack_vertical(images: &[&RgbaImage], gap: u32) -> RgbaImage {
    let width = images.iter().map(|i| i.width()).max().unwrap_or(0) + 2 * gap;
    let height = gap + images.iter().map(|i| i.height() + gap).sum::<u32>();
    let mut canvas = RgbaImage::from_pixel(width, height, PANEL_BACKGROUND);
    let mut y = gap;
    for img in images {
        image::imageops::replace(&mut canvas, *img, i64::from(gap), i64::from(y));
        y += img.height() + gap;
    }
    canvas
}

#[cfg(test)]
mod tests {
    use super::*;

    fn solid(w: u32, h: u32, c: [u8; 4]) -> RgbaImage {
        RgbaImage::from_pixel(w, h, Rgba(c))
    }

    #[test]
    fn identical_images_score_one() {
        let mut a = solid(32, 24, [10, 200, 30, 255]);
        a.put_pixel(5, 5, Rgba([0, 0, 0, 255]));
        let s = compare(&a, &a.clone(), 0);
        assert_eq!(s.pixel_match, 1.0);
        assert!((s.ssim - 1.0).abs() < 1e-12);
    }

    #[test]
    fn pixel_match_respects_tolerance() {
        let a = solid(10, 10, [100, 100, 100, 255]);
        let mut b = a.clone();
        for x in 0..10 {
            b.put_pixel(x, 0, Rgba([104, 100, 100, 255]));
        }
        assert_eq!(pixel_match(&a, &b, 3), 0.9);
        assert_eq!(pixel_match(&a, &b, 4), 1.0);
    }

    #[test]
    fn pixel_match_counts_union_area() {
        let a = solid(10, 10, [0, 0, 0, 255]);
        let b = solid(20, 10, [0, 0, 0, 255]);
        assert_eq!(pixel_match(&a, &b, 0), 0.5);
        assert_eq!(pixel_match(&RgbaImage::new(0, 0), &RgbaImage::new(0, 0), 0), 1.0);
    }

    #[test]
    fn ssim_drops_for_structural_change() {
        let a = solid(64, 64, [255, 255, 255, 255]);
        let mut b = a.clone();
        for y in 16..48 {
            for x in 16..48 {
                b.put_pixel(x, y, Rgba([0, 0, 0, 255]));
            }
        }
        let shifted = {
            let mut c = a.clone();
            for y in 16..48 {
                for x in 18..50 {
                    c.put_pixel(x, y, Rgba([0, 0, 0, 255]));
                }
            }
            c
        };
        let big = ssim(&a, &b);
        let small = ssim(&b, &shifted);
        assert!(big < 0.8, "ssim for a missing box: {big}");
        assert!(small > big && small < 1.0, "ssim for a 2px shift: {small}");
    }

    #[test]
    fn luma_composites_over_white() {
        assert_eq!(luma(&Rgba([0, 0, 0, 0])), 255.0);
        assert!(luma(&Rgba([0, 0, 0, 255])).abs() < 1e-9);
    }

    #[test]
    fn diff_image_marks_mismatches_red() {
        let a = solid(4, 2, [255, 255, 255, 255]);
        let mut b = solid(5, 2, [255, 255, 255, 255]);
        b.put_pixel(1, 1, Rgba([0, 0, 0, 255]));
        let d = diff_image(&a, &b, 8);
        assert_eq!(d.dimensions(), (5, 2));
        assert_eq!(*d.get_pixel(0, 0), Rgba([255, 255, 255, 255]));
        assert_eq!(*d.get_pixel(1, 1), Rgba([255, 0, 0, 255]));
        assert_eq!(*d.get_pixel(4, 0), Rgba([255, 0, 0, 255]));
    }

    #[test]
    fn side_by_side_layout() {
        let a = solid(10, 5, [255, 0, 0, 255]);
        let b = solid(20, 8, [0, 0, 255, 255]);
        let out = side_by_side(&[&a, &b], 2, 6);
        assert_eq!(out.dimensions(), (2 + 10 + 2 + 20 + 2, 2 + 6 + 8 + 2));
        assert_eq!(panel_origins(&[&a, &b], 2, 6), vec![(2, 8), (14, 8)]);
        assert_eq!(*out.get_pixel(2, 8), Rgba([255, 0, 0, 255]));
        assert_eq!(*out.get_pixel(14, 8), Rgba([0, 0, 255, 255]));
        assert_eq!(*out.get_pixel(2, 2), PANEL_BACKGROUND);
        let grid = stack_vertical(&[&out, &a], 3);
        assert_eq!(grid.dimensions(), (out.width() + 6, 3 + out.height() + 3 + 5 + 3));
    }
}
