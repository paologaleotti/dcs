//! Session-level tag behavior: assignment through the conductor, the tag axis
//! (bands + projections + untagged tail), color-tag toggles, undo, and that
//! tags survive a save/reopen round-trip.

use std::path::{Path, PathBuf};
use std::thread::sleep;
use std::time::Duration;

use dcs_app::{Axis, Session};
use dcs_domain::grouping::GroupKind;
use dcs_domain::photo::PhotoId;
use dcs_domain::tag::palette_color;
use image::{Rgb, RgbImage};

fn temp_folder(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("dcs_tags_{}_{}", std::process::id(), tag));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn write_jpeg(dir: &Path, name: &str) {
    // Vary the content per file so each gets a distinct content fingerprint —
    // identical bytes would collide in the fingerprint→id map on reopen and make
    // id reclaim (and thus tag re-linking) ambiguous.
    let seed = name.bytes().map(u32::from).sum::<u32>();
    let mut img = RgbImage::new(32, 32);
    for (x, y, px) in img.enumerate_pixels_mut() {
        *px = Rgb([
            ((x + seed) % 256) as u8,
            ((y * 7 + seed) % 256) as u8,
            (seed % 256) as u8,
        ]);
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

/// A session over `n` decodable JPEGs (`a.jpg`, `b.jpg`, …), fully scanned.
fn session_with(tag: &str, n: usize) -> (Session, PathBuf) {
    let dir = temp_folder(tag);
    for i in 0..n {
        write_jpeg(&dir, &format!("{}.jpg", (b'a' + i as u8) as char));
    }
    let mut session = Session::new();
    session.open_folder(dir.clone());
    drain_until(&mut session, n);
    assert_eq!(session.photo_count(), n, "all photos scanned");
    (session, dir)
}

fn ids(session: &Session) -> Vec<PhotoId> {
    (0..session.photo_count())
        .filter_map(|i| session.photo_at(i).map(|p| p.id))
        .collect()
}

#[test]
fn assign_tag_shows_on_the_photo_and_in_the_band() {
    let (mut session, _dir) = session_with("assign", 3);
    let all = ids(&session);
    let t = session.create_tag("temples", palette_color(1)).unwrap();
    session.assign_tag(t, &all[..2]); // a, b tagged; c not

    assert!(session.is_tagged(t, all[0]));
    assert!(!session.is_tagged(t, all[2]));
    assert_eq!(session.tag_photo_count(t), 2);

    session.set_axis(Axis::Tag);
    let groups = session.groups();
    assert_eq!(groups[0].kind, GroupKind::Tag(t));
    assert_eq!(groups[0].title, "temples");
    assert_eq!(groups[0].count, 2);
    assert_eq!(
        groups.last().unwrap().title,
        "Untagged",
        "c falls to the tail"
    );
}

#[test]
fn multi_tagged_photo_projects_into_each_band() {
    let (mut session, _dir) = session_with("projection", 2);
    let all = ids(&session);
    let temples = session.create_tag("temples", palette_color(1)).unwrap();
    let shrines = session.create_tag("shrines", palette_color(2)).unwrap();
    session.assign_tag(temples, &[all[0]]);
    session.assign_tag(shrines, &all); // a + b

    session.set_axis(Axis::Tag);
    let groups = session.groups();
    // a appears in both bands (projection): total cells across bands = 3 for 2 photos.
    let total: usize = groups.iter().map(|g| g.count).sum();
    assert_eq!(total, 3);
    assert!(
        groups
            .iter()
            .any(|g| g.kind == GroupKind::Tag(temples) && g.count == 1)
    );
    assert!(
        groups
            .iter()
            .any(|g| g.kind == GroupKind::Tag(shrines) && g.count == 2)
    );
}

#[test]
fn cell_info_carries_tag_colors_for_strips() {
    let (mut session, _dir) = session_with("strips", 1);
    let all = ids(&session);
    let red = session.create_tag("red", palette_color(1)).unwrap();
    let blue = session.create_tag("blue", palette_color(6)).unwrap();
    session.assign_tag(red, &all);
    session.assign_tag(blue, &all);

    let info = session.cell_info(0).unwrap();
    assert_eq!(
        info.tag_colors[0],
        Some(palette_color(1)),
        "lowest id first"
    );
    assert_eq!(info.tag_colors[1], Some(palette_color(6)));
    assert_eq!(info.tag_colors[2], None);
}

#[test]
fn undo_redo_round_trips_a_tag_assignment() {
    let (mut session, _dir) = session_with("undo", 2);
    let all = ids(&session);
    let t = session.create_tag("x", palette_color(1)).unwrap();
    session.assign_tag(t, &all);
    assert_eq!(session.tag_photo_count(t), 2);

    assert!(session.undo(), "undo the assign");
    assert_eq!(session.tag_photo_count(t), 0);
    assert!(session.tag_def(t).is_some(), "tag still exists");

    assert!(session.redo(), "redo the assign");
    assert_eq!(session.tag_photo_count(t), 2);
}

#[test]
fn selection_counts_mark_already_added_tags() {
    let (mut session, _dir) = session_with("counts", 3);
    let all = ids(&session);
    let t = session.create_tag("temples", palette_color(1)).unwrap();
    session.assign_tag(t, &all[..2]); // on 2 of 3

    // Select all three, then ask how much of the selection carries each tag.
    session.select_all_visible();
    let counts = session.tags_with_selection_counts();
    let (_, on) = counts.iter().find(|(tag, _)| tag.id == t).unwrap();
    assert_eq!(*on, 2, "two of the three selected carry the tag");

    session.assign_tag(t, &all); // now all three
    let counts = session.tags_with_selection_counts();
    assert_eq!(counts.iter().find(|(tag, _)| tag.id == t).unwrap().1, 3);
}

#[test]
fn untag_selection_removes_only_from_selected() {
    let (mut session, _dir) = session_with("untag", 2);
    let all = ids(&session);
    let t = session.create_tag("x", palette_color(1)).unwrap();
    session.assign_tag(t, &all);
    assert_eq!(session.tag_photo_count(t), 2);

    // Select only the first photo, then remove the tag from the selection.
    session.click_select(0);
    assert_eq!(
        session.selection_tags(),
        session.all_tags(),
        "selected photo has the tag"
    );
    session.untag_selection(t);
    assert_eq!(
        session.tag_photo_count(t),
        1,
        "only the selected photo lost it"
    );
    assert!(!session.is_tagged(t, all[0]));
    assert!(session.is_tagged(t, all[1]));
}

#[test]
fn manager_delete_drops_tag_and_assignments_undoably() {
    let (mut session, _dir) = session_with("mgr_del", 2);
    let all = ids(&session);
    let t = session.create_tag("temples", palette_color(1)).unwrap();
    session.assign_tag(t, &all);

    session.delete_tag(t);
    assert!(session.tag_def(t).is_none(), "definition gone");
    assert!(session.all_tags().is_empty());
    assert!(!session.is_tagged(t, all[0]), "assignments gone");

    assert!(session.undo(), "delete is undoable");
    assert!(session.tag_def(t).is_some(), "tag restored");
    assert_eq!(session.tag_photo_count(t), 2, "assignments restored");
}

#[test]
fn manager_rename_and_recolor() {
    let (mut session, _dir) = session_with("mgr_edit", 1);
    let t = session.create_tag("temple", palette_color(1)).unwrap();

    session.rename_tag(t, "temples");
    assert_eq!(session.tag_def(t).unwrap().name, "temples");

    session.set_tag_color(t, palette_color(3));
    assert_eq!(session.tag_def(t).unwrap().color, palette_color(3));

    assert!(session.undo(), "recolor undoable");
    assert_eq!(session.tag_def(t).unwrap().color, palette_color(1));
}

#[test]
fn manager_rename_onto_existing_merges() {
    let (mut session, _dir) = session_with("mgr_merge", 2);
    let all = ids(&session);
    let temples = session.create_tag("temples", palette_color(1)).unwrap();
    let shrines = session.create_tag("shrines", palette_color(2)).unwrap();
    session.assign_tag(temples, &[all[0]]);
    session.assign_tag(shrines, &[all[1]]);

    // Renaming "shrines" onto "temples" merges the two.
    session.rename_tag(shrines, "temples");
    assert!(session.tag_def(shrines).is_none(), "merged-from gone");
    assert_eq!(session.tag_photo_count(temples), 2, "photos unioned");
}

#[test]
fn tags_survive_save_and_reopen() {
    let (mut session, dir) = session_with("persist", 2);
    let all = ids(&session);
    let t = session.create_tag("temples", palette_color(1)).unwrap();
    session.assign_tag(t, &[all[0]]);
    session.save_if_dirty().expect("save");
    drop(session);

    let mut reopened = Session::new();
    reopened.open_folder(dir);
    drain_until(&mut reopened, 2);

    let tags = reopened.all_tags();
    assert_eq!(tags.len(), 1);
    assert_eq!(tags[0].name, "temples");
    let tagged = ids(&reopened)
        .into_iter()
        .filter(|&id| reopened.is_tagged(tags[0].id, id))
        .count();
    assert_eq!(tagged, 1, "the one tagged photo kept its tag across reopen");
}
