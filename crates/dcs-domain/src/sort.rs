//! Derived display ordering (§2.2, §2.3). Sort is a display setting computed
//! from metadata, never persisted. Returns indices into the pool so the pool
//! itself keeps its stable id order.

use crate::photo::Photo;

/// Order photos by capture time ascending — the default sort (§2.3). Undated
/// photos sort last (`No date`); ties break on file name so the order is
/// stable and deterministic. Returns indices into `photos`.
pub fn by_time_asc(photos: &[Photo]) -> Vec<usize> {
    // Build sort keys once (the file name is owned) rather than re-deriving
    // them inside every comparison.
    let mut keyed: Vec<(usize, &Photo, String)> = photos
        .iter()
        .enumerate()
        .map(|(i, p)| (i, p, p.file_name()))
        .collect();
    keyed.sort_by(|(_, a, a_name), (_, b, b_name)| {
        // `None` (undated) sorts after `Some` via Option's ordering, but we
        // want undated last, so compare the captured times with that flipped.
        match (a.captured_at, b.captured_at) {
            (Some(x), Some(y)) => x.cmp(&y).then_with(|| a_name.cmp(b_name)),
            (Some(_), None) => std::cmp::Ordering::Less,
            (None, Some(_)) => std::cmp::Ordering::Greater,
            (None, None) => a_name.cmp(b_name),
        }
    });
    keyed.into_iter().map(|(i, _, _)| i).collect()
}
