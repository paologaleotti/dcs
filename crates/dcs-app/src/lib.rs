//! dcs-app — conductor between the UI and `dcs-io`. The UI talks only to this.
//!
//! Current slice: `Session` owns the scanning/decode pipeline, the thumbnail
//! caches, the command registry, the durable undo stack, and the export trigger.
//!
//! Depends DOWN on dcs-io + dcs-domain. Never the reverse.

pub mod cull;
pub mod export;
pub mod history;
pub mod registry;
pub mod selection;
pub mod session;
pub mod tags;
pub mod thumb_cache;
mod util;

pub use export::{ExportScope, ExportStatus};
pub use registry::{ActionEffect, ActionEntry, AppAction, Category, catalog};
pub use session::{
    BurstMark, CaptionTime, CellInfo, ImportProgress, SaveError, Session, VerdictFilter,
    VisibleGroup,
};
pub use thumb_cache::ThumbView;

// Domain types surfaced through `AppAction`/`Session`, so the UI names them via
// the conductor rather than reaching into `dcs-domain`.
pub use dcs_domain::burst::BurstKnobs;
pub use dcs_domain::export::{
    Collision, ExportError, ExportPlan, ExportRequest, FileSelection, Layout, NameTemplate,
    SkipReason, SkippedPhoto,
};
pub use dcs_domain::grouping::{Axis, TimeGranularity};
pub use dcs_domain::sort::{Sort, SortDir, SortKey};
pub use dcs_domain::tag::{Color, Tag, TagId};
