//! The analog contact-sheet look: dark neutral grays, square corners
//! everywhere, hairline separation. A clean grotesque (Inter) carries the
//! chrome; a mono (JetBrains Mono) carries data — counts, filenames, badges.
//! No blue-tinted darks, no rounding, no shadows.

use std::sync::Arc;

use egui::{
    Color32, Context, CornerRadius, FontData, FontDefinitions, FontFamily, FontId, Margin, Stroke,
    Vec2, Visuals,
};

/// An sRGB tag color as an egui color — the one place tag colors cross into the
/// UI, so strips, chips, and band rules all map identically.
pub fn tag_color32(c: dcs_domain::tag::Color) -> Color32 {
    Color32::from_rgb(c.r, c.g, c.b)
}

/// Empty cell area — the "sheet" surface, slightly lighter than chrome.
pub const SHEET_BG: Color32 = Color32::from_gray(20);
/// Panels, bars, menus. Kept deep so chrome controls read against it.
pub const CHROME_BG: Color32 = Color32::from_gray(9);
/// A cell with no thumbnail yet.
pub const CELL_EMPTY: Color32 = Color32::from_gray(28);
/// 1 px separators — crisp enough to actually divide, not disappear.
pub const HAIRLINE: Color32 = Color32::from_gray(72);
/// The deepest surface — recessed wells like the segmented-control track, and
/// egui's `extreme_bg_color` (text-edit interiors).
pub const EXTREME: Color32 = Color32::from_gray(6);

/// The gallery's white print matte. Paper white, not pure white, so the band
/// never reads as a blown highlight next to the photo.
pub const MATTE_WHITE: Color32 = Color32::from_gray(245);
/// The gallery's black print matte. True black, below `SHEET_BG`, so the band
/// separates from the letterbox around it.
pub const MATTE_BLACK: Color32 = Color32::from_gray(0);

/// Interactive chrome ramp. Buttons rest just above chrome so they read as
/// controls without shouting, then climb on hover and press. One source for
/// every trigger, dropdown, and menu item.
pub const BTN_REST: Color32 = Color32::from_gray(30);
pub const BTN_HOVER: Color32 = Color32::from_gray(48);
pub const BTN_ACTIVE: Color32 = Color32::from_gray(64);
pub const BTN_OPEN: Color32 = Color32::from_gray(40);
/// Text on chrome: dim at rest, brightening with interaction.
pub const TEXT_REST: Color32 = Color32::from_gray(206);
pub const TEXT_HOVER: Color32 = Color32::from_gray(242);
/// The active segment of the MODE track — a clearly raised, filled block.
pub const SEGMENT_ACTIVE: Color32 = Color32::from_gray(64);
/// Secondary text and key hints.
pub const TEXT_DIM: Color32 = Color32::from_gray(170);
/// RAW badge background, and the chip behind a verdict glyph.
pub const BADGE_BG: Color32 = Color32::from_gray(8);

/// Selection — a light grease-pencil outline.
pub const SELECT_OUTLINE: Color32 = Color32::from_gray(205);
/// Focus cursor — a brighter, heavier outline than the selection.
pub const FOCUS_OUTLINE: Color32 = Color32::from_gray(248);
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

/// Crop-edit accent — marks a cropped photo's grid badge and the gallery
/// `CROPPED` chip. A distinct cool cyan, off the tag/verdict/burst/filter hues
/// (color = meaning: this photo is edited, and what's shown differs from disk).
pub const CROP_ACCENT: Color32 = Color32::from_rgb(86, 188, 200);

/// "You are filtered" accent — a muted slate marking the filter bar, its rule,
/// and the `N of M` count, so a narrowed grid reads at a glance. A UI-state
/// signal, deliberately cool and off to the side of the tag/verdict/burst hues.
pub const FILTER_ACCENT: Color32 = Color32::from_rgb(96, 124, 148);

/// Chrome shares one gutter across the top bar, filter bar, and central panel,
/// so the panels stack on a single rhythm instead of drifting per-panel.
pub const PANEL_H: i8 = 8;
pub const PANEL_V: i8 = 5;

/// The one gutter for chrome panels.
pub fn panel_margin() -> Margin {
    Margin::symmetric(PANEL_H, PANEL_V)
}

/// Family key for Inter Medium — group headers and any weighted emphasis.
const HEADER_FAMILY: &str = "inter-medium";

/// Tracked section labels in the toolbar (MODE, GROUP, SORT…). Dim, quiet.
pub fn label_micro() -> FontId {
    FontId::proportional(10.5)
}

/// Group headers — the one weighted face. Kept small so the band stays a quiet
/// edge annotation, not a row of its own.
pub fn header() -> FontId {
    FontId::new(12.5, FontFamily::Name(HEADER_FAMILY.into()))
}

