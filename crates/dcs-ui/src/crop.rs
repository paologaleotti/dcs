//! Crop view mode: a non-destructive crop + straighten editor on one photo.
//! Mirrors the gallery's shape — big frame, a docked filmstrip — with a tool
//! bar (aspect presets + straighten) and a crop overlay (rule-of-thirds grid,
//! eight drag handles, dimmed surround) layered over the *uncropped* original.
//!
//! The straighten rotates the displayed image live via a rotated `Mesh` (GPU,
//! no CPU re-decode), so dragging the angle stays smooth. The crop rectangle is
//! axis-aligned on screen; the image turns underneath it, Lightroom-style. All
//! edits clamp to the largest rectangle that fits inside the rotated frame, so
//! the result never exposes empty corners.
//!
//! Editing is ephemeral: the working [`CropEditState`] is committed as one
//! `SetCrop` command when the user applies, and discarded on cancel.

use dcs_app::{CropEdit, NormRect, Session};
use dcs_domain::crops::{self, MAX_ANGLE_DEG};
use dcs_domain::photo::PhotoId;
use egui::{Align2, Color32, FontId, Mesh, Pos2, Rect, Sense, Shape, Stroke, StrokeKind, Ui, Vec2};

use crate::gallery;
use crate::grid::{TexRef, TextureCache, contain_fit};
use crate::theme;

/// Tool-bar height in points (aspect presets + straighten + actions).
const TOOLBAR_H: f32 = 44.0;
/// Half-size of a crop handle's hit/draw box, in points.
const HANDLE: f32 = 7.0;

/// Which part of the crop rectangle a drag is moving.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Handle {
    TopLeft,
    Top,
    TopRight,
    Right,
    BottomRight,
    Bottom,
    BottomLeft,
    Left,
    Move,
}

impl Handle {
    /// The eight resize handles, in the order [`handle_centers`] lays them out.
    const RESIZE: [Handle; 8] = [
        Handle::TopLeft,
        Handle::TopRight,
        Handle::BottomRight,
        Handle::BottomLeft,
        Handle::Top,
        Handle::Right,
        Handle::Bottom,
        Handle::Left,
    ];

    fn moves_left(self) -> bool {
        matches!(self, Handle::TopLeft | Handle::Left | Handle::BottomLeft)
    }
    fn moves_right(self) -> bool {
        matches!(self, Handle::TopRight | Handle::Right | Handle::BottomRight)
    }
    fn moves_top(self) -> bool {
        matches!(self, Handle::TopLeft | Handle::Top | Handle::TopRight)
    }
    fn moves_bottom(self) -> bool {
        matches!(
            self,
            Handle::BottomLeft | Handle::Bottom | Handle::BottomRight
        )
    }
}

/// Aspect-ratio constraint for the crop. `Free` is unconstrained; `Original`
/// locks to the source image's own ratio; the rest are fixed photo ratios. The
/// fixed non-square ratios respect the `portrait` flip.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AspectMode {
    Free,
    Original,
    Square,
    ThreeTwo,
    FourThree,
    SixteenNine,
}

/// The live crop-editor state for one photo. Seeded from the photo's committed
/// crop (or identity) on entry; mutated by the overlay each frame.
pub struct CropEditState {
    /// Display index being edited (tracks the filmstrip focus).
    pub focus: usize,
    /// The photo under edit.
    pub photo: PhotoId,
    /// Straighten angle in degrees, clamped to ±[`MAX_ANGLE_DEG`].
    pub angle_deg: f32,
    /// Crop window, normalized over the rotated bounding box.
    pub rect: NormRect,
    /// Active aspect constraint.
    pub aspect: AspectMode,
    /// Flip the fixed non-square ratios to portrait orientation.
    pub portrait: bool,
    /// Recenter the filmstrip on the focus this frame.
    pub center_focus: bool,
    /// In-progress handle drag, if any.
    drag: Option<Handle>,
    /// One-shot: snap the rect to the active aspect's max-fit on the next frame
    /// (set when the user picks a preset, or on a fresh crop's first layout).
    refit: bool,
}

