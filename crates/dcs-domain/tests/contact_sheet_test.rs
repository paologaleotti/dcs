use std::path::PathBuf;

use dcs_domain::contact_sheet::{
    CellCaption, ContactSheetError, ContactSheetSettings, GridMode, PaperOrientation, PaperSize,
    SheetBackground, SheetItem, plan_contact_sheet,
};
use dcs_domain::photo::{Orientation, PhotoId};

fn settings() -> ContactSheetSettings {
    ContactSheetSettings {
        paper: PaperSize::A4,
        orientation: PaperOrientation::Landscape,
        background: SheetBackground::Black,
        grid: GridMode::FixedColumns(6),
        margin_pt: 24.0,
        gutter_pt: 6.0,
        caption: CellCaption::numbers_only(),
        number_from: 1,
        title: None,
        dest: PathBuf::from("/tmp/sheet.pdf"),
    }
}

fn items(n: usize) -> Vec<SheetItem<'static>> {
    (0..n)
        .map(|i| SheetItem {
            id: PhotoId(i as u32),
            orientation: Orientation::Normal,
            name: "DSCF0001",
            exposure: Some("35mm · f/2.8 · 1/250"),
        })
        .collect()
}

#[test]
fn empty_items_is_empty_scope_error() {
    let err = plan_contact_sheet(&[], &settings()).unwrap_err();
    assert_eq!(err, ContactSheetError::EmptyScope);
}

#[test]
fn single_item_one_page_one_cell() {
    let plan = plan_contact_sheet(&items(1), &settings()).unwrap();
    assert_eq!(plan.pages.len(), 1);
    assert_eq!(plan.pages[0].cells.len(), 1);
    assert_eq!(plan.frame_count, 1);
}

#[test]
fn fixed_columns_derives_row_count() {
    let plan = plan_contact_sheet(&items(30), &settings()).unwrap();
    assert_eq!(plan.cols, 6);
    assert!(plan.rows_per_page >= 1);
}

#[test]
fn target_cell_edge_picks_column_count() {
    let mut s = settings();
    s.grid = GridMode::TargetCellEdge(120.0);
    let plan = plan_contact_sheet(&items(20), &s).unwrap();
    // A4 landscape content width ≈ 841.89 - 48 = 793.89; (793.89+6)/(120+6) ≈ 6.
    assert_eq!(plan.cols, 6);
}

#[test]
fn pagination_splits_across_pages() {
    let s = settings();
    let plan = plan_contact_sheet(&items(1), &s).unwrap();
    let per_page = (plan.cols * plan.rows_per_page) as usize;

    let plan = plan_contact_sheet(&items(per_page + 1), &s).unwrap();
    assert_eq!(plan.pages.len(), 2);
    assert_eq!(plan.pages[0].cells.len(), per_page);
    assert_eq!(plan.pages[1].cells.len(), 1);
}

#[test]
fn exact_page_fill_no_empty_trailing_page() {
    let s = settings();
    let probe = plan_contact_sheet(&items(1), &s).unwrap();
    let per_page = (probe.cols * probe.rows_per_page) as usize;

    let plan = plan_contact_sheet(&items(per_page), &s).unwrap();
    assert_eq!(plan.pages.len(), 1);
    assert_eq!(plan.pages[0].cells.len(), per_page);
}

#[test]
fn frame_numbers_are_sequential_across_pages() {
    let s = settings();
    let probe = plan_contact_sheet(&items(1), &s).unwrap();
    let per_page = (probe.cols * probe.rows_per_page) as usize;

    let plan = plan_contact_sheet(&items(per_page + 3), &s).unwrap();
    // Page 1 first cell is 1, page 2 first cell continues at per_page+1.
    assert_eq!(plan.pages[0].cells[0].frame_number, 1);
    assert_eq!(plan.pages[1].cells[0].frame_number, per_page as u32 + 1);
    // Fully sequential over the whole sheet.
    let all: Vec<u32> = plan
        .pages
        .iter()
        .flat_map(|p| p.cells.iter().map(|c| c.frame_number))
        .collect();
    assert_eq!(all, (1..=per_page as u32 + 3).collect::<Vec<_>>());
}

