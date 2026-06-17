//! The command registry enum: one serializable record
//! type that every surface — keys, palette, menus — dispatches. Defined in the
//! pure core so both `dcs-app` (which owns the undo stack) and `dcs-io` (which
//! will append/replay the durable `undo.log`) depend *down* on it, never on
//! each other.
//!
//! Commands carry `PhotoId` **sets**; dispatch dedups them to unique photos
//! before building an undo entry — never projection identities or grid
//! positions.
//!
//! **Append-only.** Once a variant is serialized to `undo.log` it must never be
//! removed or renamed, or replay of existing logs breaks.

use serde::{Deserialize, Serialize};

use crate::cull::AcceptState;
use crate::photo::PhotoId;

/// A single undoable mutation. This phase defines the verdict mutation only;
/// tag and board variants land in later slices and are added, not
/// reshaped.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Command {
    /// Set every listed photo to one verdict. The `Vec` may contain duplicates
    /// (tag-band projections of one photo); dispatch dedups first.
    SetState(Vec<PhotoId>, AcceptState),
}
