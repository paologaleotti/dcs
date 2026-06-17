//! The eframe application: top bar (open folder, view-mode indicator),
//! central grid, status bar, and a toggleable diagnostics overlay (§10b).
//! Ephemeral UI state — zoom, the GPU texture cache, debug flags — lives here
//! and never travels down (§9).

use std::collections::HashSet;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use dcs_app::{Session, VerdictFilter};
use dcs_domain::grouping::GroupKind;
use egui::{Align, Align2, FontId, Key, Layout, RichText, Ui};

use crate::export::ExportDialog;
use crate::gallery;
use crate::grid;
use crate::grid::TextureCache;
use crate::keymap;
use crate::picker::{Picker, PickerEvent, PickerItem};
use crate::theme;

/// First row of the timezone picker — picking it clears the shoot zone back to
/// the system default (so "no zone" stays a keyboard-reachable choice).
const CLEAR_ZONE_ROW: &str = "(clear — use system default zone)";

/// The verdict filter chips, in toolbar order.
const VERDICT_FILTERS: [(&str, VerdictFilter); 4] = [
    ("all", VerdictFilter::All),
    ("unrev", VerdictFilter::Unreviewed),
    ("acc", VerdictFilter::Accepted),
    ("rej", VerdictFilter::Rejected),
];

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

/// Idle gap after the last verdict change before the project auto-saves (§10b).
/// Long enough that rapid A/X culling coalesces into one write, short enough
/// that a crash loses at most a couple seconds of verdicts.
const SAVE_DEBOUNCE: Duration = Duration::from_millis(1500);

