//! Views: the persisted arrangement lenses over the pool. A `View` carries an
//! id, a name, and a [`ViewKind`]. The Grid view is an ephemeral lens (its
//! settings stay empty for now); the Board view is a freeform canvas that owns
//! per-item placement.
//!
//! **Ownership rule (spec §9b):** per-photo facts (verdict, tags, crop) live on
//! the photo and are true in every view. *Layout* facts — which photos are on a
//! board, where, and in what stacking order — live here, on the view. The board
//! never invents photo state; the grid never stores layout.
//!
//! Geometry note: a board item owns only `pos` (scene-space top-left) and
//! `scale`. Pixel rectangles and hit-testing need the photo's display aspect,
//! which lives with the thumbnail in `dcs-ui`, so those stay in the UI. This
//! module owns the serialized, undoable state and the invariants over it.

use serde::{Deserialize, Serialize};

use crate::photo::PhotoId;

/// Stable per-view identifier, allocated on creation and never reused — board
/// item positions are keyed to a view, so a reused id would graft one board's
/// layout onto another.
#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize,
)]
pub struct ViewId(pub u32);

/// A point in board *scene* space (not screen pixels). The `Scene` pan/zoom
/// transform maps it to the screen; panning and zooming never change it.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Pos {
    pub x: f32,
    pub y: f32,
}

impl Pos {
    pub fn new(x: f32, y: f32) -> Self {
        Pos { x, y }
    }

    /// Whether both coordinates are finite. Non-finite positions (from a
    /// degenerate pan/zoom transform) must never enter owned state — they would
    /// fail JSON serialization and corrupt the precious store.
    pub fn is_finite(&self) -> bool {
        self.x.is_finite() && self.y.is_finite()
    }
}

/// One arrangement lens. Defaulted `id`/`name` so a legacy `{"kind":"Grid"}`
/// entry (written before views were typed) still loads.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct View {
    #[serde(default)]
    pub id: ViewId,
    #[serde(default)]
    pub name: String,
    #[serde(flatten)]
    pub kind: ViewKind,
}

/// What a view *is*. Internally tagged on `"kind"` so the on-disk shape reads
/// `{"kind":"Board", "items":[…]}`. Unknown kinds (a future build's) fail to
/// parse here and are preserved verbatim by the store, never rendered.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind")]
pub enum ViewKind {
    /// The grid lens. Its settings are derived/ephemeral for now (axis, sort,
    /// and zoom live in session state), so this stays an empty placeholder that
    /// keeps the type board-ready without persisting derived state.
    Grid(GridSettings),
    /// The freeform board: curated membership, per-item position, stacking order.
    Board(BoardState),
}

/// Placeholder for the grid view's owned settings. Empty in v1 — grid axis,
/// sort, and zoom are derived/session state, not view-owned.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct GridSettings {}

/// The owned state of one board: its items in stacking order. The `Vec` order
/// *is* the z-order — later items paint on top — so membership, position, and
/// z all live in one structure.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct BoardState {
    /// Items back-to-front: `items[0]` is the bottom of the stack, the last is
    /// on top. A photo appears at most once.
    pub items: Vec<BoardItem>,
}

/// One placed photo on a board.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct BoardItem {
    pub photo: PhotoId,
    /// Scene-space top-left.
    pub pos: Pos,
    /// Display size relative to the base placement size. `1.0` in v1 (resize is
    /// a later phase); persisted now so the type doesn't churn when it lands.
    pub scale: f32,
}

impl BoardItem {
    /// A freshly dropped item at `pos`, at default scale.
    pub fn placed(photo: PhotoId, pos: Pos) -> Self {
        BoardItem {
            photo,
            pos,
            scale: 1.0,
        }
    }

    /// Whether the placement is fully finite (position and scale) — the
    /// precondition for entering owned, serializable state.
    pub fn is_finite(&self) -> bool {
        self.pos.is_finite() && self.scale.is_finite()
    }
}

impl BoardState {
    /// Whether `photo` is already placed on this board.
    pub fn contains(&self, photo: PhotoId) -> bool {
        self.items.iter().any(|it| it.photo == photo)
    }

    /// The stack index of `photo`, if placed.
    pub fn index_of(&self, photo: PhotoId) -> Option<usize> {
        self.items.iter().position(|it| it.photo == photo)
    }

    /// The placement of `photo`, if present.
    pub fn item(&self, photo: PhotoId) -> Option<&BoardItem> {
        self.items.iter().find(|it| it.photo == photo)
    }

    /// Place `item` on top of the stack. No-op (returns `false`) if its photo is
    /// already on the board (a photo is on a board at most once) or its placement
    /// is non-finite (which would corrupt the persisted view).
    pub fn place(&mut self, item: BoardItem) -> bool {
        if self.contains(item.photo) || !item.is_finite() {
            return false;
        }
        self.items.push(item);
        true
    }

    /// Re-insert `item` at stack index `at` (clamped) — the inverse of removing
    /// it, restoring its original z. No-op if its photo is already present or the
    /// placement is non-finite.
    pub fn insert_at(&mut self, at: usize, item: BoardItem) -> bool {
        if self.contains(item.photo) || !item.is_finite() {
            return false;
        }
        let at = at.min(self.items.len());
        self.items.insert(at, item);
        true
    }

    /// Remove `photo`, returning its stack index and item so the removal can be
    /// inverted. `None` if it wasn't placed.
    pub fn remove(&mut self, photo: PhotoId) -> Option<(usize, BoardItem)> {
        let at = self.index_of(photo)?;
        Some((at, self.items.remove(at)))
    }

    /// Move `photo` to `pos`, returning its previous position. `None` if it
    /// wasn't placed, the position is unchanged, or `pos` is non-finite.
    pub fn move_to(&mut self, photo: PhotoId, pos: Pos) -> Option<Pos> {
        if !pos.is_finite() {
            return None;
        }
        let it = self.items.iter_mut().find(|it| it.photo == photo)?;
        if it.pos == pos {
            return None;
        }
        let before = it.pos;
        it.pos = pos;
        Some(before)
    }

    /// Bring `photo` to the top of the stack (the last slot paints on top).
    /// Returns whether the order changed — `false` if it wasn't placed or was
    /// already on top.
    pub fn raise(&mut self, photo: PhotoId) -> bool {
        let Some(at) = self.index_of(photo) else {
            return false;
        };
        if at + 1 == self.items.len() {
            return false; // already on top
        }
        let item = self.items.remove(at);
        self.items.push(item);
        true
    }

    /// Drop every item whose photo is in `ids` — the missing-file prune. Not an
    /// undoable mutation; the undo timeline is scrubbed separately.
    pub fn forget(&mut self, ids: &std::collections::HashSet<PhotoId>) {
        self.items.retain(|it| !ids.contains(&it.photo));
    }
}
