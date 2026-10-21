//! Fonts and text: font selection (CSS `font-family` matching against bundled and web
//! fonts), font metrics, shaping (via `rustybuzz`, a HarfBuzz port) and glyph
//! rasterization with subpixel positioning (via `ab_glyph`).
//!
//! Line breaking, whitespace processing and inline layout are hand-written in
//! [`crate::layout`]; this module only answers "which glyphs, how wide, what pixels".

use crate::util::FxHashMap;
use ab_glyph::{Font as _, FontRef, PxScale, point};
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

/// Index of a font face in the [`FontDb`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct FaceId(pub u16);

/// Generic families the bundled fonts cover.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Generic {
    /// Liberation Sans (metric-compatible with Arial/Helvetica).
    Sans,
    /// Liberation Serif (metric-compatible with Times New Roman).
    Serif,
    /// Liberation Mono (metric-compatible with Courier New).
    Mono,
}

struct Face {
    family: String,
    generic: Option<Generic>,
    weight: u16,
    italic: bool,
    shaper: rustybuzz::Face<'static>,
    raster: FontRef<'static>,
    units_per_em: f32,
    ascent: f32,
    descent: f32,
    line_gap: f32,
    underline_position: f32,
    underline_thickness: f32,
    x_height: f32,
    /// ab_glyph's PxScale is relative to ascent-descent; this converts from em size.
    em_to_px_scale: f32,
}

/// Font metrics at a particular size, in px, rounded the way Chrome rounds them.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FontMetrics {
    /// Distance from baseline to the top of the em box (positive).
    pub ascent: f32,
    /// Distance from baseline to the bottom (positive).
    pub descent: f32,
    /// Extra spacing recommended between lines.
    pub line_gap: f32,
    /// Underline offset below the baseline (positive = below).
    pub underline_offset: f32,
    /// Underline thickness.
    pub underline_thickness: f32,
    /// Height of lowercase letters.
    pub x_height: f32,
}

impl FontMetrics {
    /// Line height for `line-height: normal`.
    pub fn normal_line_height(&self) -> f32 {
        self.ascent + self.descent + self.line_gap
    }
}

/// One shaped glyph.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ShapedGlyph {
    /// Face the glyph comes from (may differ from the requested face for fallback).
    pub face: FaceId,
    /// Glyph id in that face.
    pub glyph: u16,
    /// Byte offset of the glyph's cluster in the shaped text.
    pub cluster: u32,
    /// Horizontal advance in px (includes letter/word spacing).
    pub advance: f32,
    /// Horizontal offset in px.
    pub x_offset: f32,
    /// Vertical offset in px.
    pub y_offset: f32,
}

/// A rasterized glyph: an 8-bit coverage mask positioned relative to the pen.
#[derive(Clone, Debug, Default)]
pub struct RasterGlyph {
    /// Offset of the mask's left edge from the pen's integer x position.
    pub left: i32,
    /// Offset of the mask's top edge from the baseline (negative = above).
    pub top: i32,
    /// Mask width.
    pub width: u32,
    /// Mask height.
    pub height: u32,
    /// Coverage, row-major, `width * height` bytes.
    pub coverage: Vec<u8>,
}

static SANS: &[u8] = include_bytes!("../../assets/fonts/LiberationSans-Regular.ttf");
static SANS_BOLD: &[u8] = include_bytes!("../../assets/fonts/LiberationSans-Bold.ttf");
static SANS_ITALIC: &[u8] = include_bytes!("../../assets/fonts/LiberationSans-Italic.ttf");
static SANS_BOLD_ITALIC: &[u8] = include_bytes!("../../assets/fonts/LiberationSans-BoldItalic.ttf");
static SERIF: &[u8] = include_bytes!("../../assets/fonts/LiberationSerif-Regular.ttf");
static SERIF_BOLD: &[u8] = include_bytes!("../../assets/fonts/LiberationSerif-Bold.ttf");
static SERIF_ITALIC: &[u8] = include_bytes!("../../assets/fonts/LiberationSerif-Italic.ttf");
static SERIF_BOLD_ITALIC: &[u8] = include_bytes!("../../assets/fonts/LiberationSerif-BoldItalic.ttf");
static MONO: &[u8] = include_bytes!("../../assets/fonts/LiberationMono-Regular.ttf");
static MONO_BOLD: &[u8] = include_bytes!("../../assets/fonts/LiberationMono-Bold.ttf");

