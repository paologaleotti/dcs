//! Session — the conductor for the open-and-view slice. Owns the pool being
//! assembled, the decode pipeline, and the thumbnail caches. The UI talks only
//! to this; it never reaches into `dcs-io`.
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

mod display;
mod edit;
mod layout;
mod store;
mod tag;

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use dcs_domain::grouping::{Axis, DerivedGroup, GroupKind, TimeGranularity};
use dcs_domain::pairing::PoolBuilder;
use dcs_domain::photo::PhotoId;
use dcs_domain::sort::Sort;
use dcs_io::cache::{SharedCache, SqliteCache};
use dcs_io::export::ExportHandle;
use dcs_io::imaging::{ThumbDecoder, ThumbDecoderPool};
use dcs_io::lock::{self, LockOutcome, ProjectLock};
use dcs_io::persistence::{
    JsonProjectStore, PersistError, PhotoRecord, ProjectConfig, ProjectSnapshot, ProjectStore,
};
use dcs_io::recents::Recents;
use dcs_io::source::{ScanHandle, scan};
use dcs_io::undo_log::{self, UndoLog};
use serde_json::Value;
use thiserror::Error;

use crate::cull::Cull;
use crate::export::ExportStatus;
use crate::history::History;
use crate::selection::Selection;
use crate::tags::TagStore;
use crate::thumb_cache::ThumbCache;
use crate::util::decode_key;

/// The `.dcs/` sidecar directory name.
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

/// RAM budget for the base pixel cache (~256 MB ≈ 1,300 thumbnails). It holds
/// the working set, not the whole folder: every decode also writes the thumbnail
/// to the on-disk cache, so an evicted off-screen thumbnail reloads with a fast
/// cached-blob decode rather than living in RAM. O(1) LRU eviction makes this
/// tight budget cheap to churn on a large folder.
const BASE_CACHE_BYTES: u64 = 256_000_000;

/// RAM budget for the hi-res cache. Small on purpose: only viewport cells while
/// zoomed live here, and it is dropped entirely on zoom-out.
const HIRES_CACHE_BYTES: u64 = 384_000_000;

/// RAM budget for the gallery cache. Holds the current preview frame plus its
/// preloaded neighbours and a little history; large enough for one full 1:1
/// decode of a big sensor (~180 MB) without starving on its own insert.
const GALLERY_CACHE_BYTES: u64 = 256_000_000;

/// Longest-side cap for the default (non-zoom) gallery preview. Judging a photo
/// never needs the full sensor resolution — like Lightroom or Photo Mechanic we
/// decode a screen-class preview and defer the 1:1 read to an explicit zoom.
/// This keeps a navigation session's RAM bounded to a handful of small frames
/// instead of accumulating full-window decodes. 3200 px is near-sharp on a 4K
/// panel at ~27 MB per RGBA frame.
const GALLERY_PREVIEW_EDGE: u32 = 3200;

/// Longest-side pixel cap for a 1:1 gallery decode. Bounds both RAM and the GPU
/// texture size (most backends cap a single texture near 8–16k); images larger
/// than this show at this resolution, which is still far past screen-sharp.
const GALLERY_FULL_EDGE: u32 = 8192;

/// Cap on base decodes kept in flight by the background fill. Enough to keep
/// the decode pool fed, low enough that viewport requests (issued first each
/// frame) aren't stuck behind a long backlog.
const BG_FILL_INFLIGHT: usize = 16;

/// Background import progress: grid thumbnails warmed into the disk cache out
/// of the displayable total. Drives the status-bar progress bar.
#[derive(Debug, Clone, Copy)]
pub struct ImportProgress {
    pub done: usize,
    pub total: usize,
}

/// Minimal per-cell facts the grid needs to paint, without cloning a `Photo`.
#[derive(Debug, Clone, Copy)]
pub struct CellInfo {
    pub id: PhotoId,
    pub raw_only: bool,
    /// Owned verdict for the verdict glyph + rejected dimming.
    pub state: dcs_domain::cull::AcceptState,
    /// Whether this cell is in the current selection (grease-pencil outline).
    pub selected: bool,
    /// The file is absent on disk — render a placeholder + `missing` badge.
    pub missing: bool,
}

