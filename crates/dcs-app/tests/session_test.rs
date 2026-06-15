use std::path::{Path, PathBuf};
use std::thread::sleep;
use std::time::Duration;

use dcs_app::Session;
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