#[test]
fn number_from_offsets_first_frame() {
    let mut s = settings();
    s.number_from = 100;
    let plan = plan_contact_sheet(&items(3), &s).unwrap();
    assert_eq!(plan.pages[0].cells[0].frame_number, 100);
    assert_eq!(plan.pages[0].cells[2].frame_number, 102);
}

#[test]
fn cell_rects_stay_within_content_and_do_not_overlap() {
    let plan = plan_contact_sheet(&items(24), &settings()).unwrap();
    let page = &plan.pages[0];
    let (pw, ph) = page.size_pt;
    let rects: Vec<_> = page
        .cells
        .iter()
        .map(|c| {
            // Full cell box = image + caption strip.
            let r = c.image_rect;
            (r.x, r.y, r.w, r.h + c.caption_rect.h)
        })
        .collect();
    for &(x, y, w, h) in &rects {
        assert!(x >= 0.0 && y >= 0.0, "cell inside page");
        assert!(x + w <= pw + 0.01 && y + h <= ph + 0.01, "cell inside page");
    }
    for i in 0..rects.len() {
        for j in (i + 1)..rects.len() {
            let (ax, ay, aw, ah) = rects[i];
            let (bx, by, bw, bh) = rects[j];
            let disjoint = ax + aw <= bx + 0.01
                || bx + bw <= ax + 0.01
                || ay + ah <= by + 0.01
                || by + bh <= ay + 0.01;
            assert!(disjoint, "cells {i} and {j} overlap");
        }
    }
}

#[test]
fn gutter_zero_tiles_edge_to_edge() {
    let mut s = settings();
    s.gutter_pt = 0.0;
    s.caption = CellCaption {
        number: false,
        filename: false,
        exposure: false,
    };
    let plan = plan_contact_sheet(&items(12), &s).unwrap();
    let cells = &plan.pages[0].cells;
    // Adjacent columns in row 0 abut exactly.
    let c0 = cells[0].image_rect;
    let c1 = cells[1].image_rect;
    assert!((c0.x + c0.w - c1.x).abs() < 0.001);
}

#[test]
fn portrait_landscape_swaps_page_dimensions() {
    let mut s = settings();
    s.orientation = PaperOrientation::Portrait;
    let portrait = plan_contact_sheet(&items(1), &s).unwrap();
    s.orientation = PaperOrientation::Landscape;
    let landscape = plan_contact_sheet(&items(1), &s).unwrap();
    let (pw, ph) = portrait.pages[0].size_pt;
    let (lw, lh) = landscape.pages[0].size_pt;
    assert!((pw - lh).abs() < 0.01 && (ph - lw).abs() < 0.01);
    assert!(pw < ph && lw > lh);
}

#[test]
fn orientation_sets_expect_portrait_for_all_variants() {
    for (o, expected) in [
        (Orientation::Normal, false),
        (Orientation::FlipH, false),
        (Orientation::Rotate180, false),
        (Orientation::FlipV, false),
        (Orientation::Transpose, true),
        (Orientation::Rotate90, true),
        (Orientation::Transverse, true),
        (Orientation::Rotate270, true),
    ] {
        let item = [SheetItem {
            id: PhotoId(0),
            orientation: o,
            name: "x",
            exposure: None,
        }];
        let plan = plan_contact_sheet(&item, &settings()).unwrap();
        assert_eq!(
            plan.pages[0].cells[0].expect_portrait, expected,
            "orientation {o:?}"
        );
    }
}

#[test]
fn caption_none_gives_zero_strip_and_square_cells() {
    let mut s = settings();
    s.caption = CellCaption {
        number: false,
        filename: false,
        exposure: false,
    };
    let plan = plan_contact_sheet(&items(1), &s).unwrap();
    let cell = &plan.pages[0].cells[0];
    assert_eq!(cell.number_text, "");
    assert!(cell.filename.is_none());
    assert!(cell.exposure.is_none());
    assert_eq!(cell.caption_rect.h, 0.0);
    // No caption strip → the cell box is exactly the 3:2 image box.
    assert!((cell.image_rect.h - cell.image_rect.w * 2.0 / 3.0).abs() < 0.01);
    assert!((plan.cell_size_pt.1 - plan.cell_size_pt.0 * 2.0 / 3.0).abs() < 0.01);
}

#[test]
fn caption_number_only_is_just_the_number() {
    let plan = plan_contact_sheet(&items(1), &settings()).unwrap();
    let cell = &plan.pages[0].cells[0];
    assert_eq!(cell.number_text, "1");
    assert!(cell.filename.is_none());
    assert!(cell.exposure.is_none());
}

