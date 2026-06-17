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

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use dcs_domain::command::Command;
use dcs_domain::cull::AcceptState;
use dcs_domain::export::{self, ExportItem, ExportPlan, ExportRequest};
use dcs_domain::grouping::{self, Axis, DerivedGroup, GroupKind, TimeGranularity};
use dcs_domain::pairing::PoolBuilder;
use dcs_domain::photo::{Photo, PhotoId};
use dcs_domain::sort::Sort;
use dcs_domain::thumb::ThumbImage;
use dcs_domain::timezone;
use dcs_io::cache::{SharedCache, SqliteCache, ThumbTier};
use dcs_io::export::{ExportEvent, ExportHandle, run_export};
use dcs_io::imaging::{DecodePriority, DecodeRequest, RayonThumbDecoder, ThumbDecoder};
use dcs_io::lock::{self, LockOutcome, ProjectLock};
use dcs_io::persistence::{
    JsonProjectStore, PersistError, PhotoRecord, ProjectConfig, ProjectSnapshot, ProjectStore,
};
use dcs_io::recents::{self, Recents};
use dcs_io::source::{ScanHandle, scan};
use dcs_io::undo_log::{self, UndoLog};
use serde_json::Value;
use thiserror::Error;
use time_tz::Tz;

use crate::cull::{Cull, UndoEntry};
use crate::export::{ExportScope, ExportStatus};
use crate::selection::Selection;
use crate::thumb_cache::{ThumbCache, ThumbView};
use crate::util::{DecodeTier, decode_key, encode_key};

/// The `.dcs/` sidecar directory name (§5).
const SIDECAR_DIR: &str = ".dcs";
const CACHE_FILE: &str = "cache.sqlite3";
const UNDO_LOG_FILE: &str = "undo.log";

/// Failure saving the project. The cache and undo log are rebuildable, so their
/// errors are logged and swallowed elsewhere; only the precious `project.json`
/// surfaces here.
#[derive(Debug, Error)]
pub enum SaveError {
    #[error("no folder is open")]
    NoFolder,
    #[error(transparent)]
    Project(#[from] PersistError),
}

/// Pixel edge the base (default-zoom) tier decodes to.
const BASE_EDGE: u32 = 256;

/// RAM budget for the base cache. At ~175 KB per thumbnail this holds well over
/// a 5–6k folder; decode is on demand so it never exceeds this regardless of
/// folder size — the LRU recycles the oldest off-screen pixels.
const BASE_CACHE_BYTES: u64 = 1_200_000_000;

/// RAM budget for the hi-res cache. Small on purpose: only viewport cells while
/// zoomed live here, and it is dropped entirely on zoom-out.
const HIRES_CACHE_BYTES: u64 = 384_000_000;

/// RAM budget for the gallery cache. Holds the current full-size frame
/// plus a couple of preloaded neighbours; large enough for one 1:1 decode of a
/// big sensor (~100 MB) and a few fit-sized neighbours.
const GALLERY_CACHE_BYTES: u64 = 512_000_000;

/// Longest-side pixel cap for a 1:1 gallery decode. Bounds both RAM and the GPU
/// texture size (most backends cap a single texture near 8–16k); images larger
/// than this show at this resolution, which is still far past screen-sharp.
const GALLERY_FULL_EDGE: u32 = 8192;

/// Cap on base decodes kept in flight by the background fill. Enough to keep
/// the decode pool fed, low enough that viewport requests (issued first each
/// frame) aren't stuck behind a long backlog.
const BG_FILL_INFLIGHT: usize = 16;

/// Minimal per-cell facts the grid needs to paint, without cloning a `Photo`.
#[derive(Debug, Clone, Copy)]
pub struct CellInfo {
    pub id: PhotoId,
    pub raw_only: bool,
    /// Owned verdict for the verdict glyph + rejected dimming (§2.11).
    pub state: AcceptState,
    /// Whether this cell is in the current selection (grease-pencil outline).
    pub selected: bool,
    /// The file is absent on disk — render a placeholder + `missing` badge (§4).
    pub missing: bool,
}

/// Verdict view toggle (§2.9, #11) — a session display setting, not the full
/// chip filter system. `Unreviewed` is the working filter while culling.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum VerdictFilter {
    #[default]
    All,
    Unreviewed,
    Accepted,
    Rejected,
}

/// A derived group as the grid sees it after the verdict filter (§2.8): its
/// title + kind, where its surviving cells begin in the visible display order,
/// and how many show out of its total. Empty-after-filter groups are omitted,
/// so the UI never renders a header with no cells.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VisibleGroup {
    pub title: String,
    pub kind: GroupKind,
    /// Display index where this group's first visible cell sits.
    pub start: usize,
    /// Visible (post-filter) members.
    pub count: usize,
    /// Total members before filtering — the `count of total` header figure.
    pub total: usize,
}

