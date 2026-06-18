use egui::{Align2, FontId, RichText, Ui};

use super::{CLEAR_ZONE_ROW, DcsApp};
use crate::keymap;
use crate::picker::{PickerEvent, PickerItem};
use crate::theme;

impl DcsApp {
    /// A banner offering "Take over" when another instance holds the lock.
    pub(super) fn read_only_banner(&mut self, ui: &mut Ui) {
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

    pub(super) fn about_window(&mut self, ctx: &egui::Context) {
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

    /// Full metadata for the focused photo (`I`): a labelled two-column grid of
    /// every known fact — gear, capture times, both timezones, and file paths.
    pub(super) fn metadata_window(&mut self, ctx: &egui::Context) {
        if !self.show_metadata {
            return;
        }
        // Closes itself if focus is lost (e.g. the pool emptied) so it never
        // strands an empty frame.
        let Some(rows) = self
            .session
            .focus()
            .and_then(|focus| self.session.photo_metadata(focus))
        else {
            self.show_metadata = false;
            return;
        };
        let mut open = true;
        egui::Window::new("Photo metadata")
            .collapsible(false)
            .resizable(false)
            .anchor(Align2::CENTER_CENTER, [0.0, 0.0])
            .open(&mut open)
            .show(ctx, |ui| {
                egui::Grid::new("metadata-grid")
                    .num_columns(2)
                    .spacing([18.0, 6.0])
                    .striped(true)
                    .show(ui, |ui| {
                        for (label, value) in &rows {
                            ui.label(
                                RichText::new(*label)
                                    .monospace()
                                    .size(12.0)
                                    .color(theme::TEXT_DIM),
                            );
                            ui.label(RichText::new(value).monospace().size(12.0));
                            ui.end_row();
                        }
                    });
            });
        self.show_metadata = open;
    }

    /// The export dialog: staged settings, a live dry-run preview, and progress
    /// — the preview and the run share one `ExportPlan` from the conductor, so
    /// the dialog never lies about what it copies.
    pub(super) fn export_dialog(&mut self, ctx: &egui::Context) {
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
        if open_dest && let Some(dest) = self.export.dest.as_deref() {
            self.session.reveal(dest);
        }
        if !keep_open || close {
            self.export.open = false;
            self.session.clear_export_status();
        }
    }

    /// Searchable timezone picker on the reusable [`Picker`]. First row clears
    /// the zone back to the system default.
    pub(super) fn zone_picker(&mut self, ctx: &egui::Context) {
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
            swatch: None,
            enabled: true,
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

    /// Camera-timezone picker: the zone the camera clock was set to, anchoring a
    /// naive EXIF time that carries no offset. First row clears to system default.
    pub(super) fn camera_zone_picker(&mut self, ctx: &egui::Context) {
        if !self.camera_zone_picker.is_open() {
            return;
        }
        let subtitle = match self.session.camera_zone() {
            Some(z) => format!("current: {z}"),
            None => "current: system default".to_string(),
        };
        let zones = dcs_domain::timezone::zone_names();
        let mut items: Vec<PickerItem> = Vec::with_capacity(zones.len() + 1);
        items.push(PickerItem {
            label: CLEAR_ZONE_ROW,
            detail: Some("system default"),
            swatch: None,
            enabled: true,
        });
        items.extend(zones.iter().map(|z| PickerItem::new(z)));

        match self
            .camera_zone_picker
            .show(ctx, Some(&subtitle), "search zone… e.g. tokyo", &items)
        {
            PickerEvent::Picked(0) => self.session.set_camera_zone(None),
            PickerEvent::Picked(i) => {
                self.session.set_camera_zone(Some(zones[i - 1].to_string()));
            }
            PickerEvent::Dismissed | PickerEvent::Pending => {}
        }
    }

    /// The `Cmd/Ctrl+P` command palette on the reusable [`Picker`]. Fuzzy over
    /// every available action, most-recently-used first; the chosen action
    /// dispatches through the same path as keys and menus.
    pub(super) fn command_palette(&mut self, ctx: &egui::Context) {
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
                swatch: None,
                enabled: true,
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

    /// The tag palette (`T` add / `Shift+T` remove). Add mode fuzzes over every
    /// tag (each with its color chip) and offers an explicit "Create" row when
    /// the typed name matches none — never creating silently. Remove mode lists
    /// only the tags currently on the selection.
    pub(super) fn tag_palette(&mut self, ctx: &egui::Context) {
        if !self.tag_palette.is_open() {
            return;
        }
        if self.tag_palette_remove {
            self.tag_remove_palette(ctx);
        } else {
            self.tag_add_palette(ctx);
        }
    }

    fn tag_add_palette(&mut self, ctx: &egui::Context) {
        let selected = self.session.selection_count();
        let tags = self.session.tags_with_selection_counts();
        let query = self.tag_palette.query().trim().to_string();
        let has_exact = tags
            .iter()
            .any(|(t, _)| t.name.eq_ignore_ascii_case(&query));
        let create_label = (!query.is_empty() && !has_exact).then(|| format!("Create “{query}”"));

        // Mark how much of the selection already carries each tag. A tag already
        // on the whole selection is disabled (adding it would do nothing).
        let details: Vec<String> = tags
            .iter()
            .map(|(_, on)| {
                if *on == 0 || selected == 0 {
                    String::new()
                } else if *on == selected {
                    "already added".to_string()
                } else {
                    format!("on {on}/{selected} photos")
                }
            })
            .collect();

        let mut items: Vec<PickerItem> = tags
            .iter()
            .enumerate()
            .map(|(i, (t, on))| PickerItem {
                label: &t.name,
                detail: (!details[i].is_empty()).then_some(details[i].as_str()),
                swatch: Some(theme::tag_color32(t.color)),
                enabled: !(selected > 0 && *on == selected),
            })
            .collect();
        if let Some(label) = &create_label {
            items.push(PickerItem {
                label,
                detail: Some("new tag"),
                swatch: None,
                enabled: true,
            });
        }

        let subtitle = if tags.is_empty() {
            format!("add to {selected} selected — type a name, then Enter to create")
        } else {
            format!("add to {selected} selected — pick a tag, or type to create")
        };
        let event =
            self.tag_palette
                .show(ctx, Some(&subtitle), "filter or name a new tag…", &items);
        match event {
            PickerEvent::Picked(i) if i < tags.len() => self.session.tag_selection(tags[i].0.id),
            PickerEvent::Picked(_) => {
                let color = dcs_domain::tag::palette_color(tags.len() + 1);
                if let Some(id) = self.session.create_tag(query, color) {
                    self.session.tag_selection(id);
                }
            }
            PickerEvent::Dismissed | PickerEvent::Pending => {}
        }
    }

    fn tag_remove_palette(&mut self, ctx: &egui::Context) {
        let selected = self.session.selection_count();
        let tags = self.session.selection_tags();
        let items: Vec<PickerItem> = tags
            .iter()
            .map(|t| PickerItem::with_swatch(&t.name, theme::tag_color32(t.color)))
            .collect();
        let subtitle = if tags.is_empty() {
            "selection has no tags to remove".to_string()
        } else {
            format!("remove from {selected} selected")
        };
        match self
            .tag_palette
            .show(ctx, Some(&subtitle), "filter tags to remove…", &items)
        {
            PickerEvent::Picked(i) => self.session.untag_selection(tags[i].id),
            PickerEvent::Dismissed | PickerEvent::Pending => {}
        }
    }

    /// The tag manager: the project's one place to rename, recolor, and delete
    /// tags. A row per tag — color swatch (click to recolor), an inline name
    /// field (rename; renaming onto another tag's name merges), photo count, and
    /// Delete. Every edit is an ordinary undoable command.
    pub(super) fn tag_manager(&mut self, ctx: &egui::Context) {
        if !self.show_tag_manager {
            return;
        }
        let tags = self.session.all_tags();
        let mut open = true;
        let mut rename: Option<(dcs_app::TagId, String)> = None;
        let mut recolor: Option<(dcs_app::TagId, dcs_app::Color)> = None;
        let mut delete: Option<dcs_app::TagId> = None;

        egui::Window::new("Tags")
            .collapsible(false)
            .resizable(true)
            .anchor(Align2::CENTER_CENTER, [0.0, 0.0])
            .open(&mut open)
            .show(ctx, |ui| {
                ui.set_min_width(540.0);
                // Buttons in this dialog shouldn't grow/shrink on hover — keep
                // them geometrically still so the table reads steady.
                ui.visuals_mut().widgets.hovered.expansion = 0.0;
                ui.visuals_mut().widgets.active.expansion = 0.0;
                ui.add_space(2.0);
                ui.label(
                    RichText::new(format!(
                        "{} tag{} · click a swatch to recolor, edit a name to rename (onto an existing name to merge)",
                        tags.len(),
                        if tags.len() == 1 { "" } else { "s" },
                    ))
                    .monospace()
                    .size(11.0)
                    .color(theme::TEXT_DIM),
                );
                ui.add_space(8.0);
                if tags.is_empty() {
                    ui.label(
                        RichText::new("No tags yet — add one with T on a selection.")
                            .monospace()
                            .color(theme::TEXT_DIM),
                    );
                    return;
                }
                egui::Grid::new("tag-manager")
                    .num_columns(4)
                    .spacing([14.0, 10.0])
                    .striped(true)
                    .show(ui, |ui| {
                        let head = |ui: &mut Ui, text: &str| {
                            ui.label(
                                RichText::new(text)
                                    .monospace()
                                    .size(10.0)
                                    .color(theme::HAIRLINE),
                            );
                        };
                        head(ui, "COLOR");
                        head(ui, "NAME");
                        head(ui, "PHOTOS");
                        head(ui, "");
                        ui.end_row();

                        for tag in &tags {
                            if let Some(c) = swatch_menu(ui, theme::tag_color32(tag.color)) {
                                recolor = Some((tag.id, c));
                            }
                            let buf = self
                                .tag_edits
                                .entry(tag.id)
                                .or_insert_with(|| tag.name.clone());
                            let resp = ui.add_sized(
                                [260.0, 26.0],
                                egui::TextEdit::singleline(buf).font(FontId::proportional(14.0)),
                            );
                            if resp.lost_focus() && !buf.trim().is_empty() && *buf != tag.name {
                                rename = Some((tag.id, buf.clone()));
                            }
                            let n = self.session.tag_photo_count(tag.id);
                            ui.label(
                                RichText::new(format!("{n} photo{}", if n == 1 { "" } else { "s" }))
                                    .monospace()
                                    .size(11.0)
                                    .color(theme::TEXT_DIM),
                            );
                            let delete_btn = egui::Button::new(
                                RichText::new("Delete").color(theme::VERDICT_REJECT),
                            )
                            .stroke(egui::Stroke::new(1.0, theme::VERDICT_REJECT));
                            if ui
                                .add_sized([90.0, 26.0], delete_btn)
                                .on_hover_text("Delete this tag and remove it from every photo")
                                .clicked()
                            {
                                delete = Some(tag.id);
                            }
                            ui.end_row();
                        }
                    });
            });

        if let Some((id, name)) = rename {
            self.session.rename_tag(id, name);
            self.tag_edits.remove(&id);
        }
        if let Some((id, color)) = recolor {
            self.session.set_tag_color(id, color);
        }
        if let Some(id) = delete {
            self.session.delete_tag(id);
            self.tag_edits.remove(&id);
        }
        if !open {
            self.show_tag_manager = false;
        }
        if !self.show_tag_manager {
            self.tag_edits.clear();
        }
    }

    pub(super) fn diagnostics(&self, ctx: &egui::Context) {
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
}

fn frame_ms(fps: f32) -> f32 {
    if fps > 0.0 { 1000.0 / fps } else { 0.0 }
}

/// Fraction copied so far, for the export progress bar.
fn progress(done: usize, total: usize) -> f32 {
    if total == 0 {
        1.0
    } else {
        (done as f32 / total as f32).clamp(0.0, 1.0)
    }
}

/// A color-swatch button that opens a menu of the curated palette colors.
/// Returns the chosen color, if any. The tag manager's recolor control.
fn swatch_menu(ui: &mut Ui, current: egui::Color32) -> Option<dcs_app::Color> {
    let mut picked = None;
    ui.menu_button(RichText::new("⬛").color(current).size(16.0), |ui| {
        ui.horizontal_wrapped(|ui| {
            for c in dcs_domain::tag::PALETTE {
                let btn =
                    egui::Button::new(RichText::new("⬛").color(theme::tag_color32(c)).size(20.0));
                if ui.add(btn).clicked() {
                    picked = Some(c);
                    ui.close();
                }
            }
        });
    });
    picked
}

/// A titled settings block in the export dialog: a heading, the controls
/// indented beneath it, and surrounding space to set it off from its neighbors.
fn section(ui: &mut Ui, title: &str, body: impl FnOnce(&mut Ui)) {
    ui.add_space(8.0);
    ui.label(RichText::new(title).strong());
    ui.add_space(2.0);
    ui.indent(title, body);
}
