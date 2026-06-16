//! End-to-end persistence: cull a folder, save, reopen a fresh session on the
//! same folder, and confirm owned state (verdicts + undo history) is restored —
//! including across a rename-in-place, where identity is keyed on content.

use std::path::{Path, PathBuf};
use std::thread::sleep;
use std::time::Duration;

use dcs_app::Session;
use dcs_domain::cull::AcceptState;
use image::{Rgb, RgbImage};

fn temp_folder(tag: &str) -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!("dcs_persist_rt_{tag}_{nanos}"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// A distinct valid JPEG per name (content differs so fingerprints differ).
fn write_jpeg(dir: &Path, name: &str, seed: u8) {
    let mut img = RgbImage::new(64, 48);
    for (x, y, px) in img.enumerate_pixels_mut() {
        *px = Rgb([(x as u8).wrapping_add(seed), (y as u8).wrapping_mul(seed.max(1)), seed]);
    }
    img.save(dir.join(name)).expect("encode jpeg");
}

fn open(dir: &Path, want: usize) -> Session {
    let mut session = Session::new();
    session.set_recents_path(None); // never touch the user's real recents in tests
    session.open_folder(dir.to_path_buf());
    for _ in 0..3000 {
        session.tick();
        if session.pool_len() >= want && !session.is_scanning() {
            break;
        }
        sleep(Duration::from_millis(1));
    }
    session
}

#[test]
fn verdicts_survive_save_and_reopen() {
    let dir = temp_folder("verdicts");
    write_jpeg(&dir, "a.jpg", 10);
    write_jpeg(&dir, "b.jpg", 20);
    write_jpeg(&dir, "c.jpg", 30);

    {
        let mut s = open(&dir, 3);
        s.click_select(0);
        s.accept();
        s.click_select(1);
        s.reject();
        assert_eq!(s.verdict_counts(), (1, 1, 1));
        s.save().unwrap();
    }

    // A brand-new session on the same folder must restore the verdicts.
    let s = open(&dir, 3);
    assert_eq!(s.verdict_counts(), (1, 1, 1), "verdicts restored from project.json");
    let first = s.photo_at(0).unwrap().id;
    let second = s.photo_at(1).unwrap().id;
    assert_eq!(s.verdict(first), AcceptState::Accepted);
    assert_eq!(s.verdict(second), AcceptState::Rejected);
}

#[test]
fn save_writes_the_sidecar_stores() {
    let dir = temp_folder("sidecar");
    write_jpeg(&dir, "a.jpg", 7);
    let mut s = open(&dir, 1);
    s.click_select(0);
    s.accept();
    s.save().unwrap();

    let sidecar = dir.join(".dcs");
    assert!(sidecar.join("project.json").exists(), "precious store written");
    assert!(sidecar.join("undo.log").exists(), "durable log written");
    assert!(sidecar.join("cache.sqlite3").exists(), "disposable cache opened");
}

#[test]
fn rename_in_place_keeps_the_verdict() {
    let dir = temp_folder("rename");
    write_jpeg(&dir, "original.jpg", 42);
    {
        let mut s = open(&dir, 1);
        s.click_select(0);
        s.accept();
        s.save().unwrap();
    }

    // Rename the file on disk: same bytes, new name → same content fingerprint.
    std::fs::rename(dir.join("original.jpg"), dir.join("renamed.jpg")).unwrap();

    let s = open(&dir, 1);
    assert_eq!(s.pool_len(), 1, "no duplicate, no blank new photo");
    assert_eq!(s.photo_at(0).unwrap().file_name(), "renamed.jpg");
    assert_eq!(
        s.verdict_counts(),
        (1, 0, 0),
        "the accept survived the rename via content identity"
    );
}

#[test]
fn undo_history_survives_reopen() {
    let dir = temp_folder("undo");
    write_jpeg(&dir, "a.jpg", 11);
    write_jpeg(&dir, "b.jpg", 22);
    {
        let mut s = open(&dir, 2);
        s.click_select(0);
        s.accept(); // entry 1
        s.click_select(1);
        s.reject(); // entry 2 (most recent)
        s.save().unwrap();
    }

    let mut s = open(&dir, 2);
    assert!(s.can_undo(), "undo stack restored from undo.log");
    assert_eq!(s.verdict_counts(), (1, 1, 0)); // two photos: 1 accepted, 1 rejected

    // Undo reverses the most recent action (the reject), not state from scratch.
    assert!(s.undo());
    assert_eq!(s.verdict_counts(), (1, 0, 1), "reject undone, accept intact");

    // And redo re-applies it.
    assert!(s.can_redo());
    assert!(s.redo());
    assert_eq!(s.verdict_counts(), (1, 1, 0));
}

#[test]
fn undo_log_keeps_appending_after_an_in_session_save() {
    // The autosave compacts `undo.log` mid-session while culling continues.
    // Actions after a save must still land in the durable on-disk log, not a
    // stale handle left on the pre-compaction inode (decision #18).
    let dir = temp_folder("log_after_save");
    write_jpeg(&dir, "a.jpg", 3);
    write_jpeg(&dir, "b.jpg", 4);

    let mut s = open(&dir, 2);
    s.click_select(0);
    s.accept(); // action A — logged, then compacted by save
    s.save().unwrap();
    s.click_select(1);
    s.reject(); // action B — appended after compaction; must be durable

    let stacks = dcs_io::undo_log::load(&dir.join(".dcs").join("undo.log")).unwrap();
    assert_eq!(stacks.undo.len(), 2, "post-save action is still durably logged");
}

#[test]
fn reopen_reclaims_the_same_photo_ids() {
    let dir = temp_folder("ids");
    write_jpeg(&dir, "a.jpg", 5);
    write_jpeg(&dir, "b.jpg", 6);
    let ids_before: Vec<u32> = {
        let mut s = open(&dir, 2);
        s.click_select(0);
        s.accept();
        s.save().unwrap();
        (0..s.pool_len()).map(|i| s.photo_at(i).unwrap().id.0).collect()
    };

    let s = open(&dir, 2);
    let ids_after: Vec<u32> = (0..s.pool_len()).map(|i| s.photo_at(i).unwrap().id.0).collect();
    assert_eq!(ids_before, ids_after, "ids are reclaimed by fingerprint, not reassigned");
}
