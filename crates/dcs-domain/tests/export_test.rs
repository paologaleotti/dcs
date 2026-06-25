//! Exhaustive tests for the pure export planner (CLAUDE.md, spec §6.9): scope
//! emptiness, per-photo file selection with skip accounting, every layout,
//! collision cascades under both policies, and the rename template (including
//! its interaction with split layout). Zero mocks — the planner is pure.

use std::path::{Path, PathBuf};

use dcs_domain::export::{
    Collision, ExportError, ExportItem, ExportRequest, FileRole, FileSelection, Layout,
    NameTemplate, SkipReason, plan_export,
};
use dcs_domain::fingerprint::ContentFingerprint;
use dcs_domain::photo::{AssociatedFiles, Photo, PhotoId, PhotoType};
use time::PrimitiveDateTime;
use time::macros::datetime;

fn photo(id: u32, jpeg: Option<&str>, raw: Option<&str>) -> Photo {
    photo_at(id, jpeg, raw, None)
}

fn photo_at(
    id: u32,
    jpeg: Option<&str>,
    raw: Option<&str>,
    when: Option<PrimitiveDateTime>,
) -> Photo {
    let photo_type = match (jpeg.is_some(), raw.is_some()) {
        (true, true) => PhotoType::Both,
        (true, false) => PhotoType::Jpeg,
        (false, true) => PhotoType::Raw,
        (false, false) => panic!("a photo always has at least one file"),
    };
    Photo {
        id: PhotoId(id),
        files: AssociatedFiles {
            jpeg: jpeg.map(PathBuf::from),
            raw: raw.map(PathBuf::from),
        },
        photo_type,
        orientation: Default::default(),
        fingerprint: ContentFingerprint::from_bytes([id as u8; 32]),
        captured_at: when,
        captured_offset: None,
        meta: dcs_domain::photo::CaptureMeta::default(),
        missing: false,
    }
}

fn items<'a>(photos: &'a [Photo]) -> Vec<ExportItem<'a>> {
    photos
        .iter()
        .map(|p| ExportItem {
            photo: p,
            group_title: None,
            primary_tag: None,
            sidecars: &[],
            crop: None,
        })
        .collect()
}

fn request(files: FileSelection, layout: Layout, collision: Collision) -> ExportRequest {
    ExportRequest {
        dest: PathBuf::from("/out"),
        files,
        layout,
        collision,
        template: None,
        sidecars: false,
        include_uncropped_originals: false,
    }
}

fn dest(parts: &[&str]) -> PathBuf {
    let mut p = PathBuf::from("/out");
    for part in parts {
        p.push(part);
    }
    p
}

#[test]
fn empty_scope_is_an_error() {
    let req = request(FileSelection::Both, Layout::Together, Collision::Rename);
    assert_eq!(
        plan_export(&[], Path::new("/src"), &req),
        Err(ExportError::EmptyScope)
    );
}

#[test]
fn jpeg_only_skips_raw_only_photos_and_counts() {
    let photos = [
        photo(1, Some("/src/a.JPG"), Some("/src/a.RAF")),
        photo(2, None, Some("/src/b.RAF")), // raw-only → skipped under JPEG-only
    ];
    let req = request(FileSelection::Jpeg, Layout::Together, Collision::Rename);
    let plan = plan_export(&items(&photos), Path::new("/src"), &req).unwrap();

    assert_eq!(plan.ops.len(), 1);
    assert_eq!(plan.jpeg_count, 1);
    assert_eq!(plan.raw_count, 0);
    assert_eq!(plan.ops[0].role, FileRole::Jpeg);
    assert_eq!(plan.ops[0].dest, dest(&["a.JPG"]));
    assert_eq!(plan.skipped.len(), 1);
    assert_eq!(plan.skipped[0].id, PhotoId(2));
    assert_eq!(plan.skipped[0].reason, SkipReason::NoJpeg);
}

