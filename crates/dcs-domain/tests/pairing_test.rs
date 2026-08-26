use std::collections::HashMap;
use std::path::{Path, PathBuf};

use dcs_domain::fingerprint::ContentFingerprint;
use dcs_domain::pairing::{FileKind, PoolBuilder, ScannedFile, classify, pair};
use dcs_domain::photo::{CaptureMeta, Orientation, PhotoId, PhotoType};

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
        captured_approx: false,
        captured_offset: None,
        meta: CaptureMeta::default(),
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
    let mut builder = PoolBuilder::seeded(
        HashMap::from([(fp("DSCF1"), PhotoId(42))]),
        HashMap::new(),
        100,
    );
    builder.add(ScannedFile {
        path: PathBuf::from("trip/RENAMED.JPG"),
        kind: FileKind::Jpeg,
        orientation: Orientation::Normal,
        fingerprint: fp("DSCF1"),
        captured_at: None,
        captured_approx: false,
        captured_offset: None,
        meta: CaptureMeta::default(),
    });
    let pool = builder.to_pool();
    assert_eq!(pool.photos()[0].id, PhotoId(42));
    // Reclaim must not advance the counter.
    assert_eq!(builder.next_id(), 100);
}

#[test]
fn jpeg_joining_a_raw_first_photo_reclaims_the_seeded_id() {
    // A JPEG+RAW pair was persisted under the JPEG's fingerprint as id 42. On
    // re-scan the RAW is classified first (parallel scan, arbitrary order): it
    // takes a tentative fresh id, then the JPEG merges and must reclaim id 42.
    let jpeg_fp = fp("trip/DSCF1.JPG");
    let mut builder =
        PoolBuilder::seeded(HashMap::from([(jpeg_fp, PhotoId(42))]), HashMap::new(), 100);

    builder.add(file("trip/DSCF1.RAF", FileKind::Raw)); // RAW first → fresh id 100
    builder.add(file("trip/DSCF1.JPG", FileKind::Jpeg)); // JPEG merges → reclaim 42

    let pool = builder.to_pool();
    assert_eq!(pool.len(), 1, "the pair is one photo, not two");
    assert_eq!(
        pool.photos()[0].id,
        PhotoId(42),
        "the JPEG's saved id is reclaimed despite the RAW arriving first"
    );
    assert_eq!(pool.photos()[0].photo_type, PhotoType::Both);
    assert_eq!(pool.photos()[0].fingerprint, jpeg_fp);

    // The seed entry was consumed by the reclaim, so the persisted photo can no
    // longer be mistaken for a missing file — the phantom-placeholder bug.
    assert!(
        !builder.add_missing(jpeg_fp, Some(PathBuf::from("trip/DSCF1.JPG")), None),
        "seed consumed → no phantom missing placeholder"
    );
}

#[test]
fn seeded_builder_assigns_fresh_id_for_unknown_fingerprint() {
    let mut builder = PoolBuilder::seeded(
        HashMap::from([(fp("old"), PhotoId(7))]),
        HashMap::new(),
        100,
    );
    builder.add(file("trip/NEW.JPG", FileKind::Jpeg));
    let pool = builder.to_pool();
    assert_eq!(pool.photos()[0].id, PhotoId(100));
    assert_eq!(builder.next_id(), 101);
}

#[test]
fn duplicate_content_does_not_reuse_one_seeded_id_twice() {
    // Two files with identical content but different names both match the seed;
    // consuming the seed entry means only the first reclaims it.
    let mut builder = PoolBuilder::seeded(
        HashMap::from([(fp("dup"), PhotoId(5))]),
        HashMap::new(),
        100,
    );
    let dup = |path: &str| ScannedFile {
        path: PathBuf::from(path),
        kind: FileKind::Jpeg,
        orientation: Orientation::Normal,
        fingerprint: fp("dup"),
        captured_at: None,
        captured_approx: false,
        captured_offset: None,
        meta: CaptureMeta::default(),
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
fn changed_content_at_a_known_path_reclaims_that_id() {
    // The bytes at a persisted path no longer hash to what was saved: an in-place
    // edit, or dcs changing how it fingerprints this kind of file. The photo is
    // the same slot in the shoot, so it keeps its id — and the stale fingerprint
    // entry must not also come back as a missing placeholder.
    let mut builder = PoolBuilder::seeded(
        HashMap::from([(fp("was"), PhotoId(9))]),
        HashMap::from([(PathBuf::from("trip/DSCF1.RAF"), PhotoId(9))]),
        100,
    );
    let mut scanned = file("trip/DSCF1.RAF", FileKind::Raw);
    scanned.fingerprint = fp("is now");
    builder.add(scanned);

    assert_eq!(builder.to_pool().photos()[0].id, PhotoId(9), "same photo");
    assert_eq!(builder.next_id(), 100, "no fresh id was burned");
    assert!(
        !builder.add_missing(fp("was"), None, Some(PathBuf::from("trip/DSCF1.RAF"))),
        "the file is present, so no placeholder for the old fingerprint"
    );
    assert_eq!(builder.to_pool().len(), 1, "one cell for one file");
}

#[test]
fn content_identity_wins_over_path_when_two_files_swap_names() {
    // Both names are known and both contents are known, just crossed over. The
    // fingerprint decides, so each photo follows its pixels rather than its name.
    let mut builder = PoolBuilder::seeded(
        HashMap::from([(fp("alpha"), PhotoId(1)), (fp("beta"), PhotoId(2))]),
        HashMap::from([
            (PathBuf::from("trip/a.JPG"), PhotoId(1)),
            (PathBuf::from("trip/b.JPG"), PhotoId(2)),
        ]),
        100,
    );
    let mut first = file("trip/a.JPG", FileKind::Jpeg);
    first.fingerprint = fp("beta");
    let mut second = file("trip/b.JPG", FileKind::Jpeg);
    second.fingerprint = fp("alpha");
    builder.add(first);
    builder.add(second);

    let pool = builder.to_pool();
    let id_of = |name: &str| {
        pool.photos()
            .iter()
            .find(|p| p.file_name() == name)
            .map(|p| p.id)
            .unwrap()
    };
    assert_eq!(id_of("a.JPG"), PhotoId(2), "beta's id followed its content");
    assert_eq!(
        id_of("b.JPG"),
        PhotoId(1),
        "alpha's id followed its content"
    );
}

#[test]
fn add_missing_creates_a_placeholder_with_the_seeded_id() {
    let mut builder = PoolBuilder::seeded(
        HashMap::from([(fp("gone"), PhotoId(77))]),
        HashMap::new(),
        100,
    );
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
    let mut builder =
        PoolBuilder::seeded(HashMap::from([(fp("c"), PhotoId(3))]), HashMap::new(), 100);
    builder.add(ScannedFile {
        path: PathBuf::from("trip/here.jpg"),
        kind: FileKind::Jpeg,
        orientation: Orientation::Normal,
        fingerprint: fp("c"),
        captured_at: None,
        captured_approx: false,
        captured_offset: None,
        meta: CaptureMeta::default(),
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
            captured_approx: false,
            captured_offset: None,
            meta: CaptureMeta::default(),
        },
        ScannedFile {
            path: PathBuf::from("a/x.JPG"),
            kind: FileKind::Jpeg,
            orientation: Orientation::Normal,
            fingerprint: fp("a/x.JPG"),
            captured_at: None,
            captured_approx: false,
            captured_offset: None,
            meta: CaptureMeta::default(),
        },
    ]);
    assert_eq!(pool.photos()[0].orientation, Orientation::Normal);
}
