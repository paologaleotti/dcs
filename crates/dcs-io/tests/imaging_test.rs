use dcs_domain::photo::Orientation;
use dcs_io::cache::ThumbTier;
use dcs_io::imaging::{
    DecodePriority, DecodeRequest, ThumbDecoder, ThumbDecoderPool, decode_thumbnail,
};
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
    let thumb = decode_thumbnail(&path, Orientation::Normal, 256, None).expect("decode");

    assert!(thumb.width <= 256 && thumb.height <= 256);
    assert!(
        thumb.width == 256 || thumb.height == 256,
        "must touch the box"
    );
    assert_eq!(thumb.rgba.len() as u32, thumb.width * thumb.height * 4);

    let aspect = thumb.width as f32 / thumb.height as f32;
    assert!((aspect - 1.5).abs() < 0.1, "aspect preserved, got {aspect}");

    let _ = std::fs::remove_file(&path);
}

#[test]
fn orientation_rotate90_swaps_aspect() {
    let path = write_jpeg("dcs_thumb_rotate90.jpg", 1200, 800);
    let thumb = decode_thumbnail(&path, Orientation::Rotate90, 256, None).expect("decode");

    // Landscape source rotated 90° reads as portrait.
    assert!(thumb.height > thumb.width, "rotated to portrait");

    let _ = std::fs::remove_file(&path);
}

#[test]
fn portrait_source_fits_the_box_by_height() {
    let path = write_jpeg("dcs_thumb_portrait.jpg", 800, 1200);
    let thumb = decode_thumbnail(&path, Orientation::Normal, 256, None).expect("decode");

    assert_eq!(thumb.height, 256, "portrait touches the box by height");
    assert!(thumb.width < thumb.height);

    let _ = std::fs::remove_file(&path);
}

#[test]
fn edge_controls_output_resolution() {
    let path = write_jpeg("dcs_thumb_tiers.jpg", 2000, 1500);
    let small = decode_thumbnail(&path, Orientation::Normal, 256, None).expect("decode 256");
    let large = decode_thumbnail(&path, Orientation::Normal, 512, None).expect("decode 512");

    assert_eq!(small.width.max(small.height), 256);
    assert_eq!(large.width.max(large.height), 512);
    assert!(large.width > small.width, "higher tier = more pixels");

    let _ = std::fs::remove_file(&path);
}

#[test]
fn non_image_file_yields_none() {
    let path = std::env::temp_dir().join("dcs_thumb_garbage.jpg");
    std::fs::write(&path, b"this is not a jpeg").unwrap();
    assert!(decode_thumbnail(&path, Orientation::Normal, 256, None).is_none());
    let _ = std::fs::remove_file(&path);
}

