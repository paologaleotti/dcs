use std::collections::{BTreeSet, HashMap, HashSet};
use std::path::PathBuf;

use dcs_domain::cull::AcceptState;
use dcs_domain::filter::{ChipOp, Filter, FilterChip, FilterCtx, FilterGroup, resolve};
use dcs_domain::fingerprint::ContentFingerprint;
use dcs_domain::photo::{AssociatedFiles, CaptureMeta, Orientation, Photo, PhotoId, PhotoType};
use dcs_domain::tag::TagId;

fn photo(id: u32) -> Photo {
    Photo {
        id: PhotoId(id),
        files: AssociatedFiles {
            jpeg: Some(PathBuf::from(format!("{id}.jpg"))),
            raw: None,
        },
        photo_type: PhotoType::Jpeg,
        orientation: Orientation::Normal,
        fingerprint: ContentFingerprint::from_bytes([id as u8; 32]),
        captured_at: None,
        captured_offset: None,
        meta: CaptureMeta::default(),
        missing: false,
    }
}

/// Five photos, ids 0..5 at the matching pool indices.
fn pool() -> Vec<Photo> {
    (0..5).map(photo).collect()
}

fn verdicts(pairs: &[(u32, AcceptState)]) -> HashMap<PhotoId, AcceptState> {
    pairs.iter().map(|&(id, s)| (PhotoId(id), s)).collect()
}

fn tag_index(pairs: &[(u32, &[u32])]) -> HashMap<PhotoId, BTreeSet<TagId>> {
    pairs
        .iter()
        .map(|&(id, tags)| (PhotoId(id), tags.iter().map(|&t| TagId(t)).collect()))
        .collect()
}

fn group(op: ChipOp, chips: Vec<FilterChip>) -> FilterGroup {
    FilterGroup { op, chips }
}

/// Resolve and return the matched indices, sorted, for stable assertions.
fn matched(
    photos: &[Photo],
    filter: &Filter,
    verdicts: &HashMap<PhotoId, AcceptState>,
    tags: &HashMap<PhotoId, BTreeSet<TagId>>,
    search: &HashMap<String, HashSet<PhotoId>>,
) -> Vec<usize> {
    let ctx = FilterCtx {
        verdicts,
        photo_tags: tags,
        search,
    };
    let mut out: Vec<usize> = resolve(photos, filter, &ctx).into_iter().collect();
    out.sort_unstable();
    out
}

fn empty_search() -> HashMap<String, HashSet<PhotoId>> {
    HashMap::new()
}

#[test]
fn empty_filter_matches_everything() {
    let photos = pool();
    let got = matched(
        &photos,
        &Filter::default(),
        &HashMap::new(),
        &HashMap::new(),
        &empty_search(),
    );
    assert_eq!(got, vec![0, 1, 2, 3, 4]);
    assert!(!Filter::default().is_active());
}

#[test]
fn verdict_unreviewed_matches_cull_absent_photos() {
    let photos = pool();
    // Only 0 and 3 carry a verdict; the rest are cull-absent → Unreviewed.
    let v = verdicts(&[(0, AcceptState::Accepted), (3, AcceptState::Rejected)]);
    let f = Filter {
        groups: vec![group(
            ChipOp::Or,
            vec![FilterChip::Verdict(AcceptState::Unreviewed)],
        )],
    };
    assert_eq!(
        matched(&photos, &f, &v, &HashMap::new(), &empty_search()),
        vec![1, 2, 4]
    );
    assert!(f.is_active());
}

#[test]
fn and_across_groups() {
    let photos = pool();
    let v = verdicts(&[
        (0, AcceptState::Accepted),
        (1, AcceptState::Accepted),
        (2, AcceptState::Accepted),
    ]);
    let tags = tag_index(&[(1, &[10]), (2, &[10]), (4, &[10])]);
    // (accepted) AND (tag 10) → only 1 and 2 are both.
    let f = Filter {
        groups: vec![
            group(ChipOp::Or, vec![FilterChip::Verdict(AcceptState::Accepted)]),
            group(ChipOp::Or, vec![FilterChip::Tag(TagId(10))]),
        ],
    };
    assert_eq!(matched(&photos, &f, &v, &tags, &empty_search()), vec![1, 2]);
}

