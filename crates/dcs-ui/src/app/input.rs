use dcs_domain::grouping::GroupKind;
use egui::Key;

use super::{DcsApp, ViewMode};
use crate::keymap;

impl DcsApp {
    pub(super) fn handle_keys(&mut self, ctx: &egui::Context) {
        // Any picker owns the keyboard while open (it consumes its own keys in
        // `Picker::show`), so the grid stays inert behind it — just bail.
        if self.palette.is_open()
            || self.tag_palette.is_open()
            || self.filter_palette.is_open()
            || self.zone_picker.is_open()
            || self.camera_zone_picker.is_open()
        {
            return;
        }
        // The About window is a plain modal: Esc closes it, grid keys inert.
        if self.show_about {
            if ctx.input(|i| i.key_pressed(Key::Escape)) {
                self.show_about = false;
            }
            return;
        }
        // The shortcuts reference is a plain modal too: Esc closes it.
        if self.show_shortcuts {
            if ctx.input(|i| i.key_pressed(Key::Escape)) {
                self.show_shortcuts = false;
            }
            return;
        }
        // The tag manager owns the keyboard while open (it has text fields); Esc
        // closes it, grid keys inert behind it.
        if self.show_tag_manager {
            if ctx.input(|i| i.key_pressed(Key::Escape)) && !ctx.egui_wants_keyboard_input() {
                self.show_tag_manager = false;
            }
            return;
        }
        // The metadata window: Esc (or `I` again) closes it, grid keys inert.
        if self.show_metadata {
            if ctx.input(|i| i.key_pressed(Key::Escape) || i.key_pressed(Key::I)) {
                self.show_metadata = false;
            }
            return;
        }
        // The export dialog is modal too: Esc dismisses it, grid keys inert.
        if self.export.open {
            if ctx.input(|i| i.key_pressed(Key::Escape)) {
                self.export.open = false;
                self.session.clear_export_status();
            }
            return;
        }
        // A focused text field owns the keyboard so its edits don't leak to the
        // grid.
        if ctx.egui_wants_keyboard_input() {
            return;
        }
        // Cmd/Ctrl+P opens the command palette.
        if ctx.input_mut(|i| i.consume_key(egui::Modifiers::COMMAND, Key::P)) {
            self.palette.open();
            return;
        }
        // `?` opens the keyboard-shortcuts reference. Handled here, not in the
        // keymap table: `?` arrives with Shift held, which the table's exact
        // modifier match would reject.
        if ctx.input(|i| i.key_pressed(Key::Questionmark)) {
            self.dispatch(dcs_app::AppAction::Shortcuts, ctx);
            return;
        }
        // Verdict/undo and the rest of the registry work in both views, so route
        // them first — full key parity in the gallery.
        for action in keymap::actions_for_input(ctx) {
            self.dispatch(action, ctx);
        }
        // The crop editor, when open, owns the keyboard (over the gallery).
        if self.crop_edit.is_some() {
            self.handle_crop_keys(ctx);
            return;
        }
        match self.view {
            // Grid geometry keys are a pure UI concern (they need the column
            // count), so they stay out of the registry. Space (open gallery) is a
            // registry binding, dispatched above.
            ViewMode::Grid => self.handle_grid_keys(ctx),
            ViewMode::Gallery => self.handle_gallery_keys(ctx),
            ViewMode::Board => self.handle_board_keys(ctx),
        }
    }

    /// Board keys: arrows navigate the sidebar grid (so the focus the registry's
    /// verdict/tag keys act on is visible and reachable), `Enter` places the
    /// selection/focus onto the canvas, and `Esc` aborts a live drag (rolling it
    /// back) or, when none, clears the sidebar selection. The canvas's pointer
    /// keys (Delete) live in `board::show`.
    fn handle_board_keys(&mut self, ctx: &egui::Context) {
        self.arrow_nav(ctx);
        if ctx.input(|i| i.key_pressed(Key::Enter)) {
            self.place_focused_on_board();
        }
        if ctx.input(|i| i.key_pressed(Key::Escape)) {
            if self.board.is_dragging() {
                self.board.end_drag();
            } else {
                self.session.clear_selection();
            }
        }
    }

