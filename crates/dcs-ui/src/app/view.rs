use dcs_app::VerdictFilter;
use egui::{Align, FontId, Layout, RichText, Ui};

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
        let text = format!(
            "{} shown · {} sel · acc {} · rej {} · unrev {}{}{}",
            self.session.photo_count(),
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
        // Reached only once scanning is done (central gates that), so the empty
        // grid is either no folder open or the filter hid everything.
        let no_folder = self.session.pool_len() == 0;
        let mut open_clicked = false;
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
            } else {
                // Pool has photos; the active verdict filter hides them all.
                pad(ui, 48.0);
                ui.label(
                    RichText::new(format!("no {} photos", filter_word(self.session.filter())))
                        .monospace()
                        .color(theme::TEXT_DIM),
                );
                ui.label(
                    RichText::new("view: all to show everything")
                        .monospace()
                        .size(11.0)
                        .color(theme::HAIRLINE),
                );
            }
        });
        if open_clicked {
            self.open_folder_dialog();
        }
    }
}

/// The verdict-filter word for the empty-view message (`All` never empties).
fn filter_word(filter: VerdictFilter) -> &'static str {
    match filter {
        VerdictFilter::All => "",
        VerdictFilter::Unreviewed => "unreviewed",
        VerdictFilter::Accepted => "accepted",
        VerdictFilter::Rejected => "rejected",
    }
}
