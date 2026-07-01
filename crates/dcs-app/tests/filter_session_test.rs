//! The chip filter through a real session: verdict toggle folded into the one
//! `Filter`, tag chips narrowing the visible set, AND/OR within a group, clear,
//! and live re-resolution when an owned verdict/tag edit lands.

use std::path::{Path, PathBuf};
use std::thread::sleep;
use std::time::Duration;

use dcs_app::{Session, TagId, VerdictFilter};
use dcs_domain::cull::AcceptState;
use dcs_domain::tag::palette_color;
use image::{Rgb, RgbImage};

fn temp_folder(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("dcs_filt_{}_{}", std::process::id(), tag));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn write_jpeg(dir: &Path, name: &str, seed: u8) {
    let mut img = RgbImage::new(16, 16);
    for (x, y, px) in img.enumerate_pixels_mut() {
        *px = Rgb([x as u8 ^ seed, y as u8, seed]);
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

/// Four named JPEGs a,b,c,d (display order a,b,c,d under the default time sort,
/// undated → name tiebreak). Returns the session.
fn open_four(tag: &str) -> Session {
    let dir = temp_folder(tag);
    for (i, n) in ["a.jpg", "b.jpg", "c.jpg", "d.jpg"].iter().enumerate() {
        write_jpeg(&dir, n, i as u8 + 1);
    }
    let mut session = Session::new();
    session.open_folder(dir);
    drain_until(&mut session, 4);
    session
}

/// Select display indices `idxs` (first replaces, rest extend), then tag them.
fn tag_indices(session: &mut Session, tag: TagId, idxs: &[usize]) {
    for (n, &i) in idxs.iter().enumerate() {
        session.set_focus(i, n > 0);
    }
    session.tag_selection(tag);
}

fn make_tag(session: &mut Session, name: &str, n: usize) -> TagId {
    session
        .create_tag(name.to_string(), palette_color(n))
        .expect("create tag")
}

/// The index of the single tag group (all-tag chips) in the active filter.
fn tag_group_index(session: &Session) -> usize {
    session
        .active_filter()
        .groups
        .iter()
        .position(|g| {
            g.chips
                .iter()
                .all(|c| matches!(c, dcs_domain::filter::FilterChip::Tag(_)))
        })
        .expect("a tag group exists")
}

#[test]
fn display_index_of_tracks_the_visible_set() {
    // The board's canvas menu relies on this to aim the pool selection at a
    // right-clicked photo, and to hide pool actions for a photo filtered out of
    // the grid.
    let mut session = open_four("dispidx");
    let a = session.photo_at(0).unwrap().id;
    let b = session.photo_at(1).unwrap().id;
    assert_eq!(session.display_index_of(a), Some(0));
    assert_eq!(session.display_index_of(b), Some(1));
    assert_eq!(session.display_index_of(dcs_domain::photo::PhotoId(9999)), None);

    // Accept only `a`, then filter to accepted: `b` is no longer visible.
    session.set_focus(0, false);
    session.accept();
    session.set_filter(VerdictFilter::Accepted);
    assert_eq!(session.display_index_of(a), Some(0));
    assert_eq!(session.display_index_of(b), None, "filtered-out photo has no index");
}

#[test]
fn verdict_toggle_round_trips_through_the_filter() {
    let mut session = open_four("verdict");
    assert!(!session.is_filtered());
    assert_eq!(session.photo_count(), 4);

    // Accept just the first photo.
    session.set_focus(0, false);
    session.accept();

    session.set_filter(VerdictFilter::Accepted);
    assert!(session.is_filtered());
    assert_eq!(session.filter(), VerdictFilter::Accepted);
    assert_eq!(session.photo_count(), 1);

    session.set_filter(VerdictFilter::All);
    assert!(!session.is_filtered());
    assert_eq!(session.filter(), VerdictFilter::All);
    assert_eq!(session.photo_count(), 4);
}

#[test]
fn tag_chip_narrows_and_removes() {
    let mut session = open_four("tagchip");
    let red = make_tag(&mut session, "red", 1);
    tag_indices(&mut session, red, &[0, 1]); // a, b

    session.add_tag_chip(red);
    assert!(session.is_filtered());
    assert_eq!(session.photo_count(), 2);

    // Adding the same tag again is a no-op.
    session.add_tag_chip(red);
    assert_eq!(session.photo_count(), 2);

    let g = tag_group_index(&session);
    session.remove_filter_chip(g, 0);
    assert!(!session.is_filtered());
    assert_eq!(session.photo_count(), 4);
}

#[test]
fn clear_drops_every_chip() {
    let mut session = open_four("clear");
    let red = make_tag(&mut session, "red", 1);
    tag_indices(&mut session, red, &[0, 1]);
    session.add_tag_chip(red);
    session.set_filter(VerdictFilter::Unreviewed);
    assert!(session.is_filtered());

    session.clear_filter();
    assert!(!session.is_filtered());
    assert_eq!(session.filter(), VerdictFilter::All);
    assert_eq!(session.photo_count(), 4);
}

#[test]
fn owned_verdict_edit_re_resolves_live() {
    let mut session = open_four("reresolve");
    // Filter to Unreviewed — all four show, since none are reviewed yet.
    session.set_filter(VerdictFilter::Unreviewed);
    assert_eq!(session.photo_count(), 4);

    // Accepting a visible photo must drop it from the Unreviewed view at once.
    session.set_focus(0, false);
    session.accept();
    assert_eq!(session.photo_count(), 3);
}

#[test]
fn verdict_chip_composes_with_a_tag_chip() {
    let mut session = open_four("compose");
    let red = make_tag(&mut session, "red", 1);
    tag_indices(&mut session, red, &[0, 1]); // a, b red
    session.set_focus(0, false);
    session.accept(); // a accepted

    session.add_tag_chip(red); // (red) → a, b
    assert_eq!(session.photo_count(), 2);
    session.set_filter(VerdictFilter::Accepted); // (red) AND (accepted) → a
    assert_eq!(session.photo_count(), 1);
    assert_eq!(session.filter(), VerdictFilter::Accepted);
}

#[test]
fn multi_verdict_toggle_filters_the_union() {
    let mut session = open_four("multiverdict");
    session.set_focus(0, false);
    session.accept(); // a accepted
    session.set_focus(1, false);
    session.reject(); // b rejected; c, d unreviewed

    session.toggle_verdict_filter(AcceptState::Accepted);
    assert_eq!(session.photo_count(), 1); // just a
    session.toggle_verdict_filter(AcceptState::Rejected);
    // (accepted OR rejected) → a, b
    assert_eq!(session.photo_count(), 2);
    assert!(session.verdict_filter_active(AcceptState::Accepted));
    assert!(session.verdict_filter_active(AcceptState::Rejected));

    // Untoggling accepted leaves just rejected → b.
    session.toggle_verdict_filter(AcceptState::Accepted);
    assert_eq!(session.photo_count(), 1);
    assert!(!session.verdict_filter_active(AcceptState::Accepted));

    // Untoggling the last verdict clears the filter entirely.
    session.toggle_verdict_filter(AcceptState::Rejected);
    assert!(!session.is_filtered());
    assert_eq!(session.photo_count(), 4);
}

#[test]
fn current_filter_export_scope_tracks_the_chips() {
    use dcs_app::ExportScope;
    let mut session = open_four("exportscope");
    let red = make_tag(&mut session, "red", 1);
    tag_indices(&mut session, red, &[0, 1]); // a, b red

    // With no filter, "Current filter" still resolves to the whole pool — it's
    // only surfaced in the dialog when filtered, but the count must be honest.
    assert_eq!(session.export_scope_count(ExportScope::CurrentFilter), 4);

    session.set_focus(0, false);
    session.accept(); // a accepted while still fully visible

    session.add_tag_chip(red); // (red) → a, b
    assert_eq!(session.export_scope_count(ExportScope::CurrentFilter), 2);

    session.set_filter(VerdictFilter::Accepted); // (red) AND (accepted) → a
    assert_eq!(session.export_scope_count(ExportScope::CurrentFilter), 1);
}

#[test]
fn within_group_and_or_toggle_changes_the_set() {
    let mut session = open_four("andor");
    let red = make_tag(&mut session, "red", 1);
    let blue = make_tag(&mut session, "blue", 2);
    tag_indices(&mut session, red, &[0, 1]); // a, b red
    tag_indices(&mut session, blue, &[0]); // a blue

    session.add_tag_chip(red);
    session.add_tag_chip(blue);
    // Default OR within the tag group: red ∪ blue = a, b.
    assert_eq!(session.photo_count(), 2);

    let g = tag_group_index(&session);
    session.toggle_filter_group_op(g);
    // AND: red ∩ blue = a only.
    assert_eq!(session.photo_count(), 1);
}
