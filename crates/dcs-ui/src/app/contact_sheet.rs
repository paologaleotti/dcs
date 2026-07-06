//! The export-contact-sheet dialog: its settings controls, the live WYSIWYG
//! page preview, and the render hand-off. Split out of `dialogs.rs` because it
//! is a self-contained concern (a bespoke preview painter plus its helpers).

use egui::{Align2, FontId, RichText, Ui};

use super::DcsApp;
use super::dialogs::{progress, section};
use crate::theme;

/// Header/footer point sizes, matching the PDF renderer so the preview text is
/// in the same proportion as the print.
const HEADER_FONT_PT: f32 = 7.0;
const FOOTER_FONT_PT: f32 = 7.0;

impl DcsApp {
    /// The export-contact-sheet dialog: analog-sheet settings on the left, a
    /// live WYSIWYG page preview on the right. Preview and render share one
    /// `ContactSheetPlan` from the conductor, so the exported/printed PDF is the
    /// preview. Two outputs: save a PDF, or print (render then open in the OS
    /// viewer's print dialog).
    pub(super) fn contact_sheet_dialog(&mut self, ctx: &egui::Context) {
        if !self.contact_sheet.open {
            return;
        }
        use dcs_app::ExportScope;

        let status = self.session.contact_sheet_status();
        let idle = status.is_none();

        // The render runs on a worker thread; egui otherwise only repaints on
        // input, so without this the progress bar would sit at 0 (and Esc would
        // feel dead) until the mouse moved. Keep repainting until it finishes.
        if status.is_some_and(|s| s.running) {
            ctx.request_repaint();
        }

        let mut scopes = vec![(ExportScope::Selection, "Selection")];
        if self.session.is_filtered() {
            scopes.push((ExportScope::CurrentFilter, "Current filter"));
        }
        scopes.extend([
            (ExportScope::Accepted, "Accepted"),
            (ExportScope::AcceptedAndUnreviewed, "Accepted + Unreviewed"),
            (ExportScope::Unreviewed, "Unreviewed"),
            (ExportScope::Rejected, "Rejected"),
            (ExportScope::Everything, "Everything"),
        ]);
        let scope_counts: Vec<(ExportScope, &str, usize)> = if idle {
            scopes
                .iter()
                .map(|&(s, l)| (s, l, self.session.export_scope_count(s)))
                .collect()
        } else {
            Vec::new()
        };

        // The preview plans against a placeholder destination so it renders
        // without one being chosen; export/print re-plan with the real path.
        let preview = idle.then(|| {
            let s = self.contact_sheet.settings(preview_dest());
            self.session
                .plan_contact_sheet(self.contact_sheet.scope, &s)
        });

        let mut keep_open = true;
        let (mut export, mut print, mut cancel, mut reveal, mut close) =
            (false, false, false, false, false);

        egui::Window::new("Export contact sheet")
            .collapsible(false)
            // Hug the content every frame — otherwise egui persists an old
            // (possibly oversized) rect and the window stops shrinking.
            .auto_sized()
            .anchor(Align2::CENTER_CENTER, [0.0, 0.0])
            .open(&mut keep_open)
            .show(ctx, |ui| {
                if let Some(st) = status {
                    ui.set_width(320.0);
                    if st.running {
                        ui.label(
                            RichText::new(format!("Rendering… {}/{}", st.done(), st.total_pages))
                                .monospace(),
                        );
                        ui.add(egui::ProgressBar::new(progress(st.done(), st.total_pages)));
                        ui.add_space(6.0);
                        if ui.button("Cancel").clicked() {
                            cancel = true;
                        }
                    } else if st.succeeded {
                        ui.label(
                            RichText::new(format!("Done — {} pages written.", st.rendered))
                                .strong(),
                        );
                        ui.add_space(6.0);
                        ui.horizontal(|ui| {
                            if ui.button("Reveal").clicked() {
                                reveal = true;
                            }
                            if ui.button("Close").clicked() {
                                close = true;
                            }
                        });
                    } else {
                        ui.label(
                            RichText::new(format!("Failed — {} pages failed.", st.failed))
                                .color(theme::VERDICT_REJECT),
                        );
                        ui.add_space(6.0);
                        if ui.button("Close").clicked() {
                            close = true;
                        }
                    }
                    return;
                }

                ui.set_width(980.0);
                ui.horizontal_top(|ui| {
                    ui.vertical(|ui| {
                        ui.set_width(360.0);
                        // All settings shown inline — no scroll, nothing hidden.
                        self.contact_sheet_settings(ui, &scope_counts);
                    });

                    // A vertical separator here would expand to the full available
                    // height (the screen) and pin the window tall; use spacing.
                    ui.add_space(16.0);

                    ui.vertical(|ui| {
                        ui.set_width(580.0);
                        match &preview {
                            Some(Err(e)) => {
                                ui.label(RichText::new(e.to_string()).color(theme::VERDICT_REJECT));
                            }
                            Some(Ok(plan)) => {
                                let pages = plan.pages.len();
                                let page_idx =
                                    self.contact_sheet.preview_page.min(pages.saturating_sub(1));
                                self.contact_sheet.preview_page = page_idx;
                                self.paint_sheet_preview(ui, plan, page_idx);
                                ui.add_space(4.0);
                                ui.horizontal(|ui| {
                                    if ui.button("◀").clicked() && page_idx > 0 {
                                        self.contact_sheet.preview_page -= 1;
                                    }
                                    ui.label(format!("Page {} / {}", page_idx + 1, pages));
                                    if ui.button("▶").clicked() && page_idx + 1 < pages {
                                        self.contact_sheet.preview_page += 1;
                                    }
                                });
                                ui.add_space(4.0);
                                ui.label(RichText::new(&plan.summary).small());
                            }
                            None => {}
                        }
                    });
                });

                ui.separator();
                ui.add_space(4.0);
                let plannable = matches!(&preview, Some(Ok(_)));
                ui.add_enabled_ui(plannable, |ui| {
                    ui.horizontal(|ui| {
                        let w = (ui.available_width() - 8.0) / 2.0;
                        if ui
                            .add_sized(
                                [w, 30.0],
                                egui::Button::new(RichText::new("Export PDF…").strong()),
                            )
                            .clicked()
                        {
                            export = true;
                        }
                        if ui
                            .add_sized(
                                [w, 30.0],
                                egui::Button::new(RichText::new("Print").strong()),
                            )
                            .on_hover_text("Render, then open in the system viewer to print")
                            .clicked()
                        {
                            print = true;
                        }
                    });
                });
            });

        if export
            && let Some(path) = rfd::FileDialog::new()
                .add_filter("PDF", &["pdf"])
                .set_file_name("contact-sheet.pdf")
                .save_file()
        {
            self.start_sheet(path, false);
        }
        if print {
            self.start_sheet(print_dest(), true);
        }
        if cancel {
            self.session.cancel_contact_sheet();
        }
        if reveal && let Some(dest) = self.contact_sheet.last_dest.as_deref() {
            self.session.reveal(dest);
        }
        if !keep_open || close {
            self.contact_sheet.open = false;
            self.session.clear_contact_sheet_status();
        }
    }

