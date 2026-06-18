//! Group-scoped batch operations behind the header context menu and the
//! palette's "…all in this group" commands. A group is addressed by its index
//! into [`Session::groups`] (the post-filter visible spans); every op resolves
//! that index to the group's *visible* member ids, so it honors the visible-only
//! batch rule and tag-band projections collapse to one photo each.

use dcs_domain::command::Command;
use dcs_domain::cull::AcceptState;
use dcs_domain::grouping::GroupKind;
use dcs_domain::photo::PhotoId;

use super::Session;

impl Session {
    /// The visible members of group `idx`, in display order, deduped to unique
    /// photos (a tag band lists a projected photo once already, but a defensive
    /// dedup keeps the contract regardless of axis). Empty when `idx` is stale.
    pub fn group_member_ids(&self, idx: usize) -> Vec<PhotoId> {
        let Some(group) = self.visible_groups.get(idx) else {
            return Vec::new();
        };
        let photos = self.builder.photos();
        let mut seen = std::collections::HashSet::new();
        self.visible[group.start..group.start + group.count]
            .iter()
            .map(|&i| photos[i].id)
            .filter(|id| seen.insert(*id))
            .collect()
    }

    /// Index of the group the focus cursor sits in, skipping the headerless
    /// `none`-axis stream (which has no group menu — `Ctrl+A` selects it whole).
    /// Drives the palette's focus-aware group commands.
    pub fn focused_group(&self) -> Option<usize> {
        let f = self.focus()?;
        self.visible_groups
            .iter()
            .position(|g| g.kind != GroupKind::Stream && f >= g.start && f < g.start + g.count)
    }

    /// Index of the group that contains visible cell `display_idx`, skipping the
    /// headerless stream. Lets the cell context menu offer "select all in group".
    pub fn group_of_index(&self, display_idx: usize) -> Option<usize> {
        self.visible_groups.iter().position(|g| {
            g.kind != GroupKind::Stream && display_idx >= g.start && display_idx < g.start + g.count
        })
    }

    /// Display title of group `idx`, for menu and palette labels. `None` for a
    /// stale index or the title-less stream.
    pub fn group_title(&self, idx: usize) -> Option<&str> {
        self.visible_groups
            .get(idx)
            .filter(|g| g.kind != GroupKind::Stream)
            .map(|g| g.title.as_str())
    }

    /// Whether any visible member of group `idx` carries a tag — gates the
    /// "untag all" affordance so it never opens an empty palette.
    pub fn group_has_tags(&self, idx: usize) -> bool {
        self.group_member_ids(idx)
            .iter()
            .any(|&id| !self.tags_of(id).is_empty())
    }

    /// Replace the selection with every visible member of group `idx` and park
    /// the focus on the group's first cell. Ephemeral, like any selection change.
    pub fn select_group(&mut self, idx: usize) {
        let ids = self.group_member_ids(idx);
        if ids.is_empty() {
            return;
        }
        let start = self.visible_groups[idx].start;
        self.sel.select_ids(&ids, Some(start));
    }

    /// Set every visible member of group `idx` to `state` in one undoable step —
    /// a bulk assignment, not the per-cell toggle `A`/`X` use. No-op when the
    /// group is empty or stale.
    pub fn set_group_state(&mut self, idx: usize, state: AcceptState) {
        let ids = self.group_member_ids(idx);
        if ids.is_empty() {
            return;
        }
        self.dispatch(Command::SetState(ids, state));
        self.rebuild_visible();
    }
}
