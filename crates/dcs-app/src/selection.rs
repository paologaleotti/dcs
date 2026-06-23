//! Ephemeral grid selection + focus cursor.
//!
//! `focus` is a position in the current *visible display order*; `selected`
//! holds stable `PhotoId`s, so a selection survives re-sort and filtering and
//! naturally honors the visible-only batch rule: operations resolve
//! against the visible order, so off-screen ids never sneak in.
//!
//! `anchor` is the range origin. A plain arrow drops the anchor on the new
//! focus and selects only it; `Shift+arrow` keeps the anchor and grows the
//! range to the new focus.

use std::collections::HashSet;

use dcs_domain::photo::PhotoId;

#[derive(Debug, Default)]
pub struct Selection {
    focus: Option<usize>,
    anchor: Option<usize>,
    selected: HashSet<PhotoId>,
}

impl Selection {
    pub fn new() -> Self {
        Self::default()
    }

    /// Display index of the focus cursor, if any.
    pub fn focus(&self) -> Option<usize> {
        self.focus
    }

    /// Display index of the selection-range anchor, if any.
    pub fn anchor(&self) -> Option<usize> {
        self.anchor
    }

    /// Relocate the focus + anchor cursors by `PhotoId` after the visible order
    /// changed (re-sort, regroup, filter). Keeps the cursor on the *same photo*
    /// instead of on whatever now sits at the old numeric index. A photo no
    /// longer in the order falls back to clamping its old index into range.
    pub fn remap_focus(
        &mut self,
        focus_id: Option<PhotoId>,
        anchor_id: Option<PhotoId>,
        order: &[PhotoId],
    ) {
        if order.is_empty() {
            self.focus = None;
            self.anchor = None;
            return;
        }
        let resolve = |id: Option<PhotoId>, fallback: Option<usize>| {
            id.and_then(|id| order.iter().position(|&p| p == id))
                .or_else(|| fallback.map(|f| f.min(order.len() - 1)))
        };
        self.focus = resolve(focus_id, self.focus);
        self.anchor = resolve(anchor_id, self.anchor);
    }

    pub fn is_selected(&self, id: PhotoId) -> bool {
        self.selected.contains(&id)
    }

    pub fn count(&self) -> usize {
        self.selected.len()
    }

    /// Move the cursor by `dx` columns and `dy` rows (a row = ±`cols`), clamped
    /// to the visible range. `extend` (Shift) grows the range from the anchor;
    /// a plain move drops the anchor on the new cell and selects only it.
    pub fn move_focus(
        &mut self,
        dx: isize,
        dy: isize,
        cols: usize,
        order: &[PhotoId],
        extend: bool,
    ) {
        if order.is_empty() {
            self.focus = None;
            self.anchor = None;
            return;
        }
        let cols = cols.max(1) as isize;
        let last = order.len() as isize - 1;
        // The first arrow press with no cursor grabs index 0 rather than
        // skipping past it; subsequent presses move by the delta.
        let next = match self.focus {
            None => 0,
            Some(cur) => (cur as isize + dx + dy * cols).clamp(0, last) as usize,
        };
        self.set_focus_index(next, order, extend);
    }

    /// Place the focus on display index `next` (clamped to `order`) and update
    /// the anchor + selection: `extend` (Shift) grows the range from the anchor,
    /// a plain move drops the anchor on `next` and selects only it. The primitive
    /// behind both flat (`move_focus`) and layout-aware navigation.
    pub fn set_focus_index(&mut self, next: usize, order: &[PhotoId], extend: bool) {
        if order.is_empty() {
            self.focus = None;
            self.anchor = None;
            return;
        }
        let next = next.min(order.len() - 1);
        self.focus = Some(next);
        if extend {
            let anchor = self.anchor.unwrap_or(next);
            self.anchor = Some(anchor);
            self.set_range(anchor, next, order);
        } else {
            self.anchor = Some(next);
            self.selected.clear();
            self.selected.insert(order[next]);
        }
    }

