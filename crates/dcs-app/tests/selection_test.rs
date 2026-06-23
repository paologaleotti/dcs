//! Focus cursor + selection math (§2.12, §2.13, #31). `Selection` works over an
//! abstract visible display order (`&[PhotoId]`), so it tests with a plain Vec.

use dcs_app::selection::Selection;
use dcs_domain::photo::PhotoId;

/// A 12-photo display order: ids 0..12.
fn order() -> Vec<PhotoId> {
    (0..12).map(PhotoId).collect()
}

fn ids(sel: &Selection, order: &[PhotoId]) -> Vec<u32> {
    sel.selected_or_focused(order)
        .into_iter()
        .map(|p| p.0)
        .collect()
}

#[test]
fn plain_arrow_moves_focus_and_selects_only_it() {
    let o = order();
    let mut sel = Selection::new();
    sel.move_focus(1, 0, 4, &o, false); // first press with no cursor grabs index 0
    assert_eq!(sel.focus(), Some(0));
    assert_eq!(
        ids(&sel, &o),
        vec![0],
        "plain move selects only the focused cell"
    );

    sel.move_focus(0, 1, 4, &o, false); // down one row (cols = 4) → 0 + 4 = 4
    assert_eq!(sel.focus(), Some(4));
    assert_eq!(ids(&sel, &o), vec![4]);
}

#[test]
fn focus_clamps_at_the_edges() {
    let o = order();
    let mut sel = Selection::new();
    sel.select_only(0, &o);
    sel.move_focus(-1, 0, 4, &o, false); // can't go below 0
    assert_eq!(sel.focus(), Some(0));

    sel.select_only(11, &o);
    sel.move_focus(0, 1, 4, &o, false); // can't go past the last index
    assert_eq!(sel.focus(), Some(11));
}

#[test]
fn shift_arrow_extends_the_range_in_display_order() {
    let o = order();
    let mut sel = Selection::new();
    sel.select_only(2, &o); // anchor at 2
    sel.move_focus(1, 0, 4, &o, true); // extend to 3
    sel.move_focus(0, 1, 4, &o, true); // extend down a row to 7
    assert_eq!(sel.focus(), Some(7));
    assert_eq!(
        ids(&sel, &o),
        vec![2, 3, 4, 5, 6, 7],
        "anchor..=focus in display order"
    );
}

#[test]
fn shift_extend_backwards_covers_the_same_span() {
    let o = order();
    let mut sel = Selection::new();
    sel.select_only(7, &o); // anchor at 7
    sel.move_focus(-1, 0, 4, &o, true); // extend back to 6
    sel.move_focus(0, -1, 4, &o, true); // up a row to 2
    assert_eq!(
        ids(&sel, &o),
        vec![2, 3, 4, 5, 6, 7],
        "range is order-independent of direction"
    );
}

#[test]
fn plain_move_after_extend_resets_the_anchor() {
    let o = order();
    let mut sel = Selection::new();
    sel.select_only(2, &o);
    sel.move_focus(2, 0, 4, &o, true); // range 2..=4
    assert_eq!(ids(&sel, &o), vec![2, 3, 4]);
    sel.move_focus(1, 0, 4, &o, false); // plain move → drop anchor on 5, select only it
    assert_eq!(ids(&sel, &o), vec![5]);
    sel.move_focus(1, 0, 4, &o, true); // new range from the new anchor (5) → 5..=6
    assert_eq!(ids(&sel, &o), vec![5, 6]);
}

#[test]
fn select_all_visible_takes_every_cell() {
    let o = order();
    let mut sel = Selection::new();
    sel.select_all_visible(&o);
    assert_eq!(sel.count(), 12);
    assert_eq!(
        sel.focus(),
        Some(0),
        "focus parks on the first cell if it had none"
    );
}

#[test]
fn selected_or_focused_falls_back_to_focus_when_empty() {
    let o = order();
    let mut sel = Selection::new();
    assert!(ids(&sel, &o).is_empty(), "no focus, no selection → empty");

    sel.select_only(3, &o);
    sel.clear(); // Esc clears selection but keeps focus
    assert_eq!(sel.count(), 0);
    assert_eq!(ids(&sel, &o), vec![3], "falls back to the focused photo");
}

#[test]
fn selection_is_visible_only_and_deduped() {
    let o = order();
    let mut sel = Selection::new();
    sel.select_all_visible(&o);

    // A shrunk visible order (filter applied): only ids 0,2,4 remain visible.
    let filtered: Vec<PhotoId> = [0u32, 2, 4].into_iter().map(PhotoId).collect();
    let targets: Vec<u32> = sel
        .selected_or_focused(&filtered)
        .into_iter()
        .map(|p| p.0)
        .collect();
    assert_eq!(
        targets,
        vec![0, 2, 4],
        "off-screen ids never sneak into a batch op (#14)"
    );
}