pub struct Session {
    builder: PoolBuilder,
    /// Full display order (pool indices), the sort result before filtering.
    order: Vec<usize>,
    /// The order after the verdict filter — what the grid actually paints and
    /// what every display index addresses. Equals `order` when filter = `All`.
    visible: Vec<usize>,
    /// Pool revision the current grouping was derived from. Regroup when it
    /// diverges, so a RAW merging into its JPEG (which doesn't change the photo
    /// count) still re-derives the date buckets.
    pool_revision: u64,
    /// Derived grouping over the whole pool (§2.4). Recomputed when the pool,
    /// axis, sort, or shoot zone changes; never persisted.
    groups: Vec<DerivedGroup>,
    /// Per-group spans over the *visible* order, for the grid's headers.
    visible_groups: Vec<VisibleGroup>,
    /// Active grouping axis + sort — derived display settings (§2.2), not owned.
    axis: Axis,
    sort: Sort,
    /// `Auto` resolved against the current data, cached at regroup so the toolbar
    /// label doesn't re-scan the pool (and re-read the system zone) every frame.
    resolved_gran: Option<TimeGranularity>,
    /// Owned verdicts + undo/redo (§2.2, §2.9). Reset per folder (ids restart).
    cull: Cull,
    /// Ephemeral focus cursor + selection (§2.12, §2.13).
    sel: Selection,
    filter: VerdictFilter,
    scan: Option<ScanHandle>,
    decoder: RayonThumbDecoder,
    /// Cheap 256 px grid thumbnails (disk-cached).
    base: ThumbCache,
    /// Sharp viewport decodes while zoomed; dropped on zoom-out.
    hires: ThumbCache,
    /// Large fit/1:1 frames for the gallery view. Dropped on leaving gallery so
    /// the big pixels stop costing RAM.
    gallery: ThumbCache,
    /// Best decode edge already requested per gallery photo, so a steady gallery
    /// view doesn't re-request every frame and a smaller-than-target source (its
    /// native size) isn't chased forever. Pruned alongside `gallery` eviction.
    gallery_edge: HashMap<PhotoId, u32>,
    /// Display index the background base-fill has walked to.
    bg_cursor: usize,
    /// Monotonic stamp handed out to thumbnails so the UI can detect changes.
    next_version: u64,
    /// Bumped on every `open_folder`. `PhotoId`s restart at 0 per folder, so a
    /// late decode from a previous folder would otherwise land on a same-id
    /// photo in the new one. The epoch tags each decode request and stale
    /// results are dropped on arrival.
    epoch: u64,
    /// `.dcs/` sidecar for the open folder; `None` when no folder is open.
    sidecar: Option<PathBuf>,
    store: JsonProjectStore,
    /// Append handle to the durable undo log; `None` if it couldn't open
    /// (history is disposable, so the failure never blocks culling).
    log: Option<UndoLog>,
    /// Disposable thumb + fingerprint cache, shared with the scan/decode pools.
    cache: Option<SharedCache>,
    /// Views as raw JSON, round-tripped on save so unknown kinds survive (§9b).
    views: Vec<Value>,
    /// Owned project settings (§4): shoot zone, grid zoom.
    config: ProjectConfig,
    /// The open folder root; needed to re-scan and to relativize stored paths.
    root: Option<PathBuf>,
    /// Persisted photos from the loaded project, retained until the scan
    /// finishes so absent files can be reconciled into missing placeholders (§4).
    loaded_records: Vec<PhotoRecord>,
    /// Single-writer lock (#34). `None` when no folder is open.
    project_lock: Option<ProjectLock>,
    /// Another live instance holds the lock: viewing is allowed, writing is not.
    read_only: bool,
    /// App-global recent-projects list, persisted outside any project.
    recents: Recents,
    recents_path: Option<PathBuf>,
    /// Owned state has changed since the last save.
    dirty: bool,
    /// Command-palette most-recently-used action ids, newest first. Drives the
    /// palette's default order (§2.10); ephemeral, not persisted in v1.
    mru: Vec<&'static str>,
    /// Running export executor (§6.9), polled in `tick`. `None` when idle.
    export_handle: Option<ExportHandle>,
    /// Progress of the running or last-finished export, read by the dialog.
    export_status: Option<ExportStatus>,
}

impl Session {
    pub fn new() -> Self {
        Session {
            builder: PoolBuilder::default(),
            order: Vec::new(),
            visible: Vec::new(),
            pool_revision: 0,
            groups: Vec::new(),
            visible_groups: Vec::new(),
            axis: Axis::Time(TimeGranularity::Auto),
            sort: Sort::default(),
            resolved_gran: None,
            cull: Cull::new(),
            sel: Selection::new(),
            filter: VerdictFilter::All,
            scan: None,
            decoder: RayonThumbDecoder::new(),
            base: ThumbCache::new(BASE_CACHE_BYTES),
            hires: ThumbCache::new(HIRES_CACHE_BYTES),
            gallery: ThumbCache::new(GALLERY_CACHE_BYTES),
            gallery_edge: HashMap::new(),
            bg_cursor: 0,
            next_version: 0,
            epoch: 0,
            sidecar: None,
            store: JsonProjectStore,
            log: None,
            cache: None,
            views: Vec::new(),
            config: ProjectConfig::default(),
            root: None,
            loaded_records: Vec::new(),
            project_lock: None,
            read_only: false,
            // Recents persistence is opt-in: the UI enables it at startup via
            // `enable_default_recents`, so tests never touch the user's real
            // `~/.dcs/recents.json`.
            recents_path: None,
            recents: Recents::default(),
            dirty: false,
            mru: Vec::new(),
            export_handle: None,
            export_status: None,
        }
    }

    /// Begin scanning a folder, discarding any previous import. Loads the
    /// `.dcs/` sidecar first: verdicts from `project.json` (authoritative),
    /// undo/redo stacks folded from `undo.log` (never replayed onto state), and
    /// the disposable cache that seeds fingerprint reuse and thumb blobs. The
    /// pool builder is seeded so a rename-in-place reclaims its id and verdict.
    pub fn open_folder(&mut self, root: PathBuf) {
        self.order = Vec::new();
        self.visible = Vec::new();
        self.pool_revision = 0;
        self.groups = Vec::new();
        self.visible_groups = Vec::new();
        self.resolved_gran = None;
        self.sel = Selection::new();
        self.filter = VerdictFilter::All;
        // Abandon any running export from the previous folder (its handle drops).
        self.export_handle = None;
        self.export_status = None;
        self.base.reset(BASE_CACHE_BYTES);
        self.hires.reset(HIRES_CACHE_BYTES);
        self.gallery.reset(GALLERY_CACHE_BYTES);
        self.gallery_edge.clear();
        self.bg_cursor = 0;
        self.epoch += 1;
        self.dirty = false;

        let sidecar = root.join(SIDECAR_DIR);
        let _ = std::fs::create_dir_all(&sidecar);
        self.cache = open_cache(&sidecar);

        // Release the previous folder's lock *before* acquiring — including
        // when reopening or re-scanning the same folder, where our own fresh
        // lock would otherwise be misread as a live second instance (#34).
        self.project_lock = None;
        // A live second instance opens read-only; a stale/absent lock is
        // reclaimed.
        let (project_lock, outcome) = ProjectLock::acquire(&sidecar, lock::DEFAULT_STALE);
        self.read_only = outcome == LockOutcome::HeldByOther;
        self.project_lock = Some(project_lock);

        // project.json is the authoritative verdict state; undo.log only
        // reconstructs the stacks (open Q#9). A missing/fresh folder yields an
        // empty pool builder and an empty Cull.
        let snapshot = self.store.load(&sidecar).ok().flatten();
        self.builder = seed_builder(&snapshot);
        self.cull = seed_cull(&snapshot, &sidecar);
        let (views, config, records) = match snapshot {
            Some(s) => (s.views, s.config, s.photos),
            None => (default_views(), ProjectConfig::default(), Vec::new()),
        };
        self.views = views;
        self.config = config;
        self.loaded_records = records;
        self.log = UndoLog::open(&sidecar.join(UNDO_LOG_FILE)).ok();
        self.sidecar = Some(sidecar);
        self.root = Some(root.clone());
        self.remember_recent(&root);

        self.rebuild_visible();
        self.scan = Some(scan(root, self.cache.clone()));
    }

