//! Decoded thumbnail pixels — a plain data type shared across layers so
//! `dcs-io` produces it, `dcs-app` caches it, and `dcs-ui` uploads it without
//! any infrastructure or egui type crossing a boundary.

/// Contain-fit RGBA8 thumbnail. `rgba.len() == width * height * 4`.
#[derive(Debug, Clone)]
pub struct ThumbImage {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}
