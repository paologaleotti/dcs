//! Export trigger types. The session resolves a scope into the
//! in-scope photos, calls the pure planner for the live preview, and on confirm
//! hands the plan to the `dcs-io` executor — the methods live on `Session`
//! (`session.rs`) where they can read its private state.

/// Which photos an export covers. The verdict shortcuts are pre-built filters
/// surfaced for the common cases; `CurrentFilter` exports exactly what the active
/// chips resolve to, so the dialog and the grid never disagree.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExportScope {
    /// The current multi-select.
    Selection,
    /// Whatever the active chip filter currently resolves to.
    CurrentFilter,
    Accepted,
    Rejected,
    Unreviewed,
    /// Accepted plus still-unreviewed — a half-finished cull, kept whole.
    AcceptedAndUnreviewed,
    /// Every photo in the pool.
    Everything,
}

/// Live progress of a running or finished export, read by the dialog each frame.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ExportStatus {
    pub total: usize,
    pub copied: usize,
    pub skipped: usize,
    pub failed: usize,
    pub running: bool,
}

impl ExportStatus {
    /// Files processed so far — the progress numerator.
    pub fn done(&self) -> usize {
        self.copied + self.skipped + self.failed
    }
}
