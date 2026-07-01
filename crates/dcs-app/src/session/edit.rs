use std::path::Path;

use dcs_domain::command::{Command, Patch};
use dcs_domain::crops::CropEdit;
use dcs_domain::cull::AcceptState;
use dcs_domain::photo::PhotoId;

use super::{Session, VisibleGroup};

impl Session {
    /// Display index of the focus cursor, if any.
    pub fn focus(&self) -> Option<usize> {
        self.sel.focus()
    }

    pub fn is_selected(&self, id: PhotoId) -> bool {
        self.sel.is_selected(id)
    }

    pub fn selection_count(&self) -> usize {
        self.sel.count()
    }

    /// The display index of a photo in the current visible order, if it's shown
    /// (a board photo filtered out of the grid has none). Lets the board aim the
    /// pool selection at a right-clicked canvas image so shared photo actions
    /// target it.
    pub fn display_index_of(&self, id: PhotoId) -> Option<usize> {
        self.visible_ids().iter().position(|&v| v == id)
    }

    /// The selected photos in visible display order — what a board drag carries
    /// when the dragged cell is part of a multi-selection.
    pub fn selected_ids(&self) -> Vec<PhotoId> {
        self.visible_ids()
            .into_iter()
            .filter(|id| self.sel.is_selected(*id))
            .collect()
    }

    /// Owned verdict for a photo (absent = `Unreviewed`).
    pub fn verdict(&self, id: PhotoId) -> AcceptState {
        self.cull.state(id)
    }

    /// `(accepted, rejected, unreviewed)` for the status bar. Totals only
    /// displayable photos: hidden RAW-only photos aren't part of the cull
    /// workflow, so counting them would make `unrev` exceed the shown count.
    /// Unreviewed = displayable count minus the two reviewed tallies.
    pub fn verdict_counts(&self) -> (usize, usize, usize) {
        let c = self.cull.counts();
        let total = self
            .builder
            .photos()
            .iter()
            .filter(|p| !p.is_raw_only())
            .count();
        let unreviewed = total.saturating_sub(c.accepted + c.rejected);
        (c.accepted, c.rejected, unreviewed)
    }

    pub fn can_undo(&self) -> bool {
        self.history.can_undo()
    }

    pub fn can_redo(&self) -> bool {
        self.history.can_redo()
    }

    /// The cover cell of a visible group: its first accepted member, else its
    /// first cell. Derived on demand — verdicts change without a regroup, so a
    /// stored cover would go stale. The collapsed group's stand-in, and the cell
    /// navigation lands on when entering a collapsed group.
    pub fn group_cover(&self, group: &VisibleGroup) -> usize {
        (group.start..group.start + group.count)
            .find(|&i| {
                self.cell_info(i)
                    .map(|c| c.state == AcceptState::Accepted)
                    .unwrap_or(false)
            })
            .unwrap_or(group.start)
    }

    /// Move the focus cursor (`←→` = ±1 column, `↑↓` = ±1 row) over the flat
    /// visible order. `extend` (Shift) grows the selection from the anchor.
    /// Group-aware navigation drives `set_focus` instead — group boundaries and
    /// collapse are layout facts the UI owns.
    pub fn nav(&mut self, dx: isize, dy: isize, cols: usize, extend: bool) {
        let order = self.visible_ids();
        self.sel.move_focus(dx, dy, cols, &order, extend);
    }

    /// Park the focus on display index `idx` (clamped), with `extend` (Shift)
    /// range semantics. The set point for layout-aware navigation, which resolves
    /// the target index from the visual group layout.
    pub fn set_focus(&mut self, idx: usize, extend: bool) {
        let order = self.visible_ids();
        self.sel.set_focus_index(idx, &order, extend);
    }

    /// `Ctrl+A`: select every visible photo.
    pub fn select_all_visible(&mut self) {
        let order = self.visible_ids();
        self.sel.select_all_visible(&order);
    }

    /// A pointer click on a cell, with the held modifiers. Owns the selection
    /// *policy* (plain = pick one, shift = extend, ctrl/cmd = toggle) so the UI
    /// only reports the raw event. Selection is ephemeral, not a registry command.
    pub fn pointer_select(&mut self, display_index: usize, shift: bool, cmd: bool) {
        if shift {
            self.shift_click_select(display_index);
        } else if cmd {
            self.toggle_click_select(display_index);
        } else {
            self.click_select(display_index);
        }
    }

    /// Click: select exactly one cell, making it focus + anchor.
    pub fn click_select(&mut self, display_index: usize) {
        let order = self.visible_ids();
        self.sel.select_only(display_index, &order);
    }

    /// Shift+click: extend the selection from the anchor to this cell.
    pub fn shift_click_select(&mut self, display_index: usize) {
        let order = self.visible_ids();
        self.sel.extend_to(display_index, &order);
    }

    /// Ctrl/Cmd+click: toggle this cell in or out of the selection.
    pub fn toggle_click_select(&mut self, display_index: usize) {
        let order = self.visible_ids();
        self.sel.toggle_at(display_index, &order);
    }

    /// `Esc`: clear the selection.
    pub fn clear_selection(&mut self) {
        self.sel.clear();
    }

    /// `A`: accept the selection (or focused photo), toggling back to
    /// `Unreviewed` when the focused cell is already accepted.
    pub fn accept(&mut self) {
        self.toggle_verdict(AcceptState::Accepted);
    }

    /// `X`: reject, with the same toggle-back semantics.
    pub fn reject(&mut self) {
        self.toggle_verdict(AcceptState::Rejected);
    }

