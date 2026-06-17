//! Gallery view mode: one photo big, judged at quality. The focused
//! photo fills the frame (contain-fit, else 1:1 under `Z`); a docked filmstrip
//! shows the visible order with the current frame centered. Keyboard lives in
//! `app.rs` (full parity with the grid); this module only paints and reports the
//! pointer events back up.
//!
//! Two texture caches feed it: `frame_textures` holds the large gallery decode
//! (`Session::gallery_image`), `strip_textures` holds the cheap base thumbs the
//! filmstrip reuses from the grid — keeping them apart so the focused photo's
//! big frame and its tiny strip thumb never collide on one `PhotoId` key.

use dcs_app::Session;
use dcs_domain::cull::AcceptState;
use egui::{Align2, Color32, FontId, Pos2, Rect, Sense, Stroke, StrokeKind, Ui, Vec2};

use crate::grid::{TextureCache, contain_fit, full_uv};
use crate::theme;

/// Docked info-bar height in points (two compact rows + padding).
const INFO_BAR_H: f32 = 48.0;
/// Filmstrip dock height in points (thumb + padding).
const STRIP_H: f32 = 84.0;
/// Filmstrip thumb edge in points.
const STRIP_THUMB: f32 = 64.0;
/// Gap between filmstrip thumbs.
const STRIP_GAP: f32 = 6.0;

/// The ephemeral gallery view state the caller drives each frame.
pub struct GalleryState {
    /// Display index of the photo being judged.
    pub focus: usize,
    /// `false` = contain-fit, `true` = 1:1 (`Z`).
    pub full_zoom: bool,
    /// Filmstrip dock hidden.
    pub strip_collapsed: bool,
    /// The focus just moved — recenter the filmstrip this frame.
    pub center_focus: bool,
}

/// What the gallery reports back after a frame.
pub struct GalleryResponse {
    /// A filmstrip thumb was clicked — jump the focus to this display index.
    pub clicked: Option<usize>,
}

/// Paint the gallery for the given [`GalleryState`].
pub fn show(
    ui: &mut Ui,
    session: &mut Session,
    frame_textures: &mut TextureCache,
    strip_textures: &mut TextureCache,
    state: &GalleryState,
) -> GalleryResponse {
    let GalleryState {
        focus,
        full_zoom,
        strip_collapsed,
        center_focus,
    } = *state;
    frame_textures.begin_frame();
    strip_textures.begin_frame();

    let area = ui.available_rect_before_wrap();
    ui.painter().rect_filled(area, 0.0, theme::SHEET_BG);

    let Some(id) = session.photo_at(focus).map(|p| p.id) else {
        return GalleryResponse { clicked: None };
    };

    // Carve the area top→bottom: image, then the docked info bar, then the
    // filmstrip. The bar never overlaps the photo (it sits in its own band), so
    // the frame is always clean.
    let strip_h = if strip_collapsed { 0.0 } else { STRIP_H };
    let image_bottom = (area.max.y - strip_h - INFO_BAR_H).max(area.min.y);
    let image_rect = Rect::from_min_max(area.min, Pos2::new(area.max.x, image_bottom));
    let bar_rect = Rect::from_min_max(
        Pos2::new(area.min.x, image_bottom),
        Pos2::new(area.max.x, image_bottom + INFO_BAR_H),
    );

    // Quality: ask for a decode sized to the frame (device pixels), and the full
    // 1:1 decode when zoomed. Neighbours preload inside `request_gallery`.
    let ppp = ui.ctx().pixels_per_point();
    let fit_edge = (image_rect.size().max_elem() * ppp).ceil() as u32;
    session.request_gallery(focus, fit_edge.max(1));
    if full_zoom {
        session.request_gallery_full(focus);
    }

    paint_frame(ui, session, frame_textures, id, image_rect, full_zoom, ppp);
    paint_info_bar(ui, &frame_info(session, focus), bar_rect);

    let clicked = if strip_collapsed {
        None
    } else {
        let strip_rect = Rect::from_min_max(Pos2::new(area.min.x, bar_rect.max.y), area.max);
        paint_filmstrip(ui, session, strip_textures, focus, center_focus, strip_rect)
    };

    GalleryResponse { clicked }
}

