//! `project.json` — the precious store. Verdicts, the id counter,
//! and the views array, behind a versioned DTO so the on-disk shape can evolve
//! without breaking old files. Owned state only; nothing derived is persisted.
//!
//! **Atomicity:** every save copies the current file to `project.json.bak`,
//! writes `project.json.tmp`, fsyncs, then atomically renames it over the
//! target. A crash leaves the old file or the new file, never a torn one; if
//! the main file is ever missing or unreadable, load falls back to the backup.
//!
//! Forward-compat: unknown `ViewKind`s round-trip untouched because `views` is
//! stored as raw JSON values and only parsed by name where a kind is known.

use std::collections::HashMap;
use std::fs::File;
use std::io::Write;
use std::path::{Path, PathBuf};

use dcs_domain::crops::CropEdit;
use dcs_domain::cull::AcceptState;
use dcs_domain::fingerprint::ContentFingerprint;
use dcs_domain::photo::PhotoId;
use dcs_domain::tag::{Tag, TagId};
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// The on-disk schema version. Bump only with a matching upgrade path in
/// `load`; a file from a newer, unknown version is refused, never guessed.
pub const CURRENT_VERSION: u32 = 1;

const PROJECT_FILE: &str = "project.json";
const BACKUP_FILE: &str = "project.json.bak";
const TEMP_FILE: &str = "project.json.tmp";

/// Errors reading or writing the project file. The domain never sees these;
/// they carry their own context, never a bare `io::Error`.
#[derive(Debug, Error)]
pub enum PersistError {
    #[error("project i/o error: {0}")]
    Io(#[from] std::io::Error),
    #[error("project json error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("unsupported project version {0} (this build understands {CURRENT_VERSION})")]
    UnsupportedVersion(u32),
    #[error("corrupt project file: {0}")]
    Corrupt(String),
}

/// One persisted photo: stable id, content identity, owned verdict, and the
/// last-known relative paths. Every known photo is recorded (not just culled
/// ones) so a rename-in-place reclaims its id even when unreviewed, and so a
/// file that goes missing keeps its state and can be shown as a placeholder.
/// Paths are relative to the project root — the folder is portable.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PhotoRecord {
    pub id: PhotoId,
    pub fingerprint: ContentFingerprint,
    pub verdict: AcceptState,
    /// Owned tag assignments for this photo, by id. Empty (the common case)
    /// isn't written, so untagged photos stay compact.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<TagId>,
    /// Owned crop + straighten, when the photo is cropped. Absent (the common
    /// case) isn't written, so uncropped photos stay compact. Additive — a
    /// pre-crop project file loads with this `None`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub crop: Option<CropEdit>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub jpeg: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub raw: Option<PathBuf>,
}

/// Owned project settings. Reserved fields are persisted now even when
/// unset so the schema is stable: the shoot timezone is freeze-critical (a
/// crystallized tag made under the wrong zone is wrong forever).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ProjectConfig {
    /// IANA shoot (display) timezone (e.g. `"Europe/Rome"`). Times are shown and
    /// grouped in this zone. `None` until the user picks (falls back to system).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shoot_zone: Option<String>,
    /// IANA camera timezone: the zone the camera clock was set to, used to anchor
    /// a naive EXIF time when the photo carries no `OffsetTimeOriginal`. `None`
    /// falls back to system. Freeze-critical alongside `shoot_zone`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub camera_zone: Option<String>,
    /// Grid cell size in logical pixels — the Grid view's zoom.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub grid_zoom: Option<f32>,
    /// Whether the grid paints the burst overlay (span accents + labels). A view
    /// preference like `grid_zoom`, not a derived value — `None` means the
    /// default (off). The derivation tuning (gap/min) stays ephemeral.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub show_bursts: Option<bool>,
    /// Whether AI search is enabled for this project. `None`/`Some(false)` = off
    /// (opt-in). A per-project preference like `show_bursts`, persisted, not
    /// derived — the embeddings themselves stay in the disposable cache.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ai_search_enabled: Option<bool>,
}

/// The app-facing payload: what `dcs-app` hands down to save and gets back on
/// load. Derived state is reconstructed by the app, never stored here.
#[derive(Debug, Clone, PartialEq)]
pub struct ProjectSnapshot {
    pub photos: Vec<PhotoRecord>,
    /// The monotonic id counter to persist (max assigned id + 1), so fresh
    /// photos never collide with reclaimed ones after reopen.
    pub next_id: u32,
    /// Tag definitions (the only persisted user-created structure). Assignments
    /// live per-photo on `PhotoRecord::tags`.
    pub tags: Vec<Tag>,
    /// The monotonic tag-id counter to persist (max assigned tag id + 1).
    pub next_tag_id: u32,
    /// Views as raw JSON values; unknown kinds survive a round-trip verbatim.
    pub views: Vec<serde_json::Value>,
    /// The monotonic view-id counter to persist (max assigned view id + 1), so a
    /// new board never reuses a retired view's id.
    pub next_view_id: u32,
    /// Owned project settings.
    pub config: ProjectConfig,
}

