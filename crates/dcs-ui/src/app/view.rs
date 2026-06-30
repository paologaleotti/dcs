use dcs_app::AppAction;
use dcs_domain::cull::AcceptState;
use dcs_domain::filter::{ChipOp, FilterChip};
use egui::{Align, Color32, FontId, Layout, RichText, Sense, Stroke, Ui, Vec2};

use super::{DcsApp, PALETTE_MOD, ViewMode};
use crate::crop;
use crate::gallery;
use crate::grid;
use crate::theme;

impl DcsApp {
    pub(super) fn status_bar(&mut self, ui: &mut Ui) {
        let scanning = if self.session.is_scanning() {
            " · scanning…"
        } else {
            ""
        };
        let (acc, rej, unrev) = self.session.verdict_counts();
        let save_state = if self.session.is_dirty() {
            " · unsaved"
        } else {
            " · saved"
        };
        // When filtered, lead with "N of M" so the narrowed set is unmistakable.
        let shown = if self.session.is_filtered() {
            format!(
                "{} of {} shown",
                self.session.photo_count(),
                self.session.displayable_count()
            )
        } else {
            format!("{} shown", self.session.photo_count())
        };
        let text = format!(
            "{} · {} sel · acc {} · rej {} · unrev {}{}{}",
            shown,
            self.session.selection_count(),
            acc,
            rej,
            unrev,
            scanning,
            save_state
        );
        let import = self.session.import_progress();
        egui::Panel::bottom("status").show_inside(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(RichText::new(text).font(FontId::monospace(12.0)));
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    let hint = format!("{PALETTE_MOD}P commands");
                    ui.label(
                        RichText::new(hint)
                            .font(FontId::monospace(12.0))
                            .color(theme::TEXT_DIM),
                    );
                    if let Some(p) = import {
                        ui.add_space(12.0);
                        let frac = p.done as f32 / p.total as f32;
                        ui.add(
                            egui::ProgressBar::new(frac).desired_width(140.0).text(
                                RichText::new(format!("importing {}/{}", p.done, p.total))
                                    .font(FontId::monospace(11.0)),
                            ),
                        );
                    }
                });
            });
        });
    }

    /// The active-filter bar: a slim accent row that appears **only when a chip
    /// filter is on** (the filter is built from the toolbar `FILTER` dropdown).
    /// Reads the active set back — removable chip pills, multi-chip groups
    /// bracketed with a clickable AND/OR, `clear`, and an `N of M` count — so
    /// being filtered is unmistakable. One dispatch path.
    pub(super) fn filter_bar(&mut self, ui: &mut Ui, ctx: &egui::Context) {
        if !self.session.is_filtered() {
            return;
        }
        let mut clicked: Option<AppAction> = None;
        let filter = self.session.active_filter().clone();
        egui::Panel::top("filter_bar")
            .frame(
                egui::Frame::default()
                    .fill(theme::CHROME_BG)
                    // Small inset above the accent rule; the breathing room goes
                    // *below* it (added before the chips), not above.
                    .inner_margin(egui::Margin {
                        left: 8,
                        right: 8,
                        top: 5,
                        bottom: 8,
                    }),
            )
            .show_inside(ui, |ui| {
                let top = ui.max_rect();
                ui.painter().hline(
                    top.x_range(),
                    top.top(),
                    Stroke::new(2.0, theme::FILTER_ACCENT),
                );
                // Gap between the accent rule and the chips below it.
                ui.add_space(10.0);
                ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                    ui.label(
                        RichText::new("FILTER")
                            .font(FontId::monospace(10.0))
                            .color(theme::FILTER_ACCENT),
                    );
                    ui.add_space(2.0);
                    for (gi, group) in filter.groups.iter().enumerate() {
                        if gi > 0 {
                            ui.label(RichText::new("and").monospace().color(theme::TEXT_DIM));
                        }
                        // Brackets only earn their ink when a group has a
                        // combinator to show (2+ chips).
                        let bracketed = group.chips.len() > 1;
                        if bracketed {
                            ui.label(RichText::new("(").monospace().color(theme::TEXT_DIM));
                        }
                        for (ci, chip) in group.chips.iter().enumerate() {
                            if ci > 0 {
                                let op = match group.op {
                                    ChipOp::Or => "or",
                                    ChipOp::And => "and",
                                };
                                if ui
                                    .small_button(RichText::new(op).monospace())
                                    .on_hover_text("toggle and / or")
                                    .clicked()
                                {
                                    clicked = Some(AppAction::ToggleFilterGroupOp(gi));
                                }
                            }
                            if self.chip_pill(ui, chip) {
                                clicked = Some(AppAction::RemoveFilterChip {
                                    group: gi,
                                    chip: ci,
                                });
                            }
                        }
                        if bracketed {
                            ui.label(RichText::new(")").monospace().color(theme::TEXT_DIM));
                        }
                    }
                    ui.add_space(4.0);
                    if ui
                        .small_button(RichText::new("clear").monospace())
                        .clicked()
                    {
                        clicked = Some(AppAction::ClearFilters);
                    }
                    ui.add_space(8.0);
                    // Tag the whole result set. Generic path opens the tag picker;
                    // a lone search also gets a one-click "tag all as <query>".
                    if ui
                        .small_button(RichText::new("+ tag results").monospace())
                        .on_hover_text("Add a tag to every photo matching this filter")
                        .clicked()
                    {
                        clicked = Some(AppAction::TagResults);
                    }
                    if let Some(query) = self.session.single_search_query()
                        && ui
                            .small_button(RichText::new(format!("+ tag all “{query}”")).monospace())
                            .on_hover_text(format!(
                                "Tag every result with a “{query}” tag (created if new)"
                            ))
                            .clicked()
                    {
                        clicked = Some(AppAction::TagResultsAsSearch);
                    }
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        ui.label(
                            RichText::new(format!(
                                "{} of {}",
                                self.session.photo_count(),
                                self.session.displayable_count()
                            ))
                            .font(FontId::monospace(12.0))
                            .color(theme::FILTER_ACCENT),
                        );
                    });
                });
            });
        if let Some(action) = clicked {
            self.dispatch(action, ctx);
        }
    }

    /// Render one chip as a pill (optional color swatch + label + `×`). Returns
    /// true when its `×` was clicked.
    fn chip_pill(&self, ui: &mut Ui, chip: &FilterChip) -> bool {
        let (label, swatch) = self.chip_label(chip);
        let mut remove = false;
        ui.horizontal(|ui| {
            if let Some(color) = swatch {
                let (rect, _) = ui.allocate_exact_size(Vec2::splat(10.0), Sense::hover());
                ui.painter().rect_filled(rect, 2.0, color);
            }
            ui.label(RichText::new(label).monospace());
            if ui.small_button("×").clicked() {
                remove = true;
            }
        });
        remove
    }

    /// A chip's display label and optional swatch color.
    fn chip_label(&self, chip: &FilterChip) -> (String, Option<Color32>) {
        match chip {
            FilterChip::Verdict(AcceptState::Accepted) => ("accepted".into(), None),
            FilterChip::Verdict(AcceptState::Rejected) => ("rejected".into(), None),
            FilterChip::Verdict(AcceptState::Unreviewed) => ("unreviewed".into(), None),
            FilterChip::Tag(id) => match self.session.tag_def(*id) {
                Some(tag) => (tag.name.clone(), Some(theme::tag_color32(tag.color))),
                None => ("?".into(), None),
            },
            FilterChip::Search(query) => (format!("search: {query}"), None),
        }
    }

    pub(super) fn central(&mut self, ui: &mut Ui) {
        egui::CentralPanel::default()
            .frame(
                egui::Frame::default()
                    .fill(theme::SHEET_BG)
                    // Match the toolbar's horizontal inset so the first column
                    // lines up under it.
                    .inner_margin(egui::Margin::symmetric(8, 6)),
            )
            .show_inside(ui, |ui| {
                // Hold the grid back until the scan has counted and grouped the
                // whole folder, so cells appear in their final places and don't
                // reflow as photos stream in. Thumbnails keep decoding in the
                // background meanwhile, so the settled grid fills in at once.
                if self.session.is_scanning() {
                    self.scanning_state(ui);
                    return;
                }
                if self.session.photo_count() == 0 {
                    self.empty_state(ui);
                    return;
                }
                // Crop is a transient editor over the gallery, not a view: when
                // one is open it owns the central area regardless of `view`.
                if self.crop_edit.is_some() {
                    self.crop_central(ui);
                    return;
                }
                match self.view {
                    ViewMode::Grid => {
                        let view_width = ui.available_width();
                        let resp = grid::show(
                            ui,
                            &mut self.session,
                            &mut self.textures,
                            self.cell,
                            view_width,
                            std::mem::take(&mut self.scroll_to_focus),
                            &mut self.collapsed,
                            &mut self.grid_ctx,
                            false,
                        );
                        self.visible = resp.visible;
                        self.cols = resp.cols;
                        // Double-click opens the photo in the gallery, like Space.
                        if let Some(idx) = resp.double_clicked {
                            self.session.set_focus(idx, false);
                            self.enter_gallery();
                        }
                        // A context-menu pick rides the one dispatch path.
                        if let Some(action) = resp.action {
                            let ctx = ui.ctx().clone();
                            self.dispatch(action, &ctx);
                        }
                    }
                    ViewMode::Gallery => {
                        let count = self.session.photo_count();
                        let focus = self.session.focus().unwrap_or(0).min(count - 1);
                        // Recenter the filmstrip only when the focus just moved.
                        let state = gallery::GalleryState {
                            focus,
                            full_zoom: self.gallery_full,
                            strip_collapsed: self.strip_collapsed,
                            center_focus: std::mem::take(&mut self.scroll_to_focus),
                        };
                        let resp = gallery::show(
                            ui,
                            &mut self.session,
                            &mut self.gallery_textures,
                            &mut self.textures,
                            &state,
                        );
                        if let Some(idx) = resp.clicked {
                            self.session.set_focus(idx, false);
                            self.scroll_to_focus = true;
                        }
                        // A context-menu pick rides the one dispatch path.
                        if let Some(action) = resp.action {
                            let ctx = ui.ctx().clone();
                            self.dispatch(action, &ctx);
                            // Selecting a whole group is a grid affordance — drop
                            // back so the selection is actually visible.
                            if matches!(action, dcs_app::AppAction::SelectGroup(_)) {
                                self.exit_gallery();
                            }
                        }
                    }
                    ViewMode::Board => {
                        let resp = crate::board::show(
                            ui,
                            &mut self.session,
                            &mut self.textures,
                            &mut self.board_textures,
                            &mut self.board,
                            &mut self.collapsed,
                            &mut self.grid_ctx,
                            std::mem::take(&mut self.scroll_to_focus),
                        );
                        // The sidebar grid drives row-nav math, like the grid view.
                        self.cols = resp.cols;
                        if let Some(action) = resp.action {
                            let ctx = ui.ctx().clone();
                            self.dispatch(action, &ctx);
                        }
                    }
                }
            });
    }

    /// Paint the crop editor and act on its result. The working edit lives in
    /// `self.crop_edit`; a missing one (focus lost) drops straight back to the
    /// gallery.
    fn crop_central(&mut self, ui: &mut Ui) {
        let Some(mut state) = self.crop_edit.take() else {
            self.exit_crop();
            return;
        };
        let resp = crop::show(
            ui,
            &mut self.session,
            &mut self.gallery_textures,
            &mut self.textures,
            &mut state,
        );
        if resp.apply {
            self.session.set_crop(state.photo, Some(state.to_edit()));
            self.exit_crop();
            return;
        }
        if resp.cancel {
            self.exit_crop();
            return;
        }
        // A filmstrip jump commits the pending edit, then re-seeds on the new
        // photo so moving away never silently loses work.
        if let Some(idx) = resp.jump
            && idx != state.focus
            && let Some(photo) = self.session.photo_at(idx).map(|p| p.id)
        {
            self.session.set_crop(state.photo, Some(state.to_edit()));
            self.session.set_focus(idx, false);
            let committed = self.session.crop_of(photo);
            self.crop_edit = Some(crop::CropEditState::new(idx, photo, committed));
            return;
        }
        self.crop_edit = Some(state);
    }

    /// Open the gallery on the focused photo (else the first visible), starting
    /// contain-fit.
    pub(super) fn enter_gallery(&mut self) {
        if self.session.photo_count() == 0 {
            return;
        }
        if self.session.focus().is_none() {
            self.session.set_focus(0, false);
        }
        self.view = ViewMode::Gallery;
        self.gallery_full = false;
        // Center the filmstrip on the opening photo.
        self.scroll_to_focus = true;
    }

    /// Leave the gallery back to the grid, dropping its large frames and asking
    /// the grid to scroll the photo we were viewing into view.
    pub(super) fn exit_gallery(&mut self) {
        self.view = ViewMode::Grid;
        self.gallery_full = false;
        self.session.clear_gallery();
        self.gallery_textures.clear();
        self.scroll_to_focus = true;
    }

    /// Enter the board view. A no-op on an empty pool (the registry already
    /// gates it); leaves the gallery first if we were there.
    pub(super) fn enter_board(&mut self) {
        if self.session.photo_count() == 0 {
            return;
        }
        if self.view == ViewMode::Gallery {
            self.session.clear_gallery();
            self.gallery_textures.clear();
        }
        self.view = ViewMode::Board;
        self.board.end_drag();
    }

    /// Leave the board back to the grid. The placements persist in the session;
    /// only the ephemeral canvas state (pan/zoom, selection, drag) is dropped.
    pub(super) fn exit_board(&mut self) {
        self.view = ViewMode::Grid;
        self.board.end_drag();
        self.session.clear_board();
        self.board_textures.clear();
        self.scroll_to_focus = true;
    }

    /// Enter the crop editor on the focused photo. Seeds the working edit from
    /// the photo's committed crop (or identity) and clears the gallery cache so
    /// the editor's *uncropped* frame doesn't collide with a cropped one on the
    /// same id. No-op when nothing croppable is focused.
    pub(super) fn enter_crop(&mut self) {
        // Already editing — don't re-seed and clobber the working edit.
        if self.crop_edit.is_some() {
            return;
        }
        if !self.session.focused_is_croppable() {
            return;
        }
        let Some(focus) = self.session.focus() else {
            return;
        };
        let Some(photo) = self.session.photo_at(focus).map(|p| p.id) else {
            return;
        };
        let committed = self.session.crop_of(photo);
        self.session.clear_gallery();
        self.gallery_textures.clear();
        self.crop_edit = Some(crop::CropEditState::new(focus, photo, committed));
    }

    /// Leave the crop editor back to the gallery, dropping the working edit and
    /// the editor's uncropped frame so the gallery re-decodes with the crop. The
    /// underlying `view` is already `Gallery` (crop never changed it), so clearing
    /// `crop_edit` falls back there.
    pub(super) fn exit_crop(&mut self) {
        self.crop_edit = None;
        self.session.clear_gallery();
        self.gallery_textures.clear();
        self.view = ViewMode::Gallery;
        self.scroll_to_focus = true;
    }

    /// Shown while a folder is being scanned: the grid waits for the full count
    /// and grouping, so this reports progress instead of a reflowing grid.
    fn scanning_state(&mut self, ui: &mut Ui) {
        let found = self.session.pool_len();
        ui.vertical_centered(|ui| {
            ui.add_space((ui.available_height() * 0.5 - 20.0).max(0.0));
            ui.label(
                RichText::new("scanning…")
                    .monospace()
                    .strong()
                    .color(theme::TEXT_DIM),
            );
            ui.add_space(4.0);
            ui.label(
                RichText::new(format!("{found} found"))
                    .font(FontId::monospace(12.0))
                    .color(theme::HAIRLINE),
            );
        });
    }

    fn empty_state(&mut self, ui: &mut Ui) {
        // Reached only once scanning is done (central gates that). Three ways to
        // be here: no folder open, a filter hid everything, or the pool holds
        // only RAW-only photos that v1 can't display (no filter to blame).
        let no_folder = self.session.pool_len() == 0;
        let filtered_out = !no_folder && self.session.is_filtered();
        // A filter hides everything *because the search is still resolving* — show
        // progress, not a false "no matches".
        let searching = filtered_out && self.session.is_search_pending();
        let mut open_clicked = false;
        let mut clear_clicked = false;
        // A top spacer of ~half the leftover height centers the fixed-size block.
        ui.vertical_centered(|ui| {
            let avail = ui.available_height();
            let pad = |ui: &mut Ui, content_h: f32| {
                ui.add_space(((avail - content_h) * 0.5).max(0.0));
            };
            if no_folder {
                pad(ui, 120.0);
                ui.label(RichText::new("dcs").monospace().strong().size(22.0));
                ui.label(
                    RichText::new("digital contact sheet")
                        .monospace()
                        .color(theme::TEXT_DIM),
                );
                ui.add_space(16.0);
                if ui
                    .button(RichText::new("Open folder…").monospace().size(14.0))
                    .clicked()
                {
                    open_clicked = true;
                }
                ui.add_space(4.0);
                ui.label(
                    RichText::new("or drag a folder in")
                        .monospace()
                        .size(11.0)
                        .color(theme::HAIRLINE),
                );
            } else if searching {
                // A search chip is active but its results aren't in yet (model
                // loading/indexing, or the query is still embedding).
                pad(ui, 40.0);
                ui.add(egui::Spinner::new());
                ui.add_space(8.0);
                ui.label(
                    RichText::new("searching…")
                        .monospace()
                        .color(theme::TEXT_DIM),
                );
            } else if filtered_out {
                // Pool has photos; the active filter hides them all.
                pad(ui, 60.0);
                ui.label(
                    RichText::new("no photos match the filter")
                        .monospace()
                        .color(theme::TEXT_DIM),
                );
                ui.add_space(8.0);
                if ui
                    .button(RichText::new("Clear filters").monospace().size(13.0))
                    .clicked()
                {
                    clear_clicked = true;
                }
            } else {
                // Pool holds only RAW-only photos — nothing to draw in v1, and no
                // filter to clear. Say so honestly instead of blaming a filter.
                pad(ui, 40.0);
                ui.label(
                    RichText::new("no displayable photos — this folder has only RAW files")
                        .monospace()
                        .color(theme::TEXT_DIM),
                );
            }
        });
        if open_clicked {
            self.open_folder_dialog();
        }
        if clear_clicked {
            let ctx = ui.ctx().clone();
            self.dispatch(AppAction::ClearFilters, &ctx);
        }
    }
}
