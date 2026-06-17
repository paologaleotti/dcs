//! Pure file pairing (§2.1, open decision #1). Groups scanned files into
//! photos by `(parent directory, case-insensitive stem)`: `DSCF1234.JPG` +
//! `DSCF1234.RAF` in the same folder become one `Both` photo.
//!
//! `PoolBuilder` assembles incrementally so progressive import keeps stable
//! ids as files stream in; `pair` is the batch convenience used in tests.

use std::collections::HashMap;
use std::path::Path;
use std::path::PathBuf;

use time::{PrimitiveDateTime, UtcOffset};

use crate::fingerprint::ContentFingerprint;
use crate::photo::{AssociatedFiles, CaptureMeta, Orientation, Photo, PhotoId, PhotoType, Pool};

/// RAW extensions recognized for pairing. Lowercase, no dot. Defaults chosen
/// to cover common camera brands; easily extended.
pub const RAW_EXTENSIONS: &[&str] = &[
    "raf", "cr2", "cr3", "nef", "nrw", "arw", "sr2", "srf", "dng", "orf", "rw2", "pef", "srw",
    "raw", "rwl", "x3f", "3fr", "erf", "kdc", "mrw", "iiq",
];

const JPEG_EXTENSIONS: &[&str] = &["jpg", "jpeg"];

/// A file discovered during a scan, classified, with its orientation read and
/// its content fingerprint computed (§10b, #33).
#[derive(Debug, Clone)]
pub struct ScannedFile {
    pub path: PathBuf,
    pub kind: FileKind,
    pub orientation: Orientation,
    pub fingerprint: ContentFingerprint,
    pub captured_at: Option<PrimitiveDateTime>,
    pub captured_offset: Option<UtcOffset>,
    pub meta: CaptureMeta,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileKind {
    Jpeg,
    Raw,
}

/// Incremental pairer with stable ids. Feeding the same file twice is a no-op
/// for identity (the existing photo absorbs it).
///
/// **Seeding (§10b, #33):** when reopened on a folder with saved state, the app
/// seeds the builder with the persisted `fingerprint → PhotoId` map and the
/// saved `next_id`. A file whose display fingerprint is known reclaims its old
/// id (so verdicts survive a rename-in-place); a genuinely new fingerprint gets
/// a fresh id. The map is consumed on reclaim, so duplicate content (two files,
/// one fingerprint) doesn't hand the same id to two photos.
#[derive(Debug, Default)]
pub struct PoolBuilder {
    index: HashMap<String, usize>,
    photos: Vec<Photo>,
    next_id: u32,
    seed: HashMap<ContentFingerprint, PhotoId>,
    revision: u64,
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
    /// A builder seeded from saved state. `known` reclaims ids by fingerprint;
    /// `next_id` is the persisted monotonic counter (max assigned id + 1) so
    /// fresh photos never collide with reclaimed ones (§10b).
    pub fn seeded(known: HashMap<ContentFingerprint, PhotoId>, next_id: u32) -> Self {
        PoolBuilder {
            index: HashMap::new(),
            photos: Vec::new(),
            next_id,
            seed: known,
            revision: 0,
        }
    }

    /// Fold one scanned file into the pool, creating or extending a photo.
    /// Bumps the revision so callers re-derive grouping even when a merge (a RAW
    /// joining its JPEG) leaves the photo count unchanged but alters a photo.
    pub fn add(&mut self, file: ScannedFile) {
        let Some(key) = pair_key(&file.path) else {
            return;
        };
        match self.index.get(&key) {
            Some(&pos) => {
                if merge_file(&mut self.photos[pos], file) {
                    self.reclaim_display_id(pos);
                }
            }
            None => {
                let id = self.assign_id(&file.fingerprint);
                self.index.insert(key, self.photos.len());
                self.photos.push(new_photo(id, file));
            }
        }
        self.revision += 1;
    }

    /// After a JPEG joins a RAW-first photo it becomes the display file, so the
    /// photo's identity is now the JPEG's fingerprint. Reclaim the persisted id
    /// seeded under that fingerprint (consuming the entry) so a pair whose RAW
    /// was scanned first recovers the JPEG's saved id and verdicts instead of
    /// keeping the tentative fresh id — and so the entry can't later be mistaken
    /// for a missing file. No-op when the fingerprint wasn't seeded.
    fn reclaim_display_id(&mut self, pos: usize) {
        let fingerprint = self.photos[pos].fingerprint;
        if let Some(id) = self.seed.remove(&fingerprint) {
            self.photos[pos].id = id;
        }
    }

