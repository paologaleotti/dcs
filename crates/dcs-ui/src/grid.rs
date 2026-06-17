//! Virtualized contact-sheet grid. Uniform square cells, contain-fit
//! thumbnails, EXIF orientation already baked by the decoder. Only the
//! rows in view are laid out (egui `show_rows`); thumbnails for the visible
//! band plus a prefetch margin are requested from the session.
//!
//! GPU textures live in a bounded LRU (`TextureCache`) rather than being
//! dropped the moment a cell leaves the viewport: scrolling back stays smooth
//! because recently-seen textures are still resident, and the ones that did
//! age out re-upload from the session's RAM pixel cache (a memcpy, not a
//! decode). The per-cell hot path allocates nothing.

use std::collections::{HashMap, HashSet};

use dcs_app::{CellInfo, Session, ThumbView};
use dcs_domain::cull::AcceptState;
use dcs_domain::grouping::GroupKind;
use dcs_domain::photo::PhotoId;
use egui::{
    Color32, FontId, Id, Pos2, Rect, Sense, Stroke, StrokeKind, TextureHandle, TextureId,
    TextureOptions, Ui, Vec2,
};

use crate::theme;

/// Rows above and below the viewport to decode ahead of the scroll.
const PREFETCH_ROWS: usize = 5;
/// Below this cell size the RAW badge is hidden (zoom-gated).
const BADGE_MIN_CELL: f32 = 96.0;
/// At or above this cell size (logical points) the grid is "zoomed in" and
/// requests sharp hi-res decodes for visible cells. Below it everything uses
/// the cheap base thumbnail, so default browsing never pays for a full decode.
const HIRES_ZOOM_CELL: f32 = 224.0;
/// Hi-res decode tiers in pixels. A zoomed cell requests the smallest tier that
/// covers its on-screen pixel size.
const TIERS: [u32; 3] = [256, 512, 1024];
/// Default VRAM budget for the grid's thumbnail textures. Larger (zoomed)
/// textures cost more, so the cache is bounded by bytes, not count.
const TEXTURE_CACHE_BYTES: u64 = 768_000_000;

/// Bounded LRU of uploaded thumbnail textures, keyed by photo, aged by frame
/// and bounded by a per-instance VRAM byte budget.
pub struct TextureCache {
    map: HashMap<PhotoId, Entry>,
    used: u64,
    frame: u64,
    budget: u64,
}

impl TextureCache {
    pub fn new() -> Self {
        Self::with_budget(TEXTURE_CACHE_BYTES)
    }

    /// A cache with an explicit VRAM budget — the gallery uses a smaller one,
    /// since it only ever holds the current frame plus a couple of neighbours
    /// (but each is far larger than a grid thumbnail).
    pub fn with_budget(budget: u64) -> Self {
        TextureCache {
            map: HashMap::new(),
            used: 0,
            frame: 0,
            budget: budget.max(1),
        }
    }

    pub fn len(&self) -> usize {
        self.map.len()
    }

    pub fn clear(&mut self) {
        self.map.clear();
        self.used = 0;
    }

    pub fn begin_frame(&mut self) {
        self.frame += 1;
    }

    /// Texture for a photo, uploading from cached pixels on first need and
    /// re-uploading when the session reports a newer version (a higher tier on
    /// zoom, or back to base on zoom-out). Touches the entry so it survives
    /// eviction. `None` until pixels exist.
    fn texture(&mut self, ui: &Ui, session: &mut Session, id: PhotoId) -> Option<TexRef> {
        let view = session.thumb(id);
        self.view_texture(ui, id, view)
    }