#[test]
fn caption_filename_rides_the_number_line() {
    let mut s = settings();
    s.caption = CellCaption {
        number: true,
        filename: true,
        exposure: false,
    };
    let plan = plan_contact_sheet(&items(1), &s).unwrap();
    let cell = &plan.pages[0].cells[0];
    assert_eq!(cell.number_text, "1");
    assert_eq!(cell.filename.as_deref(), Some("DSCF0001"));
    assert!(cell.exposure.is_none());
}

#[test]
fn caption_exposure_is_its_own_line() {
    let mut s = settings();
    s.caption = CellCaption {
        number: true,
        filename: false,
        exposure: true,
    };
    let plan = plan_contact_sheet(&items(1), &s).unwrap();
    let cell = &plan.pages[0].cells[0];
    assert_eq!(cell.number_text, "1");
    assert_eq!(cell.exposure.as_deref(), Some("35mm · f/2.8 · 1/250"));
}

#[test]
fn caption_exposure_flag_but_none_available_reserves_strip_omits_line() {
    let mut s = settings();
    s.caption = CellCaption {
        number: true,
        filename: false,
        exposure: true,
    };
    let item = [SheetItem {
        id: PhotoId(0),
        orientation: Orientation::Normal,
        name: "x",
        exposure: None,
    }];
    let plan = plan_contact_sheet(&item, &s).unwrap();
    let cell = &plan.pages[0].cells[0];
    // The number is present and no exposure is emitted, but the strip still
    // reserves the exposure line's height (uniform grid) so a missing exposure
    // doesn't shift the layout.
    assert_eq!(cell.number_text, "1");
    assert!(cell.exposure.is_none());
    assert!(cell.caption_rect.h > 0.0);
}

#[test]
fn margin_larger_than_page_is_no_usable_area() {
    let mut s = settings();
    s.margin_pt = 5000.0;
    assert_eq!(
        plan_contact_sheet(&items(1), &s).unwrap_err(),
        ContactSheetError::NoUsableArea
    );
}

#[test]
fn too_many_fixed_columns_is_cell_too_large() {
    let mut s = settings();
    s.grid = GridMode::FixedColumns(10_000);
    assert_eq!(
        plan_contact_sheet(&items(1), &s).unwrap_err(),
        ContactSheetError::CellTooLarge
    );
}

#[test]
fn target_edge_too_large_to_fit_a_row_errors() {
    let mut s = settings();
    // A huge target collapses to one very wide cell that is taller than the
    // content area → no row fits → CellTooLarge.
    s.grid = GridMode::TargetCellEdge(10_000.0);
    s.caption = CellCaption {
        number: false,
        filename: false,
        exposure: false,
    };
    let err = plan_contact_sheet(&items(1), &s).unwrap_err();
    assert_eq!(err, ContactSheetError::CellTooLarge);
}

#[test]
fn non_finite_inputs_error_rather_than_panic() {
    for bad in [f32::NAN, f32::INFINITY] {
        let mut s = settings();
        s.margin_pt = bad;
        assert!(plan_contact_sheet(&items(3), &s).is_err(), "margin {bad}");

        let mut s = settings();
        s.gutter_pt = bad;
        assert!(plan_contact_sheet(&items(3), &s).is_err(), "gutter {bad}");

        let mut s = settings();
        s.grid = GridMode::TargetCellEdge(bad);
        assert_eq!(
            plan_contact_sheet(&items(3), &s).unwrap_err(),
            ContactSheetError::CellTooLarge,
            "target {bad}"
        );
    }
}

#[test]
fn caption_all_three_reserves_number_plus_detail_lines() {
    let mut s = settings();
    s.caption = CellCaption {
        number: true,
        filename: true,
        exposure: true,
    };
    let plan = plan_contact_sheet(&items(1), &s).unwrap();
    let cell = &plan.pages[0].cells[0];
    assert_eq!(cell.number_text, "1");
    assert_eq!(cell.filename.as_deref(), Some("DSCF0001"));
    assert_eq!(cell.exposure.as_deref(), Some("35mm · f/2.8 · 1/250"));
    // Strip reserves one number line + one exposure detail line.
    let number_only = {
        let mut s2 = s;
        s2.caption = CellCaption::numbers_only();
        plan_contact_sheet(&items(1), &s2).unwrap().pages[0].cells[0]
            .caption_rect
            .h
    };
    assert!(
        cell.caption_rect.h > number_only,
        "extra detail line adds height"
    );
}

