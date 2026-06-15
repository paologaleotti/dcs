use std::path::{Path, PathBuf};

use dcs_domain::pairing::{FileKind, ScannedFile, classify, pair};
use dcs_domain::photo::{Orientation, PhotoType};

fn file(path: &str, kind: FileKind) -> ScannedFile {
    ScannedFile {
        path: PathBuf::from(path),
        kind,
        orientation: Orientation::Normal,
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
    let jpeg = pool.photos().iter().find(|p| p.file_name() == "only.JPG").unwrap();
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
    for ext in ["raf", "cr2", "cr3", "nef", "arw", "dng", "orf", "rw2", "pef"] {
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
fn orientation_prefers_the_jpeg() {
    let pool = pair([
        ScannedFile {
            path: PathBuf::from("a/x.RAF"),
            kind: FileKind::Raw,
            orientation: Orientation::Rotate90,
            captured_at: None,
        },
        ScannedFile {
            path: PathBuf::from("a/x.JPG"),
            kind: FileKind::Jpeg,
            orientation: Orientation::Normal,
            captured_at: None,
        },
    ]);
    assert_eq!(pool.photos()[0].orientation, Orientation::Normal);
}
