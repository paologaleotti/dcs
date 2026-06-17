//! Export trigger types. The session resolves a scope into the
//! in-scope photos, calls the pure planner for the live preview, and on confirm
//! hands the plan to the `dcs-io` executor — the methods live on `Session`
//! (`session.rs`) where they can read its private state.

/// Which photos an export covers. Tag- and filter-chip scopes arrive with
/// the Tags slice; these key on selection and verdict only.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExportScope {
    /// The current multi-select.
    Selection,
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
