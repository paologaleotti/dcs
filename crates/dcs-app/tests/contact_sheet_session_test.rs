//! Session-level contact sheet: scope resolution reuses the export path, the
//! plan threads through to the pure planner in visible order, and an
//! end-to-end render writes a PDF and reports completion. Pixel decode is
//! irrelevant here beyond producing a real JPEG to embed.

use std::path::{Path, PathBuf};
use std::thread::sleep;
use std::time::Duration;

use dcs_app::{
    CellCaption, ContactSheetError, ContactSheetSettings, ExportScope, GridMode, PaperOrientation,
    PaperSize, Session, SheetBackground,
};
use image::{Rgb, RgbImage};

fn temp_folder(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("dcs_cssess_{}_{}", std::process::id(), tag));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn write_jpeg(dir: &Path, name: &str) {
    let mut img = RgbImage::new(48, 48);
    for (x, y, px) in img.enumerate_pixels_mut() {
        *px = Rgb([(x % 256) as u8, (y % 256) as u8, 128]);
    }
    img.save(dir.join(name)).expect("encode jpeg");
}

fn opened_with(n: usize, tag: &str) -> (Session, PathBuf) {
    let dir = temp_folder(tag);
    for i in 0..n {
        write_jpeg(&dir, &format!("{}.jpg", (b'a' + i as u8) as char));
    }
    let mut session = Session::new();
    session.open_folder(dir.clone());
    for _ in 0..3000 {
        session.tick();
        if session.photo_count() >= n && !session.is_scanning() {
            break;
        }
        sleep(Duration::from_millis(1));
    }
    (session, dir)
}

fn settings(dest: PathBuf) -> ContactSheetSettings {
    ContactSheetSettings {
        paper: PaperSize::A4,
        orientation: PaperOrientation::Landscape,
        background: SheetBackground::Black,
        grid: GridMode::FixedColumns(4),
        margin_pt: 24.0,
        gutter_pt: 6.0,
        caption: CellCaption {
            number: true,
            filename: true,
            exposure: true,
        },
        number_from: 1,
        title: None,
        dest,
    }
}

#[test]
fn plan_covers_scope_in_visible_order_with_sequential_numbers() {
    let (session, dir) = opened_with(5, "plan");
    let plan = session
        .plan_contact_sheet(ExportScope::Everything, &settings(dir.join("sheet.pdf")))
        .unwrap();

    assert_eq!(plan.frame_count, 5);
    let numbers: Vec<u32> = plan
        .pages
        .iter()
        .flat_map(|p| p.cells.iter().map(|c| c.frame_number))
        .collect();
    assert_eq!(numbers, vec![1, 2, 3, 4, 5]);

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn empty_scope_surfaces_the_planner_error() {
    let (session, dir) = opened_with(2, "empty");
    // Nothing rejected → the rejected scope is empty.
    assert_eq!(
        session.plan_contact_sheet(ExportScope::Rejected, &settings(dir.join("sheet.pdf"))),
        Err(ContactSheetError::EmptyScope)
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn start_contact_sheet_runs_to_completion_and_writes_a_pdf() {
    let (mut session, dir) = opened_with(3, "render");
    let dest = dir.join("sheet.pdf");
    let plan = session
        .plan_contact_sheet(ExportScope::Everything, &settings(dest.clone()))
        .unwrap();

    // open_after=false so the test doesn't spawn a viewer.
    session.start_contact_sheet(plan, false);
    for _ in 0..3000 {
        session.tick();
        let done = session.contact_sheet_status().is_some_and(|s| !s.running);
        if done {
            break;
        }
        sleep(Duration::from_millis(1));
    }

    let status = session.contact_sheet_status().expect("status after render");
    assert!(!status.running);
    assert!(status.succeeded);
    assert_eq!(status.failed, 0);
    let pdf = std::fs::read(&dest).unwrap();
    assert!(pdf.starts_with(b"%PDF"));
    // The thumbnails must actually embed as image XObjects — not just captions.
    let has = |needle: &[u8]| pdf.windows(needle.len()).any(|w| w == needle);
    assert!(
        has(b"Image") && has(b"DeviceRGB"),
        "rendered PDF must contain embedded RGB image XObjects, not just captions"
    );

    let _ = std::fs::remove_dir_all(&dir);
}
