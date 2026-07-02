//! Board view — a freeform canvas paired with the contact-sheet grid.
//!
//! Layout: a resizable, collapsible left panel renders the normal grid as a
//! drag source; the rest is an [`egui::Scene`] (pan/zoom) canvas where placed
//! photos can be dragged around. Dropping a grid cell onto the canvas places it
//! (`AddToBoard`); dragging a placed photo moves it, coalesced into one undo
//! entry on release (`MoveOnBoard`); an aborted drag (Esc) commits nothing.
//! `Delete` removes the canvas selection (`RemoveFromBoard`).
//!
//! Only placements are owned (in the session's board store). Everything here —
//! pan/zoom, panel width, the canvas selection, the live drag — is ephemeral.

use std::collections::HashSet;

use dcs_app::{AppAction, Session};
use dcs_domain::photo::PhotoId;
use dcs_domain::view::{Pos, ViewId};
use egui::{Color32, Pos2, Rect, Scene, Sense, Stroke, StrokeKind, Vec2};

use crate::context_menu::{self, BoardItemPick, MenuTarget};
use crate::grid::{self, TexRef, TextureCache};
use crate::theme;

/// The drag-and-drop payload from the left grid to the canvas: the photos a
/// grid cell drag is carrying (the cell, or the whole selection it belongs to).
#[derive(Clone)]
pub struct BoardDragPayload {
    pub photos: Vec<PhotoId>,
}

/// Ephemeral board UI state owned by the app across frames.
pub struct BoardUiState {
    /// The scene→view rectangle (pan/zoom). Mutated by `Scene` on interaction.
    scene_rect: Rect,
    /// Whether the left grid panel is expanded.
    grid_open: bool,
    /// Canvas selection, by photo. Distinct from the grid's pool selection.
    selection: HashSet<PhotoId>,
    /// The live move gesture, if one is in progress.
    drag: Option<BoardDrag>,
}

impl Default for BoardUiState {
    fn default() -> Self {
        BoardUiState {
            // A roomy initial window into scene space; `Scene` re-fits if invalid.
            scene_rect: Rect::from_min_size(Pos2::ZERO, Vec2::new(1200.0, 800.0)),
            grid_open: true,
            selection: HashSet::new(),
            drag: None,
        }
    }
}

impl BoardUiState {
    /// Drop any in-progress drag without committing it — used on view changes and
    /// on Esc-abort so a half-finished gesture never leaks a move.
    pub fn end_drag(&mut self) {
        self.drag = None;
    }

    /// Whether a move gesture is currently in progress.
    pub fn is_dragging(&self) -> bool {
        self.drag.is_some()
    }

    /// The center of the current canvas view, in scene space — where a keyboard
    /// `Enter` placement lands so it's visible without panning.
    pub fn view_center(&self) -> Pos {
        let c = self.scene_rect.center();
        Pos::new(c.x, c.y)
    }
}

/// A live move gesture: which photos are moving, the one under the cursor, and
/// the accumulated scene-space offset. Esc clears it (rolling the items back).
struct BoardDrag {
    photos: Vec<PhotoId>,
    grabbed: PhotoId,
    offset: Vec2,
}

/// What the board hands back to the app — a registry action from the left grid's
/// context menu (routed through the one dispatch path) and the sidebar grid's
/// column count, which the app's `↑↓` row navigation needs.
pub struct BoardResponse {
    pub action: Option<AppAction>,
    pub cols: usize,
}

/// Longest-edge size, in scene units, of a freshly placed item at scale 1.0.
const ITEM_BASE_EDGE: f32 = 240.0;
/// Cascade step so a multi-photo drop (or keyboard placement) doesn't land
/// perfectly stacked.
pub(crate) const CASCADE: f32 = 28.0;
/// Fixed cell size for the sidebar grid — chosen so three thumbnails sit side by
/// side at the panel's default width. The sidebar is a drag source, not a
/// zoomable grid, so its zoom doesn't track the main grid's.
const SIDEBAR_CELL: f32 = 88.0;

