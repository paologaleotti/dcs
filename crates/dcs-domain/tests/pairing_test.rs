use std::collections::HashMap;
use std::path::{Path, PathBuf};

use dcs_domain::fingerprint::ContentFingerprint;
use dcs_domain::pairing::{FileKind, PoolBuilder, ScannedFile, classify, pair};
use dcs_domain::photo::{Orientation, PhotoId, PhotoType};

/// A distinct, deterministic fingerprint per string — lets tests assert id
/// reclaim by content without computing real hashes.
fn fp(seed: &str) -> ContentFingerprint {
    let mut bytes = [0u8; 32];
    for (i, b) in seed.bytes().enumerate() {
        bytes[i % 32] ^= b.wrapping_add(i as u8).wrapping_add(1);
    }
    ContentFingerprint::from_bytes(bytes)
}

fn file(path: &str, kind: FileKind) -> ScannedFile {
    ScannedFile {
        path: PathBuf::from(path),
        kind,
        orientation: Orientation::Normal,
        fingerprint: fp(path),
        captured_at: None,
    }
}

#[test]
fn classify_recognizes_jpeg_and_raw_case_insensitively() {
    assert_eq!(classify(Path::new("a/DSCF1.JPG")), Some(FileKind::Jpeg));
    assert_eq!(classify(Path::new("a/b.jpeg")), Some(FileKind::Jpeg));
    assert_eq!(classify(Path::new("a/DSCF1.RAF")), Some(FileKind::Raw));
    assert_eq!(classify(Path::new("a/x.cr3")), Some(FileKind::Raw));
    assert_eq!(classify(Path::new("a/notes.txt")), None);
    assert_eq!(classify(Path::new("a/noext")), None);
}

#[test]
fn jpeg_and_raw_same_stem_pair_into_one_both_photo() {
    let pool = pair([
        file("trip/DSCF1234.JPG", FileKind::Jpeg),
        file("trip/DSCF1234.RAF", FileKind::Raw),
    ]);
    assert_eq!(pool.len(), 1);
    let photo = &pool.photos()[0];
    assert_eq!(photo.photo_type, PhotoType::Both);
    assert!(photo.files.jpeg.is_some());
    assert!(photo.files.raw.is_some());
    assert_eq!(photo.decodable_path(), photo.files.jpeg.as_deref());
}

#[test]
fn pairing_arrival_order_does_not_matter() {
    let raw_first = pair([
        file("trip/DSCF1.RAF", FileKind::Raw),
        file("trip/DSCF1.JPG", FileKind::Jpeg),
    ]);
    assert_eq!(raw_first.len(), 1);
    assert_eq!(raw_first.photos()[0].photo_type, PhotoType::Both);
}

#[test]
fn same_stem_in_different_folders_stays_separate() {
    let pool = pair([
        file("day1/DSCF1.JPG", FileKind::Jpeg),
        file("day2/DSCF1.JPG", FileKind::Jpeg),
    ]);
    assert_eq!(pool.len(), 2);
}

#[test]
fn lone_jpeg_and_lone_raw_get_correct_types() {
    let pool = pair([
        file("a/only.JPG", FileKind::Jpeg),
        file("a/only_raw.RAF", FileKind::Raw),
    ]);
    let jpeg = pool
        .photos()
        .iter()
        .find(|p| p.file_name() == "only.JPG")
        .unwrap();
    assert_eq!(jpeg.photo_type, PhotoType::Jpeg);
    let raw = pool.photos().iter().find(|p| p.is_raw_only()).unwrap();
    assert_eq!(raw.photo_type, PhotoType::Raw);
    assert_eq!(raw.decodable_path(), None);
}

#[test]
fn ids_are_assigned_in_first_appearance_order() {
    let pool = pair([
        file("a/b.JPG", FileKind::Jpeg),
        file("a/a.JPG", FileKind::Jpeg),
    ]);
    assert_eq!(pool.photos()[0].id.0, 0);
    assert_eq!(pool.photos()[0].file_name(), "b.JPG");
    assert_eq!(pool.photos()[1].id.0, 1);
}

#[test]
fn classify_covers_many_raw_extensions() {
    for ext in [
        "raf", "cr2", "cr3", "nef", "arw", "dng", "orf", "rw2", "pef",
    ] {
        let p = format!("a/x.{}", ext.to_uppercase());
        assert_eq!(classify(Path::new(&p)), Some(FileKind::Raw), "ext {ext}");
    }
}

#[test]
fn feeding_the_same_file_twice_is_idempotent() {
    let pool = pair([
        file("a/x.JPG", FileKind::Jpeg),
        file("a/x.JPG", FileKind::Jpeg),
    ]);
    assert_eq!(pool.len(), 1);
    assert_eq!(pool.photos()[0].photo_type, PhotoType::Jpeg);
}

#[test]
fn two_jpeg_extensions_same_stem_collapse_to_one_photo() {
    let pool = pair([
        file("a/x.jpg", FileKind::Jpeg),
        file("a/x.jpeg", FileKind::Jpeg),
    ]);
    assert_eq!(pool.len(), 1);
    assert_eq!(pool.photos()[0].photo_type, PhotoType::Jpeg);
}