    /// Texture for a caller-supplied image view (e.g. the gallery's large
    /// frames), uploading on first need and re-uploading when `view.version`
    /// advances past the resident one. Touches the entry so it survives
    /// eviction. `None` until pixels exist. Lets a second `TextureCache` back the
    /// gallery from `Session::gallery_image` while the grid cache draws thumbs.
    pub fn view_texture(
        &mut self,
        ui: &Ui,
        id: PhotoId,
        view: Option<ThumbView>,
    ) -> Option<TexRef> {
        let frame = self.frame;
        // Resident at the current version → just touch it (hot path).
        if let Some(entry) = self.map.get(&id)
            && view.is_none_or(|v| v.version == entry.version)
        {
            let entry = self.map.get_mut(&id).expect("just checked it is present");
            entry.last_used = frame;
            return Some(TexRef::of(&entry.handle));
        }

        let view = view?;
        let color = egui::ColorImage::from_rgba_unmultiplied(
            [view.image.width as usize, view.image.height as usize],
            &view.image.rgba,
        );
        let handle =
            ui.ctx()
                .load_texture(format!("thumb-{}", id.0), color, TextureOptions::LINEAR);
        let weight = view.image.width as u64 * view.image.height as u64 * 4;
        let tref = TexRef::of(&handle);
        let replaced = self.map.insert(
            id,
            Entry {
                last_used: frame,
                version: view.version,
                weight,
                handle,
            },
        );
        if let Some(old) = replaced {
            self.used -= old.weight;
        }
        self.used += weight;
        self.evict_over_budget();
        Some(tref)
    }

    pub fn evict_over_budget(&mut self) {
        while self.used > self.budget && self.map.len() > 1 {
            let Some(victim) = self
                .map
                .iter()
                .min_by_key(|(_, e)| e.last_used)
                .map(|(id, _)| *id)
            else {
                break;
            };
            if let Some(removed) = self.map.remove(&victim) {
                self.used -= removed.weight;
            }
        }
    }
}

impl Default for TextureCache {
    fn default() -> Self {
        Self::new()
    }
}

struct Entry {
    last_used: u64,
    version: u64,
    weight: u64,
    handle: TextureHandle,
}

#[derive(Clone, Copy)]
pub struct TexRef {
    pub id: TextureId,
    pub size: Vec2,
}

impl TexRef {
    fn of(handle: &TextureHandle) -> Self {
        TexRef {
            id: handle.id(),
            size: handle.size_vec2(),
        }
    }
}

/// Row pitch in points: the cell edge plus the inter-cell gap. The one
/// source of this formula — both the painter and the app's auto-scroll math use
/// it, so the grid geometry can never drift between them.
pub fn row_stride(cell: f32) -> f32 {
    cell + (cell * 0.1).max(4.0)
}

/// Header band height in points — a quiet edge annotation, not a cell row.
const HEADER_H: f32 = 30.0;

/// What the grid reports back: cells drawn (diagnostics) and the column count
/// the app's keyboard nav uses for `↑↓` row moves.
pub struct GridResponse {
    pub visible: usize,
    pub cols: usize,
}

