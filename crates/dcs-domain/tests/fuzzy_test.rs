//! Tests for the fuzzy matcher wrapper (CLAUDE.md: dcs-domain is unit-tested
//! with zero mocks). The scoring/positions come from the `fuzzy-matcher` crate,
//! so these assert our *contract* over it — empty-query semantics, the
//! subsequence/non-match boundary, char-indexed positions, and the qualitative
//! ranking a picker depends on — not the crate's exact internal scores.

use dcs_domain::fuzzy::fuzzy_match;

#[test]
fn empty_needle_matches_everything_with_zero_score() {
    let m = fuzzy_match("Europe/Rome", "").expect("empty needle always matches");
    assert_eq!(m.score, 0);
    assert!(m.positions.is_empty());
}

#[test]
fn empty_haystack_only_matches_empty_needle() {
    assert!(fuzzy_match("", "").is_some());
    assert!(fuzzy_match("", "a").is_none());
}

#[test]
fn non_subsequence_does_not_match() {
    assert!(fuzzy_match("Europe/Rome", "xyz").is_none());
    // 'z' never appears, so the whole needle fails even though "rome" matches.
    assert!(fuzzy_match("Europe/Rome", "romez").is_none());
}

#[test]
fn smart_case_lowercase_query_is_case_insensitive() {
    // A lowercase query ignores case, so it finds any-cased text.
    assert!(fuzzy_match("EUROPE/ROME", "rome").is_some());
    assert!(fuzzy_match("Europe/Rome", "rome").is_some());
}

#[test]
fn smart_case_uppercase_query_is_case_sensitive() {
    // Any uppercase in the query opts into case-sensitive matching.
    assert!(fuzzy_match("Europe/Rome", "Rome").is_some());
    assert!(fuzzy_match("Europe/Rome", "ROME").is_none());
}

#[test]
fn positions_are_in_range_ascending_and_match_needle_chars() {
    let m = fuzzy_match("Europe/Rome", "rome").expect("matches");
    let chars: Vec<char> = "Europe/Rome".chars().collect();
    assert_eq!(m.positions.len(), 4);
    assert!(m.positions.windows(2).all(|w| w[0] < w[1]), "ascending");
    let matched: String = m.positions.iter().map(|&p| chars[p]).collect();
    assert_eq!(matched.to_lowercase(), "rome");
}

#[test]
fn positions_are_char_indices_not_bytes() {
    // 'é' is one char but two UTF-8 bytes; positions must be char-based.
    let m = fuzzy_match("café/bar", "bar").expect("matches");
    let chars: Vec<char> = "café/bar".chars().collect();
    let matched: String = m.positions.iter().map(|&p| chars[p]).collect();
    assert_eq!(matched, "bar");
}

#[test]
fn consecutive_run_outscores_scattered_match() {
    let run = fuzzy_match("abcxxxx", "abc").expect("matches");
    let scattered = fuzzy_match("axbxcxx", "abc").expect("matches");
    assert!(
        run.score > scattered.score,
        "consecutive {} should beat scattered {}",
        run.score,
        scattered.score
    );
}

#[test]
fn boundary_match_outscores_mid_word_match() {
    // "ny" at a word boundary should beat "ny" buried mid-word.
    let boundary = fuzzy_match("America/New_York", "ny").expect("matches");
    let buried = fuzzy_match("Funny", "ny").expect("matches");
    assert!(
        boundary.score > buried.score,
        "boundary {} should beat buried {}",
        boundary.score,
        buried.score
    );
}

#[test]
fn exact_label_matches() {
    let m = fuzzy_match("tokyo", "tokyo").expect("matches");
    assert_eq!(m.positions, vec![0, 1, 2, 3, 4]);
}

#[test]
fn single_char_matches() {
    let m = fuzzy_match("Europe/Rome", "e").expect("matches");
    assert_eq!(m.positions.len(), 1);
}
