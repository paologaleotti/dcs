use dcs_domain::cull::AcceptState;
use dcs_domain::filter::{ChipOp, Filter, FilterChip, FilterCtx, FilterGroup};
use dcs_domain::tag::TagId;

use super::{Session, VerdictFilter};

impl Session {
    /// The active chip filter (state + tag + search chips). Empty = unfiltered.
    pub fn active_filter(&self) -> &Filter {
        &self.filter
    }

    /// Borrowed membership inputs for [`dcs_domain::filter::resolve`]: the owned
    /// verdict and tag stores plus the (v1-empty) search sets. One builder so the
    /// grid (`layout`) and export (`store`) resolve against identical inputs.
    pub(super) fn filter_ctx(&self) -> FilterCtx<'_> {
        FilterCtx {
            verdicts: self.cull.verdicts(),
            photo_tags: self.tags.photo_tag_index(),
            search: &self.search_sets,
        }
    }

    /// Whether the view is narrowed by any chip — drives the filter bar, the
    /// "N of M" count, and the sheet accent.
    pub fn is_filtered(&self) -> bool {
        self.filter.is_active()
    }

    /// The verdict view as a single 4-way value — sugar over the verdict group.
    /// `All` when that group is absent or holds *more than one* verdict (a
    /// multi-select like acc+rej has no single-view name).
    pub fn filter(&self) -> VerdictFilter {
        self.filter
            .groups
            .iter()
            .find(|g| is_verdict_group(g))
            .and_then(|g| match g.chips.as_slice() {
                [FilterChip::Verdict(state)] => Some(verdict_view(*state)),
                _ => None,
            })
            .unwrap_or(VerdictFilter::All)
    }

    /// Set the verdict view to exactly one state (or clear it for `All`) — the
    /// palette's quick presets. Replaces any existing verdict group, leaving
    /// tag/search groups intact. Rewinds the background fill.
    pub fn set_filter(&mut self, view: VerdictFilter) {
        let mut groups: Vec<FilterGroup> = self
            .filter
            .groups
            .iter()
            .filter(|g| !is_verdict_group(g))
            .cloned()
            .collect();
        if let Some(state) = verdict_state(view) {
            groups.push(FilterGroup {
                op: ChipOp::Or,
                chips: vec![FilterChip::Verdict(state)],
            });
        }
        self.replace_filter(Filter { groups });
    }

    /// Whether `state` is one of the active verdict filters (drives the `+ state`
    /// dropdown's checkboxes).
    pub fn verdict_filter_active(&self, state: AcceptState) -> bool {
        self.filter
            .groups
            .iter()
            .flat_map(|g| &g.chips)
            .any(|c| matches!(c, FilterChip::Verdict(s) if *s == state))
    }

    /// Toggle one verdict in/out of the verdict group — the multi-select path, so
    /// `(accepted OR rejected)` is one flip away. Verdicts share a single OR
    /// group; emptying it drops the group.
    pub fn toggle_verdict_filter(&mut self, state: AcceptState) {
        let mut filter = self.filter.clone();
        if let Some(gi) = filter.groups.iter().position(is_verdict_group) {
            let group = &mut filter.groups[gi];
            match group
                .chips
                .iter()
                .position(|c| matches!(c, FilterChip::Verdict(s) if *s == state))
            {
                Some(ci) => {
                    group.chips.remove(ci);
                    if group.chips.is_empty() {
                        filter.groups.remove(gi);
                    }
                }
                None => group.chips.push(FilterChip::Verdict(state)),
            }
        } else {
            filter.groups.push(FilterGroup {
                op: ChipOp::Or,
                chips: vec![FilterChip::Verdict(state)],
            });
        }
        self.replace_filter(filter);
    }

    /// Whether `tag` is an active tag chip (drives the filter dropdown's tag
    /// checkboxes).
    pub fn tag_chip_active(&self, tag: TagId) -> bool {
        self.has_tag_chip(tag)
    }

    /// Toggle a tag in/out of the filter — the dropdown-checkbox path.
    pub fn toggle_tag_chip(&mut self, tag: TagId) {
        if self.has_tag_chip(tag) {
            self.remove_tag_chip(tag);
        } else {
            self.add_tag_chip(tag);
        }
    }

    /// Add a tag chip. Tag chips share one OR group by default (the spec's
    /// `temples OR shrines`), flippable to AND via [`Self::toggle_filter_group_op`].
    /// A no-op if the tag is already a chip.
    pub fn add_tag_chip(&mut self, tag: TagId) {
        if self.has_tag_chip(tag) {
            return;
        }
        let mut filter = self.filter.clone();
        match filter.groups.iter_mut().find(|g| is_all_tags(g)) {
            Some(group) => group.chips.push(FilterChip::Tag(tag)),
            None => filter.groups.push(FilterGroup {
                op: ChipOp::Or,
                chips: vec![FilterChip::Tag(tag)],
            }),
        }
        self.replace_filter(filter);
    }

    /// Chain a search chip: append it to the single shared search group (OR by
    /// default, flippable to AND in the filter bar), so multiple searches combine
    /// instead of each spawning its own AND group. No-op on a blank or duplicate
    /// query. The Shift+Enter path; matches fill in when the query embeds.
    pub fn add_search_chip(&mut self, query: String) {
        let query = query.trim().to_string();
        if query.is_empty() || self.has_search_chip(&query) {
            return;
        }
        let mut filter = self.filter.clone();
        match filter.groups.iter_mut().find(|g| is_all_search(g)) {
            Some(group) => group.chips.push(FilterChip::Search(query)),
            None => filter.groups.push(FilterGroup {
                op: ChipOp::Or,
                chips: vec![FilterChip::Search(query)],
            }),
        }
        self.replace_filter(filter);
    }

    /// Replace the search with a single query — the plain-Enter path: drop every
    /// existing search chip (and its resolved sets), then add this one. Leaves
    /// verdict/tag groups intact. No-op on a blank query.
    pub fn set_search_chip(&mut self, query: String) {
        let query = query.trim().to_string();
        if query.is_empty() {
            return;
        }
        let mut filter = self.filter.clone();
        for group in &mut filter.groups {
            group.chips.retain(|c| !matches!(c, FilterChip::Search(_)));
        }
        filter.groups.retain(|g| !g.chips.is_empty());
        // Only this query matters now; stale resolved sets/vecs go.
        self.search_sets.clear();
        self.search_vecs.clear();
        filter.groups.push(FilterGroup {
            op: ChipOp::Or,
            chips: vec![FilterChip::Search(query)],
        });
        self.replace_filter(filter);
    }

    /// Drop every active search chip and its resolved sets — used when AI search is
    /// turned off so a leftover `Search` chip (now backed by nothing) can't blank
    /// the grid. Leaves verdict/tag chips intact. No-op when no search is active.
    pub(super) fn clear_search_chips(&mut self) {
        if !self.has_any_search_chip() {
            return;
        }
        let mut filter = self.filter.clone();
        for group in &mut filter.groups {
            group.chips.retain(|c| !matches!(c, FilterChip::Search(_)));
        }
        filter.groups.retain(|g| !g.chips.is_empty());
        self.search_sets.clear();
        self.search_vecs.clear();
        self.replace_filter(filter);
    }

    fn has_any_search_chip(&self) -> bool {
        self.filter
            .groups
            .iter()
            .flat_map(|g| &g.chips)
            .any(|c| matches!(c, FilterChip::Search(_)))
    }

    /// Remove the chip at `chip` in group `group`; drop the group if it empties.
    /// Out-of-range indices are ignored.
    pub fn remove_filter_chip(&mut self, group: usize, chip: usize) {
        let mut filter = self.filter.clone();
        let Some(g) = filter.groups.get_mut(group) else {
            return;
        };
        if chip >= g.chips.len() {
            return;
        }
        let removed = g.chips.remove(chip);
        if g.chips.is_empty() {
            filter.groups.remove(group);
        }
        // Drop the resolved set/vec for a removed search so it doesn't linger.
        if let FilterChip::Search(query) = removed {
            self.search_sets.remove(&query);
            self.search_vecs.remove(&query);
        }
        self.replace_filter(filter);
    }

    /// Flip a group's within-group combinator (OR ↔ AND). Out-of-range ignored.
    pub fn toggle_filter_group_op(&mut self, group: usize) {
        let mut filter = self.filter.clone();
        let Some(g) = filter.groups.get_mut(group) else {
            return;
        };
        g.op = match g.op {
            ChipOp::Or => ChipOp::And,
            ChipOp::And => ChipOp::Or,
        };
        self.replace_filter(filter);
    }

    /// Drop every chip — back to the full grid. The chip-X / Clear path; Esc never
    /// reaches here (filters are out of the Esc chain).
    pub fn clear_filter(&mut self) {
        if !self.filter.is_active() {
            return;
        }
        self.replace_filter(Filter::default());
    }

    /// Drop a tag chip wherever it sits, pruning any group it empties.
    fn remove_tag_chip(&mut self, tag: TagId) {
        let mut filter = self.filter.clone();
        for group in &mut filter.groups {
            group
                .chips
                .retain(|c| !matches!(c, FilterChip::Tag(t) if *t == tag));
        }
        filter.groups.retain(|g| !g.chips.is_empty());
        self.replace_filter(filter);
    }

    fn has_tag_chip(&self, tag: TagId) -> bool {
        self.filter
            .groups
            .iter()
            .flat_map(|g| &g.chips)
            .any(|c| matches!(c, FilterChip::Tag(t) if *t == tag))
    }

    fn has_search_chip(&self, query: &str) -> bool {
        self.filter
            .groups
            .iter()
            .flat_map(|g| &g.chips)
            .any(|c| matches!(c, FilterChip::Search(q) if q == query))
    }

    /// Commit a new filter and refresh, rewinding the background fill so newly
    /// visible photos decode. No-op when unchanged.
    fn replace_filter(&mut self, next: Filter) {
        if next == self.filter {
            return;
        }
        self.filter = next;
        self.bg_cursor = 0;
        self.rebuild_visible();
    }
}