/// What the editor reports back after a frame.
pub struct CropResponse {
    /// Commit the working edit and leave the editor.
    pub apply: bool,
    /// Discard the working edit and leave the editor.
    pub cancel: bool,
    /// A filmstrip thumb was clicked — switch the edited photo to this index
    /// (the caller commits any pending change first).
    pub jump: Option<usize>,
}

impl CropEditState {
    /// Seed the editor from a photo's committed crop, or the identity edit when
    /// uncropped.
    pub fn new(focus: usize, photo: PhotoId, committed: Option<CropEdit>) -> Self {
        let edit = committed.unwrap_or_else(CropEdit::identity);
        // A fresh crop defaults to the original aspect (and snaps the box to it);
        // an existing crop keeps its stored free-form rect so it isn't distorted.
        let fresh = committed.is_none();
        CropEditState {
            focus,
            photo,
            angle_deg: edit.angle_deg,
            rect: edit.rect,
            aspect: if fresh {
                AspectMode::Original
            } else {
                AspectMode::Free
            },
            portrait: false,
            center_focus: true,
            drag: None,
            refit: fresh,
        }
    }

    /// The working edit as a domain [`CropEdit`] (sanitized).
    pub fn to_edit(&self) -> CropEdit {
        CropEdit {
            angle_deg: self.angle_deg,
            rect: self.rect,
        }
        .sanitized()
    }

    /// Reset to the uncropped default (the same fresh state as opening the editor
    /// on an uncropped photo: full frame, no rotation, original aspect).
    pub fn reset(&mut self) {
        *self = CropEditState::new(self.focus, self.photo, None);
    }
}

impl AspectMode {
    /// The pixel ratio (width / height) this mode imposes given the source
    /// aspect `src_ratio`, or `None` for free-form. Honors the portrait flip for
    /// the fixed non-square ratios.
    fn ratio(self, src_ratio: f32, portrait: bool) -> Option<f32> {
        let base = match self {
            AspectMode::Free => return None,
            AspectMode::Original => return Some(src_ratio),
            AspectMode::Square => return Some(1.0),
            AspectMode::ThreeTwo => 3.0 / 2.0,
            AspectMode::FourThree => 4.0 / 3.0,
            AspectMode::SixteenNine => 16.0 / 9.0,
        };
        Some(if portrait { 1.0 / base } else { base })
    }

    fn label(self) -> &'static str {
        match self {
            AspectMode::Free => "Free",
            AspectMode::Original => "Original",
            AspectMode::Square => "1:1",
            AspectMode::ThreeTwo => "3:2",
            AspectMode::FourThree => "4:3",
            AspectMode::SixteenNine => "16:9",
        }
    }
}

/// Paint the crop editor for `state` and report the user's intent.
pub fn show(
    ui: &mut Ui,
    session: &mut Session,
    textures: &mut TextureCache,
    strip_textures: &mut TextureCache,
    state: &mut CropEditState,
) -> CropResponse {
    textures.begin_frame();
    strip_textures.begin_frame();

    let area = ui.available_rect_before_wrap();
    ui.painter().rect_filled(area, 0.0, theme::SHEET_BG);

    // Carve top→bottom: tool bar, image, filmstrip.
    let toolbar_rect = Rect::from_min_max(area.min, Pos2::new(area.max.x, area.min.y + TOOLBAR_H));
    let strip_h = gallery::STRIP_H;
    let image_bottom = (area.max.y - strip_h).max(toolbar_rect.max.y);
    let image_rect = Rect::from_min_max(
        Pos2::new(area.min.x, toolbar_rect.max.y),
        Pos2::new(area.max.x, image_bottom),
    );

    let mut resp = CropResponse {
        apply: false,
        cancel: false,
        jump: None,
    };

    // Ask for an uncropped frame sized to the image area; this is what the
    // overlay draws over. (The gallery cache was cleared on entry, so this never
    // collides with a cropped gallery frame for the same photo.)
    let ppp = ui.ctx().pixels_per_point();
    let fit_edge = (image_rect.size().max_elem() * ppp).ceil() as u32;
    session.request_crop_source(state.focus, fit_edge.max(1));

    paint_toolbar(ui, toolbar_rect, state, &mut resp);
    paint_editor(ui, session, textures, image_rect, state);

    let strip_rect = Rect::from_min_max(Pos2::new(area.min.x, image_rect.max.y), area.max);
    let strip = gallery::paint_filmstrip(
        ui,
        session,
        strip_textures,
        state.focus,
        std::mem::take(&mut state.center_focus),
        strip_rect,
    );
    if let Some(idx) = strip.clicked {
        resp.jump = Some(idx);
    }

    resp
}