    /// Render the sheet to `dest`, replanned with the real destination.
    /// `open_after` opens it in the OS viewer (the print path).
    fn start_sheet(&mut self, dest: std::path::PathBuf, open_after: bool) {
        let settings = self.contact_sheet.settings(dest.clone());
        if let Ok(plan) = self
            .session
            .plan_contact_sheet(self.contact_sheet.scope, &settings)
        {
            self.contact_sheet.last_dest = Some(dest);
            self.session.start_contact_sheet(plan, open_after);
        }
    }

    /// The left-column settings controls.
    fn contact_sheet_settings(
        &mut self,
        ui: &mut Ui,
        scope_counts: &[(dcs_app::ExportScope, &str, usize)],
    ) {
        use crate::contact_sheet::PaperKind;
        use dcs_app::{PaperOrientation, SheetBackground};

        section(ui, "Scope", |ui| {
            let current = scope_counts
                .iter()
                .find(|(s, _, _)| *s == self.contact_sheet.scope)
                .map(|(_, l, c)| format!("{l}  ·  {c}"))
                .unwrap_or_default();
            egui::ComboBox::from_id_salt("contact-sheet-scope")
                .width(240.0)
                .selected_text(current)
                .show_ui(ui, |ui| {
                    for (scope, label, count) in scope_counts {
                        ui.selectable_value(
                            &mut self.contact_sheet.scope,
                            *scope,
                            format!("{label}  ·  {count}"),
                        );
                    }
                });
        });
        section(ui, "Paper", |ui| {
            ui.horizontal_wrapped(|ui| {
                for k in PaperKind::ALL {
                    ui.radio_value(&mut self.contact_sheet.paper, k, k.label());
                }
            });
            ui.horizontal(|ui| {
                ui.radio_value(
                    &mut self.contact_sheet.orientation,
                    PaperOrientation::Landscape,
                    "Landscape",
                );
                ui.radio_value(
                    &mut self.contact_sheet.orientation,
                    PaperOrientation::Portrait,
                    "Portrait",
                );
            });
        });
        section(ui, "Background", |ui| {
            ui.horizontal(|ui| {
                ui.radio_value(
                    &mut self.contact_sheet.background,
                    SheetBackground::Black,
                    "Black",
                );
                ui.radio_value(
                    &mut self.contact_sheet.background,
                    SheetBackground::White,
                    "White",
                );
            });
        });
        section(ui, "Grid", |ui| {
            ui.add(egui::Slider::new(&mut self.contact_sheet.columns, 2..=12).text("columns"));
            ui.add(
                egui::Slider::new(&mut self.contact_sheet.margin_mm, 0.0..=40.0).text("margin mm"),
            );
            ui.add(
                egui::Slider::new(&mut self.contact_sheet.gutter_mm, 0.0..=15.0).text("gutter mm"),
            );
        });
        section(ui, "Under each frame", |ui| {
            ui.checkbox(&mut self.contact_sheet.caption.number, "Frame number");
            ui.checkbox(&mut self.contact_sheet.caption.filename, "Filename");
            ui.checkbox(&mut self.contact_sheet.caption.exposure, "Exposure");
        });
        section(ui, "Title", |ui| {
            ui.checkbox(&mut self.contact_sheet.title_on, "Header title");
            if self.contact_sheet.title_on {
                ui.add(
                    egui::TextEdit::singleline(&mut self.contact_sheet.title)
                        .desired_width(f32::INFINITY)
                        .hint_text("Roll 12 — Paris"),
                );
            }
        });
    }

