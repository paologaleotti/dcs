//! Missing-file detection + reanimation (§4), config persistence (§4), and the
//! single-writer lock with read-only takeover (#34).

use std::path::{Path, PathBuf};
use std::thread::sleep;
use std::time::Duration;

use dcs_app::Session;
use image::{Rgb, RgbImage};

fn temp_folder(tag: &str) -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!("dcs_mlc_{tag}_{nanos}"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// Deterministic JPEG per seed — same seed re-creates identical bytes (so a
/// rewritten file keeps its content fingerprint).
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
    drive(&mut session, want);
    session
}

fn drive(session: &mut Session, want: usize) {
    for _ in 0..3000 {
        session.tick();
        if session.pool_len() >= want && !session.is_scanning() {
            break;
        }
        sleep(Duration::from_millis(1));
    }
}

fn missing_count(session: &Session) -> usize {
    (0..session.pool_len())
        .filter_map(|i| session.photo_at(i))
        .filter(|p| p.missing)
        .count()
}

#[test]
fn vanished_file_becomes_a_missing_placeholder_preserving_state() {
    let dir = temp_folder("missing");
    write_jpeg(&dir, "keep.jpg", 10);
    write_jpeg(&dir, "gone.jpg", 20);
    {
        let mut s = open(&dir, 2);
        // Reject the one we're about to delete.
        let idx = (0..s.pool_len())
            .find(|&i| s.photo_at(i).unwrap().file_name() == "gone.jpg")
            .unwrap();
        s.click_select(idx);
        s.reject();
        s.save().unwrap();
    }

    std::fs::remove_file(dir.join("gone.jpg")).unwrap();

    let s = open(&dir, 2);
    assert_eq!(s.pool_len(), 2, "the vanished photo is still present as a placeholder");
    assert_eq!(missing_count(&s), 1);
    // Its rejected verdict is preserved (1 rejected counted across both photos).
    let (_acc, rej, _unrev) = s.verdict_counts();
    assert_eq!(rej, 1, "missing photo keeps its verdict");
}

#[test]
fn returned_file_reanimates() {
    let dir = temp_folder("reanimate");
    write_jpeg(&dir, "a.jpg", 30);
    {
        let mut s = open(&dir, 1);
        s.click_select(0);
        s.accept();
        s.save().unwrap();
    }
    std::fs::remove_file(dir.join("a.jpg")).unwrap();
    {
        let s = open(&dir, 1);
        assert_eq!(missing_count(&s), 1, "gone while absent");
    }

    // Restore identical content → same fingerprint → reanimates.
    write_jpeg(&dir, "a.jpg", 30);
    let s = open(&dir, 1);
    assert_eq!(missing_count(&s), 0, "file returned, placeholder reanimated");
    assert_eq!(s.verdict_counts().0, 1, "verdict intact through the round-trip");
}

#[test]
fn forget_missing_prunes_the_photo_and_its_state() {
    let dir = temp_folder("forget");
    write_jpeg(&dir, "keep.jpg", 10);
    write_jpeg(&dir, "gone.jpg", 20);
    {
        let mut s = open(&dir, 2);
        let idx = (0..s.pool_len())
            .find(|&i| s.photo_at(i).unwrap().file_name() == "gone.jpg")
            .unwrap();
        s.click_select(idx);
        s.reject();
        s.save().unwrap();
    }
    std::fs::remove_file(dir.join("gone.jpg")).unwrap();

    let mut s = open(&dir, 2);
    assert_eq!(s.missing_count(), 1);
    let removed = s.forget_missing();
    assert_eq!(removed, 1);
    assert_eq!(s.pool_len(), 1, "missing photo dropped from the pool");
    assert_eq!(s.missing_count(), 0);
    assert_eq!(s.verdict_counts().1, 0, "its rejected verdict is gone too");
    s.save().unwrap();

    // After save+reopen it stays gone — the prune is durable, no resurrection.
    let s = open(&dir, 1);
    assert_eq!(s.pool_len(), 1);
    assert_eq!(s.missing_count(), 0);
}

