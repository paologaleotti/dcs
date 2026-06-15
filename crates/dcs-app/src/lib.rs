//! dcs-app — conductor.
//!
//! Session (active view settings, selection, zoom, gallery state), the single
//! command registry (keys, palette, menus all consume it), dispatch -> durable
//! undo stack -> io effects -> debounced save. Thin export trigger: gathers
//! dialog state into an `ExportRequest`, calls the pure planner for the live
//! dry-run, hands the `ExportPlan` to dcs-io on confirm. (§9)
//!
//! Depends DOWN on dcs-io + dcs-domain. Never the reverse.

pub fn placeholder() {}
