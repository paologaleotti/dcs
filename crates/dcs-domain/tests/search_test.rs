use std::collections::HashSet;

use dcs_domain::photo::PhotoId;
use dcs_domain::search::{SearchParams, cosine, rank};

/// Params that isolate the absolute floor (relative floor off) for the older
/// tests that probe `min_similarity` directly.
fn absolute(min_similarity: f32, max_results: usize) -> SearchParams {
    SearchParams {
        min_similarity,
        relative_floor: 0.0,
        max_results,
    }
}

#[test]
fn cosine_identical_is_one() {
    let v = [1.0, 2.0, 3.0];
    assert!((cosine(&v, &v) - 1.0).abs() < 1e-6);
}

#[test]
fn cosine_orthogonal_is_zero() {
    assert!((cosine(&[1.0, 0.0], &[0.0, 1.0])).abs() < 1e-6);
}

#[test]
fn cosine_opposite_is_minus_one() {
    assert!((cosine(&[1.0, 0.0], &[-1.0, 0.0]) + 1.0).abs() < 1e-6);
}

#[test]
fn cosine_zero_vector_is_zero_not_nan() {
    let c = cosine(&[0.0, 0.0], &[1.0, 1.0]);
    assert_eq!(c, 0.0);
    assert!(!c.is_nan());
}

#[test]
fn cosine_length_mismatch_is_zero() {
    assert_eq!(cosine(&[1.0, 2.0], &[1.0]), 0.0);
}

#[test]
fn rank_empty_pool_is_empty() {
    let out = rank(&[1.0, 0.0], &[], &SearchParams::default());
    assert!(out.is_empty());
}

#[test]
fn rank_floor_excludes_below_and_includes_at_boundary() {
    let a = [1.0, 0.0]; // cosine 1.0 with query
    let b = [0.0, 1.0]; // cosine 0.0 with query
    let photos: [(PhotoId, &[f32]); 2] = [(PhotoId(1), &a), (PhotoId(2), &b)];
    // Floor exactly at 0.0 is inclusive → both match.
    let out = rank(&[1.0, 0.0], &photos, &absolute(0.0, 10));
    assert_eq!(out, HashSet::from([PhotoId(1), PhotoId(2)]));

    // Raise the floor above b's score → only a survives.
    let out = rank(&[1.0, 0.0], &photos, &absolute(0.5, 10));
    assert_eq!(out, HashSet::from([PhotoId(1)]));
}

#[test]
fn rank_cap_keeps_only_strongest() {
    // Three photos with decreasing similarity to the query.
    let a = [1.0, 0.0];
    let b = [0.9, 0.1];
    let c = [0.1, 0.9];
    let photos: [(PhotoId, &[f32]); 3] = [(PhotoId(1), &a), (PhotoId(2), &b), (PhotoId(3), &c)];
    let out = rank(&[1.0, 0.0], &photos, &absolute(-1.0, 2));
    assert_eq!(out.len(), 2);
    // The two closest to [1,0] are a and b; c is dropped by the cap.
    assert!(out.contains(&PhotoId(1)) && out.contains(&PhotoId(2)));
    assert!(!out.contains(&PhotoId(3)));
}

#[test]
fn rank_cap_tie_break_is_deterministic_by_id() {
    // Two identical vectors → equal score; the cap of 1 must keep the lower id.
    let v = [1.0, 0.0];
    let photos: [(PhotoId, &[f32]); 2] = [(PhotoId(7), &v), (PhotoId(3), &v)];
    let out = rank(&[1.0, 0.0], &photos, &absolute(-1.0, 1));
    assert_eq!(out, HashSet::from([PhotoId(3)]));
}

#[test]
fn rank_all_below_floor_is_empty() {
    let v = [0.0, 1.0];
    let photos: [(PhotoId, &[f32]); 1] = [(PhotoId(1), &v)];
    assert!(rank(&[1.0, 0.0], &photos, &absolute(0.5, 10)).is_empty());
}

#[test]
fn default_relative_floor_drops_the_long_tail() {
    // Two strong matches and two weak ones; the default must keep only the cluster
    // near the top — not the whole pool (the bug this guards against).
    let strong_a = [1.0, 0.0];
    let strong_b = [0.98, 0.02];
    let weak_a = [0.2, 0.98];
    let weak_b = [0.1, 0.99];
    let photos: [(PhotoId, &[f32]); 4] = [
        (PhotoId(1), &strong_a),
        (PhotoId(2), &strong_b),
        (PhotoId(3), &weak_a),
        (PhotoId(4), &weak_b),
    ];
    let out = rank(&[1.0, 0.0], &photos, &SearchParams::default());
    assert_eq!(out, HashSet::from([PhotoId(1), PhotoId(2)]));
}

#[test]
fn negative_best_does_not_invert_the_relative_floor() {
    // Every photo is anti-correlated with the query (all cosines < 0). Even with a
    // permissive absolute floor, the relative floor must not "rescue" the least-bad
    // one — a negative best is clamped to 0, so nothing matches.
    let a = [-1.0, 0.0]; // cosine -1.0
    let b = [-0.1, 0.995]; // cosine ~ -0.1
    let photos: [(PhotoId, &[f32]); 2] = [(PhotoId(1), &a), (PhotoId(2), &b)];
    let params = SearchParams {
        min_similarity: -1.0,
        relative_floor: 0.75,
        max_results: 10,
    };
    assert!(rank(&[1.0, 0.0], &photos, &params).is_empty());
}

#[test]
fn default_absolute_floor_returns_nothing_for_pure_noise() {
    // Everything is near-orthogonal to the query → no real match → empty, never
    // "all photos".
    let n1 = [0.02, 0.999];
    let n2 = [0.01, 0.9999];
    let photos: [(PhotoId, &[f32]); 2] = [(PhotoId(1), &n1), (PhotoId(2), &n2)];
    assert!(rank(&[1.0, 0.0], &photos, &SearchParams::default()).is_empty());
}