#[test]
fn empty_input_yields_empty_pool() {
    let pool = pair(std::iter::empty());
    assert!(pool.is_empty());
}

#[test]
fn both_photo_takes_the_jpeg_fingerprint() {
    // Identity follows the display file, so a JPEG+RAW pair is keyed on the
    // JPEG's fingerprint regardless of arrival order (§10b).
    let pool = pair([
        file("trip/DSCF1.RAF", FileKind::Raw),
        file("trip/DSCF1.JPG", FileKind::Jpeg),
    ]);
    assert_eq!(pool.photos()[0].fingerprint, fp("trip/DSCF1.JPG"));
}

#[test]
fn seeded_builder_reclaims_id_by_fingerprint() {
    // A file renamed on disk arrives with a new path but the same content, so
    // it must reclaim its old id (the app then restores its verdict). The new
    // file carries the *persisted* photo's fingerprint, modelling identical
    // bytes under a new name.
    let mut builder = PoolBuilder::seeded(HashMap::from([(fp("DSCF1"), PhotoId(42))]), 100);
    builder.add(ScannedFile {
        path: PathBuf::from("trip/RENAMED.JPG"),
        kind: FileKind::Jpeg,
        orientation: Orientation::Normal,
        fingerprint: fp("DSCF1"),
        captured_at: None,
    });
    let pool = builder.to_pool();
    assert_eq!(pool.photos()[0].id, PhotoId(42));
    // Reclaim must not advance the counter.
    assert_eq!(builder.next_id(), 100);
}

#[test]
fn seeded_builder_assigns_fresh_id_for_unknown_fingerprint() {
    let mut builder = PoolBuilder::seeded(HashMap::from([(fp("old"), PhotoId(7))]), 100);
    builder.add(file("trip/NEW.JPG", FileKind::Jpeg));
    let pool = builder.to_pool();
    assert_eq!(pool.photos()[0].id, PhotoId(100));
    assert_eq!(builder.next_id(), 101);
}

#[test]
fn duplicate_content_does_not_reuse_one_seeded_id_twice() {
    // Two files with identical content but different names both match the seed;
    // consuming the seed entry means only the first reclaims it.
    let mut builder = PoolBuilder::seeded(HashMap::from([(fp("dup"), PhotoId(5))]), 100);
    let dup = |path: &str| ScannedFile {
        path: PathBuf::from(path),
        kind: FileKind::Jpeg,
        orientation: Orientation::Normal,
        fingerprint: fp("dup"),
        captured_at: None,
    };
    builder.add(dup("a/one.JPG"));
    builder.add(dup("a/two.JPG"));
    let pool = builder.to_pool();
    assert_eq!(pool.len(), 2);
    let ids: Vec<u32> = pool.photos().iter().map(|p| p.id.0).collect();
    assert!(ids.contains(&5), "first keeps the reclaimed id");
    assert!(ids.contains(&100), "second gets a fresh id, no collision");
}

#[test]
fn add_missing_creates_a_placeholder_with_the_seeded_id() {
    let mut builder = PoolBuilder::seeded(HashMap::from([(fp("gone"), PhotoId(77))]), 100);
    let added = builder.add_missing(fp("gone"), Some(PathBuf::from("trip/gone.jpg")), None);
    assert!(added);
    let pool = builder.to_pool();
    assert_eq!(pool.photos()[0].id, PhotoId(77));
    assert!(pool.photos()[0].missing);
    assert_eq!(pool.photos()[0].file_name(), "gone.jpg");
}

#[test]
fn add_missing_skips_files_already_present() {
    // The file was scanned (its fingerprint consumed), so it is not missing.
    let mut builder = PoolBuilder::seeded(HashMap::from([(fp("c"), PhotoId(3))]), 100);
    builder.add(ScannedFile {
        path: PathBuf::from("trip/here.jpg"),
        kind: FileKind::Jpeg,
        orientation: Orientation::Normal,
        fingerprint: fp("c"),
        captured_at: None,
    });
    // Now the fingerprint is consumed → add_missing must refuse.
    assert!(!builder.add_missing(fp("c"), Some(PathBuf::from("trip/here.jpg")), None));
    assert_eq!(builder.to_pool().len(), 1);
    assert!(!builder.to_pool().photos()[0].missing);
}

#[test]
fn orientation_prefers_the_jpeg() {
    let pool = pair([
        ScannedFile {
            path: PathBuf::from("a/x.RAF"),
            kind: FileKind::Raw,
            orientation: Orientation::Rotate90,
            fingerprint: fp("a/x.RAF"),
            captured_at: None,
        },
        ScannedFile {
            path: PathBuf::from("a/x.JPG"),
            kind: FileKind::Jpeg,
            orientation: Orientation::Normal,
            fingerprint: fp("a/x.JPG"),
            captured_at: None,
        },
    ]);
    assert_eq!(pool.photos()[0].orientation, Orientation::Normal);
}
