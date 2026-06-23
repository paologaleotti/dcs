//! Derived filter resolution. The active filter is a set of chip groups —
//! AND across groups, OR (or AND) within a group — narrowing the pool to a
//! visible set. Pure and derived: never persisted, recomputed on demand.
//!
//! Verdict and tag membership live in owned stores up in `dcs-app`; search
//! matches come from the embedding index (AI semantic search). All are injected
//! via [`FilterCtx`], so resolution stays pure and the layer arrows point down.

use std::collections::{BTreeSet, HashMap, HashSet};

use crate::cull::AcceptState;
use crate::photo::{Photo, PhotoId};
use crate::tag::TagId;

/// One filter predicate, matching a photo by a single criterion.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FilterChip {
    /// Photos with this verdict (an absent verdict counts as `Unreviewed`).
    Verdict(AcceptState),
    /// Photos carrying this tag.
    Tag(TagId),
    /// Photos matching an AI semantic-search query. The matching set is computed
    /// by the embedding index up in `dcs-app` and injected at resolve time via
    /// [`FilterCtx::search`]; an unknown query (not yet ranked) matches nothing.
    Search(String),
}

/// How the chips within one group combine.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ChipOp {
    /// Union — the common case and the visible default (`temples OR shrines`).
    #[default]
    Or,
    /// Intersection (`temples AND shrines`).
    And,
}

/// A row of chips combined by `op`. Groups themselves always AND together.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct FilterGroup {
    pub op: ChipOp,
    pub chips: Vec<FilterChip>,
}

/// The active filter: AND across groups, `op` within each. An empty filter (no
/// chips anywhere) matches everything. Derived display state — never persisted.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Filter {
    pub groups: Vec<FilterGroup>,
}

impl Filter {
    /// Whether any group carries a chip — i.e. the view is actually narrowed.
    pub fn is_active(&self) -> bool {
        self.groups.iter().any(|g| !g.chips.is_empty())
    }
}

/// Borrowed membership inputs, injected so resolution stays pure. Verdicts and
/// tags are owned stores in `dcs-app`; `search` is the AI semantic-search index
/// (also in `dcs-app`), empty only until a query has been ranked.
pub struct FilterCtx<'a> {
    /// Per-photo verdict; an absent entry is `Unreviewed`.
    pub verdicts: &'a HashMap<PhotoId, AcceptState>,
    /// Per-photo tag membership — the same borrow `grouping::tag_groups` takes.
    pub photo_tags: &'a HashMap<PhotoId, BTreeSet<TagId>>,
    /// Search query → matching photos, from the embedding index.
    pub search: &'a HashMap<String, HashSet<PhotoId>>,
}

impl FilterCtx<'_> {
    fn verdict(&self, id: PhotoId) -> AcceptState {
        self.verdicts
            .get(&id)
            .copied()
            .unwrap_or(AcceptState::Unreviewed)
    }
}

/// The pool indices that pass `filter`, given the injected membership in `ctx`.
/// AND across groups, `group.op` within a group; an empty filter matches every
/// photo. Pure — never reorders, never touches I/O. RAW-only exclusion is the
/// caller's concern, not the filter's.
pub fn resolve(photos: &[Photo], filter: &Filter, ctx: &FilterCtx) -> HashSet<usize> {
    let all = || (0..photos.len()).collect::<HashSet<usize>>();
    // A chip-less group never constrains — drop empties so a half-built group
    // can't blank the grid mid-edit.
    let groups: Vec<&FilterGroup> = filter
        .groups
        .iter()
        .filter(|g| !g.chips.is_empty())
        .collect();
    if groups.is_empty() {
        return all();
    }
    let mut acc: Option<HashSet<usize>> = None;
    for group in groups {
        let set = evaluate_group(photos, group, ctx);
        acc = Some(match acc {
            None => set,
            Some(prev) => &prev & &set,
        });
        if acc.as_ref().is_some_and(HashSet::is_empty) {
            break; // AND only shrinks — nothing left to keep.
        }
    }
    acc.unwrap_or_else(all)
}

/// The indices matching one group: its chips combined by `op`.
fn evaluate_group(photos: &[Photo], group: &FilterGroup, ctx: &FilterCtx) -> HashSet<usize> {
    let mut chips = group.chips.iter();
    let Some(first) = chips.next() else {
        return HashSet::new();
    };
    chips.fold(chip_set(photos, first, ctx), |acc, chip| {
        let set = chip_set(photos, chip, ctx);
        match group.op {
            ChipOp::Or => &acc | &set,
            ChipOp::And => &acc & &set,
        }
    })
}

fn chip_set(photos: &[Photo], chip: &FilterChip, ctx: &FilterCtx) -> HashSet<usize> {
    (0..photos.len())
        .filter(|&i| matches_chip(photos[i].id, chip, ctx))
        .collect()
}

fn matches_chip(id: PhotoId, chip: &FilterChip, ctx: &FilterCtx) -> bool {
    match chip {
        FilterChip::Verdict(state) => ctx.verdict(id) == *state,
        FilterChip::Tag(tag) => ctx.photo_tags.get(&id).is_some_and(|t| t.contains(tag)),
        FilterChip::Search(query) => ctx.search.get(query).is_some_and(|s| s.contains(&id)),
    }
}
