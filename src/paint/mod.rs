//! Painting: display list construction and software rasterization.

pub mod display_list;
pub mod raster;

pub use display_list::{DisplayItem, DisplayList, HitRegion, build_display_list};
pub use raster::{Canvas, rasterize};
