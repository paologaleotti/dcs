//! Verdict type. Accept/Reject/Unreviewed is a per-photo *owned* fact
//! — true in every view, mutated only through commands, fully undoable.
//! Pure: the domain defines the type; the owned store and undo policy live in
//! `dcs-app`.

use serde::{Deserialize, Serialize};

/// Cull verdict for one photo. `Unreviewed` is the default and the working
/// filter; `A`/`X` toggle to `Accepted`/`Rejected` and back.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum AcceptState {
    #[default]
    Unreviewed,
    Accepted,
    Rejected,
}
