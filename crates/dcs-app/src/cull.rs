//! Owned verdict store plus the in-memory undo/redo stacks (§2.2, §2.9, #18).
//!
//! Verdict is owned, not derived: every change is a `Command`, every change is
//! reversible. Dispatch dedups the `PhotoId` set to unique photos, captures
//! each photo's prior verdict, applies the change, and pushes one reversible
//! `UndoEntry` (#10). This phase keeps the stacks in RAM; the durable,
//! compacted `undo.log` is the next phase (§5).

use std::collections::{HashMap, HashSet, VecDeque};

use dcs_domain::command::Command;
use dcs_domain::cull::AcceptState;
use dcs_domain::photo::PhotoId;

/// Undo entries retained in memory before the oldest is dropped. Only bounds
/// RAM this phase; the durable log is capped and compacted separately (§5).
const UNDO_CAP: usize = 1000;

/// Accepted/rejected tallies for the status bar (§2.9). Unreviewed is derived
/// by the caller from the pool size — absent photos are `Unreviewed`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct VerdictCounts {
    pub accepted: usize,
    pub rejected: usize,
}

/// A reversible patch: for each affected photo, the verdict before and after.
/// `undo` restores *before*, `redo` restores *after*. Command-agnostic enough
/// for verdicts; generalize when tag mutations arrive.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UndoEntry {
    changes: Vec<(PhotoId, AcceptState, AcceptState)>,
}

impl UndoEntry {
    /// Reconstruct an entry from persisted deltas (folded from `undo.log`).
    pub fn from_changes(changes: Vec<(PhotoId, AcceptState, AcceptState)>) -> Self {
        UndoEntry { changes }
    }

    /// The (photo, before, after) deltas — what the durable log records.
    pub fn changes(&self) -> &[(PhotoId, AcceptState, AcceptState)] {
        &self.changes
    }

    /// Photos this entry touches — one per unique photo, never a duplicate.
    pub fn len(&self) -> usize {
        self.changes.len()
    }

    pub fn is_empty(&self) -> bool {
        self.changes.is_empty()
    }
}

/// The owned verdict store and its undo/redo history.
///
/// Keyed by `PhotoId` (decision #33): identity is reconciled to the content
/// fingerprint by the persistence layer at load/save, not here, so commands and
/// the durable `undo.log` keep a stable key while verdicts still survive a
/// rename-in-place.
pub struct Cull {
    verdicts: HashMap<PhotoId, AcceptState>,
    undo: VecDeque<UndoEntry>,
    redo: Vec<UndoEntry>,
}

impl Cull {
    pub fn new() -> Self {
        Cull {
            verdicts: HashMap::new(),
            undo: VecDeque::new(),
            redo: Vec::new(),
        }
    }

    /// Rebuild the store from persisted state on reopen: `verdicts` from
    /// `project.json` (the authoritative state) and the undo/redo stacks folded
    /// from `undo.log`. The stacks are *not* replayed onto state — state already
    /// reflects them (open Q#9). Absent/`Unreviewed` verdicts are not stored, so
    /// the map stays small.
    pub fn from_state(
        verdicts: impl IntoIterator<Item = (PhotoId, AcceptState)>,
        undo: Vec<UndoEntry>,
        redo: Vec<UndoEntry>,
    ) -> Self {
        let mut map = HashMap::new();
        for (id, state) in verdicts {
            if state != AcceptState::Unreviewed {
                map.insert(id, state);
            }
        }
        Cull {
            verdicts: map,
            undo: undo.into(),
            redo,
        }
    }

    /// Verdict for a photo; absent = `Unreviewed`, so the map stays small.
    pub fn state(&self, id: PhotoId) -> AcceptState {
        self.verdicts.get(&id).copied().unwrap_or_default()
    }

