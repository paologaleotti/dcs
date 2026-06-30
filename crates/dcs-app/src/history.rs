//! The single durable undo timeline. Every owned mutation — verdict *or* tag —
//! is dispatched here, applied against the relevant store, and recorded as one
//! reversible [`Patch`]. One stack reverses anything: the promptless design
//! leans on it.
//!
//! Loaded, never replayed: `project.json` is authoritative for state; on reopen
//! the folded `undo.log` seeds these stacks but is not re-applied, so the two
//! can't double-count.

use std::collections::{HashSet, VecDeque};

use dcs_domain::command::{BoardDelta, Command, Patch, TagDelta};
use dcs_domain::photo::PhotoId;

use crate::boards::BoardStore;
use crate::crops::CropStore;
use crate::cull::Cull;
use crate::tags::TagStore;

/// Undo entries retained in RAM before the oldest is dropped. The durable log is
/// capped and compacted separately.
const UNDO_CAP: usize = 1000;

/// The unified undo + redo stacks. Each entry is one command's [`Patch`]; the
/// back of `undo` / top of `redo` is the next to reverse / replay.
#[derive(Default)]
pub struct History {
    undo: VecDeque<Patch>,
    redo: Vec<Patch>,
}

impl History {
    pub fn new() -> Self {
        History::default()
    }

    /// Rebuild from the folded `undo.log` stacks on reopen (oldest→newest).
    pub fn from_stacks(undo: Vec<Patch>, redo: Vec<Patch>) -> Self {
        History {
            undo: undo.into(),
            redo,
        }
    }

    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }

    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }

    pub fn undo_depth(&self) -> usize {
        self.undo.len()
    }

    pub fn redo_depth(&self) -> usize {
        self.redo.len()
    }

    /// The undo + redo entries oldest→newest — what the durable log compacts to.
    pub fn stacks(&self) -> (Vec<Patch>, Vec<Patch>) {
        (self.undo.iter().cloned().collect(), self.redo.clone())
    }

    /// Apply a command against the stores, recording one reversible patch.
    /// A command that moves nothing records no entry (and so can't be "undone"
    /// into a surprise). Returns the recorded patch so the caller mirrors it into
    /// the durable log, or `None` when nothing moved.
    pub fn dispatch(
        &mut self,
        command: Command,
        cull: &mut Cull,
        tags: &mut TagStore,
        crops: &mut CropStore,
        boards: &mut BoardStore,
    ) -> Option<Patch> {
        let patch = match command {
            Command::SetState(ids, target) => Patch::Verdict(cull.apply_set_state(&ids, target)),
            Command::AssignTag(tag, ids) => Patch::Tag(tags.apply_assign(tag, &ids)),
            Command::UnassignTag(tag, ids) => Patch::Tag(tags.apply_unassign(tag, &ids)),
            Command::CreateTag { name, color } => Patch::Tag(tags.apply_create(name, color)),
            Command::RenameTag(id, name) => Patch::Tag(tags.apply_rename(id, name)),
            Command::MergeTags { into, from } => Patch::Tag(tags.apply_merge(into, from)),
            Command::SetTagColor(id, color) => Patch::Tag(tags.apply_recolor(id, color)),
            Command::DeleteTag(id) => Patch::Tag(tags.apply_delete(id)),
            Command::SetCrop(ids, target) => Patch::Crop(crops.apply_set_crop(&ids, target)),
            Command::AddToBoard(view, items) => Patch::Board(boards.apply_add(view, &items)),
            Command::RemoveFromBoard(view, photos) => {
                Patch::Board(boards.apply_remove(view, &photos))
            }
            Command::MoveOnBoard(view, items) => Patch::Board(boards.apply_move(view, &items)),
        };
        if patch.is_empty() {
            return None;
        }
        self.redo.clear();
        self.push_undo(patch.clone());
        Some(patch)
    }

    /// Reverse the most recent entry, moving it onto the redo stack. Returns
    /// whether anything was undone.
    pub fn undo(
        &mut self,
        cull: &mut Cull,
        tags: &mut TagStore,
        crops: &mut CropStore,
        boards: &mut BoardStore,
    ) -> Option<Patch> {
        let patch = self.undo.pop_back()?;
        Self::revert(&patch, cull, tags, crops, boards);
        self.redo.push(patch.clone());
        Some(patch)
    }

    /// Re-apply the most recently undone entry. Returns the patch that was
    /// re-applied, or `None` when the redo stack was empty.
    pub fn redo(
        &mut self,
        cull: &mut Cull,
        tags: &mut TagStore,
        crops: &mut CropStore,
        boards: &mut BoardStore,
    ) -> Option<Patch> {
        let patch = self.redo.pop()?;
        Self::apply(&patch, cull, tags, crops, boards);
        self.undo.push_back(patch.clone());
        Some(patch)
    }

    /// Forget photos: scrub them from every recorded patch, dropping any entry
    /// left empty. A maintenance op (missing-file prune), not itself undoable.
    /// Tag *definition* deltas (create/remove/rename) are kept — they don't
    /// reference a photo — so only per-photo assignments are scrubbed.
    pub fn forget(&mut self, ids: &HashSet<PhotoId>) {
        if ids.is_empty() {
            return;
        }
        self.undo.iter_mut().for_each(|p| scrub(p, ids));
        self.undo.retain(|p| !p.is_empty());
        self.redo.iter_mut().for_each(|p| scrub(p, ids));
        self.redo.retain(|p| !p.is_empty());
    }

    fn apply(
        patch: &Patch,
        cull: &mut Cull,
        tags: &mut TagStore,
        crops: &mut CropStore,
        boards: &mut BoardStore,
    ) {
        match patch {
            Patch::Verdict(c) => cull.apply(c),
            Patch::Tag(d) => tags.apply(d),
            Patch::Crop(c) => crops.apply(c),
            Patch::Board(d) => boards.apply(d),
        }
    }

    fn revert(
        patch: &Patch,
        cull: &mut Cull,
        tags: &mut TagStore,
        crops: &mut CropStore,
        boards: &mut BoardStore,
    ) {
        match patch {
            Patch::Verdict(c) => cull.revert(c),
            Patch::Tag(d) => tags.revert(d),
            Patch::Crop(c) => crops.revert(c),
            Patch::Board(d) => boards.revert(d),
        }
    }

    fn push_undo(&mut self, patch: Patch) {
        self.undo.push_back(patch);
        if self.undo.len() > UNDO_CAP {
            self.undo.pop_front(); // drop the oldest; the durable log keeps real history
        }
    }
}

/// Drop the deltas in a patch that reference a forgotten photo. Tag def-level
/// deltas (no photo) survive.
fn scrub(patch: &mut Patch, ids: &HashSet<PhotoId>) {
    match patch {
        Patch::Verdict(changes) => changes.retain(|(id, _, _)| !ids.contains(id)),
        Patch::Tag(deltas) => deltas.retain(|d| match d {
            TagDelta::Assigned(_, p) | TagDelta::Unassigned(_, p) => !ids.contains(p),
            _ => true,
        }),
        Patch::Crop(changes) => changes.retain(|(id, _, _)| !ids.contains(id)),
        Patch::Board(deltas) => deltas.retain(|d: &BoardDelta| !ids.contains(&d.photo())),
    }
}