/// The tool bar: aspect presets, an orientation flip, the straighten slider with
/// a 0.1°-precise readout, and Reset / Cancel / Apply.
fn paint_toolbar(ui: &mut Ui, rect: Rect, state: &mut CropEditState, resp: &mut CropResponse) {
    ui.painter().rect_filled(rect, 0.0, theme::CHROME_BG);
    ui.painter().hline(
        rect.x_range(),
        rect.bottom() - 0.5,
        Stroke::new(1.0, theme::HAIRLINE),
    );
    let mut child =
        ui.new_child(egui::UiBuilder::new().max_rect(rect.shrink2(Vec2::new(10.0, 6.0))));
    child.horizontal_centered(|ui| {
        for mode in [
            AspectMode::Free,
            AspectMode::Original,
            AspectMode::Square,
            AspectMode::ThreeTwo,
            AspectMode::FourThree,
            AspectMode::SixteenNine,
        ] {
            let selected = state.aspect == mode;
            if ui.selectable_label(selected, mode.label()).clicked() {
                state.aspect = mode;
                // Switching to a locked ratio snaps the box to it once; Free
                // leaves the current box as-is.
                state.refit = mode != AspectMode::Free;
            }
        }
        // Portrait flip only matters for the fixed non-square ratios.
        let flippable = matches!(
            state.aspect,
            AspectMode::ThreeTwo | AspectMode::FourThree | AspectMode::SixteenNine
        );
        if ui
            .add_enabled(flippable, egui::Button::new("⟲"))
            .on_hover_text("Flip portrait / landscape")
            .clicked()
        {
            state.portrait = !state.portrait;
            state.refit = true;
        }

        ui.separator();
        ui.label(
            egui::RichText::new("Straighten")
                .monospace()
                .color(theme::TEXT_DIM),
        );
        ui.add(
            egui::DragValue::new(&mut state.angle_deg)
                .speed(0.1)
                .range(-MAX_ANGLE_DEG..=MAX_ANGLE_DEG)
                .fixed_decimals(1)
                .suffix("°"),
        );
        ui.add(
            egui::Slider::new(&mut state.angle_deg, -MAX_ANGLE_DEG..=MAX_ANGLE_DEG)
                .show_value(false),
        );

        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if ui.button("Apply").clicked() {
                resp.apply = true;
            }
            if ui.button("Cancel").clicked() {
                resp.cancel = true;
            }
            if ui.button("Reset").clicked() {
                state.reset();
            }
        });
    });
}

