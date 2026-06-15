//! Session — the conductor for the open-and-view slice. Owns the pool being
//! assembled, the decode pipeline, and the thumbnail caches. The UI talks only
//! to this; it never reaches into `dcs-io`. (§9)
//!
//! Two-tier thumbnails, matching how fast cullers stay fast:
//!   - **Base** — a cheap 256 px decode for every photo, used for display;
//!     this is what makes a folder load and scroll fast at normal zoom.
//!   - **Hi-res** — a sharp decode sized to the cell, requested only for cells
//!     in the viewport and only once zoomed in. Held in a small cache that is
//!     dropped on zoom-out, so RAM stays low.
//!
//! Each frame the UI calls `tick`, requests base thumbnails for the band it is
//! about to paint (plus hi-res for the visible cells when zoomed), and reads
//! back the best resident thumbnail per cell.

use std::collections::HashSet;
use std::path::PathBuf;

use dcs_domain::pairing::PoolBuilder;
use dcs_domain::photo::{Photo, PhotoId};
use dcs_domain::sort;
use dcs_domain::thumb::ThumbImage;
use dcs_io::imaging::{RayonThumbDecoder, ThumbDecoder};
use dcs_io::source::{ScanHandle, scan};

use crate::util::{LruMap, decode_key, encode_key};

/// Pixel edge the base (default-zoom) tier decodes to.
const BASE_EDGE: u32 = 256;

/// RAM budget for the base cache. At ~175 KB per thumbnail this holds well over
/// a 5–6k folder; decode is on demand so it never exceeds this regardless of
/// folder size — the LRU recycles the oldest off-screen pixels.
const BASE_CACHE_BYTES: u64 = 1_200_000_000;

/// RAM budget for the hi-res cache. Small on purpose: only viewport cells while
/// zoomed live here, and it is dropped entirely on zoom-out.
const HIRES_CACHE_BYTES: u64 = 384_000_000;

/// Cap on base decodes kept in flight by the background fill. Enough to keep
/// the decode pool fed, low enough that viewport requests (issued first each
/// frame) aren't stuck behind a long backlog.
const BG_FILL_INFLIGHT: usize = 16;

/// A resident thumbnail. `version` bumps on every change so the UI knows when
/// to re-upload its texture (base → hi-res on zoom, or back on zoom-out).
struct CachedThumb {
    image: ThumbImage,
    version: u64,
}

/// Borrowed view of a resident thumbnail handed to the UI.
#[derive(Clone, Copy)]
pub struct ThumbView<'a> {
    pub image: &'a ThumbImage,
    pub version: u64,
}

/// Minimal per-cell facts the grid needs to paint, without cloning a `Photo`.
#[derive(Debug, Clone, Copy)]
pub struct CellInfo {
    pub id: PhotoId,
    pub raw_only: bool,
}

pub struct Session {
    builder: PoolBuilder,
    order: Vec<usize>,
    ordered_count: usize,
    scan: Option<ScanHandle>,
    decoder: RayonThumbDecoder,
    base: LruMap<PhotoId, CachedThumb>,
    hires: LruMap<PhotoId, CachedThumb>,
    base_inflight: HashSet<PhotoId>,
    hires_inflight: HashSet<PhotoId>,
    /// Display index the background base-fill has walked to.
    bg_cursor: usize,
    /// Monotonic stamp handed out to thumbnails so the UI can detect changes.
    next_version: u64,
    /// Bumped on every `open_folder`. `PhotoId`s restart at 0 per folder, so a
    /// late decode from a previous folder would otherwise land on a same-id
    /// photo in the new one. The epoch tags each decode request and stale
    /// results are dropped on arrival.
    epoch: u64,
}

