use dcs_app::AppAction;
use dcs_domain::cull::AcceptState;
use dcs_domain::filter::{ChipOp, FilterChip};
use egui::{Align, Color32, FontId, Layout, RichText, Sense, Stroke, Ui, Vec2};

use super::{DcsApp, PALETTE_MOD, ViewMode};
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
                }
            });
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
