//! Owned verdict store. Verdict is owned, not derived: every change is a
//! `Command`, every change reversible. This module is now just the map of
//! `PhotoId → AcceptState` plus the primitives to apply and reverse a verdict
//! delta; the unified undo/redo timeline (verdict *and* tag mutations) lives in
//! [`crate::history::History`], so one stack reverses anything.

use std::collections::{HashMap, HashSet};

use dcs_domain::command::VerdictChange;
use dcs_domain::cull::AcceptState;
use dcs_domain::photo::PhotoId;

/// Accepted/rejected tallies for the status bar. Unreviewed is derived
/// by the caller from the pool size — absent photos are `Unreviewed`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct VerdictCounts {
    pub accepted: usize,
    pub rejected: usize,
}

/// The owned verdict store, keyed by `PhotoId`. Identity is reconciled to the
/// content fingerprint by the persistence layer at load/save, not here, so the
/// key stays stable while verdicts survive a rename-in-place. Absent ==
/// `Unreviewed`, so the map only ever holds reviewed photos.
#[derive(Default)]
pub struct Cull {
    verdicts: HashMap<PhotoId, AcceptState>,
}

impl Cull {
    pub fn new() -> Self {
        Cull::default()
    }

    /// Rebuild the store from persisted `project.json` verdicts on reopen.
    /// `Unreviewed` entries are dropped so the map stays small.
    pub fn from_verdicts(verdicts: impl IntoIterator<Item = (PhotoId, AcceptState)>) -> Self {
        let mut map = HashMap::new();
        for (id, state) in verdicts {
            if state != AcceptState::Unreviewed {
                map.insert(id, state);
            }
        }
        Cull { verdicts: map }
    }

    /// Verdict for a photo; absent = `Unreviewed`.
    pub fn state(&self, id: PhotoId) -> AcceptState {
        self.verdicts.get(&id).copied().unwrap_or_default()
    }

    /// Accepted/rejected tallies.
    pub fn counts(&self) -> VerdictCounts {
        let mut c = VerdictCounts::default();
        for state in self.verdicts.values() {
            match state {
                AcceptState::Accepted => c.accepted += 1,
                AcceptState::Rejected => c.rejected += 1,
                AcceptState::Unreviewed => {}
            }
        }
        c
    }

    /// Forget photos entirely: drop their verdicts. Used when the user prunes
    /// missing files; scrubbing the undo timeline is the history's job.
    pub fn forget(&mut self, ids: &HashSet<PhotoId>) {
        self.verdicts.retain(|id, _| !ids.contains(id));
    }

    /// Apply a `SetState` over `ids` to `target`, returning the reversible
    /// deltas — deduped to unique photos, with no-op photos omitted. An
    /// empty result means nothing moved.
    pub fn apply_set_state(&mut self, ids: &[PhotoId], target: AcceptState) -> Vec<VerdictChange> {
        let mut seen = HashSet::new();
        let mut changes = Vec::new();
        for &id in ids {
            if !seen.insert(id) {
                continue; // dedup to unique photos before the undo entry
            }
            let before = self.state(id);
            if before == target {
                continue; // no-op for this photo records nothing
            }
            self.set_raw(id, target);
            changes.push((id, before, target));
        }
        changes
    }

    /// Re-apply a recorded delta set (redo): each photo to its *after* state.
    pub fn apply(&mut self, changes: &[VerdictChange]) {
        for &(id, _before, after) in changes {
            self.set_raw(id, after);
        }
    }

    /// Reverse a recorded delta set (undo): each photo to its *before* state.
    pub fn revert(&mut self, changes: &[VerdictChange]) {
        for &(id, before, _after) in changes {
            self.set_raw(id, before);
        }
    }

    fn set_raw(&mut self, id: PhotoId, state: AcceptState) {
        if state == AcceptState::Unreviewed {
            self.verdicts.remove(&id); // absent == Unreviewed, keep the map small
        } else {
            self.verdicts.insert(id, state);
        }
    }
}