#[test]
fn or_within_a_mixed_kind_group() {
    let photos = pool();
    let v = verdicts(&[(0, AcceptState::Accepted)]);
    let tags = tag_index(&[(3, &[10])]);
    // accepted OR tag10 → 0 (accepted) and 3 (tagged).
    let f = Filter {
        groups: vec![group(
            ChipOp::Or,
            vec![
                FilterChip::Verdict(AcceptState::Accepted),
                FilterChip::Tag(TagId(10)),
            ],
        )],
    };
    assert_eq!(matched(&photos, &f, &v, &tags, &empty_search()), vec![0, 3]);
}

#[test]
fn and_within_a_group_needs_both_tags() {
    let photos = pool();
    let tags = tag_index(&[(1, &[10, 20]), (2, &[10]), (3, &[20])]);
    let f = Filter {
        groups: vec![group(
            ChipOp::And,
            vec![FilterChip::Tag(TagId(10)), FilterChip::Tag(TagId(20))],
        )],
    };
    assert_eq!(
        matched(&photos, &f, &HashMap::new(), &tags, &empty_search()),
        vec![1]
    );
}

#[test]
fn empty_group_is_dropped_not_blanking() {
    let photos = pool();
    let tags = tag_index(&[(2, &[10])]);
    // A real group plus a chip-less group must return the real group's set,
    // never empty (a half-built group can't blank the grid).
    let f = Filter {
        groups: vec![
            group(ChipOp::Or, vec![FilterChip::Tag(TagId(10))]),
            group(ChipOp::Or, vec![]),
        ],
    };
    assert_eq!(
        matched(&photos, &f, &HashMap::new(), &tags, &empty_search()),
        vec![2]
    );
}

#[test]
fn only_empty_groups_match_everything() {
    let photos = pool();
    let f = Filter {
        groups: vec![group(ChipOp::Or, vec![]), group(ChipOp::And, vec![])],
    };
    assert_eq!(
        matched(
            &photos,
            &f,
            &HashMap::new(),
            &HashMap::new(),
            &empty_search()
        ),
        vec![0, 1, 2, 3, 4]
    );
}

#[test]
fn search_query_absent_matches_nothing() {
    let photos = pool();
    // No model: the search map is empty, so a Search chip contributes nothing,
    // and ANDed against everything it blanks the result.
    let f = Filter {
        groups: vec![group(
            ChipOp::Or,
            vec![FilterChip::Search("temple".to_string())],
        )],
    };
    assert!(
        matched(
            &photos,
            &f,
            &HashMap::new(),
            &HashMap::new(),
            &empty_search()
        )
        .is_empty()
    );
}

#[test]
fn search_query_present_matches_injected_ids() {
    let photos = pool();
    let mut search = HashMap::new();
    search.insert(
        "temple".to_string(),
        HashSet::from([PhotoId(1), PhotoId(4)]),
    );
    let f = Filter {
        groups: vec![group(
            ChipOp::Or,
            vec![FilterChip::Search("temple".to_string())],
        )],
    };
    assert_eq!(
        matched(&photos, &f, &HashMap::new(), &HashMap::new(), &search),
        vec![1, 4]
    );
}

#[test]
fn tag_with_no_members_is_empty() {
    let photos = pool();
    let f = Filter {
        groups: vec![group(ChipOp::Or, vec![FilterChip::Tag(TagId(99))])],
    };
    assert!(
        matched(
            &photos,
            &f,
            &HashMap::new(),
            &HashMap::new(),
            &empty_search()
        )
        .is_empty()
    );
}

#[test]
fn multitagged_photo_counted_once() {
    let photos = pool();
    let tags = tag_index(&[(2, &[10, 20, 30])]);
    // Photo 2 carries all three; an OR over them yields just index 2, not three.
    let f = Filter {
        groups: vec![group(
            ChipOp::Or,
            vec![
                FilterChip::Tag(TagId(10)),
                FilterChip::Tag(TagId(20)),
                FilterChip::Tag(TagId(30)),
            ],
        )],
    };
    assert_eq!(
        matched(&photos, &f, &HashMap::new(), &tags, &empty_search()),
        vec![2]
    );
}

#[test]
fn empty_pool_resolves_empty() {
    let photos: Vec<Photo> = Vec::new();
    let f = Filter {
        groups: vec![group(ChipOp::Or, vec![FilterChip::Tag(TagId(10))])],
    };
    assert!(
        matched(
            &photos,
            &f,
            &HashMap::new(),
            &HashMap::new(),
            &empty_search()
        )
        .is_empty()
    );
    // And an empty filter over an empty pool is still empty (not a panic).
    assert!(
        matched(
            &photos,
            &Filter::default(),
            &HashMap::new(),
            &HashMap::new(),
            &empty_search()
        )
        .is_empty()
    );
}
