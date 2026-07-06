//! The dumb contact-sheet renderer. Walks a finished `ContactSheetPlan` on a
//! worker thread, decodes each frame's thumbnail, composes one PDF page per
//! `PagePlan` (background fill, contain-fit JPEG thumbnails, monospace Courier
//! captions), and writes the PDF atomically. It makes no layout decisions — the
//! pure planner settled every rectangle, number, and caption; this only turns
//! that geometry into pixels and PDF operators.
//!
//! Text is drawn with printpdf's built-in Courier (a standard-14 PDF font), for
//! the analog film-rebate look, so it stays crisp vector glyphs at any print DPI
//! and no font file is bundled — and no `egui` type ever reaches this layer.

use std::collections::HashMap;
use std::fs::{self, File};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;

use crossbeam_channel::{Receiver, unbounded};
use dcs_domain::contact_sheet::{
    CellPlacement, ContactSheetPlan, PagePlan, RectPt, SheetBackground, ascii_caption,
};
use dcs_domain::photo::{Orientation, PhotoId};
use printpdf::{
    BuiltinFont, Color, ImageOptimizationOptions, LinePoint, Op, PaintMode, PdfDocument,
    PdfFontHandle, PdfPage, PdfSaveOptions, Point, Polygon, PolygonRing, Pt, RawImage,
    RawImageData, RawImageFormat, Rgb, TextItem, WindingOrder, XObjectTransform,
};
use rayon::prelude::*;

use crate::imaging;

/// Where the renderer finds the file to decode for each photo. The domain plan
/// carries only `PhotoId`s; the conductor supplies the decodable path and
/// orientation per id (exactly as `ExportOp` carries a concrete source path).
/// Photos absent from the map (RAW-only, missing) render as a placeholder frame.
pub type SheetThumbSource = HashMap<PhotoId, ThumbSrc>;

/// The file backing one frame's thumbnail.
#[derive(Debug, Clone)]
pub struct ThumbSrc {
    pub path: PathBuf,
    pub orientation: Orientation,
}

/// One renderer outcome. `PageRendered` streams per-page progress; `Failed`
/// carries a message for the toast (a decode issue is drawn as a placeholder,
/// not a failure — only writing the PDF can fail the run).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ContactSheetEvent {
    PageRendered { index: usize },
    Failed { error: String },
}

/// Live handle to a running render. Poll for events each frame; cancel stops the
/// worker after the current page and writes nothing.
pub struct ContactSheetHandle {
    rx: Receiver<ContactSheetEvent>,
    cancel: Arc<AtomicBool>,
    done: Arc<AtomicBool>,
    total: usize,
}

/// Render `plan` to a PDF on a worker thread, decoding thumbnails via `thumbs`.
/// Returns immediately; the UI drains [`ContactSheetHandle::poll`] each frame.
pub fn run_contact_sheet(plan: ContactSheetPlan, thumbs: SheetThumbSource) -> ContactSheetHandle {
    let (tx, rx) = unbounded();
    let cancel = Arc::new(AtomicBool::new(false));
    let done = Arc::new(AtomicBool::new(false));
    let total = plan.pages.len();

    let worker_cancel = Arc::clone(&cancel);
    let worker_done = Arc::clone(&done);
    thread::spawn(move || {
        let _guard = DoneGuard(worker_done);
        let mut doc = PdfDocument::new("Contact Sheet");
        let mut pages: Vec<PdfPage> = Vec::with_capacity(plan.pages.len());

        for page in &plan.pages {
            if worker_cancel.load(Ordering::Acquire) {
                // Cancelled mid-render: write nothing, leave no file.
                return;
            }
            let ops = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                render_page(&mut doc, page, &thumbs)
            }));
            match ops {
                Ok(ops) => {
                    let (w_mm, h_mm) = (pt_to_mm(page.size_pt.0), pt_to_mm(page.size_pt.1));
                    pages.push(PdfPage::new(printpdf::Mm(w_mm), printpdf::Mm(h_mm), ops));
                    if tx
                        .send(ContactSheetEvent::PageRendered { index: page.index })
                        .is_err()
                    {
                        return;
                    }
                }
                Err(_) => {
                    let _ = tx.send(ContactSheetEvent::Failed {
                        error: format!("render page {}: worker panicked", page.index + 1),
                    });
                    return;
                }
            }
        }

        doc.with_pages(pages);
        // JPEG-compress embedded thumbnails (DCTDecode) at high quality so a
        // whole-roll sheet stays a few MB and writes fast.
        let save_opts = PdfSaveOptions {
            image_optimization: Some(ImageOptimizationOptions {
                quality: Some(0.9),
                ..ImageOptimizationOptions::default()
            }),
            ..PdfSaveOptions::default()
        };
        let bytes = doc.save(&save_opts, &mut Vec::new());
        if let Err(e) = atomic_write(&plan.dest, &bytes) {
            let _ = tx.send(ContactSheetEvent::Failed { error: e });
        }
    });

    ContactSheetHandle {
        rx,
        cancel,
        done,
        total,
    }
}

