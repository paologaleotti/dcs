//! Pure contact-sheet planner. Takes the resolved dialog settings
//! (`ContactSheetSettings`) plus the in-scope photos (`SheetItem`s the conductor
//! builds from the current selection/filter) and decides *everything*: the page
//! grid, pagination, per-frame rectangles, sequential frame numbers, the caption
//! under each frame, and the dry-run sentence. No disk access, no pixels — it
//! only decides geometry. `dcs-io` renders the resulting `ContactSheetPlan` into
//! a PDF verbatim and the dialog paints the *same* plan as a live preview, so
//! preview and print are the one artifact.
//!
//! All geometry is in PostScript points (1 pt = 1/72"), the PDF- and print-native
//! unit; millimetre/inch paper presets convert to points on construction. The
//! domain never sees a photo's pixel dimensions: every cell is a uniform box and
//! the pixel-owning layer contain-fits the thumbnail inside it. `Orientation`
//! supplies an advisory `expect_portrait` hint only.

use std::path::PathBuf;

use thiserror::Error;

use crate::photo::{Orientation, PhotoId};

/// A paper size, stored canonically in points. Portrait is canonical;
/// `oriented` swaps width/height for landscape.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PaperSize {
    pub width_pt: f32,
    pub height_pt: f32,
}

impl PaperSize {
    pub const A4: PaperSize = PaperSize {
        width_pt: 595.276,
        height_pt: 841.890,
    };
    pub const A3: PaperSize = PaperSize {
        width_pt: 841.890,
        height_pt: 1190.551,
    };
    pub const LETTER: PaperSize = PaperSize {
        width_pt: 612.0,
        height_pt: 792.0,
    };
    pub const LEGAL: PaperSize = PaperSize {
        width_pt: 612.0,
        height_pt: 1008.0,
    };

    /// Build a paper size from millimetres (portrait, `w_mm` × `h_mm`).
    pub fn from_mm(w_mm: f32, h_mm: f32) -> Self {
        PaperSize {
            width_pt: mm_to_pt(w_mm),
            height_pt: mm_to_pt(h_mm),
        }
    }

    /// The `(width, height)` in points after applying the orientation.
    pub fn oriented(self, orientation: PaperOrientation) -> (f32, f32) {
        match orientation {
            PaperOrientation::Portrait => (self.width_pt, self.height_pt),
            PaperOrientation::Landscape => (self.height_pt, self.width_pt),
        }
    }
}

/// Page orientation. Landscape swaps the paper's width and height.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PaperOrientation {
    Portrait,
    Landscape,
}

/// The sheet background — a black rebate (classic darkroom) or white paper.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SheetBackground {
    Black,
    White,
}

impl SheetBackground {
    /// The word used in the dry-run summary.
    pub fn label(self) -> &'static str {
        match self {
            SheetBackground::Black => "black",
            SheetBackground::White => "white",
        }
    }
}

/// How the grid columns are sized. `FixedColumns` pins the column count and
/// derives the rows that fit; `TargetCellEdge` picks the column count so cells
/// land near a target edge (the density slider). Rows always paginate.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum GridMode {
    FixedColumns(u32),
    TargetCellEdge(f32),
}

/// What text rides in the strip under each frame — composable. The frame number
/// is the prominent label; the filename rides beside it dimmed, and the exposure
/// is a dimmed line below. All-off leaves no caption strip.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CellCaption {
    pub number: bool,
    pub filename: bool,
    pub exposure: bool,
}

impl CellCaption {
    /// The classic contact-sheet default: frame number only.
    pub fn numbers_only() -> Self {
        CellCaption {
            number: true,
            filename: false,
            exposure: false,
        }
    }

    /// Whether any caption element is enabled.
    pub fn any(self) -> bool {
        self.number || self.filename || self.exposure
    }
}

/// The resolved dialog settings. The conductor builds this; the planner consumes
/// it. Scope lives in the `SheetItem` list, not here.
#[derive(Debug, Clone, PartialEq)]
pub struct ContactSheetSettings {
    pub paper: PaperSize,
    pub orientation: PaperOrientation,
    pub background: SheetBackground,
    pub grid: GridMode,
    /// Outer page margin, points.
    pub margin_pt: f32,
    /// Gap between cells, points.
    pub gutter_pt: f32,
    pub caption: CellCaption,
    /// The first frame's number (usually 1).
    pub number_from: u32,
    /// Optional header printed at the top of every page (alongside the app mark).
    pub title: Option<String>,
    pub dest: PathBuf,
}