/// Capture time for the gallery caption. `adjusted` is the time in the travel
/// (display) zone; `offset` is that zone's `±HH:MM` offset for the instant (so
/// the caption can mark the time as zone-adjusted); `shot` is the raw EXIF shot
/// time, present only when it differs from `adjusted`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CaptionTime {
    pub adjusted: String,
    pub offset: String,
    pub shot: Option<String>,
}

/// Verdict view toggle — a session display setting, not the full chip filter
/// system. `Unreviewed` is the working filter while culling.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum VerdictFilter {
    #[default]
    All,
    Unreviewed,
    Accepted,
    Rejected,
}

/// A derived group as the grid sees it after the verdict filter: its title +
/// kind, where its surviving cells begin in the visible display order, and how
/// many show out of its total. Empty-after-filter groups are omitted, so the UI
/// never renders a header with no cells.
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
    /// Derived grouping over the whole pool. Recomputed when the pool, axis,
    /// sort, or shoot zone changes; never persisted.
    groups: Vec<DerivedGroup>,
    /// Per-group spans over the *visible* order, for the grid's headers.
    visible_groups: Vec<VisibleGroup>,
    /// Active grouping axis + sort — derived display settings, not owned.
    axis: Axis,
    sort: Sort,
    /// `Auto` resolved against the current data, cached at regroup so the toolbar
    /// label doesn't re-scan the pool (and re-read the system zone) every frame.
    resolved_gran: Option<TimeGranularity>,
    /// Owned verdict store. Reset per folder (ids restart).
    cull: Cull,
    /// Owned tag store: defs + photo↔tag assignments. Reset per folder.
    tags: TagStore,
    /// Unified durable undo/redo timeline over verdict + tag mutations.
    history: History,
    /// Ephemeral focus cursor + selection.
    sel: Selection,
    filter: VerdictFilter,
    scan: Option<ScanHandle>,
    decoder: ThumbDecoderPool,
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
    /// Displayable photos whose grid thumbnail has been warmed into the disk
    /// cache — the import-progress numerator. Seeded from the disk cache when a
    /// scan settles so a reopened folder resumes its warm-up instead of
    /// restarting it, then grown as background decodes land. Monotonic within a
    /// folder session: it isn't pruned if the disk cache later evicts a blob,
    /// which only happens for a folder larger than the cache can hold — a regime
    /// where re-warming would just thrash, so "imported once" is the right
    /// meaning and the fill correctly leaves those photos alone.
    imported: HashSet<PhotoId>,
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
    /// Views as raw JSON, round-tripped on save so unknown kinds survive.
    views: Vec<Value>,
    /// Owned project settings: shoot zone, grid zoom.
    config: ProjectConfig,
    /// The open folder root; needed to re-scan and to relativize stored paths.
    root: Option<PathBuf>,
    /// Persisted photos from the loaded project, retained until the scan
    /// finishes so absent files can be reconciled into missing placeholders.
    loaded_records: Vec<PhotoRecord>,
    /// Single-writer lock. `None` when no folder is open.
    project_lock: Option<ProjectLock>,
    /// Another live instance holds the lock: viewing is allowed, writing is not.
    read_only: bool,
    /// App-global recent-projects list, persisted outside any project.
    recents: Recents,
    recents_path: Option<PathBuf>,
    /// Owned state has changed since the last save.
    dirty: bool,
    /// Command-palette most-recently-used action ids, newest first. Drives the
    /// palette's default order; ephemeral, not persisted in v1.
    mru: Vec<&'static str>,
    /// Running export executor, polled in `tick`. `None` when idle.
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
            tags: TagStore::new(),
            history: History::new(),
            sel: Selection::new(),
            filter: VerdictFilter::All,
            scan: None,
            decoder: ThumbDecoderPool::new(),
            base: ThumbCache::new(BASE_CACHE_BYTES),
            hires: ThumbCache::new(HIRES_CACHE_BYTES),
            gallery: ThumbCache::new(GALLERY_CACHE_BYTES),
            gallery_edge: HashMap::new(),
            bg_cursor: 0,
            imported: HashSet::new(),
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
        self.imported.clear();
        self.epoch += 1;
        self.dirty = false;

        let sidecar = root.join(SIDECAR_DIR);
        let _ = std::fs::create_dir_all(&sidecar);
        self.cache = open_cache(&sidecar);

        // Release the previous folder's lock *before* acquiring — including
        // when reopening or re-scanning the same folder, where our own fresh
        // lock would otherwise be misread as a live second instance.
        self.project_lock = None;
        // A live second instance opens read-only; a stale/absent lock is
        // reclaimed.
        let (project_lock, outcome) = ProjectLock::acquire(&sidecar, lock::DEFAULT_STALE);
        self.read_only = outcome == LockOutcome::HeldByOther;
        self.project_lock = Some(project_lock);

        // project.json is the authoritative verdict state; undo.log only
        // reconstructs the stacks. A missing/fresh folder yields an empty pool
        // builder and an empty Cull.
        let snapshot = self.store.load(&sidecar).ok().flatten();
        self.builder = seed_builder(&snapshot);
        self.cull = seed_cull(&snapshot);
        self.tags = seed_tags(&snapshot);
        self.history = seed_history(&snapshot, &sidecar);
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
            // their state is preserved and they reanimate if the file returns.
            self.reconcile_missing();
            // Baseline the import from whatever the disk cache already holds, so
            // a reopened folder resumes its warm-up instead of starting over.
            self.seed_imported();
            // The grid reveals now that the order is settled; rewind the fill so
            // it walks that final order from the top — already-imported thumbs
            // skip instantly, the rest fill in display order.
            self.bg_cursor = 0;
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

    pub fn is_scanning(&self) -> bool {
        self.scan.is_some()
    }

    /// True when a folder is open (so the UI can enable "Rescan").
    pub fn has_folder(&self) -> bool {
        self.root.is_some()
    }

    /// Re-scan the open folder: save first so the reload restores owned state,
    /// then reopen. New files appear, returned files reanimate, and removed
    /// files become missing placeholders.
    pub fn rescan(&mut self) {
        if let Some(root) = self.root.clone() {
            let _ = self.save_if_dirty();
            self.open_folder(root);
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

/// Rebuild the verdict store from `project.json` (authoritative state). Empty
/// when there's no saved project.
fn seed_cull(snapshot: &Option<ProjectSnapshot>) -> Cull {
    match snapshot {
        Some(s) => Cull::from_verdicts(s.verdicts()),
        None => Cull::new(),
    }
}

/// Rebuild the tag store from `project.json`: defs + per-photo assignments + the
/// id counter. Empty when there's no saved project.
fn seed_tags(snapshot: &Option<ProjectSnapshot>) -> TagStore {
    match snapshot {
        Some(s) => TagStore::from_state(s.tag_defs(), s.tag_assignments(), s.next_tag_id),
        None => TagStore::new(),
    }
}

/// Rebuild the unified undo timeline from `undo.log` (folded, never replayed —
/// `project.json` already reflects it). Empty when there's no saved project.
fn seed_history(snapshot: &Option<ProjectSnapshot>, sidecar: &Path) -> History {
    if snapshot.is_none() {
        return History::new();
    }
    let stacks = undo_log::load(&sidecar.join(UNDO_LOG_FILE)).unwrap_or_default();
    History::from_stacks(stacks.undo, stacks.redo)
}

/// The default views array for a fresh project: one Grid view.
fn default_views() -> Vec<Value> {
    vec![serde_json::json!({ "kind": "Grid" })]
}

/// Make an absolute photo path relative to the project root for storage. Paths
/// already relative (or outside the root) are stored as-is.
fn relativize(path: Option<&Path>, root: Option<&Path>) -> Option<PathBuf> {
    let path = path?;
    Some(match root {
        Some(root) => path.strip_prefix(root).unwrap_or(path).to_path_buf(),
        None => path.to_path_buf(),
    })
}