    /// Replace the selection with a single cell and make it the focus + anchor.
    pub fn select_only(&mut self, idx: usize, order: &[PhotoId]) {
        if idx >= order.len() {
            return;
        }
        self.focus = Some(idx);
        self.anchor = Some(idx);
        self.selected.clear();
        self.selected.insert(order[idx]);
    }

    /// Shift+click: move focus to `idx` and select the anchor→`idx` range,
    /// keeping the existing anchor. With no prior anchor it behaves like a
    /// plain click on `idx`.
    pub fn extend_to(&mut self, idx: usize, order: &[PhotoId]) {
        if idx >= order.len() {
            return;
        }
        let anchor = self.anchor.unwrap_or(idx);
        self.anchor = Some(anchor);
        self.focus = Some(idx);
        self.set_range(anchor, idx, order);
    }

    /// Ctrl/Cmd+click: toggle one cell in or out of the selection without
    /// disturbing the rest, and make it the focus + anchor for a following
    /// Shift+click or Shift+arrow.
    pub fn toggle_at(&mut self, idx: usize, order: &[PhotoId]) {
        let Some(&id) = order.get(idx) else {
            return;
        };
        if !self.selected.remove(&id) {
            self.selected.insert(id);
        }
        self.focus = Some(idx);
        self.anchor = Some(idx);
    }

    /// `Ctrl+A`: select every visible photo. Focus parks on the first
    /// cell if it had none.
    pub fn select_all_visible(&mut self, order: &[PhotoId]) {
        self.selected = order.iter().copied().collect();
        if self.focus.is_none() && !order.is_empty() {
            self.focus = Some(0);
            self.anchor = Some(0);
        }
    }

    /// Replace the selection with an explicit id set (a group's members), moving
    /// focus + anchor to `focus` when given. Ids the visible order doesn't hold
    /// are still stored but simply never paint, matching the existing rule that
    /// selection survives filtering.
    pub fn select_ids(&mut self, ids: &[PhotoId], focus: Option<usize>) {
        self.selected = ids.iter().copied().collect();
        if let Some(f) = focus {
            self.focus = Some(f);
            self.anchor = Some(f);
        }
    }

    /// `Esc`: clear the selection (the only Esc-chain member this phase).
    /// Focus stays put; the anchor collapses onto it.
    pub fn clear(&mut self) {
        self.selected.clear();
        self.anchor = self.focus;
    }

    /// Photos a command should target: the selection if non-empty, else the
    /// focused photo. Returned in display order, deduped (a set), and filtered
    /// to the visible order so the visible-only rule holds.
    pub fn selected_or_focused(&self, order: &[PhotoId]) -> Vec<PhotoId> {
        if !self.selected.is_empty() {
            // Dedup while preserving display order: under the tag axis a
            // multi-tagged photo's id appears in `order` once per band it
            // projects into, and a selected photo must be counted (and targeted)
            // exactly once — spec §2.8 "tag projections count once".
            let mut seen = HashSet::new();
            return order
                .iter()
                .copied()
                .filter(|id| self.selected.contains(id) && seen.insert(*id))
                .collect();
        }
        match self.focus {
            Some(i) => order.get(i).copied().into_iter().collect(),
            None => Vec::new(),
        }
    }

    /// Keep focus valid after the visible order shrinks (e.g. a filter change).
    pub fn clamp_focus(&mut self, len: usize) {
        if len == 0 {
            self.focus = None;
            self.anchor = None;
        } else if let Some(f) = self.focus {
            self.focus = Some(f.min(len - 1));
        }
    }

    fn set_range(&mut self, a: usize, b: usize, order: &[PhotoId]) {
        if order.is_empty() {
            return;
        }
        let last = order.len() - 1;
        let (lo, hi) = if a <= b { (a, b) } else { (b, a) };
        self.selected.clear();
        for &id in &order[lo.min(last)..=hi.min(last)] {
            self.selected.insert(id);
        }
    }
}