/// Paint the grouped, virtualized grid. `cell` is the square cell edge; the grid
/// is segmented by the session's derived groups, each with a header band
/// then its cells flowing in rows of `cols`. Only the rows intersecting the
/// viewport are painted. When `scroll_to_focus` is set, the focus cell is
/// scrolled into view (after a keyboard nav move).
pub fn show(
    ui: &mut Ui,
    session: &mut Session,
    textures: &mut TextureCache,
    cell: f32,
    view_width: f32,
    scroll_to_focus: bool,
    collapsed: &mut HashSet<String>,
) -> GridResponse {
    let count = session.photo_count();
    if count == 0 {
        return GridResponse {
            visible: 0,
            cols: 1,
        };
    }
    textures.begin_frame();

    let stride = row_stride(cell);
    let gap = stride - cell;
    let cols = (((view_width + gap) / stride).floor() as usize).max(1);

    // Default browsing uses only the cheap base thumbnail. Once zoomed in,
    // visible cells additionally request a sharp decode sized to device pixels.
    let zoomed = cell >= HIRES_ZOOM_CELL;
    let hires_edge = tier_for(cell * ui.ctx().pixels_per_point());
    if !zoomed {
        session.clear_hires();
    }
    let focus = session.focus();
    // Drop collapse entries for titles no longer present (regroup/sort/filter
    // changed the group set) so the set can't grow without bound across changes.
    if !collapsed.is_empty() {
        let live: HashSet<&str> = session.groups().iter().map(|g| g.title.as_str()).collect();
        collapsed.retain(|t| live.contains(t.as_str()));
    }
    let layout = Layout::build(group_inputs(session, collapsed), cols, stride);
    let mut visible = 0usize;
    let mut clicked: Option<usize> = None;
    // Header title to flip collapse on, applied after the paint borrow ends.
    let mut toggle: Option<String> = None;

    ui.spacing_mut().item_spacing = Vec2::ZERO;
    egui::ScrollArea::vertical()
        .auto_shrink([false, false])
        .show_viewport(ui, |ui, viewport| {
            let (rect, _) =
                ui.allocate_exact_size(Vec2::new(view_width, layout.total), Sense::hover());
            let origin = rect.min;

            if scroll_to_focus
                && let Some(f) = focus
                && let Some(r) = layout.cell_rect(f, origin, cell, stride)
            {
                // Jump straight to the focus cell — the animated glide across a
                // long list reads as jank, not feedback.
                ui.scroll_to_rect_animation(
                    r,
                    Some(egui::Align::Center),
                    egui::style::ScrollAnimation::none(),
                );
            }

            let resp = ui.interact(rect, Id::new("dcs_grid"), Sense::click());
            let hover_pos = ui.input(|i| i.pointer.hover_pos());
            let click_pos = resp
                .clicked()
                .then(|| resp.interact_pointer_pos())
                .flatten();

            let (first, last) = layout.visible_rows(viewport.min.y, viewport.max.y);
            for r in first..last {
                let y = origin.y + layout.offsets[r];
                match &layout.rows[r] {
                    Row::Header(h) => {
                        let info = &layout.headers[*h];
                        let hrect = Rect::from_min_size(
                            Pos2::new(origin.x, y),
                            Vec2::new(view_width, HEADER_H),
                        );
                        if click_pos.is_some_and(|p| hrect.contains(p)) {
                            toggle = Some(info.title.clone());
                        }
                        let hovered = hover_pos.is_some_and(|p| hrect.contains(p));
                        paint_header(ui, info, hrect, hovered);
                    }
                    Row::Cells { start, len } => {
                        for c in 0..*len {
                            let idx = start + c;
                            let Some(info) = session.cell_info(idx) else {
                                continue;
                            };
                            if zoomed {
                                session.request_hires(idx, hires_edge);
                            }
                            let cell_rect = Rect::from_min_size(
                                Pos2::new(origin.x + c as f32 * stride, y),
                                Vec2::splat(cell),
                            );
                            if click_pos.is_some_and(|p| cell_rect.contains(p)) {
                                clicked = Some(idx);
                            }
                            paint_cell(ui, session, textures, info, focus == Some(idx), cell_rect);
                            visible += 1;
                        }
                    }
                }
            }
            // Prefetch the rows around the viewport so scrolling stays smooth.
            // Walking rows (not a min/max index span) keeps collapsed groups
            // cheap: a collapsed group contributes only its single cover cell,
            // never the hidden run between covers.
            let pf_first = first.saturating_sub(PREFETCH_ROWS);
            let pf_last = (last + PREFETCH_ROWS).min(layout.rows.len());
            for r in pf_first..pf_last {
                if let Row::Cells { start, len } = layout.rows[r] {
                    for idx in start..start + len {
                        session.request_base(idx);
                    }
                }
            }
        });

    // The UI only reports the raw click + modifiers; the app owns the policy.
    if let Some(idx) = clicked {
        let (shift, cmd) = ui.input(|i| (i.modifiers.shift, i.modifiers.command));
        session.pointer_select(idx, shift, cmd);
    }
    // A clicked header flips its collapse state (ephemeral UI, keyed by title).
    if let Some(title) = toggle
        && !collapsed.remove(&title)
    {
        collapsed.insert(title);
    }

    textures.evict_over_budget();
    GridResponse { visible, cols }
}