/// One in-scope photo handed to the planner, in resolved visible order.
/// Pixel-free: identity, orientation (for the fit hint), and the caption source
/// strings the conductor pre-formats from the photo (borrowed, like `ExportItem`).
#[derive(Debug, Clone, Copy)]
pub struct SheetItem<'a> {
    pub id: PhotoId,
    pub orientation: Orientation,
    /// The filename stem shown when the filename caption is on.
    pub name: &'a str,
    /// The exposure line (`f/2.8 · 1/250 · ISO 400`) shown when that caption is
    /// on. The conductor supplies it already formatted.
    pub exposure: Option<&'a str>,
}

/// A rectangle in page points, origin top-left. The shared coordinate space:
/// the UI maps it into egui pixels, `dcs-io` maps it into PDF/device space.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RectPt {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

/// One placed frame on a page. `image_rect` is the uniform 3:2 box the thumbnail
/// contain-fits inside; `caption_rect` is the reserved strip beneath (zero-height
/// when no caption). The caption is the bold frame number with a dimmed
/// `(filename)` beside it and a dimmed exposure line below. `expect_portrait` is
/// an advisory orientation hint, never geometry.
#[derive(Debug, Clone, PartialEq)]
pub struct CellPlacement {
    pub id: PhotoId,
    pub frame_number: u32,
    pub image_rect: RectPt,
    pub caption_rect: RectPt,
    /// The frame number as text, or empty when the number caption is off. Shares
    /// the first caption line with `filename` (rendered `12 (DSCF1234)`).
    pub number_text: String,
    /// The filename, when that caption is on — rides on the number line, dimmed.
    pub filename: Option<String>,
    /// The exposure line, when that caption is on and known — a dimmed line below.
    pub exposure: Option<String>,
    pub expect_portrait: bool,
}

/// One page of the sheet: its size, background, the top header band (app mark +
/// optional title), the footer, and the placed cells.
#[derive(Debug, Clone, PartialEq)]
pub struct PagePlan {
    pub index: usize,
    pub size_pt: (f32, f32),
    pub background: SheetBackground,
    /// The small top band, always reserved, for the app mark and optional title.
    pub header_rect: RectPt,
    pub title: Option<String>,
    pub footer_rect: RectPt,
    pub footer: String,
    pub cells: Vec<CellPlacement>,
}

/// The fully-decided plan: the pages, the derived grid shape, and the dry-run
/// sentence. Everything the preview paints and the renderer draws comes from
/// this one artifact.
#[derive(Debug, Clone, PartialEq)]
pub struct ContactSheetPlan {
    pub pages: Vec<PagePlan>,
    pub cols: u32,
    pub rows_per_page: u32,
    /// The uniform cell box (image + caption strip together), points.
    pub cell_size_pt: (f32, f32),
    pub frame_count: usize,
    pub dest: PathBuf,
    /// One-sentence restatement, e.g.
    /// `48 frames on 4 A4 pages (landscape), 6×4, black background.`
    pub summary: String,
}

/// Why a plan could not be produced. Domain-owned; no I/O concepts.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ContactSheetError {
    /// No photos in scope.
    #[error("nothing in scope for the contact sheet")]
    EmptyScope,
    /// The page has no positive area left after margins, header, and footer.
    #[error("page too small for the chosen margins")]
    NoUsableArea,
    /// The cell target (or column count) leaves no room for even one cell.
    #[error("cells too large to fit the page")]
    CellTooLarge,
}