impl ContactSheetHandle {
    /// Take every event produced since the last call. Non-blocking.
    pub fn poll(&self) -> Vec<ContactSheetEvent> {
        self.rx.try_iter().collect()
    }

    /// True while pages are still being rendered.
    pub fn is_running(&self) -> bool {
        !self.done.load(Ordering::Acquire)
    }

    /// Total pages in the plan — the progress denominator.
    pub fn total(&self) -> usize {
        self.total
    }

    /// Request cancellation; the worker stops after the in-flight page.
    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::Release);
    }
}

/// Dropping the handle cancels the worker: a render nobody can observe (folder
/// swapped, a new render started) must not keep working or write a stale PDF.
impl Drop for ContactSheetHandle {
    fn drop(&mut self) {
        self.cancel();
    }
}

struct DoneGuard(Arc<AtomicBool>);

impl Drop for DoneGuard {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Release);
    }
}

/// Target long-edge pixel size for a decoded frame thumbnail. High enough that
/// print-size cells stay crisp; the parallel decode keeps a whole roll fast.
const THUMB_EDGE: u32 = 900;
/// Assumed DPI for the embedded-image transform. Cancels out of the final size
/// (we scale to the exact point rect), so any positive value works.
const IMAGE_DPI: f32 = 300.0;

/// The prominent frame-number glyph size — the analog contact-sheet label.
const NUMBER_FONT_PT: f32 = 6.0;
/// Small secondary detail lines (filename, exposure).
const DETAIL_FONT_PT: f32 = 4.5;
/// Header (app mark + title) and footer text.
const HEADER_FONT_PT: f32 = 7.0;
const FOOTER_FONT_PT: f32 = 7.0;
/// Left inset for text inside its band, points.
const TEXT_INSET_PT: f32 = 1.0;
/// Courier glyph advance as a fraction of the font size. Courier is monospaced
/// at exactly 600/1000 em, so this width estimate is exact.
const AVG_GLYPH_FRAC: f32 = 0.6;

/// The app mark stamped in the header of every page.
fn app_mark() -> String {
    format!("dcs v{}", env!("CARGO_PKG_VERSION"))
}

/// Build the PDF operators for one page: background, frames, captions, title,
/// footer. `doc` is borrowed mutably to register each thumbnail image XObject.
fn render_page(doc: &mut PdfDocument, page: &PagePlan, thumbs: &SheetThumbSource) -> Vec<Op> {
    let page_h = page.size_pt.1;
    let mut ops: Vec<Op> = Vec::new();

    // Background fills the whole page. (printpdf's `DrawRectangle` ignores its
    // paint mode and paints nothing, so fill a polygon instead.)
    ops.push(Op::SetFillColor {
        col: solid(&background_rgb(page.background)),
    });
    ops.push(Op::DrawPolygon {
        polygon: rect_polygon(0.0, 0.0, page.size_pt.0, page_h, PaintMode::Fill),
    });

    let bright = text_rgb(page.background);
    let number = number_rgb(page.background);
    let dim = dim_rgb(page.background);
    let placeholder = placeholder_rgb(page.background);

    // Decode every frame's thumbnail in parallel (the slow part), then register
    // and place them serially (printpdf's document is not shared across threads).
    let decoded: Vec<Option<(RawImage, u32, u32)>> = page
        .cells
        .par_iter()
        .map(|cell| thumbs.get(&cell.id).and_then(decode_frame))
        .collect();

    for (cell, frame) in page.cells.iter().zip(decoded) {
        match frame {
            Some((raw, w_px, h_px)) => {
                place_image(doc, &mut ops, raw, w_px, h_px, cell.image_rect, page_h);
            }
            None => draw_placeholder(&mut ops, cell.image_rect, page_h, &placeholder),
        }
        draw_caption(&mut ops, cell, page_h, &number, &dim);
    }

    draw_header(&mut ops, page, page_h, &bright);
    // Footer, left-aligned along the bottom band.
    let footer_baseline = page.footer_rect.y + FOOTER_FONT_PT;
    emit_text(
        &mut ops,
        &page.footer,
        page.footer_rect.x + TEXT_INSET_PT,
        page_h - footer_baseline,
        FOOTER_FONT_PT,
        BuiltinFont::Courier,
        &dim,
    );

    ops
}

