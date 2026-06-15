//! The eframe application: top bar (open folder, view-mode indicator),
//! central grid, status bar, and a toggleable diagnostics overlay (§10b).
//! Ephemeral UI state — zoom, the GPU texture cache, debug flags — lives here
//! and never travels down (§9).

use std::time::Duration;

use dcs_app::Session;
use egui::{Align, Align2, FontId, Layout, RichText, Ui};

use crate::grid;
use crate::grid::TextureCache;
use crate::theme;

const CELL_MIN: f32 = 80.0;
const CELL_MAX: f32 = 400.0;
const ZOOM_STEP: f32 = 1.15;

pub struct DcsApp {
    session: Session,
    textures: TextureCache,
    cell: f32,
    debug: bool,
    fps: f32,
    visible: usize,
}

impl DcsApp {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        theme::apply(&cc.egui_ctx);
        DcsApp {
            session: Session::new(),
            textures: TextureCache::new(),
            cell: 160.0,
            debug: false,
            fps: 0.0,
            visible: 0,
        }
    }
}

impl eframe::App for DcsApp {
    fn ui(&mut self, ui: &mut Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.session.tick();
        self.handle_keys(&ctx);
        self.track_fps(&ctx);

        self.top_bar(ui);
        self.status_bar(ui);
        self.central(ui);
        if self.debug {
            self.diagnostics(&ctx);
        }

        // While decodes are streaming in, poll at ~30 fps rather than spinning
        // a core at full framerate — new thumbnails still appear within a frame
        // or two. Active scrolling repaints at 60 fps regardless, because input
        // drives its own repaints. Fully idle = no repaint at all (§3).
        if self.session.is_scanning() || self.session.has_pending() {
            ctx.request_repaint_after(Duration::from_millis(33));
        } else if self.debug {
            ctx.request_repaint_after(Duration::from_millis(250));
        }
    }
}

impl DcsApp {
    fn top_bar(&mut self, ui: &mut Ui) {
        egui::Panel::top("top").show_inside(ui, |ui| {
            ui.horizontal(|ui| {
                if ui.button("Open folder…").clicked()
                    && let Some(dir) = rfd::FileDialog::new().pick_folder()
                {
                    self.textures.clear();
                    self.session.open_folder(dir);
                }
                ui.separator();
                ui.label(RichText::new("GRID").monospace().strong());
                ui.label(RichText::new("gallery").monospace().color(theme::TEXT_DIM));

                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if ui
                        .selectable_label(self.debug, RichText::new("dbg").monospace())
                        .clicked()
                    {
                        self.debug = !self.debug;
                    }
                    ui.separator();
                    if ui.button("+").clicked() {
                        self.zoom(ZOOM_STEP);
                    }
                    if ui.button("−").clicked() {
                        self.zoom(1.0 / ZOOM_STEP);
                    }
                    ui.label(RichText::new("zoom").monospace().color(theme::TEXT_DIM));
                });
            });
        });
    }

    fn status_bar(&mut self, ui: &mut Ui) {
        let scanning = if self.session.is_scanning() {
            " · scanning…"
        } else {
            ""
        };
        let text = format!(
            "{} photos · {} loaded{}",
            self.session.photo_count(),
            self.session.loaded_count(),
            scanning
        );
        egui::Panel::bottom("status").show_inside(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(RichText::new(text).font(FontId::monospace(12.0)));
            });
        });
    }

    fn central(&mut self, ui: &mut Ui) {
        egui::CentralPanel::default()
            .frame(egui::Frame::default().fill(theme::SHEET_BG))
            .show_inside(ui, |ui| {
                if self.session.photo_count() == 0 {
                    self.empty_state(ui);
                    return;
                }
                let view_width = ui.available_width();
                self.visible = grid::show(
                    ui,
                    &mut self.session,
                    &mut self.textures,
                    self.cell,
                    view_width,
                );
            });
    }

    fn empty_state(&self, ui: &mut Ui) {
        ui.centered_and_justified(|ui| {
            let hint = if self.session.is_scanning() {
                "scanning…"
            } else {
                "Open folder…"
            };
            ui.label(RichText::new(hint).monospace().color(theme::TEXT_DIM));
        });
    }

    fn diagnostics(&self, ctx: &egui::Context) {
        egui::Window::new("diagnostics")
            .anchor(Align2::RIGHT_TOP, [-8.0, 8.0])
            .collapsible(false)
            .resizable(false)
            .title_bar(false)
            .show(ctx, |ui| {
                let lines = [
                    format!("fps     {:>6.1}", self.fps),
                    format!("frame   {:>5.1} ms", frame_ms(self.fps)),
                    format!("photos  {:>6}", self.session.photo_count()),
                    format!("loaded  {:>6}", self.session.loaded_count()),
                    format!("hires   {:>6}", self.session.hires_count()),
                    format!("queue   {:>6}", self.session.decode_queue_depth()),
                    format!("texs    {:>6}", self.textures.len()),
                    format!("visible {:>6}", self.visible),
                    format!("cell    {:>6.0}", self.cell),
                    format!("pix mem ~{:>4.0} MB", self.session.thumb_memory_mb()),
                ];
                for line in lines {
                    ui.label(RichText::new(line).font(FontId::monospace(12.0)));
                }
            });
    }

    fn handle_keys(&mut self, ctx: &egui::Context) {
        let (zoom_in, zoom_out, toggle_debug) = ctx.input(|i| {
            (
                i.key_pressed(egui::Key::Plus) || i.key_pressed(egui::Key::Equals),
                i.key_pressed(egui::Key::Minus),
                i.key_pressed(egui::Key::F12),
            )
        });
        if zoom_in {
            self.zoom(ZOOM_STEP);
        }
        if zoom_out {
            self.zoom(1.0 / ZOOM_STEP);
        }
        if toggle_debug {
            self.debug = !self.debug;
        }
    }

    fn track_fps(&mut self, ctx: &egui::Context) {
        let dt = ctx.input(|i| i.stable_dt);
        if dt > 0.0 {
            let instant = 1.0 / dt;
            self.fps = if self.fps == 0.0 {
                instant
            } else {
                self.fps * 0.9 + instant * 0.1
            };
        }
    }

    fn zoom(&mut self, factor: f32) {
        self.cell = (self.cell * factor).clamp(CELL_MIN, CELL_MAX);
    }
}

fn frame_ms(fps: f32) -> f32 {
    if fps > 0.0 { 1000.0 / fps } else { 0.0 }
}
