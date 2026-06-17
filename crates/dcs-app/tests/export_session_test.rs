//! Session-level export: scope resolution from verdicts/selection, the honesty
//! count, and that the plan threads through to the pure planner with the current
//! grouping. Pixel decode is irrelevant here, only the pool + verdicts.

use std::path::{Path, PathBuf};
use std::thread::sleep;
use std::time::Duration;

use dcs_app::{Collision, ExportError, ExportRequest, ExportScope, FileSelection, Layout, Session};
use image::{Rgb, RgbImage};

fn temp_folder(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("dcs_exsess_{}_{}", std::process::id(), tag));
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

fn request(out: &Path, files: FileSelection, layout: Layout) -> ExportRequest {
    ExportRequest {
        dest: out.to_path_buf(),
        files,
        layout,
        collision: Collision::Rename,
        template: None,
    }
}

#[test]
fn scope_counts_track_verdicts() {
    let (mut session, dir) = opened_with(3, "scopes");
    session.set_focus(0, false);
    session.accept(); // a.jpg accepted; b, c unreviewed

    assert_eq!(session.export_scope_count(ExportScope::Everything), 3);
    assert_eq!(session.export_scope_count(ExportScope::Accepted), 1);
    assert_eq!(session.export_scope_count(ExportScope::Unreviewed), 2);
    assert_eq!(
        session.export_scope_count(ExportScope::AcceptedAndUnreviewed),
        3
    );
    assert_eq!(session.export_scope_count(ExportScope::Rejected), 0);
    assert_eq!(session.unreviewed_count(), 2);

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn everything_scope_plans_one_op_per_jpeg() {
    let (session, dir) = opened_with(3, "plan_all");
    let out = dir.join("export");
    // JPEG-only photos: Any copies what exists (Both would skip them all).
    let plan = session
        .plan_export(
            ExportScope::Everything,
            &request(&out, FileSelection::Any, Layout::Together),
        )
        .unwrap();

    assert_eq!(plan.ops.len(), 3);
    assert_eq!(plan.jpeg_count, 3);
    assert_eq!(plan.raw_count, 0);
    assert_eq!(plan.dest, out);
    assert!(plan.summary.starts_with("Copy 3 files"));

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn selection_scope_plans_only_selected() {
    let (mut session, dir) = opened_with(3, "plan_sel");
    session.pointer_select(0, false, false); // select a.jpg only
    let out = dir.join("export");
    let plan = session
        .plan_export(
            ExportScope::Selection,
            &request(&out, FileSelection::Jpeg, Layout::Together),
        )
        .unwrap();

    assert_eq!(plan.ops.len(), 1);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn empty_scope_surfaces_the_planner_error() {
    let (session, dir) = opened_with(2, "plan_empty");
    let out = dir.join("export");
    // Nothing rejected → the rejected scope is empty.
    assert_eq!(
        session.plan_export(
            ExportScope::Rejected,
            &request(&out, FileSelection::Both, Layout::Together)
        ),
        Err(ExportError::EmptyScope)
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn start_export_runs_to_completion_and_status_accounts_for_every_op() {
    // End-to-end through the session: plan, run, poll until done. The final
    // status must total every op — no events dropped on the finishing frame.
    let (mut session, dir) = opened_with(3, "run_all");
    let out = dir.join("export");
    let plan = session
        .plan_export(
            ExportScope::Everything,
            &request(&out, FileSelection::Jpeg, Layout::Together),
        )
        .unwrap();
    let total = plan.ops.len();
    assert_eq!(total, 3);

    session.start_export(plan);
    let mut status = session.export_status().expect("status set on start");
    for _ in 0..3000 {
        session.tick();
        status = session.export_status().unwrap();
        if !status.running {
            break;
        }
        sleep(Duration::from_millis(1));
    }

    assert!(!status.running, "export never finished");
    assert_eq!(status.total, total);
    assert_eq!(
        status.done(),
        total,
        "every op must be accounted for in the final status"
    );
    assert_eq!(status.copied, total);
    for i in 0..3 {
        let name = format!("{}.jpg", (b'a' + i as u8) as char);
        assert!(out.join(&name).exists(), "{name} was copied");
    }

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn group_as_folders_uses_the_current_group_title() {
    let (session, dir) = opened_with(2, "plan_groups");
    let out = dir.join("export");
    // Plain JPEGs are undated → one "No date" group, so every file lands there.
    let plan = session
        .plan_export(
            ExportScope::Everything,
            &request(&out, FileSelection::Jpeg, Layout::GroupAsFolders),
        )
        .unwrap();

    assert!(
        plan.ops
            .iter()
            .all(|op| op.dest.starts_with(out.join("No date")))
    );
    let _ = std::fs::remove_dir_all(&dir);
}
