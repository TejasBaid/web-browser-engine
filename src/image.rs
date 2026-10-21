//! Decoded raster images (PNG, JPEG, first frame of GIF), stored premultiplied for
//! compositing.

use std::rc::Rc;

/// A decoded image in premultiplied RGBA8.
#[derive(Clone, Debug, PartialEq)]
pub struct Image {
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
    /// Premultiplied RGBA, row-major.
    pub pixels: Vec<u8>,
}

impl Image {
    /// Builds an image from straight (non-premultiplied) RGBA pixels.
    pub fn from_rgba(width: u32, height: u32, mut rgba: Vec<u8>) -> Image {
        for px in rgba.as_chunks_mut::<4>().0 {
            let a = px[3] as u32;
            if a != 255 {
                px[0] = ((px[0] as u32 * a + 127) / 255) as u8;
                px[1] = ((px[1] as u32 * a + 127) / 255) as u8;
                px[2] = ((px[2] as u32 * a + 127) / 255) as u8;
            }
        }
        Image {
            width,
            height,
            pixels: rgba,
        }
    }

    /// Decodes PNG/JPEG/GIF bytes (format sniffed from the content).
    pub fn decode(bytes: &[u8]) -> Option<Rc<Image>> {
        let img = image::load_from_memory(bytes).ok()?;
        let rgba = img.to_rgba8();
        let (w, h) = rgba.dimensions();
        if w == 0 || h == 0 {
            return None;
        }
        Some(Rc::new(Image::from_rgba(w, h, rgba.into_raw())))
    }

    /// Bilinearly samples the image at (`u`, `v`) in source pixel coordinates
    /// (pixel centres at +0.5), clamping at the edges. Returns premultiplied RGBA.
    #[inline]
    pub fn sample_bilinear(&self, u: f32, v: f32) -> [f32; 4] {
        let x = (u - 0.5).clamp(0.0, (self.width - 1) as f32);
        let y = (v - 0.5).clamp(0.0, (self.height - 1) as f32);
        let x0 = x.floor() as u32;
        let y0 = y.floor() as u32;
        let x1 = (x0 + 1).min(self.width - 1);
        let y1 = (y0 + 1).min(self.height - 1);
        let fx = x - x0 as f32;
        let fy = y - y0 as f32;
        let px = |xx: u32, yy: u32| {
            let i = ((yy * self.width + xx) * 4) as usize;
            [
                self.pixels[i] as f32,
                self.pixels[i + 1] as f32,
                self.pixels[i + 2] as f32,
                self.pixels[i + 3] as f32,
            ]
        };
        let (a, b, c, d) = (px(x0, y0), px(x1, y0), px(x0, y1), px(x1, y1));
        let mut out = [0.0; 4];
        for k in 0..4 {
            let top = a[k] + (b[k] - a[k]) * fx;
            let bot = c[k] + (d[k] - c[k]) * fx;
            out[k] = top + (bot - top) * fy;
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn premultiplies_and_samples() {
        let img = Image::from_rgba(2, 1, vec![255, 0, 0, 255, 0, 0, 255, 128]);
        assert_eq!(&img.pixels[4..8], &[0, 0, 128, 128]);
        let mid = img.sample_bilinear(1.0, 0.5);
        assert!((mid[0] - 127.5).abs() < 0.01);
        assert_eq!(img.sample_bilinear(0.0, 0.0)[0], 255.0);
    }

    #[test]
    fn decodes_png() {
        let mut buf = Vec::new();
        let src = image::RgbaImage::from_pixel(3, 2, image::Rgba([10, 20, 30, 255]));
        src.write_to(&mut std::io::Cursor::new(&mut buf), image::ImageFormat::Png)
            .unwrap();
        let img = Image::decode(&buf).unwrap();
        assert_eq!((img.width, img.height), (3, 2));
        assert_eq!(&img.pixels[0..4], &[10, 20, 30, 255]);
        assert!(Image::decode(b"not an image").is_none());
    }
}
