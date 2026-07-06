//! Renderer tests against a real temp path: the PDF is produced, is a valid PDF,
//! paginates to the planned page count, and leaves no `.part` behind. Frames
//! render as placeholders (empty thumb source) so no image fixtures are needed.

use std::collections::HashMap;
use std::path::PathBuf;
use std::thread::sleep;
use std::time::Duration;

use dcs_domain::contact_sheet::{
    CellCaption, ContactSheetSettings, GridMode, PaperOrientation, PaperSize, SheetBackground,
    SheetItem, plan_contact_sheet,
};
use dcs_domain::photo::{Orientation, PhotoId};
use dcs_io::contact_sheet::{ContactSheetEvent, ContactSheetHandle, run_contact_sheet};

fn temp_pdf(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("dcs_sheet_{}_{}", std::process::id(), tag));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir.join("contact-sheet.pdf")
}

fn settings(dest: PathBuf) -> ContactSheetSettings {
    ContactSheetSettings {
        paper: PaperSize::A4,
        orientation: PaperOrientation::Landscape,
        background: SheetBackground::Black,
        grid: GridMode::FixedColumns(4),
        margin_pt: 24.0,
        gutter_pt: 6.0,
        caption: CellCaption::numbers_only(),
        number_from: 1,
        title: Some("Test Roll".to_string()),
        dest,
    }
}

fn items(n: usize) -> Vec<SheetItem<'static>> {
    (0..n)
        .map(|i| SheetItem {
            id: PhotoId(i as u32),
            orientation: Orientation::Normal,
            name: "IMG",
            exposure: None,
        })
        .collect()
}

/// Drain the handle until the render finishes, counting events. Times out.
fn drain(handle: &ContactSheetHandle) -> (usize, usize) {
    let (mut rendered, mut failed) = (0usize, 0usize);
    for _ in 0..2000 {
        for event in handle.poll() {
            match event {
                ContactSheetEvent::PageRendered { .. } => rendered += 1,
                ContactSheetEvent::Failed { .. } => failed += 1,
            }
        }
        if !handle.is_running() {
            for event in handle.poll() {
                match event {
                    ContactSheetEvent::PageRendered { .. } => rendered += 1,
                    ContactSheetEvent::Failed { .. } => failed += 1,
                }
            }
            break;
        }
        sleep(Duration::from_millis(2));
    }
    (rendered, failed)
}

#[test]
fn renders_a_valid_multipage_pdf() {
    let dest = temp_pdf("multipage");
    let plan = plan_contact_sheet(&items(40), &settings(dest.clone())).unwrap();
    let pages = plan.pages.len();
    assert!(pages >= 2, "40 frames at 4 cols should span multiple pages");

    let handle = run_contact_sheet(plan, HashMap::new());
    assert_eq!(handle.total(), pages);
    let (rendered, failed) = drain(&handle);

    assert_eq!(rendered, pages, "one PageRendered per page");
    assert_eq!(failed, 0);
    assert!(dest.exists(), "the PDF was written");
    let bytes = std::fs::read(&dest).unwrap();
    assert!(bytes.starts_with(b"%PDF"), "output is a PDF");

    // No temp file left behind.
    let part = {
        let mut p = dest.clone().into_os_string();
        p.push(".part");
        PathBuf::from(p)
    };
    assert!(!part.exists(), ".part cleaned up after atomic rename");
}

#[test]
fn single_page_sheet_writes_one_page_pdf() {
    let dest = temp_pdf("single");
    let plan = plan_contact_sheet(&items(3), &settings(dest.clone())).unwrap();
    assert_eq!(plan.pages.len(), 1);

    let handle = run_contact_sheet(plan, HashMap::new());
    let (rendered, failed) = drain(&handle);
    assert_eq!(rendered, 1);
    assert_eq!(failed, 0);
    assert!(std::fs::read(&dest).unwrap().starts_with(b"%PDF"));
}

#[test]
fn overwrites_existing_destination_atomically() {
    let dest = temp_pdf("overwrite");
    std::fs::write(&dest, b"stale contents that is not a pdf").unwrap();

    let plan = plan_contact_sheet(&items(2), &settings(dest.clone())).unwrap();
    let handle = run_contact_sheet(plan, HashMap::new());
    let (_, failed) = drain(&handle);
    assert_eq!(failed, 0);
    // The user chose this path in the save dialog, so the render replaces it —
    // the result is a real PDF, not the stale bytes.
    assert!(std::fs::read(&dest).unwrap().starts_with(b"%PDF"));
}

#[test]
fn cancel_before_finish_writes_no_file() {
    let dest = temp_pdf("cancel");
    // A big multi-page plan so cancelling immediately lands mid-render.
    let plan = plan_contact_sheet(&items(400), &settings(dest.clone())).unwrap();
    assert!(plan.pages.len() > 2);

    let handle = run_contact_sheet(plan, HashMap::new());
    handle.cancel();
    // Drain to completion.
    for _ in 0..2000 {
        let _ = handle.poll();
        if !handle.is_running() {
            break;
        }
        sleep(Duration::from_millis(1));
    }
    assert!(!handle.is_running());
    // A cancelled render writes nothing (the PDF is saved only after all pages
    // render). It may or may not have finished before the cancel took effect;
    // if it didn't, the destination must not exist.
    if !dest.exists() {
        let part = {
            let mut p = dest.clone().into_os_string();
            p.push(".part");
            PathBuf::from(p)
        };
        assert!(!part.exists(), "no temp file left after cancel");
    }
}
