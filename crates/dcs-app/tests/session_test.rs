use std::path::{Path, PathBuf};
use std::thread::sleep;
use std::time::Duration;

use dcs_app::{Session, VerdictFilter};
use dcs_domain::cull::AcceptState;
use image::{Rgb, RgbImage};

fn temp_folder(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("dcs_session_{}_{}", std::process::id(), tag));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn touch(dir: &Path, name: &str) {
    std::fs::write(dir.join(name), b"placeholder").unwrap();
}

fn write_jpeg(dir: &Path, name: &str, w: u32, h: u32) {
    let mut img = RgbImage::new(w, h);
    for (x, y, px) in img.enumerate_pixels_mut() {
        *px = Rgb([(x % 256) as u8, (y % 256) as u8, 128]);
    }
    img.save(dir.join(name)).expect("encode jpeg");
}

/// Drive `tick` until the pool reaches `want` photos or the budget runs out.
fn drain_until(session: &mut Session, want: usize) {
    for _ in 0..3000 {
        session.tick();
        if session.photo_count() >= want && !session.is_scanning() {
            return;
        }
        sleep(Duration::from_millis(1));
    }
}

/// Drive `tick` (re-issuing `request` each pass) until `done` holds or the
/// budget runs out — for async decode results.
fn pump_until(session: &mut Session, mut request: impl FnMut(&mut Session), done: impl Fn(&Session) -> bool) {
    for _ in 0..3000 {
        request(session);
        session.tick();
        if done(session) {
            return;
        }
        sleep(Duration::from_millis(1));
    }
}