/// Render the board view. `grid_textures` backs the left panel (shared with the
/// normal grid); `canvas_textures` is the board's own cache so its frame
/// bookkeeping is independent of whether the panel is open.
#[allow(clippy::too_many_arguments)]
pub fn show(
    ui: &mut egui::Ui,
    session: &mut Session,
    grid_textures: &mut TextureCache,
    canvas_textures: &mut TextureCache,
    state: &mut BoardUiState,
    collapsed: &mut HashSet<String>,
    grid_ctx: &mut Option<MenuTarget>,
    scroll_to_focus: bool,
) -> BoardResponse {
    let Some(view) = session.primary_board() else {
        ui.centered_and_justified(|ui| {
            ui.label(
                egui::RichText::new("board unavailable while the project is read-only")
                    .monospace()
                    .color(theme::TEXT_DIM),
            );
        });
        return BoardResponse {
            action: None,
            cols: 1,
        };
    };

    let (grid_action, cols) = left_panel(
        ui,
        session,
        grid_textures,
        state,
        collapsed,
        grid_ctx,
        scroll_to_focus,
    );
    let canvas_action = canvas(ui, session, canvas_textures, state, view);
    // Only one menu is ever open at a time, so either side yields at most one.
    BoardResponse {
        action: canvas_action.or(grid_action),
        cols,
    }
}

/// The left panel: the grid as a drag source, with a collapse toggle. When
/// collapsed it shrinks to a thin re-open strip. Returns any context-menu action
/// and the grid's column count (for keyboard row navigation).
#[allow(clippy::too_many_arguments)]
fn left_panel(
    ui: &mut egui::Ui,
    session: &mut Session,
    textures: &mut TextureCache,
    state: &mut BoardUiState,
    collapsed: &mut HashSet<String>,
    grid_ctx: &mut Option<MenuTarget>,
    scroll_to_focus: bool,
) -> (Option<AppAction>, usize) {
    let mut action = None;
    let mut cols = 1;
    let mut set_open: Option<bool> = None;
    if state.grid_open {
        egui::Panel::left("board_grid_panel")
            .resizable(true)
            .default_size(300.0)
            .size_range(egui::Rangef::new(200.0, 560.0))
            .frame(
                egui::Frame::default()
                    .fill(theme::SHEET_BG)
                    .inner_margin(egui::Margin::same(4)),
            )
            .show_inside(ui, |ui| {
                ui.horizontal(|ui| {
                    if ui
                        .small_button("‹")
                        .on_hover_text("Hide the photo grid")
                        .clicked()
                    {
                        set_open = Some(false);
                    }
                    ui.label(
                        egui::RichText::new("drag photos onto the canvas →")
                            .monospace()
                            .size(11.0)
                            .color(theme::TEXT_DIM),
                    );
                });
                let view_width = ui.available_width();
                let resp = grid::show(
                    ui,
                    session,
                    textures,
                    SIDEBAR_CELL,
                    view_width,
                    scroll_to_focus,
                    collapsed,
                    grid_ctx,
                    true,
                );
                action = resp.action;
                cols = resp.cols;
            });
    } else {
        egui::Panel::left("board_grid_collapsed")
            .resizable(false)
            .exact_size(26.0)
            .frame(
                egui::Frame::default()
                    .fill(theme::CHROME_BG)
                    .inner_margin(egui::Margin::same(4)),
            )
            .show_inside(ui, |ui| {
                if ui
                    .small_button("›")
                    .on_hover_text("Show the photo grid")
                    .clicked()
                {
                    set_open = Some(true);
                }
            });
    }
    if let Some(open) = set_open {
        state.grid_open = open;
    }
    (action, cols)
}

