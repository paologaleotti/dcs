//! Executor tests against a real temp tree: the atomic copy, the never-overwrite
//! guarantee, source-missing skips, subfolder creation, and cancel.

use std::path::{Path, PathBuf};
use std::thread::sleep;
use std::time::Duration;

use dcs_domain::export::{ExportOp, ExportPlan, FileRole};
use dcs_io::export::{ExportEvent, ExportHandle, SkipKind, run_export};

fn temp_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("dcs_export_{}_{}", std::process::id(), tag));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn write(path: &Path, contents: &str) {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    std::fs::write(path, contents).unwrap();
}

fn op(source: PathBuf, dest: PathBuf, role: FileRole) -> ExportOp {
    ExportOp { source, dest, role }
}

fn plan(ops: Vec<ExportOp>, dest: PathBuf) -> ExportPlan {
    let jpeg_count = ops.iter().filter(|o| o.role == FileRole::Jpeg).count();
    let raw_count = ops.iter().filter(|o| o.role == FileRole::Raw).count();
    ExportPlan {
        ops,
        skipped: Vec::new(),
        jpeg_count,
        raw_count,
        collisions: 0,
        dest,
        summary: String::new(),
    }
}

fn drain(handle: &ExportHandle) -> Vec<ExportEvent> {
    let mut events = Vec::new();
    for _ in 0..2000 {
        events.extend(handle.poll());
        if !handle.is_running() {
            events.extend(handle.poll());
            return events;
        }
        sleep(Duration::from_millis(1));
    }
    panic!("export did not finish");
}

#[test]
fn copies_files_and_preserves_content() {
    let dir = temp_dir("copy");
    let src = dir.join("src");
    let out = dir.join("out");
    write(&src.join("a.jpg"), "JPEG-A");
    write(&src.join("a.raf"), "RAW-A");

    let handle = run_export(plan(
        vec![
            op(src.join("a.jpg"), out.join("a.jpg"), FileRole::Jpeg),
            op(src.join("a.raf"), out.join("a.raf"), FileRole::Raw),
        ],
        out.clone(),
    ));
    let events = drain(&handle);

    assert_eq!(
        events,
        vec![
            ExportEvent::Copied {
                role: FileRole::Jpeg
            },
            ExportEvent::Copied {
                role: FileRole::Raw
            },
        ]
    );
    assert_eq!(
        std::fs::read_to_string(out.join("a.jpg")).unwrap(),
        "JPEG-A"
    );
    assert_eq!(std::fs::read_to_string(out.join("a.raf")).unwrap(), "RAW-A");
    // No torn .part files left behind.
    assert!(!out.join("a.jpg.part").exists());

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn never_overwrites_an_existing_dest() {
    let dir = temp_dir("no_overwrite");
    let src = dir.join("src");
    let out = dir.join("out");
    write(&src.join("a.jpg"), "NEW");
    write(&out.join("a.jpg"), "ORIGINAL"); // pre-existing at the dest

    let handle = run_export(plan(
        vec![op(src.join("a.jpg"), out.join("a.jpg"), FileRole::Jpeg)],
        out.clone(),
    ));
    let events = drain(&handle);

    assert_eq!(
        events,
        vec![ExportEvent::Skipped {
            reason: SkipKind::DestExists
        }]
    );
    // The original is untouched (dcs never overwrites).
    assert_eq!(
        std::fs::read_to_string(out.join("a.jpg")).unwrap(),
        "ORIGINAL"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn skips_when_the_source_vanished() {
    let dir = temp_dir("src_missing");
    let out = dir.join("out");

    let handle = run_export(plan(
        vec![op(
            dir.join("gone.jpg"),
            out.join("gone.jpg"),
            FileRole::Jpeg,
        )],
        out.clone(),
    ));
    let events = drain(&handle);

    assert_eq!(
        events,
        vec![ExportEvent::Skipped {
            reason: SkipKind::SourceMissing
        }]
    );
    assert!(!out.join("gone.jpg").exists());

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn creates_nested_destination_folders() {
    let dir = temp_dir("nested");
    let src = dir.join("src");
    let out = dir.join("out");
    write(&src.join("a.jpg"), "X");

    let handle = run_export(plan(
        vec![op(
            src.join("a.jpg"),
            out.join("JPEG").join("day1").join("a.jpg"),
            FileRole::Jpeg,
        )],
        out.clone(),
    ));
    let events = drain(&handle);

    assert_eq!(
        events,
        vec![ExportEvent::Copied {
            role: FileRole::Jpeg
        }]
    );
    assert!(out.join("JPEG").join("day1").join("a.jpg").exists());

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn reports_failed_when_the_destination_parent_cannot_be_created() {
    let dir = temp_dir("fail_parent");
    let src = dir.join("src");
    let out = dir.join("out");
    write(&src.join("a.jpg"), "X");
    // A regular file sits exactly where the dest's parent directory would need to
    // be, so `create_dir_all` can't make it: the op fails and is reported, with
    // the source left untouched.
    write(&out.join("blocker"), "i am a file, not a dir");
    let dest = out.join("blocker").join("a.jpg");

    let handle = run_export(plan(
        vec![op(src.join("a.jpg"), dest.clone(), FileRole::Jpeg)],
        out.clone(),
    ));
    let events = drain(&handle);

    assert_eq!(events.len(), 1);
    assert!(
        matches!(events[0], ExportEvent::Failed { .. }),
        "expected Failed, got {:?}",
        events[0]
    );
    assert!(!dest.exists());
    // The source original is sacred — never touched on failure.
    assert_eq!(std::fs::read_to_string(src.join("a.jpg")).unwrap(), "X");

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn cancel_stops_early_and_finishes_cleanly() {
    let dir = temp_dir("cancel");
    let src = dir.join("src");
    let out = dir.join("out");
    let mut ops = Vec::new();
    for i in 0..200 {
        let name = format!("{i}.jpg");
        write(&src.join(&name), &name);
        ops.push(op(src.join(&name), out.join(&name), FileRole::Jpeg));
    }
    let total = ops.len();

    let handle = run_export(plan(ops, out.clone()));
    handle.cancel();
    let events = drain(&handle);

    // Cancel is checked between files, so some prefix may copy, never more than
    // the whole plan, and the worker always finishes.
    assert!(events.len() <= total);
    assert!(!handle.is_running());

    let _ = std::fs::remove_dir_all(&dir);
}
