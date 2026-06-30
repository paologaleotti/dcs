//! The command registry enum: one serializable record
//! type that every surface — keys, palette, menus — dispatches. Defined in the
//! pure core so both `dcs-app` (which owns the undo stack) and `dcs-io` (which
//! appends/replays the durable `undo.log`) depend *down* on it, never on each
//! other.
//!
//! Commands carry `PhotoId` **sets**; dispatch dedups them to unique photos
//! before building an undo entry — never projection identities or grid
//! positions.
//!
//! **Append-only.** Once a variant is serialized to `undo.log` it must never be
//! removed or renamed, or replay of existing logs breaks. New variants are
//! added at the end.
//!
//! Reversibility is carried by [`Patch`] (the per-command delta set), not by the
//! `Command` itself: `Command` is the intent, a `Patch` is the concrete,
//! invertible record of what that intent changed. `dcs-app` records one `Patch`
//! per applied command; `dcs-io` persists it.

use serde::{Deserialize, Serialize};

use crate::crops::CropEdit;
use crate::cull::AcceptState;
use crate::photo::PhotoId;
use crate::tag::{Color, Tag, TagId};
use crate::view::{BoardItem, Pos, ViewId};

/// A single undoable mutation, as dispatched by a command surface. Verdict and
/// tag intents; board variants land with the board view, added at the end.
///
/// Tag intents carry only what the user expressed — `CreateTag` has no id, since
/// the id is allocated by dispatch and captured in the resulting [`Patch`], so
/// replay stays deterministic from the recorded deltas, never from re-running a
/// command.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Command {
    /// Set every listed photo to one verdict. The `Vec` may contain duplicates
    /// (tag-band projections of one photo); dispatch dedups first.
    SetState(Vec<PhotoId>, AcceptState),
    /// Add a tag to every listed photo (no-op per already-tagged photo).
    AssignTag(TagId, Vec<PhotoId>),
    /// Remove a tag from every listed photo (no-op per untagged photo).
    UnassignTag(TagId, Vec<PhotoId>),
    /// Create a new tag. The id is allocated by dispatch.
    CreateTag { name: String, color: Color },
    /// Rename a tag. Renaming onto an existing name merges into that tag
    /// (merge = rename-to-existing); dispatch decides.
    RenameTag(TagId, String),
    /// Merge `from` into `into`: reassign `from`'s photos to `into`, drop `from`.
    MergeTags { into: TagId, from: TagId },
    /// Delete a tag and all its assignments.
    DeleteTag(TagId),
    /// Recolor a tag.
    SetTagColor(TagId, Color),
    /// Set (or clear, with `None`) the crop+straighten edit on every listed
    /// photo. The `Vec` may contain duplicates; dispatch dedups first.
    SetCrop(Vec<PhotoId>, Option<CropEdit>),
    /// Place photos on a board at the given scene positions (a drop). Photos
    /// already on the board are skipped — membership is a set. Dispatch dedups
    /// the list to unique photos first.
    AddToBoard(ViewId, Vec<(PhotoId, Pos)>),
    /// Remove photos from a board. Untouched if a photo isn't placed.
    RemoveFromBoard(ViewId, Vec<PhotoId>),
    /// Move placed photos to new scene positions — one entry per dragged photo,
    /// coalesced from a whole drag gesture so the drop is a single undo step.
    MoveOnBoard(ViewId, Vec<(PhotoId, Pos)>),
}

/// One reversible verdict change: the photo, its verdict before, and after.
pub type VerdictChange = (PhotoId, AcceptState, AcceptState);

/// One reversible crop change: the photo, its edit before, and after. `None`
/// means uncropped, so this captures setting, changing, and clearing a crop.
pub type CropChange = (PhotoId, Option<CropEdit>, Option<CropEdit>);