    /// Paint one contact-sheet page into the dialog: the paper background, each
    /// frame's thumbnail contain-fit from the grid texture cache, the prominent
    /// frame number, and the small (clipped) detail lines — all from the same
    /// plan geometry the PDF renderer consumes, so the preview and the exported
    /// sheet agree by construction.
    fn paint_sheet_preview(
        &mut self,
        ui: &mut Ui,
        plan: &dcs_app::ContactSheetPlan,
        page_idx: usize,
    ) {
        use egui::{Color32, Sense, Stroke, StrokeKind, vec2};

        let page = &plan.pages[page_idx];
        let (pw, ph) = page.size_pt;
        let avail_w = ui.available_width().min(560.0);
        let scale = avail_w / pw;
        let (rect, _) = ui.allocate_exact_size(vec2(pw * scale, ph * scale), Sense::hover());
        let painter = ui.painter_at(rect);

        let (bg, text_col, number_col, dim_col, edge) = match page.background {
            dcs_app::SheetBackground::Black => (
                Color32::BLACK,
                Color32::from_gray(235),
                Color32::WHITE,
                Color32::from_gray(148),
                Color32::from_gray(90),
            ),
            dcs_app::SheetBackground::White => (
                Color32::WHITE,
                Color32::from_gray(38),
                Color32::BLACK,
                Color32::from_gray(128),
                Color32::from_gray(160),
            ),
        };
        painter.rect_filled(rect, 0.0, bg);

        let to_screen = |r: dcs_app::RectPt| {
            egui::Rect::from_min_size(
                rect.min + vec2(r.x * scale, r.y * scale),
                vec2(r.w * scale, r.h * scale),
            )
        };

        for cell in &page.cells {
            let img_rect = to_screen(cell.image_rect);
            let view = self.session.thumb(cell.id);
            if let Some(tex) = self.textures.view_texture(ui, cell.id, view) {
                let fit = crate::grid::contain_fit(img_rect, tex.size);
                painter.image(tex.id, fit, crate::grid::full_uv(), Color32::WHITE);
            } else {
                painter.rect_stroke(img_rect, 0.0, Stroke::new(1.0, edge), StrokeKind::Inside);
            }
            paint_caption(&painter, cell, &to_screen, scale, number_col, dim_col);
        }

        // Header: title left, app mark right (monospace, like the print). Sizes
        // use the PDF's fixed point sizes scaled to the preview, so text is in
        // the same proportion as the print.
        let hr = to_screen(page.header_rect);
        let head_size = (HEADER_FONT_PT * scale).max(5.0);
        if let Some(title) = page.title.as_ref().filter(|t| !t.is_empty()) {
            painter.text(
                hr.left_top() + vec2(1.0, 0.0),
                Align2::LEFT_TOP,
                title,
                FontId::monospace(head_size),
                text_col,
            );
        }
        painter.text(
            hr.right_top() - vec2(1.0, 0.0),
            Align2::RIGHT_TOP,
            app_mark(),
            FontId::monospace(head_size),
            text_col,
        );

        let fr = to_screen(page.footer_rect);
        painter.text(
            fr.left_top(),
            Align2::LEFT_TOP,
            &page.footer,
            FontId::monospace((FOOTER_FONT_PT * scale).max(5.0)),
            dim_col,
        );
    }
}

