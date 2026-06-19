//! Burst derivation through a real scanned folder: synthesizes JPEGs carrying
//! EXIF `DateTimeOriginal` + `SubSecTimeOriginal` so the whole pipeline runs —
//! the io subsecond reader, per-group segmentation in the session, and the
//! per-cell `BurstMark` the grid paints from. Zones are pinned to UTC so day
//! grouping is deterministic regardless of the CI machine's zone.

use std::io::Cursor;
use std::path::{Path, PathBuf};
use std::thread::sleep;
use std::time::Duration;

use dcs_app::{BurstKnobs, Session, Sort, SortDir, SortKey, VerdictFilter};
use image::{Rgb, RgbImage};

fn temp_folder(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("dcs_burst_{}_{}", std::process::id(), tag));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// Write a JPEG with a little-endian EXIF block carrying `DateTimeOriginal`
/// (`dt`, exactly `YYYY:MM:DD HH:MM:SS`) and `SubSecTimeOriginal` (`subsec`, ≤3
/// digits so it inlines per the TIFF 4-byte value rule). The unique `seed`
/// perturbs the pixels so each file fingerprints distinctly.
fn exif_jpeg(dir: &Path, name: &str, dt: &str, subsec: &str, seed: u8) {
    assert_eq!(dt.len(), 19, "DateTimeOriginal must be YYYY:MM:DD HH:MM:SS");
    assert!(subsec.len() <= 3, "subsec must inline (≤3 digits)");

    let mut img = RgbImage::new(16, 16);
    for (x, y, px) in img.enumerate_pixels_mut() {
        *px = Rgb([x as u8 ^ seed, y as u8, seed]);
    }
    let mut jpeg = Vec::new();
    img.write_to(&mut Cursor::new(&mut jpeg), image::ImageFormat::Jpeg)
        .expect("encode jpeg");

    // TIFF block: header → IFD0 (one Exif-IFD pointer) → Exif IFD (the two time
    // tags) → the DateTimeOriginal string. Offsets are relative to the header.
    let mut tiff: Vec<u8> = Vec::new();
    tiff.extend(b"II");
    tiff.extend(42u16.to_le_bytes());
    tiff.extend(8u32.to_le_bytes()); // IFD0 at offset 8
    // IFD0: one entry, the Exif IFD pointer (tag 0x8769) → offset 26.
    tiff.extend(1u16.to_le_bytes());
    push_entry(&mut tiff, 0x8769, 4, 1, 26);
    tiff.extend(0u32.to_le_bytes());
    // Exif IFD at 26: DateTimeOriginal then SubSecTimeOriginal (tag-sorted).
    tiff.extend(2u16.to_le_bytes());
    push_entry(&mut tiff, 0x9003, 2, 20, 56); // ASCII[20] → string at offset 56
    let count = (subsec.len() + 1) as u32;
    let mut inline = [0u8; 4];
    inline[..subsec.len()].copy_from_slice(subsec.as_bytes());
    tiff.extend(0x9291u16.to_le_bytes());
    tiff.extend(2u16.to_le_bytes());
    tiff.extend(count.to_le_bytes());
    tiff.extend(inline);
    tiff.extend(0u32.to_le_bytes());
    assert_eq!(tiff.len(), 56);
    tiff.extend(dt.as_bytes());
    tiff.push(0);

    let mut app1 = Vec::new();
    app1.extend(b"Exif\0\0");
    app1.extend(&tiff);
    let seg_len = (app1.len() + 2) as u16;

    let mut out = Vec::new();
    out.extend(&jpeg[0..2]); // SOI
    out.extend([0xFF, 0xE1]); // APP1 marker
    out.extend(seg_len.to_be_bytes()); // JPEG segment length is big-endian
    out.extend(&app1);
    out.extend(&jpeg[2..]);
    std::fs::write(dir.join(name), out).expect("write jpeg");
}

fn push_entry(v: &mut Vec<u8>, tag: u16, typ: u16, count: u32, value: u32) {
    v.extend(tag.to_le_bytes());
    v.extend(typ.to_le_bytes());
    v.extend(count.to_le_bytes());
    v.extend(value.to_le_bytes());
}

fn drain_until(session: &mut Session, want: usize) {
    for _ in 0..3000 {
        session.tick();
        if session.photo_count() >= want && !session.is_scanning() {
            return;
        }
        sleep(Duration::from_millis(1));
    }
}

fn open(dir: PathBuf, want: usize) -> Session {
    let mut session = Session::new();
    session.open_folder(dir);
    drain_until(&mut session, want);
    // Pin both zones to UTC so day-grouping never drifts with the CI machine's
    // zone; either setter regroups, re-deriving bursts.
    session.set_camera_zone(Some("UTC".to_string()));
    session.set_shoot_zone(Some("UTC".to_string()));
    // The overlay is off by default; the derivation tests need it on.
    session.toggle_bursts();
    session
}