/// Plan a contact sheet: decide the page grid, pagination, frame rectangles,
/// sequential numbering, and captions from the in-scope `items` and `settings`.
/// Pure — no disk, no pixels. Frames flow left→right, top→bottom, paginating
/// down, numbered sequentially in `items` order (the conductor passes the
/// resolved visible order).
pub fn plan_contact_sheet(
    items: &[SheetItem],
    settings: &ContactSheetSettings,
) -> Result<ContactSheetPlan, ContactSheetError> {
    if items.is_empty() {
        return Err(ContactSheetError::EmptyScope);
    }

    let (page_w, page_h) = settings.paper.oriented(settings.orientation);
    // A small header band is always reserved (app mark + optional title), so the
    // grid geometry is identical whether or not a title is set.
    let content = RectPt {
        x: settings.margin_pt,
        y: settings.margin_pt + HEADER_H,
        w: page_w - 2.0 * settings.margin_pt,
        h: page_h - 2.0 * settings.margin_pt - HEADER_H - FOOTER_H,
    };
    // Reject non-finite (NaN/inf) dimensions too — they would slip past a plain
    // `<= 0.0` check and divide by zero below.
    if !content.w.is_finite() || !content.h.is_finite() || content.w <= 0.0 || content.h <= 0.0 {
        return Err(ContactSheetError::NoUsableArea);
    }

    let cols = columns(&content, settings)?;
    let cell_w = (content.w - (cols - 1) as f32 * settings.gutter_pt) / cols as f32;
    if !cell_w.is_finite() || cell_w <= 0.0 {
        return Err(ContactSheetError::CellTooLarge);
    }
    let image_h = cell_w * FRAME_H_RATIO;
    let cap_h = caption_height(settings.caption);
    let cell_h = image_h + cap_h;

    let rows_per_page = ((content.h + settings.gutter_pt) / (cell_h + settings.gutter_pt)).floor();
    if !rows_per_page.is_finite() || rows_per_page < 1.0 {
        return Err(ContactSheetError::CellTooLarge);
    }
    let rows_per_page = rows_per_page as u32;

    let per_page = (cols * rows_per_page) as usize;
    let page_count = items.len().div_ceil(per_page);

    let mut pages: Vec<PagePlan> = Vec::with_capacity(page_count);
    for (gi, item) in items.iter().enumerate() {
        let page_idx = gi / per_page;
        let within = gi % per_page;
        let row = (within / cols as usize) as f32;
        let col = (within % cols as usize) as f32;

        if within == 0 {
            pages.push(new_page(page_idx, (page_w, page_h), &content, settings));
        }

        let cell_x = content.x + col * (cell_w + settings.gutter_pt);
        let cell_top = content.y + row * (cell_h + settings.gutter_pt);
        let image_rect = RectPt {
            x: cell_x,
            y: cell_top,
            w: cell_w,
            h: image_h,
        };
        let caption_rect = RectPt {
            x: cell_x,
            y: cell_top + image_h,
            w: cell_w,
            h: cap_h,
        };
        let frame_number = settings.number_from.saturating_add(gi as u32);
        let number_text = if settings.caption.number {
            frame_number.to_string()
        } else {
            String::new()
        };
        let filename = settings.caption.filename.then(|| item.name.to_string());
        let exposure = settings
            .caption
            .exposure
            .then(|| item.exposure.map(str::to_string))
            .flatten();

        pages[page_idx].cells.push(CellPlacement {
            id: item.id,
            frame_number,
            image_rect,
            caption_rect,
            number_text,
            filename,
            exposure,
            expect_portrait: is_portrait(item.orientation),
        });
    }

    // Footer text needs the total page count, known only after pagination.
    for page in &mut pages {
        page.footer = footer_text(page.index, page_count, items.len());
    }

    let summary = summarize(items.len(), page_count, cols, rows_per_page, settings);
    Ok(ContactSheetPlan {
        pages,
        cols,
        rows_per_page,
        cell_size_pt: (cell_w, cell_h),
        frame_count: items.len(),
        dest: settings.dest.clone(),
        summary,
    })
}

/// Transliterate caption text to a WinAnsi-safe ASCII subset. The PDF renderer's
/// built-in fonts encode via WinAnsi (where lopdf passes unmapped codepoints
/// through as raw UTF-8, rendering as mojibake like `·` → `Â·`), so both the
/// renderer and the live preview run text through this to stay identical and
/// legible. Shared here so the two can never drift.
pub fn ascii_caption(s: &str) -> String {
    s.chars()
        .filter_map(|c| match c {
            '·' | '•' | '–' | '—' => Some('-'),
            '…' => Some('.'),
            '’' | '‘' => Some('\''),
            '“' | '”' => Some('"'),
            c if c.is_ascii() => Some(c),
            _ => None,
        })
        .collect()
}

/// Reserved top band for the app mark + optional title, points.
const HEADER_H: f32 = 14.0;
/// Reserved bottom band for the page footer, points.
const FOOTER_H: f32 = 14.0;
/// The image box aspect (height / width). Film contact-sheet frames are 3:2
/// landscape; a landscape photo fills the box and a portrait one letterboxes,
/// which packs more rows per page than a square cell.
const FRAME_H_RATIO: f32 = 2.0 / 3.0;
/// Height reserved for the prominent frame-number line, points.
const NUMBER_LINE_PT: f32 = 7.0;
/// Height reserved for each small detail line (filename, exposure), points.
const DETAIL_LINE_PT: f32 = 6.0;
/// Padding around the caption strip, points.
const CAPTION_PAD_PT: f32 = 1.5;

