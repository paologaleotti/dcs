//! A keyboard-first quick-pick overlay — a fuzzy search field over a scrollable
//! result list, driven by `↑`/`↓`/`Enter`/`Esc`, mouse optional. This is the
//! reusable foundation for every searchable select in dcs: the shoot-timezone
//! picker today, the command palette (`Cmd+P`, §2.10) and the tag palette
//! (§2.7) next — one component so they all share nav, fuzzy highlighting, and
//! feel. Pure presentation: it owns only ephemeral UI state (query, cursor) and
//! reports the chosen item back by index; the caller maps that to its domain.
//!
//! Filtering and ranking come from `dcs_domain::fuzzy` (the pure core), so the
//! match behaviour is identical wherever a picker appears and is unit-tested
//! once, in the domain.

use dcs_domain::fuzzy::fuzzy_match;
use egui::{
    Align, Align2, Color32, FontId, Key, Modifiers, RichText, ScrollArea, Sense, TextEdit,
    TextFormat, Ui, Vec2, text::LayoutJob,
};

use crate::theme;

/// One selectable row. `label` is fuzzy-matched and highlighted; `detail` is
/// optional right-aligned dim text (a key hint for the command palette, an
/// offset for a zone, …).
#[derive(Clone, Copy)]
pub struct PickerItem<'a> {
    pub label: &'a str,
    pub detail: Option<&'a str>,
}

impl<'a> PickerItem<'a> {
    /// A bare label with no trailing detail.
    pub fn new(label: &'a str) -> Self {
        PickerItem {
            label,
            detail: None,
        }
    }
}

/// What the picker did this frame.
pub enum PickerEvent {
    /// Chose the item at this index into the caller's `items` slice.
    Picked(usize),
    /// Dismissed without choosing (`Esc`, or clicking the close affordance).
    Dismissed,
    /// Still open, nothing chosen.
    Pending,
}

/// Reusable quick-pick state. One instance per logical picker (the app holds a
/// `Picker` for the shoot zone; a future command palette holds another).
pub struct Picker {
    title: String,
    query: String,
    /// Highlighted row within the *filtered* list, not the input slice.
    cursor: usize,
    open: bool,
    /// Set on open so the next frame focuses the search field and resets scroll.
    just_opened: bool,
}

impl Picker {
    /// A closed picker with the given window title.
    pub fn new(title: impl Into<String>) -> Self {
        Picker {
            title: title.into(),
            query: String::new(),
            cursor: 0,
            open: false,
            just_opened: false,
        }
    }

    pub fn is_open(&self) -> bool {
        self.open
    }

    /// Open fresh: clear the query, reset the cursor, and grab keyboard focus
    /// next frame.
    pub fn open(&mut self) {
        self.open = true;
        self.just_opened = true;
        self.query.clear();
        self.cursor = 0;
    }