#[test]
fn raw_only_selection_skips_photos_without_a_raw() {
    let photos = [
        photo(1, Some("/src/a.JPG"), None), // jpeg-only → skipped under RAW-only
        photo(2, Some("/src/b.JPG"), Some("/src/b.RAF")),
    ];
    let req = request(FileSelection::Raw, Layout::Together, Collision::Rename);
    let plan = plan_export(&items(&photos), Path::new("/src"), &req).unwrap();

    assert_eq!(plan.raw_count, 1);
    assert_eq!(plan.jpeg_count, 0);
    assert_eq!(
        plan.skipped,
        vec![dcs_domain::export::SkippedPhoto {
            id: PhotoId(1),
            reason: SkipReason::NoRaw,
        }]
    );
}

#[test]
fn both_emits_jpeg_then_raw_for_each_photo() {
    let photos = [photo(1, Some("/src/a.JPG"), Some("/src/a.RAF"))];
    let req = request(FileSelection::Both, Layout::Together, Collision::Rename);
    let plan = plan_export(&items(&photos), Path::new("/src"), &req).unwrap();

    assert_eq!(plan.ops.len(), 2);
    assert_eq!(plan.ops[0].role, FileRole::Jpeg);
    assert_eq!(plan.ops[1].role, FileRole::Raw);
    assert_eq!(plan.jpeg_count, 1);
    assert_eq!(plan.raw_count, 1);
}

#[test]
fn both_requires_the_pair_and_skips_photos_missing_either() {
    let photos = [
        photo(1, Some("/src/a.JPG"), Some("/src/a.RAF")), // pair → both copied
        photo(2, Some("/src/b.JPG"), None),               // no raw → skipped
        photo(3, None, Some("/src/c.RAF")),               // no jpeg → skipped
    ];
    let req = request(FileSelection::Both, Layout::Together, Collision::Rename);
    let plan = plan_export(&items(&photos), Path::new("/src"), &req).unwrap();

    assert_eq!(plan.ops.len(), 2);
    assert_eq!(plan.jpeg_count, 1);
    assert_eq!(plan.raw_count, 1);
    assert_eq!(
        plan.skipped,
        vec![
            dcs_domain::export::SkippedPhoto {
                id: PhotoId(2),
                reason: SkipReason::NoRaw,
            },
            dcs_domain::export::SkippedPhoto {
                id: PhotoId(3),
                reason: SkipReason::NoJpeg,
            },
        ]
    );
}

#[test]
fn rename_cascades_on_basename_collisions() {
    // Three photos, same basename in different source folders, flattened together.
    let photos = [
        photo(1, Some("/src/x/a.JPG"), None),
        photo(2, Some("/src/y/a.JPG"), None),
        photo(3, Some("/src/z/a.JPG"), None),
    ];
    let req = request(FileSelection::Jpeg, Layout::Together, Collision::Rename);
    let plan = plan_export(&items(&photos), Path::new("/src"), &req).unwrap();

    assert_eq!(plan.ops[0].dest, dest(&["a.JPG"]));
    assert_eq!(plan.ops[1].dest, dest(&["a-1.JPG"]));
    assert_eq!(plan.ops[2].dest, dest(&["a-2.JPG"]));
    assert_eq!(plan.collisions, 2);
}

#[test]
fn case_only_collisions_are_renamed_not_overwritten() {
    // `a.JPG` and `a.jpg` are one filename on the default Windows (NTFS) and
    // macOS (APFS) filesystems; the planner must treat them as a collision so the
    // dumb executor never overwrites the first copy.
    let photos = [
        photo(1, Some("/src/x/a.JPG"), None),
        photo(2, Some("/src/y/a.jpg"), None),
    ];
    let req = request(FileSelection::Jpeg, Layout::Together, Collision::Rename);
    let plan = plan_export(&items(&photos), Path::new("/src"), &req).unwrap();

    assert_eq!(plan.ops.len(), 2);
    assert_eq!(plan.ops[0].dest, dest(&["a.JPG"]));
    assert_eq!(
        plan.ops[1].dest,
        dest(&["a-1.jpg"]),
        "the case-clash is renamed, not emitted as a second a.jpg"
    );
    assert_eq!(plan.collisions, 1);
}

