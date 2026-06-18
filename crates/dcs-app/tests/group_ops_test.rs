//! Group-scoped batch ops behind the header context menu and the palette's
//! "…all in this group" commands (registry `SelectGroup`/`AcceptGroup`/…).
//! Driven through a real folder + tag bands so the index→members mapping is
//! exercised end to end.

use std::path::{Path, PathBuf};
use std::thread::sleep;
use std::time::Duration;

use dcs_app::{ActionEffect, AppAction, Axis, Session, VerdictFilter};
use dcs_domain::cull::AcceptState;
use dcs_domain::photo::PhotoId;
use dcs_domain::tag::Color;
use image::{Rgb, RgbImage};

fn temp_folder(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("dcs_groupops_{}_{}", std::process::id(), tag));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn write_jpeg(dir: &Path, name: &str) {
    let mut img = RgbImage::new(32, 32);
    for (x, y, px) in img.enumerate_pixels_mut() {
        *px = Rgb([(x % 256) as u8, (y % 256) as u8, 128]);
    }
    img.save(dir.join(name)).expect("encode jpeg");
}

fn drain_until(session: &mut Session, want: usize) {
    for _ in 0..3000 {
        session.tick();
        if session.photo_count() >= want && !session.is_scanning() {
            return;
        }
        sleep(Duration::from_millis(1));
    }
}

/// Four named JPEGs, sorted name-asc → display order a,b,c,d. Returns their ids.
/// `tag` names a per-test folder so parallel tests never share a directory.
fn open_four(tag: &str) -> (Session, Vec<PhotoId>) {
    let dir = temp_folder(tag);
    for s in ["a", "b", "c", "d"] {
        write_jpeg(&dir, &format!("{s}.jpg"));
    }
    let mut session = Session::new();
    session.open_folder(dir);
    drain_until(&mut session, 4);
    let ids: Vec<PhotoId> = (0..4)
        .map(|i| session.cell_info(i).expect("cell").id)
        .collect();
    (session, ids)
}

/// Tag a,b "red" and c "blue", leaving d untagged, then group by tag.
/// Returns (session, ids, red_band, blue_band, untagged_band).
fn tagged_bands(tag: &str) -> (Session, Vec<PhotoId>, usize, usize, usize) {
    let (mut session, ids) = open_four(tag);
    let red = session
        .create_tag("red", Color { r: 200, g: 0, b: 0 })
        .expect("red");
    let blue = session
        .create_tag("blue", Color { r: 0, g: 0, b: 200 })
        .expect("blue");
    session.assign_tag(red, &[ids[0], ids[1]]);
    session.assign_tag(blue, &[ids[2]]);
    session.set_axis(Axis::Tag);

    let band = |s: &Session, title: &str| {
        s.groups()
            .iter()
            .position(|g| g.title == title)
            .unwrap_or_else(|| panic!("no band {title}"))
    };
    let r = band(&session, "red");
    let b = band(&session, "blue");
    let u = band(&session, "Untagged");
    (session, ids, r, b, u)
}

#[test]
fn member_ids_are_the_bands_visible_photos() {
    let (session, ids, red, _blue, untagged) = tagged_bands("members");
    let mut red_members = session.group_member_ids(red);
    red_members.sort_by_key(|p| p.0);
    let mut expect = vec![ids[0], ids[1]];
    expect.sort_by_key(|p| p.0);
    assert_eq!(red_members, expect);
    assert_eq!(session.group_member_ids(untagged), vec![ids[3]]);
    // A stale index resolves to nothing rather than panicking.
    assert!(session.group_member_ids(999).is_empty());
}

#[test]
fn focused_group_maps_the_cursor_to_its_band() {
    let (mut session, _ids, red, _blue, _u) = tagged_bands("focused");
    let start = session.groups()[red].start;
    session.set_focus(start, false);
    assert_eq!(session.focused_group(), Some(red));
    // group_of_index agrees for the same cell.
    assert_eq!(session.group_of_index(start), Some(red));
}

#[test]
fn select_group_selects_exactly_its_members() {
    let (mut session, ids, red, _blue, _u) = tagged_bands("select");
    session.select_group(red);
    assert_eq!(session.selection_count(), 2);
    assert!(session.is_selected(ids[0]));
    assert!(session.is_selected(ids[1]));
    assert!(!session.is_selected(ids[2]));
}

#[test]
fn set_group_state_is_bulk_and_undoable() {
    let (mut session, ids, red, _blue, _u) = tagged_bands("setstate");
    session.set_group_state(red, AcceptState::Accepted);
    assert_eq!(session.verdict(ids[0]), AcceptState::Accepted);
    assert_eq!(session.verdict(ids[1]), AcceptState::Accepted);
    assert_eq!(
        session.verdict(ids[2]),
        AcceptState::Unreviewed,
        "blue untouched"
    );

    assert!(session.undo());
    assert_eq!(session.verdict(ids[0]), AcceptState::Unreviewed);
    assert_eq!(session.verdict(ids[1]), AcceptState::Unreviewed);
}