    /// Render the picker and report what happened. No-op returning `Pending`
    /// when closed. `subtitle` is a small dim line under the search field (e.g.
    /// the freeze-critical note, the current value).
    ///
    /// Returns the index into `items` of the chosen row — `items` is the
    /// caller's full list each frame; the picker re-filters it internally.
    pub fn show(
        &mut self,
        ctx: &egui::Context,
        subtitle: Option<&str>,
        hint: &str,
        items: &[PickerItem<'_>],
    ) -> PickerEvent {
        if !self.open {
            return PickerEvent::Pending;
        }

        // Fuzzy-match the query, best score first, ties by original order.
        let mut ranked: Vec<(usize, dcs_domain::fuzzy::FuzzyMatch)> = items
            .iter()
            .enumerate()
            .filter_map(|(i, it)| fuzzy_match(it.label, &self.query).map(|m| (i, m)))
            .collect();
        ranked.sort_by(|a, b| b.1.score.cmp(&a.1.score).then(a.0.cmp(&b.0)));

        self.cursor = self.cursor.min(ranked.len().saturating_sub(1));
        let nav = self.consume_nav_keys(ctx, ranked.len());
        match nav {
            Nav::Dismiss => {
                self.open = false;
                return PickerEvent::Dismissed;
            }
            Nav::Accept => {
                if let Some(&(idx, _)) = ranked.get(self.cursor) {
                    self.open = false;
                    return PickerEvent::Picked(idx);
                }
            }
            Nav::Move | Nav::None => {}
        }
        // On reopen, force the scroll back to the top so it agrees with cursor 0.
        let opened_fresh = self.just_opened;
        let scroll_to_cursor = matches!(nav, Nav::Move) || opened_fresh;
        // Hover claims the cursor only while the pointer moves, so a resting
        // mouse doesn't fight the arrow keys.
        let pointer_moving = ctx.input(|i| i.pointer.delta() != Vec2::ZERO);

        let mut event = PickerEvent::Pending;
        let mut window_open = true;
        egui::Window::new(&self.title)
            .collapsible(false)
            .resizable(true)
            .default_size([420.0, 460.0])
            .anchor(Align2::CENTER_CENTER, [0.0, 0.0])
            .open(&mut window_open)
            .show(ctx, |ui| {
                if let Some(sub) = subtitle {
                    ui.label(
                        RichText::new(sub)
                            .monospace()
                            .size(11.0)
                            .color(theme::TEXT_DIM),
                    );
                }
                let search = ui.add(
                    TextEdit::singleline(&mut self.query)
                        .hint_text(hint)
                        .desired_width(f32::INFINITY)
                        .font(FontId::monospace(14.0)),
                );
                // Keep the field focused so typing always lands in the query.
                if self.just_opened {
                    search.request_focus();
                    self.just_opened = false;
                } else if !search.has_focus() {
                    search.request_focus();
                }

                ui.add_space(2.0);
                ui.label(
                    RichText::new(format!("{} / {}", ranked.len(), items.len()))
                        .monospace()
                        .size(10.0)
                        .color(theme::HAIRLINE),
                );
                ui.separator();

                if ranked.is_empty() {
                    ui.add_space(8.0);
                    ui.label(
                        RichText::new("no matches")
                            .monospace()
                            .color(theme::TEXT_DIM),
                    );
                    return;
                }

                let row_h = ui.text_style_height(&egui::TextStyle::Monospace) + 6.0;
                let mut area = ScrollArea::vertical().auto_shrink([false, false]);
                if opened_fresh {
                    area = area.vertical_scroll_offset(0.0);
                }
                area.show_rows(ui, row_h, ranked.len(), |ui, range| {
                    for r in range {
                        let (idx, m) = &ranked[r];
                        let row = Row {
                            item: items[*idx],
                            hits: &m.positions,
                            index: r,
                            want_scroll: scroll_to_cursor && r == self.cursor,
                            pointer_moving,
                        };
                        if self.row(ui, row_h, row) {
                            event = PickerEvent::Picked(*idx);
                        }
                    }
                });
            });

        if !window_open {
            self.open = false;
            return PickerEvent::Dismissed;
        }
        if matches!(event, PickerEvent::Picked(_)) {
            self.open = false;
        }
        event
    }

    /// One result row: a full-width clickable strip, highlighted when it is the
    /// keyboard cursor, with fuzzy hits brightened and an optional right detail.
    /// A moving pointer hovering the row claims the cursor so mouse and keyboard
    /// agree. Returns whether it was clicked.
    fn row(&mut self, ui: &mut Ui, h: f32, row: Row<'_>) -> bool {
        let on = row.index == self.cursor;
        let (rect, resp) =
            ui.allocate_exact_size(Vec2::new(ui.available_width(), h), Sense::click());
        if row.want_scroll {
            resp.scroll_to_me(Some(Align::Center));
        }
        if resp.hovered() && row.pointer_moving {
            self.cursor = row.index;
        }
        if on {
            ui.painter().rect_filled(rect, 0.0, theme::CELL_EMPTY);
        }
        let text_color = if on {
            theme::FOCUS_OUTLINE
        } else {
            theme::TEXT_DIM
        };
        let job = highlight(row.item.label, row.hits, text_color);
        let galley = ui.painter().layout_job(job);
        let text_pos = egui::pos2(rect.left() + 6.0, rect.center().y - galley.size().y / 2.0);
        ui.painter().galley(text_pos, galley, text_color);

        if let Some(detail) = row.item.detail {
            // Key hints (and picker detail) read at a glance — dim, not invisible.
            let detail_color = if on {
                theme::SELECT_OUTLINE
            } else {
                theme::TEXT_DIM
            };
            ui.painter().text(
                egui::pos2(rect.right() - 6.0, rect.center().y),
                Align2::RIGHT_CENTER,
                detail,
                FontId::monospace(11.0),
                detail_color,
            );
        }
        resp.clicked()
    }

    /// Read and consume the navigation keys so they never leak to the grid or
    /// move the text cursor. Wraps top↔bottom on `↑`/`↓`.
    fn consume_nav_keys(&mut self, ctx: &egui::Context, len: usize) -> Nav {
        ctx.input_mut(|i| {
            if i.consume_key(Modifiers::NONE, Key::Escape) {
                return Nav::Dismiss;
            }
            if i.consume_key(Modifiers::NONE, Key::Enter) {
                return Nav::Accept;
            }
            let mut moved = false;
            if len > 0 {
                if i.consume_key(Modifiers::NONE, Key::ArrowDown) {
                    self.cursor = (self.cursor + 1) % len;
                    moved = true;
                }
                if i.consume_key(Modifiers::NONE, Key::ArrowUp) {
                    self.cursor = (self.cursor + len - 1) % len;
                    moved = true;
                }
                if i.consume_key(Modifiers::NONE, Key::Home) {
                    self.cursor = 0;
                    moved = true;
                }
                if i.consume_key(Modifiers::NONE, Key::End) {
                    self.cursor = len - 1;
                    moved = true;
                }
            }
            if moved { Nav::Move } else { Nav::None }
        })
    }
}

enum Nav {
    Dismiss,
    Accept,
    Move,
    None,
}

/// Per-frame render inputs for a single result row.
struct Row<'a> {
    item: PickerItem<'a>,
    hits: &'a [usize],
    index: usize,
    /// This is the keyboard cursor and a key move just happened — scroll it in.
    want_scroll: bool,
    /// The pointer moved this frame, so hover may claim the cursor.
    pointer_moving: bool,
}

/// Build a layout job that brightens the fuzzy-matched chars and dims the rest,
/// grouping consecutive runs so short labels stay cheap to lay out.
fn highlight(label: &str, hits: &[usize], base: Color32) -> LayoutJob {
    let mut job = LayoutJob::default();
    let font = FontId::monospace(13.0);
    let mut run = String::new();
    let mut run_hit = false;
    let mut hit_at = 0usize;
    for (i, ch) in label.chars().enumerate() {
        let is_hit = hit_at < hits.len() && hits[hit_at] == i;
        if is_hit {
            hit_at += 1;
        }
        if !run.is_empty() && is_hit != run_hit {
            push_run(&mut job, &run, run_hit, &font, base);
            run.clear();
        }
        run_hit = is_hit;
        run.push(ch);
    }
    if !run.is_empty() {
        push_run(&mut job, &run, run_hit, &font, base);
    }
    job
}

fn push_run(job: &mut LayoutJob, text: &str, hit: bool, font: &FontId, base: Color32) {
    let color = if hit { theme::VERDICT_ACCEPT } else { base };
    job.append(
        text,
        0.0,
        TextFormat {
            font_id: font.clone(),
            color,
            ..Default::default()
        },
    );
}