/// The app mark stamped into the sheet header (matches the PDF renderer).
fn app_mark() -> String {
    format!("dcs v{}", env!("CARGO_PKG_VERSION"))
}

/// Placeholder destination for the live preview (never written).
fn preview_dest() -> std::path::PathBuf {
    std::env::temp_dir().join("dcs-contact-sheet-preview.pdf")
}

/// Destination for the "Print" path — a temp PDF opened in the OS viewer.
fn print_dest() -> std::path::PathBuf {
    std::env::temp_dir().join("dcs-contact-sheet.pdf")
}

/// Paint a frame's caption in the preview, mirroring the PDF renderer: line 1 is
/// the bold mono number followed by the dimmed `(filename)`; line 2 is the dimmed
/// exposure. Monospace, clipped to the cell.
fn paint_caption(
    painter: &egui::Painter,
    cell: &dcs_app::CellPlacement,
    to_screen: &impl Fn(dcs_app::RectPt) -> egui::Rect,
    scale: f32,
    bright: egui::Color32,
    dim: egui::Color32,
) {
    if cell.caption_rect.h <= 0.0 {
        return;
    }
    let cap = to_screen(cell.caption_rect);
    let clip = painter.with_clip_rect(cap);
    let left = cap.left() + 1.0;
    let mut top = cap.top();

    if !cell.number_text.is_empty() || cell.filename.is_some() {
        let size = (6.0 * scale).clamp(5.0, 40.0);
        let mut x = left;
        if !cell.number_text.is_empty() {
            let r = clip.text(
                egui::pos2(x, top),
                Align2::LEFT_TOP,
                &cell.number_text,
                FontId::monospace(size),
                bright,
            );
            x = r.right();
        }
        if let Some(name) = &cell.filename {
            let text = if cell.number_text.is_empty() {
                dcs_app::ascii_caption(name)
            } else {
                format!(" ({})", dcs_app::ascii_caption(name))
            };
            clip.text(
                egui::pos2(x, top),
                Align2::LEFT_TOP,
                text,
                FontId::monospace(size),
                dim,
            );
        }
        top += size + 0.5;
    }
    if let Some(exp) = &cell.exposure {
        let size = (4.5 * scale).clamp(4.0, 40.0);
        clip.text(
            egui::pos2(left, top),
            Align2::LEFT_TOP,
            // Match the PDF's WinAnsi-safe transliteration so the preview reads
            // exactly like the print (`·` → `-`).
            dcs_app::ascii_caption(exp),
            FontId::monospace(size),
            dim,
        );
    }
}
