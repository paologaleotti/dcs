//! Semantic-search ranking. Photos and the typed query are each turned into an
//! embedding vector up in `dcs-io` (a local CLIP-style model); the math that
//! turns those vectors into a matching set is pure and lives here.
//!
//! Pure and derived: the matching set feeds [`crate::filter`]'s injected `search`
//! map and is never persisted. No I/O, no model — just vectors in, ids out.

use std::collections::HashSet;

use crate::photo::PhotoId;

/// How permissive a search is. Two floors guard against the two failure modes of
/// a single absolute threshold on CLIP-style cosines (which sit in a narrow,
/// query-dependent positive band):
///
/// - `min_similarity` — an absolute noise floor that drops the clearly-unrelated
///   long tail regardless of query.
/// - `relative_floor` — a fraction of the *best* match's score; the effective
///   cutoff is `max(min_similarity, best * relative_floor)`. This adapts to query
///   strength so a strong query keeps a tight cluster and a weak one still returns
///   its few best guesses rather than everything.
///
/// `max_results` caps the survivors as a final backstop.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SearchParams {
    /// Inclusive absolute floor on cosine similarity.
    pub min_similarity: f32,
    /// Keep scores within this fraction of the top score (`0.0`..=`1.0`). `0.0`
    /// disables the relative floor; `1.0` keeps only ties with the best match.
    pub relative_floor: f32,
    /// Keep at most this many top-ranked matches.
    pub max_results: usize,
}

impl Default for SearchParams {
    /// Sensible starting defaults for SigLIP-class cosines: drop near-zero noise,
    /// keep matches scoring at least 75% of the best one, cap at 200. Tune
    /// `min_similarity`/`relative_floor` on a real folder if recall feels off.
    fn default() -> Self {
        SearchParams {
            min_similarity: 0.05,
            relative_floor: 0.75,
            max_results: 200,
        }
    }
}

/// Cosine similarity of two equal-length vectors, in `[-1, 1]`. Returns `0.0` for
/// a length mismatch or a zero-magnitude vector — a degenerate input is "no
/// similarity", never a panic or a NaN.
pub fn cosine(a: &[f32], b: &[f32]) -> f32 {
    if a.len() != b.len() {
        return 0.0;
    }
    let mut dot = 0.0f32;
    let mut na = 0.0f32;
    let mut nb = 0.0f32;
    for (&x, &y) in a.iter().zip(b) {
        dot += x * y;
        na += x * x;
        nb += y * y;
    }
    let denom = na.sqrt() * nb.sqrt();
    if denom == 0.0 { 0.0 } else { dot / denom }
}

/// The photos matching `query`: every photo whose cosine clears the effective
/// cutoff (see [`SearchParams`]), ranked by cosine descending and capped at
/// `params.max_results`. Pure — no ordering is imposed on the returned set; the
/// cap is applied to the *ranked* candidates so only the strongest survive.
pub fn rank(
    query: &[f32],
    photos: &[(PhotoId, &[f32])],
    params: &SearchParams,
) -> HashSet<PhotoId> {
    let mut scored: Vec<(PhotoId, f32)> = photos
        .iter()
        .map(|(id, vec)| (*id, cosine(query, vec)))
        .collect();
    // Anchor the relative floor to the best score, then clear noise with both.
    // Clamp `best` to ≥0: a negative best (query anti-correlated with the whole
    // pool) would otherwise make `best * relative_floor` *larger* than `best` and
    // drop even the top match — meaningless. Non-negative best keeps the floor a
    // true fraction; the absolute `min_similarity` still applies.
    let best = scored
        .iter()
        .map(|(_, s)| *s)
        .fold(f32::MIN, f32::max)
        .max(0.0);
    let cutoff = params.min_similarity.max(best * params.relative_floor);
    scored.retain(|(_, score)| *score >= cutoff);
    // Descending by score; ties broken by id so the cap is deterministic.
    scored.sort_by(|a, b| {
        b.1.partial_cmp(&a.1)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(a.0.0.cmp(&b.0.0))
    });
    scored
        .into_iter()
        .take(params.max_results)
        .map(|(id, _)| id)
        .collect()
}