/// The big frame: the resident gallery decode if ready, else the base thumb
/// upscaled as an instant stand-in that sharpens when the full decode lands.
/// Contain-fit, or 1:1 in a scroll area under `Z`.
fn paint_frame(
    ui: &mut Ui,
    session: &mut Session,
    textures: &mut TextureCache,
    id: dcs_domain::photo::PhotoId,
    rect: Rect,
    full_zoom: bool,
    ppp: f32,
) {
    // Prefer the sharp gallery frame; fall back to the base thumb so something
    // shows immediately. Each is fetched then handed straight to the texture
    // cache, so the session borrow ends before the next call.
    let tex = if session.gallery_image(id).is_some() {
        let view = session.gallery_image(id);
        textures.view_texture(ui, id, view)
    } else {
        let view = session.thumb(id);
        textures.view_texture(ui, id, view)
    };
    let Some(tex) = tex else {
        return;
    };

    if full_zoom {
        // 1:1 — one image pixel per device pixel, scrollable when larger than
        // the frame. Texture size is in pixels; divide by the scale factor.
        let native = tex.size / ppp;
        egui::ScrollArea::both()
            .id_salt("gallery_zoom")
            .show_viewport(ui, |ui, _vp| {
                let (r, _) = ui.allocate_exact_size(native.max(rect.size()), Sense::hover());
                let placed = Rect::from_center_size(r.center(), native);
                ui.painter()
                    .image(tex.id, placed, full_uv(), Color32::WHITE);
            });
    } else {
        let fit = contain_fit(rect, tex.size);
        ui.painter().image(tex.id, fit, full_uv(), Color32::WHITE);
    }
}

/// The facts shown in the info bar for one photo, assembled from the session so
/// the renderer stays pure painting. Empty strings mean "absent" and are not
/// drawn.
struct FrameInfo {
    /// The displayed file's name (the JPEG when present); the type chip conveys
    /// whether a RAW also exists.
    filename: String,
    /// `RAW+JPEG` · `JPEG` · `RAW`.
    kind: &'static str,
    /// Whether a RAW is present — drives the kind chip's emphasis.
    has_raw: bool,
    /// `camera · lens · 35mm · f/2.8 · 1/250 · ISO 400`, present parts only.
    detail: String,
    /// Derived group title, or empty for the headerless stream.
    group: String,
    /// Adjusted capture time, or empty when undated.
    time: String,
}

/// Gather the info-bar facts for display index `focus`.
fn frame_info(session: &Session, focus: usize) -> FrameInfo {
    let mut info = FrameInfo {
        filename: String::new(),
        kind: "JPEG",
        has_raw: false,
        detail: String::new(),
        group: session
            .group_title_at(focus)
            .unwrap_or_default()
            .to_string(),
        time: session.caption_time(focus).unwrap_or_default(),
    };
    let Some(photo) = session.photo_at(focus) else {
        return info;
    };

    let has_jpeg = photo.files.jpeg.is_some();
    info.has_raw = photo.files.raw.is_some();
    info.kind = match (has_jpeg, info.has_raw) {
        (true, true) => "RAW+JPEG",
        (false, true) => "RAW",
        _ => "JPEG",
    };
    // Just the file being shown (the JPEG when present) — the RAW's presence is
    // already told by the type chip, so a second filename only adds noise.
    info.filename = photo.file_name();

    let detail: Vec<String> = [photo.meta.camera.clone(), photo.meta.lens.clone()]
        .into_iter()
        .flatten()
        .chain(photo.meta.exposure_line())
        .collect();
    info.detail = detail.join("  ·  ");
    info
}

