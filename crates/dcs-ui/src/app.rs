//! The eframe application: top bar (open folder, view-mode indicator),
//! central grid, status bar, and a toggleable diagnostics overlay (§10b).
//! Ephemeral UI state — zoom, the GPU texture cache, debug flags — lives here
//! and never travels down (§9).

use std::path::PathBuf;
use std::time::{Duration, Instant};

use dcs_app::{Session, VerdictFilter};
use egui::{Align, Align2, FontId, Key, Layout, RichText, Ui};

use crate::grid;
use crate::grid::TextureCache;
use crate::theme;

const CELL_MIN: f32 = 80.0;
const CELL_MAX: f32 = 400.0;
const ZOOM_STEP: f32 = 1.15;

/// Idle gap after the last verdict change before the project auto-saves (§10b).
/// Long enough that rapid A/X culling coalesces into one write, short enough
/// that a crash loses at most a couple seconds of verdicts.
const SAVE_DEBOUNCE: Duration = Duration::from_millis(1500);

/// How often the live instance refreshes its lock timestamp so peers keep
/// seeing it as alive. Well under the stale window (#34).
const LOCK_HEARTBEAT: Duration = Duration::from_secs(60);

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
    /// When the project first became dirty since the last save — drives the
    /// debounced autosave. `None` while clean.
    dirty_since: Option<Instant>,
    /// About window visibility.
    show_about: bool,
    /// Shoot-timezone picker visibility + its search query.
    show_zone_picker: bool,
    zone_query: String,
    /// Last time we refreshed the project lock; throttles the heartbeat (#34).
    last_heartbeat: Option<Instant>,
}

impl DcsApp {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        theme::apply(&cc.egui_ctx);
        let mut session = Session::new();
        // Real app: remember recently-opened projects (~/.dcs/recents.json).
        session.enable_default_recents();
        DcsApp {
            session,
            textures: TextureCache::new(),
            cell: 160.0,
            debug: false,
            fps: 0.0,
            visible: 0,
            cols: 1,
            scroll_y: 0.0,
            viewport_h: 0.0,
            pending_scroll: None,
            dirty_since: None,
            show_about: false,
            show_zone_picker: false,
            zone_query: String::new(),
            last_heartbeat: None,
        }
    }
}

impl eframe::App for DcsApp {
    fn ui(&mut self, ui: &mut Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.session.tick();
        self.handle_keys(&ctx);
        self.track_fps(&ctx);

        self.menu_bar(ui, &ctx);
        self.top_bar(ui);
        self.read_only_banner(ui);
        self.status_bar(ui);
        self.central(ui);
        self.autosave(&ctx);
        self.heartbeat(&ctx);
        self.about_window(&ctx);
        self.zone_picker(&ctx);
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

    /// Final save on quit — the durable backstop behind the debounced autosave.
    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        self.save_now();
    }
}

