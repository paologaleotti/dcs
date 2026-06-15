use dcs_domain::photo::Orientation;
use dcs_io::imaging::decode_thumbnail;
use image::{Rgb, RgbImage};

fn write_jpeg(name: &str, w: u32, h: u32) -> std::path::PathBuf {
    let mut img = RgbImage::new(w, h);
    for (x, y, px) in img.enumerate_pixels_mut() {
        *px = Rgb([(x % 256) as u8, (y % 256) as u8, 128]);
    }
    let path = std::env::temp_dir().join(name);
    img.save(&path).expect("encode jpeg");
    path
}

#[test]
fn decodes_landscape_jpeg_to_contain_fit_thumbnail() {
    let path = write_jpeg("dcs_thumb_landscape.jpg", 1200, 800);
    let thumb = decode_thumbnail(&path, Orientation::Normal, 256).expect("decode");

    assert!(thumb.width <= 256 && thumb.height <= 256);
    assert!(thumb.width == 256 || thumb.height == 256, "must touch the box");
    assert_eq!(thumb.rgba.len() as u32, thumb.width * thumb.height * 4);

    let aspect = thumb.width as f32 / thumb.height as f32;
    assert!((aspect - 1.5).abs() < 0.1, "aspect preserved, got {aspect}");

    let _ = std::fs::remove_file(&path);
}

#[test]
fn orientation_rotate90_swaps_aspect() {
    let path = write_jpeg("dcs_thumb_rotate90.jpg", 1200, 800);
    let thumb = decode_thumbnail(&path, Orientation::Rotate90, 256).expect("decode");

    // Landscape source rotated 90° reads as portrait.
    assert!(thumb.height > thumb.width, "rotated to portrait");

    let _ = std::fs::remove_file(&path);
}

#[test]
fn portrait_source_fits_the_box_by_height() {
    let path = write_jpeg("dcs_thumb_portrait.jpg", 800, 1200);
    let thumb = decode_thumbnail(&path, Orientation::Normal, 256).expect("decode");

    assert_eq!(thumb.height, 256, "portrait touches the box by height");
    assert!(thumb.width < thumb.height);

    let _ = std::fs::remove_file(&path);
}

#[test]
fn edge_controls_output_resolution() {
    let path = write_jpeg("dcs_thumb_tiers.jpg", 2000, 1500);
    let small = decode_thumbnail(&path, Orientation::Normal, 256).expect("decode 256");
    let large = decode_thumbnail(&path, Orientation::Normal, 512).expect("decode 512");

    assert_eq!(small.width.max(small.height), 256);
    assert_eq!(large.width.max(large.height), 512);
    assert!(large.width > small.width, "higher tier = more pixels");

    let _ = std::fs::remove_file(&path);
}

#[test]
fn non_image_file_yields_none() {
    let path = std::env::temp_dir().join("dcs_thumb_garbage.jpg");
    std::fs::write(&path, b"this is not a jpeg").unwrap();
    assert!(decode_thumbnail(&path, Orientation::Normal, 256).is_none());
    let _ = std::fs::remove_file(&path);
}

#[test]
fn missing_file_yields_none() {
    let path = std::env::temp_dir().join("dcs_thumb_does_not_exist.jpg");
    let _ = std::fs::remove_file(&path);
    assert!(decode_thumbnail(&path, Orientation::Normal, 256).is_none());
}

#[test]
fn decodes_real_image_aspect_with_no_letterbox() {
    // A 2:1 JPEG must come back 2:1 — we decode the real image, never a
    // padded/letterboxed thumbnail, so there are no bars to trim.
    let path = write_jpeg("dcs_thumb_wide.jpg", 1000, 500);
    let thumb = decode_thumbnail(&path, Orientation::Normal, 256).expect("decode");
    let aspect = thumb.width as f32 / thumb.height as f32;
    assert!((aspect - 2.0).abs() < 0.05, "aspect preserved, got {aspect}");
    let _ = std::fs::remove_file(&path);
}