type GlyphKey = (FaceId, u16, u32, u8);
type SelectKey = (Rc<[String]>, u16, bool);

/// The font database: bundled faces plus web fonts registered by `@font-face`.
pub struct FontDb {
    faces: Vec<Face>,
    glyph_cache: RefCell<FxHashMap<GlyphKey, Rc<RasterGlyph>>>,
    select_cache: RefCell<FxHashMap<SelectKey, FaceId>>,
    plan_cache: RefCell<FxHashMap<(FaceId, rustybuzz::Script), Rc<rustybuzz::ShapePlan>>>,
}

impl Default for FontDb {
    fn default() -> Self {
        Self::new()
    }
}

/// Subpixel positions per pixel used for glyph rendering.
pub const SUBPIXEL_STEPS: u8 = 4;

impl FontDb {
    /// Creates a database containing the bundled Liberation fonts.
    pub fn new() -> FontDb {
        let mut db = FontDb {
            faces: Vec::new(),
            glyph_cache: RefCell::new(FxHashMap::default()),
            select_cache: RefCell::new(FxHashMap::default()),
            plan_cache: RefCell::new(FxHashMap::default()),
        };
        let bundled: [(&'static [u8], &str, Generic, u16, bool); 10] = [
            (SANS, "Liberation Sans", Generic::Sans, 400, false),
            (SANS_BOLD, "Liberation Sans", Generic::Sans, 700, false),
            (SANS_ITALIC, "Liberation Sans", Generic::Sans, 400, true),
            (SANS_BOLD_ITALIC, "Liberation Sans", Generic::Sans, 700, true),
            (SERIF, "Liberation Serif", Generic::Serif, 400, false),
            (SERIF_BOLD, "Liberation Serif", Generic::Serif, 700, false),
            (SERIF_ITALIC, "Liberation Serif", Generic::Serif, 400, true),
            (SERIF_BOLD_ITALIC, "Liberation Serif", Generic::Serif, 700, true),
            (MONO, "Liberation Mono", Generic::Mono, 400, false),
            (MONO_BOLD, "Liberation Mono", Generic::Mono, 700, false),
        ];
        for (data, family, generic, weight, italic) in bundled {
            db.add_face(data, family.to_string(), Some(generic), weight, italic)
                .expect("bundled font is valid");
        }
        db
    }

    fn add_face(
        &mut self,
        data: &'static [u8],
        family: String,
        generic: Option<Generic>,
        weight: u16,
        italic: bool,
    ) -> Option<FaceId> {
        let shaper = rustybuzz::Face::from_slice(data, 0)?;
        let raster = FontRef::try_from_slice(data).ok()?;
        let upem = shaper.units_per_em() as f32;
        let ascent = shaper.ascender() as f32;
        let descent = -(shaper.descender() as f32);
        let line_gap = shaper.line_gap() as f32;
        let (ul_pos, ul_thick) = shaper
            .underline_metrics()
            .map(|m| (-(m.position as f32), m.thickness as f32))
            .unwrap_or((upem * 0.1, upem * 0.05));
        let x_height = shaper.x_height().map(|x| x as f32).unwrap_or(upem * 0.5);
        let height_unscaled = raster.height_unscaled();
        let id = FaceId(self.faces.len() as u16);
        self.faces.push(Face {
            family,
            generic,
            weight,
            italic,
            shaper,
            raster,
            units_per_em: upem,
            ascent,
            descent,
            line_gap,
            underline_position: ul_pos,
            underline_thickness: ul_thick,
            x_height,
            em_to_px_scale: height_unscaled / upem,
        });
        self.select_cache.borrow_mut().clear();
        Some(id)
    }

    /// Registers a web font (TTF/OTF bytes) under `family`. The bytes are leaked to give
    /// them a `'static` lifetime; callers should de-duplicate by URL.
    pub fn add_web_font(&mut self, bytes: Vec<u8>, family: &str, weight: u16, italic: bool) -> Option<FaceId> {
        // Reject formats we cannot parse (WOFF/WOFF2) before leaking.
        rustybuzz::Face::from_slice(&bytes, 0)?;
        let data = intern_font_bytes(bytes);
        self.add_face(data, family.to_string(), None, weight, italic)
    }

    /// Number of faces.
    pub fn face_count(&self) -> usize {
        self.faces.len()
    }

    /// The family name of a face.
    pub fn family_name(&self, face: FaceId) -> &str {
        &self.faces[face.0 as usize].family
    }

    /// Selects the best face for a CSS `font-family` list, weight and style.
    pub fn select(&self, families: &Rc<[String]>, weight: u16, italic: bool) -> FaceId {
        let key = (families.clone(), weight, italic);
        if let Some(&f) = self.select_cache.borrow().get(&key) {
            return f;
        }
        let mut chosen = None;
        for fam in families.iter() {
            // Web fonts / exact family names first.
            if let Some(f) = self.best_in_family(|face| face.family.eq_ignore_ascii_case(fam), weight, italic) {
                chosen = Some(f);
                break;
            }
            if let Some(g) = generic_for_family(fam) {
                chosen = self.best_in_family(
                    |face| face.generic == Some(g) && face.family.starts_with("Liberation"),
                    weight,
                    italic,
                );
                if chosen.is_some() {
                    break;
                }
            }
        }
        // Browser default font: serif.
        let f = chosen.unwrap_or_else(|| {
            self.best_in_family(|face| face.generic == Some(Generic::Serif), weight, italic)
                .expect("serif face")
        });
        self.select_cache.borrow_mut().insert(key, f);
        f
    }

    /// CSS font matching within a family: style first, then nearest weight.
    fn best_in_family(&self, pred: impl Fn(&Face) -> bool, weight: u16, italic: bool) -> Option<FaceId> {
        let candidates: Vec<(usize, &Face)> = self.faces.iter().enumerate().filter(|(_, f)| pred(f)).collect();
        if candidates.is_empty() {
            return None;
        }
        let style_match: Vec<&(usize, &Face)> = candidates.iter().filter(|(_, f)| f.italic == italic).collect();
        let pool: Vec<&(usize, &Face)> = if style_match.is_empty() {
            candidates.iter().collect()
        } else {
            style_match
        };
        let want_bold = weight >= 600;
        let best = pool
            .iter()
            .min_by_key(|(_, f)| {
                let d = (f.weight as i32 - weight as i32).abs();
                // Prefer heavier faces for bold requests and lighter for light ones.
                let wrong_side = if want_bold { f.weight < 600 } else { f.weight >= 600 };
                (wrong_side, d)
            })
            .unwrap();
        Some(FaceId(best.0 as u16))
    }

    /// Metrics for `face` at `size` px.
    pub fn metrics(&self, face: FaceId, size: f32) -> FontMetrics {
        let f = &self.faces[face.0 as usize];
        let s = size / f.units_per_em;
        FontMetrics {
            ascent: (f.ascent * s).round(),
            descent: (f.descent * s).round(),
            line_gap: (f.line_gap * s).round(),
            underline_offset: f.underline_position * s,
            underline_thickness: (f.underline_thickness * s).max(1.0),
            x_height: f.x_height * s,
        }
    }

    /// Whether `face` has a glyph for `c`.
    pub fn has_glyph(&self, face: FaceId, c: char) -> bool {
        self.faces[face.0 as usize].shaper.glyph_index(c).is_some()
    }

    /// Picks a face that can render `c`, preferring `primary`.
    fn face_for_char(&self, primary: FaceId, c: char) -> FaceId {
        // Bundled faces cover all of ASCII; web fonts are trusted to as well.
        if c.is_ascii() || c.is_whitespace() || c.is_control() || self.has_glyph(primary, c) {
            return primary;
        }
        let p = &self.faces[primary.0 as usize];
        // Same style in another bundled family, then anything.
        let mut fallback = None;
        for (i, f) in self.faces.iter().enumerate() {
            if f.shaper.glyph_index(c).is_some() {
                if f.italic == p.italic && (f.weight >= 600) == (p.weight >= 600) {
                    return FaceId(i as u16);
                }
                fallback.get_or_insert(FaceId(i as u16));
            }
        }
        fallback.unwrap_or(primary)
    }

    /// Shapes `text` with `face` at `size` px. `letter_spacing` is added after every
    /// glyph and `word_spacing` after every space. Missing glyphs fall back to other
    /// faces that contain them.
    pub fn shape(
        &self,
        face: FaceId,
        size: f32,
        text: &str,
        letter_spacing: f32,
        word_spacing: f32,
    ) -> Vec<ShapedGlyph> {
        let mut out = Vec::with_capacity(text.len());
        if text.is_empty() {
            return out;
        }
        // Split into runs by the face that supports each character.
        let mut run_start = 0;
        let mut run_face = None;
        for (i, c) in text.char_indices() {
            let f = self.face_for_char(face, c);
            match run_face {
                None => run_face = Some(f),
                Some(rf) if rf != f => {
                    self.shape_run(rf, size, &text[run_start..i], run_start as u32, &mut out);
                    run_start = i;
                    run_face = Some(f);
                }
                _ => {}
            }
        }
        if let Some(rf) = run_face {
            self.shape_run(rf, size, &text[run_start..], run_start as u32, &mut out);
        }
        if letter_spacing != 0.0 || word_spacing != 0.0 {
            for g in out.iter_mut() {
                g.advance += letter_spacing;
                if word_spacing != 0.0 && text[g.cluster as usize..].starts_with(' ') {
                    g.advance += word_spacing;
                }
            }
        }
        out
    }

    fn shape_run(&self, face: FaceId, size: f32, text: &str, base: u32, out: &mut Vec<ShapedGlyph>) {
        let f = &self.faces[face.0 as usize];
        let mut buf = rustybuzz::UnicodeBuffer::new();
        buf.push_str(text);
        buf.guess_segment_properties();
        buf.set_direction(rustybuzz::Direction::LeftToRight);
        // Compiling a shape plan is expensive; reuse one per face and script.
        let script = buf.script();
        let plan = self
            .plan_cache
            .borrow_mut()
            .entry((face, script))
            .or_insert_with(|| {
                Rc::new(rustybuzz::ShapePlan::new(
                    &f.shaper,
                    rustybuzz::Direction::LeftToRight,
                    Some(script),
                    None,
                    &[],
                ))
            })
            .clone();
        let shaped = rustybuzz::shape_with_plan(&f.shaper, &plan, buf);
        let scale = size / f.units_per_em;
        for (info, pos) in shaped.glyph_infos().iter().zip(shaped.glyph_positions()) {
            out.push(ShapedGlyph {
                face,
                glyph: info.glyph_id as u16,
                cluster: base + info.cluster,
                advance: pos.x_advance as f32 * scale,
                x_offset: pos.x_offset as f32 * scale,
                y_offset: pos.y_offset as f32 * scale,
            });
        }
    }

    /// Total advance of `text`.
    pub fn measure(&self, face: FaceId, size: f32, text: &str) -> f32 {
        self.shape(face, size, text, 0.0, 0.0).iter().map(|g| g.advance).sum()
    }

    /// Rasterizes a glyph at `size` px with the pen at a fractional x offset of
    /// `subpixel / SUBPIXEL_STEPS` px. Results are cached.
    pub fn rasterize(&self, face: FaceId, glyph: u16, size: f32, subpixel: u8) -> Rc<RasterGlyph> {
        let key = (face, glyph, size.to_bits(), subpixel);
        if let Some(g) = self.glyph_cache.borrow().get(&key) {
            return g.clone();
        }
        let f = &self.faces[face.0 as usize];
        let scale = PxScale::from(size * f.em_to_px_scale);
        let gl = ab_glyph::GlyphId(glyph)
            .with_scale_and_position(scale, point(subpixel as f32 / SUBPIXEL_STEPS as f32, 0.0));
        let raster = match f.raster.outline_glyph(gl) {
            Some(outlined) => {
                let b = outlined.px_bounds();
                let w = (b.max.x - b.min.x).max(0.0) as u32;
                let h = (b.max.y - b.min.y).max(0.0) as u32;
                let mut coverage = vec![0u8; (w * h) as usize];
                outlined.draw(|x, y, c| {
                    if x < w && y < h {
                        coverage[(y * w + x) as usize] = (c.clamp(0.0, 1.0) * 255.0 + 0.5) as u8;
                    }
                });
                RasterGlyph {
                    left: b.min.x as i32,
                    top: b.min.y as i32,
                    width: w,
                    height: h,
                    coverage,
                }
            }
            None => RasterGlyph::default(),
        };
        let r = Rc::new(raster);
        let mut cache = self.glyph_cache.borrow_mut();
        if cache.len() > 50_000 {
            cache.clear();
        }
        cache.insert(key, r.clone());
        r
    }
}

/// Leaks font bytes to obtain a `'static` slice, de-duplicating identical files so that
/// reloading pages does not grow memory.
fn intern_font_bytes(bytes: Vec<u8>) -> &'static [u8] {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    use std::sync::{Mutex, OnceLock};
    static FONTS: OnceLock<Mutex<HashMap<u64, &'static [u8]>>> = OnceLock::new();
    let mut h = DefaultHasher::new();
    bytes.hash(&mut h);
    let key = h.finish();
    let mut map = FONTS.get_or_init(|| Mutex::new(HashMap::new())).lock().unwrap();
    if let Some(&d) = map.get(&key)
        && d == bytes.as_slice()
    {
        return d;
    }
    let d: &'static [u8] = Box::leak(bytes.into_boxed_slice());
    map.insert(key, d);
    d
}