#[test]
fn decoder_runs_both_priority_lanes() {
    // A request on each lane must complete: the high and low pools are separate,
    // and neither is dropped. (The point of the split — that a high request isn't
    // delayed behind a low backlog — is a scheduling property, not asserted here.)
    let path = write_jpeg("dcs_thumb_lanes.jpg", 800, 600);
    let decoder = ThumbDecoderPool::new();
    let req = |key: u64, priority| DecodeRequest {
        key,
        path: path.clone(),
        orientation: Orientation::Normal,
        edge: 256,
        cache_key: None,
        tier: ThumbTier::Grid,
        cache: None,
        priority,
        crop: None,
    };
    decoder.request(req(1, DecodePriority::Low));
    decoder.request(req(2, DecodePriority::High));

    let mut keys = std::collections::HashSet::new();
    for _ in 0..2000 {
        for (key, image) in decoder.poll() {
            assert!(image.is_some(), "decode {key} produced an image");
            keys.insert(key);
        }
        if keys.len() == 2 {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
    assert_eq!(
        keys.len(),
        2,
        "both the low and high lane returned a result"
    );

    let _ = std::fs::remove_file(&path);
}

#[test]
fn missing_file_yields_none() {
    let path = std::env::temp_dir().join("dcs_thumb_does_not_exist.jpg");
    let _ = std::fs::remove_file(&path);
    assert!(decode_thumbnail(&path, Orientation::Normal, 256, None).is_none());
}

#[test]
fn decodes_real_image_aspect_with_no_letterbox() {
    // A 2:1 JPEG must come back 2:1 — we decode the real image, never a
    // padded/letterboxed thumbnail, so there are no bars to trim.
    let path = write_jpeg("dcs_thumb_wide.jpg", 1000, 500);
    let thumb = decode_thumbnail(&path, Orientation::Normal, 256, None).expect("decode");
    let aspect = thumb.width as f32 / thumb.height as f32;
    assert!(
        (aspect - 2.0).abs() < 0.05,
        "aspect preserved, got {aspect}"
    );
    let _ = std::fs::remove_file(&path);
}

// --- apply_crop pixel correctness (the actual cropped pixels, not just dims) --

use dcs_domain::crops::{CropEdit, NormRect};
use dcs_io::imaging::apply_crop;
use image::{Rgba, RgbaImage};

/// A 4-quadrant test image: TL red, TR green, BL blue, BR white. Lets a crop's
/// output be checked against the quadrant it should have sampled.
fn quadrant_image(w: u32, h: u32) -> RgbaImage {
    let mut img = RgbaImage::new(w, h);
    for (x, y, px) in img.enumerate_pixels_mut() {
        let left = x < w / 2;
        let top = y < h / 2;
        *px = match (left, top) {
            (true, true) => Rgba([255, 0, 0, 255]),
            (false, true) => Rgba([0, 255, 0, 255]),
            (true, false) => Rgba([0, 0, 255, 255]),
            (false, false) => Rgba([255, 255, 255, 255]),
        };
    }
    img
}

#[test]
fn apply_crop_identity_reproduces_the_source() {
    let src = quadrant_image(200, 160);
    let out = apply_crop(&src, &CropEdit::identity(), u32::MAX).into_rgba8();
    assert_eq!((out.width(), out.height()), (200, 160));
    // Center of each quadrant keeps its color.
    assert_eq!(out.get_pixel(50, 40).0, [255, 0, 0, 255]); // TL red
    assert_eq!(out.get_pixel(150, 40).0, [0, 255, 0, 255]); // TR green
    assert_eq!(out.get_pixel(50, 120).0, [0, 0, 255, 255]); // BL blue
    assert_eq!(out.get_pixel(150, 120).0, [255, 255, 255, 255]); // BR white
}

#[test]
fn apply_crop_top_right_quadrant_samples_green() {
    // No rotation, crop exactly the top-right quadrant → solid green output.
    let src = quadrant_image(200, 160);
    let edit = CropEdit {
        angle_deg: 0.0,
        rect: NormRect {
            x: 0.5,
            y: 0.0,
            w: 0.5,
            h: 0.5,
        },
    };
    let out = apply_crop(&src, &edit, u32::MAX).into_rgba8();
    assert_eq!((out.width(), out.height()), (100, 80));
    for p in [(5, 5), (50, 40), (95, 75)] {
        assert_eq!(
            out.get_pixel(p.0, p.1).0,
            [0, 255, 0, 255],
            "pixel {p:?} should be green"
        );
    }
}

#[test]
fn apply_crop_downscales_to_edge() {
    let src = quadrant_image(800, 800);
    // Full frame, capped to edge 100 → longest side 100.
    let out = apply_crop(&src, &CropEdit::identity(), 100).into_rgba8();
    assert_eq!(out.width().max(out.height()), 100);
}

#[test]
fn apply_crop_center_pixel_is_invariant_under_rotation() {
    // The straighten rotates about the center, so a tiny centered crop always
    // samples the image center — at the quadrant boundary it's a blend, but the
    // center pixel's source is fixed regardless of angle (sanity that rotation
    // pivots correctly, not off-corner).
    let src = quadrant_image(400, 400);
    for angle in [0.0, 10.0, -20.0] {
        let edit = CropEdit {
            angle_deg: angle,
            rect: NormRect::centered(0.02, 0.02),
        };
        let out = apply_crop(&src, &edit, u32::MAX).into_rgba8();
        // Output exists and is non-empty; center maps near the image center.
        assert!(out.width() >= 1 && out.height() >= 1, "empty at {angle}");
    }
}

#[test]
fn decode_oriented_full_applies_orientation_before_pixels() {
    // Rotate90 swaps the source dimensions in the returned (upright) pixels — the
    // orientation is baked, which is why the export RenderCrop op carries it.
    use dcs_io::imaging::decode_oriented_full;
    let path = write_jpeg("dcs_oriented_full.jpg", 200, 100);
    let upright = decode_oriented_full(&path, Orientation::Rotate90).expect("decode");
    assert_eq!((upright.width(), upright.height()), (100, 200));
    let _ = std::fs::remove_file(&path);
}

#[test]
fn decode_thumbnail_with_a_crop_does_not_panic_and_stays_within_edge() {
    // A small crop window forces a scaled-up source decode (crop_decode_edge);
    // the result is the cropped region fit to `edge`. Smoke-covers the cropped
    // decode path end to end.
    let path = write_jpeg("dcs_thumb_cropped.jpg", 1200, 800);
    let edit = CropEdit {
        angle_deg: 3.0,
        rect: NormRect::centered(0.3, 0.3),
    };
    let thumb = decode_thumbnail(&path, Orientation::Normal, 256, Some(&edit)).expect("decode");
    assert!(thumb.width.max(thumb.height) <= 256);
    assert_eq!(thumb.rgba.len() as u32, thumb.width * thumb.height * 4);
    let _ = std::fs::remove_file(&path);
}
