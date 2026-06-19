//! The eframe application: top bar (open folder, view-mode indicator),
//! central grid, status bar, and a toggleable diagnostics overlay.
//! Ephemeral UI state — zoom, the GPU texture cache, debug flags — lives here
//! and never travels down.

mod dialogs;
mod input;
mod menus;
mod view;

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::time::{Duration, Instant};

use dcs_app::Session;
use dcs_domain::grouping::GroupKind;
use egui::Ui;

use crate::context_menu::MenuTarget;
use crate::export::ExportDialog;
use crate::grid::TextureCache;
use crate::picker::Picker;
use crate::theme;

/// First row of the timezone picker — picking it clears the shoot zone back to
/// the system default (so "no zone" stays a keyboard-reachable choice).
const CLEAR_ZONE_ROW: &str = "(clear — use system default zone)";

const CELL_MIN: f32 = 80.0;
const CELL_MAX: f32 = 400.0;
const ZOOM_STEP: f32 = 1.15;

/// VRAM budget for the gallery's texture cache. Enough for one full-resolution
/// 1:1 frame plus a few fit-sized neighbours, far below the grid's budget.
const GALLERY_TEXTURE_BYTES: u64 = 256_000_000;

/// Primary-modifier glyph for key hints — ⌘ on macOS, `Ctrl+` elsewhere.
#[cfg(target_os = "macos")]
const PALETTE_MOD: &str = "⌘";
#[cfg(not(target_os = "macos"))]
const PALETTE_MOD: &str = "Ctrl+";

/// Idle gap after the last verdict change before the project auto-saves.
/// Long enough that rapid A/X culling coalesces into one write, short enough
/// that a crash loses at most a couple seconds of verdicts.
const SAVE_DEBOUNCE: Duration = Duration::from_millis(1500);

/// How often the live instance refreshes its lock timestamp so peers keep
/// seeing it as alive. Well under the stale window.
const LOCK_HEARTBEAT: Duration = Duration::from_secs(60);

/// Which view the central area is in. Ephemeral UI state; gallery opens
/// on the focused photo and arrows traverse the same visible order as the grid.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ViewMode {
    Grid,
    Gallery,
}

pub struct DcsApp {
    session: Session,
    textures: TextureCache,
    /// Which view mode the central area shows.
    view: ViewMode,
    /// Texture cache for the gallery's large frames, kept apart from the grid's
    /// thumb cache so one `PhotoId` never holds both a big frame and a thumb.
    gallery_textures: TextureCache,
    /// Gallery zoom: `false` = contain-fit, `true` = 1:1 (`Z` toggles).
    gallery_full: bool,
    /// Filmstrip dock collapsed (ephemeral UI).
    strip_collapsed: bool,
    cell: f32,
    debug: bool,
    fps: f32,
    visible: usize,
    /// Column count from the last painted frame — keyboard `↑↓` row math runs
    /// before the grid is laid out this frame.
    cols: usize,
    /// Set when a keyboard nav move happened, so the grid scrolls the focus cell
    /// into view next paint. Cleared once consumed.
    scroll_to_focus: bool,
    /// When the project first became dirty since the last save — drives the
    /// debounced autosave. `None` while clean.
    dirty_since: Option<Instant>,
    /// About window visibility.
    show_about: bool,
    /// Photo metadata window visibility (`I`); reads the focused photo each frame.
    show_metadata: bool,
    /// Shoot-timezone (display) picker — a keyboard-first fuzzy quick-pick (the
    /// same component the command palette and tag palette will use).
    zone_picker: Picker,
    /// Camera-timezone picker — the zone the camera clock was set to.
    camera_zone_picker: Picker,
    /// The `Cmd/Ctrl+P` command palette over the whole registry.
    palette: Picker,
    /// The `T` tag palette: fuzzy over existing tags, with an explicit
    /// create-new row, acting on the selection.
    tag_palette: Picker,
    /// Whether the tag palette is in remove mode (`Shift+T`) vs add mode (`T`).
    tag_palette_remove: bool,
    /// The palette-path filter picker (`Filter: State…` / `Filter: Tag…`),
    /// toggling one chip at a time.
    filter_palette: Picker,
    /// Which dimension the filter picker is editing: `true` = verdict state,
    /// `false` = tags.
    filter_palette_state: bool,
    /// Tag manager window visibility.
    show_tag_manager: bool,
    /// Per-tag name edit buffers while the manager is open (the `TextEdit`s need
    /// stable backing strings across frames); cleared on close.
    tag_edits: HashMap<dcs_app::TagId, String>,
    /// Collapsed group titles (ephemeral UI state). Keyed by header title.
    collapsed: HashSet<String>,
    /// What the last right-click in the grid landed on, kept so the context menu
    /// shows the right items while it stays open across frames.
    grid_ctx: Option<MenuTarget>,
    /// Export dialog state; persisted across opens.
    export: ExportDialog,
    /// Last time we refreshed the project lock; throttles the heartbeat.
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
            view: ViewMode::Grid,
            // The gallery holds only the current frame and its neighbours, but
            // each is large — a dedicated, smaller VRAM budget keeps the two
            // caches from together pinning ~1.5 GB.
            gallery_textures: TextureCache::with_budget(GALLERY_TEXTURE_BYTES),
            gallery_full: false,
            strip_collapsed: false,
            cell: 92.0,
            debug: false,
            fps: 0.0,
            visible: 0,
            cols: 1,
            scroll_to_focus: false,
            dirty_since: None,
            show_about: false,
            show_metadata: false,
            zone_picker: Picker::new("Travel timezone"),
            camera_zone_picker: Picker::new("Camera timezone"),
            palette: Picker::new("Command Palette"),
            tag_palette: Picker::new("Tags"),
            tag_palette_remove: false,
            filter_palette: Picker::new("Filter"),
            filter_palette_state: false,
            show_tag_manager: false,
            tag_edits: HashMap::new(),
            collapsed: HashSet::new(),
            grid_ctx: None,
            export: ExportDialog::default(),
            last_heartbeat: None,
        }
    }
}