/// How often the live instance refreshes its lock timestamp so peers keep
/// seeing it as alive. Well under the stale window (#34).
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
    /// Shoot-timezone picker — a keyboard-first fuzzy quick-pick (the same
    /// component the command palette and tag palette will use).
    zone_picker: Picker,
    /// The `Cmd/Ctrl+P` command palette over the whole registry (§2.10).
    palette: Picker,
    /// Collapsed group titles (ephemeral UI state, §2.8). Keyed by header title.
    collapsed: HashSet<String>,
    /// Export dialog state (§6); persisted across opens.
    export: ExportDialog,
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
            view: ViewMode::Grid,
            // The gallery holds only the current frame and its neighbours, but
            // each is large — a dedicated, smaller VRAM budget keeps the two
            // caches from together pinning ~1.5 GB.
            gallery_textures: TextureCache::with_budget(GALLERY_TEXTURE_BYTES),
            gallery_full: false,
            strip_collapsed: false,
            cell: 160.0,
            debug: false,
            fps: 0.0,
            visible: 0,
            cols: 1,
            scroll_to_focus: false,
            dirty_since: None,
            show_about: false,
            zone_picker: Picker::new("Shoot timezone"),
            palette: Picker::new("Command Palette"),
            collapsed: HashSet::new(),
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
        // Toolbar and status bar only exist once a project is open (§4).
        if self.session.has_folder() {
            self.top_bar(ui, &ctx);
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
        self.export_dialog(&ctx);
        self.zone_picker(&ctx);
        self.command_palette(&ctx);
        if self.debug {
            self.diagnostics(&ctx);
        }

        // While decodes are streaming in, poll at ~30 fps rather than spinning
        // a core at full framerate — new thumbnails still appear within a frame
        // or two. Active scrolling repaints at 60 fps regardless, because input
        // drives its own repaints. Fully idle = no repaint at all (§3).
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
    fn top_bar(&mut self, ui: &mut Ui, ctx: &egui::Context) {
        // Dispatch after the closure so the registry stays the only mutation path.
        let mut clicked: Option<dcs_app::AppAction> = None;
        // View-mode switch is UI-only (not a registry action); applied post-panel.
        let mut switch_grid = false;
        let mut switch_gallery = false;
        egui::Panel::top("top")
            .frame(
                egui::Frame::default()
                    .fill(theme::CHROME_BG)
                    .inner_margin(egui::Margin::symmetric(8, 5)),
            )
            .show_inside(ui, |ui| {
                // Center every item on the row's vertical axis so the small section
                // labels line up with the taller chips.
                ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                    micro_label(ui, "MODE");
                    if ui
                        .selectable_label(
                            self.view == ViewMode::Grid,
                            RichText::new("grid").monospace(),
                        )
                        .clicked()
                    {
                        switch_grid = true;
                    }
                    if ui
                        .selectable_label(
                            self.view == ViewMode::Gallery,
                            RichText::new("gallery").monospace(),
                        )
                        .on_hover_text("Open the focused photo big (Space)")
                        .clicked()
                    {
                        switch_gallery = true;
                    }

                    ui.separator();
                    micro_label(ui, "VIEW");
                    let active = self.session.filter();
                    for (label, filter) in VERDICT_FILTERS {
                        if ui
                            .selectable_label(active == filter, RichText::new(label).monospace())
                            .clicked()
                        {
                            clicked = Some(dcs_app::AppAction::SetFilter(filter));
                        }
                    }

                    ui.separator();
                    micro_label(ui, "GROUP");
                    if let Some(a) = self.group_menu(ui) {
                        clicked = Some(a);
                    }
                    micro_label(ui, "SORT");
                    if let Some(a) = self.sort_menu(ui) {
                        clicked = Some(a);
                    }

                    ui.separator();
                    micro_label(ui, "TZ");
                    let zone = self.session.shoot_zone().unwrap_or("set").to_string();
                    if ui
                        .selectable_label(false, RichText::new(zone).monospace())
                        .on_hover_text("Timezone used to group photos by time")
                        .clicked()
                    {
                        clicked = Some(dcs_app::AppAction::SetShootZone);
                    }

                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        // Diagnostics toggle lives in the command palette (⌘P), not
                        // the toolbar — it's a dev affordance, not a daily control.
                        if ui.button("+").clicked() {
                            clicked = Some(dcs_app::AppAction::ZoomIn);
                        }
                        if ui.button("−").clicked() {
                            clicked = Some(dcs_app::AppAction::ZoomOut);
                        }
                        micro_label(ui, "ZOOM");

                        ui.separator();
                        if ui
                            .add_enabled(
                                self.session.pool_len() > 0,
                                egui::Button::new(RichText::new("Export…").monospace()),
                            )
                            .clicked()
                        {
                            clicked = Some(dcs_app::AppAction::OpenExport);
                        }
                    });
                });
            });
        if let Some(action) = clicked {
            self.dispatch(action, ctx);
        }
        if switch_gallery && self.view == ViewMode::Grid {
            self.enter_gallery();
        } else if switch_grid && self.view == ViewMode::Gallery {
            self.exit_gallery();
        }
    }

    /// The GROUP dropdown (§2.3 always-visible control): pick the axis +
    /// granularity inline. The palette mirrors these; the menu is the direct UI.
    fn group_menu(&self, ui: &mut Ui) -> Option<dcs_app::AppAction> {
        use dcs_app::{Axis, TimeGranularity};
        let axis = self.session.axis();
        let mut picked = None;
        ui.menu_button(RichText::new(self.group_label()).monospace(), |ui| {
            if ui
                .selectable_label(axis == Axis::None, RichText::new("None").monospace())
                .clicked()
            {
                picked = Some(dcs_app::AppAction::GroupBy(Axis::None));
                ui.close();
            }
            ui.separator();
            for (g, label) in [
                (TimeGranularity::Auto, "Auto"),
                (TimeGranularity::SmartDay, "Smart day"),
                (TimeGranularity::Hour, "Hour"),
                (TimeGranularity::Day, "Day"),
                (TimeGranularity::Week, "Week"),
            ] {
                if ui
                    .selectable_label(axis == Axis::Time(g), RichText::new(label).monospace())
                    .clicked()
                {
                    picked = Some(dcs_app::AppAction::SetGranularity(g));
                    ui.close();
                }
            }
        });
        picked
    }

    /// The SORT dropdown: pick key + direction inline.
    fn sort_menu(&self, ui: &mut Ui) -> Option<dcs_app::AppAction> {
        use dcs_app::{Sort, SortDir, SortKey};
        let active = self.session.sort();
        let mut picked = None;
        ui.menu_button(RichText::new(self.sort_label()).monospace(), |ui| {
            for (key, name) in [(SortKey::Time, "Time"), (SortKey::Name, "Name")] {
                for dir in [SortDir::Asc, SortDir::Desc] {
                    let sort = Sort { key, dir };
                    let label = format!("{name} {}", sort_dir_label(dir));
                    if ui
                        .selectable_label(active == sort, RichText::new(label).monospace())
                        .clicked()
                    {
                        picked = Some(dcs_app::AppAction::SetSort(sort));
                        ui.close();
                    }
                }
            }
        });
        picked
    }

    /// Short label for the active grouping (§2.3 always visible): the axis, or
    /// the time granularity with `auto`'s resolution shown, e.g. `auto (day)`.
    fn group_label(&self) -> String {
        use dcs_app::{Axis, TimeGranularity};
        match self.session.axis() {
            Axis::None => "none".to_string(),
            Axis::Time(g) => {
                let resolved = self.session.resolved_granularity();
                match (g, resolved) {
                    (TimeGranularity::Auto, Some(r)) => format!("auto ({})", gran_word(r)),
                    _ => gran_word(g).to_string(),
                }
            }
        }
    }

    /// Short label for the active sort, e.g. `time ↑ asc`.
    fn sort_label(&self) -> String {
        use dcs_app::SortKey;
        let key = match self.session.sort().key {
            SortKey::Time => "time",
            SortKey::Name => "name",
        };
        format!("{key} {}", sort_dir_label(self.session.sort().dir))
    }

    fn menu_bar(&mut self, ui: &mut Ui, ctx: &egui::Context) {
        use dcs_app::AppAction;
        // Menu items mirror the registry (§2.10): each just names an `AppAction`;
        // the selected one dispatches through the same path as keys and palette.
        let mut clicked: Option<AppAction> = None;
        egui::Panel::top("menu").show_inside(ui, |ui| {
            egui::MenuBar::new().ui(ui, |ui| {
                ui.menu_button("File", |ui| {
                    if ui.button("Open Project…").clicked() {
                        clicked = Some(AppAction::OpenFolder);
                        ui.close();
                    }
                    ui.menu_button("Open Recent", |ui| {
                        let recents = self.session.recent_projects().to_vec();
                        if recents.is_empty() {
                            ui.add_enabled(false, egui::Button::new("(none)"));
                            return;
                        }
                        for (i, path) in recents.iter().enumerate() {
                            let label = path
                                .file_name()
                                .map(|n| n.to_string_lossy().into_owned())
                                .unwrap_or_else(|| path.to_string_lossy().into_owned());
                            if ui
                                .button(label)
                                .on_hover_text(path.to_string_lossy())
                                .clicked()
                            {
                                clicked = Some(AppAction::OpenRecent(i));
                                ui.close();
                            }
                        }
                        ui.separator();
                        if ui.button("Clear Recents").clicked() {
                            clicked = Some(AppAction::ClearRecents);
                            ui.close();
                        }
                    });
                    ui.separator();
                    if ui
                        .add_enabled(
                            self.session.has_folder(),
                            egui::Button::new("Rescan Folder"),
                        )
                        .clicked()
                    {
                        clicked = Some(AppAction::Rescan);
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
                        clicked = Some(AppAction::ForgetMissing);
                        ui.close();
                    }
                    ui.separator();
                    if ui
                        .add_enabled(self.session.pool_len() > 0, egui::Button::new("Export…"))
                        .clicked()
                    {
                        clicked = Some(AppAction::OpenExport);
                        ui.close();
                    }
                    if ui
                        .add_enabled(
                            self.session.has_rejected(),
                            egui::Button::new("Reveal Rejected in File Manager"),
                        )
                        .clicked()
                    {
                        clicked = Some(AppAction::RevealRejected);
                        ui.close();
                    }
                    ui.separator();
                    if ui.button("Quit").clicked() {
                        clicked = Some(AppAction::Quit);
                    }
                });
                ui.menu_button("Help", |ui| {
                    if ui.button("About dcs").clicked() {
                        clicked = Some(AppAction::About);
                        ui.close();
                    }
                });
            });
        });
        if let Some(action) = clicked {
            self.dispatch(action, ctx);
        }
    }

    /// Open a folder dropped onto the window (§4). A dropped file opens its
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
                    RichText::new(concat!(
                        "digital contact sheet · v",
                        env!("CARGO_PKG_VERSION")
                    ))
                    .monospace()
                    .color(theme::TEXT_DIM),
                );
                ui.add_space(6.0);
                ui.label(
                    RichText::new(
                        "Fast, keyboard-first photo culling.\nScan · cull · tag · export.",
                    )
                    .monospace()
                    .size(12.0),
                );
            });
        self.show_about = open;
    }

    /// The export dialog (§6.1–6.7): staged settings, a live dry-run preview, and
    /// progress — the preview and the run share one `ExportPlan` from the
    /// conductor, so the dialog never lies about what it copies.
    fn export_dialog(&mut self, ctx: &egui::Context) {
        if !self.export.open {
            return;
        }
        use dcs_app::{Collision, ExportScope, FileSelection, Layout};

        // Resolve everything that needs the session before the panel borrows
        // `self.export` mutably for its controls. Only the settings view needs
        // the scope counts and live plan, so skip that work once a run starts.
        let status = self.session.export_status();
        let idle = status.is_none();
        let scopes = [
            (ExportScope::Selection, "Selection"),
            (ExportScope::Accepted, "Accepted"),
            (ExportScope::AcceptedAndUnreviewed, "Accepted + Unreviewed"),
            (ExportScope::Unreviewed, "Unreviewed"),
            (ExportScope::Rejected, "Rejected"),
            (ExportScope::Everything, "Everything"),
        ];
        let scope_counts: Vec<(ExportScope, &str, usize)> = if idle {
            scopes
                .iter()
                .map(|&(s, l)| (s, l, self.session.export_scope_count(s)))
                .collect()
        } else {
            Vec::new()
        };
        let unreviewed = if idle {
            self.session.unreviewed_count()
        } else {
            0
        };
        let preview = idle
            .then(|| self.export.request())
            .flatten()
            .map(|r| self.session.plan_export(self.export.scope, &r));

        let mut keep_open = true;
        let (mut choose, mut run, mut cancel, mut open_dest, mut close) =
            (false, false, false, false, false);

        egui::Window::new("Export")
            .collapsible(false)
            .resizable(false)
            .anchor(Align2::CENTER_CENTER, [0.0, 0.0])
            .open(&mut keep_open)
            .show(ctx, |ui| {
                ui.set_width(440.0);

                if let Some(st) = status {
                    if st.running {
                        ui.label(
                            RichText::new(format!("Copying… {}/{}", st.done(), st.total))
                                .monospace(),
                        );
                        ui.add(egui::ProgressBar::new(progress(st.done(), st.total)));
                        ui.add_space(6.0);
                        if ui.button("Cancel").clicked() {
                            cancel = true;
                        }
                    } else {
                        ui.label(
                            RichText::new(format!(
                                "Done — {} copied, {} skipped, {} failed.",
                                st.copied, st.skipped, st.failed
                            ))
                            .strong(),
                        );
                        ui.add_space(6.0);
                        ui.horizontal(|ui| {
                            if ui.button("Open folder").clicked() {
                                open_dest = true;
                            }
                            if ui.button("Close").clicked() {
                                close = true;
                            }
                        });
                    }
                    return;
                }

                let max_h = ui.ctx().content_rect().height() * 0.55;
                egui::ScrollArea::vertical()
                    .max_height(max_h)
                    .auto_shrink([false, true])
                    .show(ui, |ui| {
                        section(ui, "Scope", |ui| {
                            for (scope, label, count) in &scope_counts {
                                ui.radio_value(
                                    &mut self.export.scope,
                                    *scope,
                                    format!("{label}  ·  {count}"),
                                );
                            }
                            if self.export.scope == ExportScope::Accepted && unreviewed > 0 {
                                ui.add_space(2.0);
                                ui.label(
                                    RichText::new(format!("{unreviewed} unreviewed excluded"))
                                        .small()
                                        .color(theme::TEXT_DIM),
                                );
                            }
                        });

                        section(ui, "Files", |ui| {
                            ui.horizontal_wrapped(|ui| {
                                ui.radio_value(&mut self.export.files, FileSelection::Any, "Any")
                                    .on_hover_text(
                                        "Copy whatever files each photo has, as shot — never skips",
                                    );
                                ui.radio_value(
                                    &mut self.export.files,
                                    FileSelection::Both,
                                    "RAW + JPEG",
                                )
                                .on_hover_text(
                                    "Only photos that have both a RAW and a JPEG — copies both \
                                     files, skips the rest",
                                );
                                ui.radio_value(
                                    &mut self.export.files,
                                    FileSelection::Jpeg,
                                    "JPEG only",
                                )
                                .on_hover_text("Copy each photo's JPEG; skip photos with no JPEG");
                                ui.radio_value(
                                    &mut self.export.files,
                                    FileSelection::Raw,
                                    "RAW only",
                                )
                                .on_hover_text("Copy each photo's RAW; skip photos with no RAW");
                            });
                        });

                        section(ui, "Layout", |ui| {
                            ui.radio_value(&mut self.export.layout, Layout::Together, "One folder");
                            ui.radio_value(
                                &mut self.export.layout,
                                Layout::SplitJpegRaw,
                                "Split JPEG / RAW",
                            );
                            ui.radio_value(
                                &mut self.export.layout,
                                Layout::MirrorSource,
                                "Mirror source tree",
                            );
                            ui.radio_value(
                                &mut self.export.layout,
                                Layout::GroupAsFolders,
                                "A folder per group",
                            );
                        });

                        section(ui, "On name collision", |ui| {
                            ui.horizontal_wrapped(|ui| {
                                ui.radio_value(
                                    &mut self.export.collision,
                                    Collision::Rename,
                                    "Rename (-1, -2…)",
                                );
                                ui.radio_value(&mut self.export.collision, Collision::Skip, "Skip");
                            });
                        });

                        section(ui, "Rename template", |ui| {
                            ui.checkbox(&mut self.export.template_on, "Rename copies");
                            if self.export.template_on {
                                ui.add(
                                    egui::TextEdit::singleline(&mut self.export.template)
                                        .desired_width(f32::INFINITY)
                                        .hint_text("{name}_{seq}"),
                                );
                                ui.label(
                                    RichText::new("tokens: {name} {date} {time} {group} {seq}")
                                        .small()
                                        .color(theme::TEXT_DIM),
                                );
                            }
                        });

                        section(ui, "Destination", |ui| {
                            ui.horizontal(|ui| {
                                if ui.button("Choose…").clicked() {
                                    choose = true;
                                }
                                match &self.export.dest {
                                    Some(p) => ui.label(RichText::new(p.display().to_string())),
                                    None => ui
                                        .label(RichText::new("none chosen").color(theme::TEXT_DIM)),
                                };
                            });
                        });
                    });

                ui.separator();
                ui.add_space(4.0);

                let mut ops = 0usize;
                match &preview {
                    None => {
                        ui.label(
                            RichText::new("Choose a destination to preview.")
                                .color(theme::TEXT_DIM),
                        );
                    }
                    Some(Ok(plan)) => {
                        ops = plan.ops.len();
                        ui.label(&plan.summary);
                        if !plan.skipped.is_empty() {
                            ui.label(
                                RichText::new(format!(
                                    "{} skipped — no matching file",
                                    plan.skipped.len()
                                ))
                                .small()
                                .color(theme::TEXT_DIM),
                            );
                        }
                    }
                    Some(Err(e)) => {
                        ui.label(RichText::new(e.to_string()).color(theme::VERDICT_REJECT));
                    }
                }
                ui.add_space(6.0);
                let files = if ops == 1 { "file" } else { "files" };
                ui.add_enabled_ui(ops > 0, |ui| {
                    if ui
                        .add_sized(
                            [ui.available_width(), 28.0],
                            egui::Button::new(
                                RichText::new(format!("Copy {ops} {files}")).strong(),
                            ),
                        )
                        .clicked()
                    {
                        run = true;
                    }
                });
            });

        if choose && let Some(dir) = rfd::FileDialog::new().pick_folder() {
            self.export.dest = Some(dir);
        }
        if run && let Some(Ok(plan)) = preview {
            self.session.start_export(plan);
        }
        if cancel {
            self.session.cancel_export();
        }
        if open_dest && let Some(dest) = self.export.dest.clone() {
            self.session.reveal(&dest);
        }
        if !keep_open || close {
            self.export.open = false;
            self.session.clear_export_status();
        }
    }

    /// Searchable timezone picker on the reusable [`Picker`]. First row clears
    /// the zone back to the system default.
    fn zone_picker(&mut self, ctx: &egui::Context) {
        if !self.zone_picker.is_open() {
            return;
        }
        let subtitle = match self.session.shoot_zone() {
            Some(z) => format!("current: {z}"),
            None => "current: system default".to_string(),
        };
        let zones = dcs_domain::timezone::zone_names();
        let mut items: Vec<PickerItem> = Vec::with_capacity(zones.len() + 1);
        items.push(PickerItem {
            label: CLEAR_ZONE_ROW,
            detail: Some("system default"),
        });
        items.extend(zones.iter().map(|z| PickerItem::new(z)));

        match self
            .zone_picker
            .show(ctx, Some(&subtitle), "search zone… e.g. tokyo", &items)
        {
            PickerEvent::Picked(0) => self.session.set_shoot_zone(None),
            PickerEvent::Picked(i) => {
                self.session.set_shoot_zone(Some(zones[i - 1].to_string()));
            }
            PickerEvent::Dismissed | PickerEvent::Pending => {}
        }
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

    fn central(&mut self, ui: &mut Ui) {
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
                        );
                        self.visible = resp.visible;
                        self.cols = resp.cols;
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
                    }
                }
            });
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
        // Any picker owns the keyboard while open (it consumes its own keys in
        // `Picker::show`), so the grid stays inert behind it — just bail.
        if self.palette.is_open() || self.zone_picker.is_open() {
            return;
        }
        // The About window is a plain modal: Esc closes it, grid keys inert.
        if self.show_about {
            if ctx.input(|i| i.key_pressed(Key::Escape)) {
                self.show_about = false;
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

    /// Open the gallery on the focused photo (else the first visible), starting
    /// contain-fit.
    fn enter_gallery(&mut self) {
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
    fn exit_gallery(&mut self) {
        self.view = ViewMode::Grid;
        self.gallery_full = false;
        self.session.clear_gallery();
        self.gallery_textures.clear();
        self.scroll_to_focus = true;
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
            E::ShowAbout => self.show_about = true,
            E::OpenExport => {
                self.export.open = true;
                // Default scope to the current selection when there is one (§6.2).
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

    /// Arrow navigation and selection — UI-only because they run over the visual
    /// group layout (column count + collapse), which is a view fact (§2.8).
    /// `Esc` clears the selection (§2.12). A move flags the grid to scroll the
    /// focus cell into view next paint.
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
    /// A collapsed group exposes only its cover cell (#16), so focus never lands
    /// on a hidden cell. Group boundaries reset the row, matching the painted
    /// layout (each group flows its own rows of `cols`).
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

    /// The `Cmd/Ctrl+P` command palette (§2.10) on the reusable [`Picker`].
    /// Fuzzy over every available action, most-recently-used first; the chosen
    /// action dispatches through the same path as keys and menus.
    fn command_palette(&mut self, ctx: &egui::Context) {
        if !self.palette.is_open() {
            return;
        }
        let entries = dcs_app::catalog(&self.session);
        let hints: Vec<Option<String>> = entries.iter().map(|e| keymap::hint(e.action)).collect();
        let items: Vec<PickerItem> = entries
            .iter()
            .zip(&hints)
            .map(|(e, hint)| PickerItem {
                label: &e.title,
                detail: hint.as_deref(),
            })
            .collect();
        let picked = match self.palette.show(ctx, None, "type a command…", &items) {
            PickerEvent::Picked(i) => Some(entries[i].action),
            PickerEvent::Dismissed | PickerEvent::Pending => None,
        };
        if let Some(action) = picked {
            self.dispatch(action, ctx);
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

/// Locate display index `idx` in the navigable row layout as `(row, col)`.
/// `None` when the cell isn't focusable (e.g. hidden inside a collapsed group).
fn nav_locate(rows: &[Vec<usize>], idx: usize) -> Option<(usize, usize)> {
    rows.iter()
        .enumerate()
        .find_map(|(r, row)| row.iter().position(|&i| i == idx).map(|c| (r, c)))
}

/// Fraction copied so far, for the export progress bar.
fn progress(done: usize, total: usize) -> f32 {
    if total == 0 {
        1.0
    } else {
        (done as f32 / total as f32).clamp(0.0, 1.0)
    }
}

/// A titled settings block in the export dialog: a heading, the controls
/// indented beneath it, and surrounding space to set it off from its neighbors.
fn section(ui: &mut Ui, title: &str, body: impl FnOnce(&mut Ui)) {
    ui.add_space(8.0);
    ui.label(RichText::new(title).strong());
    ui.add_space(2.0);
    ui.indent(title, body);
}

/// Arrow + word for a sort direction, e.g. `↑ asc` — both the glyph and the
/// spelled-out word so it reads even where the arrow font is sparse.
fn sort_dir_label(dir: dcs_app::SortDir) -> &'static str {
    match dir {
        dcs_app::SortDir::Asc => "↑ asc",
        dcs_app::SortDir::Desc => "↓ desc",
    }
}

/// One-word label for a time granularity, for the toolbar group readout.
fn gran_word(g: dcs_app::TimeGranularity) -> &'static str {
    use dcs_app::TimeGranularity as G;
    match g {
        G::Auto => "auto",
        G::SmartDay => "smart day",
        G::Hour => "hour",
        G::Day => "day",
        G::Week => "week",
    }
}

/// A small uppercase mono section label, dim — the "edge annotation" style
/// (§3). One source so every toolbar group labels the same way.
fn micro_label(ui: &mut Ui, text: &str) {
    ui.label(
        RichText::new(text)
            .font(FontId::monospace(10.0))
            .color(theme::TEXT_DIM),
    );
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