/// The burst mark for the photo at display index `idx`.
fn burst_at(session: &Session, idx: usize) -> Option<dcs_app::BurstMark> {
    session.cell_info(idx).expect("cell").burst
}

#[test]
fn subsecond_frames_in_one_second_form_a_burst() {
    let dir = temp_folder("subsec");
    // Three frames in the same clock second, sub-second apart, then a lone frame
    // hours later — all on one calendar day.
    exif_jpeg(&dir, "a.jpg", "2025:05:11 10:00:00", "10", 1);
    exif_jpeg(&dir, "b.jpg", "2025:05:11 10:00:00", "30", 2);
    exif_jpeg(&dir, "c.jpg", "2025:05:11 10:00:00", "60", 3);
    exif_jpeg(&dir, "d.jpg", "2025:05:11 18:00:00", "00", 4);
    let session = open(dir, 4);

    assert_eq!(session.burst_count(), 1);
    // a,b,c carry the run (chronological order = subsecond order); d is alone.
    let marks: Vec<_> = (0..4).map(|i| burst_at(&session, i)).collect();
    let in_burst: Vec<bool> = marks.iter().map(Option::is_some).collect();
    assert_eq!(in_burst, vec![true, true, true, false]);
    let ids: Vec<u32> = marks.iter().flatten().map(|m| m.id).collect();
    assert!(ids.iter().all(|&id| id == ids[0]), "one run, one id");
    let lens: Vec<usize> = marks.iter().flatten().map(|m| m.len).collect();
    assert!(lens.iter().all(|&l| l == 3));
    // Exactly one frame is the run's first (the label carrier) and one its last.
    assert_eq!(marks.iter().flatten().filter(|m| m.first).count(), 1);
    assert_eq!(marks.iter().flatten().filter(|m| m.last).count(), 1);
}

#[test]
fn bursts_segment_at_group_boundaries() {
    let dir = temp_folder("segment");
    // Three tight frames on day 1, three on day 2 → two day groups, one burst
    // each. A burst never spans a group boundary.
    exif_jpeg(&dir, "a.jpg", "2025:05:11 10:00:00", "10", 1);
    exif_jpeg(&dir, "b.jpg", "2025:05:11 10:00:00", "40", 2);
    exif_jpeg(&dir, "c.jpg", "2025:05:11 10:00:01", "10", 3);
    exif_jpeg(&dir, "d.jpg", "2025:05:12 10:00:00", "10", 4);
    exif_jpeg(&dir, "e.jpg", "2025:05:12 10:00:00", "40", 5);
    exif_jpeg(&dir, "f.jpg", "2025:05:12 10:00:01", "10", 6);
    let session = open(dir, 6);

    assert_eq!(session.burst_count(), 2);
    // Every frame is in a burst, but the two days carry distinct run ids.
    let ids: Vec<u32> = (0..6)
        .map(|i| burst_at(&session, i).expect("in a burst").id)
        .collect();
    let day1 = &ids[0..3];
    let day2 = &ids[3..6];
    assert!(day1.iter().all(|&id| id == day1[0]));
    assert!(day2.iter().all(|&id| id == day2[0]));
    assert_ne!(day1[0], day2[0], "a burst never spans two groups");
}

#[test]
fn knobs_re_derive_live_without_regroup() {
    let dir = temp_folder("knobs");
    exif_jpeg(&dir, "a.jpg", "2025:05:11 10:00:00", "10", 1);
    exif_jpeg(&dir, "b.jpg", "2025:05:11 10:00:00", "40", 2);
    exif_jpeg(&dir, "c.jpg", "2025:05:11 10:00:01", "10", 3);
    let mut session = open(dir, 3);
    assert_eq!(session.burst_count(), 1);

    // Raising the frame floor past the run dissolves the burst instantly.
    session.set_burst_knobs(BurstKnobs {
        min: 4,
        ..session.burst_knobs()
    });
    assert_eq!(session.burst_count(), 0);
    assert!((0..3).all(|i| burst_at(&session, i).is_none()));

    // Lowering it back restores the run.
    session.set_burst_knobs(BurstKnobs {
        min: 3,
        ..session.burst_knobs()
    });
    assert_eq!(session.burst_count(), 1);

    // Turning bursts off clears them entirely.
    session.set_burst_knobs(BurstKnobs {
        on: false,
        ..session.burst_knobs()
    });
    assert_eq!(session.burst_count(), 0);
    assert!((0..3).all(|i| burst_at(&session, i).is_none()));
}

