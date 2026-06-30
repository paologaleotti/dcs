//! Owned views store, including boards. Mirrors [`crate::crops::CropStore`]:
//! the typed state plus the primitives to apply and reverse one board delta;
//! the unified undo/redo timeline lives in [`crate::history::History`].
//!
//! **Forward-compat bridge.** `project.json` keeps views as raw JSON so a future
//! build's view kind survives untouched (spec §9b). This store is where that
//! JSON becomes typed: each entry parses into a [`View`] when its kind is known,
//! and is otherwise kept verbatim as [`ViewEntry::Unknown`] — preserved on the
//! next save, never rendered. `dcs-io` stays ignorant of view semantics.
//!
//! Scope of the guarantee: only unknown *kinds* round-trip verbatim. Unknown
//! *fields* on a **known** kind (e.g. a future field added to `BoardState`) are
//! dropped on re-save, since the typed value is what gets re-serialized. Any
//! additive field on a known view kind must therefore bump the schema version.

use std::collections::HashSet;

use dcs_domain::command::BoardDelta;
use dcs_domain::photo::PhotoId;
use dcs_domain::view::{BoardItem, BoardState, Pos, View, ViewId, ViewKind};
use serde_json::Value;

/// One persisted view: typed when this build knows its kind, raw otherwise.
enum ViewEntry {
    Known(View),
    /// A kind this build doesn't understand — kept exactly as loaded so a newer
    /// build that wrote it finds it intact.
    Unknown(Value),
}

/// The owned views, in persisted order, plus the monotonic view-id counter.
/// Board mutations route through here; everything else is structural.
#[derive(Default)]
pub struct BoardStore {
    views: Vec<ViewEntry>,
    next_view_id: u32,
}

impl BoardStore {
    /// Parse the persisted views (raw JSON) into typed entries, preserving any
    /// unknown kinds verbatim. `next_view_id` is the persisted counter, floored
    /// to one past the largest known id so a reused id can never collide.
    pub fn from_values(values: Vec<Value>, next_view_id: u32) -> Self {
        let views: Vec<ViewEntry> = values
            .into_iter()
            .map(|v| match serde_json::from_value::<View>(v.clone()) {
                Ok(view) => ViewEntry::Known(view),
                Err(_) => ViewEntry::Unknown(v),
            })
            .collect();
        let max_known = views
            .iter()
            .filter_map(|e| match e {
                ViewEntry::Known(v) => Some(v.id.0),
                ViewEntry::Unknown(_) => None,
            })
            .max();
        let next_view_id = match max_known {
            Some(m) => next_view_id.max(m + 1),
            None => next_view_id,
        };
        BoardStore {
            views,
            next_view_id,
        }
    }

    /// Serialize back to raw JSON for `project.json`, in persisted order, with
    /// unknown kinds passed through untouched.
    pub fn to_values(&self) -> Vec<Value> {
        self.views
            .iter()
            .map(|e| match e {
                // A typed view serializes infallibly: its only non-string, non-int
                // fields are the `f32` position + scale on each `BoardItem`, and
                // `BoardState` rejects non-finite placements at every mutation
                // boundary (`place`/`insert_at`/`move_to` via `is_finite`), so
                // `to_value` can't hit its one failure mode (NaN/∞). Any future
                // mutator of `scale` must keep that finiteness gate, or this turns
                // into a panic. Writing `null` into the precious store on a
                // swallowed error would silently drop a whole board, so this is a
                // hard invariant, not a fallback.
                ViewEntry::Known(v) => {
                    serde_json::to_value(v).expect("a finite-positioned View always serializes")
                }
                ViewEntry::Unknown(v) => v.clone(),
            })
            .collect()
    }

    /// The monotonic view-id counter to persist.
    pub fn next_view_id(&self) -> u32 {
        self.next_view_id
    }

    /// Whether any board view exists.
    pub fn has_board(&self) -> bool {
        self.views
            .iter()
            .any(|e| matches!(e, ViewEntry::Known(v) if matches!(v.kind, ViewKind::Board(_))))
    }

    /// The id of the first board, creating one if none exists. Returns the id and
    /// whether a board was created (so the caller can mark the project dirty).
    /// v1 keeps a single auto-board; multiple named boards are a later phase.
    pub fn ensure_board(&mut self) -> (ViewId, bool) {
        if let Some(id) = self.first_board_id() {
            return (id, false);
        }
        let id = ViewId(self.next_view_id);
        self.next_view_id += 1;
        self.views.push(ViewEntry::Known(View {
            id,
            name: "Board".into(),
            kind: ViewKind::Board(BoardState::default()),
        }));
        (id, true)
    }

    /// The placed items of a board, back-to-front. Empty for an unknown id.
    pub fn items(&self, view: ViewId) -> &[BoardItem] {
        self.board(view).map(|b| b.items.as_slice()).unwrap_or(&[])
    }