/// Draw the rotated image, the dimmed surround, the crop rectangle with its
/// thirds grid and handles, and process handle drags.
fn paint_editor(
    ui: &mut Ui,
    session: &mut Session,
    textures: &mut TextureCache,
    image_rect: Rect,
    state: &mut CropEditState,
) {
    let id = state.photo;
    let view = session.gallery_image(id);
    let Some(tex) = textures.view_texture(ui, id, view) else {
        // Nothing decoded yet — show a hint so the area isn't blank.
        ui.painter().text(
            image_rect.center(),
            Align2::CENTER_CENTER,
            "loading…",
            FontId::monospace(13.0),
            theme::TEXT_DIM,
        );
        return;
    };

    let (sw, sh) = (tex.size.x.max(1.0), tex.size.y.max(1.0));

    // One-shot snap to the active aspect when a preset was just picked / on a
    // fresh crop; otherwise just keep the rect valid (clamped to the inset).
    if std::mem::take(&mut state.refit) {
        refit_to_aspect(state, sw, sh);
    } else {
        clamp_to_inset(state, sw, sh);
    }

    // The rotated bounding box, contain-fit into the image area.
    let (bw, bh) = crops::bounding_box(sw as u32, sh as u32, state.angle_deg);
    let bbox_screen = contain_fit(image_rect.shrink(HANDLE + 4.0), Vec2::new(bw, bh));
    let screen_scale = bbox_screen.width() / bw;

    // Draw the source rotated about the bounding box center via a textured mesh
    // — GPU rotation, no CPU re-decode while the angle drags.
    paint_rotated_image(
        ui,
        &tex,
        bbox_screen.center(),
        screen_scale,
        state.angle_deg,
    );

    // The crop rect in screen space, mapped from normalized bbox coords.
    let crop_screen = norm_to_screen(state.rect, bbox_screen);

    // Dim everything outside the crop rect.
    paint_surround_dim(ui, image_rect, crop_screen);

    // Interaction: hit-test handles and drag.
    let resp = ui.interact(
        image_rect,
        ui.id().with(("crop_canvas", id.0)),
        Sense::click_and_drag(),
    );
    process_drag(state, &resp, bbox_screen, crop_screen, sw, sh);

    // Recompute after a drag so the overlay reflects this frame's edit.
    clamp_to_inset(state, sw, sh);
    let crop_screen = norm_to_screen(state.rect, bbox_screen);

    // Cursor feedback: the dragged handle while dragging, else whatever the
    // pointer hovers over.
    set_handle_cursor(ui, &resp, state, crop_screen);

    paint_thirds(
        ui,
        crop_screen,
        state.drag.is_some() || state.angle_deg.abs() > 0.01,
    );
    paint_crop_border_and_handles(ui, crop_screen);
}

/// Set the pointer cursor to match the handle under it (or the active drag):
/// diagonal resize on corners, axis resize on edges, move inside.
fn set_handle_cursor(ui: &Ui, resp: &egui::Response, state: &CropEditState, crop: Rect) {
    let handle = state.drag.or_else(|| {
        resp.hover_pos()
            .filter(|p| crop.expand(HANDLE * 3.0).contains(*p))
            .and_then(|p| hit_handle(crop, p))
    });
    let Some(handle) = handle else { return };
    use egui::CursorIcon as C;
    let icon = match handle {
        Handle::TopLeft | Handle::BottomRight => C::ResizeNwSe,
        Handle::TopRight | Handle::BottomLeft => C::ResizeNeSw,
        Handle::Top | Handle::Bottom => C::ResizeVertical,
        Handle::Left | Handle::Right => C::ResizeHorizontal,
        Handle::Move => C::Grab,
    };
    ui.ctx().set_cursor_icon(icon);
}

/// Map a normalized-over-bbox rect into screen coordinates.
fn norm_to_screen(r: NormRect, bbox: Rect) -> Rect {
    Rect::from_min_size(
        Pos2::new(
            bbox.min.x + r.x * bbox.width(),
            bbox.min.y + r.y * bbox.height(),
        ),
        Vec2::new(r.w * bbox.width(), r.h * bbox.height()),
    )
}

/// Draw the source texture rotated `angle_deg` about `center`, scaled by
/// `scale` (screen px per source px). Two triangles, full UV.
fn paint_rotated_image(ui: &Ui, tex: &TexRef, center: Pos2, scale: f32, angle_deg: f32) {
    let a = angle_deg.to_radians();
    let (sin, cos) = (a.sin(), a.cos());
    let (hw, hh) = (tex.size.x * 0.5 * scale, tex.size.y * 0.5 * scale);
    // Corner offsets (TL, TR, BR, BL) before rotation.
    let corners = [(-hw, -hh), (hw, -hh), (hw, hh), (-hw, hh)];
    let rotated: Vec<Pos2> = corners
        .iter()
        .map(|&(x, y)| Pos2::new(center.x + x * cos - y * sin, center.y + x * sin + y * cos))
        .collect();
    let uv = [
        Pos2::new(0.0, 0.0),
        Pos2::new(1.0, 0.0),
        Pos2::new(1.0, 1.0),
        Pos2::new(0.0, 1.0),
    ];
    let mut mesh = Mesh::with_texture(tex.id);
    for (pos, uv) in rotated.iter().zip(uv.iter()) {
        mesh.colored_vertex(*pos, Color32::WHITE);
        let i = mesh.vertices.len() - 1;
        mesh.vertices[i].uv = *uv;
    }
    mesh.add_triangle(0, 1, 2);
    mesh.add_triangle(0, 2, 3);
    ui.painter().add(Shape::mesh(mesh));
}

