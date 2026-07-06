//! Export-contact-sheet dialog state. Holds the staged settings; the live
//! preview and the render share one `ContactSheetPlan` from the conductor, so
//! the printed/exported sheet is exactly the preview. Rendering lives on
//! `DcsApp` (`app/dialogs.rs`).

use std::path::PathBuf;

use dcs_app::{
    CellCaption, ContactSheetSettings, ExportScope, GridMode, PaperOrientation, PaperSize,
    SheetBackground,
};

/// A paper preset the dialog offers, resolved to a `PaperSize` on demand.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PaperKind {
    A4,
    A3,
    Letter,
    Legal,
}

impl PaperKind {
    pub const ALL: [PaperKind; 4] = [
        PaperKind::A4,
        PaperKind::A3,
        PaperKind::Letter,
        PaperKind::Legal,
    ];

    pub fn label(self) -> &'static str {
        match self {
            PaperKind::A4 => "A4",
            PaperKind::A3 => "A3",
            PaperKind::Letter => "Letter",
            PaperKind::Legal => "Legal",
        }
    }

    pub fn size(self) -> PaperSize {
        match self {
            PaperKind::A4 => PaperSize::A4,
            PaperKind::A3 => PaperSize::A3,
            PaperKind::Letter => PaperSize::LETTER,
            PaperKind::Legal => PaperSize::LEGAL,
        }
    }
}

/// The dialog's current selections, persisted across opens so re-exporting a
/// refined cull is one confirm.
pub struct ContactSheetDialog {
    pub open: bool,
    pub scope: ExportScope,
    pub paper: PaperKind,
    pub orientation: PaperOrientation,
    pub background: SheetBackground,
    /// Fixed column count (rows derive to fit). The domain also supports a
    /// target-edge mode; the dialog exposes columns as the simpler control.
    pub columns: u32,
    pub caption: CellCaption,
    pub margin_mm: f32,
    pub gutter_mm: f32,
    pub title: String,
    pub title_on: bool,
    /// Which page the live preview shows.
    pub preview_page: usize,
    /// The last destination rendered to, for the finished-state "Reveal".
    pub last_dest: Option<PathBuf>,
}

impl Default for ContactSheetDialog {
    fn default() -> Self {
        ContactSheetDialog {
            open: false,
            scope: ExportScope::Everything,
            paper: PaperKind::A4,
            orientation: PaperOrientation::Landscape,
            background: SheetBackground::White,
            columns: 6,
            caption: CellCaption {
                number: true,
                filename: true,
                exposure: false,
            },
            margin_mm: 6.0,
            gutter_mm: 3.0,
            title: String::new(),
            title_on: false,
            preview_page: 0,
            last_dest: None,
        }
    }
}

impl ContactSheetDialog {
    /// The resolved settings for a given destination. The preview passes a
    /// placeholder path (so it renders without a chosen destination); export and
    /// print pass the real target.
    pub fn settings(&self, dest: PathBuf) -> ContactSheetSettings {
        let title =
            (self.title_on && !self.title.trim().is_empty()).then(|| self.title.trim().to_string());
        ContactSheetSettings {
            paper: self.paper.size(),
            orientation: self.orientation,
            background: self.background,
            grid: GridMode::FixedColumns(self.columns.max(1)),
            margin_pt: mm_to_pt(self.margin_mm),
            gutter_pt: mm_to_pt(self.gutter_mm),
            caption: self.caption,
            number_from: 1,
            title,
            dest,
        }
    }
}

fn mm_to_pt(mm: f32) -> f32 {
    mm * 72.0 / 25.4
}
