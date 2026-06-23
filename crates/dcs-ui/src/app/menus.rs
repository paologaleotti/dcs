use egui::containers::menu::{MenuButton, MenuConfig};
use egui::{Align, FontId, Layout, PopupCloseBehavior, RichText, Sense, Ui, Vec2};

use super::{DcsApp, ViewMode};
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
                    micro_label(ui, "GROUP");
                    if let Some(a) = self.group_menu(ui) {
                        clicked = Some(a);
                    }
                    micro_label(ui, "SORT");
                    if let Some(a) = self.sort_menu(ui) {
                        clicked = Some(a);
                    }

                    ui.separator();
                    micro_label(ui, "FILTER");
                    if let Some(a) = self.filter_menu(ui) {
                        clicked = Some(a);
                    }

                    ui.separator();
                    micro_label(ui, "TZ");
                    if let Some(a) = self.tz_menu(ui) {
                        clicked = Some(a);
                    }

                    ui.separator();
                    micro_label(ui, "SEARCH");
                    self.search_box(ui);

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

    /// The AI-search control in the toolbar. Shape follows [`dcs_app::AiStatus`]:
    /// an opt-in gate, load progress, then a live search field (with a
    /// quiet indexing count while the background sweep finishes).
    fn search_box(&mut self, ui: &mut Ui) {
        use dcs_app::AiStatus;
        match self.session.ai_status().clone() {
            AiStatus::Disabled => {
                if ui
                    .button(RichText::new("Enable AI search").monospace())
                    .on_hover_text("Turn on local AI search — find photos by what's in them")
                    .clicked()
                {
                    self.session.enable_ai_search();
                }
            }
            AiStatus::Loading => {
                ui.add(egui::Spinner::new());
                ui.label(
                    RichText::new("loading model…")
                        .monospace()
                        .color(theme::TEXT_DIM),
                );
            }
            AiStatus::Indexing { done, total } => {
                self.search_field(ui);
                ui.label(
                    RichText::new(format!("indexing {done}/{total}"))
                        .font(FontId::monospace(11.0))
                        .color(theme::TEXT_DIM),
                );
                self.disable_search_button(ui);
            }
            AiStatus::Ready => {
                self.search_field(ui);
                self.disable_search_button(ui);
            }
            AiStatus::Error(message) => {
                if ui
                    .button(
                        RichText::new("AI search failed — retry")
                            .monospace()
                            .color(theme::TEXT_DIM),
                    )
                    .on_hover_text(message)
                    .clicked()
                {
                    self.session.enable_ai_search();
                }
            }
        }
    }

    /// The live search text field. Enter **replaces** the current search;
    /// Shift+Enter **chains** an OR'd term. Clears the buffer on submit.
    fn search_field(&mut self, ui: &mut Ui) {
        let response = ui
            .add(
                egui::TextEdit::singleline(&mut self.search_input)
                    .hint_text("search photos…")
                    .desired_width(150.0)
                    .font(FontId::monospace(12.0)),
            )
            .on_hover_text("Enter replaces the search · Shift+Enter adds a term");
        if response.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
            let chain = ui.input(|i| i.modifiers.shift);
            let query = std::mem::take(&mut self.search_input);
            if chain {
                self.session.append_search(query);
            } else {
                self.session.run_search(query);
            }
        }
    }

    /// A quiet "turn AI search off for this project" affordance next to the field.
    fn disable_search_button(&mut self, ui: &mut Ui) {
        if ui
            .small_button(RichText::new("off").monospace().color(theme::TEXT_DIM))
            .on_hover_text("Disable AI search for this project")
            .clicked()
        {
            self.session.disable_ai_search();
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
                        if ui.button("Delete recent projects history").clicked() {
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
                ui.menu_button("View", |ui| {
                    // The checkbox renders the live state; its change dispatches
                    // the registry toggle (the local bool is just the indicator).
                    let mut show_bursts = self.session.show_bursts();
                    let time_sort = self.session.sort().key == dcs_app::SortKey::Time;
                    if ui
                        .checkbox(&mut show_bursts, "Show Bursts overlay")
                        .on_hover_text("Burst overlay shows only under a time sort")
                        .changed()
                    {
                        clicked = Some(AppAction::ToggleBursts);
                        ui.close();
                    }
                    // The pref is on but a name sort suppresses the overlay — say so.
                    if show_bursts && !time_sort {
                        ui.label(RichText::new("needs time sort").small().weak());
                    }
                    ui.separator();
                    if ui.button("Toggle Diagnostics").clicked() {
                        clicked = Some(AppAction::ToggleDiagnostics);
                        ui.close();
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

    /// The FILTER dropdown: one place to narrow the sheet — verdict checkboxes
    /// (multi-select) and a checkbox + color swatch per tag. Toggling emits a
    /// chip; the active chips show in the accent bar below. The button reads the
    /// active count so the toolbar shows at a glance whether a filter is on.
    fn filter_menu(&self, ui: &mut Ui) -> Option<dcs_app::AppAction> {
        use dcs_app::AppAction;
        use dcs_domain::cull::AcceptState;
        let mut picked = None;
        // CloseOnClickOutside keeps the dropdown open while you tick several
        // boxes — toggling a checkbox shouldn't dismiss the menu.
        MenuButton::new(RichText::new(self.filter_summary()).monospace())
            .config(MenuConfig::new().close_behavior(PopupCloseBehavior::CloseOnClickOutside))
            .ui(ui, |ui| {
                ui.set_min_width(180.0);
                ui.label(RichText::new("STATE").small().weak());
                for (label, state) in [
                    ("unreviewed", AcceptState::Unreviewed),
                    ("accepted", AcceptState::Accepted),
                    ("rejected", AcceptState::Rejected),
                ] {
                    let mut on = self.session.verdict_filter_active(state);
                    if ui.checkbox(&mut on, label).changed() {
                        picked = Some(AppAction::ToggleVerdictFilter(state));
                    }
                }

                ui.add_space(4.0);
                ui.label(RichText::new("TAGS").small().weak());
                let tags = self.session.all_tags();
                if tags.is_empty() {
                    ui.add_enabled(false, egui::Button::new(RichText::new("no tags").small()));
                } else {
                    for tag in &tags {
                        // Checkbox, then the tag's color swatch, then the name.
                        ui.horizontal(|ui| {
                            let mut on = self.session.tag_chip_active(tag.id);
                            let cb = ui.checkbox(&mut on, "");
                            let (rect, _) =
                                ui.allocate_exact_size(Vec2::splat(11.0), Sense::hover());
                            ui.painter()
                                .rect_filled(rect, 2.0, theme::tag_color32(tag.color));
                            ui.add_space(4.0);
                            let lbl = ui.add(
                                egui::Label::new(RichText::new(&tag.name).monospace())
                                    .sense(Sense::click()),
                            );
                            if cb.changed() || lbl.clicked() {
                                picked = Some(AppAction::ToggleFilterTag(tag.id));
                            }
                        });
                    }
                }

                if self.session.is_filtered() {
                    ui.separator();
                    if ui.button(RichText::new("clear").monospace()).clicked() {
                        picked = Some(AppAction::ClearFilters);
                        ui.close();
                    }
                }
            });
        picked
    }

    /// The FILTER button's label: how many chips are active, or `none`.
    fn filter_summary(&self) -> String {
        let n: usize = self
            .session
            .active_filter()
            .groups
            .iter()
            .map(|g| g.chips.len())
            .sum();
        if n == 0 {
            "none".to_string()
        } else {
            format!("{n} active")
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