/// A non-empty group whose chips are all verdicts — the shared verdict group.
fn is_verdict_group(group: &FilterGroup) -> bool {
    !group.chips.is_empty()
        && group
            .chips
            .iter()
            .all(|c| matches!(c, FilterChip::Verdict(_)))
}

/// A non-empty group whose chips are all tags — the shared tag-chip group.
fn is_all_tags(group: &FilterGroup) -> bool {
    !group.chips.is_empty() && group.chips.iter().all(|c| matches!(c, FilterChip::Tag(_)))
}

/// A non-empty group whose chips are all searches — the shared search group.
fn is_all_search(group: &FilterGroup) -> bool {
    !group.chips.is_empty()
        && group
            .chips
            .iter()
            .all(|c| matches!(c, FilterChip::Search(_)))
}

fn verdict_state(view: VerdictFilter) -> Option<AcceptState> {
    match view {
        VerdictFilter::All => None,
        VerdictFilter::Unreviewed => Some(AcceptState::Unreviewed),
        VerdictFilter::Accepted => Some(AcceptState::Accepted),
        VerdictFilter::Rejected => Some(AcceptState::Rejected),
    }
}

fn verdict_view(state: AcceptState) -> VerdictFilter {
    match state {
        AcceptState::Unreviewed => VerdictFilter::Unreviewed,
        AcceptState::Accepted => VerdictFilter::Accepted,
        AcceptState::Rejected => VerdictFilter::Rejected,
    }
}
