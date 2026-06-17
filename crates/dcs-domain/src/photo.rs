//! Core photo identity and types. A photo is the logical unit: a
//! JPEG, a RAW, or both paired under one id. Cull/tag photos; export files.

use std::path::{Path, PathBuf};

use time::{PrimitiveDateTime, UtcOffset};

use crate::fingerprint::ContentFingerprint;

/// Stable per-photo identifier. Assigned on import, never reused. Serializable
/// because commands carrying `PhotoId`s are persisted to `undo.log`.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, serde::Serialize, serde::Deserialize,
)]
pub struct PhotoId(pub u32);

/// Which files back a photo. Display prefers the JPEG.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PhotoType {
    Jpeg,
    Raw,
    Both,
}

/// EXIF orientation, normalized. Applied to pixels in `dcs-io`; the domain
/// only carries the value.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Orientation {
    #[default]
    Normal,
    FlipH,
    Rotate180,
    FlipV,
    Transpose,
    Rotate90,
    Transverse,
    Rotate270,
}

/// The file paths backing one photo. At least one is always present.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AssociatedFiles {
    pub jpeg: Option<PathBuf>,
    pub raw: Option<PathBuf>,
}

/// Camera capture facts read from EXIF, for the gallery metadata caption.
/// Every field is optional — cameras and formats vary, and a missing tag never
/// fails import. Derived, never persisted: re-read from the file on each scan,
/// exactly like `orientation` and `captured_at`.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct CaptureMeta {
    /// Camera make + model, already joined (e.g. `FUJIFILM X-T5`).
    pub camera: Option<String>,
    /// Lens model (e.g. `XF16-55mmF2.8 R LM WR`).
    pub lens: Option<String>,
    /// Focal length in millimetres.
    pub focal_mm: Option<f32>,
    /// Aperture as an f-number (e.g. `2.8`).
    pub aperture: Option<f32>,
    /// Exposure time in seconds (e.g. `0.004` for 1/250).
    pub exposure_secs: Option<f32>,
    /// ISO sensitivity.
    pub iso: Option<u32>,
}

impl CaptureMeta {
    /// `f/2.8`, or `None` when the aperture is unknown.
    pub fn aperture_label(&self) -> Option<String> {
        self.aperture.map(format_aperture)
    }

    /// `1/250`, `0.5s`, or `2"` — the photographer's reading of the exposure.
    pub fn shutter_label(&self) -> Option<String> {
        self.exposure_secs.map(format_shutter)
    }

    /// `35mm`, or `None` when the focal length is unknown.
    pub fn focal_label(&self) -> Option<String> {
        self.focal_mm.map(format_focal)
    }

    /// `ISO 400`, or `None` when the ISO is unknown.
    pub fn iso_label(&self) -> Option<String> {
        self.iso.map(|iso| format!("ISO {iso}"))
    }

    /// The exposure triplet as one compact line — `35mm · f/2.8 · 1/250 · ISO
    /// 400` — omitting any field that's missing. `None` when nothing is known.
    pub fn exposure_line(&self) -> Option<String> {
        let parts: Vec<String> = [
            self.focal_label(),
            self.aperture_label(),
            self.shutter_label(),
            self.iso_label(),
        ]
        .into_iter()
        .flatten()
        .collect();
        (!parts.is_empty()).then(|| parts.join(" · "))
    }
}

/// Format an f-number: drop a trailing `.0` so `2.8` stays `f/2.8` but `8.0`
/// reads `f/8`.
pub fn format_aperture(f: f32) -> String {
    if (f.fract()).abs() < 0.05 {
        format!("f/{}", f.round() as i32)
    } else {
        format!("f/{f:.1}")
    }
}

/// Format an exposure time the way a photographer reads it: sub-second exposures
/// as `1/N` (rounded to the nearest whole denominator), one second or longer as
/// `0.5s` / `2"`.
pub fn format_shutter(secs: f32) -> String {
    if secs <= 0.0 {
        return "0s".to_string();
    }
    if secs < 1.0 {
        let denom = (1.0 / secs).round() as i32;
        format!("1/{denom}")
    } else if secs.fract().abs() < 0.05 {
        format!("{}\"", secs.round() as i32)
    } else {
        format!("{secs:.1}s")
    }
}