#[test]
fn case_only_collisions_are_skipped_under_skip_policy() {
    let photos = [
        photo(1, Some("/src/x/IMG.JPG"), None),
        photo(2, Some("/src/y/img.jpg"), None),
    ];
    let req = request(FileSelection::Jpeg, Layout::Together, Collision::Skip);
    let plan = plan_export(&items(&photos), Path::new("/src"), &req).unwrap();

    assert_eq!(
        plan.ops.len(),
        1,
        "the case-clash is skipped, never overwritten"
    );
    assert_eq!(plan.ops[0].dest, dest(&["IMG.JPG"]));
    assert_eq!(plan.collisions, 1);
}

#[test]
fn skip_policy_drops_colliding_files() {
    let photos = [
        photo(1, Some("/src/x/a.JPG"), None),
        photo(2, Some("/src/y/a.JPG"), None),
    ];
    let req = request(FileSelection::Jpeg, Layout::Together, Collision::Skip);
    let plan = plan_export(&items(&photos), Path::new("/src"), &req).unwrap();

    assert_eq!(plan.ops.len(), 1, "the second collides and is dropped");
    assert_eq!(plan.ops[0].dest, dest(&["a.JPG"]));
    assert_eq!(plan.collisions, 1);
}

#[test]
fn split_layout_routes_jpeg_and_raw_to_subfolders() {
    let photos = [photo(1, Some("/src/a.JPG"), Some("/src/a.RAF"))];
    let req = request(FileSelection::Both, Layout::SplitJpegRaw, Collision::Rename);
    let plan = plan_export(&items(&photos), Path::new("/src"), &req).unwrap();

    assert_eq!(plan.ops[0].dest, dest(&["JPEG", "a.JPG"]));
    assert_eq!(plan.ops[1].dest, dest(&["RAW", "a.RAF"]));
}

#[test]
fn group_as_folders_sanitizes_the_group_title() {
    let p = photo(1, Some("/src/a.JPG"), None);
    // A real time-group title carries `/` (the date) and `·`, illegal in a path.
    let item = ExportItem {
        photo: &p,
        group_title: Some("Day 1 · 11/05/25"),
        primary_tag: None,
        sidecars: &[],
        crop: None,
    };
    let req = request(
        FileSelection::Jpeg,
        Layout::GroupAsFolders,
        Collision::Rename,
    );
    let plan = plan_export(&[item], Path::new("/src"), &req).unwrap();

    assert_eq!(plan.ops[0].dest, dest(&["Day 1 · 11-05-25", "a.JPG"]));
}

#[test]
fn group_as_folders_falls_back_when_ungrouped() {
    let photos = [photo(1, Some("/src/a.JPG"), None)];
    let req = request(
        FileSelection::Jpeg,
        Layout::GroupAsFolders,
        Collision::Rename,
    );
    let plan = plan_export(&items(&photos), Path::new("/src"), &req).unwrap();
    assert_eq!(plan.ops[0].dest, dest(&["Ungrouped", "a.JPG"]));
}

#[test]
fn mirror_source_recreates_the_subtree() {
    let photos = [photo(1, Some("/src/japan/day1/a.JPG"), None)];
    let req = request(FileSelection::Jpeg, Layout::MirrorSource, Collision::Rename);
    let plan = plan_export(&items(&photos), Path::new("/src"), &req).unwrap();
    assert_eq!(plan.ops[0].dest, dest(&["japan", "day1", "a.JPG"]));
}

#[test]
fn unknown_template_token_is_rejected() {
    let photos = [photo(1, Some("/src/a.JPG"), None)];
    let mut req = request(FileSelection::Jpeg, Layout::Together, Collision::Rename);
    req.template = Some(NameTemplate("{name}_{bogus}".to_string()));
    assert_eq!(
        plan_export(&items(&photos), Path::new("/src"), &req),
        Err(ExportError::BadTemplate("bogus".to_string()))
    );
}

