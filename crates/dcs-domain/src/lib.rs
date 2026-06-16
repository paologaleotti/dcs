//! dcs-domain — PURE core: types + pure functions, no I/O, no async, no egui.
//!
//! Current slice: photo identity + pairing, time sort, the shared thumbnail
//! pixel type. Grouping, bursts, filters, tags, and the export planner land in
//! later slices (§9, §11).
//!
//! Bottom of the dependency tree — depends on no internal crate.

pub mod command;
pub mod cull;
pub mod fingerprint;
pub mod fuzzy;
pub mod pairing;
pub mod photo;
pub mod sort;
pub mod thumb;
pub mod timezone;