impl Session {
    pub fn new() -> Self {
        Session {
            builder: PoolBuilder::default(),
            order: Vec::new(),
            ordered_count: 0,
            scan: None,
            decoder: RayonThumbDecoder::new(),
            base: LruMap::new(BASE_CACHE_BYTES),
            hires: LruMap::new(HIRES_CACHE_BYTES),
            base_inflight: HashSet::new(),
            hires_inflight: HashSet::new(),
            bg_cursor: 0,
            next_version: 0,
            epoch: 0,
        }
    }

    /// Begin scanning a folder, discarding any previous import.
    pub fn open_folder(&mut self, root: PathBuf) {
        self.builder = PoolBuilder::default();
        self.order = Vec::new();
        self.ordered_count = 0;
        self.base = LruMap::new(BASE_CACHE_BYTES);
        self.hires = LruMap::new(HIRES_CACHE_BYTES);
        self.base_inflight.clear();
        self.hires_inflight.clear();
        self.bg_cursor = 0;
        self.epoch += 1;
        self.scan = Some(scan(root));
    }

    /// Drain pending scan results and decoded thumbnails. Cheap; call once a
    /// frame before painting.
    pub fn tick(&mut self) {
        if let Some(handle) = &self.scan {
            for file in handle.drain() {
                self.builder.add(file);
            }
            if !handle.is_running() {
                // Final flush, then retire the handle so we stop polling.
                for file in handle.drain() {
                    self.builder.add(file);
                }
                self.scan = None;
            }
        }
        if self.builder.len() != self.ordered_count {
            self.order = sort::by_time_asc(self.builder.photos());
            self.ordered_count = self.builder.len();
        }
        for (key, image) in self.decoder.poll() {
            let (epoch, id, hires) = decode_key(key);
            if epoch != self.epoch {
                continue; // stale decode from a previously opened folder
            }
            self.absorb_thumb(id, hires, image);
        }
    }

    pub fn photo_count(&self) -> usize {
        self.order.len()
    }

    /// Photo at a display position in the current sort order (§2.3).
    pub fn photo_at(&self, display_index: usize) -> Option<&Photo> {
        let &pool_index = self.order.get(display_index)?;
        self.builder.photos().get(pool_index)
    }

    /// Cheap `Copy` descriptor for painting a cell — no allocation, so the grid
    /// can call it for every visible cell each frame.
    pub fn cell_info(&self, display_index: usize) -> Option<CellInfo> {
        let &pool_index = self.order.get(display_index)?;
        let photo = self.builder.photos().get(pool_index)?;
        Some(CellInfo {
            id: photo.id,
            raw_only: photo.is_raw_only(),
        })
    }

    /// Decode jobs currently in flight, base + hi-res (diagnostics §10b).
    pub fn decode_queue_depth(&self) -> usize {
        self.base_inflight.len() + self.hires_inflight.len()
    }

    /// Photos with a base thumbnail resident.
    pub fn loaded_count(&self) -> usize {
        self.base.len()
    }

    /// Resident hi-res thumbnails (diagnostics §10b).
    pub fn hires_count(&self) -> usize {
        self.hires.len()
    }

    /// Resident thumbnail pixel memory in MB across both tiers (§10b).
    pub fn thumb_memory_mb(&self) -> f32 {
        (self.base.weight() + self.hires.weight()) as f32 / (1024.0 * 1024.0)
    }

    pub fn is_scanning(&self) -> bool {
        self.scan.is_some()
    }

    /// True while there is decode work in flight — the UI uses this to keep
    /// repainting until the visible set settles.
    pub fn has_pending(&self) -> bool {
        !self.base_inflight.is_empty() || !self.hires_inflight.is_empty()
    }

    /// Ensure the cheap base (256 px) thumbnail for a display position is
    /// decoding or cached. Called for the whole visible + prefetch band every
    /// frame. No-op if cached, already decoding, or RAW-only (§2.1).
    pub fn request_base(&mut self, display_index: usize) {
        let Some(&pool_index) = self.order.get(display_index) else {
            return;
        };
        let id = self.builder.photos()[pool_index].id;
        if self.base_inflight.contains(&id) || self.base.get(&id).is_some() {
            return;
        }
        let photo = &self.builder.photos()[pool_index];
        let Some(path) = photo.decodable_path() else {
            return;
        };
        let path = path.to_path_buf();
        let orientation = photo.orientation;
        self.base_inflight.insert(id);
        self.decoder
            .request(encode_key(self.epoch, id, false), path, orientation, BASE_EDGE);
    }

