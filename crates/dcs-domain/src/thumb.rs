//! Decoded thumbnail pixels — a plain data type shared across layers so
//! `dcs-io` produces it, `dcs-app` caches it, and `dcs-ui` uploads it without
//! any infrastructure or egui type crossing a boundary.

/// Contain-fit RGBA8 thumbnail. `rgba.len() == width * height * 4`.
///
/// `source_width`/`source_height` are the full-resolution pixel dimensions of the
/// decoded region (after orientation and crop, before the fit-downscale). They let
/// a viewer compute a true 1:1 zoom ceiling independent of which tier is currently
/// decoded; a decode with no known source falls back to the thumbnail's own dims.
#[derive(Debug, Clone)]
pub struct ThumbImage {
    pub width: u32,
    pub height: u32,
    pub source_width: u32,
    pub source_height: u32,
    pub rgba: Vec<u8>,
}