impl eframe::App for DcsApp {
    fn ui(&mut self, ui: &mut Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.session.tick();
        self.handle_keys(&ctx);
        self.handle_dropped_folder(&ctx);
        self.track_fps(&ctx);

        self.menu_bar(ui, &ctx);
        // Toolbar and status bar only exist once a project is open.
        if self.session.has_folder() {
            self.top_bar(ui, &ctx);
            self.filter_bar(ui, &ctx);
            self.read_only_banner(ui);
            self.status_bar(ui);
        }
        self.central(ui);
        // Keep prefetching the whole folder's thumbnails in the background,
        // regardless of view mode, at low decode priority.
        self.session.fill_base_background();
        self.autosave(&ctx);
        self.heartbeat(&ctx);
        self.about_window(&ctx);
        self.metadata_window(&ctx);
        self.export_dialog(&ctx);
        self.zone_picker(&ctx);
        self.camera_zone_picker(&ctx);
        self.command_palette(&ctx);
        self.tag_palette(&ctx);
        self.filter_palette(&ctx);
        self.tag_manager(&ctx);
        if self.debug {
            self.diagnostics(&ctx);
        }

        // While decodes are streaming in, poll at ~30 fps rather than spinning
        // a core at full framerate — new thumbnails still appear within a frame
        // or two. Active scrolling repaints at 60 fps regardless, because input
        // drives its own repaints. Fully idle = no repaint at all.
        if self.session.is_scanning()
            || self.session.has_pending()
            || self.session.has_gallery_pending()
            || self.session.has_background_work()
        {
            ctx.request_repaint_after(Duration::from_millis(33));
        } else if self.debug {
            ctx.request_repaint_after(Duration::from_millis(250));
        }
    }

    /// Final save on quit — the durable backstop behind the debounced autosave.
    fn on_exit(&mut self) {
        self.save_now();
    }
}

impl DcsApp {
    /// Open a folder dropped onto the window. A dropped file opens its
    /// containing folder, so dropping any photo works as well as a folder.
    fn handle_dropped_folder(&mut self, ctx: &egui::Context) {
        let Some(path) = ctx.input(|i| i.raw.dropped_files.iter().find_map(|f| f.path.clone()))
        else {
            return;
        };
        let dir = if path.is_dir() {
            path
        } else {
            path.parent().map(PathBuf::from).unwrap_or(path)
        };
        self.open_path(dir);
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

    /// Refresh the project lock on a throttled heartbeat so peers see us as live
    /// while a folder is open and we own it.
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

    /// Run a registry action: the app does what it can, then we perform whatever
    /// shell work it hands back. The one path every surface (keys, palette,
    /// menus, chips) funnels through.
    fn dispatch(&mut self, action: dcs_app::AppAction, ctx: &egui::Context) {
        let effect = self.session.run_action(action);
        self.apply_effect(effect, ctx);
    }

    fn apply_effect(&mut self, effect: dcs_app::ActionEffect, ctx: &egui::Context) {
        use dcs_app::ActionEffect as E;
        match effect {
            E::None => {}
            E::PickFolder => self.open_folder_dialog(),
            E::OpenPath(path) => self.open_path(path),
            E::ClearTextures => self.textures.clear(),
            E::ZoomIn => self.zoom(ZOOM_STEP),
            E::ZoomOut => self.zoom(1.0 / ZOOM_STEP),
            E::ToggleDiagnostics => self.debug = !self.debug,
            E::OpenZonePicker => self.zone_picker.open(),
            E::OpenCameraZonePicker => self.camera_zone_picker.open(),
            E::OpenFilterStatePalette => {
                self.filter_palette_state = true;
                self.filter_palette.open_sticky();
            }
            E::OpenFilterTagPalette => {
                self.filter_palette_state = false;
                self.filter_palette.open_sticky();
            }
            E::ShowMetadata => self.show_metadata = true,
            E::ShowAbout => self.show_about = true,
            E::OpenTagPalette => {
                self.tag_palette_remove = false;
                self.tag_palette.open();
            }
            E::OpenUntagPalette => {
                self.tag_palette_remove = true;
                self.tag_palette.open();
            }
            E::OpenTagManager => {
                self.show_tag_manager = true;
                self.tag_edits.clear();
            }
            E::OpenExport => {
                self.export.open = true;
                // Default scope to the current selection when there is one.
                if self.session.selection_count() > 0 {
                    self.export.scope = dcs_app::ExportScope::Selection;
                }
            }
            E::CollapseAllGroups => {
                let titles: Vec<String> = self
                    .session
                    .groups()
                    .iter()
                    .filter(|g| g.kind != GroupKind::Stream)
                    .map(|g| g.title.clone())
                    .collect();
                self.collapsed.extend(titles);
            }
            E::ExpandAllGroups => self.collapsed.clear(),
            E::Quit => ctx.send_viewport_cmd(egui::ViewportCommand::Close),
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
        // Persist the Grid view's zoom so reopening the folder restores it.
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