    /// Drain pending scan results and decoded thumbnails. Cheap; call once a
    /// frame before painting.
    pub fn tick(&mut self) {
        let mut scan_finished = false;
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
                scan_finished = true;
            }
        }
        if scan_finished {
            // Persisted photos whose files weren't found become placeholders so
            // their state is preserved and they reanimate if the file returns (§4).
            self.reconcile_missing();
        }
        if self.builder.revision() != self.pool_revision {
            self.regroup();
        }
        for (key, image) in self.decoder.poll() {
            let (epoch, id, tier) = decode_key(key);
            if epoch != self.epoch {
                continue; // stale decode from a previously opened folder
            }
            self.absorb_thumb(id, tier, image);
        }
        self.poll_export();
    }

    /// Drain export-executor events into the live status; clear the handle when
    /// the run finishes so the dialog can show its completion state (§6.7).
    fn poll_export(&mut self) {
        let Some(handle) = self.export_handle.as_ref() else {
            return;
        };
        let mut events = handle.poll();
        let running = handle.is_running();
        if !running {
            // The worker finishes the loop *then* flips the done flag, so events
            // sent between the poll above and this check are still in the channel.
            // Drain once more before retiring the handle or they're lost.
            events.extend(handle.poll());
        }
        if let Some(status) = self.export_status.as_mut() {
            for event in events {
                match event {
                    ExportEvent::Copied { .. } => status.copied += 1,
                    ExportEvent::Skipped { .. } => status.skipped += 1,
                    ExportEvent::Failed { .. } => status.failed += 1,
                }
            }
            status.running = running;
        }
        if !running {
            self.export_handle = None;
        }
    }

    /// Number of cells the grid paints — visible photos after the verdict
    /// filter (§2.9). Equals the pool size when filter = `All`.
    pub fn photo_count(&self) -> usize {
        self.visible.len()
    }

    /// Total photos imported, ignoring the filter. Lets the UI tell "no folder
    /// open" apart from "the filter hid everything".
    pub fn pool_len(&self) -> usize {
        self.builder.len()
    }

    /// Photo at a display position in the current visible order (§2.3).
    pub fn photo_at(&self, display_index: usize) -> Option<&Photo> {
        let &pool_index = self.visible.get(display_index)?;
        self.builder.photos().get(pool_index)
    }

    /// The derived group title a display position falls under, for the gallery
    /// caption. `None` for the headerless `none`-axis stream.
    pub fn group_title_at(&self, display_index: usize) -> Option<&str> {
        self.visible_groups
            .iter()
            .find(|g| display_index >= g.start && display_index < g.start + g.count)
            .map(|g| g.title.as_str())
            .filter(|t| !t.is_empty())
    }

    /// The capture time adjusted to the shoot zone, formatted for the gallery
    /// caption. `None` when the photo is undated.
    pub fn caption_time(&self, display_index: usize) -> Option<String> {
        let naive = self.photo_at(display_index)?.captured_at?;
        let dt = timezone::adjusted(naive, self.resolve_zone());
        Some(format!(
            "{:04}-{:02}-{:02} {:02}:{:02}:{:02}",
            dt.year(),
            u8::from(dt.month()),
            dt.day(),
            dt.hour(),
            dt.minute(),
            dt.second()
        ))
    }

    /// Cheap `Copy` descriptor for painting a cell — no allocation, so the grid
    /// can call it for every visible cell each frame.
    pub fn cell_info(&self, display_index: usize) -> Option<CellInfo> {
        let &pool_index = self.visible.get(display_index)?;
        let photo = self.builder.photos().get(pool_index)?;
        Some(CellInfo {
            id: photo.id,
            raw_only: photo.is_raw_only(),
            state: self.cull.state(photo.id),
            selected: self.sel.is_selected(photo.id),
            missing: photo.missing,
        })
    }

    /// Decode jobs currently in flight, base + hi-res (diagnostics §10b).
    pub fn decode_queue_depth(&self) -> usize {
        self.base.inflight_len() + self.hires.inflight_len() + self.gallery.inflight_len()
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
        (self.base.weight() + self.hires.weight() + self.gallery.weight()) as f32
            / (1024.0 * 1024.0)
    }

    pub fn is_scanning(&self) -> bool {
        self.scan.is_some()
    }

    /// True while there is decode work in flight — the UI uses this to keep
    /// repainting until the visible set settles.
    pub fn has_pending(&self) -> bool {
        self.base.pending() || self.hires.pending()
    }

    /// Ensure the cheap base (256 px) thumbnail for a display position is
    /// decoding or cached. Called for the whole visible + prefetch band every
    /// frame at foreground priority. No-op if cached, already decoding, or
    /// RAW-only.
    pub fn request_base(&mut self, display_index: usize) {
        self.request_base_at(display_index, DecodePriority::High);
    }

    /// Core base request, parameterized by scheduling lane: the viewport/filmstrip
    /// use `High`, the whole-folder background fill uses `Low`.
    fn request_base_at(&mut self, display_index: usize, priority: DecodePriority) {
        let Some(&pool_index) = self.visible.get(display_index) else {
            return;
        };
        // A missing file has no pixels; its decode always fails and so never
        // caches, which would re-request it every frame. Skip it.
        if self.builder.photos()[pool_index].missing {
            return;
        }
        let id = self.builder.photos()[pool_index].id;
        if !self.base.idle(id) {
            return;
        }
        let photo = &self.builder.photos()[pool_index];
        let Some(path) = photo.decodable_path() else {
            return;
        };
        let path = path.to_path_buf();
        let orientation = photo.orientation;
        // The grid (base) tier is disk-cached by content fingerprint, so a
        // reopened folder paints from cached blobs instead of re-decoding.
        let cache_key = Some(photo.fingerprint);
        self.base.start(id);
        self.decoder.request(DecodeRequest {
            key: encode_key(self.epoch, id, DecodeTier::Base),
            path,
            orientation,
            edge: BASE_EDGE,
            cache_key,
            tier: ThumbTier::Grid,
            cache: self.cache.clone(),
            priority,
        });
    }

    /// Ensure a hi-res thumbnail covering `target_edge` on-screen pixels is
    /// decoding or cached. Called only for viewport cells while zoomed in.
    /// Re-decodes at a larger tier when the cell grew past the cached one.
    pub fn request_hires(&mut self, display_index: usize, target_edge: u32) {
        let Some(&pool_index) = self.visible.get(display_index) else {
            return;
        };
        if self.builder.photos()[pool_index].missing {
            return; // §4: nothing to decode for a missing file (see `request_base`)
        }
        let id = self.builder.photos()[pool_index].id;
        if self.hires.is_inflight(id) {
            return;
        }
        // Already resident at or above the wanted size → nothing to do.
        if self
            .hires
            .view(id)
            .is_some_and(|v| v.image.width.max(v.image.height) >= target_edge)
        {
            return;
        }
        let photo = &self.builder.photos()[pool_index];
        let Some(path) = photo.decodable_path() else {
            return;
        };
        let path = path.to_path_buf();
        let orientation = photo.orientation;
        self.hires.start(id);
        // Hi-res is viewport-ephemeral and its size tracks the zoom, so it is
        // not disk-cached (no stable tier to key it on); it lives only in RAM
        // and is dropped on zoom-out.
        self.decoder.request(DecodeRequest {
            key: encode_key(self.epoch, id, DecodeTier::Hires),
            path,
            orientation,
            edge: target_edge,
            cache_key: None,
            tier: ThumbTier::Gallery,
            cache: None,
            priority: DecodePriority::High,
        });
    }

    /// Drop all hi-res thumbnails — called on zoom-out so the sharp pixels
    /// stop costing RAM. Base thumbnails (the display fallback) are untouched.
    pub fn clear_hires(&mut self) {
        if self.hires.len() == 0 && !self.hires.pending() {
            return;
        }
        self.hires.reset(HIRES_CACHE_BYTES);
    }

    /// Ensure the gallery frame for a display position is decoding or resident,
    /// sized to cover `fit_edge` device pixels, and preload the two neighbours at
    /// the same size so `←`/`→` lands on a ready image. Called each frame
    /// while the gallery is open.
    pub fn request_gallery(&mut self, display_index: usize, fit_edge: u32) {
        self.request_gallery_at(display_index, fit_edge);
        if display_index > 0 {
            self.request_gallery_at(display_index - 1, fit_edge);
        }
        self.request_gallery_at(display_index + 1, fit_edge);
    }

    /// Request a 1:1 (native-resolution) decode of one photo, capped at the GPU
    /// texture limit — the `Z` zoom-to-100% path.
    pub fn request_gallery_full(&mut self, display_index: usize) {
        self.request_gallery_at(display_index, GALLERY_FULL_EDGE);
    }

    /// The resident gallery frame for a photo, if decoded. Marks it recently used.
    pub fn gallery_image(&mut self, id: PhotoId) -> Option<ThumbView<'_>> {
        self.gallery.view(id)
    }

    /// Drop every gallery frame — called on leaving the gallery so the large
    /// pixels stop costing RAM. Base/hi-res caches are untouched.
    pub fn clear_gallery(&mut self) {
        if self.gallery.len() == 0 && !self.gallery.pending() {
            return;
        }
        self.gallery.reset(GALLERY_CACHE_BYTES);
        self.gallery_edge.clear();
    }

    /// True while a gallery frame is still decoding — the UI keeps repainting
    /// until the visible frame resolves.
    pub fn has_gallery_pending(&self) -> bool {
        self.gallery.pending()
    }

    /// Core gallery decode request: decode `display_index` at `edge` px on its
    /// longest side unless an at-least-as-large frame is already resident or in
    /// flight. Not disk-cached — gallery frames are large and ephemeral.
    fn request_gallery_at(&mut self, display_index: usize, edge: u32) {
        let Some(&pool_index) = self.visible.get(display_index) else {
            return;
        };
        if self.builder.photos()[pool_index].missing {
            return;
        }
        let id = self.builder.photos()[pool_index].id;
        if self.gallery.is_inflight(id) {
            return;
        }
        // `gallery_edge` is cleared on eviction, so a recorded edge ≥ target
        // means the frame is still resident (or in flight) at that size.
        if self.gallery_edge.get(&id).copied().unwrap_or(0) >= edge {
            return;
        }
        let photo = &self.builder.photos()[pool_index];
        let Some(path) = photo.decodable_path() else {
            return;
        };
        let path = path.to_path_buf();
        let orientation = photo.orientation;
        self.gallery_edge.insert(id, edge);
        self.gallery.start(id);
        self.decoder.request(DecodeRequest {
            key: encode_key(self.epoch, id, DecodeTier::Gallery),
            path,
            orientation,
            edge,
            cache_key: None,
            tier: ThumbTier::Gallery,
            cache: None,
            priority: DecodePriority::High,
        });
    }

    /// Whether the background fill still has folder left to walk and cache room
    /// for it — so the UI keeps repainting to drive it even when fully idle.
    pub fn has_background_work(&self) -> bool {
        self.bg_cursor < self.visible.len() && self.base.weight() < BASE_CACHE_BYTES
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
        while self.base.inflight_len() < BG_FILL_INFLIGHT && self.bg_cursor < self.visible.len() {
            let index = self.bg_cursor;
            self.bg_cursor += 1;
            // Low priority: the whole-folder fill must never delay a viewport or
            // gallery decode, which run on the high-priority pool.
            self.request_base_at(index, DecodePriority::Low);
        }
    }

    /// The best resident thumbnail for a photo — hi-res if present, else base —
    /// with its version. Marks it recently used so it survives eviction.
    pub fn thumb(&mut self, id: PhotoId) -> Option<ThumbView<'_>> {
        if let Some(view) = self.hires.view(id) {
            return Some(view);
        }
        self.base.view(id)
    }

    /// Display index of the focus cursor, if any (§2.13, #31).
    pub fn focus(&self) -> Option<usize> {
        self.sel.focus()
    }

    pub fn is_selected(&self, id: PhotoId) -> bool {
        self.sel.is_selected(id)
    }

    pub fn selection_count(&self) -> usize {
        self.sel.count()
    }

    /// Owned verdict for a photo (absent = `Unreviewed`).
    pub fn verdict(&self, id: PhotoId) -> AcceptState {
        self.cull.state(id)
    }

    /// `(accepted, rejected, unreviewed)` for the status bar. Totals only
    /// displayable photos: hidden RAW-only photos aren't part of the cull
    /// workflow, so counting them would make `unrev` exceed the shown count.
    /// Unreviewed = displayable count minus the two reviewed tallies.
    pub fn verdict_counts(&self) -> (usize, usize, usize) {
        let c = self.cull.counts();
        let total = self
            .builder
            .photos()
            .iter()
            .filter(|p| !p.is_raw_only())
            .count();
        let unreviewed = total.saturating_sub(c.accepted + c.rejected);
        (c.accepted, c.rejected, unreviewed)
    }

    /// Owned state has changed since the last successful save (§10b debounce).
    pub fn is_dirty(&self) -> bool {
        self.dirty
    }

    /// Persist owned state if it changed since the last save; no-op when clean.
    /// The UI calls this on a debounce, on quit, and on an interval (§10b).
    pub fn save_if_dirty(&mut self) -> Result<(), SaveError> {
        if !self.dirty {
            return Ok(());
        }
        self.save()
    }

    /// Persist owned state now: `project.json` written atomically (verdicts +
    /// every known photo's id/fingerprint/paths + views + config), then the
    /// durable `undo.log` compacted. The log is rebuildable, so a compaction
    /// failure is swallowed and never fails the precious save. A read-only
    /// instance can't write, so this is a no-op there (#34).
    pub fn save(&mut self) -> Result<(), SaveError> {
        if self.read_only {
            return Ok(());
        }
        let sidecar = self.sidecar.clone().ok_or(SaveError::NoFolder)?;
        let snapshot = self.build_snapshot();
        self.store.save(&sidecar, &snapshot)?;
        let stacks = undo_log::Stacks {
            undo: self.cull.undo_entries(),
            redo: self.cull.redo_entries(),
        };
        let log_path = sidecar.join(UNDO_LOG_FILE);
        // Close the append handle before compaction rewrites the log via
        // tmp→rename. Otherwise our handle is left on the old, now-unlinked
        // inode (Unix) — silently dropping every record appended after this
        // save — or blocks the rename entirely (Windows). Reopen onto the fresh
        // compacted file so live appends keep landing in it.
        self.log = None;
        let _ = undo_log::compact(&log_path, &stacks, undo_log::DEFAULT_ENTRY_CAP);
        self.log = UndoLog::open(&log_path).ok();
        self.refresh_lock(); // keep our lock fresh on every save (#34)
        self.dirty = false;
        Ok(())
    }

    /// Snapshot every known photo (not just culled ones, and including missing
    /// placeholders) so a rename-in-place reclaims its id and a vanished file
    /// keeps its state (§4, §10b). Paths are stored relative to the root.
    fn build_snapshot(&self) -> ProjectSnapshot {
        let root = self.root.as_deref();
        let photos = self
            .builder
            .photos()
            .iter()
            .map(|p| PhotoRecord {
                id: p.id,
                fingerprint: p.fingerprint,
                verdict: self.cull.state(p.id),
                jpeg: relativize(p.files.jpeg.as_deref(), root),
                raw: relativize(p.files.raw.as_deref(), root),
            })
            .collect();
        ProjectSnapshot {
            photos,
            next_id: self.builder.next_id(),
            views: self.views.clone(),
            config: self.config.clone(),
        }
    }

    /// Fold persisted photos whose files weren't scanned into missing
    /// placeholders (§4). Runs once after the scan completes; consumed records
    /// leave `loaded_records` empty so it's idempotent.
    fn reconcile_missing(&mut self) {
        let root = self.root.clone();
        for rec in std::mem::take(&mut self.loaded_records) {
            let abs = |rel: Option<PathBuf>| match (&root, rel) {
                (Some(r), Some(p)) => Some(r.join(p)),
                (None, p) => p,
                (_, None) => None,
            };
            self.builder
                .add_missing(rec.fingerprint, abs(rec.jpeg), abs(rec.raw));
        }
    }

    /// Photos whose files are currently absent on disk (§4).
    pub fn missing_count(&self) -> usize {
        self.builder.photos().iter().filter(|p| p.missing).count()
    }

    /// Forget every missing photo, removing it and its owned state from the
    /// project (§4) — the explicit prune for files the user knows are gone for
    /// good. Returns how many were removed. A no-op when read-only (#34).
    pub fn forget_missing(&mut self) -> usize {
        if self.read_only {
            return 0;
        }
        let ids: HashSet<PhotoId> = self
            .builder
            .photos()
            .iter()
            .filter(|p| p.missing)
            .map(|p| p.id)
            .collect();
        if ids.is_empty() {
            return 0;
        }
        let removed = ids.len();
        self.builder.forget(&ids);
        self.cull.forget(&ids);
        self.sel.clear();
        self.regroup();
        self.dirty = true;
        removed
    }

    /// True while another live instance holds the write lock (#34).
    pub fn is_read_only(&self) -> bool {
        self.read_only
    }

    /// Forcibly claim the write lock (UI "Take over"); we become read-write.
    pub fn take_over(&mut self) {
        if let Some(lock) = &mut self.project_lock {
            lock.take_over();
            self.read_only = !lock.is_owned();
        }
    }

    /// Refresh our lock timestamp so other instances keep seeing us as live.
    /// The UI calls this on a heartbeat while a folder is open (#34).
    pub fn refresh_lock(&self) {
        if let Some(lock) = &self.project_lock {
            lock.refresh();
        }
    }

    /// True when a folder is open (so the UI can enable "Rescan").
    pub fn has_folder(&self) -> bool {
        self.root.is_some()
    }

    /// Re-scan the open folder: save first so the reload restores owned state,
    /// then reopen. New files appear, returned files reanimate, and removed
    /// files become missing placeholders (§4).
    pub fn rescan(&mut self) {
        if let Some(root) = self.root.clone() {
            let _ = self.save_if_dirty();
            self.open_folder(root);
        }
    }

    /// Enable the app-global recents store at its default location
    /// (`~/.dcs/recents.json`), loading the existing list and pruning folders
    /// that no longer exist. The UI calls this once at startup; tests leave it
    /// off so they never touch the real file.
    pub fn enable_default_recents(&mut self) {
        self.set_recents_path(recents::recents_path());
        self.recents.retain_existing();
        self.persist_recents();
    }

    /// Redirect or disable the app-global recents store. `None` disables
    /// persistence entirely (and clears the in-memory list). Tests that exercise
    /// recents point this at a temp file.
    pub fn set_recents_path(&mut self, path: Option<PathBuf>) {
        self.recents = path.as_deref().map(recents::load).unwrap_or_default();
        self.recents_path = path;
    }

    /// Recent project folders, most-recent first (app-global, §4).
    pub fn recent_projects(&self) -> &[PathBuf] {
        &self.recents.projects
    }

    /// Clear the recent-projects list and persist the change.
    pub fn clear_recents(&mut self) {
        self.recents.clear();
        self.persist_recents();
    }

    /// The persisted grid cell size, if any (§4, §9b).
    pub fn grid_zoom(&self) -> Option<f32> {
        self.config.grid_zoom
    }

    /// Persist the grid zoom; marks the project dirty so it saves on debounce.
    pub fn set_grid_zoom(&mut self, zoom: f32) {
        if self.read_only || self.config.grid_zoom == Some(zoom) {
            return;
        }
        self.config.grid_zoom = Some(zoom);
        self.dirty = true;
    }

    /// The persisted IANA shoot timezone (freeze-critical, open Q#5).
    pub fn shoot_zone(&self) -> Option<&str> {
        self.config.shoot_zone.as_deref()
    }

    /// Set the shoot timezone; marks the project dirty and regroups, since time
    /// derivation depends on it (§2.4).
    pub fn set_shoot_zone(&mut self, zone: Option<String>) {
        if self.read_only || self.config.shoot_zone == zone {
            return;
        }
        self.config.shoot_zone = zone;
        self.dirty = true;
        self.regroup();
    }

    fn remember_recent(&mut self, root: &Path) {
        self.recents.record(root.to_path_buf());
        self.persist_recents();
    }

    fn persist_recents(&self) {
        if let Some(path) = &self.recents_path {
            let _ = recents::save(path, &self.recents);
        }
    }

    /// Palette action ids in most-recently-used order, newest first (§2.10).
    pub fn action_mru(&self) -> &[&'static str] {
        &self.mru
    }

    /// Record that a palette action ran, moving its id to the front of the MRU.
    pub(crate) fn note_action(&mut self, id: &'static str) {
        self.mru.retain(|&existing| existing != id);
        self.mru.insert(0, id);
    }

    pub fn can_undo(&self) -> bool {
        self.cull.can_undo()
    }

    pub fn can_redo(&self) -> bool {
        self.cull.can_redo()
    }

    pub fn filter(&self) -> VerdictFilter {
        self.filter
    }

    /// Switch the verdict view (§2.9). Recomputes the visible order and rewinds
    /// the background fill so any newly-visible photos decode.
    pub fn set_filter(&mut self, filter: VerdictFilter) {
        if self.filter == filter {
            return;
        }
        self.filter = filter;
        self.bg_cursor = 0;
        self.rebuild_visible();
    }

    /// The visible groups (post-filter spans) the grid draws headers from (§2.8).
    pub fn groups(&self) -> &[VisibleGroup] {
        &self.visible_groups
    }

    pub fn axis(&self) -> Axis {
        self.axis
    }

    pub fn sort(&self) -> Sort {
        self.sort
    }

    /// The granularity actually in effect, with `Auto` resolved against the data
    /// (§2.4) — what the UI shows as `groups: auto (day)`.
    pub fn resolved_granularity(&self) -> Option<TimeGranularity> {
        self.resolved_gran
    }

    /// Change the grouping axis (§2.8) — a derived display setting; regroups.
    pub fn set_axis(&mut self, axis: Axis) {
        if self.axis == axis {
            return;
        }
        self.axis = axis;
        self.bg_cursor = 0;
        self.regroup();
    }

    /// Change the sort key/direction (§2.3); regroups (group order + members).
    pub fn set_sort(&mut self, sort: Sort) {
        if self.sort == sort {
            return;
        }
        self.sort = sort;
        self.bg_cursor = 0;
        self.regroup();
    }

    /// Plan an export over `scope` with the dialog's `request`, via the pure
    /// planner (§6.9). The result drives both the live preview and the run, so
    /// the dialog can never disagree with what gets copied. Pure: no disk access.
    pub fn plan_export(
        &self,
        scope: ExportScope,
        request: &ExportRequest,
    ) -> Result<ExportPlan, dcs_domain::export::ExportError> {
        let photos = self.builder.photos();
        let title_of = self.group_titles();
        let items: Vec<ExportItem> = self
            .scope_indices(scope)
            .into_iter()
            .map(|i| ExportItem {
                photo: &photos[i],
                group_title: title_of.get(&i).copied(),
            })
            .collect();
        let root = self.root.as_deref().unwrap_or(Path::new(""));
        export::plan_export(&items, root, request)
    }

    /// How many photos `scope` resolves to — the dialog's live per-scope count.
    pub fn export_scope_count(&self, scope: ExportScope) -> usize {
        self.scope_indices(scope).len()
    }

    /// Unreviewed photos in the pool — surfaced as the "N unreviewed excluded"
    /// honesty note when scope is `Accepted` (§6.2).
    pub fn unreviewed_count(&self) -> usize {
        self.export_scope_count(ExportScope::Unreviewed)
    }

    /// Hand a planned export to the `dcs-io` executor and begin tracking it.
    pub fn start_export(&mut self, plan: ExportPlan) {
        let total = plan.ops.len();
        self.export_handle = Some(run_export(plan));
        self.export_status = Some(ExportStatus {
            total,
            running: true,
            ..ExportStatus::default()
        });
    }

    /// Progress of the running or last-finished export, if one has started.
    pub fn export_status(&self) -> Option<ExportStatus> {
        self.export_status
    }

    /// Request cancellation of the running export (§6.7).
    pub fn cancel_export(&self) {
        if let Some(handle) = &self.export_handle {
            handle.cancel();
        }
    }

    /// Forget the last export's finished status (the dialog dismissing its toast).
    pub fn clear_export_status(&mut self) {
        if self.export_handle.is_none() {
            self.export_status = None;
        }
    }

    /// Whether any photo is rejected — gates the "reveal rejected" action.
    /// Reads the maintained verdict tally (O(reviewed)) rather than scanning the
    /// whole pool, since the menu bar polls this every frame.
    pub fn has_rejected(&self) -> bool {
        self.cull.counts().rejected > 0
    }

    /// Open the OS file manager at the source folder so the rejected originals
    /// can be acted on outside the app (§6.5). No-op when no folder is open.
    pub fn reveal_rejected(&self) {
        if let Some(root) = &self.root {
            self.reveal(root);
        }
    }

    /// Open the OS file manager at `path` — the "Open folder" affordance after an
    /// export (§6.7).
    pub fn reveal(&self, path: &Path) {
        dcs_io::reveal::reveal(path);
    }

    /// Pool indices in `scope`, in display order so `{seq}` and the on-disk
    /// order match the sheet.
    fn scope_indices(&self, scope: ExportScope) -> Vec<usize> {
        let photos = self.builder.photos();
        self.order
            .iter()
            .copied()
            .filter(|&i| {
                let id = photos[i].id;
                match scope {
                    ExportScope::Selection => self.sel.is_selected(id),
                    ExportScope::Accepted => self.cull.state(id) == AcceptState::Accepted,
                    ExportScope::Rejected => self.cull.state(id) == AcceptState::Rejected,
                    ExportScope::Unreviewed => self.cull.state(id) == AcceptState::Unreviewed,
                    ExportScope::AcceptedAndUnreviewed => {
                        self.cull.state(id) != AcceptState::Rejected
                    }
                    ExportScope::Everything => true,
                }
            })
            .collect()
    }

    /// Map each pool index to its derived group title (for `GroupAsFolders` and
    /// `{group}`). The empty stream title (axis `none`) maps to nothing.
    fn group_titles(&self) -> HashMap<usize, &str> {
        let mut map = HashMap::new();
        for group in &self.groups {
            if group.title.is_empty() {
                continue;
            }
            for &member in &group.members {
                map.insert(member, group.title.as_str());
            }
        }
        map
    }

    /// Move the focus cursor (`←→` = ±1 column, `↑↓` = ±1 row) over the flat
    /// visible order. `extend` (Shift) grows the selection from the anchor
    /// (§2.13, #31). Group-aware navigation drives `set_focus` instead — group
    /// boundaries and collapse are layout facts the UI owns (§2.8).
    pub fn nav(&mut self, dx: isize, dy: isize, cols: usize, extend: bool) {
        let order = self.visible_ids();
        self.sel.move_focus(dx, dy, cols, &order, extend);
    }

    /// Park the focus on display index `idx` (clamped), with `extend` (Shift)
    /// range semantics. The set point for layout-aware navigation, which resolves
    /// the target index from the visual group layout.
    pub fn set_focus(&mut self, idx: usize, extend: bool) {
        let order = self.visible_ids();
        self.sel.set_focus_index(idx, &order, extend);
    }

    /// The cover cell of a visible group: its first accepted member, else its
    /// first cell (#16). Derived on demand — verdicts change without a regroup,
    /// so a stored cover would go stale. The collapsed group's stand-in, and the
    /// cell navigation lands on when entering a collapsed group.
    pub fn group_cover(&self, group: &VisibleGroup) -> usize {
        (group.start..group.start + group.count)
            .find(|&i| {
                self.cell_info(i)
                    .map(|c| c.state == AcceptState::Accepted)
                    .unwrap_or(false)
            })
            .unwrap_or(group.start)
    }

    /// `Ctrl+A`: select every visible photo (#14).
    pub fn select_all_visible(&mut self) {
        let order = self.visible_ids();
        self.sel.select_all_visible(&order);
    }

    /// A pointer click on a cell, with the held modifiers. Owns the selection
    /// *policy* (plain = pick one, shift = extend, ctrl/cmd = toggle) so the UI
    /// only reports the raw event (§2.12). Selection is ephemeral, not a
    /// registry command.
    pub fn pointer_select(&mut self, display_index: usize, shift: bool, cmd: bool) {
        if shift {
            self.shift_click_select(display_index);
        } else if cmd {
            self.toggle_click_select(display_index);
        } else {
            self.click_select(display_index);
        }
    }

    /// Click: select exactly one cell, making it focus + anchor.
    pub fn click_select(&mut self, display_index: usize) {
        let order = self.visible_ids();
        self.sel.select_only(display_index, &order);
    }

    /// Shift+click: extend the selection from the anchor to this cell.
    pub fn shift_click_select(&mut self, display_index: usize) {
        let order = self.visible_ids();
        self.sel.extend_to(display_index, &order);
    }

    /// Ctrl/Cmd+click: toggle this cell in or out of the selection.
    pub fn toggle_click_select(&mut self, display_index: usize) {
        let order = self.visible_ids();
        self.sel.toggle_at(display_index, &order);
    }

    /// `Esc`: clear the selection (the only Esc-chain member this phase, §2.12).
    pub fn clear_selection(&mut self) {
        self.sel.clear();
    }

    /// `A`: accept the selection (or focused photo), toggling back to
    /// `Unreviewed` when the focused cell is already accepted (§2.9).
    pub fn accept(&mut self) {
        self.toggle_verdict(AcceptState::Accepted);
    }

    /// `X`: reject, with the same toggle-back semantics (§2.9).
    pub fn reject(&mut self) {
        self.toggle_verdict(AcceptState::Rejected);
    }

    /// `Ctrl+Z`: undo the last verdict change.
    pub fn undo(&mut self) -> bool {
        if self.read_only {
            return false;
        }
        if self.cull.undo() {
            if let Some(log) = &mut self.log {
                let _ = log.record_undo();
            }
            self.dirty = true;
            self.rebuild_visible();
            true
        } else {
            false
        }
    }

    /// `Ctrl+Shift+Z`: redo.
    pub fn redo(&mut self) -> bool {
        if self.read_only {
            return false;
        }
        if self.cull.redo() {
            if let Some(log) = &mut self.log {
                let _ = log.record_redo();
            }
            self.dirty = true;
            self.rebuild_visible();
            true
        } else {
            false
        }
    }

    /// Toggle target is decided by the *focused* photo's verdict, then applied
    /// to the whole selection — so a mixed selection resolves predictably (§2.9).
    fn toggle_verdict(&mut self, on: AcceptState) {
        if self.read_only {
            return; // another instance owns the write lock (#34)
        }
        let order = self.visible_ids();
        let targets = self.sel.selected_or_focused(&order);
        if targets.is_empty() {
            return;
        }
        let focus_state = self
            .sel
            .focus()
            .and_then(|i| order.get(i).copied())
            .map(|id| self.cull.state(id))
            .unwrap_or_default();
        let target = if focus_state == on {
            AcceptState::Unreviewed
        } else {
            on
        };
        if let Some(changes) = self.cull.dispatch(Command::SetState(targets, target)) {
            if let Some(log) = &mut self.log {
                let _ = log.record_do(&changes);
            }
            self.dirty = true;
        }
        self.rebuild_visible();
    }

    /// Recompute the grouping over the whole pool, then the visible order.
    /// Called when the pool, axis, sort, or shoot zone changes (§2.2 derived).
    fn regroup(&mut self) {
        let zone = self.resolve_zone();
        self.resolved_gran = match self.axis {
            Axis::Time(g) => Some(grouping::resolve_auto(self.builder.photos(), zone, g)),
            Axis::None => None,
        };
        let groups = grouping::group(self.builder.photos(), self.axis, zone, self.sort);
        self.order = groups
            .iter()
            .flat_map(|g| g.members.iter().copied())
            .collect();
        self.groups = groups;
        self.pool_revision = self.builder.revision();
        self.rebuild_visible();
    }

    /// Resolve the shoot zone for derivation: the configured IANA zone, else the
    /// system zone, else UTC. Domain stays pure — it only ever sees a concrete
    /// `Tz`; the system lookup (an environment read) lives here.
    fn resolve_zone(&self) -> &'static Tz {
        self.config
            .shoot_zone
            .as_deref()
            .and_then(timezone::zone)
            .or_else(|| time_tz::system::get_timezone().ok())
            .unwrap_or_else(|| timezone::zone("UTC").expect("UTC is always present"))
    }

    /// Filter the grouped order into the visible order and rebuild the per-group
    /// spans the grid headers read. Walks groups in order so spans and cells
    /// stay in lockstep; groups with no surviving members are omitted (§2.8).
    fn rebuild_visible(&mut self) {
        let filter = self.filter;
        let photos = self.builder.photos();
        let cull = &self.cull;
        let passes = |i: usize| {
            // v1 can't decode a RAW, so a RAW-only photo has nothing to show:
            // keep it in the pool (paired, persisted, ready for RAW decode later)
            // but out of the grid. A paired photo displays via its JPEG.
            if photos[i].is_raw_only() {
                return false;
            }
            let state = cull.state(photos[i].id);
            match filter {
                VerdictFilter::All => true,
                VerdictFilter::Unreviewed => state == AcceptState::Unreviewed,
                VerdictFilter::Accepted => state == AcceptState::Accepted,
                VerdictFilter::Rejected => state == AcceptState::Rejected,
            }
        };
        let mut visible = Vec::new();
        let mut spans = Vec::new();
        for g in &self.groups {
            let start = visible.len();
            visible.extend(g.members.iter().copied().filter(|&i| passes(i)));
            let count = visible.len() - start;
            if count > 0 {
                spans.push(VisibleGroup {
                    title: g.title.clone(),
                    kind: g.kind,
                    start,
                    count,
                    total: g.members.len(),
                });
            }
        }
        self.visible = visible;
        self.visible_groups = spans;
        self.sel.clamp_focus(self.visible.len());
    }

    /// The visible order as stable ids, for selection/nav. Allocates — only
    /// called on input events, never on the per-frame paint path.
    fn visible_ids(&self) -> Vec<PhotoId> {
        let photos = self.builder.photos();
        self.visible.iter().map(|&i| photos[i].id).collect()
    }

    /// Fold a decode result into its tier's cache, retiring the in-flight marker
    /// (even on a failed decode). For the gallery tier, the per-photo edge record
    /// is pruned in lockstep with eviction so a re-entered photo re-decodes.
    fn absorb_thumb(&mut self, id: PhotoId, tier: DecodeTier, image: Option<ThumbImage>) {
        let Some(image) = image else {
            match tier {
                DecodeTier::Base => self.base.fail(id),
                DecodeTier::Hires => self.hires.fail(id),
                DecodeTier::Gallery => {
                    self.gallery.fail(id);
                    self.gallery_edge.remove(&id);
                }
            }
            return;
        };
        self.next_version += 1;
        let version = self.next_version;
        let evicted = match tier {
            DecodeTier::Base => self.base.store(id, image, version),
            DecodeTier::Hires => self.hires.store(id, image, version),
            DecodeTier::Gallery => self.gallery.store(id, image, version),
        };
        if tier == DecodeTier::Gallery {
            for id in evicted {
                self.gallery_edge.remove(&id);
            }
        }
    }
}

