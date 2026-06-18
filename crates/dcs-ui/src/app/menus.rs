use egui::{Align, FontId, Layout, RichText, Ui};

use super::{DcsApp, VERDICT_FILTERS, ViewMode};
use crate::theme;

impl DcsApp {
    pub(super) fn top_bar(&mut self, ui: &mut Ui, ctx: &egui::Context) {
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
                    if let Some(a) = self.tz_menu(ui) {
                        clicked = Some(a);
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

    pub(super) fn menu_bar(&mut self, ui: &mut Ui, ctx: &egui::Context) {
        use dcs_app::AppAction;
        // Menu items mirror the registry: each just names an `AppAction`;
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
                            self.session.has_folder(),
                            egui::Button::new("Reveal in File Manager"),
                        )
                        .on_hover_text("Open the project folder in your file manager")
                        .clicked()
                    {
                        clicked = Some(AppAction::RevealFolder);
                        ui.close();
                    }
                    ui.separator();
                    if ui.button("Quit").clicked() {
                        clicked = Some(AppAction::Quit);
                    }
                });
                ui.menu_button("Tags", |ui| {
                    let has_sel = self.session.selection_count() > 0;
                    if ui
                        .add_enabled(has_sel, egui::Button::new("Add to Selection…"))
                        .clicked()
                    {
                        clicked = Some(AppAction::OpenTagPalette);
                        ui.close();
                    }
                    if ui
                        .add_enabled(
                            self.session.selection_has_tags(),
                            egui::Button::new("Remove from Selection…"),
                        )
                        .clicked()
                    {
                        clicked = Some(AppAction::OpenUntagPalette);
                        ui.close();
                    }
                    ui.separator();
                    if ui
                        .add_enabled(
                            !self.session.all_tags().is_empty(),
                            egui::Button::new("Manage Tags"),
                        )
                        .clicked()
                    {
                        clicked = Some(AppAction::ManageTags);
                        ui.close();
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

    /// The GROUP dropdown: pick the axis + granularity inline. The palette
    /// mirrors these; the menu is the direct UI.
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
            if ui
                .selectable_label(axis == Axis::Tag, RichText::new("Tag").monospace())
                .clicked()
            {
                picked = Some(dcs_app::AppAction::GroupBy(Axis::Tag));
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

    /// The TZ dropdown: one entry point for both timezones. The button shows the
    /// active travel zone (what times are displayed in); the menu sets the travel
    /// and camera zones and explains how they interact.
    fn tz_menu(&self, ui: &mut Ui) -> Option<dcs_app::AppAction> {
        let mut picked = None;
        let travel = self.session.shoot_zone();
        let label = travel.unwrap_or("set").to_string();
        ui.menu_button(RichText::new(label).monospace(), |ui| {
            ui.set_min_width(240.0);
            ui.label(RichText::new("TIMEZONES").small().weak());
            ui.add_space(2.0);

            let travel_now = travel.unwrap_or("system default");
            if tz_row(
                ui,
                "Travel TZ",
                travel_now,
                "Times are shown & grouped in this zone",
            )
            .clicked()
            {
                picked = Some(dcs_app::AppAction::SetShootZone);
                ui.close();
            }
            let camera_now = self.session.camera_zone().unwrap_or("system default");
            if tz_row(
                ui,
                "Camera TZ",
                camera_now,
                "Zone the camera clock was set to — used only when a photo has no EXIF offset",
            )
            .clicked()
            {
                picked = Some(dcs_app::AppAction::SetCameraZone);
                ui.close();
            }

            ui.add_space(4.0);
            ui.separator();
            ui.label(
                RichText::new("A photo's own EXIF offset always wins over the camera zone.")
                    .small()
                    .weak(),
            );
        });
        picked
    }

    /// Short label for the active grouping: the axis, or the time granularity
    /// with `auto`'s resolution shown, e.g. `auto (day)`.
    fn group_label(&self) -> String {
        use dcs_app::{Axis, TimeGranularity};
        match self.session.axis() {
            Axis::None => "none".to_string(),
            Axis::Tag => "tag".to_string(),
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
}

/// A small uppercase mono section label, dim — the "edge annotation" style.
/// One source so every toolbar group labels the same way.
fn micro_label(ui: &mut Ui, text: &str) {
    ui.label(
        RichText::new(text)
            .font(FontId::monospace(10.0))
            .color(theme::TEXT_DIM),
    );
}

/// One full-width clickable row in the TZ menu: field name + its current value,
/// with a hover hint. Returns the row's response so the caller can dispatch.
fn tz_row(ui: &mut Ui, name: &str, value: &str, hint: &str) -> egui::Response {
    let text = format!("{name:<10}{value}");
    let row = ui.selectable_label(false, RichText::new(text).monospace());
    row.on_hover_text(hint)
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
