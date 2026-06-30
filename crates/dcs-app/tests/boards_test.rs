//! The views store's JSON bridge: typed boards round-trip, unknown kinds are
//! preserved verbatim *alongside* a typed board, and the view-id counter is
//! floored above the largest known id so a reopened project never reuses one.

use dcs_app::boards::BoardStore;
use dcs_domain::photo::PhotoId;
use dcs_domain::view::{Pos, ViewId};
use serde_json::json;

fn pid(n: u32) -> PhotoId {
    PhotoId(n)
}

#[test]
fn known_board_and_unknown_kind_coexist_and_round_trip() {
    let board = json!({ "id": 1, "name": "Board", "kind": "Board", "items": [] });
    // A future build's view kind this build doesn't understand.
    let mosaic = json!({ "kind": "Mosaic", "id": 5, "tiles": [1, 2, 3], "nest": { "a": true } });
    let mut store = BoardStore::from_values(vec![board, mosaic.clone()], 0);

    // The known board is live and mutable.
    let v = store.ensure_board().0;
    assert_eq!(v, ViewId(1), "existing board reused, not recreated");
    store.apply_add(v, &[(pid(7), Pos::new(3.0, 4.0))]);
    assert_eq!(store.items(v).len(), 1);

    // The unknown kind survives a round-trip byte-for-byte, and order is kept.
    let out = store.to_values();
    assert_eq!(out.len(), 2);
    assert_eq!(out[1], mosaic, "unknown kind preserved verbatim");
    assert_eq!(out[0]["kind"], "Board");
    assert_eq!(out[0]["items"].as_array().unwrap().len(), 1);
}

#[test]
fn next_view_id_floors_above_largest_known_id() {
    // Persisted counter is stale (0) but a known view already uses id 5.
    let store = BoardStore::from_values(vec![json!({ "id": 5, "kind": "Grid" })], 0);
    assert_eq!(
        store.next_view_id(),
        6,
        "counter floored past the largest id"
    );

    // A persisted counter ahead of the ids wins.
    let store = BoardStore::from_values(vec![json!({ "id": 2, "kind": "Grid" })], 9);
    assert_eq!(store.next_view_id(), 9);
}

#[test]
fn ensure_board_creates_one_when_absent_and_allocates_a_fresh_id() {
    let mut store = BoardStore::from_values(vec![json!({ "id": 3, "kind": "Grid" })], 4);
    assert!(!store.has_board());
    let (id, created) = store.ensure_board();
    assert!(created);
    assert_eq!(id, ViewId(4), "new board takes the next free id");
    assert_eq!(store.next_view_id(), 5);
    // Idempotent: a second call finds the one just made.
    let (again, created2) = store.ensure_board();
    assert_eq!(again, id);
    assert!(!created2);
}

#[test]
fn ensure_board_on_empty_store_starts_at_zero() {
    let mut store = BoardStore::default();
    let (id, created) = store.ensure_board();
    assert!(created);
    assert_eq!(id, ViewId(0));
    assert!(store.has_board());
}

#[test]
fn add_and_move_survive_a_to_values_round_trip() {
    let mut store = BoardStore::default();
    let v = store.ensure_board().0;
    store.apply_add(
        v,
        &[
            (pid(1), Pos::new(10.0, 20.0)),
            (pid(2), Pos::new(30.0, 40.0)),
        ],
    );
    store.apply_move(v, &[(pid(1), Pos::new(15.0, 25.0))]);

    let next = store.next_view_id();
    let reloaded = BoardStore::from_values(store.to_values(), next);
    let items = reloaded.items(v);
    assert_eq!(items.len(), 2);
    assert_eq!(items[0].photo, pid(1));
    assert_eq!(items[0].pos, Pos::new(15.0, 25.0));
    assert_eq!(items[1].pos, Pos::new(30.0, 40.0));
}

#[test]
fn raise_reorders_to_top_and_reports_change() {
    let mut store = BoardStore::default();
    let v = store.ensure_board().0;
    store.apply_add(
        v,
        &[
            (pid(1), Pos::new(0.0, 0.0)),
            (pid(2), Pos::new(0.0, 0.0)),
            (pid(3), Pos::new(0.0, 0.0)),
        ],
    );
    assert!(store.raise(v, pid(1)), "bottom item rises");
    assert_eq!(
        store.items(v).iter().map(|i| i.photo).collect::<Vec<_>>(),
        vec![pid(2), pid(3), pid(1)]
    );
    assert!(!store.raise(v, pid(1)), "already on top → no change");
    assert!(!store.raise(v, pid(9)), "absent photo → no change");
    assert!(!store.raise(ViewId(99), pid(1)), "unknown view → no change");
}

#[test]
fn non_finite_positions_are_rejected_not_persisted() {
    let mut store = BoardStore::default();
    let v = store.ensure_board().0;
    // A degenerate drop position must not enter owned state.
    let deltas = store.apply_add(v, &[(pid(1), Pos::new(f32::NAN, 0.0))]);
    assert!(deltas.is_empty(), "non-finite placement records no delta");
    assert!(store.items(v).is_empty());
    // to_values must still serialize cleanly (no panic, no null).
    assert!(store.to_values().iter().all(|x| !x.is_null()));
}
