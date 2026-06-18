//! Owned tag store: assignment indexes, id allocation, rename→merge, delete,
//! and the apply/revert delta primitives. No I/O.

use dcs_app::tags::TagStore;
use dcs_domain::photo::PhotoId;
use dcs_domain::tag::{Color, TagId, palette_color};

fn store_with(names: &[&str]) -> (TagStore, Vec<TagId>) {
    let mut s = TagStore::new();
    let mut ids = Vec::new();
    for (i, name) in names.iter().enumerate() {
        let deltas = s.apply_create((*name).into(), palette_color(i + 1));
        // Created delta carries the allocated id.
        if let Some(dcs_domain::command::TagDelta::Created(tag)) = deltas.first() {
            ids.push(tag.id);
        }
    }
    (s, ids)
}

#[test]
fn create_allocates_increasing_ids() {
    let (s, ids) = store_with(&["a", "b", "c"]);
    assert_eq!(ids, vec![TagId(0), TagId(1), TagId(2)]);
    assert_eq!(s.next_id(), 3);
    assert_eq!(s.defs().len(), 3);
}

#[test]
fn assign_dedups_and_skips_already_tagged() {
    let (mut s, ids) = store_with(&["temples"]);
    let t = ids[0];
    let deltas = s.apply_assign(t, &[PhotoId(1), PhotoId(1), PhotoId(2)]);
    assert_eq!(deltas.len(), 2, "one assign per unique, newly-tagged photo");
    assert!(s.is_assigned(t, PhotoId(1)));
    assert_eq!(s.photo_count(t), 2);

    // Re-assigning an already-tagged photo records nothing.
    assert!(s.apply_assign(t, &[PhotoId(1)]).is_empty());
}

#[test]
fn assign_to_unknown_tag_is_noop() {
    let mut s = TagStore::new();
    assert!(s.apply_assign(TagId(99), &[PhotoId(1)]).is_empty());
}

#[test]
fn unassign_only_records_tagged_photos() {
    let (mut s, ids) = store_with(&["t"]);
    let t = ids[0];
    s.apply_assign(t, &[PhotoId(1)]);
    let deltas = s.apply_unassign(t, &[PhotoId(1), PhotoId(2)]);
    assert_eq!(deltas.len(), 1, "photo 2 wasn't tagged");
    assert_eq!(s.photo_count(t), 0);
    assert!(s.tags_of(PhotoId(1)).is_empty());
}

#[test]
fn tags_of_is_ordered_by_id() {
    let (mut s, ids) = store_with(&["a", "b"]);
    s.apply_assign(ids[1], &[PhotoId(1)]);
    s.apply_assign(ids[0], &[PhotoId(1)]);
    assert_eq!(s.tags_of(PhotoId(1)), vec![ids[0], ids[1]]);
}

#[test]
fn rename_changes_name() {
    let (mut s, ids) = store_with(&["temple"]);
    let deltas = s.apply_rename(ids[0], "temples".into());
    assert_eq!(deltas.len(), 1);
    assert_eq!(s.def(ids[0]).unwrap().name, "temples");
}

#[test]
fn rename_onto_existing_name_merges() {
    // spec §2.7: rename-to-existing = merge.
    let (mut s, ids) = store_with(&["temples", "shrines"]);
    let (temples, shrines) = (ids[0], ids[1]);
    s.apply_assign(shrines, &[PhotoId(1), PhotoId(2)]);
    s.apply_assign(temples, &[PhotoId(2)]);

    // Rename "shrines" → "Temples" (case-insensitive match) merges into temples.
    s.apply_rename(shrines, "Temples".into());
    assert!(s.def(shrines).is_none(), "merged-from tag removed");
    assert_eq!(s.photo_count(temples), 2, "photos unioned, counted once");
    assert!(s.is_assigned(temples, PhotoId(1)));
}

#[test]
fn merge_unions_photos_and_drops_from() {
    let (mut s, ids) = store_with(&["a", "b"]);
    let (a, b) = (ids[0], ids[1]);
    s.apply_assign(a, &[PhotoId(1)]);
    s.apply_assign(b, &[PhotoId(1), PhotoId(2)]);
    s.apply_merge(a, b);
    assert!(s.def(b).is_none());
    assert_eq!(s.photo_count(a), 2);
}

#[test]
fn delete_removes_def_and_assignments() {
    let (mut s, ids) = store_with(&["a"]);
    s.apply_assign(ids[0], &[PhotoId(1)]);
    s.apply_delete(ids[0]);
    assert!(s.def(ids[0]).is_none());
    assert!(s.tags_of(PhotoId(1)).is_empty());
}

