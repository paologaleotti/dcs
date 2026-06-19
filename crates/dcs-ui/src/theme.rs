//! The analog contact-sheet look: dark neutral grays, square corners
//! everywhere, hairline separation, monospace for data. No blue-tinted darks,
//! no rounding, no shadows.

use egui::{Color32, Context, CornerRadius, Stroke, Vec2, Visuals};

/// An sRGB tag color as an egui color — the one place tag colors cross into the
/// UI, so strips, chips, and band rules all map identically.
pub fn tag_color32(c: dcs_domain::tag::Color) -> Color32 {
    Color32::from_rgb(c.r, c.g, c.b)
}

/// Empty cell area — the "sheet" surface, slightly lighter than chrome.
pub const SHEET_BG: Color32 = Color32::from_gray(20);
/// Panels, bars, menus.
pub const CHROME_BG: Color32 = Color32::from_gray(11);
/// A cell with no thumbnail yet.
pub const CELL_EMPTY: Color32 = Color32::from_gray(28);
/// 1 px separators.
pub const HAIRLINE: Color32 = Color32::from_gray(54);
/// Secondary text and key hints.
pub const TEXT_DIM: Color32 = Color32::from_gray(150);
/// RAW badge background, and the chip behind a verdict glyph.
pub const BADGE_BG: Color32 = Color32::from_gray(8);

/// Selection — a light grease-pencil outline.
pub const SELECT_OUTLINE: Color32 = Color32::from_gray(180);
/// Focus cursor — a brighter outline than the selection.
pub const FOCUS_OUTLINE: Color32 = Color32::from_gray(240);
/// Rejected cells are dimmed by this translucent black overlay.
pub const REJECT_DIM: Color32 = Color32::from_black_alpha(130);
/// Accepted verdict mark. Green/red verdict marks are the only non-gray colors
/// so far (color = meaning only).
pub const VERDICT_ACCEPT: Color32 = Color32::from_rgb(90, 190, 110);
/// Rejected verdict mark.
pub const VERDICT_REJECT: Color32 = Color32::from_rgb(210, 90, 90);

/// Burst span accent — the single muted, warm neutral painted behind a run of
/// rapid-fire frames. One of the three meaning-bearing colors (tags, verdict,
/// burst); kept low-saturation so it reads as a band, not a tag, and evokes the
/// film rebate the design language leans on.
pub const BURST_SPAN: Color32 = Color32::from_rgb(74, 64, 46);
/// The burst run's count label, a brighter tint of the span accent.
pub const BURST_LABEL: Color32 = Color32::from_rgb(198, 172, 120);

/// "You are filtered" accent — a muted slate marking the filter bar, its rule,
/// and the `N of M` count, so a narrowed grid reads at a glance. A UI-state
/// signal, deliberately cool and off to the side of the tag/verdict/burst hues.
pub const FILTER_ACCENT: Color32 = Color32::from_rgb(96, 124, 148);

pub fn apply(ctx: &Context) {
    let mut v = Visuals::dark();
    squareify(&mut v);

    v.panel_fill = CHROME_BG;
    v.window_fill = CHROME_BG;
    v.extreme_bg_color = Color32::from_gray(6);
    v.faint_bg_color = Color32::from_gray(16);
    v.widgets.noninteractive.bg_stroke = Stroke::new(1.0, HAIRLINE);
    v.window_stroke = Stroke::new(1.0, HAIRLINE);
    // Selection reads as a grease-pencil outline, not a brand tint.
    v.selection.bg_fill = Color32::from_gray(70);
    v.selection.stroke = Stroke::new(1.0, Color32::from_gray(200));

    ctx.set_visuals(v);
    ctx.global_style_mut(|s| {
        s.spacing.item_spacing = Vec2::new(6.0, 6.0);
        s.spacing.button_padding = Vec2::new(8.0, 4.0);
        // Chrome text is a label, not content — no drag-to-select.
        s.interaction.selectable_labels = false;
    });
}

fn squareify(v: &mut Visuals) {
    let z = CornerRadius::ZERO;
    v.widgets.noninteractive.corner_radius = z;
    v.widgets.inactive.corner_radius = z;
    v.widgets.hovered.corner_radius = z;
    v.widgets.active.corner_radius = z;
    v.widgets.open.corner_radius = z;
    v.window_corner_radius = z;
    v.menu_corner_radius = z;
}
