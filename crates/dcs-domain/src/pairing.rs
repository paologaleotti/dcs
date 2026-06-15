//! Pure file pairing (§2.1, open decision #1). Groups scanned files into
//! photos by `(parent directory, case-insensitive stem)`: `DSCF1234.JPG` +
//! `DSCF1234.RAF` in the same folder become one `Both` photo.
//!
//! `PoolBuilder` assembles incrementally so progressive import keeps stable
//! ids as files stream in; `pair` is the batch convenience used in tests.

use std::collections::HashMap;
use std::path::Path;
use std::path::PathBuf;

use time::PrimitiveDateTime;

use crate::photo::{AssociatedFiles, Orientation, Photo, PhotoId, PhotoType, Pool};

/// RAW extensions recognized for pairing. Lowercase, no dot. Defaults chosen
/// to cover common camera brands; easily extended.
pub const RAW_EXTENSIONS: &[&str] = &[
    "raf", "cr2", "cr3", "nef", "nrw", "arw", "sr2", "srf", "dng", "orf", "rw2", "pef", "srw",
    "raw", "rwl", "x3f", "3fr", "erf", "kdc", "mrw", "iiq",
];

const JPEG_EXTENSIONS: &[&str] = &["jpg", "jpeg"];

/// A file discovered during a scan, classified and with its orientation read.
#[derive(Debug, Clone)]
pub struct ScannedFile {
    pub path: PathBuf,
    pub kind: FileKind,
    pub orientation: Orientation,
    pub captured_at: Option<PrimitiveDateTime>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileKind {
    Jpeg,
    Raw,
}

/// Incremental pairer with stable ids. Feeding the same file twice is a no-op
/// for identity (the existing photo absorbs it).
#[derive(Debug, Default)]
pub struct PoolBuilder {
    index: HashMap<String, usize>,
    photos: Vec<Photo>,
    next_id: u32,
}

/// Classify a path by extension. Non-image files return `None`.
pub fn classify(path: &Path) -> Option<FileKind> {
    let ext = path.extension()?.to_str()?.to_ascii_lowercase();
    if JPEG_EXTENSIONS.contains(&ext.as_str()) {
        Some(FileKind::Jpeg)
    } else if RAW_EXTENSIONS.contains(&ext.as_str()) {
        Some(FileKind::Raw)
    } else {
        None
    }
}

/// Pair a batch of scanned files into a pool. Order follows first appearance.
pub fn pair(files: impl IntoIterator<Item = ScannedFile>) -> Pool {
    let mut builder = PoolBuilder::default();
    for file in files {
        builder.add(file);
    }
    builder.to_pool()
}

impl PoolBuilder {
    /// Fold one scanned file into the pool, creating or extending a photo.
    pub fn add(&mut self, file: ScannedFile) {
        let Some(key) = pair_key(&file.path) else {
            return;
        };
        match self.index.get(&key) {
            Some(&pos) => merge_file(&mut self.photos[pos], file),
            None => {
                let id = PhotoId(self.next_id);
                self.next_id += 1;
                self.index.insert(key, self.photos.len());
                self.photos.push(new_photo(id, file));
            }
        }
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

    pub fn to_pool(&self) -> Pool {
        Pool::from_photos(self.photos.clone())
    }
}

fn pair_key(path: &Path) -> Option<String> {
    let stem = path.file_stem()?.to_str()?.to_ascii_lowercase();
    let parent = path
        .parent()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_default();
    Some(format!("{parent}\0{stem}"))
}

fn new_photo(id: PhotoId, file: ScannedFile) -> Photo {
    let (files, photo_type) = match file.kind {
        FileKind::Jpeg => (
            AssociatedFiles {
                jpeg: Some(file.path),
                raw: None,
            },
            PhotoType::Jpeg,
        ),
        FileKind::Raw => (
            AssociatedFiles {
                jpeg: None,
                raw: Some(file.path),
            },
            PhotoType::Raw,
        ),
    };
    Photo {
        id,
        files,
        photo_type,
        orientation: file.orientation,
        captured_at: file.captured_at,
    }
}

fn merge_file(photo: &mut Photo, file: ScannedFile) {
    match file.kind {
        FileKind::Jpeg => {
            if photo.files.jpeg.is_none() {
                photo.files.jpeg = Some(file.path);
                photo.orientation = file.orientation;
                photo.captured_at = file.captured_at;
            }
        }
        FileKind::Raw => {
            if photo.files.raw.is_none() {
                photo.files.raw = Some(file.path);
                if photo.files.jpeg.is_none() {
                    photo.orientation = file.orientation;
                    photo.captured_at = file.captured_at;
                }
            }
        }
    }
    photo.photo_type = derive_type(&photo.files);
}

fn derive_type(files: &AssociatedFiles) -> PhotoType {
    match (files.jpeg.is_some(), files.raw.is_some()) {
        (true, true) => PhotoType::Both,
        (true, false) => PhotoType::Jpeg,
        _ => PhotoType::Raw,
    }
}