    /// Accepted/rejected tallies (§2.9).
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

    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }

    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }

    /// Undo / redo stack depths (diagnostics + tests).
    pub fn undo_depth(&self) -> usize {
        self.undo.len()
    }

    pub fn redo_depth(&self) -> usize {
        self.redo.len()
    }

    /// Undo entries oldest→newest, as delta vecs — what the log compacts to.
    pub fn undo_entries(&self) -> Vec<Vec<(PhotoId, AcceptState, AcceptState)>> {
        self.undo.iter().map(|e| e.changes.clone()).collect()
    }

    /// Redo entries oldest→newest (bottom→top of the stack).
    pub fn redo_entries(&self) -> Vec<Vec<(PhotoId, AcceptState, AcceptState)>> {
        self.redo.iter().map(|e| e.changes.clone()).collect()
    }

    /// Apply a command, recording one reversible entry. Duplicate `PhotoId`s
    /// collapse to unique photos first (#10); a change that moves nothing
    /// records no entry (and so cannot be "undone" into a surprise). Returns the
    /// recorded deltas so the caller can mirror them into the durable log, or
    /// `None` when nothing moved.
    pub fn dispatch(
        &mut self,
        command: Command,
    ) -> Option<Vec<(PhotoId, AcceptState, AcceptState)>> {
        let Command::SetState(ids, target) = command;
        let changes = self.apply_set_state(&ids, target);
        if changes.is_empty() {
            return None;
        }
        self.redo.clear();
        let recorded = changes.clone();
        self.push_undo(UndoEntry { changes });
        Some(recorded)
    }

    /// Reverse the most recent entry, moving it onto the redo stack. Returns
    /// whether anything was undone.
    pub fn undo(&mut self) -> bool {
        let Some(entry) = self.undo.pop_back() else {
            return false;
        };
        for &(id, before, _after) in &entry.changes {
            self.set_raw(id, before);
        }
        self.redo.push(entry);
        true
    }

    /// Re-apply the most recently undone entry. Returns whether anything ran.
    pub fn redo(&mut self) -> bool {
        let Some(entry) = self.redo.pop() else {
            return false;
        };
        for &(id, _before, after) in &entry.changes {
            self.set_raw(id, after);
        }
        self.undo.push_back(entry);
        true
    }

    /// Forget photos entirely: drop their verdicts and scrub them from the
    /// undo/redo stacks, removing any entry left empty. Used when the user
    /// removes missing files (§4); a maintenance op, not itself undoable.
    pub fn forget(&mut self, ids: &HashSet<PhotoId>) {
        if ids.is_empty() {
            return;
        }
        self.verdicts.retain(|id, _| !ids.contains(id));
        for entry in &mut self.undo {
            entry.changes.retain(|(id, _, _)| !ids.contains(id));
        }
        self.undo.retain(|e| !e.changes.is_empty());
        for entry in &mut self.redo {
            entry.changes.retain(|(id, _, _)| !ids.contains(id));
        }
        self.redo.retain(|e| !e.changes.is_empty());
    }

    fn apply_set_state(
        &mut self,
        ids: &[PhotoId],
        target: AcceptState,
    ) -> Vec<(PhotoId, AcceptState, AcceptState)> {
        let mut seen = HashSet::new();
        let mut changes = Vec::new();
        for &id in ids {
            if !seen.insert(id) {
                continue; // dedup to unique photos before the undo entry (#10)
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

    fn set_raw(&mut self, id: PhotoId, state: AcceptState) {
        if state == AcceptState::Unreviewed {
            self.verdicts.remove(&id); // absent == Unreviewed, keep the map small
        } else {
            self.verdicts.insert(id, state);
        }
    }

    fn push_undo(&mut self, entry: UndoEntry) {
        self.undo.push_back(entry);
        if self.undo.len() > UNDO_CAP {
            self.undo.pop_front(); // drop the oldest; the durable log keeps real history
        }
    }
}

impl Default for Cull {
    fn default() -> Self {
        Self::new()
    }
}