#[test]
fn template_expands_with_split_layout_and_keeps_the_source_extension() {
    let photos = [photo_at(
        1,
        Some("/src/DSCF1.JPG"),
        Some("/src/DSCF1.RAF"),
        Some(datetime!(2025 - 05 - 11 14:30:05)),
    )];
    let mut req = request(FileSelection::Both, Layout::SplitJpegRaw, Collision::Rename);
    req.template = Some(NameTemplate("{date}_{seq}_{name}".to_string()));
    let plan = plan_export(&items(&photos), Path::new("/src"), &req).unwrap();

    // Template drives the stem; the extension still comes from each source file,
    // and the split layout still routes by role.
    assert_eq!(plan.ops[0].dest, dest(&["JPEG", "20250511_0001_DSCF1.JPG"]));
    assert_eq!(plan.ops[1].dest, dest(&["RAW", "20250511_0001_DSCF1.RAF"]));
}

#[test]
fn template_collisions_cascade_too() {
    // Two photos whose template output is identical → rename cascade.
    let photos = [
        photo(1, Some("/src/a.JPG"), None),
        photo(2, Some("/src/b.JPG"), None),
    ];
    let mut req = request(FileSelection::Jpeg, Layout::Together, Collision::Rename);
    req.template = Some(NameTemplate("shot".to_string()));
    let plan = plan_export(&items(&photos), Path::new("/src"), &req).unwrap();

    assert_eq!(plan.ops[0].dest, dest(&["shot.JPG"]));
    assert_eq!(plan.ops[1].dest, dest(&["shot-1.JPG"]));
    assert_eq!(plan.collisions, 1);
}

#[test]
fn nothing_to_copy_when_selection_matches_no_files() {
    // RAW-only selection over an all-JPEG scope: every photo skips → no ops.
    let photos = [
        photo(1, Some("/src/a.JPG"), None),
        photo(2, Some("/src/b.JPG"), None),
    ];
    let req = request(FileSelection::Raw, Layout::Together, Collision::Rename);
    assert_eq!(
        plan_export(&items(&photos), Path::new("/src"), &req),
        Err(ExportError::NothingToCopy)
    );
}

#[test]
fn any_copies_whatever_files_exist() {
    // Mixed scope: a pair, a jpeg-only, a raw-only. Any copies every file
    // present, skipping nothing (spec §6.3 "whatever files exist").
    let photos = [
        photo(1, Some("/src/a.JPG"), Some("/src/a.RAF")),
        photo(2, Some("/src/b.JPG"), None),
        photo(3, None, Some("/src/c.RAF")),
    ];
    let req = request(FileSelection::Any, Layout::Together, Collision::Rename);
    let plan = plan_export(&items(&photos), Path::new("/src"), &req).unwrap();

    assert_eq!(plan.ops.len(), 4);
    assert_eq!(plan.jpeg_count, 2);
    assert_eq!(plan.raw_count, 2);
    assert!(plan.skipped.is_empty());
}

#[test]
fn group_token_expands_and_falls_back_when_ungrouped() {
    let p = photo(1, Some("/src/a.JPG"), None);
    let item = ExportItem {
        photo: &p,
        group_title: Some("Day 1 · 11/05/25"),
        primary_tag: None,
        sidecars: &[],
        crop: None,
    };
    let mut req = request(FileSelection::Jpeg, Layout::Together, Collision::Rename);
    req.template = Some(NameTemplate("{group}_{name}".to_string()));
    let plan = plan_export(&[item], Path::new("/src"), &req).unwrap();
    // The whole expanded stem is sanitized, so the title's `/` becomes `-`.
    assert_eq!(plan.ops[0].dest, dest(&["Day 1 · 11-05-25_a.JPG"]));

    // Ungrouped → the "Ungrouped" fallback.
    let ungrouped = photo(2, Some("/src/b.JPG"), None);
    let plan = plan_export(&items(&[ungrouped]), Path::new("/src"), &req).unwrap();
    assert_eq!(plan.ops[0].dest, dest(&["Ungrouped_b.JPG"]));
}

