use dcs_domain::command::{Patch, TagDelta};
use dcs_domain::cull::AcceptState;
use dcs_domain::photo::PhotoId;
use dcs_domain::tag::{Color, Tag, TagId};
use dcs_io::undo_log::{self, Stacks, UndoLog, VerdictChange};

fn chg(id: u32, before: AcceptState, after: AcceptState) -> VerdictChange {
    (PhotoId(id), before, after)
}

/// A verdict patch (one accepted photo).
fn entry(id: u32) -> Patch {
    Patch::Verdict(vec![chg(
        id,
        AcceptState::Unreviewed,
        AcceptState::Accepted,
    )])
}

/// A tag patch (create + assign), exercising the `DoTag` record path.
fn tag_entry(tag: u32, photo: u32) -> Patch {
    Patch::Tag(vec![
        TagDelta::Created(Tag {
            id: TagId(tag),
            name: format!("t{tag}"),
            color: Color::rgb(1, 2, 3),
        }),
        TagDelta::Assigned(TagId(tag), PhotoId(photo)),
    ])
}

fn log_path() -> std::path::PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!(
        "dcs-undolog-{nanos}-{:?}",
        std::thread::current().id()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir.join("undo.log")
}

#[test]
fn missing_log_folds_to_empty_stacks() {
    let path = log_path();
    assert_eq!(undo_log::load(&path).unwrap(), Stacks::default());
}

#[test]
fn append_then_fold_reconstructs_undo_and_redo() {
    let path = log_path();
    let mut log = UndoLog::open(&path).unwrap();
    log.record_patch(&entry(1)).unwrap();
    log.record_patch(&entry(2)).unwrap();
    log.record_undo().unwrap();
    drop(log);

    let stacks = undo_log::load(&path).unwrap();
    assert_eq!(stacks.undo, vec![entry(1)], "entry 2 was undone");
    assert_eq!(stacks.redo, vec![entry(2)], "entry 2 is now redoable");
}

#[test]
fn verdict_and_tag_patches_interleave_in_one_timeline() {
    let path = log_path();
    let mut log = UndoLog::open(&path).unwrap();
    log.record_patch(&entry(1)).unwrap();
    log.record_patch(&tag_entry(5, 1)).unwrap();
    log.record_undo().unwrap(); // tag entry → redo
    drop(log);

    let stacks = undo_log::load(&path).unwrap();
    assert_eq!(stacks.undo, vec![entry(1)], "verdict entry remains");
    assert_eq!(stacks.redo, vec![tag_entry(5, 1)], "tag entry redoable");
}

#[test]
fn a_new_do_clears_the_redo_stack() {
    let path = log_path();
    let mut log = UndoLog::open(&path).unwrap();
    log.record_patch(&entry(1)).unwrap();
    log.record_undo().unwrap(); // redo = [1]
    log.record_patch(&entry(2)).unwrap(); // a fresh action drops the redo branch
    drop(log);

    let stacks = undo_log::load(&path).unwrap();
    assert_eq!(stacks.undo, vec![entry(2)]);
    assert!(stacks.redo.is_empty(), "redo branch dropped by the new Do");
}

#[test]
fn compaction_round_trips_both_stacks() {
    let path = log_path();
    let stacks = Stacks {
        undo: vec![entry(1), tag_entry(2, 2), entry(3)],
        redo: vec![entry(8), tag_entry(9, 9)],
    };
    undo_log::compact(&path, &stacks, 100).unwrap();
    assert_eq!(undo_log::load(&path).unwrap(), stacks);
}

#[test]
fn compaction_trims_undo_to_the_newest_cap_entries() {
    let path = log_path();
    let stacks = Stacks {
        undo: vec![entry(1), entry(2), entry(3), entry(4), entry(5)],
        redo: vec![],
    };
    undo_log::compact(&path, &stacks, 2).unwrap();
    let loaded = undo_log::load(&path).unwrap();
    assert_eq!(
        loaded.undo,
        vec![entry(4), entry(5)],
        "oldest dropped, newest kept"
    );
}

#[test]
fn appending_after_compaction_keeps_folding_correctly() {
    let path = log_path();
    let start = Stacks {
        undo: vec![entry(1)],
        redo: vec![],
    };
    undo_log::compact(&path, &start, 100).unwrap();

    let mut log = UndoLog::open(&path).unwrap();
    log.record_patch(&entry(2)).unwrap();
    drop(log);

    let stacks = undo_log::load(&path).unwrap();
    assert_eq!(stacks.undo, vec![entry(1), entry(2)]);
}

#[test]
fn a_torn_trailing_line_is_ignored_not_fatal() {
    let path = log_path();
    let mut log = UndoLog::open(&path).unwrap();
    log.record_patch(&entry(1)).unwrap();
    drop(log);
    // Simulate a crash mid-append: a partial JSON line with no newline.
    use std::io::Write;
    let mut f = std::fs::OpenOptions::new()
        .append(true)
        .open(&path)
        .unwrap();
    f.write_all(b"{\"Do\":{\"changes\":[").unwrap();
    drop(f);

    let stacks = undo_log::load(&path).unwrap();
    assert_eq!(
        stacks.undo,
        vec![entry(1)],
        "good entries survive, torn line dropped"
    );
}

// --- Crop records (DoCrop) ---------------------------------------------------

#[test]
fn crop_patches_round_trip_through_the_log() {
    use dcs_domain::command::CropChange;
    use dcs_domain::crops::{CropEdit, NormRect};

    let dir = std::env::temp_dir().join(format!("dcs_undolog_crop_{}", std::process::id()));
    let _ = std::fs::create_dir_all(&dir);
    let path = dir.join("undo.log");
    let _ = std::fs::remove_file(&path);

    let edit = CropEdit {
        angle_deg: 3.5,
        rect: NormRect::centered(0.7, 0.6),
    };
    let change: CropChange = (PhotoId(9), None, Some(edit));
    let patch = Patch::Crop(vec![change]);

    {
        let mut log = UndoLog::open(&path).unwrap();
        log.record_patch(&patch).unwrap();
    }
    let stacks = undo_log::load(&path).unwrap();
    assert_eq!(stacks.undo, vec![patch]);
    assert!(stacks.redo.is_empty());

    let _ = std::fs::remove_dir_all(&dir);
}