impl Default for Session {
    fn default() -> Self {
        Self::new()
    }
}

/// Open the disposable cache, returning `None` on failure — the cache rebuilds,
/// so a corrupt or unopenable file must never block opening the folder.
fn open_cache(sidecar: &Path) -> Option<SharedCache> {
    SqliteCache::open(&sidecar.join(CACHE_FILE))
        .ok()
        .map(|c| Arc::new(Mutex::new(c)))
}

/// A pool builder seeded to reclaim ids by fingerprint, or an empty one for a
/// fresh folder.
fn seed_builder(snapshot: &Option<ProjectSnapshot>) -> PoolBuilder {
    match snapshot {
        Some(s) => PoolBuilder::seeded(s.seed_map(), s.next_id),
        None => PoolBuilder::default(),
    }
}

/// Rebuild the verdict store from `project.json` (state) plus `undo.log`
/// (stacks, folded not replayed). Empty when there's no saved project.
fn seed_cull(snapshot: &Option<ProjectSnapshot>, sidecar: &Path) -> Cull {
    let Some(snapshot) = snapshot else {
        return Cull::new();
    };
    let stacks = undo_log::load(&sidecar.join(UNDO_LOG_FILE)).unwrap_or_default();
    let undo = stacks
        .undo
        .into_iter()
        .map(UndoEntry::from_changes)
        .collect();
    let redo = stacks
        .redo
        .into_iter()
        .map(UndoEntry::from_changes)
        .collect();
    Cull::from_state(snapshot.verdicts(), undo, redo)
}

/// The default views array for a fresh project: one Grid view (§9b).
fn default_views() -> Vec<Value> {
    vec![serde_json::json!({ "kind": "Grid" })]
}

/// Make an absolute photo path relative to the project root for storage (§5).
/// Paths already relative (or outside the root) are stored as-is.
fn relativize(path: Option<&Path>, root: Option<&Path>) -> Option<PathBuf> {
    let path = path?;
    Some(match root {
        Some(root) => path.strip_prefix(root).unwrap_or(path).to_path_buf(),
        None => path.to_path_buf(),
    })
}
