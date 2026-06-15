//! The analog contact-sheet look (§3): dark neutral grays, square corners
//! everywhere, hairline separation, monospace for data. No blue-tinted darks,
//! no rounding, no shadows.

use egui::{Color32, Context, CornerRadius, Stroke, Vec2, Visuals};

/// Empty cell area — the "sheet" surface, slightly lighter than chrome.
pub const SHEET_BG: Color32 = Color32::from_gray(30);
/// Panels, bars, menus.
pub const CHROME_BG: Color32 = Color32::from_gray(18);
/// A cell with no thumbnail yet.
pub const CELL_EMPTY: Color32 = Color32::from_gray(38);
/// 1 px separators.
pub const HAIRLINE: Color32 = Color32::from_gray(64);
/// Secondary text and key hints.
pub const TEXT_DIM: Color32 = Color32::from_gray(150);
/// RAW badge background.
pub const BADGE_BG: Color32 = Color32::from_gray(12);

pub fn apply(ctx: &Context) {
    let mut v = Visuals::dark();
    squareify(&mut v);

    v.panel_fill = CHROME_BG;
    v.window_fill = CHROME_BG;
    v.extreme_bg_color = Color32::from_gray(10);
    v.faint_bg_color = Color32::from_gray(24);
    v.widgets.noninteractive.bg_stroke = Stroke::new(1.0, HAIRLINE);
    v.window_stroke = Stroke::new(1.0, HAIRLINE);
    // Selection reads as a grease-pencil outline, not a brand tint.
    v.selection.bg_fill = Color32::from_gray(70);
    v.selection.stroke = Stroke::new(1.0, Color32::from_gray(200));

    ctx.set_visuals(v);
    ctx.global_style_mut(|s| {
        s.spacing.item_spacing = Vec2::new(6.0, 6.0);
        s.spacing.button_padding = Vec2::new(8.0, 4.0);
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