impl DcsApp {
    fn top_bar(&mut self, ui: &mut Ui) {
        egui::Panel::top("top").show_inside(ui, |ui| {
            ui.horizontal(|ui| {
                if ui.button("Open folder…").clicked() {
                    self.open_folder_dialog();
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

                ui.separator();
                let zone = self.session.shoot_zone().unwrap_or("set zone").to_string();
                if ui
                    .button(RichText::new(format!("tz: {zone}")).monospace())
                    .on_hover_text("Shoot timezone (freeze-critical)")
                    .clicked()
                {
                    self.show_zone_picker = true;
                }

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

    fn menu_bar(&mut self, ui: &mut Ui, ctx: &egui::Context) {
        egui::Panel::top("menu").show_inside(ui, |ui| {
            egui::MenuBar::new().ui(ui, |ui| {
                ui.menu_button("File", |ui| {
                    if ui.button("Open Project…").clicked() {
                        self.open_folder_dialog();
                        ui.close();
                    }
                    ui.menu_button("Open Recent", |ui| {
                        let recents = self.session.recent_projects().to_vec();
                        if recents.is_empty() {
                            ui.add_enabled(false, egui::Button::new("(none)"));
                            return;
                        }
                        for path in &recents {
                            let label = path
                                .file_name()
                                .map(|n| n.to_string_lossy().into_owned())
                                .unwrap_or_else(|| path.to_string_lossy().into_owned());
                            if ui
                                .button(label)
                                .on_hover_text(path.to_string_lossy())
                                .clicked()
                            {
                                self.open_path(path.clone());
                                ui.close();
                            }
                        }
                        ui.separator();
                        if ui.button("Clear Recents").clicked() {
                            self.session.clear_recents();
                            ui.close();
                        }
                    });
                    ui.separator();
                    let can_rescan = self.session.has_folder();
                    if ui
                        .add_enabled(can_rescan, egui::Button::new("Rescan Folder"))
                        .clicked()
                    {
                        self.rescan();
                        ui.close();
                    }
                    let missing = self.session.missing_count();
                    if ui
                        .add_enabled(
                            missing > 0,
                            egui::Button::new(format!("Remove Missing ({missing})")),
                        )
                        .on_hover_text("Forget photos whose files are gone for good")
                        .clicked()
                    {
                        self.session.forget_missing();
                        self.textures.clear();
                        ui.close();
                    }
                    ui.separator();
                    if ui.button("Quit").clicked() {
                        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                    }
                });
                ui.menu_button("Help", |ui| {
                    if ui.button("About dcs").clicked() {
                        self.show_about = true;
                        ui.close();
                    }
                });
            });
        });
    }

    /// Pick a folder and open it, persisting the current project first.
    fn open_folder_dialog(&mut self) {
        if let Some(dir) = rfd::FileDialog::new().pick_folder() {
            self.open_path(dir);
        }
    }

    /// Open a specific folder: save the current project, swap, and restore the
    /// folder's persisted grid zoom.
    fn open_path(&mut self, dir: PathBuf) {
        self.save_now();
        self.textures.clear();
        self.session.open_folder(dir);
        if let Some(zoom) = self.session.grid_zoom() {
            self.cell = zoom.clamp(CELL_MIN, CELL_MAX);
        }
    }

    /// Re-scan the open folder (new/returned/removed files reconcile, §4).
    fn rescan(&mut self) {
        self.textures.clear();
        self.session.rescan();
    }

    /// A banner offering "Take over" when another instance holds the lock (#34).
    fn read_only_banner(&mut self, ui: &mut Ui) {
        if !self.session.is_read_only() {
            return;
        }
        egui::Panel::top("readonly").show_inside(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(
                    RichText::new("read-only — another instance has this project open")
                        .monospace()
                        .color(theme::VERDICT_REJECT),
                );
                if ui.button("Take over").clicked() {
                    self.session.take_over();
                }
            });
        });
    }

    fn about_window(&mut self, ctx: &egui::Context) {
        if !self.show_about {
            return;
        }
        let mut open = true;
        egui::Window::new("About dcs")
            .collapsible(false)
            .resizable(false)
            .anchor(Align2::CENTER_CENTER, [0.0, 0.0])
            .open(&mut open)
            .show(ctx, |ui| {
                ui.label(RichText::new("dcs").monospace().strong().size(18.0));
                ui.label(
                    RichText::new(concat!("digital contact sheet · v", env!("CARGO_PKG_VERSION")))
                        .monospace()
                        .color(theme::TEXT_DIM),
                );
                ui.add_space(6.0);
                ui.label(
                    RichText::new("Fast, keyboard-first photo culling.\nScan · cull · tag · export.")
                        .monospace()
                        .size(12.0),
                );
            });
        self.show_about = open;
    }

    /// Searchable IANA timezone picker (open Q#5). Type to filter; click to set
    /// the project's shoot zone (persisted in config).
    fn zone_picker(&mut self, ctx: &egui::Context) {
        if !self.show_zone_picker {
            return;
        }
        let mut open = true;
        egui::Window::new("Shoot timezone")
            .collapsible(false)
            .resizable(true)
            .default_size([320.0, 440.0])
            .anchor(Align2::CENTER_CENTER, [0.0, 0.0])
            .open(&mut open)
            .show(ctx, |ui| {
                ui.label(
                    RichText::new("freeze-critical — used for time grouping & crystallization")
                        .monospace()
                        .size(11.0)
                        .color(theme::TEXT_DIM),
                );
                ui.horizontal(|ui| {
                    let current = self.session.shoot_zone().unwrap_or("none").to_string();
                    ui.label(RichText::new(format!("current: {current}")).monospace());
                    if self.session.shoot_zone().is_some() && ui.button("clear").clicked() {
                        self.session.set_shoot_zone(None);
                    }
                });
                ui.add(
                    egui::TextEdit::singleline(&mut self.zone_query)
                        .hint_text("search… e.g. tokyo")
                        .desired_width(f32::INFINITY),
                );
                ui.separator();

                let query = self.zone_query.to_lowercase();
                let current = self.session.shoot_zone().map(str::to_string);
                let mut picked: Option<String> = None;
                egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
                    for name in dcs_domain::timezone::zone_names() {
                        if !query.is_empty() && !name.to_lowercase().contains(&query) {
                            continue;
                        }
                        let selected = current.as_deref() == Some(name);
                        if ui
                            .selectable_label(selected, RichText::new(name).monospace())
                            .clicked()
                        {
                            picked = Some(name.to_string());
                        }
                    }
                });
                if let Some(name) = picked {
                    self.session.set_shoot_zone(Some(name));
                }
            });
        self.show_zone_picker = open;
    }

    /// Refresh the project lock on a throttled heartbeat so peers see us as live
    /// while a folder is open and we own it (#34).
    fn heartbeat(&mut self, ctx: &egui::Context) {
        if !self.session.has_folder() || self.session.is_read_only() {
            return;
        }
        let due = self
            .last_heartbeat
            .is_none_or(|t| t.elapsed() >= LOCK_HEARTBEAT);
        if due {
            self.session.refresh_lock();
            self.last_heartbeat = Some(Instant::now());
        }
        // Keep waking so the heartbeat fires even when the user is idle.
        ctx.request_repaint_after(LOCK_HEARTBEAT);
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
        let save_state = if self.session.is_dirty() {
            " · unsaved"
        } else {
            " · saved"
        };
        let text = format!(
            "{} shown · {} sel · acc {} · rej {} · unrev {} · {} loaded{}{}",
            self.session.photo_count(),
            self.session.selection_count(),
            acc,
            rej,
            unrev,
            self.session.loaded_count(),
            scanning,
            save_state
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
        // A modal is up: Esc closes it and grid shortcuts stay inert (so the
        // contact sheet doesn't react behind the dialog).
        if self.show_about || self.show_zone_picker {
            if ctx.input(|i| i.key_pressed(Key::Escape)) {
                self.show_about = false;
                self.show_zone_picker = false;
            }
            return;
        }
        // A focused text field (e.g. the zone search) owns the keyboard, so
        // editing shortcuts like Cmd+A select text instead of leaking through
        // to the grid.
        if ctx.egui_wants_keyboard_input() {
            return;
        }
        for action in resolve_bindings(ctx) {
            self.apply(action, ctx);
        }
    }

    fn apply(&mut self, action: Action, ctx: &egui::Context) {
        match action {
            Action::OpenProject => self.open_folder_dialog(),
            Action::Quit => ctx.send_viewport_cmd(egui::ViewportCommand::Close),
            Action::Nav { dx, dy, extend } => {
                self.session.nav(dx, dy, self.cols, extend);
                self.ensure_focus_visible();
            }
            Action::Accept => self.session.accept(),
            Action::Reject => self.session.reject(),
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
        // Persist the Grid view's zoom so reopening the folder restores it (§4).
        self.session.set_grid_zoom(self.cell);
    }

    /// Debounced autosave: once verdicts go dirty, save after a quiet gap so a
    /// burst of A/X keystrokes collapses into a single `project.json` write.
    fn autosave(&mut self, ctx: &egui::Context) {
        if self.session.is_dirty() {
            let since = *self.dirty_since.get_or_insert_with(Instant::now);
            if since.elapsed() >= SAVE_DEBOUNCE {
                self.save_now();
            } else {
                ctx.request_repaint_after(SAVE_DEBOUNCE);
            }
        } else {
            self.dirty_since = None;
        }
    }

    /// Save now if dirty, surfacing any failure to stderr. The cache and undo
    /// log are rebuildable; only a `project.json` write can fail loudly here.
    fn save_now(&mut self) {
        if let Err(e) = self.session.save_if_dirty() {
            eprintln!("dcs: save failed: {e}");
        }
        self.dirty_since = None;
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
    ClearSelection,
    Undo,
    Redo,
    ZoomIn,
    ZoomOut,
    ToggleDebug,
    OpenProject,
    Quit,
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
        if i.key_pressed(Key::A) && !cmd {
            actions.push(Action::Accept);
        }
        if i.key_pressed(Key::X) && !cmd {
            actions.push(Action::Reject);
        }
        if i.key_pressed(Key::Z) && cmd {
            actions.push(if shift { Action::Redo } else { Action::Undo });
        }
        if i.key_pressed(Key::O) && cmd {
            actions.push(Action::OpenProject);
        }
        if i.key_pressed(Key::Q) && cmd {
            actions.push(Action::Quit);
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
