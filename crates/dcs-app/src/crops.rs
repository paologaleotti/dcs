//! Owned crop store. Crop+straighten is owned, not derived: every change is a
//! `Command`, every change reversible. Like [`crate::cull::Cull`], this is the
//! map of `PhotoId → CropEdit` plus the primitives to apply and reverse a crop
//! delta; the unified undo/redo timeline lives in [`crate::history::History`].

use std::collections::{HashMap, HashSet};

use dcs_domain::command::CropChange;
use dcs_domain::crops::CropEdit;
use dcs_domain::photo::PhotoId;

/// The owned crop store, keyed by `PhotoId`. Absent == uncropped, so the map
/// only ever holds cropped photos. Identity is reconciled to the content
/// fingerprint by the persistence layer at load/save, so a rename-in-place keeps
/// its crop.
#[derive(Default)]
pub struct CropStore {
    crops: HashMap<PhotoId, CropEdit>,
}

impl CropStore {
    pub fn new() -> Self {
        CropStore::default()
    }

    /// Rebuild the store from persisted `project.json` crops on reopen.
    pub fn from_crops(crops: impl IntoIterator<Item = (PhotoId, CropEdit)>) -> Self {
        CropStore {
            crops: crops.into_iter().collect(),
        }
    }

    /// The crop for a photo, or `None` when uncropped.
    pub fn crop_of(&self, id: PhotoId) -> Option<CropEdit> {
        self.crops.get(&id).copied()
    }

    /// Whether a photo has a crop.
    pub fn has_crop(&self, id: PhotoId) -> bool {
        self.crops.contains_key(&id)
    }

    /// How many photos are cropped.
    pub fn count(&self) -> usize {
        self.crops.len()
    }

    /// Forget photos entirely: drop their crops. Used when pruning missing files;
    /// scrubbing the undo timeline is the history's job.
    pub fn forget(&mut self, ids: &HashSet<PhotoId>) {
        self.crops.retain(|id, _| !ids.contains(id));
    }

    /// Apply a `SetCrop` over `ids` to `target` (`None` clears), returning the
    /// reversible deltas — deduped to unique photos, with no-op photos omitted.
    /// A `Some(edit)` that is itself a no-op (identity) is normalized to `None`,
    /// so "crop back to the full frame" clears rather than stores an identity.
    pub fn apply_set_crop(&mut self, ids: &[PhotoId], target: Option<CropEdit>) -> Vec<CropChange> {
        let target = target.filter(|e| !e.is_noop());
        let mut seen = HashSet::new();
        let mut changes = Vec::new();
        for &id in ids {
            if !seen.insert(id) {
                continue; // dedup to unique photos before the undo entry
            }
            let before = self.crop_of(id);
            if before == target {
                continue; // no-op for this photo records nothing
            }
            self.set_raw(id, target);
            changes.push((id, before, target));
        }
        changes
    }

    /// Re-apply a recorded delta set (redo): each photo to its *after* edit.
    pub fn apply(&mut self, changes: &[CropChange]) {
        for &(id, _before, after) in changes {
            self.set_raw(id, after);
        }
    }

    /// Reverse a recorded delta set (undo): each photo to its *before* edit.
    pub fn revert(&mut self, changes: &[CropChange]) {
        for &(id, before, _after) in changes {
            self.set_raw(id, before);
        }
    }

    fn set_raw(&mut self, id: PhotoId, edit: Option<CropEdit>) {
        match edit {
            Some(e) => {
                self.crops.insert(id, e);
            }
            None => {
                self.crops.remove(&id); // absent == uncropped, keep the map small
            }
        }
    }
}
