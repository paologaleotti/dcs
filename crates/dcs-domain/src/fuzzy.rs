//! Fuzzy subsequence matching — the scorer behind every searchable surface in
//! dcs: the shoot-timezone picker today, the command palette (`Cmd+P`)
//! and the tag palette next. A thin wrapper over the `fuzzy-matcher`
//! crate's Skim v2 algorithm (the same matcher skim/fzf-style tools use), so we
//! don't hand-roll scoring; we own only the small result type and the
//! empty-query contract the UI relies on.
//!
//! Pure: no I/O, no egui. Wherever a picker appears it ranks identically, and
//! the contract is unit-tested once here.

use fuzzy_matcher::FuzzyMatcher;
use fuzzy_matcher::skim::SkimMatcherV2;

/// A successful fuzzy match: a relevance `score` (higher is better) and the
/// char indices in the haystack the needle matched, ascending, for
/// highlighting.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FuzzyMatch {
    pub score: i64,
    pub positions: Vec<usize>,
}

/// Fuzzy-match `needle` against `haystack` (smart-case, see `matcher`). Returns `None`
/// when `needle` is not a subsequence of `haystack`. An empty needle matches
/// everything with score `0` and no positions, so an empty query lists every
/// item in its original order (the matcher itself is unspecified on an empty
/// pattern, so we pin that behaviour here).
pub fn fuzzy_match(haystack: &str, needle: &str) -> Option<FuzzyMatch> {
    if needle.is_empty() {
        return Some(FuzzyMatch {
            score: 0,
            positions: Vec::new(),
        });
    }
    matcher()
        .fuzzy_indices(haystack, needle)
        .map(|(score, positions)| FuzzyMatch { score, positions })
}

/// Smart-case matcher (the crate default): a lowercase query matches any case,
/// while any uppercase char opts into case-sensitive matching — the behaviour a
/// palette wants. Cheap to construct; no shared state to thread through.
fn matcher() -> SkimMatcherV2 {
    SkimMatcherV2::default()
}
