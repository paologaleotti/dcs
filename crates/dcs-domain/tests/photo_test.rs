use std::path::{Path, PathBuf};

use dcs_domain::fingerprint::ContentFingerprint;
use dcs_domain::photo::{AssociatedFiles, Orientation, Photo, PhotoId, PhotoType};

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
