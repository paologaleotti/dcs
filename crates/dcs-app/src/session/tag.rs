//! Tag mutations and queries on the session. Each mutation rides the unified
//! undo timeline via [`Session::dispatch`]; the registry exposes the same set
//! to the UI's keys, palette, and menus. Queries are the read side the grid
//! (cell strips) and tag bands consume.
//!
//! Tag changes are owned-state edits, not display settings, so they mark the
//! project dirty but don't regroup — grouping by tag re-derives from these
//! assignments when that axis is active.

use dcs_domain::command::{Command, Patch, TagDelta};
use dcs_domain::photo::PhotoId;
use dcs_domain::tag::{Color, Tag, TagId};

use super::Session;

impl Session {
    /// Create a tag, returning its freshly allocated id (so the caller can
    /// immediately assign it to the selection). `None` when read-only.
    pub fn create_tag(&mut self, name: impl Into<String>, color: Color) -> Option<TagId> {
        let patch = self.dispatch(Command::CreateTag {
            name: name.into(),
            color,
        })?;
        let Patch::Tag(deltas) = patch else {
            return None;
        };
        deltas.into_iter().find_map(|d| match d {
            TagDelta::Created(tag) => Some(tag.id),
            _ => None,
        })
    }

    /// Add a tag to every listed photo (no-op per already-tagged photo).
    pub fn assign_tag(&mut self, tag: TagId, ids: &[PhotoId]) {
        self.dispatch(Command::AssignTag(tag, ids.to_vec()));
    }

    /// Remove a tag from every listed photo.
    pub fn unassign_tag(&mut self, tag: TagId, ids: &[PhotoId]) {
        self.dispatch(Command::UnassignTag(tag, ids.to_vec()));
    }

    /// Rename a tag — or merge it into an existing tag if the new name already
    /// belongs to one.
    pub fn rename_tag(&mut self, tag: TagId, name: impl Into<String>) {
        self.dispatch(Command::RenameTag(tag, name.into()));
    }

    /// Merge `from` into `into`: `from`'s photos move to `into`, `from` is gone.
    pub fn merge_tags(&mut self, into: TagId, from: TagId) {
        self.dispatch(Command::MergeTags { into, from });
    }

    /// Delete a tag and all its assignments.
    pub fn delete_tag(&mut self, tag: TagId) {
        self.dispatch(Command::DeleteTag(tag));
    }

    /// Assign a tag to the current selection (or the focused photo). The
    /// selection-driven entry the UI's `T` / `1–9` keys call.
    pub fn tag_selection(&mut self, tag: TagId) {
        let targets = self.selection_targets();
        if !targets.is_empty() {
            self.assign_tag(tag, &targets);
        }
    }

    /// Remove a tag from the current selection (or the focused photo) — `Shift+T`.
    pub fn untag_selection(&mut self, tag: TagId) {
        let targets = self.selection_targets();
        if !targets.is_empty() {
            self.unassign_tag(tag, &targets);
        }
    }

    /// All tag definitions, in creation (id) order.
    pub fn all_tags(&self) -> Vec<Tag> {
        self.tags.defs()
    }

    /// The definition for one tag, cloned for the UI.
    pub fn tag_def(&self, tag: TagId) -> Option<Tag> {
        self.tags.def(tag).cloned()
    }

    /// The tags on a photo, by id — the cell's strips.
    pub fn tags_of(&self, photo: PhotoId) -> Vec<TagId> {
        self.tags.tags_of(photo)
    }

    /// Whether a photo carries a tag.
    pub fn is_tagged(&self, tag: TagId, photo: PhotoId) -> bool {
        self.tags.is_assigned(tag, photo)
    }

    /// Unique photos carrying a tag — a band's count.
    pub fn tag_photo_count(&self, tag: TagId) -> usize {
        self.tags.photo_count(tag)
    }

    /// The selection, or the focused photo when nothing is selected — the
    /// targets a selection-driven mutation hits, in visible order.
    fn selection_targets(&self) -> Vec<PhotoId> {
        let order = self.visible_ids();
        self.sel.selected_or_focused(&order)
    }
}