/// Format a focal length: `35mm`, dropping a trailing `.0`.
pub fn format_focal(mm: f32) -> String {
    if mm.fract().abs() < 0.05 {
        format!("{}mm", mm.round() as i32)
    } else {
        format!("{mm:.1}mm")
    }
}

/// One logical photo. Identity, files, and the facts needed to display it.
/// Owned cull/tag state arrives with later slices.
#[derive(Debug, Clone)]
pub struct Photo {
    pub id: PhotoId,
    pub files: AssociatedFiles,
    pub photo_type: PhotoType,
    pub orientation: Orientation,
    /// Content identity: the fingerprint of the *display* file
    /// (JPEG when present, else RAW). Keyed from import so a rename-in-place
    /// reclaims this photo's id, verdicts, and tags instead of returning blank.
    pub fingerprint: ContentFingerprint,
    /// Raw EXIF `DateTimeOriginal`, naive (no zone). Timezone adjustment to an
    /// `OffsetDateTime` is derived later; `None` means undated.
    pub captured_at: Option<PrimitiveDateTime>,
    /// EXIF `OffsetTimeOriginal`, the camera's UTC offset at capture when the tag
    /// is present. Lets the absolute instant be derived per-photo without guessing
    /// a camera zone; `None` falls back to the project camera zone.
    pub captured_offset: Option<UtcOffset>,
    /// EXIF capture facts for the gallery caption. Derived, not persisted —
    /// empty for missing photos until the file returns.
    pub meta: CaptureMeta,
    /// The backing file is known (from `project.json`) but absent on disk.
    /// State is preserved and the cell renders as a placeholder; it reanimates
    /// when the file returns, matched by fingerprint.
    pub missing: bool,
}

/// The full set of imported photos, in scan order.
#[derive(Debug, Clone, Default)]
pub struct Pool {
    photos: Vec<Photo>,
}

impl Orientation {
    /// Map a raw EXIF orientation tag (1–8) to a normalized variant.
    /// Out-of-range values fall back to `Normal`.
    pub fn from_exif(tag: u16) -> Self {
        match tag {
            2 => Orientation::FlipH,
            3 => Orientation::Rotate180,
            4 => Orientation::FlipV,
            5 => Orientation::Transpose,
            6 => Orientation::Rotate90,
            7 => Orientation::Transverse,
            8 => Orientation::Rotate270,
            _ => Orientation::Normal,
        }
    }
}

impl Photo {
    /// The path to display: the JPEG when present, else the RAW.
    pub fn display_path(&self) -> &Path {
        self.files
            .jpeg
            .as_deref()
            .or(self.files.raw.as_deref())
            .expect("a photo always has at least one file")
    }

    /// The JPEG path to decode for a thumbnail, if one exists. RAW-only photos
    /// return `None` (no raw decode in v1).
    pub fn decodable_path(&self) -> Option<&Path> {
        self.files.jpeg.as_deref()
    }

    /// Basename of the displayed file.
    pub fn file_name(&self) -> String {
        self.display_path()
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default()
    }

    pub fn is_raw_only(&self) -> bool {
        matches!(self.photo_type, PhotoType::Raw)
    }
}

impl Photo {
    /// Build a placeholder for a known photo whose file is absent on disk.
    /// Carries its last-known paths and identity; `captured_at` is unknown until
    /// the file returns, so it sorts undated.
    pub fn missing(
        id: PhotoId,
        fingerprint: ContentFingerprint,
        files: AssociatedFiles,
        photo_type: PhotoType,
    ) -> Self {
        Photo {
            id,
            files,
            photo_type,
            orientation: Orientation::Normal,
            fingerprint,
            captured_at: None,
            captured_offset: None,
            meta: CaptureMeta::default(),
            missing: true,
        }
    }
}

impl Pool {
    pub fn from_photos(photos: Vec<Photo>) -> Self {
        Pool { photos }
    }

    pub fn photos(&self) -> &[Photo] {
        &self.photos
    }

    pub fn len(&self) -> usize {
        self.photos.len()
    }

    pub fn is_empty(&self) -> bool {
        self.photos.is_empty()
    }
}