/// The pan/zoom canvas. Paints placed photos, handles drag-to-move (coalesced),
/// drop-to-place, click-to-select, the per-image right-click menu, and `Delete`.
/// Returns a registry action chosen from an image's context menu, dispatched by
/// the app through the one registry path (board-native picks are applied here).
fn canvas(
    ui: &mut egui::Ui,
    session: &mut Session,
    textures: &mut TextureCache,
    state: &mut BoardUiState,
    view: ViewId,
) -> Option<AppAction> {
    textures.begin_frame();
    // Owned placements, snapshotted so the per-item texture borrow of `session`
    // doesn't clash with reading the board.
    let mut items: Vec<dcs_domain::view::BoardItem> = session.board_items(view).to_vec();
    // Drop any selection entries whose photos left the board (e.g. a missing-file
    // prune), so stale ids don't linger or drive empty work.
    state
        .selection
        .retain(|id| items.iter().any(|it| it.photo == *id));
    // While dragging, paint the grabbed items last so they ride on top of the
    // rest — a stable partition of the frame snapshot only. The persisted z-order
    // isn't touched until the move actually commits (an aborted drag changes
    // nothing), so this is display-order, not owned state.
    if let Some(d) = &state.drag {
        items.sort_by_key(|it| d.photos.contains(&it.photo));
    }

    // A single recenter button pinned to the canvas's bottom-right corner: fit
    // the view to all placed photos (so a lost/panned-away canvas is one click
    // from showing everything again). Pinned to the canvas rect, not the window.
    let corner = ui.max_rect().right_bottom() + Vec2::new(-12.0, -12.0);
    egui::Area::new(ui.id().with("board_recenter"))
        .fixed_pos(corner)
        .pivot(egui::Align2::RIGHT_BOTTOM)
        .show(ui.ctx(), |ui| {
            if ui
                .button(egui::RichText::new("recenter").monospace().size(11.0))
                .on_hover_text("Fit the view to all placed photos")
                .clicked()
            {
                state.scene_rect = fit_rect(&items);
            }
        });

    // Deferred owned mutations — applied after the scene closure so no session
    // borrow is held across dispatch.
    let mut to_add: Option<(Vec<PhotoId>, Pos)> = None;
    let mut to_move: Option<Vec<(PhotoId, Pos)>> = None;
    let mut started: Option<PhotoId> = None;
    let mut frame_delta = Vec2::ZERO;
    let mut released = false;
    let mut background_clicked = false;
    // Context-menu results: a registry action to dispatch, and board-native picks
    // to apply, all deferred out of the scene closure. `menu_remove` removes the
    // canvas selection (right-click keeps an existing multi-selection intact).
    let mut menu_action: Option<AppAction> = None;
    let mut menu_raise: Option<PhotoId> = None;
    let mut menu_remove = false;
    // The grabbed photo to raise once a move actually commits (raise-on-grab
    // would persist even for an aborted drag — spec #35 says abort commits
    // nothing).
    let mut to_raise: Option<PhotoId> = None;

    let scene = Scene::new().zoom_range(egui::Rangef::new(0.1, 4.0));
    scene.show(ui, &mut state.scene_rect, |ui| {
        // A background drop target + click catcher over the visible region.
        let bg = ui.interact(ui.clip_rect(), ui.id().with("board_bg"), Sense::click());
        // Scene-local pointer position via the layer transform (both the drop
        // point and the live drag math live in scene space).
        let scene_ptr = scene_pointer(ui);
        // A release that drops a payload places photos; otherwise a plain click
        // clears the selection. The two are mutually exclusive on one release.
        if let Some(payload) = bg.dnd_release_payload::<BoardDragPayload>() {
            if let Some(p) = scene_ptr {
                to_add = Some((payload.photos.clone(), Pos::new(p.x, p.y)));
            }
        } else if bg.clicked() {
            background_clicked = true;
        }

        // Scene→screen scale, so each item can be decoded to its on-screen size.
        let scaling = ui
            .ctx()
            .layer_transform_to_global(ui.layer_id())
            .map(|t| t.scaling)
            .unwrap_or(1.0);
        let ppp = ui.ctx().pixels_per_point();
        let visible = ui.clip_rect();

        for item in &items {
            let offset = state
                .drag
                .as_ref()
                .filter(|d| d.photos.contains(&item.photo))
                .map_or(Vec2::ZERO, |d| d.offset);
            let base = ITEM_BASE_EDGE * item.scale;
            // Conservative square bound (height ≤ base) for an off-screen cull, so
            // a big board only decodes/paints what's actually in view.
            let bound = Rect::from_min_size(
                Pos2::new(item.pos.x + offset.x, item.pos.y + offset.y),
                Vec2::splat(base),
            );
            if !visible.expand(base).intersects(bound) {
                continue;
            }
            // Decode a sharp frame sized to the item's current on-screen pixels;
            // the GPU scales between sizes, so zoom stays crisp without thrash.
            // Always request (board membership is independent of the sidebar's
            // filter, so a placed photo may have no base thumb to fall back to);
            // `board_tier` floors the size and the cull keeps it to visible items.
            let target_px = (base * scaling * ppp).ceil() as u32;
            session.request_board(item.photo, target_px);
            // Prefer the sharp board frame, else the base thumb until it lands.
            let src = session.board_or_thumb(item.photo);
            let tex = textures.view_texture(ui, item.photo, src);
            let rect = item_rect(item, tex, offset);
            let id = ui.id().with(("board_item", item.photo.0));
            let resp = ui.interact(rect, id, Sense::click_and_drag());

            paint_item(ui, rect, tex, state.selection.contains(&item.photo));

            if resp.drag_started() {
                started = Some(item.photo);
            }
            if let Some(d) = &state.drag
                && d.grabbed == item.photo
            {
                if resp.dragged() {
                    frame_delta = resp.drag_delta();
                }
                if resp.drag_stopped() {
                    released = true;
                }
            }
            if resp.clicked() {
                let cmd = ui.input(|i| i.modifiers.command);
                if cmd {
                    if !state.selection.remove(&item.photo) {
                        state.selection.insert(item.photo);
                    }
                } else {
                    state.selection.clear();
                    state.selection.insert(item.photo);
                }
            }
            // Right-click aims both selections at the photo so the shared photo
            // actions target it — but right-clicking inside an existing canvas
            // multi-selection leaves it intact (the file-manager convention, and
            // what the grid does), so a menu "Remove from board" can act on the
            // whole set.
            if resp.secondary_clicked() {
                if !state.selection.contains(&item.photo) {
                    state.selection.clear();
                    state.selection.insert(item.photo);
                }
                if let Some(idx) = session.display_index_of(item.photo) {
                    session.click_select(idx);
                }
            }
            resp.context_menu(|ui| {
                // `display_index_of` walks the visible order, so compute it only
                // while the menu is actually open (not per item per frame).
                let idx = session.display_index_of(item.photo);
                match context_menu::board_item_menu(ui, session, idx) {
                    Some(BoardItemPick::Registry(a)) => menu_action = Some(a),
                    Some(BoardItemPick::Raise) => menu_raise = Some(item.photo),
                    Some(BoardItemPick::Remove) => menu_remove = true,
                    None => {}
                }
            });
        }
    });

    // A click on empty canvas clears the selection.
    if background_clicked {
        state.selection.clear();
    }

    // Begin a drag: the grabbed photo, plus the whole canvas selection when the
    // grabbed photo is part of it. Make sure the grabbed photo is selected.
    if let Some(photo) = started {
        let photos = if state.selection.contains(&photo) && state.selection.len() > 1 {
            ordered_selection(&items, &state.selection)
        } else {
            state.selection.clear();
            state.selection.insert(photo);
            vec![photo]
        };
        state.drag = Some(BoardDrag {
            photos,
            grabbed: photo,
            offset: Vec2::ZERO,
        });
    }

    // Accumulate this frame's movement into the live offset. (Esc-to-abort is
    // handled in the app's key router so it doesn't also clear the grid
    // selection — dropping the drag rolls items back to committed positions.)
    if let Some(d) = state.drag.as_mut() {
        d.offset += frame_delta;
    }

    // On release, commit a single coalesced move, then clear.
    if released
        && let Some(d) = state.drag.take()
        && d.offset != Vec2::ZERO
    {
        let moves: Vec<(PhotoId, Pos)> = items
            .iter()
            .filter(|it| d.photos.contains(&it.photo))
            .map(|it| {
                (
                    it.photo,
                    Pos::new(it.pos.x + d.offset.x, it.pos.y + d.offset.y),
                )
            })
            .collect();
        to_move = Some(moves);
        // A committed move brings the grabbed photo to the front.
        to_raise = Some(d.grabbed);
    }

    // `Delete` and the menu's "Remove from board" both drop the canvas selection.
    // Skipped while a text field owns the keyboard — otherwise Backspace while
    // typing in the search field or a palette would remove board photos.
    let delete = !ui.ctx().egui_wants_keyboard_input()
        && ui.input(|i| i.key_pressed(egui::Key::Delete) || i.key_pressed(egui::Key::Backspace));
    if (delete || menu_remove) && !state.selection.is_empty() {
        let removed: Vec<PhotoId> = state.selection.iter().copied().collect();
        session.remove_from_board(view, removed.clone());
        for id in removed {
            state.selection.remove(&id);
        }
    }

    // Apply the deferred placements/moves.
    if let Some((photos, at)) = to_add {
        let placed: Vec<(PhotoId, Pos)> = photos
            .iter()
            .enumerate()
            .filter(|(_, id)| !items.iter().any(|it| it.photo == **id))
            .map(|(k, id)| {
                (
                    *id,
                    Pos::new(at.x + k as f32 * CASCADE, at.y + k as f32 * CASCADE),
                )
            })
            .collect();
        if !placed.is_empty() {
            session.add_to_board(view, placed);
        }
    }
    if let Some(moves) = to_move {
        session.move_on_board(view, moves);
    }
    // Raise the grabbed photo after its move commits, and honor a menu raise.
    if let Some(photo) = to_raise.or(menu_raise) {
        session.raise_on_board(view, photo);
    }

    textures.evict_over_budget();
    menu_action
}