    /// Place photos at the given positions (a drop), returning the reversible
    /// deltas. Photos already on the board are skipped; the list is deduped to
    /// unique photos first.
    pub fn apply_add(&mut self, view: ViewId, items: &[(PhotoId, Pos)]) -> Vec<BoardDelta> {
        let mut seen = HashSet::new();
        let mut deltas = Vec::new();
        let Some(board) = self.board_mut(view) else {
            return deltas;
        };
        for &(photo, pos) in items {
            if !seen.insert(photo) {
                continue;
            }
            let item = BoardItem::placed(photo, pos);
            if board.place(item) {
                deltas.push(BoardDelta::Added(view, item));
            }
        }
        deltas
    }

    /// Remove photos, returning the reversible deltas (each carrying the removed
    /// item and its stack index so undo restores z).
    pub fn apply_remove(&mut self, view: ViewId, photos: &[PhotoId]) -> Vec<BoardDelta> {
        let mut seen = HashSet::new();
        let mut deltas = Vec::new();
        let Some(board) = self.board_mut(view) else {
            return deltas;
        };
        for &photo in photos {
            if !seen.insert(photo) {
                continue;
            }
            if let Some((at, item)) = board.remove(photo) {
                deltas.push(BoardDelta::Removed(view, at, item));
            }
        }
        deltas
    }

    /// Move photos to new positions (a coalesced drag), returning the deltas.
    /// No-op moves (unchanged or absent) are omitted, so an aborted drag that
    /// snapped back records nothing.
    pub fn apply_move(&mut self, view: ViewId, items: &[(PhotoId, Pos)]) -> Vec<BoardDelta> {
        let mut seen = HashSet::new();
        let mut deltas = Vec::new();
        let Some(board) = self.board_mut(view) else {
            return deltas;
        };
        for &(photo, pos) in items {
            if !seen.insert(photo) {
                continue;
            }
            if let Some(before) = board.move_to(photo, pos) {
                deltas.push(BoardDelta::Moved(view, photo, before, pos));
            }
        }
        deltas
    }

    /// Bring a photo to the top of its board's stack. Returns whether the order
    /// changed. Not a `BoardDelta` — z-order-on-grab is persisted but not part of
    /// the undo timeline.
    pub fn raise(&mut self, view: ViewId, photo: PhotoId) -> bool {
        self.board_mut(view).is_some_and(|b| b.raise(photo))
    }

    /// Re-apply recorded deltas (redo): replay each forward, in order.
    pub fn apply(&mut self, deltas: &[BoardDelta]) {
        for delta in deltas {
            self.apply_one(delta);
        }
    }

    /// Reverse recorded deltas (undo): replay each backward, in reverse order.
    pub fn revert(&mut self, deltas: &[BoardDelta]) {
        for delta in deltas.iter().rev() {
            self.revert_one(delta);
        }
    }

    /// Forget photos entirely: drop them from every board. The undo timeline is
    /// scrubbed separately (history's job).
    pub fn forget(&mut self, ids: &HashSet<PhotoId>) {
        for entry in &mut self.views {
            if let ViewEntry::Known(View {
                kind: ViewKind::Board(board),
                ..
            }) = entry
            {
                board.forget(ids);
            }
        }
    }

    fn apply_one(&mut self, delta: &BoardDelta) {
        let view = delta.view();
        let Some(board) = self.board_mut(view) else {
            return;
        };
        match delta {
            BoardDelta::Added(_, item) => {
                board.place(*item);
            }
            BoardDelta::Removed(_, _, item) => {
                board.remove(item.photo);
            }
            BoardDelta::Moved(_, photo, _before, after) => {
                board.move_to(*photo, *after);
            }
        }
    }

    fn revert_one(&mut self, delta: &BoardDelta) {
        let view = delta.view();
        let Some(board) = self.board_mut(view) else {
            return;
        };
        match delta {
            BoardDelta::Added(_, item) => {
                board.remove(item.photo);
            }
            BoardDelta::Removed(_, at, item) => {
                board.insert_at(*at, *item);
            }
            BoardDelta::Moved(_, photo, before, _after) => {
                board.move_to(*photo, *before);
            }
        }
    }

    fn first_board_id(&self) -> Option<ViewId> {
        self.views.iter().find_map(|e| match e {
            ViewEntry::Known(v) if matches!(v.kind, ViewKind::Board(_)) => Some(v.id),
            _ => None,
        })
    }

    fn board(&self, view: ViewId) -> Option<&BoardState> {
        self.views.iter().find_map(|e| match e {
            ViewEntry::Known(v) if v.id == view => match &v.kind {
                ViewKind::Board(b) => Some(b),
                ViewKind::Grid(_) => None,
            },
            _ => None,
        })
    }

    fn board_mut(&mut self, view: ViewId) -> Option<&mut BoardState> {
        self.views.iter_mut().find_map(|e| match e {
            ViewEntry::Known(v) if v.id == view => match &mut v.kind {
                ViewKind::Board(b) => Some(b),
                ViewKind::Grid(_) => None,
            },
            _ => None,
        })
    }
}
