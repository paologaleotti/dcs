use std::path::{Path, PathBuf};
use std::thread::sleep;
use std::time::Duration;

use dcs_app::{Axis, Session, VerdictFilter};
use dcs_domain::cull::AcceptState;
use dcs_domain::grouping::GroupKind;
use image::{Rgb, RgbImage};

fn temp_folder(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("dcs_session_{}_{}", std::process::id(), tag));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// A stand-in RAW: content dcs can't decode, but distinct per file. Identity is
/// the content fingerprint, so byte-identical placeholders would be one photo —
/// which no two real RAW files ever are.
fn touch(dir: &Path, name: &str) {
    std::fs::write(dir.join(name), format!("placeholder {name}")).unwrap();
}

/// Identity is the content fingerprint, so the pixels are tinted by filename:
/// byte-identical JPEGs would be one photo to dcs, and id reclaim across a
/// rescan could not tell them apart.
fn write_jpeg(dir: &Path, name: &str, w: u32, h: u32) {
    let tint = name.bytes().fold(0u8, |a, b| a.wrapping_add(b));
    let mut img = RgbImage::new(w, h);
    for (x, y, px) in img.enumerate_pixels_mut() {
        *px = Rgb([(x % 256) as u8, (y % 256) as u8, tint]);
    }
    img.save(dir.join(name)).expect("encode jpeg");
}

/// Drive `tick` until the pool reaches `want` photos or the budget runs out.
fn drain_until(session: &mut Session, want: usize) {
    for _ in 0..3000 {
        session.tick();
        if session.photo_count() >= want && !session.is_scanning() {
            return;
        }
        sleep(Duration::from_millis(1));
    }
}

/// Drive `tick` (re-issuing `request` each pass) until `done` holds or the
/// budget runs out — for async decode results.
fn pump_until(
    session: &mut Session,
    mut request: impl FnMut(&mut Session),
    done: impl Fn(&Session) -> bool,
) {
    for _ in 0..3000 {
        request(session);
        session.tick();
        if done(session) {
            return;
        }
        sleep(Duration::from_millis(1));
    }
}