/// Dim the area of `outer` outside `hole` (the crop window).
fn paint_surround_dim(ui: &Ui, outer: Rect, hole: Rect) {
    let dim = Color32::from_black_alpha(140);
    let p = ui.painter();
    // Top, bottom, left, right bands around the hole.
    let top = Rect::from_min_max(outer.min, Pos2::new(outer.max.x, hole.min.y));
    let bottom = Rect::from_min_max(Pos2::new(outer.min.x, hole.max.y), outer.max);
    let left = Rect::from_min_max(
        Pos2::new(outer.min.x, hole.min.y),
        Pos2::new(hole.min.x, hole.max.y),
    );
    let right = Rect::from_min_max(
        Pos2::new(hole.max.x, hole.min.y),
        Pos2::new(outer.max.x, hole.max.y),
    );
    for r in [top, bottom, left, right] {
        if r.is_positive() {
            p.rect_filled(r, 0.0, dim);
        }
    }
}

/// Draw the rule-of-thirds grid inside the crop rect. A denser feel while
/// actively straightening/dragging helps line up a horizon.
fn paint_thirds(ui: &Ui, crop: Rect, emphasize: bool) {
    let p = ui.painter();
    let alpha = if emphasize { 180 } else { 110 };
    let stroke = Stroke::new(1.0, Color32::from_white_alpha(alpha));
    for i in 1..3 {
        let x = crop.min.x + crop.width() * i as f32 / 3.0;
        p.vline(x, crop.y_range(), stroke);
        let y = crop.min.y + crop.height() * i as f32 / 3.0;
        p.hline(crop.x_range(), y, stroke);
    }
}

/// Draw the crop border and its eight handles.
fn paint_crop_border_and_handles(ui: &Ui, crop: Rect) {
    let p = ui.painter();
    p.rect_stroke(
        crop,
        0.0,
        Stroke::new(1.5, theme::SELECT_OUTLINE),
        StrokeKind::Inside,
    );
    for (_, center) in handle_centers(crop) {
        let box_rect = Rect::from_center_size(center, Vec2::splat(HANDLE * 2.0));
        p.rect_filled(box_rect, 1.0, theme::SELECT_OUTLINE);
    }
}

/// Each resize handle paired with its center on `crop` — one source of truth for
/// both painting the handles and hit-testing them, so the two can't drift.
fn handle_centers(crop: Rect) -> [(Handle, Pos2); 8] {
    let (l, r, t, b) = (crop.left(), crop.right(), crop.top(), crop.bottom());
    let (cx, cy) = (crop.center().x, crop.center().y);
    Handle::RESIZE.map(|h| {
        let center = match h {
            Handle::TopLeft => Pos2::new(l, t),
            Handle::TopRight => Pos2::new(r, t),
            Handle::BottomRight => Pos2::new(r, b),
            Handle::BottomLeft => Pos2::new(l, b),
            Handle::Top => Pos2::new(cx, t),
            Handle::Right => Pos2::new(r, cy),
            Handle::Bottom => Pos2::new(cx, b),
            Handle::Left => Pos2::new(l, cy),
            Handle::Move => crop.center(),
        };
        (h, center)
    })
}

/// Decide which handle a press landed on: a corner/edge box, else inside =
/// move, else nothing.
fn hit_handle(crop: Rect, pos: Pos2) -> Option<Handle> {
    for (h, center) in handle_centers(crop) {
        if Rect::from_center_size(center, Vec2::splat(HANDLE * 3.0)).contains(pos) {
            return Some(h);
        }
    }
    // Inside the box (or a hair past its border) translates — so a press right on
    // the edge line still grabs something rather than dying.
    crop.expand(HANDLE).contains(pos).then_some(Handle::Move)
}