/// Pre-computed visual layout: the ordered header/cell rows with their vertical
/// offsets, so the viewport band can be found by binary search and the focus
/// cell located for auto-scroll. Owns its header text so painting touches no
/// session state (which the per-cell loop mutates).
struct Layout {
    rows: Vec<Row>,
    /// `offsets[r]` = top of row `r`; `offsets[rows.len()]` = total height.
    offsets: Vec<f32>,
    total: f32,
    headers: Vec<HeaderInfo>,
}

#[derive(Clone, Copy)]
enum Row {
    Header(usize),
    Cells { start: usize, len: usize },
}

struct HeaderInfo {
    title: String,
    count: usize,
    total: usize,
    collapsed: bool,
}

/// A group prepared for layout: its span plus whether it's collapsed and, if so,
/// the cover cell to stand in for it (first accepted, else first).
struct GroupInput {
    title: String,
    kind: GroupKind,
    start: usize,
    count: usize,
    total: usize,
    collapsed: bool,
    cover: usize,
}

/// Resolve the per-group layout inputs from the session and the collapse set.
/// Reads verdicts (for the cover) before the mutable paint loop borrows.
fn group_inputs(session: &Session, collapsed: &HashSet<String>) -> Vec<GroupInput> {
    session
        .groups()
        .iter()
        .map(|g| {
            let collapsible = g.kind != GroupKind::Stream;
            let is_collapsed = collapsible && collapsed.contains(&g.title);
            let cover = if is_collapsed {
                session.group_cover(g)
            } else {
                g.start
            };
            GroupInput {
                title: g.title.clone(),
                kind: g.kind,
                start: g.start,
                count: g.count,
                total: g.total,
                collapsed: is_collapsed,
                cover,
            }
        })
        .collect()
}

impl Layout {
    fn build(groups: Vec<GroupInput>, cols: usize, stride: f32) -> Layout {
        let mut rows = Vec::new();
        let mut headers = Vec::new();
        for g in groups {
            // The single `none`-axis stream has no header.
            if g.kind != GroupKind::Stream {
                headers.push(HeaderInfo {
                    title: g.title,
                    count: g.count,
                    total: g.total,
                    collapsed: g.collapsed,
                });
                rows.push(Row::Header(headers.len() - 1));
            }
            if g.collapsed {
                // Collapsed: a single cover cell stands in for the group.
                rows.push(Row::Cells {
                    start: g.cover,
                    len: 1,
                });
                continue;
            }
            let mut c = 0;
            while c < g.count {
                let len = (g.count - c).min(cols);
                rows.push(Row::Cells {
                    start: g.start + c,
                    len,
                });
                c += len;
            }
        }
        let mut offsets = Vec::with_capacity(rows.len() + 1);
        let mut y = 0.0;
        for row in &rows {
            offsets.push(y);
            y += match row {
                Row::Header(_) => HEADER_H,
                Row::Cells { .. } => stride,
            };
        }
        offsets.push(y);
        Layout {
            rows,
            offsets,
            total: y,
            headers,
        }
    }

    /// The half-open row range intersecting the vertical viewport `[top, bot]`.
    fn visible_rows(&self, top: f32, bot: f32) -> (usize, usize) {
        // First row whose bottom is past `top`; last row whose top is before `bot`.
        let first = self
            .offsets
            .partition_point(|&y| y <= top)
            .saturating_sub(1);
        let last = self.offsets.partition_point(|&y| y < bot);
        (first.min(self.rows.len()), last.min(self.rows.len()))
    }

    /// Screen rect of the cell at display index `idx`, for auto-scroll.
    fn cell_rect(&self, idx: usize, origin: Pos2, cell: f32, stride: f32) -> Option<Rect> {
        for (r, row) in self.rows.iter().enumerate() {
            if let Row::Cells { start, len } = row
                && idx >= *start
                && idx < start + len
            {
                let col = idx - start;
                return Some(Rect::from_min_size(
                    Pos2::new(origin.x + col as f32 * stride, origin.y + self.offsets[r]),
                    Vec2::splat(cell),
                ));
            }
        }
        None
    }
}

