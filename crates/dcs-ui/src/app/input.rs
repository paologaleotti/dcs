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
        // Verdict/undo and the rest of the registry work in both views, so route
        // them first — full key parity in the gallery.
        for action in keymap::actions_for_input(ctx) {
            self.dispatch(action, ctx);
        }
        match self.view {
            // Grid geometry keys are a pure UI concern (they need the column
            // count), so they stay out of the registry.
            ViewMode::Grid => {
                self.handle_grid_keys(ctx);
                // Space / F open the focused photo in the gallery.
                if ctx.input(|i| i.key_pressed(Key::Space) || i.key_pressed(Key::F)) {
                    self.enter_gallery();
                }
            }
            ViewMode::Gallery => self.handle_gallery_keys(ctx),
        }
    }

    /// Gallery keys: `←`/`↑` previous, `→`/`↓` next over the visible
    /// order; `Z` toggles fit ↔ 1:1; `Space`/`Esc` return to the grid. `A`/`X`/
    /// undo already routed through the registry.
    fn handle_gallery_keys(&mut self, ctx: &egui::Context) {
        if ctx.input(|i| i.key_pressed(Key::Escape) || i.key_pressed(Key::Space)) {
            self.exit_gallery();
            return;
        }
        if ctx.input(|i| i.key_pressed(Key::Z)) {
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
        if ctx.input(|i| i.key_pressed(Key::Escape)) {
            self.session.clear_selection();
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
