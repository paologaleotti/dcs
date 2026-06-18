//! Tag types — the only persisted user-created structure. A tag is
//! `{id, name, color}`, many-to-many with photos. Accept/reject is a verdict,
//! not a tag (see `cull`). The owned store (defs + assignments) and the undo
//! policy live in `dcs-app`; the domain defines the types and the built-in
//! color palette.
//!
//! Color is meaning, never decoration: every color on the sheet is a tag color,
//! auto-assigned from [`palette_color`]. The domain only carries the value; the
//! UI maps it to a swatch.

use serde::{Deserialize, Serialize};

/// Stable per-tag identifier. Assigned on create, never reused — so an
/// assignment or a persisted reference survives a rename or a reopen.
/// Serializable because tag mutations are persisted to `undo.log`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct TagId(pub u32);

/// An sRGB tag color. The one color system on the sheet; stored as plain bytes
/// so `project.json` stays human-readable and diffable.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Color {
    pub r: u8,
    pub g: u8,
    pub b: u8,
}

/// A tag: identity, user-facing name, and its one color. Many-to-many with
/// photos; all user-created. Empty tags never render (a display rule the UI
/// enforces), but the definition persists until explicitly deleted or merged.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Tag {
    pub id: TagId,
    pub name: String,
    pub color: Color,
}

impl Color {
    pub const fn rgb(r: u8, g: u8, b: u8) -> Self {
        Color { r, g, b }
    }
}

/// Curated tag colors for the first handful of tags — ordered so adjacent tags
/// read as clearly different hues (no two near-yellows in a row). Beyond this
/// set, [`palette_color`] generates further distinct hues, so tags are
/// effectively unlimited in color as well as in count.
pub const PALETTE: [Color; 8] = [
    Color::rgb(0xE5, 0x4B, 0x4B), // red
    Color::rgb(0x4B, 0x8F, 0xE5), // blue
    Color::rgb(0x5C, 0xB8, 0x4B), // green
    Color::rgb(0xE5, 0x8A, 0x33), // orange
    Color::rgb(0xB5, 0x4B, 0xE5), // violet
    Color::rgb(0x3F, 0xB8, 0xAF), // teal
    Color::rgb(0xE5, 0x4B, 0x9C), // magenta
    Color::rgb(0xE5, 0xC0, 0x3A), // yellow
];

/// The color for the `slot`-th tag (one-based): the curated [`PALETTE`] for the
/// first few, then a golden-angle hue rotation so any number of further tags
/// keep getting distinct, well-spread colors that never tightly repeat.
pub fn palette_color(slot: usize) -> Color {
    let i = slot.saturating_sub(1);
    match PALETTE.get(i) {
        Some(c) => *c,
        None => generated_color(i),
    }
}

/// A distinct color for tag index `i` past the curated set: the golden angle
/// (≈137.5°) spreads successive hues maximally around the wheel, at a fixed
/// saturation/value tuned to read against the near-black sheet. A phase offset
/// keeps the first generated hue clear of the curated reds/oranges.
fn generated_color(i: usize) -> Color {
    let hue = (i as f32 * 137.508 + 60.0) % 360.0;
    hsv_to_rgb(hue, 0.62, 0.90)
}

/// HSV (`h` in degrees, `s`/`v` in `0..=1`) to an sRGB byte triple.
fn hsv_to_rgb(h: f32, s: f32, v: f32) -> Color {
    let c = v * s;
    let x = c * (1.0 - ((h / 60.0) % 2.0 - 1.0).abs());
    let m = v - c;
    let (r, g, b) = match h as u32 / 60 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    let byte = |f: f32| ((f + m) * 255.0).round().clamp(0.0, 255.0) as u8;
    Color::rgb(byte(r), byte(g), byte(b))
}

/// Normalize a tag name for identity comparison: trimmed, case-folded. Two names
/// that normalize equal are "the same tag" for the merge-via-rename rule
/// (renaming a tag onto an existing name merges them).
pub fn normalize_name(name: &str) -> String {
    name.trim().to_lowercase()
}