/// Maps a CSS family name (or generic keyword) to a bundled generic family.
pub fn generic_for_family(name: &str) -> Option<Generic> {
    let n = name.trim().to_ascii_lowercase();
    let n = n.trim_matches(|c| c == '"' || c == '\'');
    Some(match n {
        "sans-serif"
        | "system-ui"
        | "ui-sans-serif"
        | "-apple-system"
        | "blinkmacsystemfont"
        | "arial"
        | "helvetica"
        | "helvetica neue"
        | "liberation sans"
        | "arimo"
        | "segoe ui"
        | "roboto"
        | "verdana"
        | "tahoma"
        | "trebuchet ms"
        | "noto sans"
        | "open sans"
        | "lato"
        | "inter"
        | "dejavu sans"
        | "ubuntu"
        | "cantarell"
        | "fira sans"
        | "lucida grande"
        | "lucida sans unicode"
        | "geneva"
        | "source sans pro"
        | "montserrat"
        | "nimbus sans"
        | "freesans"
        | "sf pro text"
        | "sf pro display"
        | "avenir"
        | "gill sans"
        | "calibri"
        | "franklin gothic medium"
        | "oxygen"
        | "pt sans"
        | "ui-rounded" => Generic::Sans,
        "serif" | "ui-serif" | "times" | "times new roman" | "liberation serif" | "tinos" | "georgia" | "cambria"
        | "palatino" | "palatino linotype" | "book antiqua" | "garamond" | "baskerville" | "linux libertine"
        | "noto serif" | "dejavu serif" | "charter" | "iowan old style" | "hoefler text" | "freeserif"
        | "nimbus roman" | "merriweather" | "pt serif" | "source serif pro" | "math" => Generic::Serif,
        "monospace"
        | "ui-monospace"
        | "courier"
        | "courier new"
        | "liberation mono"
        | "cousine"
        | "menlo"
        | "monaco"
        | "consolas"
        | "dejavu sans mono"
        | "sfmono-regular"
        | "sf mono"
        | "lucida console"
        | "source code pro"
        | "fira code"
        | "fira mono"
        | "roboto mono"
        | "ubuntu mono"
        | "jetbrains mono"
        | "andale mono"
        | "noto sans mono"
        | "freemono"
        | "liberation mono regular" => Generic::Mono,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fam(names: &[&str]) -> Rc<[String]> {
        Rc::from(names.iter().map(|s| s.to_string()).collect::<Vec<_>>())
    }

    #[test]
    fn family_matching() {
        let db = FontDb::new();
        let sans = db.select(&fam(&["Helvetica Neue", "Arial", "sans-serif"]), 400, false);
        assert_eq!(db.family_name(sans), "Liberation Sans");
        let bold_serif = db.select(&fam(&["Unknown Font", "Georgia"]), 700, false);
        assert_eq!(db.family_name(bold_serif), "Liberation Serif");
        assert_eq!(db.faces[bold_serif.0 as usize].weight, 700);
        let mono = db.select(&fam(&["monospace"]), 400, true);
        assert_eq!(db.family_name(mono), "Liberation Mono");
        let default = db.select(&fam(&["NoSuchFont"]), 400, false);
        assert_eq!(
            db.family_name(default),
            "Liberation Serif",
            "default font is serif like Chrome"
        );
    }

    #[test]
    fn metrics_match_chrome_rounding() {
        let db = FontDb::new();
        let sans = db.select(&fam(&["sans-serif"]), 400, false);
        let m = db.metrics(sans, 16.0);
        assert_eq!((m.ascent, m.descent, m.line_gap), (14.0, 3.0, 1.0));
        assert_eq!(m.normal_line_height(), 18.0);
    }

    #[test]
    fn shaping_advances_and_kerning() {
        let db = FontDb::new();
        let mono = db.select(&fam(&["monospace"]), 400, false);
        // Liberation Mono advance is 1229/2048 em.
        let w = db.measure(mono, 20.0, "abcd");
        assert!((w - 4.0 * 1229.0 / 2048.0 * 20.0).abs() < 0.01, "{w}");
        let sans = db.select(&fam(&["sans-serif"]), 400, false);
        let av = db.measure(sans, 100.0, "AV");
        let a_v = db.measure(sans, 100.0, "A") + db.measure(sans, 100.0, "V");
        assert!(av < a_v, "kerning should tighten AV ({av} vs {a_v})");
        let glyphs = db.shape(sans, 16.0, "a b", 1.0, 2.0);
        assert_eq!(glyphs.len(), 3);
        let plain = db.shape(sans, 16.0, "a b", 0.0, 0.0);
        assert!((glyphs[1].advance - plain[1].advance - 3.0).abs() < 1e-4);
    }

    #[test]
    fn rasterizes_glyphs() {
        let db = FontDb::new();
        let sans = db.select(&fam(&["sans-serif"]), 400, false);
        let g = db.shape(sans, 32.0, "H", 0.0, 0.0)[0];
        let r = db.rasterize(sans, g.glyph, 32.0, 0);
        assert!(r.width > 10 && r.height > 15);
        assert!(r.top < 0, "glyph sits above the baseline");
        assert!(r.coverage.contains(&255));
    }
}
