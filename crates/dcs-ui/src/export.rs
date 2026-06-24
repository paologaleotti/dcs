//! Export dialog state. Holds the staged settings; the live preview
//! and the run share one `ExportPlan` from the conductor, so what the dialog
//! says is exactly what gets copied. Rendering lives on `DcsApp` (`app.rs`).

use std::path::PathBuf;

use dcs_app::{Collision, ExportRequest, ExportScope, FileSelection, Layout, NameTemplate};

/// The dialog's current selections, persisted across opens so re-exporting a
/// refined cull is one confirm.
pub struct ExportDialog {
    pub open: bool,
    pub scope: ExportScope,
    pub files: FileSelection,
    pub layout: Layout,
    pub collision: Collision,
    pub template_on: bool,
    pub template: String,
    pub sidecars: bool,
    pub dest: Option<PathBuf>,
}

impl Default for ExportDialog {
    fn default() -> Self {
        ExportDialog {
            open: false,
            scope: ExportScope::Everything,
            files: FileSelection::Any,
            layout: Layout::Together,
            collision: Collision::Rename,
            template_on: false,
            template: String::new(),
            sidecars: false,
            dest: None,
        }
    }
}

impl ExportDialog {
    /// The resolved request, or `None` until a destination is chosen.
    pub fn request(&self) -> Option<ExportRequest> {
        let dest = self.dest.clone()?;
        let template = (self.template_on && !self.template.trim().is_empty())
            .then(|| NameTemplate(self.template.clone()));
        Some(ExportRequest {
            dest,
            files: self.files,
            layout: self.layout,
            collision: self.collision,
            template,
            sidecars: self.sidecars,
        })
    }
}