#[test]
fn tag_token_expands_and_falls_back_when_untagged() {
    let p = photo(1, Some("/src/a.JPG"), None);
    let tagged = ExportItem {
        photo: &p,
        group_title: None,
        primary_tag: Some("temple"),
        sidecars: &[],
        crop: None,
    };
    let mut req = request(FileSelection::Jpeg, Layout::Together, Collision::Rename);
    req.template = Some(NameTemplate("{tag}_{name}".to_string()));
    let plan = plan_export(&[tagged], Path::new("/src"), &req).unwrap();
    assert_eq!(plan.ops[0].dest, dest(&["temple_a.JPG"]));

    // Untagged → the "untagged" fallback.
    let plan = plan_export(
        &items(&[photo(2, Some("/src/b.JPG"), None)]),
        Path::new("/src"),
        &req,
    )
    .unwrap();
    assert_eq!(plan.ops[0].dest, dest(&["untagged_b.JPG"]));
}

#[test]
fn sidecars_ride_into_the_primary_folder_when_opted_in() {
    let p = photo(1, Some("/src/a.JPG"), None);
    let xmp = PathBuf::from("/src/a.xmp");
    let item = ExportItem {
        photo: &p,
        group_title: None,
        primary_tag: None,
        sidecars: std::slice::from_ref(&xmp),
        crop: None,
    };
    let mut req = request(FileSelection::Jpeg, Layout::Together, Collision::Rename);
    req.sidecars = true;
    let plan = plan_export(&[item], Path::new("/src"), &req).unwrap();

    assert_eq!(plan.ops.len(), 2);
    assert_eq!(plan.ops[1].role, FileRole::Sidecar);
    assert_eq!(plan.ops[1].dest, dest(&["a.xmp"]));
    assert_eq!(plan.sidecar_count, 1);
    assert!(plan.summary.contains("1 sidecar"));
}

#[test]
fn sidecars_follow_the_template_rename_and_are_off_by_default() {
    let p = photo(1, Some("/src/a.JPG"), None);
    let xmp = PathBuf::from("/src/a.xmp");
    let item = ExportItem {
        photo: &p,
        group_title: None,
        primary_tag: None,
        sidecars: std::slice::from_ref(&xmp),
        crop: None,
    };
    // Off by default: the sidecar is ignored.
    let off = request(FileSelection::Jpeg, Layout::Together, Collision::Rename);
    assert_eq!(
        plan_export(&[item], Path::new("/src"), &off)
            .unwrap()
            .ops
            .len(),
        1
    );

    // On + a template: the sidecar takes the renamed stem so the link survives.
    let mut on = off.clone();
    on.sidecars = true;
    on.template = Some(NameTemplate("{seq}".to_string()));
    let plan = plan_export(&[item], Path::new("/src"), &on).unwrap();
    assert_eq!(plan.ops[0].dest, dest(&["0001.JPG"]));
    assert_eq!(plan.ops[1].dest, dest(&["0001.xmp"]));
}

#[test]
fn a_skipped_photo_leaves_no_orphan_sidecar() {
    // RAW-only selection: the JPEG-only photo is skipped, so its sidecar must not
    // ride along; the photo that does contribute a file carries its own.
    let kept = photo(1, Some("/src/a.JPG"), Some("/src/a.RAF"));
    let dropped = photo(2, Some("/src/b.JPG"), None);
    let a_xmp = PathBuf::from("/src/a.xmp");
    let b_xmp = PathBuf::from("/src/b.xmp");
    let items = vec![
        ExportItem {
            photo: &kept,
            group_title: None,
            primary_tag: None,
            sidecars: std::slice::from_ref(&a_xmp),
            crop: None,
        },
        ExportItem {
            photo: &dropped,
            group_title: None,
            primary_tag: None,
            sidecars: std::slice::from_ref(&b_xmp),
            crop: None,
        },
    ];
    let mut req = request(FileSelection::Raw, Layout::Together, Collision::Rename);
    req.sidecars = true;
    let plan = plan_export(&items, Path::new("/src"), &req).unwrap();

    let dests: Vec<&Path> = plan.ops.iter().map(|o| o.dest.as_path()).collect();
    assert!(dests.contains(&dest(&["a.RAF"]).as_path()));
    assert!(dests.contains(&dest(&["a.xmp"]).as_path()));
    assert!(
        !dests.contains(&dest(&["b.xmp"]).as_path()),
        "skipped photo's sidecar must not copy"
    );
}