#[test]
fn caption_number_off_exposure_on_reserves_strip() {
    let mut s = settings();
    s.caption = CellCaption {
        number: false,
        filename: false,
        exposure: true,
    };
    let plan = plan_contact_sheet(&items(1), &s).unwrap();
    let cell = &plan.pages[0].cells[0];
    assert_eq!(cell.number_text, "");
    assert!(cell.filename.is_none());
    assert_eq!(cell.exposure.as_deref(), Some("35mm · f/2.8 · 1/250"));
    assert!(cell.caption_rect.h > 0.0);
}

#[test]
fn ascii_caption_transliterates_to_winansi_safe() {
    use dcs_domain::contact_sheet::ascii_caption;
    assert_eq!(
        ascii_caption("f/2.8 · 1/250 · ISO 400"),
        "f/2.8 - 1/250 - ISO 400"
    );
    assert_eq!(ascii_caption("DSCF1234"), "DSCF1234");
    // Non-representable glyphs are dropped, not passed through as mojibake.
    assert_eq!(ascii_caption("café 日本"), "caf ");
    assert_eq!(ascii_caption("a…"), "a.");
}

#[test]
fn mm_presets_convert_to_points() {
    let a4 = PaperSize::from_mm(210.0, 297.0);
    assert!((a4.width_pt - 595.276).abs() < 0.5);
    assert!((a4.height_pt - 841.890).abs() < 0.5);
}

#[test]
fn summary_mentions_frames_pages_grid_and_background() {
    let plan = plan_contact_sheet(&items(7), &settings()).unwrap();
    let s = &plan.summary;
    assert!(s.contains("7 frames"), "{s}");
    assert!(s.contains("A4"), "{s}");
    assert!(s.contains("landscape"), "{s}");
    assert!(s.contains("black"), "{s}");
    assert!(
        s.contains(&format!("{}×{}", plan.cols, plan.rows_per_page)),
        "{s}"
    );
}

#[test]
fn rows_per_page_at_least_one_when_barely_fitting() {
    let mut s = settings();
    // Tall-ish page, big cells: still must fit at least one row.
    s.orientation = PaperOrientation::Portrait;
    s.grid = GridMode::FixedColumns(2);
    let plan = plan_contact_sheet(&items(2), &s).unwrap();
    assert!(plan.rows_per_page >= 1);
}

#[test]
fn header_band_is_always_reserved_and_title_does_not_change_the_grid() {
    let mut s = settings();
    let without = plan_contact_sheet(&items(30), &s).unwrap();
    s.title = Some("Roll 12 — Paris".to_string());
    let with = plan_contact_sheet(&items(30), &s).unwrap();
    // The header band is reserved on every page regardless of the title, so
    // setting a title carries the text without changing the grid geometry.
    for page in &with.pages {
        assert!(page.header_rect.h > 0.0);
        assert_eq!(page.title.as_deref(), Some("Roll 12 — Paris"));
    }
    for page in &without.pages {
        assert!(page.header_rect.h > 0.0);
        assert!(page.title.is_none());
    }
    assert_eq!(with.rows_per_page, without.rows_per_page);
    assert_eq!(with.cell_size_pt, without.cell_size_pt);
    // Grid uniform across pages: first cell y-origin identical on every page.
    let y0 = with.pages[0].cells[0].image_rect.y;
    for page in &with.pages {
        assert!((page.cells[0].image_rect.y - y0).abs() < 0.001);
    }
}

#[test]
fn footer_reports_page_position_and_frame_count() {
    let s = settings();
    let probe = plan_contact_sheet(&items(1), &s).unwrap();
    let per_page = (probe.cols * probe.rows_per_page) as usize;
    let plan = plan_contact_sheet(&items(per_page + 1), &s).unwrap();
    assert!(plan.pages[0].footer.contains("Page 1 of 2"));
    assert!(plan.pages[1].footer.contains("Page 2 of 2"));
    assert!(
        plan.pages[0]
            .footer
            .contains(&format!("{} frames", per_page + 1))
    );
}