/// Draw the top band: the optional user title on the left, the app mark on the
/// right (right-aligned via an estimated string width).
fn draw_header(ops: &mut Vec<Op>, page: &PagePlan, page_h: f32, rgb: &Rgb) {
    let rect = page.header_rect;
    let baseline = rect.y + HEADER_FONT_PT;
    if let Some(title) = page.title.as_ref().filter(|t| !t.is_empty()) {
        emit_text(
            ops,
            title,
            rect.x + TEXT_INSET_PT,
            page_h - baseline,
            HEADER_FONT_PT,
            BuiltinFont::CourierBold,
            rgb,
        );
    }
    let mark = app_mark();
    let mark_w = text_width(&mark, HEADER_FONT_PT);
    emit_text(
        ops,
        &mark,
        rect.x + rect.w - mark_w - TEXT_INSET_PT,
        page_h - baseline,
        HEADER_FONT_PT,
        BuiltinFont::Courier,
        rgb,
    );
}

/// Decode one frame's thumbnail into an embeddable `RawImage` plus its pixel
/// dimensions. Builds the image from raw RGB8 samples directly — no JPEG
/// re-encode/re-decode round-trip and no image-codec dependency — so it's fast
/// and needs none of printpdf's optional codec features. `None` when the file
/// can't be decoded (drawn as a placeholder).
fn decode_frame(src: &ThumbSrc) -> Option<(RawImage, u32, u32)> {
    let thumb = imaging::decode_thumbnail(&src.path, src.orientation, THUMB_EDGE, None)?;
    // A zero-dimension image would make the fit aspect NaN/inf downstream.
    if thumb.width == 0 || thumb.height == 0 {
        return None;
    }
    // Drop the (opaque) alpha channel: thumbnails contain no transparency, and a
    // DeviceRGB image is the smallest, simplest thing to embed.
    let mut rgb = Vec::with_capacity(thumb.rgba.len() / 4 * 3);
    for px in thumb.rgba.chunks_exact(4) {
        rgb.extend_from_slice(&px[..3]);
    }
    let raw = RawImage {
        pixels: RawImageData::U8(rgb),
        width: thumb.width as usize,
        height: thumb.height as usize,
        data_format: RawImageFormat::RGB8,
        tag: Vec::new(),
    };
    Some((raw, thumb.width, thumb.height))
}

/// Register `raw` and emit a `UseXobject` op placing it contain-fit inside
/// `image_rect` (top-left coordinate space), converted to PDF's bottom-left space.
fn place_image(
    doc: &mut PdfDocument,
    ops: &mut Vec<Op>,
    raw: RawImage,
    w_px: u32,
    h_px: u32,
    image_rect: RectPt,
    page_h: f32,
) {
    let fit = contain_fit(image_rect, w_px as f32 / h_px as f32);
    // Natural on-page size printpdf gives the image before our scale, at IMAGE_DPI.
    let natural_w = w_px as f32 * 72.0 / IMAGE_DPI;
    let natural_h = h_px as f32 * 72.0 / IMAGE_DPI;
    let id = doc.add_image(&raw);
    ops.push(Op::UseXobject {
        id,
        transform: XObjectTransform {
            translate_x: Some(Pt(fit.x)),
            translate_y: Some(Pt(page_h - (fit.y + fit.h))),
            scale_x: Some(fit.w / natural_w),
            scale_y: Some(fit.h / natural_h),
            rotate: None,
            dpi: Some(IMAGE_DPI),
        },
    });
}

