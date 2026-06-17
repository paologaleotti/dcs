//! Derived display ordering. Sort is a display setting computed
//! from metadata, never persisted. Returns indices into the pool so the pool
//! itself keeps its stable id order.

use std::cmp::Ordering;

use crate::photo::Photo;

/// What to sort by. Always paired with a direction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SortKey {
    Time,
    Name,
}

/// The sort direction. Pairs with a [`SortKey`]; together they are the active
/// [`Sort`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SortDir {
    Asc,
    Desc,
}

/// The active sort: an explicit key + direction, always visible in the UI
/// Default is `time ↑`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Sort {
    pub key: SortKey,
    pub dir: SortDir,
}

impl Default for Sort {
    fn default() -> Self {
        Sort {
            key: SortKey::Time,
            dir: SortDir::Asc,
        }
    }
}

impl Sort {
    /// The default sort, also the seed for the grouped display order.
    pub const TIME_ASC: Sort = Sort {
        key: SortKey::Time,
        dir: SortDir::Asc,
    };
}

/// Order pool indices by the given sort. Undated photos always sort last under
/// `Time` (the `No date` tail) regardless of direction; name breaks ties
/// so the order is stable and deterministic.
pub fn order(photos: &[Photo], sort: Sort) -> Vec<usize> {
    let mut keyed: Vec<(usize, &Photo, String)> = photos
        .iter()
        .enumerate()
        .map(|(i, p)| (i, p, p.file_name()))
        .collect();
    keyed.sort_by(|(_, a, a_name), (_, b, b_name)| compare(a, a_name, b, b_name, sort));
    keyed.into_iter().map(|(i, _, _)| i).collect()
}

/// Order pool indices by capture time ascending — the default sort.
pub fn by_time_asc(photos: &[Photo]) -> Vec<usize> {
    order(photos, Sort::TIME_ASC)
}

/// Compare two photos under a sort. Pre-derived names are passed in so callers
/// that sort repeatedly don't re-derive them per comparison. Undated photos
/// always come last under `Time`, both directions.
pub fn compare(a: &Photo, a_name: &str, b: &Photo, b_name: &str, sort: Sort) -> Ordering {
    match sort.key {
        SortKey::Time => match (a.captured_at, b.captured_at) {
            (Some(x), Some(y)) => oriented(x.cmp(&y), sort.dir).then_with(|| a_name.cmp(b_name)),
            // Dated before undated, both directions — the `No date` tail stays put.
            (Some(_), None) => Ordering::Less,
            (None, Some(_)) => Ordering::Greater,
            (None, None) => a_name.cmp(b_name),
        },
        SortKey::Name => oriented(a_name.cmp(b_name), sort.dir),
    }
}

fn oriented(ord: Ordering, dir: SortDir) -> Ordering {
    match dir {
        SortDir::Asc => ord,
        SortDir::Desc => ord.reverse(),
    }
}