/// Apply a handle drag to the normalized crop rect. The dragged edge(s) follow
/// the pointer (clamped so they can't cross the opposite edge); `Move`
/// translates. When an aspect is locked, the resize keeps that ratio, anchored
/// on the fixed edge(s). The inset clamp is applied separately afterward.
fn process_drag(
    state: &mut CropEditState,
    resp: &egui::Response,
    bbox: Rect,
    crop_screen: Rect,
    sw: f32,
    sh: f32,
) {
    if !resp.dragged() {
        state.drag = None;
        return;
    }
    // Grab a handle on drag start — and *keep retrying* on later drag frames while
    // nothing is grabbed yet. egui's drag threshold (or a not-yet-decoded frame at
    // the start) can make the `drag_started` frame's pointer land just off a
    // handle; without the retry the whole gesture would be dead until release.
    if state.drag.is_none()
        && let Some(pos) = resp.interact_pointer_pos()
    {
        state.drag = hit_handle(crop_screen, pos);
    }
    let Some(handle) = state.drag else { return };

    let r = state.rect;
    if handle == Handle::Move {
        let dx = resp.drag_delta().x / bbox.width();
        let dy = resp.drag_delta().y / bbox.height();
        state.rect = NormRect {
            x: r.x + dx,
            y: r.y + dy,
            w: r.w,
            h: r.h,
        };
        return;
    }
    let Some(pos) = resp.interact_pointer_pos() else {
        return;
    };
    // Pointer in normalized bbox coords, clamped to the box.
    let nx = ((pos.x - bbox.min.x) / bbox.width()).clamp(0.0, 1.0);
    let ny = ((pos.y - bbox.min.y) / bbox.height()).clamp(0.0, 1.0);

    // The resize geometry (free-edge move + aspect lock) is the pure domain
    // function; this only maps the handle to which edges move and the locked
    // ratio into the bounding-box's normalized w/h.
    let sides = crops::DragSides {
        left: handle.moves_left(),
        right: handle.moves_right(),
        top: handle.moves_top(),
        bottom: handle.moves_bottom(),
    };
    let ratio_norm = state.aspect.ratio(sw / sh, state.portrait).map(|ratio| {
        let (bw, bh) = crops::bounding_box(sw as u32, sh as u32, state.angle_deg);
        ratio * bh / bw
    });
    state.rect = crops::drag_rect(r, sides, nx, ny, ratio_norm);
}

/// One-shot: snap the rect to the active aspect's largest centered fit inside
/// the rotated frame's inset bound (free aspect just clamps).
fn refit_to_aspect(state: &mut CropEditState, sw: f32, sh: f32) {
    let inset = crops::max_inset_rect(sw as u32, sh as u32, state.angle_deg);
    match state.aspect.ratio(sw / sh, state.portrait) {
        Some(ratio) => {
            let (bw, bh) = crops::bounding_box(sw as u32, sh as u32, state.angle_deg);
            state.rect = crops::fit_aspect(bw, bh, inset, ratio);
        }
        None => state.rect = crops::clamp_rect(state.rect, inset),
    }
}

/// Keep the working rect valid each frame: inside the inset bound, preserving the
/// locked ratio (shrinking to fit when the straighten angle narrowed the bound).
/// All clamping is the pure, panic-safe domain math.
fn clamp_to_inset(state: &mut CropEditState, sw: f32, sh: f32) {
    let inset = crops::max_inset_rect(sw as u32, sh as u32, state.angle_deg);
    state.rect = match state.aspect.ratio(sw / sh, state.portrait) {
        Some(ratio) => {
            let (bw, bh) = crops::bounding_box(sw as u32, sh as u32, state.angle_deg);
            let rn = ratio * bh / bw;
            crops::clamp_rect_ratio(state.rect, inset, rn)
        }
        None => crops::clamp_rect(state.rect, inset),
    };
}