#[test]
fn sidecar_names_cascade_on_collision() {
    let p1 = photo(1, Some("/src/x/a.JPG"), None);
    let p2 = photo(2, Some("/src/y/a.JPG"), None);
    let x1 = PathBuf::from("/src/x/a.xmp");
    let x2 = PathBuf::from("/src/y/a.xmp");
    let items = vec![
        ExportItem {
            photo: &p1,
            group_title: None,
            primary_tag: None,
            sidecars: std::slice::from_ref(&x1),
            crop: None,
        },
        ExportItem {
            photo: &p2,
            group_title: None,
            primary_tag: None,
            sidecars: std::slice::from_ref(&x2),
            crop: None,
        },
    ];
    let mut req = request(FileSelection::Jpeg, Layout::Together, Collision::Rename);
    req.sidecars = true;
    let plan = plan_export(&items, Path::new("/src"), &req).unwrap();

    let dests: Vec<&Path> = plan.ops.iter().map(|o| o.dest.as_path()).collect();
    assert!(dests.contains(&dest(&["a.xmp"]).as_path()));
    assert!(dests.contains(&dest(&["a-1.xmp"]).as_path()));
}

#[test]
fn sidecar_rides_into_the_split_jpeg_folder() {
    let p = photo(1, Some("/src/a.JPG"), Some("/src/a.RAF"));
    let xmp = PathBuf::from("/src/a.xmp");
    let item = ExportItem {
        photo: &p,
        group_title: None,
        primary_tag: None,
        sidecars: std::slice::from_ref(&xmp),
        crop: None,
    };
    let mut req = request(FileSelection::Both, Layout::SplitJpegRaw, Collision::Rename);
    req.sidecars = true;
    let plan = plan_export(&[item], Path::new("/src"), &req).unwrap();

    // The sidecar follows the first emitted role (JPEG) into JPEG/.
    let sidecar = plan
        .ops
        .iter()
        .find(|o| o.role == FileRole::Sidecar)
        .unwrap();
    assert_eq!(sidecar.dest, dest(&["JPEG", "a.xmp"]));
}

#[test]
fn date_and_time_tokens_fall_back_when_undated() {
    let photos = [photo(1, Some("/src/a.JPG"), None)]; // no captured_at
    let mut req = request(FileSelection::Jpeg, Layout::Together, Collision::Rename);
    req.template = Some(NameTemplate("{date}_{time}".to_string()));
    let plan = plan_export(&items(&photos), Path::new("/src"), &req).unwrap();
    assert_eq!(plan.ops[0].dest, dest(&["nodate_000000.JPG"]));
}

#[test]
fn template_that_sanitizes_to_empty_falls_back_to_untitled() {
    // A template that trims away to nothing (leading/trailing dots are stripped)
    // collapses to the `untitled` fallback rather than an empty filename.
    let photos = [photo(1, Some("/src/a.JPG"), None)];
    let mut req = request(FileSelection::Jpeg, Layout::Together, Collision::Rename);
    req.template = Some(NameTemplate("...".to_string()));
    let plan = plan_export(&items(&photos), Path::new("/src"), &req).unwrap();
    assert_eq!(plan.ops[0].dest, dest(&["untitled.JPG"]));
}