    /// A monotonic counter bumped on every pool mutation. Lets a consumer detect
    /// merges that don't change the photo count.
    pub fn revision(&self) -> u64 {
        self.revision
    }

    /// The id for a brand-new photo: reclaim by fingerprint if seeded, else the
    /// next fresh counter. Reclaiming consumes the seed entry and never advances
    /// the counter, so re-scanning a saved folder reproduces the same ids.
    fn assign_id(&mut self, fingerprint: &ContentFingerprint) -> PhotoId {
        if let Some(id) = self.seed.remove(fingerprint) {
            return id;
        }
        let id = PhotoId(self.next_id);
        self.next_id += 1;
        id
    }

    /// The next id the builder would assign — the counter to persist (§10b).
    pub fn next_id(&self) -> u32 {
        self.next_id
    }

    /// Add a placeholder for a persisted photo whose file wasn't found in the
    /// scan (§4). The id comes from the seed (its persisted id) and the seed
    /// entry is consumed, so this is idempotent and a returned file — scanned
    /// normally — reclaims the same id instead. Returns `false` when the
    /// fingerprint was already seen this scan (the file is present, not
    /// missing) or carries no path.
    pub fn add_missing(
        &mut self,
        fingerprint: ContentFingerprint,
        jpeg: Option<PathBuf>,
        raw: Option<PathBuf>,
    ) -> bool {
        if jpeg.is_none() && raw.is_none() {
            return false;
        }
        let Some(id) = self.seed.remove(&fingerprint) else {
            return false; // file present (seed consumed by `add`) — not missing
        };
        let files = AssociatedFiles { jpeg, raw };
        let photo_type = derive_type(&files);
        self.photos
            .push(Photo::missing(id, fingerprint, files, photo_type));
        self.revision += 1;
        true
    }

    /// Drop the given photos from the pool and rebuild the pairing index.
    /// Used to forget missing files the user no longer wants tracked (§4). Ids
    /// are never reused, so `next_id` is left untouched.
    pub fn forget(&mut self, ids: &std::collections::HashSet<PhotoId>) {
        if ids.is_empty() {
            return;
        }
        self.photos.retain(|p| !ids.contains(&p.id));
        self.index.clear();
        for (pos, photo) in self.photos.iter().enumerate() {
            if let Some(key) = pair_key(photo.display_path()) {
                self.index.insert(key, pos);
            }
        }
        self.revision += 1;
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
        // The lone file is the display file, so it owns the photo's identity.
        fingerprint: file.fingerprint,
        captured_at: file.captured_at,
        captured_offset: file.captured_offset,
        meta: file.meta,
        missing: false,
    }
}

/// Fold a second file into an existing photo. Returns `true` when the joining
/// file became the new display file (so its fingerprint is now the photo's
/// identity) — the caller re-reclaims the id in that case.
fn merge_file(photo: &mut Photo, file: ScannedFile) -> bool {
    let mut display_changed = false;
    match file.kind {
        FileKind::Jpeg => {
            if photo.files.jpeg.is_none() {
                // A JPEG joining a RAW-only photo becomes the display file, so
                // identity moves to it (a photo's fingerprint is its display
                // file's). Common now that RAW-first arrival happens during the
                // parallel scan.
                photo.files.jpeg = Some(file.path);
                photo.orientation = file.orientation;
                photo.fingerprint = file.fingerprint;
                photo.captured_at = file.captured_at;
                photo.captured_offset = file.captured_offset;
                photo.meta = file.meta;
                display_changed = true;
            }
        }
        FileKind::Raw => {
            if photo.files.raw.is_none() {
                photo.files.raw = Some(file.path);
            }
        }
    }
    photo.photo_type = derive_type(&photo.files);
    display_changed
}

fn derive_type(files: &AssociatedFiles) -> PhotoType {
    match (files.jpeg.is_some(), files.raw.is_some()) {
        (true, true) => PhotoType::Both,
        (true, false) => PhotoType::Jpeg,
        _ => PhotoType::Raw,
    }
}
