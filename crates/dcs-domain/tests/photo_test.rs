use std::path::{Path, PathBuf};

use dcs_domain::fingerprint::ContentFingerprint;
use dcs_domain::photo::{AssociatedFiles, CaptureMeta, Orientation, Photo, PhotoId, PhotoType};

fn photo(jpeg: Option<&str>, raw: Option<&str>, photo_type: PhotoType) -> Photo {
    Photo {
        id: PhotoId(0),
        files: AssociatedFiles {
            jpeg: jpeg.map(PathBuf::from),
            raw: raw.map(PathBuf::from),
        },
        photo_type,
        orientation: Orientation::Normal,
        fingerprint: ContentFingerprint::from_bytes([0u8; 32]),
        captured_at: None,
        meta: CaptureMeta::default(),
        missing: false,
    }
}

#[test]
fn from_exif_maps_all_eight_orientations() {
    assert_eq!(Orientation::from_exif(1), Orientation::Normal);
    assert_eq!(Orientation::from_exif(2), Orientation::FlipH);
    assert_eq!(Orientation::from_exif(3), Orientation::Rotate180);
    assert_eq!(Orientation::from_exif(4), Orientation::FlipV);
    assert_eq!(Orientation::from_exif(5), Orientation::Transpose);
    assert_eq!(Orientation::from_exif(6), Orientation::Rotate90);
    assert_eq!(Orientation::from_exif(7), Orientation::Transverse);
    assert_eq!(Orientation::from_exif(8), Orientation::Rotate270);
}

#[test]
fn from_exif_out_of_range_falls_back_to_normal() {
    assert_eq!(Orientation::from_exif(0), Orientation::Normal);
    assert_eq!(Orientation::from_exif(9), Orientation::Normal);
    assert_eq!(Orientation::from_exif(255), Orientation::Normal);
}

#[test]
fn display_path_prefers_jpeg_over_raw() {
    let both = photo(Some("a/x.JPG"), Some("a/x.RAF"), PhotoType::Both);
    assert_eq!(both.display_path(), Path::new("a/x.JPG"));
    assert_eq!(both.decodable_path(), Some(Path::new("a/x.JPG")));
}

#[test]
fn raw_only_has_no_decodable_path() {
    let raw = photo(None, Some("a/x.RAF"), PhotoType::Raw);
    assert!(raw.is_raw_only());
    assert_eq!(raw.decodable_path(), None);
    assert_eq!(raw.display_path(), Path::new("a/x.RAF"));
}

#[test]
fn file_name_is_basename_of_displayed_file() {
    let p = photo(Some("trip/day1/DSCF1234.JPG"), None, PhotoType::Jpeg);
    assert_eq!(p.file_name(), "DSCF1234.JPG");
    assert!(!p.is_raw_only());
}

#[test]
fn aperture_label_drops_trailing_zero() {
    use dcs_domain::photo::format_aperture;
    assert_eq!(format_aperture(2.8), "f/2.8");
    assert_eq!(format_aperture(8.0), "f/8");
    assert_eq!(format_aperture(1.4), "f/1.4");
}

#[test]
fn shutter_label_reads_like_a_photographer() {
    use dcs_domain::photo::format_shutter;
    assert_eq!(format_shutter(0.004), "1/250"); // 1/250 s
    assert_eq!(format_shutter(0.5), "1/2");
    assert_eq!(format_shutter(1.0), "1\"");
    assert_eq!(format_shutter(2.0), "2\"");
    assert_eq!(format_shutter(1.3), "1.3s");
    assert_eq!(format_shutter(0.0), "0s");
}

#[test]
fn focal_label_drops_trailing_zero() {
    use dcs_domain::photo::format_focal;
    assert_eq!(format_focal(35.0), "35mm");
    assert_eq!(format_focal(16.5), "16.5mm");
}

#[test]
fn exposure_line_joins_present_fields_and_omits_missing() {
    use dcs_domain::photo::CaptureMeta;
    let full = CaptureMeta {
        camera: Some("FUJIFILM X-T5".into()),
        lens: Some("XF35mmF1.4".into()),
        focal_mm: Some(35.0),
        aperture: Some(1.4),
        exposure_secs: Some(0.004),
        iso: Some(400),
    };
    assert_eq!(
        full.exposure_line().as_deref(),
        Some("35mm · f/1.4 · 1/250 · ISO 400")
    );

    let partial = CaptureMeta {
        aperture: Some(2.8),
        iso: Some(100),
        ..CaptureMeta::default()
    };
    assert_eq!(partial.exposure_line().as_deref(), Some("f/2.8 · ISO 100"));

    assert_eq!(CaptureMeta::default().exposure_line(), None);
}
