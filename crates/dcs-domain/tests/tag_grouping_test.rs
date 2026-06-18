//! Pure tag-axis grouping: one band per non-empty tag ordered by earliest
//! member, multi-tagged photos projected into each band, `Untagged` last.

use std::collections::{BTreeSet, HashMap};
use std::path::PathBuf;

use dcs_domain::fingerprint::ContentFingerprint;
use dcs_domain::grouping::{GroupKind, tag_groups};
use dcs_domain::pairing::{FileKind, ScannedFile, pair};
use dcs_domain::photo::{CaptureMeta, PhotoId, Pool};
use dcs_domain::sort::Sort;
use dcs_domain::tag::TagId;
use time::PrimitiveDateTime;
use time::macros::datetime;

fn at(path: &str, when: PrimitiveDateTime) -> ScannedFile {
    let mut bytes = [0u8; 32];
    for (i, b) in path.bytes().enumerate() {
        bytes[i % 32] ^= b;
    }
    ScannedFile {
        path: PathBuf::from(path),
        kind: FileKind::Jpeg,
        orientation: Default::default(),
        fingerprint: ContentFingerprint::from_bytes(bytes),
        captured_at: Some(when),
        captured_offset: None,
        meta: CaptureMeta::default(),
    }
}

fn names(pool: &Pool, members: &[usize]) -> Vec<String> {
    members
        .iter()
        .map(|&i| pool.photos()[i].file_name())
        .collect()
}

/// The per-photo tag index keyed by `PhotoId`, built by a tag picker keyed on
/// the file name (so the test reads regardless of the pool's internal order).
fn photo_tags(pool: &Pool, pick: impl Fn(&str) -> Vec<TagId>) -> HashMap<PhotoId, BTreeSet<TagId>> {
    pool.photos()
        .iter()
        .map(|p| (p.id, pick(&p.file_name()).into_iter().collect()))
        .collect()
}

fn names_map<'a>(pairs: &[(u32, &'a str)]) -> HashMap<TagId, &'a str> {
    pairs.iter().map(|&(id, n)| (TagId(id), n)).collect()
}

#[test]
fn one_band_per_tag_untagged_last() {
    let pool = pair([
        at("a.JPG", datetime!(2025-05-11 09:00:00)),
        at("b.JPG", datetime!(2025-05-11 10:00:00)),
        at("c.JPG", datetime!(2025-05-11 11:00:00)),
    ]);
    let tags = photo_tags(&pool, |name| match name {
        "a.JPG" => vec![TagId(0)],
        "b.JPG" => vec![TagId(1)],
        _ => vec![], // c untagged
    });
    let groups = tag_groups(
        pool.photos(),
        &tags,
        &names_map(&[(0, "temples"), (1, "shrines")]),
        Sort::default(),
    );

    assert_eq!(groups.len(), 3);
    assert_eq!(groups[0].kind, GroupKind::Tag(TagId(0)));
    assert_eq!(groups[0].title, "temples");
    assert_eq!(names(&pool, &groups[0].members), ["a.JPG"]);
    assert_eq!(groups[2].kind, GroupKind::Leftover);
    assert_eq!(groups[2].title, "Untagged");
    assert_eq!(names(&pool, &groups[2].members), ["c.JPG"]);
}

#[test]
fn multi_tagged_photo_projects_into_every_band() {
    let pool = pair([
        at("a.JPG", datetime!(2025-05-11 09:00:00)),
        at("b.JPG", datetime!(2025-05-11 10:00:00)),
    ]);
    // a has both tags → appears in both bands.
    let tags = photo_tags(&pool, |name| match name {
        "a.JPG" => vec![TagId(0), TagId(1)],
        _ => vec![TagId(1)],
    });
    let groups = tag_groups(
        pool.photos(),
        &tags,
        &names_map(&[(0, "x"), (1, "y")]),
        Sort::default(),
    );

    let band_x = groups
        .iter()
        .find(|g| g.kind == GroupKind::Tag(TagId(0)))
        .unwrap();
    let band_y = groups
        .iter()
        .find(|g| g.kind == GroupKind::Tag(TagId(1)))
        .unwrap();
    assert_eq!(names(&pool, &band_x.members), ["a.JPG"]);
    assert_eq!(
        names(&pool, &band_y.members),
        ["a.JPG", "b.JPG"],
        "a projects into y too"
    );
    assert!(
        groups.iter().all(|g| g.kind != GroupKind::Leftover),
        "no untagged band"
    );
}

#[test]
fn bands_order_by_earliest_member() {
    let pool = pair([
        at("early.JPG", datetime!(2025-05-11 08:00:00)),
        at("late.JPG", datetime!(2025-05-11 20:00:00)),
    ]);
    // Tag 9 is on the earliest photo, tag 1 on the latest: band 9 must lead.
    let tags = photo_tags(&pool, |name| match name {
        "early.JPG" => vec![TagId(9)],
        _ => vec![TagId(1)],
    });
    let groups = tag_groups(
        pool.photos(),
        &tags,
        &names_map(&[(1, "one"), (9, "nine")]),
        Sort::default(),
    );
    assert_eq!(
        groups[0].kind,
        GroupKind::Tag(TagId(9)),
        "earliest member's band leads"
    );
    assert_eq!(groups[1].kind, GroupKind::Tag(TagId(1)));
}

#[test]
fn tag_without_a_name_is_skipped() {
    let pool = pair([at("a.JPG", datetime!(2025-05-11 09:00:00))]);
    let tags = photo_tags(&pool, |_| vec![TagId(7)]);
    let groups = tag_groups(pool.photos(), &tags, &HashMap::new(), Sort::default());
    assert!(
        groups.is_empty(),
        "no name → no band, and the photo isn't untagged"
    );
}

#[test]
fn empty_pool_has_no_bands() {
    let pool = Pool::default();
    let groups = tag_groups(
        pool.photos(),
        &HashMap::new(),
        &HashMap::new(),
        Sort::default(),
    );
    assert!(groups.is_empty());
}