/// The docked info bar under the photo: a charcoal band with the filename + a
/// type chip and the time on the bright top row, the camera/lens/exposure and
/// group dim below. Right-aligned fields hug the far edge so the row reads at a
/// glance.
fn paint_info_bar(ui: &Ui, info: &FrameInfo, rect: Rect) {
    let p = ui.painter();
    p.rect_filled(rect, 0.0, theme::CHROME_BG);
    p.hline(
        rect.x_range(),
        rect.top() + 0.5,
        Stroke::new(1.0, theme::HAIRLINE),
    );

    let pad = 12.0;
    let left = rect.left() + pad;
    let right = rect.right() - pad;
    let row1 = rect.top() + rect.height() * 0.34;
    let row2 = rect.top() + rect.height() * 0.70;

    // Top row: filename (bright) → type chip → time (right).
    let name = p.text(
        Pos2::new(left, row1),
        Align2::LEFT_CENTER,
        &info.filename,
        FontId::monospace(13.0),
        theme::SELECT_OUTLINE,
    );
    paint_kind_chip(p, info, Pos2::new(name.right() + 10.0, row1));
    if !info.time.is_empty() {
        p.text(
            Pos2::new(right, row1),
            Align2::RIGHT_CENTER,
            &info.time,
            FontId::monospace(12.0),
            theme::TEXT_DIM,
        );
    }

    // Bottom row: gear + exposure (left), group (right) — both dim.
    p.text(
        Pos2::new(left, row2),
        Align2::LEFT_CENTER,
        &info.detail,
        FontId::monospace(12.0),
        theme::TEXT_DIM,
    );
    if !info.group.is_empty() {
        p.text(
            Pos2::new(right, row2),
            Align2::RIGHT_CENTER,
            &info.group,
            FontId::monospace(12.0),
            theme::TEXT_DIM,
        );
    }
}

/// The file-type chip (`RAW+JPEG` / `JPEG` / `RAW`), centered on its left edge at
/// `anchor`. A RAW-bearing photo gets a brighter outline so the (load-bearing)
/// "has a RAW" fact reads instantly; grayscale only.
fn paint_kind_chip(p: &egui::Painter, info: &FrameInfo, anchor: Pos2) {
    let fg = if info.has_raw {
        theme::FOCUS_OUTLINE
    } else {
        theme::TEXT_DIM
    };
    let galley = p.layout_no_wrap(info.kind.to_string(), FontId::monospace(11.0), fg);
    let (px, py) = (6.0, 3.0);
    let size = Vec2::new(galley.size().x + px * 2.0, galley.size().y + py * 2.0);
    let chip = Rect::from_min_size(Pos2::new(anchor.x, anchor.y - size.y / 2.0), size);
    p.rect_stroke(chip, 0.0, Stroke::new(1.0, fg), StrokeKind::Inside);
    p.galley(Pos2::new(chip.left() + px, chip.top() + py), galley, fg);
}