#[test]
fn rescan_keeps_raw_jpeg_pairs_present_with_no_phantoms() {
    let dir = temp_folder("rescan_pairs");
    // Three JPEG+RAW pairs: real JPEGs decode, empty RAFs pair by stem.
    for s in ["a", "b", "c"] {
        write_jpeg(&dir, &format!("{s}.jpg"), 48, 48);
        touch(&dir, &format!("{s}.raf"));
    }
    let mut session = Session::new();
    session.open_folder(dir.clone());
    drain_until(&mut session, 3);
    assert_eq!(session.photo_count(), 3);
    assert_eq!(session.pool_len(), 3, "each pair is one photo");
    assert_eq!(session.missing_count(), 0);

    // Accept the first photo and persist the project.
    session.set_focus(0, false);
    session.accept();
    session.save().expect("save");

    // Rescan (re-imports both kinds). The pairs must return present and paired,
    // the verdict intact, and — the bug — no phantom missing placeholders and no
    // duplicate photos inflating the pool.
    session.rescan();
    drain_until(&mut session, 3);

    assert_eq!(
        session.pool_len(),
        3,
        "no phantom/duplicate photos after rescan"
    );
    assert_eq!(session.photo_count(), 3, "all pairs still shown");
    assert_eq!(
        session.missing_count(),
        0,
        "no phantom missing placeholders"
    );
    let (acc, _rej, unrev) = session.verdict_counts();
    assert_eq!(acc, 1, "the accepted verdict survives the rescan");
    assert_eq!(unrev, 2);

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn import_progress_reaches_full_then_disappears() {
    let dir = temp_folder("import_progress");
    for i in 0..4 {
        write_jpeg(&dir, &format!("p{i}.jpg"), 40, 40);
    }
    let mut session = Session::new();
    session.open_folder(dir.clone());
    drain_until(&mut session, 4);

    // A fresh folder reports progress out of the four displayable photos.
    let start = session
        .import_progress()
        .expect("import in progress on a cold open");
    assert_eq!(start.total, 4);

    // Drive the background fill until every thumbnail is warm; the bar is then
    // dropped (None) so the status bar hides it.
    pump_until(
        &mut session,
        |s| s.fill_base_background(),
        |s| s.import_progress().is_none(),
    );
    assert!(session.import_progress().is_none(), "all imported → no bar");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn reopen_resumes_import_from_disk_cache() {
    let dir = temp_folder("import_resume");
    for i in 0..4 {
        write_jpeg(&dir, &format!("p{i}.jpg"), 40, 40);
    }
    // A first session warms every thumbnail into the on-disk cache.
    let mut first = Session::new();
    first.open_folder(dir.clone());
    drain_until(&mut first, 4);
    pump_until(
        &mut first,
        |s| s.fill_base_background(),
        |s| s.import_progress().is_none(),
    );
    drop(first);

    // Reopening the same folder recognizes the warm cache the moment the scan
    // settles: the import is already complete, no re-decode needed.
    let mut second = Session::new();
    second.open_folder(dir.clone());
    drain_until(&mut second, 4);
    assert!(
        second.import_progress().is_none(),
        "warm disk cache resumes a finished import without re-warming"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn pairs_raw_with_jpeg_and_hides_raw_only_photos() {
    let dir = temp_folder("scan");
    touch(&dir, "a.jpg"); // JPEG-only
    touch(&dir, "b.JPG"); // JPEG-only
    touch(&dir, "notes.txt"); // ignored
    touch(&dir, "c.raf"); // RAW-only: paired with nothing → hidden in v1
    touch(&dir, "d.jpg"); // d.jpg + d.RAF pair into ONE photo
    touch(&dir, "d.RAF");

    let mut session = Session::new();
    session.open_folder(dir.clone());
    drain_until(&mut session, 3);

    // The pool holds four photos: a, b, the d pair, and the RAW-only c. The d
    // pair proves RAW+JPEG merged into one (else it'd be five).
    assert_eq!(session.pool_len(), 4);
    assert!(!session.is_scanning());

    // RAW-only photos are hidden by default, so the RAW-only c doesn't show:
    // three cells, none of them RAW-only, and the count says how many are out.
    assert!(!session.raw_files_shown(), "hidden by default");
    assert_eq!(session.photo_count(), 3, "RAW-only photo is not displayed");
    assert_eq!(session.raw_hidden_count(), 1, "and the UI can say so");
    assert!(session.cell_info(3).is_none());
    let raw_only = (0..3)
        .filter(|&i| session.cell_info(i).is_some_and(|c| c.raw_only))
        .count();
    assert_eq!(raw_only, 0, "no RAW-only cell is shown");

    // The status tallies count only displayable photos, so unreviewed matches
    // the shown count — the hidden RAW-only photo never drifts them apart.
    let (acc, rej, unrev) = session.verdict_counts();
    assert_eq!((acc, rej), (0, 0));
    assert_eq!(
        unrev,
        session.photo_count(),
        "unrev tracks shown, not pool_len"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// A stand-in RAW that embeds a JPEG preview, the way every camera RAW does:
/// filler bytes standing in for the container, then a whole JPEG.
fn raw_with_preview(dir: &Path, name: &str, seed: u8) {
    let mut img = RgbImage::new(600, 400);
    for (x, y, px) in img.enumerate_pixels_mut() {
        // Noisy enough that the encoded JPEG clears the preview size threshold.
        let n = (x * 7 + y * 13) as u8 ^ seed;
        *px = Rgb([n, n.wrapping_mul(3), seed]);
    }
    let mut jpeg = Vec::new();
    img.write_to(
        &mut std::io::Cursor::new(&mut jpeg),
        image::ImageFormat::Jpeg,
    )
    .expect("encode jpeg");

    let mut data = vec![0x2A; 128];
    data.extend_from_slice(&jpeg);
    data.extend_from_slice(&[0u8; 256]);
    std::fs::write(dir.join(name), data).expect("write raw");
}

#[test]
fn a_shown_raw_only_photo_decodes_its_embedded_preview() {
    let dir = temp_folder("raw_preview");
    raw_with_preview(&dir, "shot.nef", 40);

    let mut session = Session::new();
    session.open_folder(dir.clone());
    drain_until(&mut session, 1);
    // A RAW-only folder shows its photos without any toggle: the auto default.
    assert!(session.raw_files_shown(), "all-RAW folder auto-shows");
    assert_eq!(session.photo_count(), 1);

    let id = session.photo_at(0).expect("the RAW-only photo").id;
    // A decoded thumbnail counts as imported, so an empty progress means this
    // one landed.
    pump_until(
        &mut session,
        |s| s.fill_base_background(),
        |s| s.import_progress().is_none(),
    );

    let thumb = session.thumb(id).expect("the embedded preview decodes");
    assert!(
        thumb.image.width > thumb.image.height,
        "the preview's own landscape aspect, not a placeholder"
    );
    assert_eq!(session.raw_no_preview_count(), 0);

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_raw_with_no_preview_is_asked_for_once() {
    let dir = temp_folder("raw_no_preview");
    // No JPEG anywhere in these bytes, so there is nothing to find — ever.
    std::fs::write(dir.join("blank.nef"), vec![0x5A; 200 * 1024]).unwrap();

    let mut session = Session::new();
    session.open_folder(dir.clone());
    drain_until(&mut session, 1);
    assert!(session.raw_files_shown(), "all-RAW folder auto-shows");
    assert_eq!(session.photo_count(), 1);

    let id = session.photo_at(0).expect("the RAW-only photo").id;
    pump_until(
        &mut session,
        |s| s.request_base(0),
        |s| s.raw_no_preview_count() > 0,
    );

    // Recorded as having no preview, so the grid paints a plate and the decoder
    // is never asked again — a retry would re-scan the file every frame.
    assert_eq!(session.raw_no_preview_count(), 1);
    assert!(session.thumb(id).is_none());
    session.request_base(0);
    assert!(!session.has_pending(), "no further decode was queued");

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn showing_raw_files_makes_raw_only_photos_full_citizens() {
    let dir = temp_folder("show_raw");
    write_jpeg(&dir, "a.jpg", 32, 32);
    touch(&dir, "c.raf"); // RAW-only
    write_jpeg(&dir, "d.jpg", 32, 32);
    touch(&dir, "d.RAF"); // pairs with d.jpg

    let mut session = Session::new();
    session.open_folder(dir.clone());
    drain_until(&mut session, 2);
    assert_eq!(session.photo_count(), 2);

    session.toggle_raw_files();

    assert!(session.raw_files_shown());
    assert_eq!(
        session.photo_count(),
        3,
        "the RAW-only photo joins the grid"
    );
    assert_eq!(
        session.displayable_count(),
        3,
        "and the denominator with it"
    );
    assert_eq!(session.raw_hidden_count(), 0, "nothing is hidden now");
    let raw_only = (0..3)
        .filter(|&i| session.cell_info(i).is_some_and(|c| c.raw_only))
        .count();
    assert_eq!(raw_only, 1, "the RAW-only cell is tagged as RAW");
    // The paired photo displays via its JPEG, so it is not a RAW-only cell.
    let (_, _, unrev) = session.verdict_counts();
    assert_eq!(unrev, 3, "tallies cover the shown set");

    // Cull it like any photo, then hide RAWs again: the verdict is owned state
    // and survives, while the tallies go back to describing what's on screen.
    let focus = (0..3)
        .find(|&i| session.cell_info(i).is_some_and(|c| c.raw_only))
        .expect("a RAW-only cell");
    session.set_focus(focus, false);
    session.reject();
    assert_eq!(session.verdict_counts().1, 1, "the RAW-only photo rejected");

    session.toggle_raw_files();
    assert_eq!(session.photo_count(), 2);
    assert_eq!(
        session.verdict_counts(),
        (0, 0, 2),
        "a hidden photo's verdict is not reported"
    );

    session.toggle_raw_files();
    assert_eq!(
        session.verdict_counts().1,
        1,
        "and comes back with it, never lost"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_read_only_project_refuses_the_raw_toggle() {
    let dir = temp_folder("raw_read_only");
    touch(&dir, "c.raf");

    // A live lock from a first session forces the second to open read-only.
    let mut first = Session::new();
    first.open_folder(dir.clone());
    let mut second = Session::new();
    second.open_folder(dir.clone());
    drain_until(&mut second, 1);
    assert!(second.is_read_only(), "the second session is a reader");
    assert!(second.raw_files_shown(), "all-RAW folder auto-shows");

    second.toggle_raw_files();

    assert!(second.raw_files_shown(), "a reader changes nothing");
    assert!(!second.is_dirty());

    drop(first);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn opening_a_new_folder_replaces_the_previous_pool() {
    let first = temp_folder("first");
    touch(&first, "x.jpg");
    touch(&first, "y.jpg");
    touch(&first, "z.jpg");

    let second = temp_folder("second");
    touch(&second, "only.jpg");

    let mut session = Session::new();
    session.open_folder(first.clone());
    drain_until(&mut session, 3);
    assert_eq!(session.photo_count(), 3);

    session.open_folder(second.clone());
    drain_until(&mut session, 1);
    assert_eq!(session.photo_count(), 1, "pool reset to the new folder");

    let _ = std::fs::remove_dir_all(&first);
    let _ = std::fs::remove_dir_all(&second);
}

#[test]
fn requesting_thumbnails_never_panics_and_skips_unloadable() {
    let dir = temp_folder("thumbs");
    touch(&dir, "a.jpg");

    let mut session = Session::new();
    session.open_folder(dir.clone());
    drain_until(&mut session, 1);

    // Placeholder bytes can't decode, but base + hi-res requests must be safe
    // no-ops and both must retire their in-flight entries.
    session.request_base(0);
    session.request_hires(0, 512);
    // Let the decode workers run and the terminal results drain the requests.
    for _ in 0..500 {
        session.tick();
        if !session.has_pending() {
            break;
        }
        sleep(Duration::from_millis(1));
    }
    assert_eq!(session.loaded_count(), 0);
    assert_eq!(session.hires_count(), 0);
    assert!(
        !session.has_pending(),
        "failed decodes still retire their requests"
    );

    // Dropping hi-res is always safe, even when empty.
    session.clear_hires();
    assert_eq!(session.hires_count(), 0);

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn base_thumbnails_decode_after_scan() {
    let dir = temp_folder("base");
    write_jpeg(&dir, "a.jpg", 400, 300);
    write_jpeg(&dir, "b.jpg", 300, 400);

    let mut session = Session::new();
    session.open_folder(dir.clone());
    drain_until(&mut session, 2);
    pump_until(
        &mut session,
        |s| {
            s.request_base(0);
            s.request_base(1);
        },
        |s| s.loaded_count() >= 2,
    );

    assert_eq!(session.loaded_count(), 2);
    let id = session.cell_info(0).unwrap().id;
    assert!(session.thumb(id).is_some());

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn background_fill_decodes_the_whole_folder_without_viewport_requests() {
    let dir = temp_folder("bgfill");
    for i in 0..5 {
        write_jpeg(&dir, &format!("p{i}.jpg"), 320, 240);
    }

    let mut session = Session::new();
    session.open_folder(dir.clone());
    drain_until(&mut session, 5);

    // Only the background fill drives decoding — no per-cell request_base.
    pump_until(
        &mut session,
        |s| s.fill_base_background(),
        |s| s.loaded_count() >= 5,
    );
    assert_eq!(
        session.loaded_count(),
        5,
        "every photo's base decodes in the background"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// Open a folder of `n` plain JPEGs (a.jpg, b.jpg, …) and drain the scan. The
/// pool needs no decode for cull tests, only pairing.
fn opened_with(n: usize, tag: &str) -> (Session, PathBuf) {
    let dir = temp_folder(tag);
    for i in 0..n {
        let name = format!("{}.jpg", (b'a' + i as u8) as char);
        write_jpeg(&dir, &name, 80, 80);
    }
    let mut session = Session::new();
    session.open_folder(dir.clone());
    drain_until(&mut session, n);
    (session, dir)
}

#[test]
fn accept_toggles_off_focus_and_undo_redo_round_trip() {
    let (mut session, dir) = opened_with(3, "cull_toggle");

    session.nav(1, 0, 3, false); // first arrow grabs the cursor at index 0
    let id0 = session.cell_info(0).unwrap().id;
    assert_eq!(session.verdict(id0), AcceptState::Unreviewed);

    session.accept();
    assert_eq!(session.verdict(id0), AcceptState::Accepted);
    assert!(session.is_selected(id0));
    assert!(session.can_undo());

    session.accept(); // focus is accepted → toggles back to unreviewed
    assert_eq!(session.verdict(id0), AcceptState::Unreviewed);

    assert!(session.undo()); // reverse the toggle-back
    assert_eq!(session.verdict(id0), AcceptState::Accepted);
    assert!(session.redo());
    assert_eq!(session.verdict(id0), AcceptState::Unreviewed);

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn groups_expose_filtered_spans_and_omit_empty_groups() {
    let (mut session, dir) = opened_with(3, "group_spans");

    // EXIF-less JPEGs fall back to their (identical) file time, so the default
    // time axis pools them into one date group spanning the whole visible order.
    let g = session.groups();
    assert_eq!(g.len(), 1);
    assert_eq!(g[0].kind, GroupKind::Time);
    assert_eq!((g[0].start, g[0].count, g[0].total), (0, 3, 3));

    // Accept one; under the Accepted filter the span reports 1 visible of 3.
    session.nav(1, 0, 3, false); // focus index 0
    session.accept();
    session.set_filter(VerdictFilter::Accepted);
    let g = session.groups();
    assert_eq!(g.len(), 1);
    assert_eq!((g[0].start, g[0].count, g[0].total), (0, 1, 3));

    // A filter no photo matches drops the group entirely — never an empty header.
    session.set_filter(VerdictFilter::Rejected);
    assert!(session.groups().is_empty());

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn axis_none_collapses_to_a_single_stream_group() {
    let (mut session, dir) = opened_with(3, "group_stream");
    session.set_axis(Axis::None);
    let g = session.groups();
    assert_eq!(g.len(), 1);
    assert_eq!(g[0].kind, GroupKind::Stream);
    assert_eq!((g[0].start, g[0].count, g[0].total), (0, 3, 3));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn group_cover_is_first_accepted_else_first_cell() {
    let (mut session, dir) = opened_with(3, "group_cover");

    // Nothing accepted yet → the cover is the group's first cell.
    let g0 = session.groups()[0].clone();
    assert_eq!(session.group_cover(&g0), 0);

    // Accept the second cell; it becomes the cover the collapsed group shows (#16).
    session.set_focus(1, false);
    session.accept();
    let g0 = session.groups()[0].clone();
    assert_eq!(session.group_cover(&g0), 1);

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn set_focus_clamps_and_extends_from_the_anchor() {
    let (mut session, dir) = opened_with(4, "set_focus");

    session.set_focus(99, false); // out of range → clamped to the last cell
    assert_eq!(session.focus(), Some(3));

    session.set_focus(1, false); // plain move drops the anchor on 1
    assert_eq!(session.focus(), Some(1));
    assert_eq!(session.selection_count(), 1);

    session.set_focus(3, true); // extend selects the anchor→focus run 1..=3
    assert_eq!(session.focus(), Some(3));
    assert_eq!(session.selection_count(), 3);

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn filter_routes_visible_and_undo_rebuilds_it() {
    let (mut session, dir) = opened_with(3, "cull_filter");

    session.nav(1, 0, 3, false); // focus index 0
    session.accept(); // one photo accepted, two unreviewed

    assert_eq!(session.pool_len(), 3, "pool size ignores the filter");
    session.set_filter(VerdictFilter::Accepted);
    assert_eq!(session.photo_count(), 1);
    session.set_filter(VerdictFilter::Unreviewed);
    assert_eq!(session.photo_count(), 2);

    // Undo un-accepts the photo; under the unreviewed view it reappears.
    assert!(session.undo());
    assert_eq!(session.photo_count(), 3, "undo rebuilds the visible order");

    session.set_filter(VerdictFilter::All);
    assert_eq!(session.photo_count(), 3);

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn select_all_then_reject_marks_every_visible_photo() {
    let (mut session, dir) = opened_with(3, "cull_reject_all");

    session.select_all_visible();
    assert_eq!(session.selection_count(), 3);

    session.reject(); // focus is unreviewed → rejects the whole selection
    for i in 0..3 {
        let id = session.cell_info(i).unwrap().id;
        assert_eq!(session.verdict(id), AcceptState::Rejected);
    }
    let (accepted, rejected, unreviewed) = session.verdict_counts();
    assert_eq!((accepted, rejected, unreviewed), (0, 3, 0));

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn opening_a_new_folder_clears_verdicts_and_selection() {
    let (mut session, first) = opened_with(2, "cull_reset_a");
    session.select_all_visible();
    session.accept();
    assert_eq!(session.verdict_counts().0, 2);

    let second = temp_folder("cull_reset_b");
    write_jpeg(&second, "z.jpg", 80, 80);
    session.open_folder(second.clone());
    drain_until(&mut session, 1);

    // Ids restart at 0 per folder; prior verdicts/selection must not bleed.
    assert_eq!(session.verdict_counts(), (0, 0, 1));
    assert_eq!(session.selection_count(), 0);
    assert_eq!(session.focus(), None);

    let _ = std::fs::remove_dir_all(&first);
    let _ = std::fs::remove_dir_all(&second);
}

#[test]
fn hi_res_upgrades_then_clears_back_to_base() {
    let dir = temp_folder("hires");
    write_jpeg(&dir, "big.jpg", 1000, 800);

    let mut session = Session::new();
    session.open_folder(dir.clone());
    drain_until(&mut session, 1);
    pump_until(
        &mut session,
        |s| s.request_base(0),
        |s| s.loaded_count() >= 1,
    );

    let id = session.cell_info(0).unwrap().id;
    let base_version = session.thumb(id).unwrap().version;

    pump_until(
        &mut session,
        |s| s.request_hires(0, 512),
        |s| s.hires_count() >= 1,
    );
    assert_eq!(session.hires_count(), 1);
    let view = session.thumb(id).unwrap();
    assert!(
        view.version != base_version,
        "hi-res is a newer version than base"
    );
    assert!(
        view.image.width.max(view.image.height) > 256,
        "hi-res is sharper than base"
    );

    // Zoom-out drops hi-res RAM; the base thumbnail still displays.
    session.clear_hires();
    assert_eq!(session.hires_count(), 0);
    let after = session.thumb(id).unwrap();
    assert_eq!(
        after.version, base_version,
        "falls back to the base thumbnail"
    );

    let _ = std::fs::remove_dir_all(&dir);
}
