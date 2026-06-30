//! Board state invariants and the view (de)serialization contract: unique
//! membership, position/stack bookkeeping that undo relies on, and the JSON
//! shape that the app's view store parses.

use std::collections::HashSet;

use dcs_domain::photo::PhotoId;
use dcs_domain::view::{BoardItem, BoardState, GridSettings, Pos, View, ViewId, ViewKind};

fn pid(n: u32) -> PhotoId {
    PhotoId(n)
}

#[test]
fn place_appends_on_top_and_is_unique() {
    let mut b = BoardState::default();
    assert!(b.place(BoardItem::placed(pid(1), Pos::new(0.0, 0.0))));
    assert!(b.place(BoardItem::placed(pid(2), Pos::new(10.0, 10.0))));
    // Last placed is on top (highest index).
    assert_eq!(b.index_of(pid(2)), Some(1));
    // A photo is on a board at most once: re-placing is a no-op.
    assert!(!b.place(BoardItem::placed(pid(1), Pos::new(99.0, 99.0))));
    assert_eq!(b.items.len(), 2);
    assert_eq!(b.item(pid(1)).unwrap().pos, Pos::new(0.0, 0.0));
}

#[test]
fn remove_returns_index_and_item_for_inversion() {
    let mut b = BoardState::default();
    b.place(BoardItem::placed(pid(1), Pos::new(0.0, 0.0)));
    b.place(BoardItem::placed(pid(2), Pos::new(5.0, 5.0)));
    b.place(BoardItem::placed(pid(3), Pos::new(9.0, 9.0)));

    let (at, item) = b.remove(pid(2)).expect("placed");
    assert_eq!(at, 1);
    assert_eq!(item.photo, pid(2));
    assert!(!b.contains(pid(2)));
    assert_eq!(b.remove(pid(2)), None);

    // Re-insert at the recorded index restores the original stacking order.
    assert!(b.insert_at(at, item));
    assert_eq!(
        b.items.iter().map(|i| i.photo).collect::<Vec<_>>(),
        vec![pid(1), pid(2), pid(3)]
    );
}

#[test]
fn insert_at_clamps_and_stays_unique() {
    let mut b = BoardState::default();
    b.place(BoardItem::placed(pid(1), Pos::new(0.0, 0.0)));
    // Out-of-range index clamps to the end rather than panicking.
    assert!(b.insert_at(99, BoardItem::placed(pid(2), Pos::new(1.0, 1.0))));
    assert_eq!(b.index_of(pid(2)), Some(1));
    // Already present → no-op.
    assert!(!b.insert_at(0, BoardItem::placed(pid(2), Pos::new(2.0, 2.0))));
}

#[test]
fn move_to_reports_previous_and_ignores_noops() {
    let mut b = BoardState::default();
    b.place(BoardItem::placed(pid(1), Pos::new(3.0, 4.0)));
    assert_eq!(
        b.move_to(pid(1), Pos::new(7.0, 8.0)),
        Some(Pos::new(3.0, 4.0))
    );
    assert_eq!(b.item(pid(1)).unwrap().pos, Pos::new(7.0, 8.0));
    // Moving to the same spot is not a change (records no undo entry upstream).
    assert_eq!(b.move_to(pid(1), Pos::new(7.0, 8.0)), None);
    // Moving an absent photo is not a change.
    assert_eq!(b.move_to(pid(2), Pos::new(0.0, 0.0)), None);
}

#[test]
fn non_finite_placements_are_rejected() {
    let mut b = BoardState::default();
    // Placing or moving to a non-finite position is a no-op — these would fail
    // JSON serialization and corrupt the persisted view.
    assert!(!b.place(BoardItem::placed(pid(1), Pos::new(f32::NAN, 0.0))));
    assert!(!b.place(BoardItem::placed(pid(2), Pos::new(0.0, f32::INFINITY))));
    assert!(b.items.is_empty());

    assert!(b.place(BoardItem::placed(pid(3), Pos::new(1.0, 1.0))));
    assert_eq!(b.move_to(pid(3), Pos::new(f32::NAN, 2.0)), None);
    assert_eq!(b.item(pid(3)).unwrap().pos, Pos::new(1.0, 1.0), "unchanged");
}

#[test]
fn raise_moves_a_photo_to_the_top_of_the_stack() {
    let mut b = BoardState::default();
    b.place(BoardItem::placed(pid(1), Pos::new(0.0, 0.0)));
    b.place(BoardItem::placed(pid(2), Pos::new(0.0, 0.0)));
    b.place(BoardItem::placed(pid(3), Pos::new(0.0, 0.0)));

    // Raise the bottom one to the top; the others keep their relative order.
    assert!(b.raise(pid(1)));
    assert_eq!(
        b.items.iter().map(|i| i.photo).collect::<Vec<_>>(),
        vec![pid(2), pid(3), pid(1)]
    );
    // Already on top → no change; absent → no change.
    assert!(!b.raise(pid(1)));
    assert!(!b.raise(pid(9)));
}

#[test]
fn forget_drops_only_listed_photos() {
    let mut b = BoardState::default();
    b.place(BoardItem::placed(pid(1), Pos::new(0.0, 0.0)));
    b.place(BoardItem::placed(pid(2), Pos::new(0.0, 0.0)));
    b.place(BoardItem::placed(pid(3), Pos::new(0.0, 0.0)));
    let ids: HashSet<PhotoId> = [pid(2)].into_iter().collect();
    b.forget(&ids);
    assert_eq!(
        b.items.iter().map(|i| i.photo).collect::<Vec<_>>(),
        vec![pid(1), pid(3)]
    );
}

#[test]
fn board_view_round_trips_through_json() {
    let mut state = BoardState::default();
    state.place(BoardItem::placed(pid(7), Pos::new(12.5, -3.0)));
    let view = View {
        id: ViewId(2),
        name: "Board".into(),
        kind: ViewKind::Board(state),
    };
    let json = serde_json::to_value(&view).unwrap();
    // Internally tagged + flattened: id/name/kind live at the top level.
    assert_eq!(json["kind"], "Board");
    assert_eq!(json["id"], 2);
    let back: View = serde_json::from_value(json).unwrap();
    assert_eq!(back, view);
}

#[test]
fn legacy_grid_entry_parses_with_defaulted_id_and_name() {
    // The shape written before views were typed: only `{"kind":"Grid"}`.
    let legacy = serde_json::json!({ "kind": "Grid" });
    let view: View = serde_json::from_value(legacy).unwrap();
    assert_eq!(view.id, ViewId(0));
    assert_eq!(view.name, "");
    assert!(matches!(view.kind, ViewKind::Grid(GridSettings {})));
}

#[test]
fn unknown_kind_fails_to_parse_so_the_store_can_preserve_it() {
    // A future build's kind must NOT silently parse as a known one — the store
    // relies on a parse error to keep it verbatim.
    let future = serde_json::json!({ "kind": "Mosaic", "tiles": 9 });
    assert!(serde_json::from_value::<View>(future).is_err());
}