impl ProjectSnapshot {
    /// The `fingerprint → PhotoId` map used to seed `PoolBuilder` on reopen.
    pub fn seed_map(&self) -> HashMap<ContentFingerprint, PhotoId> {
        self.photos.iter().map(|p| (p.fingerprint, p.id)).collect()
    }

    /// The `PhotoId → verdict` pairs used to seed the verdict store.
    pub fn verdicts(&self) -> Vec<(PhotoId, AcceptState)> {
        self.photos.iter().map(|p| (p.id, p.verdict)).collect()
    }

    /// The `PhotoId → CropEdit` pairs used to seed the crop store. Only cropped
    /// photos appear.
    pub fn crops(&self) -> Vec<(PhotoId, CropEdit)> {
        self.photos
            .iter()
            .filter_map(|p| p.crop.map(|c| (p.id, c)))
            .collect()
    }

    /// The tag definitions used to seed the tag store.
    pub fn tag_defs(&self) -> Vec<Tag> {
        self.tags.clone()
    }

    /// The `PhotoId → [TagId]` assignments used to seed the tag store. Only
    /// tagged photos appear.
    pub fn tag_assignments(&self) -> Vec<(PhotoId, Vec<TagId>)> {
        self.photos
            .iter()
            .filter(|p| !p.tags.is_empty())
            .map(|p| (p.id, p.tags.clone()))
            .collect()
    }
}

/// Reads and writes the project sidecar within a `.dcs/` directory.
pub trait ProjectStore {
    /// Load the project from `dir/project.json`, falling back to the backup if
    /// the main file is missing or unreadable. `Ok(None)` means no project
    /// exists yet (a fresh folder).
    fn load(&self, dir: &Path) -> Result<Option<ProjectSnapshot>, PersistError>;

    /// Atomically save the snapshot to `dir/project.json`, rotating the prior
    /// file to `project.json.bak`. Creates `dir` if needed.
    fn save(&self, dir: &Path, snapshot: &ProjectSnapshot) -> Result<(), PersistError>;
}

/// The JSON-backed `ProjectStore`.
pub struct JsonProjectStore;

impl ProjectStore for JsonProjectStore {
    fn load(&self, dir: &Path) -> Result<Option<ProjectSnapshot>, PersistError> {
        let main = dir.join(PROJECT_FILE);
        let backup = dir.join(BACKUP_FILE);
        match read_snapshot(&main) {
            Ok(Some(s)) => Ok(Some(s)),
            Ok(None) => read_snapshot(&backup), // main absent → try the backup
            // A newer, unknown version is refused, never guessed: falling back to
            // a stale backup here would load old owned state and then clobber the
            // newer file on the next save. Only genuine corruption falls back.
            Err(PersistError::UnsupportedVersion(v)) => Err(PersistError::UnsupportedVersion(v)),
            Err(_) if backup.exists() => read_snapshot(&backup), // main torn → backup
            Err(e) => Err(e),
        }
    }

    fn save(&self, dir: &Path, snapshot: &ProjectSnapshot) -> Result<(), PersistError> {
        std::fs::create_dir_all(dir)?;
        let main = dir.join(PROJECT_FILE);
        let dto = ProjectDto::from_snapshot(snapshot);
        let bytes = serde_json::to_vec_pretty(&dto)?;
        // Back up the last-good file before replacing it (best-effort: a missing
        // main just means there's nothing to back up yet).
        if main.exists() {
            std::fs::copy(&main, dir.join(BACKUP_FILE))?;
        }
        atomic_write(dir, &main, &bytes)
    }
}

/// Handle to a background save worker: `project.json` writes (serialize +
/// fsync) run off the caller's thread, so a UI thread never blocks on disk.
/// Requests are processed serially in order — there is exactly one writer.
/// Dropping the handle joins the worker, so every queued save lands before
/// the process can exit under it.
pub struct SaveWorker {
    tx: Option<crossbeam_channel::Sender<(PathBuf, ProjectSnapshot)>>,
    results: crossbeam_channel::Receiver<Result<(), PersistError>>,
    worker: Option<std::thread::JoinHandle<()>>,
    /// Requests sent but with no result received yet. `Cell`: the handle lives
    /// on one thread; the worker only touches the channels.
    outstanding: std::cell::Cell<usize>,
}

/// Spawn the save worker owning its own store instance.
pub fn spawn_saver<S: ProjectStore + Send + 'static>(store: S) -> SaveWorker {
    let (tx, rx) = crossbeam_channel::unbounded::<(PathBuf, ProjectSnapshot)>();
    let (res_tx, results) = crossbeam_channel::unbounded();
    let worker = std::thread::spawn(move || {
        while let Ok((dir, snapshot)) = rx.recv() {
            if res_tx.send(store.save(&dir, &snapshot)).is_err() {
                break; // handle dropped: nobody will read further results
            }
        }
    });
    SaveWorker {
        tx: Some(tx),
        results,
        worker: Some(worker),
        outstanding: std::cell::Cell::new(0),
    }
}