#[test]
fn revert_undoes_a_merge_exactly() {
    let (mut s, ids) = store_with(&["a", "b"]);
    let (a, b) = (ids[0], ids[1]);
    s.apply_assign(a, &[PhotoId(1)]);
    s.apply_assign(b, &[PhotoId(1), PhotoId(2)]);
    let deltas = s.apply_merge(a, b);

    s.revert(&deltas);
    assert!(s.def(b).is_some(), "from-tag restored");
    assert_eq!(s.photo_count(b), 2, "b's photos restored");
    assert_eq!(s.photo_count(a), 1, "a back to its own photo only");
    assert!(!s.is_assigned(a, PhotoId(2)), "merge-added link removed");
}

#[test]
fn revert_then_apply_round_trips_a_delete() {
    let (mut s, ids) = store_with(&["a"]);
    let t = ids[0];
    s.apply_assign(t, &[PhotoId(1), PhotoId(2)]);
    let deltas = s.apply_delete(t);

    s.revert(&deltas);
    assert!(s.def(t).is_some());
    assert_eq!(s.photo_count(t), 2);

    s.apply(&deltas);
    assert!(s.def(t).is_none(), "redo deletes again");
}

#[test]
fn from_state_rebuilds_indexes_and_ignores_orphan_assignments() {
    let (seed, ids) = store_with(&["a"]);
    let defs = seed.defs();
    let store = TagStore::from_state(
        defs,
        vec![
            (PhotoId(1), vec![ids[0]]),
            (PhotoId(2), vec![ids[0], TagId(99)]), // 99 has no def → ignored
        ],
        seed.next_id(),
    );
    assert_eq!(store.photo_count(ids[0]), 2);
    assert_eq!(
        store.tags_of(PhotoId(2)),
        vec![ids[0]],
        "orphan tag dropped"
    );
    assert_eq!(store.next_id(), 1);
}

#[test]
fn forget_scrubs_photos_from_every_tag() {
    let (mut s, ids) = store_with(&["a", "b"]);
    s.apply_assign(ids[0], &[PhotoId(1)]);
    s.apply_assign(ids[1], &[PhotoId(1), PhotoId(2)]);
    s.forget(&[PhotoId(1)].into_iter().collect());
    assert_eq!(s.photo_count(ids[0]), 0);
    assert_eq!(s.photo_count(ids[1]), 1);
    assert!(s.tags_of(PhotoId(1)).is_empty());
}

#[test]
fn find_by_name_is_case_insensitive_and_excludes_self() {
    let (s, ids) = store_with(&["Temples"]);
    assert_eq!(s.find_by_name("temples", TagId(99)), Some(ids[0]));
    assert_eq!(s.find_by_name("temples", ids[0]), None, "excludes itself");
    assert_eq!(s.find_by_name("nope", TagId(99)), None);
}

#[test]
fn strip_returns_colors_in_id_order_and_caps() {
    let mut s = TagStore::new();
    let mut tag_ids = Vec::new();
    for i in 0..8 {
        let d = s.apply_create(format!("t{i}"), Color::rgb(i, 0, 0));
        if let dcs_domain::command::TagDelta::Created(t) = &d[0] {
            tag_ids.push(t.id);
        }
    }
    for &t in &tag_ids {
        s.apply_assign(t, &[PhotoId(1)]);
    }
    let strip = s.strip(PhotoId(1));
    // Capped at MAX_STRIP, lowest ids first.
    assert!(strip.iter().all(|c| c.is_some()), "all six slots filled");
    assert_eq!(strip[0], Some(Color::rgb(0, 0, 0)));
    assert_eq!(strip[5], Some(Color::rgb(5, 0, 0)), "7th/8th tags dropped");
}

#[test]
fn recolor_changes_color_and_reverts() {
    let (mut s, ids) = store_with(&["a"]);
    let t = ids[0];
    let deltas = s.apply_recolor(t, Color::rgb(9, 9, 9));
    assert_eq!(deltas.len(), 1);
    assert_eq!(s.def(t).unwrap().color, Color::rgb(9, 9, 9));

    // Same color is a no-op.
    assert!(s.apply_recolor(t, Color::rgb(9, 9, 9)).is_empty());

    s.revert(&deltas);
    assert_eq!(
        s.def(t).unwrap().color,
        palette_color(1),
        "revert restores the prior color"
    );
}

#[test]
fn strip_empty_for_untagged_photo() {
    let s = TagStore::new();
    assert!(s.strip(PhotoId(1)).iter().all(|c| c.is_none()));
}

#[test]
fn color_is_carried_through() {
    let mut s = TagStore::new();
    let deltas = s.apply_create("x".into(), Color::rgb(9, 8, 7));
    if let dcs_domain::command::TagDelta::Created(tag) = &deltas[0] {
        assert_eq!(tag.color, Color::rgb(9, 8, 7));
    } else {
        panic!("expected Created");
    }
}
