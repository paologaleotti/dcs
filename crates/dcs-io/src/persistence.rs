//! `project.json` — the precious store (§5, §10b). Verdicts, the id counter,
//! and the views array, behind a versioned DTO so the on-disk shape can evolve
//! without breaking old files. Owned state only; nothing derived is persisted.
//!
//! **Atomicity:** every save copies the current file to `project.json.bak`,
//! writes `project.json.tmp`, fsyncs, then atomically renames it over the
//! target. A crash leaves the old file or the new file, never a torn one; if
//! the main file is ever missing or unreadable, load falls back to the backup.
//!
//! Forward-compat: unknown `ViewKind`s round-trip untouched because `views` is
//! stored as raw JSON values and only parsed by name where a kind is known
//! (spec §9b, open Q#6).

use std::collections::HashMap;
use std::fs::File;
use std::io::Write;
use std::path::{Path, PathBuf};

use dcs_domain::cull::AcceptState;
use dcs_domain::fingerprint::ContentFingerprint;
use dcs_domain::photo::PhotoId;
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// The on-disk schema version. Bump only with a matching upgrade path in
/// `load`; a file from a newer, unknown version is refused, never guessed.
pub const CURRENT_VERSION: u32 = 1;

const PROJECT_FILE: &str = "project.json";
const BACKUP_FILE: &str = "project.json.bak";
const TEMP_FILE: &str = "project.json.tmp";

/// Errors reading or writing the project file. The domain never sees these;
/// they carry their own context, never a bare `io::Error` (CLAUDE.md).
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
/// file that goes missing keeps its state and can be shown as a placeholder
/// (§4, §10b). Paths are relative to the project root — the folder is portable.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PhotoRecord {
    pub id: PhotoId,
    pub fingerprint: ContentFingerprint,
    pub verdict: AcceptState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub jpeg: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub raw: Option<PathBuf>,
}

/// Owned project settings (§4). Reserved fields are persisted now even when
/// unset so the schema is stable: the shoot timezone is freeze-critical (a
/// crystallized tag made under the wrong zone is wrong forever, open Q#5).
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
    /// Grid cell size in logical pixels — the Grid view's zoom (§9b).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub grid_zoom: Option<f32>,
}

/// The app-facing payload: what `dcs-app` hands down to save and gets back on
/// load. Derived state is reconstructed by the app, never stored here.
#[derive(Debug, Clone, PartialEq)]
pub struct ProjectSnapshot {
    pub photos: Vec<PhotoRecord>,
    /// The monotonic id counter to persist (max assigned id + 1), so fresh
    /// photos never collide with reclaimed ones after reopen (§10b).
    pub next_id: u32,
    /// Views as raw JSON values; unknown kinds survive a round-trip verbatim.
    pub views: Vec<serde_json::Value>,
    /// Owned project settings (§4).
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

/// The versioned on-disk envelope. Distinct from `ProjectSnapshot` so the wire
/// shape can change independently of the app-facing type.
#[derive(Serialize, Deserialize)]
struct ProjectDto {
    version: u32,
    photos: Vec<PhotoRecord>,
    next_id: u32,
    #[serde(default)]
    views: Vec<serde_json::Value>,
    #[serde(default)]
    config: ProjectConfig,
}

impl ProjectDto {
    fn from_snapshot(s: &ProjectSnapshot) -> Self {
        ProjectDto {
            version: CURRENT_VERSION,
            photos: s.photos.clone(),
            next_id: s.next_id,
            views: s.views.clone(),
            config: s.config.clone(),
        }
    }

    fn into_snapshot(self) -> ProjectSnapshot {
        ProjectSnapshot {
            photos: self.photos,
            next_id: self.next_id,
            views: self.views,
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
