//! Virtualized contact-sheet grid. Uniform square cells, contain-fit
//! thumbnails, EXIF orientation already baked by the decoder (§2.6). Only the
//! rows in view are laid out (egui `show_rows`); thumbnails for the visible
//! band plus a prefetch margin are requested from the session.
//!
//! GPU textures live in a bounded LRU (`TextureCache`) rather than being
//! dropped the moment a cell leaves the viewport: scrolling back stays smooth
//! because recently-seen textures are still resident, and the ones that did
//! age out re-upload from the session's RAM pixel cache (a memcpy, not a
//! decode). The per-cell hot path allocates nothing.

use std::collections::HashMap;

use dcs_app::Session;
use dcs_domain::photo::PhotoId;
use egui::{Color32, FontId, Pos2, Rect, Sense, TextureHandle, TextureId, TextureOptions, Ui, Vec2};

use crate::theme;

/// Rows above and below the viewport to decode ahead of the scroll.
const PREFETCH_ROWS: usize = 5;
/// Below this cell size the RAW badge is hidden (§2.1, zoom-gated).
const BADGE_MIN_CELL: f32 = 96.0;
/// At or above this cell size (logical points) the grid is "zoomed in" and
/// requests sharp hi-res decodes for visible cells. Below it everything uses
/// the cheap base thumbnail, so default browsing never pays for a full decode.
const HIRES_ZOOM_CELL: f32 = 224.0;
/// Hi-res decode tiers in pixels. A zoomed cell requests the smallest tier that
/// covers its on-screen pixel size.
const TIERS: [u32; 3] = [256, 512, 1024];
/// VRAM budget for resident thumbnail textures. Larger (zoomed) textures cost
/// more, so the cache is bounded by bytes, not count.
const TEXTURE_CACHE_BYTES: u64 = 768_000_000;

/// Bounded LRU of uploaded thumbnail textures, keyed by photo, aged by frame
/// and bounded by a VRAM byte budget.
pub struct TextureCache {
    map: HashMap<PhotoId, Entry>,
    used: u64,
    frame: u64,
}

impl TextureCache {
    pub fn new() -> Self {
        TextureCache {
            map: HashMap::new(),
            used: 0,
            frame: 0,
        }
    }

    pub fn len(&self) -> usize {
        self.map.len()
    }

    pub fn clear(&mut self) {
        self.map.clear();
        self.used = 0;
    }

    fn begin_frame(&mut self) {
        self.frame += 1;
    }

    /// Texture for a photo, uploading from cached pixels on first need and
    /// re-uploading when the session reports a newer version (a higher tier on
    /// zoom, or back to base on zoom-out). Touches the entry so it survives
    /// eviction. `None` until pixels exist.
    fn texture(&mut self, ui: &Ui, session: &mut Session, id: PhotoId) -> Option<TexRef> {
        let frame = self.frame;
        // Resident at the current version → just touch it (hot path).
        if let Some(entry) = self.map.get(&id)
            && session
                .thumb(id)
                .is_none_or(|view| view.version == entry.version)
        {
            let entry = self.map.get_mut(&id).expect("just checked it is present");
            entry.last_used = frame;
            return Some(TexRef::of(&entry.handle));
        }

        let view = session.thumb(id)?;
        let color = egui::ColorImage::from_rgba_unmultiplied(
            [view.image.width as usize, view.image.height as usize],
            &view.image.rgba,
        );
        let handle = ui
            .ctx()
            .load_texture(format!("thumb-{}", id.0), color, TextureOptions::LINEAR);
        let weight = view.image.width as u64 * view.image.height as u64 * 4;
        let tref = TexRef::of(&handle);
        let replaced = self.map.insert(
            id,
            Entry {
                last_used: frame,
                version: view.version,
                weight,
                handle,
            },
        );
        if let Some(old) = replaced {
            self.used -= old.weight;
        }
        self.used += weight;
        self.evict_over_budget();
        Some(tref)
    }

    fn evict_over_budget(&mut self) {
        while self.used > TEXTURE_CACHE_BYTES && self.map.len() > 1 {
            let Some(victim) = self
                .map
                .iter()
                .min_by_key(|(_, e)| e.last_used)
                .map(|(id, _)| *id)
            else {
                break;
            };
            if let Some(removed) = self.map.remove(&victim) {
                self.used -= removed.weight;
            }
        }
    }
}

impl Default for TextureCache {
    fn default() -> Self {
        Self::new()
    }
}

struct Entry {
    last_used: u64,
    version: u64,
    weight: u64,
    handle: TextureHandle,
}

#[derive(Clone, Copy)]
struct TexRef {
    id: TextureId,
    size: Vec2,
}

impl TexRef {
    fn of(handle: &TextureHandle) -> Self {
        TexRef {
            id: handle.id(),
            size: handle.size_vec2(),
        }
    }
}