#[test]
fn forgotten_file_returning_comes_back_as_a_fresh_unreviewed_photo() {
    let dir = temp_folder("forget_return");
    write_jpeg(&dir, "a.jpg", 30);
    {
        let mut s = open(&dir, 1);
        s.click_select(0);
        s.accept();
        s.save().unwrap();
    }
    std::fs::remove_file(dir.join("a.jpg")).unwrap();
    {
        let mut s = open(&dir, 1);
        assert_eq!(s.forget_missing(), 1);
        s.save().unwrap();
    }

    // The user forgot it; if the file reappears it's a brand-new, unreviewed
    // photo — not a resurrection of the old verdict.
    write_jpeg(&dir, "a.jpg", 30);
    let s = open(&dir, 1);
    assert_eq!(s.pool_len(), 1);
    assert_eq!(s.verdict_counts(), (0, 0, 1), "returns unreviewed, not accepted");
}

#[test]
fn missing_photos_issue_no_decode_requests() {
    // A missing file keeps its stored path, so its decode would always fail and
    // never cache — re-requesting every frame. `request_base`/`request_hires`
    // must skip it (§4).
    let dir = temp_folder("missing_no_decode");
    write_jpeg(&dir, "gone.jpg", 14);
    {
        let mut s = open(&dir, 1);
        s.save().unwrap();
    }
    std::fs::remove_file(dir.join("gone.jpg")).unwrap();

    let mut s = open(&dir, 1);
    assert_eq!(s.missing_count(), 1);
    s.request_base(0);
    s.request_hires(0, 512);
    assert_eq!(s.decode_queue_depth(), 0, "missing files never enter the decode pool");
}

#[test]
fn grid_zoom_persists_across_reopen() {
    let dir = temp_folder("zoom");
    write_jpeg(&dir, "a.jpg", 7);
    {
        let mut s = open(&dir, 1);
        s.set_grid_zoom(212.0);
        s.save().unwrap();
    }
    let s = open(&dir, 1);
    assert_eq!(s.grid_zoom(), Some(212.0), "config round-trips");
}

#[test]
fn shoot_zone_persists_across_reopen() {
    let dir = temp_folder("zone");
    write_jpeg(&dir, "a.jpg", 8);
    {
        let mut s = open(&dir, 1);
        s.set_shoot_zone(Some("Asia/Tokyo".to_string()));
        s.save().unwrap();
    }
    let s = open(&dir, 1);
    assert_eq!(s.shoot_zone(), Some("Asia/Tokyo"));
}

#[test]
fn second_instance_is_read_only_and_can_take_over() {
    let dir = temp_folder("lock");
    write_jpeg(&dir, "a.jpg", 9);

    // First session holds the write lock for the whole test.
    let mut first = open(&dir, 1);
    first.click_select(0);
    first.accept();

    // Second session on the same folder opens read-only.
    let mut second = open(&dir, 1);
    assert!(second.is_read_only(), "a live first instance forces read-only");

    // Mutations are blocked while read-only.
    let before = second.verdict_counts();
    second.click_select(0);
    second.reject();
    assert_eq!(second.verdict_counts(), before, "read-only blocks culling");
    assert!(!second.undo(), "read-only blocks undo");

    // Take over → writable.
    second.take_over();
    assert!(!second.is_read_only());
    second.click_select(0);
    second.reject();
    assert_eq!(second.verdict_counts().1, 1, "writable after take over");

    drop(first);
}

#[test]
fn reopening_or_rescanning_the_same_folder_stays_writable() {
    let dir = temp_folder("reopen");
    write_jpeg(&dir, "a.jpg", 1);
    let mut s = open(&dir, 1);
    assert!(!s.is_read_only());

    // Reopen the very same folder: our own lock must be released and reclaimed,
    // not mistaken for a live second instance.
    s.open_folder(dir.clone());
    drive(&mut s, 1);
    assert!(!s.is_read_only(), "reopening our own folder must stay writable");

    // Rescan goes through the same path.
    s.rescan();
    drive(&mut s, 1);
    assert!(!s.is_read_only(), "rescan must stay writable");

    // Still actually writable.
    s.click_select(0);
    s.accept();
    assert_eq!(s.verdict_counts().0, 1);
}

#[test]
fn opening_records_a_recent_project() {
    let dir = temp_folder("recents");
    write_jpeg(&dir, "a.jpg", 5);

    // Point recents at an isolated temp file so the test never pollutes the
    // user's real ~/.dcs/recents.json.
    let mut s = Session::new();
    s.set_recents_path(Some(dir.join("recents.json")));
    s.open_folder(dir.clone());
    drive(&mut s, 1);

    assert!(
        s.recent_projects().iter().any(|p| p == &dir),
        "the opened folder is remembered in recents"
    );
}