/// Data: status line, counts, filenames — mono for tabular alignment.
pub fn data() -> FontId {
    FontId::monospace(12.0)
}

/// Smaller data: badge and count glyphs.
pub fn data_small() -> FontId {
    FontId::monospace(11.0)
}

pub fn apply(ctx: &Context) {
    install_fonts(ctx);

    let mut v = Visuals::dark();
    squareify(&mut v);

    v.panel_fill = CHROME_BG;
    v.window_fill = CHROME_BG;
    v.extreme_bg_color = EXTREME;
    v.faint_bg_color = Color32::from_gray(16);
    v.window_stroke = Stroke::new(1.0_f32, HAIRLINE);
    // Selection reads as a grease-pencil outline, not a brand tint.
    v.selection.bg_fill = Color32::from_gray(70);
    v.selection.stroke = Stroke::new(1.0_f32, Color32::from_gray(200));
    // Links stay in the monochrome language — no lone default blue in the chrome.
    v.hyperlink_color = TEXT_REST;

    widget_ramp(&mut v);

    ctx.set_visuals(v);
    ctx.global_style_mut(|s| {
        s.spacing.item_spacing = Vec2::new(6.0, 6.0);
        s.spacing.button_padding = Vec2::new(8.0, 4.0);
        // Chrome text is a label, not content — no drag-to-select.
        s.interaction.selectable_labels = false;
    });
}

/// Bundle Inter (proportional) and JetBrains Mono (monospace), keeping egui's
/// default faces as the fallback tail so glyphs the two faces lack (e.g. `⌘`)
/// still resolve.
fn install_fonts(ctx: &Context) {
    let mut fonts = FontDefinitions::default();

    fonts.font_data.insert(
        "inter".to_owned(),
        Arc::new(FontData::from_static(include_bytes!(
            "../../../assets/fonts/Inter-Regular.ttf"
        ))),
    );
    fonts.font_data.insert(
        HEADER_FAMILY.to_owned(),
        Arc::new(FontData::from_static(include_bytes!(
            "../../../assets/fonts/Inter-Medium.ttf"
        ))),
    );
    fonts.font_data.insert(
        "jetbrains-mono".to_owned(),
        Arc::new(FontData::from_static(include_bytes!(
            "../../../assets/fonts/JetBrainsMono-Regular.ttf"
        ))),
    );

    fonts
        .families
        .entry(FontFamily::Proportional)
        .or_default()
        .insert(0, "inter".to_owned());
    fonts
        .families
        .entry(FontFamily::Monospace)
        .or_default()
        .insert(0, "jetbrains-mono".to_owned());

    // Inter Medium, then the proportional fallback tail for glyphs it lacks.
    let mut header_chain = vec![HEADER_FAMILY.to_owned()];
    header_chain.extend(fonts.families[&FontFamily::Proportional].iter().cloned());
    fonts
        .families
        .insert(FontFamily::Name(HEADER_FAMILY.into()), header_chain);

    ctx.set_fonts(fonts);
}

/// The interactive grey ramp for chrome widgets. Without this the buttons fall
/// back to egui's default dark ramp (a bright gray-60 rest fill), which clashes
/// with the flat toolbar and reads as two different button styles.
fn widget_ramp(v: &mut Visuals) {
    let w = &mut v.widgets;

    w.noninteractive.bg_fill = CHROME_BG;
    w.noninteractive.weak_bg_fill = CHROME_BG;
    w.noninteractive.bg_stroke = Stroke::new(1.0_f32, HAIRLINE);
    w.noninteractive.fg_stroke = Stroke::new(1.0_f32, TEXT_DIM);

    w.inactive.bg_fill = BTN_REST;
    w.inactive.weak_bg_fill = BTN_REST;
    w.inactive.bg_stroke = Stroke::NONE;
    w.inactive.fg_stroke = Stroke::new(1.0_f32, TEXT_REST);

    w.hovered.bg_fill = BTN_HOVER;
    w.hovered.weak_bg_fill = BTN_HOVER;
    w.hovered.bg_stroke = Stroke::new(1.0_f32, Color32::from_gray(60));
    w.hovered.fg_stroke = Stroke::new(1.0_f32, TEXT_HOVER);

    w.active.bg_fill = BTN_ACTIVE;
    w.active.weak_bg_fill = BTN_ACTIVE;
    w.active.bg_stroke = Stroke::new(1.0_f32, Color32::from_gray(74));
    w.active.fg_stroke = Stroke::new(1.0_f32, Color32::from_gray(245));

    w.open.bg_fill = BTN_OPEN;
    w.open.weak_bg_fill = BTN_OPEN;
    w.open.bg_stroke = Stroke::new(1.0_f32, Color32::from_gray(60));
    w.open.fg_stroke = Stroke::new(1.0_f32, TEXT_HOVER);
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