impl SaveWorker {
    /// Queue a save without waiting. `false` when the worker is gone (it
    /// panicked); the caller falls back to a direct write.
    pub fn request(&self, dir: PathBuf, snapshot: ProjectSnapshot) -> bool {
        let sent = self
            .tx
            .as_ref()
            .is_some_and(|tx| tx.send((dir, snapshot)).is_ok());
        if sent {
            self.outstanding.set(self.outstanding.get() + 1);
        }
        sent
    }

    /// True while queued saves haven't reported back yet. A caller deciding
    /// "is everything on disk?" must consider this alongside its own dirty
    /// flag — an optimistically-cleaned state may still be in flight.
    pub fn has_pending(&self) -> bool {
        self.outstanding.get() > 0
    }

    /// Results of finished saves, in request order. Non-blocking.
    pub fn poll(&self) -> Vec<Result<(), PersistError>> {
        let drained: Vec<_> = self.results.try_iter().collect();
        self.outstanding
            .set(self.outstanding.get().saturating_sub(drained.len()));
        drained
    }

    /// Queue a save and wait for it to finish. Results of earlier queued saves
    /// are absorbed (this snapshot supersedes theirs). `None` when the worker
    /// is gone — including dying mid-drain, where an earlier save's result
    /// must never pass for this one's — the caller falls back to a direct
    /// write.
    pub fn save_blocking(
        &self,
        dir: PathBuf,
        snapshot: ProjectSnapshot,
    ) -> Option<Result<(), PersistError>> {
        if !self.request(dir, snapshot) {
            return None;
        }
        let mut last = None;
        while self.outstanding.get() > 0 {
            match self.results.recv() {
                Ok(result) => {
                    self.outstanding.set(self.outstanding.get() - 1);
                    last = Some(result);
                }
                Err(_) => return None, // worker died before our result arrived
            }
        }
        last
    }
}

/// Join the worker on drop: an in-flight `project.json` write finishes before
/// the handle's owner can proceed to exit — the queue is never abandoned
/// mid-write.
impl Drop for SaveWorker {
    fn drop(&mut self) {
        self.tx = None; // close the queue so the worker's recv loop ends
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

/// The versioned on-disk envelope. Distinct from `ProjectSnapshot` so the wire
/// shape can change independently of the app-facing type.
#[derive(Serialize, Deserialize)]
struct ProjectDto {
    version: u32,
    photos: Vec<PhotoRecord>,
    next_id: u32,
    /// Tag defs. Defaulted so a pre-tags project file (none written) loads with
    /// no tags rather than failing — additive, no version bump needed.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    tags: Vec<Tag>,
    #[serde(default)]
    next_tag_id: u32,
    #[serde(default)]
    views: Vec<serde_json::Value>,
    /// Defaulted so a pre-board project file (none written) loads as 0 —
    /// additive, no version bump.
    #[serde(default)]
    next_view_id: u32,
    #[serde(default)]
    config: ProjectConfig,
}

impl ProjectDto {
    fn from_snapshot(s: &ProjectSnapshot) -> Self {
        ProjectDto {
            version: CURRENT_VERSION,
            photos: s.photos.clone(),
            next_id: s.next_id,
            tags: s.tags.clone(),
            next_tag_id: s.next_tag_id,
            views: s.views.clone(),
            next_view_id: s.next_view_id,
            config: s.config.clone(),
        }
    }

    fn into_snapshot(self) -> ProjectSnapshot {
        ProjectSnapshot {
            photos: self.photos,
            next_id: self.next_id,
            tags: self.tags,
            next_tag_id: self.next_tag_id,
            views: self.views,
            next_view_id: self.next_view_id,
            config: self.config,
        }
    }
}

/// Read and validate one project file. `Ok(None)` when the file doesn't exist.
fn read_snapshot(path: &Path) -> Result<Option<ProjectSnapshot>, PersistError> {
    let bytes = match std::fs::read(path) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e.into()),
    };
    let dto: ProjectDto = serde_json::from_slice(&bytes)?;
    if dto.version > CURRENT_VERSION {
        return Err(PersistError::UnsupportedVersion(dto.version));
    }
    Ok(Some(dto.into_snapshot()))
}

/// Write `bytes` to `path` atomically: tmp file → fsync → rename, then fsync
/// the directory so the rename itself is durable.
fn atomic_write(dir: &Path, path: &Path, bytes: &[u8]) -> Result<(), PersistError> {
    let tmp: PathBuf = dir.join(TEMP_FILE);
    {
        let mut file = File::create(&tmp)?;
        file.write_all(bytes)?;
        file.sync_all()?;
    }
    std::fs::rename(&tmp, path)?;
    // Directory fsync makes the rename durable; not all platforms permit
    // opening a dir for sync, so a failure here is non-fatal.
    if let Ok(dir_handle) = File::open(dir) {
        let _ = dir_handle.sync_all();
    }
    Ok(())
}
