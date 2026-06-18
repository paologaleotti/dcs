//! Tag types — the only persisted user-created structure. A tag is
//! `{id, name, color}`, many-to-many with photos. Accept/reject is a verdict,
//! not a tag (see `cull`). The owned store (defs + assignments) and the undo
//! policy live in `dcs-app`; the domain defines the types and the built-in
//! color palette.
//!
//! Color is meaning, never decoration: every color on the sheet is a tag color,
//! and the `1–9` keys are built-in tags drawn from [`PALETTE`]. The domain only
//! carries the value; the UI maps it to a swatch.

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

/// The nine built-in tag colors bound to `1–9`. Distinct hues at a consistent
/// mid lightness so each reads clearly against the near-black sheet and apart
/// from its neighbours. Provisional values; the UI maps an
/// index here, so changing a hue never touches stored assignments.
pub const PALETTE: [Color; 9] = [
    Color::rgb(0xE5, 0x4B, 0x4B), // red
    Color::rgb(0xE5, 0x8A, 0x33), // orange
    Color::rgb(0xE5, 0xC0, 0x3A), // yellow
    Color::rgb(0x5C, 0xB8, 0x4B), // green
    Color::rgb(0x3F, 0xB8, 0xAF), // teal
    Color::rgb(0x4B, 0x8F, 0xE5), // blue
    Color::rgb(0x7B, 0x5C, 0xE5), // indigo
    Color::rgb(0xB5, 0x4B, 0xE5), // violet
    Color::rgb(0xE5, 0x4B, 0x9C), // magenta
];

/// The built-in color for a one-based slot `1–9`, wrapping past nine so an
/// auto-assigned cycle never runs out. `slot` is the key digit, not an index.
pub fn palette_color(slot: usize) -> Color {
    let n = PALETTE.len();
    // slot 1..=9 → index 0..=8; 0 and >9 wrap rather than panic.
    let idx = slot.wrapping_sub(1) % n;
    PALETTE[idx]
}

/// Normalize a tag name for identity comparison: trimmed, case-folded. Two names
/// that normalize equal are "the same tag" for the merge-via-rename rule
/// (renaming a tag onto an existing name merges them).
pub fn normalize_name(name: &str) -> String {
    name.trim().to_lowercase()
}