/// A scene rect that frames every placed item, with a margin so nothing sits on
/// the edge. Items are bounded by a `base`-square (height ≤ base), so the fit is
/// always generous. Falls back to the default window when the board is empty.
fn fit_rect(items: &[dcs_domain::view::BoardItem]) -> Rect {
    let mut bbox: Option<Rect> = None;
    for item in items {
        let base = ITEM_BASE_EDGE * item.scale;
        let r = Rect::from_min_size(Pos2::new(item.pos.x, item.pos.y), Vec2::splat(base));
        bbox = Some(bbox.map_or(r, |b| b.union(r)));
    }
    match bbox {
        Some(b) => b.expand(ITEM_BASE_EDGE * 0.25),
        None => Rect::from_min_size(Pos2::ZERO, Vec2::new(1200.0, 800.0)),
    }
}

/// The scene-space rect for an item: longest edge `ITEM_BASE_EDGE * scale`, the other
/// edge by the thumbnail's aspect (square until it decodes). `offset` is the
/// live drag displacement.
fn item_rect(item: &dcs_domain::view::BoardItem, tex: Option<TexRef>, offset: Vec2) -> Rect {
    let aspect = tex
        .filter(|t| t.size.y > 0.0)
        .map(|t| t.size.x / t.size.y)
        .unwrap_or(1.0);
    let base = ITEM_BASE_EDGE * item.scale;
    let size = if aspect >= 1.0 {
        Vec2::new(base, base / aspect)
    } else {
        Vec2::new(base * aspect, base)
    };
    let min = Pos2::new(item.pos.x + offset.x, item.pos.y + offset.y);
    Rect::from_min_size(min, size)
}

