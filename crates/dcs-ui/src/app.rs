//! The eframe application: top bar (open folder, view-mode indicator),
//! central grid, status bar, and a toggleable diagnostics overlay (§10b).
//! Ephemeral UI state — zoom, the GPU texture cache, debug flags — lives here
//! and never travels down (§9).

use std::time::Duration;

use dcs_app::{Session, VerdictFilter};
use egui::{Align, Align2, FontId, Key, Layout, RichText, Ui};

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
    /// Grid geometry from the last painted frame, used by keyboard nav before
    /// the grid is laid out this frame (row math + auto-scroll).
    cols: usize,
    scroll_y: f32,
    viewport_h: f32,
    /// When set, forces the grid's scroll offset for one frame to keep the
    /// focus cursor visible after a nav move.
    pending_scroll: Option<f32>,
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
            cols: 1,
            scroll_y: 0.0,
            viewport_h: 0.0,
            pending_scroll: None,
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

                ui.separator();
                ui.label(RichText::new("view").monospace().color(theme::TEXT_DIM));
                self.filter_chip(ui, "all", VerdictFilter::All);
                self.filter_chip(ui, "unrev", VerdictFilter::Unreviewed);
                self.filter_chip(ui, "acc", VerdictFilter::Accepted);
                self.filter_chip(ui, "rej", VerdictFilter::Rejected);

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

    fn filter_chip(&mut self, ui: &mut Ui, label: &str, filter: VerdictFilter) {
        let on = self.session.filter() == filter;
        if ui
            .selectable_label(on, RichText::new(label).monospace())
            .clicked()
        {
            self.session.set_filter(filter);
        }
    }

    fn status_bar(&mut self, ui: &mut Ui) {
        let scanning = if self.session.is_scanning() {
            " · scanning…"
        } else {
            ""
        };
        let (acc, rej, unrev) = self.session.verdict_counts();
        // Verdicts are in-memory only this phase — persistence is the next slice
        // (§5). Say so rather than silently losing state on close.
        let text = format!(
            "{} shown · {} sel · acc {} · rej {} · unrev {} · {} loaded{} · verdicts not saved yet",
            self.session.photo_count(),
            self.session.selection_count(),
            acc,
            rej,
            unrev,
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
                let resp = grid::show(
                    ui,
                    &mut self.session,
                    &mut self.textures,
                    self.cell,
                    view_width,
                    self.pending_scroll.take(),
                );
                self.visible = resp.visible;
                self.cols = resp.cols;
                self.scroll_y = resp.scroll_y;
                self.viewport_h = resp.viewport_h;
            });
    }

    fn empty_state(&self, ui: &mut Ui) {
        // `photo_count` is post-filter, so an empty grid means one of three
        // things — distinguish them instead of always inviting "Open folder".
        let scanning = self.session.is_scanning();
        let no_folder = self.session.pool_len() == 0;
        ui.centered_and_justified(|ui| {
            if scanning && no_folder {
                ui.label(RichText::new("scanning…").monospace().color(theme::TEXT_DIM));
            } else if no_folder {
                ui.label(RichText::new("Open folder…").monospace().color(theme::TEXT_DIM));
            } else {
                // Pool has photos; the active verdict filter hides them all.
                ui.vertical_centered(|ui| {
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
                });
            }
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
        for action in resolve_bindings(ctx) {
            self.apply(action);
        }
    }

    fn apply(&mut self, action: Action) {
        match action {
            Action::Nav { dx, dy, extend } => {
                self.session.nav(dx, dy, self.cols, extend);
                self.ensure_focus_visible();
            }
            Action::Accept => self.session.accept(),
            Action::Reject => self.session.reject(),
            Action::SelectAllVisible => self.session.select_all_visible(),
            Action::ClearSelection => self.session.clear_selection(),
            Action::Undo => {
                self.session.undo();
            }
            Action::Redo => {
                self.session.redo();
            }
            Action::ZoomIn => self.zoom(ZOOM_STEP),
            Action::ZoomOut => self.zoom(1.0 / ZOOM_STEP),
            Action::ToggleDebug => self.debug = !self.debug,
        }
    }

    /// Minimal scroll offset to bring the focus cell fully into view, applied
    /// next frame via `pending_scroll`. No-op when already visible.
    fn ensure_focus_visible(&mut self) {
        let Some(focus) = self.session.focus() else {
            return;
        };
        let cols = self.cols.max(1);
        let stride = grid::row_stride(self.cell);
        let row = focus / cols;
        let row_top = row as f32 * stride;
        let row_bot = row_top + stride;
        let view_top = self.scroll_y;
        let view_bot = self.scroll_y + self.viewport_h;
        let target = if row_top < view_top {
            Some(row_top)
        } else if row_bot > view_bot {
            Some(row_bot - self.viewport_h)
        } else {
            None
        };
        self.pending_scroll = target.map(|t| t.max(0.0));
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

/// The verdict-filter word for the empty-view message (`All` never empties).
fn filter_word(filter: VerdictFilter) -> &'static str {
    match filter {
        VerdictFilter::All => "",
        VerdictFilter::Unreviewed => "unreviewed",
        VerdictFilter::Accepted => "accepted",
        VerdictFilter::Rejected => "rejected",
    }
}

/// A resolved keyboard intent. Decoupled from raw keys so the binding layer can
/// become user-configurable later (spec: remappable keys) without touching the
/// handlers.
enum Action {
    Nav { dx: isize, dy: isize, extend: bool },
    Accept,
    Reject,
    SelectAllVisible,
    ClearSelection,
    Undo,
    Redo,
    ZoomIn,
    ZoomOut,
    ToggleDebug,
}

/// The keyboard bindings. The single place raw keys map to `Action`s — a future
/// config-driven keymap replaces just this function.
fn resolve_bindings(ctx: &egui::Context) -> Vec<Action> {
    ctx.input(|i| {
        let cmd = i.modifiers.command;
        let shift = i.modifiers.shift;
        let mut actions = Vec::new();
        if i.key_pressed(Key::ArrowLeft) {
            actions.push(Action::Nav { dx: -1, dy: 0, extend: shift });
        }
        if i.key_pressed(Key::ArrowRight) {
            actions.push(Action::Nav { dx: 1, dy: 0, extend: shift });
        }
        if i.key_pressed(Key::ArrowUp) {
            actions.push(Action::Nav { dx: 0, dy: -1, extend: shift });
        }
        if i.key_pressed(Key::ArrowDown) {
            actions.push(Action::Nav { dx: 0, dy: 1, extend: shift });
        }
        if i.key_pressed(Key::A) {
            actions.push(if cmd { Action::SelectAllVisible } else { Action::Accept });
        }
        if i.key_pressed(Key::X) && !cmd {
            actions.push(Action::Reject);
        }
        if i.key_pressed(Key::Z) && cmd {
            actions.push(if shift { Action::Redo } else { Action::Undo });
        }
        if i.key_pressed(Key::Escape) {
            actions.push(Action::ClearSelection);
        }
        if i.key_pressed(Key::Plus) || i.key_pressed(Key::Equals) {
            actions.push(Action::ZoomIn);
        }
        if i.key_pressed(Key::Minus) {
            actions.push(Action::ZoomOut);
        }
        if i.key_pressed(Key::F12) {
            actions.push(Action::ToggleDebug);
        }
        actions
    })
}