#[test]
fn scans_jpegs_and_ignores_non_images() {
    let dir = temp_folder("scan");
    touch(&dir, "a.jpg");
    touch(&dir, "b.JPG");
    touch(&dir, "notes.txt");
    touch(&dir, "c.raf"); // RAW: recognized by domain, not imported yet

    let mut session = Session::new();
    session.open_folder(dir.clone());
    drain_until(&mut session, 2);

    assert_eq!(session.photo_count(), 2, "only the two JPEGs import");
    assert!(!session.is_scanning());
    assert!(session.cell_info(0).is_some());
    assert!(session.cell_info(2).is_none());

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn opening_a_new_folder_replaces_the_previous_pool() {
    let first = temp_folder("first");
    touch(&first, "x.jpg");
    touch(&first, "y.jpg");
    touch(&first, "z.jpg");

    let second = temp_folder("second");
    touch(&second, "only.jpg");

    let mut session = Session::new();
    session.open_folder(first.clone());
    drain_until(&mut session, 3);
    assert_eq!(session.photo_count(), 3);

    session.open_folder(second.clone());
    drain_until(&mut session, 1);
    assert_eq!(session.photo_count(), 1, "pool reset to the new folder");

    let _ = std::fs::remove_dir_all(&first);
    let _ = std::fs::remove_dir_all(&second);
}

#[test]
fn requesting_thumbnails_never_panics_and_skips_unloadable() {
    let dir = temp_folder("thumbs");
    touch(&dir, "a.jpg");

    let mut session = Session::new();
    session.open_folder(dir.clone());
    drain_until(&mut session, 1);

    // Placeholder bytes can't decode, but base + hi-res requests must be safe
    // no-ops and both must retire their in-flight entries.
    session.request_base(0);
    session.request_hires(0, 512);
    // Let the decode workers run and the terminal results drain the requests.
    for _ in 0..500 {
        session.tick();
        if !session.has_pending() {
            break;
        }
        sleep(Duration::from_millis(1));
    }
    assert_eq!(session.loaded_count(), 0);
    assert_eq!(session.hires_count(), 0);
    assert!(!session.has_pending(), "failed decodes still retire their requests");

    // Dropping hi-res is always safe, even when empty.
    session.clear_hires();
    assert_eq!(session.hires_count(), 0);

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn base_thumbnails_decode_after_scan() {
    let dir = temp_folder("base");
    write_jpeg(&dir, "a.jpg", 400, 300);
    write_jpeg(&dir, "b.jpg", 300, 400);

    let mut session = Session::new();
    session.open_folder(dir.clone());
    drain_until(&mut session, 2);
    pump_until(
        &mut session,
        |s| {
            s.request_base(0);
            s.request_base(1);
        },
        |s| s.loaded_count() >= 2,
    );

    assert_eq!(session.loaded_count(), 2);
    let id = session.cell_info(0).unwrap().id;
    assert!(session.thumb(id).is_some());

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn background_fill_decodes_the_whole_folder_without_viewport_requests() {
    let dir = temp_folder("bgfill");
    for i in 0..5 {
        write_jpeg(&dir, &format!("p{i}.jpg"), 320, 240);
    }

    let mut session = Session::new();
    session.open_folder(dir.clone());
    drain_until(&mut session, 5);

    // Only the background fill drives decoding — no per-cell request_base.
    pump_until(&mut session, |s| s.fill_base_background(), |s| s.loaded_count() >= 5);
    assert_eq!(session.loaded_count(), 5, "every photo's base decodes in the background");

    let _ = std::fs::remove_dir_all(&dir);
}

/// Open a folder of `n` plain JPEGs (a.jpg, b.jpg, …) and drain the scan. The
/// pool needs no decode for cull tests, only pairing.
fn opened_with(n: usize, tag: &str) -> (Session, PathBuf) {
    let dir = temp_folder(tag);
    for i in 0..n {
        let name = format!("{}.jpg", (b'a' + i as u8) as char);
        write_jpeg(&dir, &name, 80, 80);
    }
    let mut session = Session::new();
    session.open_folder(dir.clone());
    drain_until(&mut session, n);
    (session, dir)
}

#[test]
fn accept_toggles_off_focus_and_undo_redo_round_trip() {
    let (mut session, dir) = opened_with(3, "cull_toggle");

    session.nav(1, 0, 3, false); // first arrow grabs the cursor at index 0
    let id0 = session.cell_info(0).unwrap().id;
    assert_eq!(session.verdict(id0), AcceptState::Unreviewed);

    session.accept();
    assert_eq!(session.verdict(id0), AcceptState::Accepted);
    assert!(session.is_selected(id0));
    assert!(session.can_undo());

    session.accept(); // focus is accepted → toggles back to unreviewed
    assert_eq!(session.verdict(id0), AcceptState::Unreviewed);

    assert!(session.undo()); // reverse the toggle-back
    assert_eq!(session.verdict(id0), AcceptState::Accepted);
    assert!(session.redo());
    assert_eq!(session.verdict(id0), AcceptState::Unreviewed);

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn filter_routes_visible_and_undo_rebuilds_it() {
    let (mut session, dir) = opened_with(3, "cull_filter");

    session.nav(1, 0, 3, false); // focus index 0
    session.accept(); // one photo accepted, two unreviewed

    assert_eq!(session.pool_len(), 3, "pool size ignores the filter");
    session.set_filter(VerdictFilter::Accepted);
    assert_eq!(session.photo_count(), 1);
    session.set_filter(VerdictFilter::Unreviewed);
    assert_eq!(session.photo_count(), 2);

    // Undo un-accepts the photo; under the unreviewed view it reappears.
    assert!(session.undo());
    assert_eq!(session.photo_count(), 3, "undo rebuilds the visible order");

    session.set_filter(VerdictFilter::All);
    assert_eq!(session.photo_count(), 3);

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn select_all_then_reject_marks_every_visible_photo() {
    let (mut session, dir) = opened_with(3, "cull_reject_all");

    session.select_all_visible();
    assert_eq!(session.selection_count(), 3);

    session.reject(); // focus is unreviewed → rejects the whole selection
    for i in 0..3 {
        let id = session.cell_info(i).unwrap().id;
        assert_eq!(session.verdict(id), AcceptState::Rejected);
    }
    let (accepted, rejected, unreviewed) = session.verdict_counts();
    assert_eq!((accepted, rejected, unreviewed), (0, 3, 0));

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn opening_a_new_folder_clears_verdicts_and_selection() {
    let (mut session, first) = opened_with(2, "cull_reset_a");
    session.select_all_visible();
    session.accept();
    assert_eq!(session.verdict_counts().0, 2);

    let second = temp_folder("cull_reset_b");
    write_jpeg(&second, "z.jpg", 80, 80);
    session.open_folder(second.clone());
    drain_until(&mut session, 1);

    // Ids restart at 0 per folder; prior verdicts/selection must not bleed.
    assert_eq!(session.verdict_counts(), (0, 0, 1));
    assert_eq!(session.selection_count(), 0);
    assert_eq!(session.focus(), None);

    let _ = std::fs::remove_dir_all(&first);
    let _ = std::fs::remove_dir_all(&second);
}

#[test]
fn hi_res_upgrades_then_clears_back_to_base() {
    let dir = temp_folder("hires");
    write_jpeg(&dir, "big.jpg", 1000, 800);

    let mut session = Session::new();
    session.open_folder(dir.clone());
    drain_until(&mut session, 1);
    pump_until(&mut session, |s| s.request_base(0), |s| s.loaded_count() >= 1);

    let id = session.cell_info(0).unwrap().id;
    let base_version = session.thumb(id).unwrap().version;

    pump_until(&mut session, |s| s.request_hires(0, 512), |s| s.hires_count() >= 1);
    assert_eq!(session.hires_count(), 1);
    let view = session.thumb(id).unwrap();
    assert!(view.version != base_version, "hi-res is a newer version than base");
    assert!(view.image.width.max(view.image.height) > 256, "hi-res is sharper than base");

    // Zoom-out drops hi-res RAM; the base thumbnail still displays.
    session.clear_hires();
    assert_eq!(session.hires_count(), 0);
    let after = session.thumb(id).unwrap();
    assert_eq!(after.version, base_version, "falls back to the base thumbnail");

    let _ = std::fs::remove_dir_all(&dir);
}
