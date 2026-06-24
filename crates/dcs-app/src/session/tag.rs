//! Tag mutations and queries on the session. Each mutation rides the unified
//! undo timeline via [`Session::dispatch`]; the registry exposes the same set
//! to the UI's keys, palette, and menus. Queries are the read side the grid
//! (cell strips) and tag bands consume.
//!
//! Tag changes are owned-state edits, not display settings, so they mark the
//! project dirty but don't regroup — grouping by tag re-derives from these
//! assignments when that axis is active.

use std::collections::BTreeSet;

use dcs_domain::command::{Command, Patch, TagDelta};
use dcs_domain::grouping::{self, DerivedGroup};
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
        self.refresh_after_owned_change();
    }

    /// Remove a tag from every listed photo.
    pub fn unassign_tag(&mut self, tag: TagId, ids: &[PhotoId]) {
        self.dispatch(Command::UnassignTag(tag, ids.to_vec()));
        self.refresh_after_owned_change();
    }

    /// Rename a tag — or merge it into an existing tag if the new name already
    /// belongs to one.
    pub fn rename_tag(&mut self, tag: TagId, name: impl Into<String>) {
        self.dispatch(Command::RenameTag(tag, name.into()));
        self.refresh_after_owned_change();
    }

    /// Merge `from` into `into`: `from`'s photos move to `into`, `from` is gone.
    pub fn merge_tags(&mut self, into: TagId, from: TagId) {
        self.dispatch(Command::MergeTags { into, from });
        self.refresh_after_owned_change();
    }

    /// Recolor a tag. A display-only change to the tag definition; no regroup.
    pub fn set_tag_color(&mut self, tag: TagId, color: Color) {
        self.dispatch(Command::SetTagColor(tag, color));
    }

    /// Delete a tag and all its assignments.
    pub fn delete_tag(&mut self, tag: TagId) {
        self.dispatch(Command::DeleteTag(tag));
        self.refresh_after_owned_change();
    }

    /// Assign a tag to the current selection (or the focused photo). The
    /// selection-driven entry the `T` palette calls.
    pub fn tag_selection(&mut self, tag: TagId) {
        let targets = self.selection_targets();
        if !targets.is_empty() {
            self.assign_tag(tag, &targets);
        }
    }

    /// Tag every filtered photo with a tag named after the lone active search
    /// query, reusing an existing same-name tag (case-insensitive) or creating
    /// one. No-op when the filter isn't a single search, the project is
    /// read-only, or nothing matches.
    pub fn tag_results_as_search(&mut self) {
        let Some(query) = self.single_search_query() else {
            return;
        };
        let ids = self.visible_ids();
        if ids.is_empty() {
            return;
        }
        let tags = self.all_tags();
        let existing = tags
            .iter()
            .find(|t| t.name.eq_ignore_ascii_case(&query))
            .map(|t| t.id);
        let tag = match existing {
            Some(id) => id,
            None => {
                let color = dcs_domain::tag::palette_color(tags.len() + 1);
                match self.create_tag(query, color) {
                    Some(id) => id,
                    None => return,
                }
            }
        };
        self.assign_tag(tag, &ids);
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

    /// The photo's lowest-id tag — a stable, deterministic "primary" (ids are
    /// monotonic, so this is the earliest-created tag on the photo), or `None`
    /// when untagged. The `{tag}` export token's source.
    pub fn primary_tag_name(&self, photo: PhotoId) -> Option<String> {
        self.tags_of(photo)
            .into_iter()
            .min()
            .and_then(|t| self.tag_def(t))
            .map(|t| t.name)
    }

    /// Whether a photo carries a tag.
    pub fn is_tagged(&self, tag: TagId, photo: PhotoId) -> bool {
        self.tags.is_assigned(tag, photo)
    }

    /// Unique photos carrying a tag — a band's count.
    pub fn tag_photo_count(&self, tag: TagId) -> usize {
        self.tags.photo_count(tag)
    }

    /// The tags present on the current selection (union, deduped, id order) —
    /// the removable set the untag palette lists.
    pub fn selection_tags(&self) -> Vec<Tag> {
        let mut ids: BTreeSet<TagId> = BTreeSet::new();
        for photo in self.selection_targets() {
            ids.extend(self.tags.tags_of(photo));
        }
        ids.into_iter()
            .filter_map(|id| self.tags.def(id).cloned())
            .collect()
    }

    /// Every tag paired with how many of the current selection already carry it
    /// — lets the add palette mark tags already on the selection.
    pub fn tags_with_selection_counts(&self) -> Vec<(Tag, usize)> {
        let targets = self.selection_targets();
        self.tags
            .defs()
            .into_iter()
            .map(|t| {
                let on = targets
                    .iter()
                    .filter(|&&p| self.tags.is_assigned(t.id, p))
                    .count();
                (t, on)
            })
            .collect()
    }

    /// Whether the current selection carries any tag — gates the remove action.
    pub fn selection_has_tags(&self) -> bool {
        self.selection_targets()
            .iter()
            .any(|&p| !self.tags.tags_of(p).is_empty())
    }

    /// Derive the tag bands for the tag axis: one band per non-empty tag,
    /// projections into each, `Untagged` last. Borrows the owned assignment
    /// index directly, so a regroup allocates only the bands it builds.
    pub(super) fn derive_tag_groups(&self) -> Vec<DerivedGroup> {
        let photos = self.builder.photos();
        grouping::tag_groups(
            photos,
            self.tags.photo_tag_index(),
            &self.tags.name_map(),
            self.sort,
        )
    }

    /// The selection, or the focused photo when nothing is selected — the
    /// targets a selection-driven mutation hits, in visible order.
    fn selection_targets(&self) -> Vec<PhotoId> {
        let order = self.visible_ids();
        self.sel.selected_or_focused(&order)
    }
}
