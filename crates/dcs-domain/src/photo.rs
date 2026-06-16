//! Core photo identity and types. A photo is the logical unit (§2.1): a
//! JPEG, a RAW, or both paired under one id. Cull/tag photos; export files.

use std::path::{Path, PathBuf};

use time::PrimitiveDateTime;

use crate::fingerprint::ContentFingerprint;

/// Stable per-photo identifier. Assigned on import, never reused. Serializable
/// because commands carrying `PhotoId`s are persisted to `undo.log` (§5).
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, serde::Serialize, serde::Deserialize,
)]
pub struct PhotoId(pub u32);

/// Which files back a photo (§2.1). Display prefers the JPEG.
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

/// One logical photo. Identity, files, and the facts needed to display it.
/// Owned cull/tag state arrives with later slices.
#[derive(Debug, Clone)]
pub struct Photo {
    pub id: PhotoId,
    pub files: AssociatedFiles,
    pub photo_type: PhotoType,
    pub orientation: Orientation,
    /// Content identity (§10b, #33): the fingerprint of the *display* file
    /// (JPEG when present, else RAW). Keyed from import so a rename-in-place
    /// reclaims this photo's id, verdicts, and tags instead of returning blank.
    pub fingerprint: ContentFingerprint,
    /// Raw EXIF `DateTimeOriginal`, naive (no zone). Timezone adjustment to an
    /// `OffsetDateTime` is derived later (§2.4); `None` means undated.
    pub captured_at: Option<PrimitiveDateTime>,
    /// The backing file is known (from `project.json`) but absent on disk (§4).
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
    /// return `None` (no raw decode in v1, §2.1).
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
    /// Build a placeholder for a known photo whose file is absent on disk (§4).
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
