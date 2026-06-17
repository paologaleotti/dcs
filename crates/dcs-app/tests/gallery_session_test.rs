//! Session-level gallery decode (§2.13): the large-frame request path decodes
//! the focused photo and preloads neighbours, dedups repeat requests, and frees
//! its frames on `clear_gallery`. Drives the real rayon decoder over a temp
//! folder of small JPEGs.

use std::path::{Path, PathBuf};
use std::thread::sleep;
use std::time::Duration;

use dcs_app::Session;
use image::{Rgb, RgbImage};

fn temp_folder(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("dcs_gallery_{}_{}", std::process::id(), tag));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn write_jpeg(dir: &Path, name: &str) {
    let mut img = RgbImage::new(64, 64);
    for (x, y, px) in img.enumerate_pixels_mut() {
        *px = Rgb([(x % 256) as u8, (y % 256) as u8, 96]);
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

/// Tick until `f` holds, draining decode results each frame, or panic.
fn pump(session: &mut Session, mut f: impl FnMut(&mut Session) -> bool) {
    for _ in 0..3000 {
        session.tick();
        if f(session) {
            return;
        }
        sleep(Duration::from_millis(1));
    }
    panic!("gallery decode did not settle");
}

#[test]
fn request_gallery_decodes_focus_and_preloads_neighbours() {
    let (mut session, dir) = opened_with(3, "decode");
    let id0 = session.photo_at(0).unwrap().id;
    let id1 = session.photo_at(1).unwrap().id;

    session.request_gallery(0, 512);
    pump(&mut session, |s| s.gallery_image(id0).is_some());

    assert!(session.gallery_image(id0).is_some(), "focus frame decoded");
    // The neighbour at index 1 is preloaded by the same call.
    pump(&mut session, |s| s.gallery_image(id1).is_some());
    assert!(session.gallery_image(id1).is_some(), "neighbour preloaded");

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn smaller_repeat_request_does_not_drop_a_resident_frame() {
    let (mut session, dir) = opened_with(1, "dedup");
    let id0 = session.photo_at(0).unwrap().id;

    session.request_gallery(0, 1024);
    pump(&mut session, |s| s.gallery_image(id0).is_some());

    // A later, smaller request is already satisfied: the frame stays resident
    // and nothing new goes in flight.
    session.request_gallery(0, 256);
    session.tick();
    assert!(session.gallery_image(id0).is_some());
    assert!(!session.has_gallery_pending());

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn clear_gallery_frees_the_frames() {
    let (mut session, dir) = opened_with(2, "clear");
    let id0 = session.photo_at(0).unwrap().id;

    session.request_gallery(0, 512);
    pump(&mut session, |s| s.gallery_image(id0).is_some());

    session.clear_gallery();
    assert!(
        session.gallery_image(id0).is_none(),
        "frames dropped on clear"
    );
    assert!(!session.has_gallery_pending());

    let _ = std::fs::remove_dir_all(&dir);
}