    /// Place the sidebar selection — or, with none, the focused photo — onto the
    /// board at the center of the current canvas view, cascading multiples and
    /// skipping any already placed. One undo entry.
    fn place_focused_on_board(&mut self) {
        let Some(view) = self.session.primary_board() else {
            return;
        };
        let mut photos = self.session.selected_ids();
        if photos.is_empty() {
            let Some(id) = self
                .session
                .focus()
                .and_then(|i| self.session.photo_at(i))
                .map(|p| p.id)
            else {
                return;
            };
            photos = vec![id];
        }
        // Skip already-placed photos before cascading, so the offset has no gaps
        // (matching the drop path) and the store's dedup has nothing to drop.
        let on_board: std::collections::HashSet<_> = self
            .session
            .board_items(view)
            .iter()
            .map(|it| it.photo)
            .collect();
        let at = self.board.view_center();
        let placed: Vec<_> = photos
            .into_iter()
            .filter(|id| !on_board.contains(id))
            .enumerate()
            .map(|(k, id)| {
                let step = k as f32 * crate::board::CASCADE;
                (id, dcs_domain::view::Pos::new(at.x + step, at.y + step))
            })
            .collect();
        if !placed.is_empty() {
            self.session.add_to_board(view, placed);
        }
    }

    /// Crop-editor keys: `Enter` applies, `Esc` cancels, `R` resets, `[`/`]`
    /// nudge the straighten angle by 0.1°. The crop rect/handles are pointer
    /// driven (in `crop::show`); these are the keyboard affordances.
    fn handle_crop_keys(&mut self, ctx: &egui::Context) {
        let Some(state) = self.crop_edit.as_mut() else {
            self.exit_crop();
            return;
        };
        if ctx.input(|i| i.key_pressed(Key::Escape)) {
            self.exit_crop();
            return;
        }
        if ctx.input(|i| i.key_pressed(Key::Enter)) {
            let edit = state.to_edit();
            let photo = state.photo;
            self.session.set_crop(photo, Some(edit));
            self.exit_crop();
            return;
        }
        if ctx.input(|i| i.key_pressed(Key::R) && !i.modifiers.command) {
            state.reset();
            return;
        }
        let mut delta = 0.0;
        if ctx.input(|i| i.key_pressed(Key::OpenBracket)) {
            delta -= 0.1;
        }
        if ctx.input(|i| i.key_pressed(Key::CloseBracket)) {
            delta += 0.1;
        }
        if delta != 0.0 {
            state.angle_deg = (state.angle_deg + delta).clamp(
                -dcs_domain::crops::MAX_ANGLE_DEG,
                dcs_domain::crops::MAX_ANGLE_DEG,
            );
        }
    }

    /// Gallery keys: `←`/`↑` previous, `→`/`↓` next over the visible order; `Z`
    /// toggles fit ↔ 1:1; `Esc` returns to the grid. Space (toggle gallery),
    /// `A`/`X`, and undo are registry bindings, dispatched before this runs.
    fn handle_gallery_keys(&mut self, ctx: &egui::Context) {
        if ctx.input(|i| i.key_pressed(Key::Escape)) {
            self.exit_gallery();
            return;
        }
        // Bare `Z` only — the registry runs first and binds Cmd+Z / Cmd+Shift+Z
        // to undo/redo, so without the modifier guard those would also flip the
        // 1:1 zoom as a side effect.
        if ctx.input(|i| i.key_pressed(Key::Z) && !i.modifiers.command && !i.modifiers.shift) {
            self.gallery_full = !self.gallery_full;
        }
        let prev = ctx.input(|i| i.key_pressed(Key::ArrowLeft) || i.key_pressed(Key::ArrowUp));
        let next = ctx.input(|i| i.key_pressed(Key::ArrowRight) || i.key_pressed(Key::ArrowDown));
        if prev || next {
            let count = self.session.photo_count();
            if count > 0 {
                let cur = self.session.focus().unwrap_or(0);
                let target = if next {
                    (cur + 1).min(count - 1)
                } else {
                    cur.saturating_sub(1)
                };
                // 1:1 framing is per-photo, so reset to fit when stepping on.
                if target != cur {
                    self.gallery_full = false;
                }
                self.session.set_focus(target, false);
                self.scroll_to_focus = true;
            }
        }
    }