/// A thin outlined box for a frame with no decodable thumbnail (RAW-only,
/// missing). Drawn in the placeholder colour so it reads as an empty rebate.
fn draw_placeholder(ops: &mut Vec<Op>, image_rect: RectPt, page_h: f32, rgb: &Rgb) {
    ops.push(Op::SetOutlineColor { col: solid(rgb) });
    ops.push(Op::SetOutlineThickness { pt: Pt(0.75) });
    ops.push(Op::DrawPolygon {
        polygon: rect_polygon(
            image_rect.x,
            page_h - (image_rect.y + image_rect.h),
            image_rect.w,
            image_rect.h,
            PaintMode::Stroke,
        ),
    });
}

/// A filled/stroked rectangle as a polygon. printpdf's `Op::DrawRectangle`
/// ignores its paint mode (it only builds a path), so anything that must show
/// ink goes through a `Polygon`, which honours `PaintMode`.
fn rect_polygon(x: f32, y: f32, w: f32, h: f32, mode: PaintMode) -> Polygon {
    let corners = [(x, y), (x + w, y), (x + w, y + h), (x, y + h)];
    Polygon {
        rings: vec![PolygonRing {
            points: corners
                .iter()
                .map(|&(px, py)| LinePoint {
                    p: Point {
                        x: Pt(px),
                        y: Pt(py),
                    },
                    bezier: false,
                })
                .collect(),
        }],
        mode,
        winding_order: WindingOrder::NonZero,
    }
}

/// Draw the caption beneath a frame in the analog style: line 1 is the bold
/// mono frame number followed by the dimmed `(filename)`; line 2 is the dimmed
/// exposure. Everything is Courier and clipped/truncated to the cell width.
fn draw_caption(ops: &mut Vec<Op>, cell: &CellPlacement, page_h: f32, bright: &Rgb, dim: &Rgb) {
    let rect = cell.caption_rect;
    if rect.h <= 0.0 {
        return;
    }
    let inset_x = rect.x + TEXT_INSET_PT;
    let max_w = rect.w - 2.0 * TEXT_INSET_PT;
    let mut top = rect.y;

    if !cell.number_text.is_empty() || cell.filename.is_some() {
        let baseline = page_h - (top + NUMBER_FONT_PT);
        let mut x = inset_x;
        if !cell.number_text.is_empty() {
            emit_text(
                ops,
                &cell.number_text,
                x,
                baseline,
                NUMBER_FONT_PT,
                BuiltinFont::CourierBold,
                bright,
            );
            x += text_width(&cell.number_text, NUMBER_FONT_PT);
        }
        if let Some(name) = &cell.filename {
            let prefix = if cell.number_text.is_empty() {
                name.clone()
            } else {
                format!(" ({name})")
            };
            let avail = (inset_x + max_w - x).max(0.0);
            let text = truncate_to_width(&prefix, avail, NUMBER_FONT_PT);
            emit_text(
                ops,
                &text,
                x,
                baseline,
                NUMBER_FONT_PT,
                BuiltinFont::Courier,
                dim,
            );
        }
        top += NUMBER_FONT_PT + 0.5;
    }

    if let Some(exp) = &cell.exposure {
        let text = truncate_to_width(exp, max_w, DETAIL_FONT_PT);
        emit_text(
            ops,
            &text,
            inset_x,
            page_h - (top + DETAIL_FONT_PT),
            DETAIL_FONT_PT,
            BuiltinFont::Courier,
            dim,
        );
    }
}

/// Emit the operator sequence for one run of text at a PDF (bottom-left) point.
/// Text is transliterated to the WinAnsi-safe subset the built-in fonts can
/// render (shared with the preview via `dcs_domain`).
fn emit_text(
    ops: &mut Vec<Op>,
    text: &str,
    x_pt: f32,
    y_pt: f32,
    size: f32,
    font: BuiltinFont,
    rgb: &Rgb,
) {
    let text = ascii_caption(text);
    if text.is_empty() {
        return;
    }
    ops.push(Op::SetFillColor { col: solid(rgb) });
    ops.push(Op::StartTextSection);
    ops.push(Op::SetFont {
        font: PdfFontHandle::Builtin(font),
        size: Pt(size),
    });
    ops.push(Op::SetTextCursor {
        pos: Point {
            x: Pt(x_pt),
            y: Pt(y_pt),
        },
    });
    ops.push(Op::ShowText {
        items: vec![TextItem::Text(text)],
    });
    ops.push(Op::EndTextSection);
}

/// Estimated rendered width of `text` in points at `size`, using Courier's
/// exact monospace advance (no font metrics needed for these short labels).
fn text_width(text: &str, size: f32) -> f32 {
    text.chars().count() as f32 * size * AVG_GLYPH_FRAC
}