#[test]
fn mirror_source_falls_back_to_dest_root_for_files_outside_the_root() {
    // The source path isn't under `source_root`, so the subtree can't be
    // recreated — the file lands at the destination root instead of erroring.
    let photos = [photo(1, Some("/elsewhere/a.JPG"), None)];
    let req = request(FileSelection::Jpeg, Layout::MirrorSource, Collision::Rename);
    let plan = plan_export(&items(&photos), Path::new("/src"), &req).unwrap();
    assert_eq!(plan.ops[0].dest, dest(&["a.JPG"]));
}

#[test]
fn skip_policy_cascade_drops_every_later_collision() {
    let photos = [
        photo(1, Some("/src/x/a.JPG"), None),
        photo(2, Some("/src/y/a.JPG"), None),
        photo(3, Some("/src/z/a.JPG"), None),
    ];
    let req = request(FileSelection::Jpeg, Layout::Together, Collision::Skip);
    let plan = plan_export(&items(&photos), Path::new("/src"), &req).unwrap();
    assert_eq!(plan.ops.len(), 1);
    assert_eq!(plan.ops[0].dest, dest(&["a.JPG"]));
    assert_eq!(plan.collisions, 2);
}

#[test]
fn summary_reads_as_a_true_sentence() {
    let photos = [photo(1, Some("/src/a.JPG"), Some("/src/a.RAF"))];
    let req = request(FileSelection::Both, Layout::SplitJpegRaw, Collision::Rename);
    let plan = plan_export(&items(&photos), Path::new("/src"), &req).unwrap();
    assert_eq!(
        plan.summary,
        "Copy 2 files (1 JPEG + 1 RAW) into \"/out\", split JPEG/RAW, rename on collision."
    );
}

// --- Crop ops (RenderCrop + include-uncropped-originals) ---------------------

use dcs_domain::crops::{CropEdit, NormRect};
use dcs_domain::export::OpKind;

fn cropped_item<'a>(photo: &'a Photo, edit: CropEdit) -> ExportItem<'a> {
    ExportItem {
        photo,
        group_title: None,
        primary_tag: None,
        sidecars: &[],
        crop: Some(edit),
    }
}

fn an_edit() -> CropEdit {
    CropEdit {
        angle_deg: 2.0,
        rect: NormRect::centered(0.8, 0.8),
    }
}

#[test]
fn cropped_jpeg_becomes_a_render_op() {
    let p = photo(1, Some("/src/a.JPG"), None);
    let edit = an_edit();
    let req = request(FileSelection::Jpeg, Layout::Together, Collision::Rename);
    let plan = plan_export(&[cropped_item(&p, edit)], Path::new("/src"), &req).unwrap();
    assert_eq!(plan.ops.len(), 1);
    assert_eq!(
        plan.ops[0].kind,
        OpKind::RenderCrop {
            edit,
            orientation: Default::default()
        }
    );
    assert_eq!(plan.ops[0].dest, dest(&["a.JPG"]));
    assert_eq!(plan.ops[0].role, FileRole::Jpeg);
}

#[test]
fn cropped_pair_renders_jpeg_but_copies_raw() {
    let p = photo(1, Some("/src/a.JPG"), Some("/src/a.RAF"));
    let req = request(FileSelection::Both, Layout::Together, Collision::Rename);
    let plan = plan_export(&[cropped_item(&p, an_edit())], Path::new("/src"), &req).unwrap();
    assert_eq!(plan.ops.len(), 2);
    assert!(matches!(plan.ops[0].kind, OpKind::RenderCrop { .. }));
    assert_eq!(plan.ops[0].role, FileRole::Jpeg);
    assert_eq!(plan.ops[1].kind, OpKind::Copy);
    assert_eq!(plan.ops[1].role, FileRole::Raw);
}

