use std::path::PathBuf;

use dcs_domain::fingerprint::ContentFingerprint;
use dcs_domain::pairing::{FileKind, ScannedFile, pair};
use dcs_domain::sort::by_time_asc;
use time::macros::datetime;

fn at(path: &str, when: Option<time::PrimitiveDateTime>) -> ScannedFile {
    let mut bytes = [0u8; 32];
    for (i, b) in path.bytes().enumerate() {
        bytes[i % 32] ^= b;
    }
    ScannedFile {
        path: PathBuf::from(path),
        kind: FileKind::Jpeg,
        orientation: Default::default(),
        fingerprint: ContentFingerprint::from_bytes(bytes),
        captured_at: when,
    }
}

#[test]
fn orders_by_capture_time_ascending() {
    let pool = pair([
        at("a/c.JPG", Some(datetime!(2025-05-11 14:00:00))),
        at("a/a.JPG", Some(datetime!(2025-05-11 09:00:00))),
        at("a/b.JPG", Some(datetime!(2025-05-11 12:00:00))),
    ]);
    let order = by_time_asc(pool.photos());
    let names: Vec<_> = order
        .iter()
        .map(|&i| pool.photos()[i].file_name())
        .collect();
    assert_eq!(names, vec!["a.JPG", "b.JPG", "c.JPG"]);
}

#[test]
fn undated_photos_sort_last_then_by_name() {
    let pool = pair([
        at("a/z_dated.JPG", Some(datetime!(2025-05-11 10:00:00))),
        at("a/b_undated.JPG", None),
        at("a/a_undated.JPG", None),
    ]);
    let order = by_time_asc(pool.photos());
    let names: Vec<_> = order
        .iter()
        .map(|&i| pool.photos()[i].file_name())
        .collect();
    assert_eq!(names, vec!["z_dated.JPG", "a_undated.JPG", "b_undated.JPG"]);
}

#[test]
fn empty_pool_yields_empty_order() {
    let pool = pair(std::iter::empty());
    assert!(by_time_asc(pool.photos()).is_empty());
}

#[test]
fn all_undated_falls_back_to_name_order() {
    let pool = pair([at("a/c.JPG", None), at("a/a.JPG", None), at("a/b.JPG", None)]);
    let order = by_time_asc(pool.photos());
    let names: Vec<_> = order
        .iter()
        .map(|&i| pool.photos()[i].file_name())
        .collect();
    assert_eq!(names, vec!["a.JPG", "b.JPG", "c.JPG"]);
}

#[test]
fn equal_times_break_on_name() {
    let t = datetime!(2025-05-11 10:00:00);
    let pool = pair([at("a/b.JPG", Some(t)), at("a/a.JPG", Some(t))]);
    let order = by_time_asc(pool.photos());
    assert_eq!(pool.photos()[order[0]].file_name(), "a.JPG");
}