/// The filmstrip dock: a centered band of the visible order, base thumbs reused
/// from the grid, current frame outlined, verdict glyphs visible. Click-to-jump
/// returns the display index hit.
fn paint_filmstrip(
    ui: &mut Ui,
    session: &mut Session,
    textures: &mut TextureCache,
    focus: usize,
    center_focus: bool,
    rect: Rect,
) -> Option<usize> {
    // The band background spans the full width (painter, absolute rect) so it
    // reads as one surface even past the ends of the scroll content.
    let painter = ui.painter();
    painter.rect_filled(rect, 0.0, theme::CHROME_BG);
    painter.hline(
        rect.x_range(),
        rect.top() + 0.5,
        Stroke::new(1.0, theme::HAIRLINE),
    );

    let count = session.photo_count();
    if count == 0 {
        return None;
    }
    let pitch = STRIP_THUMB + STRIP_GAP;
    // Leading/trailing pad so the first and last thumbs can still scroll to the
    // centre, like a real filmstrip.
    let pad = (rect.width() / 2.0 - STRIP_THUMB / 2.0).max(0.0);
    let content_w = pad * 2.0 + count as f32 * pitch - STRIP_GAP;
    let mut hit = None;

    // A horizontal scroll area carved into the band: virtualized to the thumbs
    // intersecting the viewport, click-to-jump like the grid, and centred on the
    // current frame only when the focus changed (so manual scrolling sticks).
    ui.scope_builder(egui::UiBuilder::new().max_rect(rect), |ui| {
        egui::ScrollArea::horizontal()
            .id_salt("dcs_filmstrip")
            .show_viewport(ui, |ui, viewport| {
                let (alloc, resp) =
                    ui.allocate_exact_size(Vec2::new(content_w, STRIP_THUMB), Sense::click());
                let origin = alloc.min;
                let cy = alloc.center().y;
                let slot_x = |idx: usize| origin.x + pad + idx as f32 * pitch;

                // Only the thumbs whose column meets the viewport are painted.
                let first = (((viewport.min.x - pad) / pitch).floor() as isize).max(0) as usize;
                let last = (((viewport.max.x - pad) / pitch).ceil() as usize + 1).min(count);
                for idx in first..last {
                    let slot = Rect::from_min_size(
                        Pos2::new(slot_x(idx), cy - STRIP_THUMB / 2.0),
                        Vec2::splat(STRIP_THUMB),
                    );
                    let Some(info) = session.cell_info(idx) else {
                        continue;
                    };
                    ui.painter().rect_filled(slot, 0.0, theme::CELL_EMPTY);
                    session.request_base(idx);
                    let view = session.thumb(info.id);
                    if let Some(tex) = textures.view_texture(ui, info.id, view) {
                        let fit = contain_fit(slot, tex.size);
                        ui.painter().image(tex.id, fit, full_uv(), Color32::WHITE);
                    }
                    if info.state == AcceptState::Rejected {
                        ui.painter().rect_filled(slot, 0.0, theme::REJECT_DIM);
                    }
                    paint_strip_glyph(ui, slot, info.state);
                    if idx == focus {
                        ui.painter().rect_stroke(
                            slot,
                            0.0,
                            Stroke::new(2.0, theme::FOCUS_OUTLINE),
                            StrokeKind::Inside,
                        );
                    }
                }

                // Map a click to the thumb under it (ignoring the inter-thumb gap).
                if let Some(pos) = resp.interact_pointer_pos().filter(|_| resp.clicked()) {
                    let rel = pos.x - origin.x - pad;
                    if rel >= 0.0 {
                        let idx = (rel / pitch) as usize;
                        if idx < count && rel - idx as f32 * pitch <= STRIP_THUMB {
                            hit = Some(idx);
                        }
                    }
                }

                // Recentre on the current frame when the focus just moved.
                if center_focus {
                    let frame = Rect::from_min_size(
                        Pos2::new(slot_x(focus), cy - STRIP_THUMB / 2.0),
                        Vec2::splat(STRIP_THUMB),
                    );
                    // Jump, don't glide — the fast auto-scroll across the strip
                    // reads as jank.
                    ui.scroll_to_rect_animation(
                        frame,
                        Some(egui::Align::Center),
                        egui::style::ScrollAnimation::none(),
                    );
                }
            });
    });
    textures.evict_over_budget();
    hit
}

/// A small verdict tick in the filmstrip thumb's corner — green accept, red
/// reject, nothing for unreviewed.
fn paint_strip_glyph(ui: &Ui, slot: Rect, state: AcceptState) {
    let color = match state {
        AcceptState::Accepted => theme::VERDICT_ACCEPT,
        AcceptState::Rejected => theme::VERDICT_REJECT,
        AcceptState::Unreviewed => return,
    };
    let s = 12.0;
    let box_rect = Rect::from_min_max(Pos2::new(slot.right() - s, slot.bottom() - s), slot.max);
    ui.painter().rect_filled(box_rect, 0.0, theme::BADGE_BG);
    let c = box_rect.center();
    let r = s * 0.28;
    let stroke = Stroke::new(1.6, color);
    match state {
        AcceptState::Accepted => {
            ui.painter().add(egui::Shape::line(
                vec![
                    Pos2::new(c.x - r, c.y),
                    Pos2::new(c.x - r * 0.25, c.y + r * 0.7),
                    Pos2::new(c.x + r, c.y - r * 0.6),
                ],
                stroke,
            ));
        }
        AcceptState::Rejected => {
            ui.painter().line_segment(
                [Pos2::new(c.x - r, c.y - r), Pos2::new(c.x + r, c.y + r)],
                stroke,
            );
            ui.painter().line_segment(
                [Pos2::new(c.x + r, c.y - r), Pos2::new(c.x - r, c.y + r)],
                stroke,
            );
        }
        AcceptState::Unreviewed => {}
    }
}