/// Paint a placed photo: its thumbnail filling `rect` (which already carries the
/// aspect), a hairline frame, and a brighter outline when selected.
fn paint_item(ui: &egui::Ui, rect: Rect, tex: Option<TexRef>, selected: bool) {
    let painter = ui.painter();
    if let Some(tex) = tex {
        painter.image(
            tex.id,
            rect,
            Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
            Color32::WHITE,
        );
    } else {
        painter.rect_filled(rect, 0.0, theme::CELL_EMPTY);
    }
    let stroke = if selected {
        Stroke::new(2.0, theme::FOCUS_OUTLINE)
    } else {
        Stroke::new(1.0, theme::HAIRLINE)
    };
    painter.rect_stroke(rect, 0.0, stroke, StrokeKind::Outside);
}

/// The selected photos in board stacking order — what a multi-photo move carries.
fn ordered_selection(
    items: &[dcs_domain::view::BoardItem],
    selection: &HashSet<PhotoId>,
) -> Vec<PhotoId> {
    items
        .iter()
        .map(|it| it.photo)
        .filter(|p| selection.contains(p))
        .collect()
}

/// The pointer position in scene space, mapped through the active layer
/// transform. `None` when the pointer is off-screen.
fn scene_pointer(ui: &egui::Ui) -> Option<Pos2> {
    let global = ui.ctx().input(|i| i.pointer.interact_pos())?;
    match ui.ctx().layer_transform_from_global(ui.layer_id()) {
        Some(t) => Some(t * global),
        None => Some(global),
    }
}
