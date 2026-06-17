//! App-global recent-projects list, persisted to `~/.dcs/recents.json`. This is
//! the one store that is NOT per-project — the spec's three stores all live in
//! a project's `.dcs/`; this tracks folders across projects so the menu can
//! offer "Open Recent". Storage is intentionally minimal and disposable — a
//! corrupt file just resets the list.

use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// How many recent projects to remember.
pub const MAX_RECENTS: usize = 10;

/// The recent-projects list, most-recent first.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Recents {
    #[serde(default)]
    pub projects: Vec<PathBuf>,
}

impl Recents {
    /// Promote `path` to the front, removing any existing entry, capped to
    /// `MAX_RECENTS`.
    pub fn record(&mut self, path: PathBuf) {
        self.projects.retain(|p| p != &path);
        self.projects.insert(0, path);
        self.projects.truncate(MAX_RECENTS);
    }

    pub fn clear(&mut self) {
        self.projects.clear();
    }

    /// Drop entries whose folder no longer exists, so the menu never offers a
    /// dead path (clicking which would recreate an empty folder).
    pub fn retain_existing(&mut self) {
        self.projects.retain(|p| p.exists());
    }
}

/// `$HOME/.dcs/recents.json` (USERPROFILE, then HOMEDRIVE+HOMEPATH on Windows).
/// `None` when no home directory can be resolved.
pub fn recents_path() -> Option<PathBuf> {
    Some(home_dir()?.join(".dcs").join("recents.json"))
}

/// Load the list, returning an empty one on a missing or corrupt file — recents
/// are convenience, never precious.
pub fn load(path: &Path) -> Recents {
    std::fs::read(path)
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default()
}

/// Persist the list, creating the parent directory if needed. Written via a
/// temp file + rename so a crash can't leave a half-written list.
pub fn save(path: &Path, recents: &Recents) -> io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let bytes = serde_json::to_vec_pretty(recents).map_err(io::Error::other)?;
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, &bytes)?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

fn home_dir() -> Option<PathBuf> {
    for key in ["HOME", "USERPROFILE"] {
        if let Some(value) = std::env::var_os(key)
            && !value.is_empty()
        {
            return Some(PathBuf::from(value));
        }
    }
    // Windows fallback: HOMEDRIVE + HOMEPATH.
    let drive = std::env::var_os("HOMEDRIVE")?;
    let path = std::env::var_os("HOMEPATH")?;
    let mut home = PathBuf::from(drive);
    home.push(path);
    Some(home)
}
