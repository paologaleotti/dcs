//! Contact-sheet trigger types. The session resolves a scope into the in-scope
//! photos (reusing [`crate::export::ExportScope`]), calls the pure planner for
//! the live preview, and on confirm hands the plan to the `dcs-io` renderer —
//! the methods live on `Session` (`session/store.rs`) where they can read its
//! private state.

/// Live progress of a running or finished contact-sheet render, read by the
/// dialog each frame.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ContactSheetStatus {
    pub total_pages: usize,
    pub rendered: usize,
    pub failed: usize,
    pub running: bool,
    /// True once the run finished with the PDF written (no failure). Drives the
    /// "open folder / print" completion state.
    pub succeeded: bool,
}

impl ContactSheetStatus {
    /// Pages processed so far — the progress numerator.
    pub fn done(&self) -> usize {
        self.rendered + self.failed
    }
}
