//! dcs-domain — PURE core.
//!
//! Types + pure functions: `Photo`, `Pool`, `Tag`, grouping, `derive_bursts`,
//! timezone adjustment, filter resolution, fuzzy match, tag merge, the pure
//! export planner (`plan_export` -> `ExportPlan`), and the `Command` enum.
//! No I/O, no async, no egui. Owns its own error enums. (§9, §11)
//!
//! Bottom of the dependency tree — depends on no internal crate.

pub fn placeholder() {}