    /// Arrow navigation and selection — UI-only because they run over the visual
    /// group layout (column count + collapse), which is a view fact.
    /// `Esc` clears the selection. A move flags the grid to scroll the focus
    /// cell into view next paint.
    fn handle_grid_keys(&mut self, ctx: &egui::Context) {
        self.arrow_nav(ctx);
        if ctx.input(|i| i.key_pressed(Key::Escape)) {
            self.session.clear_selection();
        }
    }

    /// Focus-cursor arrow navigation, shared by the grid view and the board's
    /// sidebar grid (`Shift` extends the selection from the anchor).
    fn arrow_nav(&mut self, ctx: &egui::Context) {
        let shift = ctx.input(|i| i.modifiers.shift);
        if ctx.input(|i| i.key_pressed(Key::ArrowLeft)) {
            self.nav(-1, 0, shift);
        }
        if ctx.input(|i| i.key_pressed(Key::ArrowRight)) {
            self.nav(1, 0, shift);
        }
        if ctx.input(|i| i.key_pressed(Key::ArrowUp)) {
            self.nav(0, -1, shift);
        }
        if ctx.input(|i| i.key_pressed(Key::ArrowDown)) {
            self.nav(0, 1, shift);
        }
    }

    /// Move the focus over the visual group layout. `←→` step through the flat
    /// run of focusable cells (skipping the members a collapsed group hides);
    /// `↑↓` move one visual row, holding the column where the target row allows.
    /// A collapsed group exposes only its cover cell, so focus never lands on a
    /// hidden cell. Group boundaries reset the row, matching the painted layout
    /// (each group flows its own rows of `cols`).
    fn nav(&mut self, dx: isize, dy: isize, extend: bool) {
        let cols = self.cols.max(1);
        let rows = self.nav_rows(cols);
        let Some(first) = rows.first().and_then(|r| r.first()).copied() else {
            return;
        };
        let cur = self.session.focus();
        let Some((r, col)) = cur.and_then(|f| nav_locate(&rows, f)) else {
            // No cursor (or it sat on a now-hidden cell): grab the first cell.
            self.session.set_focus(first, extend);
            self.scroll_to_focus = true;
            return;
        };
        let target = if dy != 0 {
            let nr = (r as isize + dy).clamp(0, rows.len() as isize - 1) as usize;
            let nc = col.min(rows[nr].len() - 1);
            rows[nr][nc]
        } else {
            // Horizontal: walk the flat sequence so moves cross row/group edges.
            let flat: Vec<usize> = rows.iter().flatten().copied().collect();
            let pos = flat.iter().position(|&i| Some(i) == cur).unwrap_or(0);
            let np = (pos as isize + dx).clamp(0, flat.len() as isize - 1) as usize;
            flat[np]
        };
        self.session.set_focus(target, extend);
        self.scroll_to_focus = true;
    }

    /// The focusable cells as visual rows: each group flows in rows of `cols`,
    /// a collapsed group contributes a single row holding only its cover. Mirrors
    /// the grid's paint layout so nav and the painted cells agree.
    fn nav_rows(&self, cols: usize) -> Vec<Vec<usize>> {
        let mut rows = Vec::new();
        for g in self.session.groups() {
            let collapsed = g.kind != GroupKind::Stream && self.collapsed.contains(&g.title);
            if collapsed {
                rows.push(vec![self.session.group_cover(g)]);
                continue;
            }
            let mut c = 0;
            while c < g.count {
                let len = (g.count - c).min(cols);
                rows.push((g.start + c..g.start + c + len).collect());
                c += len;
            }
        }
        rows
    }
}

/// Locate display index `idx` in the navigable row layout as `(row, col)`.
/// `None` when the cell isn't focusable (e.g. hidden inside a collapsed group).
fn nav_locate(rows: &[Vec<usize>], idx: usize) -> Option<(usize, usize)> {
    rows.iter()
        .enumerate()
        .find_map(|(r, row)| row.iter().position(|&i| i == idx).map(|c| (r, c)))
}