    /// Whether the photo at `display_index` can be cropped: it exists, isn't a
    /// missing placeholder, and has a JPEG to decode (RAW-only isn't croppable in
    /// v1).
    pub fn is_croppable(&self, display_index: usize) -> bool {
        self.photo_at(display_index)
            .is_some_and(|p| !p.missing && p.decodable_path().is_some())
    }

    /// Whether the focused photo can be cropped (see [`Self::is_croppable`]).
    pub fn focused_is_croppable(&self) -> bool {
        self.focus().is_some_and(|i| self.is_croppable(i))
    }

    /// The committed crop for a photo, or `None` when uncropped.
    pub fn crop_of(&self, id: PhotoId) -> Option<CropEdit> {
        self.crops.crop_of(id)
    }

    /// Whether a photo has a committed crop.
    pub fn has_crop(&self, id: PhotoId) -> bool {
        self.crops.has_crop(id)
    }

    /// Set (or clear, with `None`) the crop on one photo through the unified undo
    /// timeline. The crop editor commits here on apply; an identity edit clears.
    /// A no-op records nothing.
    pub fn set_crop(&mut self, id: PhotoId, edit: Option<CropEdit>) {
        if let Some(patch) = self.dispatch(Command::SetCrop(vec![id], edit)) {
            // Same invalidation path as undo/redo, so a crop change can't drift
            // between them.
            self.invalidate_thumbs_for_patch(&patch);
            self.refresh_after_owned_change();
        }
    }

    /// Drop cached thumbnails for any photo whose crop a patch changed, so the
    /// grid/gallery re-decode with the new (or reverted) framing. A no-op for
    /// verdict/tag patches, which don't touch pixels.
    fn invalidate_thumbs_for_patch(&mut self, patch: &Patch) {
        if let Patch::Crop(changes) = patch {
            for &(id, _, _) in changes {
                self.invalidate_photo_thumbs(id);
            }
        }
    }

    /// `Ctrl+Z`: undo the last mutation (verdict or tag).
    pub fn undo(&mut self) -> bool {
        if self.read_only {
            return false;
        }
        if let Some(patch) = self.history.undo(
            &mut self.cull,
            &mut self.tags,
            &mut self.crops,
            &mut self.boards,
        ) {
            if let Some(log) = &mut self.log {
                let _ = log.record_undo();
            }
            self.dirty = true;
            self.invalidate_thumbs_for_patch(&patch);
            self.refresh_after_owned_change();
            true
        } else {
            false
        }
    }

    /// `Ctrl+Shift+Z`: redo.
    pub fn redo(&mut self) -> bool {
        if self.read_only {
            return false;
        }
        if let Some(patch) = self.history.redo(
            &mut self.cull,
            &mut self.tags,
            &mut self.crops,
            &mut self.boards,
        ) {
            if let Some(log) = &mut self.log {
                let _ = log.record_redo();
            }
            self.dirty = true;
            self.invalidate_thumbs_for_patch(&patch);
            self.refresh_after_owned_change();
            true
        } else {
            false
        }
    }

    /// Whether any photo is rejected — gates the "reveal rejected" action.
    /// Reads the maintained verdict tally (O(reviewed)) rather than scanning the
    /// whole pool, since the menu bar polls this every frame.
    pub fn has_rejected(&self) -> bool {
        self.cull.counts().rejected > 0
    }

    /// Open the OS file manager at the source folder so the rejected originals
    /// can be acted on outside the app. No-op when no folder is open.
    pub fn reveal_rejected(&self) {
        if let Some(root) = &self.root {
            self.reveal(root);
        }
    }

    /// Open the OS file manager at `path` — the "Open folder" affordance after an
    /// export.
    pub fn reveal(&self, path: &Path) {
        dcs_io::reveal::reveal(path);
    }

    /// Open the OS file manager at the open project folder. No-op with no folder.
    pub fn reveal_folder(&self) {
        if let Some(root) = &self.root {
            self.reveal(root);
        }
    }

    /// Reveal the focused photo's file in the OS file manager, selected where the
    /// platform allows. No-op when nothing is focused.
    pub fn reveal_focused(&self) {
        if let Some(photo) = self.focus().and_then(|i| self.photo_at(i)) {
            dcs_io::reveal::reveal_file(photo.display_path());
        }
    }

    /// Toggle target is decided by the *focused* photo's verdict, then applied
    /// to the whole selection — so a mixed selection resolves predictably.
    fn toggle_verdict(&mut self, on: AcceptState) {
        if self.read_only {
            return; // another instance owns the write lock
        }
        let order = self.visible_ids();
        let targets = self.sel.selected_or_focused(&order);
        if targets.is_empty() {
            return;
        }
        let focus_state = self
            .sel
            .focus()
            .and_then(|i| order.get(i).copied())
            .map(|id| self.cull.state(id))
            .unwrap_or_default();
        let target = if focus_state == on {
            AcceptState::Unreviewed
        } else {
            on
        };
        self.dispatch(Command::SetState(targets, target));
        self.rebuild_visible();
    }

    /// Apply one command through the unified undo timeline and mirror its patch
    /// into the durable log. Routes verdict and tag mutations alike; a no-op
    /// command records nothing. Returns the recorded patch (the caller may need
    /// the allocated tag id), or `None` when nothing moved or we're read-only.
    pub(crate) fn dispatch(&mut self, command: Command) -> Option<Patch> {
        if self.read_only {
            return None;
        }
        let patch = self.history.dispatch(
            command,
            &mut self.cull,
            &mut self.tags,
            &mut self.crops,
            &mut self.boards,
        )?;
        if let Some(log) = &mut self.log {
            let _ = log.record_patch(&patch);
        }
        self.dirty = true;
        Some(patch)
    }
}