#[test]
fn overlay_toggle_gates_and_persists_the_preference() {
    let dir = temp_folder("toggle");
    exif_jpeg(&dir, "a.jpg", "2025:05:11 10:00:00", "10", 1);
    exif_jpeg(&dir, "b.jpg", "2025:05:11 10:00:00", "40", 2);
    exif_jpeg(&dir, "c.jpg", "2025:05:11 10:00:01", "10", 3);
    let mut session = open(dir, 3); // `open` flips the overlay on

    // A fresh session hides bursts until asked.
    assert!(!Session::new().show_bursts());
    assert!(session.show_bursts());
    assert_eq!(session.burst_count(), 1);

    // Hiding clears the overlay entirely (no marks paint).
    session.toggle_bursts();
    assert!(!session.show_bursts());
    assert_eq!(session.burst_count(), 0);
    assert!((0..3).all(|i| burst_at(&session, i).is_none()));

    // Showing again re-derives.
    session.toggle_bursts();
    assert!(session.show_bursts());
    assert_eq!(session.burst_count(), 1);
}

#[test]
fn name_sort_suppresses_bursts() {
    let dir = temp_folder("namesort");
    exif_jpeg(&dir, "a.jpg", "2025:05:11 10:00:00", "10", 1);
    exif_jpeg(&dir, "b.jpg", "2025:05:11 10:00:00", "40", 2);
    exif_jpeg(&dir, "c.jpg", "2025:05:11 10:00:01", "10", 3);
    let mut session = open(dir, 3);
    assert_eq!(session.burst_count(), 1);

    // Bursts are chronological runs; under a name sort their frames need not be
    // contiguous in display order, so the overlay is suppressed wholesale.
    session.set_sort(Sort {
        key: SortKey::Name,
        dir: SortDir::Asc,
    });
    assert!(!session.bursts_available());
    assert_eq!(session.burst_count(), 0);
    assert!((0..3).all(|i| burst_at(&session, i).is_none()));

    // Returning to a time sort brings them back.
    session.set_sort(Sort {
        key: SortKey::Time,
        dir: SortDir::Asc,
    });
    assert_eq!(session.burst_count(), 1);
}

#[test]
fn descending_time_sort_anchors_label_on_the_leftmost_cell() {
    let dir = temp_folder("descsort");
    exif_jpeg(&dir, "a.jpg", "2025:05:11 10:00:00", "10", 1);
    exif_jpeg(&dir, "b.jpg", "2025:05:11 10:00:00", "40", 2);
    exif_jpeg(&dir, "c.jpg", "2025:05:11 10:00:01", "10", 3);
    let mut session = open(dir, 3);

    // Chronological order is a,b,c; descending display reverses to c,b,a. `first`
    // (the label/left-cap) must follow the *display* leftmost cell — index 0 —
    // not the chronological first, or the label lands on the right end.
    session.set_sort(Sort {
        key: SortKey::Time,
        dir: SortDir::Desc,
    });
    assert_eq!(session.burst_count(), 1);
    assert!(burst_at(&session, 0).expect("in burst").first);
    assert!(!burst_at(&session, 0).unwrap().last);
    assert!(burst_at(&session, 2).expect("in burst").last);
}

#[test]
fn no_bursts_under_the_tag_axis() {
    use dcs_app::Axis;
    let dir = temp_folder("tagaxis");
    exif_jpeg(&dir, "a.jpg", "2025:05:11 10:00:00", "10", 1);
    exif_jpeg(&dir, "b.jpg", "2025:05:11 10:00:00", "40", 2);
    exif_jpeg(&dir, "c.jpg", "2025:05:11 10:00:01", "10", 3);
    let mut session = open(dir, 3);
    assert_eq!(session.burst_count(), 1);

    // A tag band is not a timeline — bursts switch off under it.
    session.set_axis(Axis::Tag);
    assert_eq!(session.burst_count(), 0);
    assert!((0..session.photo_count()).all(|i| burst_at(&session, i).is_none()));
}

#[test]
fn rejected_frames_stay_in_their_burst() {
    let dir = temp_folder("reject");
    exif_jpeg(&dir, "a.jpg", "2025:05:11 10:00:00", "10", 1);
    exif_jpeg(&dir, "b.jpg", "2025:05:11 10:00:00", "40", 2);
    exif_jpeg(&dir, "c.jpg", "2025:05:11 10:00:01", "10", 3);
    let mut session = open(dir, 3);
    assert_eq!(session.burst_count(), 1);

    // Cull-by-burst: keep the first frame, reject the other two. Membership is by
    // photo, so the surviving accepted frame is still marked as a burst even
    // while the filter hides the rest.
    session.set_focus(0, false);
    session.accept();
    session.set_focus(1, false);
    session.set_focus(2, true); // extend selection over the two rejects
    session.reject();
    assert_eq!(session.burst_count(), 1);

    session.set_filter(VerdictFilter::Accepted);
    assert_eq!(session.photo_count(), 1);
    assert!(burst_at(&session, 0).is_some());
}
