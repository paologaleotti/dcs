//! dcs-app — conductor between the UI and `dcs-io`. The UI talks only to this.
//!
//! Current slice: `Session` owns the scanning/decode pipeline and the
//! thumbnail caches. The command registry, undo stack, and export trigger land
//! in later slices (§9).
//!
//! Depends DOWN on dcs-io + dcs-domain. Never the reverse.

pub mod cull;
pub mod registry;
pub mod selection;
pub mod session;
mod util;

pub use registry::{ActionEffect, ActionEntry, AppAction, Category, catalog};
pub use session::{CellInfo, SaveError, Session, VerdictFilter, VisibleGroup};

// Derived display-setting types surfaced through `AppAction`/`Session`, so the
// UI names them via the conductor rather than reaching into `dcs-domain`.
pub use dcs_domain::grouping::{Axis, TimeGranularity};
pub use dcs_domain::sort::{Sort, SortDir, SortKey};