/// A collapse caret painted as a small triangle (no font glyph, so it always
/// renders): pointing right when collapsed, down when expanded.
fn paint_caret(p: &egui::Painter, center: Pos2, collapsed: bool) {
    let r = 4.0;
    let pts = if collapsed {
        vec![
            Pos2::new(center.x - r * 0.6, center.y - r),
            Pos2::new(center.x + r * 0.7, center.y),
            Pos2::new(center.x - r * 0.6, center.y + r),
        ]
    } else {
        vec![
            Pos2::new(center.x - r, center.y - r * 0.6),
            Pos2::new(center.x + r, center.y - r * 0.6),
            Pos2::new(center.x, center.y + r * 0.7),
        ]
    };
    p.add(egui::Shape::convex_polygon(
        pts,
        theme::TEXT_DIM,
        Stroke::NONE,
    ));
}

/// A group header: a charcoal band distinct from the sheet, a
/// collapse caret, the title in sans, and a mono `shown of total` count — an
/// edge annotation that's also the click target for collapsing.
fn paint_header(ui: &Ui, info: &HeaderInfo, rect: Rect, hovered: bool) {
    let p = ui.painter();
    // The band reads as chrome over the lighter sheet, brighter on hover.
    let band = if hovered {
        Color32::from_gray(18)
    } else {
        theme::CHROME_BG
    };
    p.rect_filled(rect, 0.0, band);
    p.hline(
        rect.x_range(),
        rect.top() + 0.5,
        Stroke::new(1.0, theme::HAIRLINE),
    );
    let cy = rect.center().y;
    paint_caret(p, Pos2::new(rect.left() + 10.0, cy), info.collapsed);
    let title_color = if hovered {
        theme::FOCUS_OUTLINE
    } else {
        theme::SELECT_OUTLINE
    };
    p.text(
        Pos2::new(rect.left() + 24.0, cy),
        egui::Align2::LEFT_CENTER,
        &info.title,
        FontId::proportional(14.0),
        title_color,
    );
    let count = if info.count == info.total {
        format!("{}", info.total)
    } else {
        format!("{} of {}", info.count, info.total)
    };
    p.text(
        Pos2::new(rect.right() - 8.0, cy),
        egui::Align2::RIGHT_CENTER,
        count,
        FontId::monospace(11.0),
        theme::TEXT_DIM,
    );
}

/// Smallest decode tier covering `px` on-screen pixels.
fn tier_for(px: f32) -> u32 {
    let px = px.ceil() as u32;
    *TIERS
        .iter()
        .find(|&&t| t >= px)
        .unwrap_or(TIERS.last().expect("TIERS is non-empty"))
}

fn paint_cell(
    ui: &mut Ui,
    session: &mut Session,
    textures: &mut TextureCache,
    info: CellInfo,
    focused: bool,
    cell_rect: Rect,
) {
    ui.painter().rect_filled(cell_rect, 0.0, theme::CELL_EMPTY);

    if !info.missing
        && let Some(tex) = textures.texture(ui, session, info.id)
    {
        let fit = contain_fit(cell_rect, tex.size);
        ui.painter().image(tex.id, fit, full_uv(), Color32::WHITE);
    }

    // A missing file has no pixels — show a placeholder so its preserved
    // verdict still reads, rather than a blank or a stale thumbnail.
    if info.missing {
        paint_missing(ui, cell_rect);
    }

    // Rejected cells dim, then carry the glyph on top so it stays legible.
    if info.state == AcceptState::Rejected {
        ui.painter().rect_filled(cell_rect, 0.0, theme::REJECT_DIM);
    }

    if info.raw_only && cell_rect.width() >= BADGE_MIN_CELL {
        paint_raw_badge(ui, cell_rect);
    }

    paint_verdict_glyph(ui, cell_rect, info.state);

    // Selection first, focus on top and brighter — a focused cell that is also
    // selected reads as the focus.
    if info.selected {
        ui.painter().rect_stroke(
            cell_rect,
            0.0,
            Stroke::new(1.0, theme::SELECT_OUTLINE),
            StrokeKind::Inside,
        );
    }
    if focused {
        ui.painter().rect_stroke(
            cell_rect,
            0.0,
            Stroke::new(2.0, theme::FOCUS_OUTLINE),
            StrokeKind::Inside,
        );
    }
}