/// Truncate `text` to fit `max_w` points at `size`, appending `…` when cut.
fn truncate_to_width(text: &str, max_w: f32, size: f32) -> String {
    if text_width(text, size) <= max_w {
        return text.to_string();
    }
    let glyph = size * AVG_GLYPH_FRAC;
    let budget = (max_w / glyph).floor() as usize;
    if budget <= 1 {
        return "…".to_string();
    }
    let keep = budget - 1;
    let mut out: String = text.chars().take(keep).collect();
    out.push('…');
    out
}

/// Contain-fit an image of the given aspect inside `outer` (top-left space),
/// centred, never distorting.
fn contain_fit(outer: RectPt, aspect: f32) -> RectPt {
    let outer_aspect = outer.w / outer.h;
    let (w, h) = if aspect > outer_aspect {
        (outer.w, outer.w / aspect)
    } else {
        (outer.h * aspect, outer.h)
    };
    RectPt {
        x: outer.x + (outer.w - w) / 2.0,
        y: outer.y + (outer.h - h) / 2.0,
        w,
        h,
    }
}

fn background_rgb(bg: SheetBackground) -> Rgb {
    match bg {
        SheetBackground::Black => rgb(0.0, 0.0, 0.0),
        SheetBackground::White => rgb(1.0, 1.0, 1.0),
    }
}

/// Bright caption/header colour: high-contrast against the background.
fn text_rgb(bg: SheetBackground) -> Rgb {
    match bg {
        SheetBackground::Black => rgb(0.92, 0.92, 0.92),
        SheetBackground::White => rgb(0.15, 0.15, 0.15),
    }
}

/// The frame-number colour: pure ink — black on white paper, white on a black
/// rebate — so the stamped number reads like a real contact sheet.
fn number_rgb(bg: SheetBackground) -> Rgb {
    match bg {
        SheetBackground::Black => rgb(1.0, 1.0, 1.0),
        SheetBackground::White => rgb(0.0, 0.0, 0.0),
    }
}

/// Dimmed colour for secondary caption text (filename, exposure, footer).
fn dim_rgb(bg: SheetBackground) -> Rgb {
    match bg {
        SheetBackground::Black => rgb(0.58, 0.58, 0.58),
        SheetBackground::White => rgb(0.5, 0.5, 0.5),
    }
}

/// Placeholder outline colour: a mid grey that reads on either background.
fn placeholder_rgb(bg: SheetBackground) -> Rgb {
    match bg {
        SheetBackground::Black => rgb(0.4, 0.4, 0.4),
        SheetBackground::White => rgb(0.6, 0.6, 0.6),
    }
}

fn rgb(r: f32, g: f32, b: f32) -> Rgb {
    Rgb {
        r,
        g,
        b,
        icc_profile: None,
    }
}

fn solid(c: &Rgb) -> Color {
    Color::Rgb(c.clone())
}

/// Write `bytes` to `dest` atomically: `.part` → fsync → rename. Unlike the
/// export executor this *does* replace an existing file — the destination is a
/// path the user explicitly chose in the save dialog — but the rename is atomic,
/// so a crash leaves either the old file or the new one, never a torn PDF.
fn atomic_write(dest: &Path, bytes: &[u8]) -> Result<(), String> {
    if let Some(parent) = dest.parent()
        && let Err(e) = fs::create_dir_all(parent)
    {
        return Err(format!("create {}: {e}", parent.display()));
    }
    let tmp = part_path(dest);
    let write = File::create(&tmp).and_then(|mut f| {
        f.write_all(bytes)?;
        f.sync_all()
    });
    if let Err(e) = write {
        let _ = fs::remove_file(&tmp);
        return Err(format!("write {}: {e}", tmp.display()));
    }
    if let Err(e) = fs::rename(&tmp, dest) {
        let _ = fs::remove_file(&tmp);
        return Err(format!("rename into {}: {e}", dest.display()));
    }
    if let Some(parent) = dest.parent()
        && let Ok(handle) = File::open(parent)
    {
        let _ = handle.sync_all();
    }
    Ok(())
}

fn part_path(dest: &Path) -> PathBuf {
    let mut name = dest.as_os_str().to_os_string();
    name.push(".part");
    name.into()
}

fn pt_to_mm(pt: f32) -> f32 {
    pt * 25.4 / 72.0
}