/// Paint the grid and return how many cells were drawn this frame (for the
/// diagnostics overlay). `cell` is the square cell edge in points; `view_width`
/// is the width available for column math (measured before the scroll area so
/// the count stays stable).
pub fn show(
    ui: &mut Ui,
    session: &mut Session,
    textures: &mut TextureCache,
    cell: f32,
    view_width: f32,
) -> usize {
    let count = session.photo_count();
    if count == 0 {
        return 0;
    }
    textures.begin_frame();

    let gap = (cell * 0.1).max(4.0);
    let stride = cell + gap;
    let cols = (((view_width + gap) / stride).floor() as usize).max(1);
    let rows = count.div_ceil(cols);

    // Default browsing uses only the cheap base thumbnail. Once zoomed in,
    // visible cells additionally request a sharp decode sized to device pixels.
    let zoomed = cell >= HIRES_ZOOM_CELL;
    let hires_edge = tier_for(cell * ui.ctx().pixels_per_point());
    if !zoomed {
        session.clear_hires();
    }
    let mut visible = 0usize;

    egui::ScrollArea::vertical()
        .auto_shrink([false, false])
        .show_rows(ui, stride, rows, |ui, row_range| {
            request_base_band(session, cols, count, &row_range, rows);

            for row in row_range {
                let (strip, _) =
                    ui.allocate_exact_size(Vec2::new(ui.available_width(), stride), Sense::hover());
                for col in 0..cols {
                    let idx = row * cols + col;
                    if idx >= count {
                        break;
                    }
                    let Some(info) = session.cell_info(idx) else {
                        break;
                    };
                    if zoomed {
                        session.request_hires(idx, hires_edge);
                    }
                    let origin = Pos2::new(strip.left() + col as f32 * stride, strip.top());
                    let cell_rect = Rect::from_min_size(origin, Vec2::splat(cell));
                    paint_cell(ui, session, textures, info.id, info.raw_only, cell_rect);
                    visible += 1;
                }
            }
        });

    // Keep filling base thumbnails for the rest of the folder in the
    // background — viewport requests above already took priority this frame.
    session.fill_base_background();

    textures.evict_over_budget();
    visible
}

/// Request base thumbnails for the visible rows first, then the prefetch
/// margin (below, then above), so on-screen cells win the decode pool over
/// rows that just scrolled out of view.
fn request_base_band(
    session: &mut Session,
    cols: usize,
    count: usize,
    row_range: &std::ops::Range<usize>,
    rows: usize,
) {
    let above = row_range.start.saturating_sub(PREFETCH_ROWS)..row_range.start;
    let below = row_range.end..(row_range.end + PREFETCH_ROWS).min(rows);
    let request_rows = |session: &mut Session, rows: std::ops::Range<usize>| {
        for row in rows {
            for col in 0..cols {
                let idx = row * cols + col;
                if idx >= count {
                    break;
                }
                session.request_base(idx);
            }
        }
    };
    request_rows(session, row_range.clone());
    request_rows(session, below);
    request_rows(session, above);
}

/// Smallest decode tier covering `px` on-screen pixels.
fn tier_for(px: f32) -> u32 {
    let px = px.ceil() as u32;
    *TIERS
        .iter()
        .find(|&&t| t >= px)
        .unwrap_or(TIERS.last().expect("TIERS is non-empty"))
}

fn paint_cell(
    ui: &mut Ui,
    session: &mut Session,
    textures: &mut TextureCache,
    id: PhotoId,
    raw_only: bool,
    cell_rect: Rect,
) {
    ui.painter().rect_filled(cell_rect, 0.0, theme::CELL_EMPTY);

    if let Some(tex) = textures.texture(ui, session, id) {
        let fit = contain_fit(cell_rect, tex.size);
        ui.painter().image(tex.id, fit, full_uv(), Color32::WHITE);
    }

    if raw_only && cell_rect.width() >= BADGE_MIN_CELL {
        paint_raw_badge(ui, cell_rect);
    }
}

fn paint_raw_badge(ui: &Ui, cell_rect: Rect) {
    let size = (cell_rect.width() * 0.16).clamp(14.0, 22.0);
    let badge = Rect::from_min_size(cell_rect.min, Vec2::splat(size));
    ui.painter().rect_filled(badge, 0.0, theme::BADGE_BG);
    ui.painter().text(
        badge.center(),
        egui::Align2::CENTER_CENTER,
        "R",
        FontId::monospace(size * 0.7),
        theme::TEXT_DIM,
    );
}

fn contain_fit(outer: Rect, size: Vec2) -> Rect {
    let scale = (outer.width() / size.x).min(outer.height() / size.y);
    let fitted = Vec2::new(size.x * scale, size.y * scale);
    Rect::from_center_size(outer.center(), fitted)
}

fn full_uv() -> Rect {
    Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0))
}