/// Bottom-right verdict glyph: a green check (accepted) or red cross
/// (rejected); nothing for unreviewed. Drawn as line segments rather than font
/// glyphs so it renders identically regardless of the loaded font.
fn paint_verdict_glyph(ui: &Ui, cell_rect: Rect, state: AcceptState) {
    let color = match state {
        AcceptState::Accepted => theme::VERDICT_ACCEPT,
        AcceptState::Rejected => theme::VERDICT_REJECT,
        AcceptState::Unreviewed => return,
    };
    let s = (cell_rect.width() * 0.2).clamp(14.0, 26.0);
    let box_rect = Rect::from_min_max(
        Pos2::new(cell_rect.right() - s, cell_rect.bottom() - s),
        cell_rect.max,
    );
    ui.painter().rect_filled(box_rect, 0.0, theme::BADGE_BG);
    let stroke = Stroke::new((s * 0.12).max(1.5), color);
    let c = box_rect.center();
    let r = s * 0.28;
    match state {
        AcceptState::Accepted => {
            // One polyline so the elbow joins cleanly (two segments blob).
            let pts = vec![
                Pos2::new(c.x - r, c.y),
                Pos2::new(c.x - r * 0.25, c.y + r * 0.7),
                Pos2::new(c.x + r, c.y - r * 0.6),
            ];
            ui.painter().add(egui::Shape::line(pts, stroke));
        }
        AcceptState::Rejected => {
            ui.painter().line_segment(
                [Pos2::new(c.x - r, c.y - r), Pos2::new(c.x + r, c.y + r)],
                stroke,
            );
            ui.painter().line_segment(
                [Pos2::new(c.x + r, c.y - r), Pos2::new(c.x - r, c.y + r)],
                stroke,
            );
        }
        AcceptState::Unreviewed => {}
    }
}

/// Placeholder for a missing file: a hairline outline plus a `missing`
/// label when the cell is large enough, else a small corner badge.
fn paint_missing(ui: &Ui, cell_rect: Rect) {
    ui.painter().rect_stroke(
        cell_rect,
        0.0,
        Stroke::new(1.0, theme::HAIRLINE),
        StrokeKind::Inside,
    );
    if cell_rect.width() >= BADGE_MIN_CELL {
        ui.painter().text(
            cell_rect.center(),
            egui::Align2::CENTER_CENTER,
            "missing",
            FontId::monospace((cell_rect.width() * 0.12).clamp(10.0, 16.0)),
            theme::TEXT_DIM,
        );
    } else {
        let size = (cell_rect.width() * 0.16).clamp(12.0, 18.0);
        let badge = Rect::from_min_size(cell_rect.min, Vec2::splat(size));
        ui.painter().rect_filled(badge, 0.0, theme::BADGE_BG);
        ui.painter().text(
            badge.center(),
            egui::Align2::CENTER_CENTER,
            "!",
            FontId::monospace(size * 0.7),
            theme::TEXT_DIM,
        );
    }
}

fn paint_raw_badge(ui: &Ui, cell_rect: Rect) {
    let size = (cell_rect.width() * 0.16).clamp(14.0, 22.0);
    let badge = Rect::from_min_size(cell_rect.min, Vec2::splat(size));
    ui.painter().rect_filled(badge, 0.0, theme::BADGE_BG);
    ui.painter().text(
        badge.center(),
        egui::Align2::CENTER_CENTER,
        "R",
        FontId::monospace(size * 0.7),
        theme::TEXT_DIM,
    );
}

/// Largest rect of the texture's aspect ratio that fits inside `outer`,
/// centered — the contain-fit used by both the grid cell and the gallery frame.
pub fn contain_fit(outer: Rect, size: Vec2) -> Rect {
    let scale = (outer.width() / size.x).min(outer.height() / size.y);
    let fitted = Vec2::new(size.x * scale, size.y * scale);
    Rect::from_center_size(outer.center(), fitted)
}

/// The full `[0,0]–[1,1]` UV rect for painting a whole texture.
pub fn full_uv() -> Rect {
    Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0))
}