#[test]
fn include_uncropped_originals_adds_a_plain_copy_into_originals() {
    let p = photo(1, Some("/src/a.JPG"), None);
    let mut req = request(FileSelection::Jpeg, Layout::Together, Collision::Rename);
    req.include_uncropped_originals = true;
    let plan = plan_export(&[cropped_item(&p, an_edit())], Path::new("/src"), &req).unwrap();
    assert_eq!(plan.ops.len(), 2);
    assert!(matches!(plan.ops[0].kind, OpKind::RenderCrop { .. }));
    assert_eq!(plan.ops[0].dest, dest(&["a.JPG"]));
    // The original copy lands untouched under originals/.
    assert_eq!(plan.ops[1].kind, OpKind::Copy);
    assert_eq!(plan.ops[1].dest, dest(&["originals", "a.JPG"]));
    assert_eq!(plan.ops[1].source, PathBuf::from("/src/a.JPG"));
}

#[test]
fn originals_flag_is_inert_without_a_crop() {
    let p = photo(1, Some("/src/a.JPG"), None);
    let mut req = request(FileSelection::Jpeg, Layout::Together, Collision::Rename);
    req.include_uncropped_originals = true;
    let plan = plan_export(&items(&[p]), Path::new("/src"), &req).unwrap();
    assert_eq!(plan.ops.len(), 1);
    assert_eq!(plan.ops[0].kind, OpKind::Copy);
}

#[test]
fn crop_summary_says_export_and_counts_cropped() {
    let p = photo(1, Some("/src/a.JPG"), None);
    let req = request(FileSelection::Jpeg, Layout::Together, Collision::Rename);
    let plan = plan_export(&[cropped_item(&p, an_edit())], Path::new("/src"), &req).unwrap();
    assert_eq!(
        plan.summary,
        "Export 1 file (1 JPEG + 0 RAW) into \"/out\", one folder, rename on collision (1 cropped)."
    );
}

#[test]
fn include_originals_collisions_cascade_independently_of_renders() {
    // Two cropped photos sharing a JPEG basename: the rendered dests collide and
    // cascade (-1), and the originals/ copies collide and cascade independently in
    // their own folder. One shared claim set, two folders.
    let p1 = photo(1, Some("/src/a.JPG"), None);
    let p2 = photo(2, Some("/src/sub/a.JPG"), None);
    let mut req = request(FileSelection::Jpeg, Layout::Together, Collision::Rename);
    req.include_uncropped_originals = true;
    let items = vec![cropped_item(&p1, an_edit()), cropped_item(&p2, an_edit())];
    let plan = plan_export(&items, Path::new("/src"), &req).unwrap();

    // 2 renders + 2 originals copies.
    assert_eq!(plan.ops.len(), 4);
    assert_eq!(plan.ops[0].dest, dest(&["a.JPG"]));
    assert_eq!(plan.ops[1].dest, dest(&["originals", "a.JPG"]));
    assert_eq!(plan.ops[2].dest, dest(&["a-1.JPG"]));
    assert_eq!(plan.ops[3].dest, dest(&["originals", "a-1.JPG"]));
    // The render is a RenderCrop, the original is a plain Copy.
    assert!(matches!(plan.ops[0].kind, OpKind::RenderCrop { .. }));
    assert_eq!(plan.ops[1].kind, OpKind::Copy);
    // Two renamed dests (a-1.JPG and originals/a-1.JPG).
    assert_eq!(plan.collisions, 2);
}

#[test]
fn include_originals_under_skip_drops_the_colliding_original() {
    // With Skip policy a pre-claimed originals/ name is dropped, not renamed.
    let p1 = photo(1, Some("/src/a.JPG"), None);
    let p2 = photo(2, Some("/src/sub/a.JPG"), None);
    let mut req = request(FileSelection::Jpeg, Layout::Together, Collision::Skip);
    req.include_uncropped_originals = true;
    let items = vec![cropped_item(&p1, an_edit()), cropped_item(&p2, an_edit())];
    let plan = plan_export(&items, Path::new("/src"), &req).unwrap();
    // photo1: render a.JPG + originals/a.JPG. photo2: both names taken → both
    // dropped, so only the two photo1 ops remain.
    assert_eq!(plan.ops.len(), 2);
    assert_eq!(plan.ops[0].dest, dest(&["a.JPG"]));
    assert_eq!(plan.ops[1].dest, dest(&["originals", "a.JPG"]));
}
