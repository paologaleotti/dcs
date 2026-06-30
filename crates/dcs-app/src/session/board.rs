//! Board view session API: the single auto-board's id, its placed items (read),
//! and the three owned mutations (add / move / remove) routed through the
//! unified undo timeline. The board is a pure arrangement lens — it never
//! changes verdicts, tags, grouping, or filters.

use dcs_domain::command::Command;
use dcs_domain::photo::PhotoId;
use dcs_domain::view::{BoardItem, Pos, ViewId};

use super::Session;

impl Session {
    /// The id of the project's board, creating it on first use. v1 keeps a
    /// single auto-board. Returns `None` on a read-only project that has no
    /// board yet — we can't persist a new one, so the canvas shows empty.
    pub fn primary_board(&mut self) -> Option<ViewId> {
        if self.read_only && !self.boards.has_board() {
            return None;
        }
        let (id, created) = self.boards.ensure_board();
        if created {
            self.dirty = true;
        }
        Some(id)
    }

    /// The items placed on `view`, back-to-front (last is on top). Empty for an
    /// unknown id.
    pub fn board_items(&self, view: ViewId) -> &[BoardItem] {
        self.boards.items(view)
    }

    /// Place photos on `view` at the given scene positions (a drop). Photos
    /// already present are ignored. One undo entry.
    pub fn add_to_board(&mut self, view: ViewId, items: Vec<(PhotoId, Pos)>) {
        self.dispatch(Command::AddToBoard(view, items));
    }

    /// Commit a finished drag: move the listed photos to their final positions
    /// as one undo entry. Unchanged or absent photos are dropped by the store,
    /// so an aborted drag that snapped back commits nothing.
    pub fn move_on_board(&mut self, view: ViewId, items: Vec<(PhotoId, Pos)>) {
        self.dispatch(Command::MoveOnBoard(view, items));
    }

    /// Remove photos from `view`. One undo entry; its inverse re-places them at
    /// their original positions and stacking order.
    pub fn remove_from_board(&mut self, view: ViewId, photos: Vec<PhotoId>) {
        self.dispatch(Command::RemoveFromBoard(view, photos));
    }

    /// Bring a board photo to the front (grab-to-raise). z-order is owned, so
    /// this persists, but it is deliberately **not undoable** — a reorder on
    /// every grab would bury real edits under raise/lower churn. No-op when
    /// read-only or already on top.
    pub fn raise_on_board(&mut self, view: ViewId, photo: PhotoId) {
        if self.read_only {
            return;
        }
        if self.boards.raise(view, photo) {
            self.dirty = true;
        }
    }
}