    /// Ensure a hi-res thumbnail covering `target_edge` on-screen pixels is
    /// decoding or cached. Called only for viewport cells while zoomed in.
    /// Re-decodes at a larger tier when the cell grew past the cached one.
    pub fn request_hires(&mut self, display_index: usize, target_edge: u32) {
        let Some(&pool_index) = self.order.get(display_index) else {
            return;
        };
        let id = self.builder.photos()[pool_index].id;
        if self.hires_inflight.contains(&id) {
            return;
        }
        if let Some(cached) = self.hires.get(&id)
            && cached.image.width.max(cached.image.height) >= target_edge
        {
            return;
        }
        let photo = &self.builder.photos()[pool_index];
        let Some(path) = photo.decodable_path() else {
            return;
        };
        let path = path.to_path_buf();
        let orientation = photo.orientation;
        self.hires_inflight.insert(id);
        self.decoder
            .request(encode_key(self.epoch, id, true), path, orientation, target_edge);
    }

    /// Drop all hi-res thumbnails — called on zoom-out so the sharp pixels
    /// stop costing RAM. Base thumbnails (the display fallback) are untouched.
    pub fn clear_hires(&mut self) {
        if self.hires.len() == 0 && self.hires_inflight.is_empty() {
            return;
        }
        self.hires = LruMap::new(HIRES_CACHE_BYTES);
        self.hires_inflight.clear();
    }

    /// Keep base thumbnails decoding for the whole folder in the background,
    /// independent of the viewport — so zooming in (a small visible band) never
    /// stalls the rest of the folder, and scrolling later finds it ready. Walks
    /// the folder once via a cursor, throttled to a small in-flight count so
    /// viewport requests keep priority. Stops once the base cache is full.
    pub fn fill_base_background(&mut self) {
        if self.base.weight() >= BASE_CACHE_BYTES {
            return;
        }
        while self.base_inflight.len() < BG_FILL_INFLIGHT && self.bg_cursor < self.order.len() {
            let index = self.bg_cursor;
            self.bg_cursor += 1;
            self.request_base(index);
        }
    }

    /// The best resident thumbnail for a photo — hi-res if present, else base —
    /// with its version. Marks it recently used so it survives eviction.
    pub fn thumb(&mut self, id: PhotoId) -> Option<ThumbView<'_>> {
        if let Some(cached) = self.hires.get(&id) {
            return Some(ThumbView {
                image: &cached.image,
                version: cached.version,
            });
        }
        let cached = self.base.get(&id)?;
        Some(ThumbView {
            image: &cached.image,
            version: cached.version,
        })
    }

    /// Fold a decode result into the right cache, retiring the in-flight entry
    /// (even when the image is `None` after a failed decode).
    fn absorb_thumb(&mut self, id: PhotoId, hires: bool, image: Option<ThumbImage>) {
        if hires {
            self.hires_inflight.remove(&id);
        } else {
            self.base_inflight.remove(&id);
        }
        let Some(image) = image else {
            return;
        };
        let bytes = image.rgba.len() as u64;
        self.next_version += 1;
        let entry = CachedThumb {
            image,
            version: self.next_version,
        };
        let evicted = if hires {
            self.hires.insert(id, entry, bytes)
        } else {
            self.base.insert(id, entry, bytes)
        };
        for id in evicted {
            if hires {
                self.hires_inflight.remove(&id);
            } else {
                self.base_inflight.remove(&id);
            }
        }
    }
}

impl Default for Session {
    fn default() -> Self {
        Self::new()
    }
}