#[test]
fn group_has_tags_gates_the_untag_affordance() {
    let (session, _ids, red, _blue, untagged) = tagged_bands("hastags");
    assert!(session.group_has_tags(red));
    assert!(
        !session.group_has_tags(untagged),
        "untagged band offers no untag"
    );
}

#[test]
fn group_title_skips_the_stream() {
    // Axis::None is one headerless stream — no group menu, no focused group.
    let (mut session, _ids) = open_four("stream");
    session.set_axis(Axis::None);
    session.set_focus(0, false);
    assert_eq!(session.focused_group(), None);
    assert_eq!(session.group_title(0), None);
    assert_eq!(
        session.group_of_index(0),
        None,
        "in-stream cell has no group"
    );
}

#[test]
fn run_action_accept_and_reject_group() {
    let (mut session, ids, red, _blue, _u) = tagged_bands("ra_verdict");
    assert_eq!(
        session.run_action(AppAction::AcceptGroup(red)),
        ActionEffect::None
    );
    assert_eq!(session.verdict(ids[0]), AcceptState::Accepted);
    assert_eq!(session.verdict(ids[1]), AcceptState::Accepted);

    assert_eq!(
        session.run_action(AppAction::RejectGroup(red)),
        ActionEffect::None
    );
    assert_eq!(session.verdict(ids[0]), AcceptState::Rejected);
    assert_eq!(session.verdict(ids[1]), AcceptState::Rejected);
}

#[test]
fn run_action_tag_group_selects_then_opens_palette() {
    let (mut session, ids, red, _blue, _u) = tagged_bands("ra_tag");
    let effect = session.run_action(AppAction::TagGroup(red));
    assert_eq!(effect, ActionEffect::OpenTagPalette);
    assert_eq!(
        session.selection_count(),
        2,
        "group selected for the palette"
    );
    assert!(session.is_selected(ids[0]));
    assert!(session.is_selected(ids[1]));

    let effect = session.run_action(AppAction::UntagGroup(red));
    assert_eq!(effect, ActionEffect::OpenUntagPalette);
    assert_eq!(session.selection_count(), 2);
}

#[test]
fn run_action_select_group_is_a_no_op_effect() {
    let (mut session, _ids, red, _blue, _u) = tagged_bands("ra_select");
    assert_eq!(
        session.run_action(AppAction::SelectGroup(red)),
        ActionEffect::None
    );
    assert_eq!(session.selection_count(), 2);
}

#[test]
fn stale_index_action_is_a_silent_no_op() {
    let (mut session, ids, _red, _blue, _u) = tagged_bands("ra_stale");
    // A group index past the end touches nothing and never panics.
    assert_eq!(
        session.run_action(AppAction::AcceptGroup(999)),
        ActionEffect::None
    );
    assert_eq!(session.verdict(ids[0]), AcceptState::Unreviewed);
    assert_eq!(session.selection_count(), 0);
}

#[test]
fn set_group_state_under_filter_drops_the_satisfied_band() {
    // The load-bearing case: accepting a band while the Unreviewed filter is
    // active removes it from the visible groups, and the remaining indices stay
    // addressable (no panic, correct membership).
    let (mut session, ids, red, _blue, _u) = tagged_bands("ra_filter");
    session.set_filter(VerdictFilter::Unreviewed);
    assert!(
        session.groups().iter().any(|g| g.title == "red"),
        "red visible while unreviewed"
    );

    session.set_group_state(red, AcceptState::Accepted);
    assert_eq!(session.verdict(ids[0]), AcceptState::Accepted);
    assert!(
        !session.groups().iter().any(|g| g.title == "red"),
        "accepted band leaves the Unreviewed view"
    );
    // Untagged band (d, still unreviewed) is still there and addressable.
    let u = session
        .groups()
        .iter()
        .position(|g| g.title == "Untagged")
        .expect("untagged remains");
    assert_eq!(session.group_member_ids(u), vec![ids[3]]);
}

#[test]
fn selection_survives_a_filter_that_hides_it() {
    // select_ids stores stable ids; a filter hiding them shrinks the visible
    // order but the selection count holds (the "survives filtering" contract).
    let (mut session, _ids, red, _blue, _u) = tagged_bands("ra_survive");
    session.select_group(red); // red members are unreviewed
    assert_eq!(session.selection_count(), 2);
    session.set_filter(VerdictFilter::Accepted); // hides the unreviewed red band
    assert_eq!(
        session.selection_count(),
        2,
        "hidden selection is retained, not dropped"
    );
}