#[test]
fn remap_focus_follows_the_photo_across_a_reorder() {
    // After a re-sort the same photo sits at a different index; the cursor must
    // stay on that photo, not jump to whatever now occupies the old index.
    let o = order();
    let mut sel = Selection::new();
    sel.select_only(4, &o); // focus + anchor on photo id 4
    assert_eq!(sel.focus(), Some(4));

    // The order reverses (e.g. time asc → desc): id 4 is now near the end.
    let reversed: Vec<PhotoId> = o.iter().rev().copied().collect();
    let focus_id = Some(PhotoId(4));
    sel.remap_focus(focus_id, focus_id, &reversed);

    assert_eq!(
        reversed[sel.focus().unwrap()],
        PhotoId(4),
        "cursor stays on id 4"
    );
    assert_eq!(sel.anchor(), sel.focus(), "anchor follows too");
}

#[test]
fn remap_focus_clamps_when_the_photo_is_filtered_out() {
    let o = order();
    let mut sel = Selection::new();
    sel.select_only(7, &o);

    // A filter shrinks the order and drops id 7; focus falls back to a clamped
    // index rather than dangling out of range.
    let shrunk: Vec<PhotoId> = [0u32, 1, 2].into_iter().map(PhotoId).collect();
    let gone = Some(PhotoId(7));
    sel.remap_focus(gone, gone, &shrunk);

    let f = sel.focus().expect("focus stays set");
    assert!(f < shrunk.len(), "clamped into the new range");
}

#[test]
fn tag_axis_projection_duplicates_are_counted_once() {
    // Under the tag axis a multi-tagged photo's id appears in the display order
    // once per band it projects into. A selected photo must be targeted (and
    // counted) exactly once — spec §2.8 "tag projections count once".
    let projected: Vec<PhotoId> = [0u32, 1, 3, 2, 3].into_iter().map(PhotoId).collect();
    let mut sel = Selection::new();
    sel.select_all_visible(&projected);

    let targets: Vec<u32> = sel
        .selected_or_focused(&projected)
        .into_iter()
        .map(|p| p.0)
        .collect();
    assert_eq!(
        targets,
        vec![0, 1, 3, 2],
        "the photo projected into two bands appears once, in display order"
    );
}

#[test]
fn click_then_shift_click_selects_the_range() {
    let o = order();
    let mut sel = Selection::new();
    sel.select_only(2, &o); // click cell 2 → anchor + focus at 2
    assert_eq!(ids(&sel, &o), vec![2]);
    sel.extend_to(6, &o); // shift+click cell 6
    assert_eq!(sel.focus(), Some(6));
    assert_eq!(
        ids(&sel, &o),
        vec![2, 3, 4, 5, 6],
        "shift+click selects anchor..=clicked"
    );
}

#[test]
fn shift_click_with_no_anchor_acts_like_a_plain_click() {
    let o = order();
    let mut sel = Selection::new();
    sel.extend_to(4, &o);
    assert_eq!(ids(&sel, &o), vec![4]);
    assert_eq!(sel.focus(), Some(4));
}

#[test]
fn ctrl_click_toggles_one_cell_without_disturbing_others() {
    let o = order();
    let mut sel = Selection::new();
    sel.select_only(1, &o);
    sel.toggle_at(5, &o); // add 5
    sel.toggle_at(9, &o); // add 9
    assert_eq!(ids(&sel, &o), vec![1, 5, 9]);
    sel.toggle_at(5, &o); // remove 5
    assert_eq!(ids(&sel, &o), vec![1, 9]);
    assert_eq!(
        sel.focus(),
        Some(5),
        "toggled cell becomes the focus + anchor"
    );
}

#[test]
fn shift_click_ranges_from_the_last_anchor_replacing_the_selection() {
    // Consistent with Shift+arrow: a Shift+click is a fresh contiguous range
    // from the current anchor, not an additive merge (use Ctrl+click for that).
    let o = order();
    let mut sel = Selection::new();
    sel.select_only(0, &o);
    sel.toggle_at(8, &o); // selection {0, 8}, anchor moves to 8
    sel.extend_to(10, &o); // shift+click from anchor 8 → range 8..=10 replaces
    assert_eq!(ids(&sel, &o), vec![8, 9, 10]);
}

#[test]
fn clamp_focus_keeps_the_cursor_in_range() {
    let o = order();
    let mut sel = Selection::new();
    sel.select_only(11, &o);
    sel.clamp_focus(5); // visible order shrank to 5 cells
    assert_eq!(sel.focus(), Some(4));
    sel.clamp_focus(0);
    assert_eq!(sel.focus(), None, "an empty order drops the cursor");
}