/// One reversible board change, recorded as the *forward* mutation. A store
/// replays it forward to apply and backward to revert — no separate inverse
/// type, mirroring [`CropChange`]. Carries the full [`BoardItem`] on add/remove
/// so reverting restores the original position, scale, and stacking index.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum BoardDelta {
    /// A photo was placed on top of the board. Revert: remove it.
    Added(ViewId, BoardItem),
    /// A photo was removed from stack index `usize`. Revert: re-insert it there.
    Removed(ViewId, usize, BoardItem),
    /// A photo moved from `before` to `after`. Revert: move it back.
    Moved(ViewId, PhotoId, Pos, Pos),
}

impl BoardDelta {
    /// The view this delta concerns.
    pub fn view(&self) -> ViewId {
        match self {
            BoardDelta::Added(v, _)
            | BoardDelta::Removed(v, _, _)
            | BoardDelta::Moved(v, _, _, _) => *v,
        }
    }

    /// The photo this delta concerns.
    pub fn photo(&self) -> PhotoId {
        match self {
            BoardDelta::Added(_, item) | BoardDelta::Removed(_, _, item) => item.photo,
            BoardDelta::Moved(_, p, _, _) => *p,
        }
    }
}

/// One reversible tag change. Granular and self-inverting so a command that
/// touches many photos or several defs records a flat list of these, and undo
/// is "apply the inverse of each, in reverse order".
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum TagDelta {
    /// A photo gained a tag. Inverse: `Unassigned`.
    Assigned(TagId, PhotoId),
    /// A photo lost a tag. Inverse: `Assigned`.
    Unassigned(TagId, PhotoId),
    /// A tag definition was added. Inverse: `Removed`.
    Created(Tag),
    /// A tag definition was removed. Inverse: `Created`.
    Removed(Tag),
    /// A tag was renamed. Inverse: swap before/after.
    Renamed {
        id: TagId,
        before: String,
        after: String,
    },
    /// A tag was recolored. Inverse: swap before/after.
    Recolored {
        id: TagId,
        before: Color,
        after: Color,
    },
}

impl TagDelta {
    /// The inverse delta — applying it undoes `self`.
    pub fn invert(&self) -> TagDelta {
        match self {
            TagDelta::Assigned(t, p) => TagDelta::Unassigned(*t, *p),
            TagDelta::Unassigned(t, p) => TagDelta::Assigned(*t, *p),
            TagDelta::Created(tag) => TagDelta::Removed(tag.clone()),
            TagDelta::Removed(tag) => TagDelta::Created(tag.clone()),
            TagDelta::Renamed { id, before, after } => TagDelta::Renamed {
                id: *id,
                before: after.clone(),
                after: before.clone(),
            },
            TagDelta::Recolored { id, before, after } => TagDelta::Recolored {
                id: *id,
                before: *after,
                after: *before,
            },
        }
    }
}

/// The concrete, invertible record of one applied command — what the undo stack
/// holds and the durable log persists. One `Patch` per command; `Verdict` and
/// `Tag` never mix in a single patch, so the two stores stay independent while
/// sharing one undo timeline.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Patch {
    /// Verdict deltas, one per changed photo (deduped to unique photos).
    Verdict(Vec<VerdictChange>),
    /// Tag deltas, in apply order; undo inverts each in reverse.
    Tag(Vec<TagDelta>),
    /// Crop deltas, one per changed photo (deduped to unique photos).
    Crop(Vec<CropChange>),
    /// Board deltas (place/remove/move), in apply order; undo reverts each in
    /// reverse. One patch per board command — a coalesced drag is one patch.
    Board(Vec<BoardDelta>),
}

impl Patch {
    /// Whether this patch changed nothing — an empty patch is never recorded, so
    /// a no-op command can't be "undone" into a surprise.
    pub fn is_empty(&self) -> bool {
        match self {
            Patch::Verdict(c) => c.is_empty(),
            Patch::Tag(d) => d.is_empty(),
            Patch::Crop(c) => c.is_empty(),
            Patch::Board(d) => d.is_empty(),
        }
    }
}