/// The column count for the content area under the chosen grid mode.
fn columns(content: &RectPt, settings: &ContactSheetSettings) -> Result<u32, ContactSheetError> {
    let cols = match settings.grid {
        GridMode::FixedColumns(n) => n.max(1),
        GridMode::TargetCellEdge(target) => {
            if !target.is_finite() || target <= 0.0 {
                return Err(ContactSheetError::CellTooLarge);
            }
            (((content.w + settings.gutter_pt) / (target + settings.gutter_pt)).floor() as u32)
                .max(1)
        }
    };
    Ok(cols)
}

/// The caption strip height for the enabled caption flags (0 when none). The
/// number and filename share the first (taller) line; the exposure is a second,
/// smaller line — reserved from the flags alone so a missing exposure never
/// shifts the grid.
fn caption_height(caption: CellCaption) -> f32 {
    if !caption.any() {
        return 0.0;
    }
    let line1 = if caption.number || caption.filename {
        NUMBER_LINE_PT
    } else {
        0.0
    };
    let line2 = if caption.exposure {
        DETAIL_LINE_PT
    } else {
        0.0
    };
    line1 + line2 + CAPTION_PAD_PT
}

fn new_page(
    index: usize,
    page_size: (f32, f32),
    content: &RectPt,
    settings: &ContactSheetSettings,
) -> PagePlan {
    let header_rect = RectPt {
        x: settings.margin_pt,
        y: settings.margin_pt,
        w: content.w,
        h: HEADER_H,
    };
    let footer_rect = RectPt {
        x: settings.margin_pt,
        y: page_size.1 - settings.margin_pt - FOOTER_H,
        w: content.w,
        h: FOOTER_H,
    };
    PagePlan {
        index,
        size_pt: page_size,
        background: settings.background,
        header_rect,
        title: settings.title.clone(),
        footer_rect,
        // Filled once the total page count is known.
        footer: String::new(),
        cells: Vec::new(),
    }
}

fn footer_text(page_index: usize, page_count: usize, frames: usize) -> String {
    let noun = if frames == 1 { "frame" } else { "frames" };
    format!(
        "Page {} of {} · {} {}",
        page_index + 1,
        page_count,
        frames,
        noun
    )
}

/// An orientation whose upright pixels are taller than wide.
fn is_portrait(orientation: Orientation) -> bool {
    matches!(
        orientation,
        Orientation::Rotate90
            | Orientation::Rotate270
            | Orientation::Transpose
            | Orientation::Transverse
    )
}

fn summarize(
    frames: usize,
    pages: usize,
    cols: u32,
    rows: u32,
    settings: &ContactSheetSettings,
) -> String {
    let paper = paper_label(settings.paper);
    let orientation = match settings.orientation {
        PaperOrientation::Portrait => "portrait",
        PaperOrientation::Landscape => "landscape",
    };
    let frame_noun = if frames == 1 { "frame" } else { "frames" };
    let page_noun = if pages == 1 { "page" } else { "pages" };
    format!(
        "{frames} {frame_noun} on {pages} {paper} {page_noun} ({orientation}), {cols}×{rows}, {} background.",
        settings.background.label()
    )
}

/// Name a paper size by matching its point dimensions to the known presets
/// (order-independent), falling back to `custom`.
fn paper_label(paper: PaperSize) -> &'static str {
    let matches = |p: PaperSize| {
        let (a, b) = (paper.width_pt, paper.height_pt);
        let (c, d) = (p.width_pt, p.height_pt);
        (near(a, c) && near(b, d)) || (near(a, d) && near(b, c))
    };
    if matches(PaperSize::A4) {
        "A4"
    } else if matches(PaperSize::A3) {
        "A3"
    } else if matches(PaperSize::LETTER) {
        "Letter"
    } else if matches(PaperSize::LEGAL) {
        "Legal"
    } else {
        "custom"
    }
}

fn near(a: f32, b: f32) -> bool {
    (a - b).abs() < 1.0
}

fn mm_to_pt(mm: f32) -> f32 {
    mm * 72.0 / 25.4
}
