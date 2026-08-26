use std::path::PathBuf;

use dcs_domain::photo::Orientation;
use dcs_io::imaging::{decode_thumbnail, embedded_previews};
use image::ExtendedColorType;
use image::codecs::jpeg::JpegEncoder;

/// A JPEG of pseudo-random pixels, so its encoded size scales with its
/// dimensions instead of collapsing to a few hundred bytes of flat color — the
/// extractor's size threshold is what separates a preview from a thumbnail.
fn noise_jpeg(w: u32, h: u32) -> Vec<u8> {
    let mut seed: u32 = w.wrapping_mul(2_654_435_761).wrapping_add(h);
    let mut pixels = Vec::with_capacity((w * h * 3) as usize);
    for _ in 0..(w * h * 3) {
        seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        pixels.push((seed >> 16) as u8);
    }
    let mut out = Vec::new();
    JpegEncoder::new_with_quality(&mut out, 92)
        .encode(&pixels, w, h, ExtendedColorType::Rgb8)
        .expect("encode jpeg");
    out
}

/// A JPEG of a smooth gradient, which compresses the way a real photo's EXIF
/// thumbnail does — a few KB, well under the preview threshold.
fn smooth_jpeg(w: u32, h: u32) -> Vec<u8> {
    let mut pixels = Vec::with_capacity((w * h * 3) as usize);
    for y in 0..h {
        for x in 0..w {
            pixels.extend_from_slice(&[(x * 255 / w) as u8, (y * 255 / h) as u8, 96]);
        }
    }
    let mut out = Vec::new();
    JpegEncoder::new_with_quality(&mut out, 85)
        .encode(&pixels, w, h, ExtendedColorType::Rgb8)
        .expect("encode jpeg");
    out
}

fn write_fixture(name: &str, bytes: &[u8]) -> PathBuf {
    let path = std::env::temp_dir().join(name);
    std::fs::write(&path, bytes).expect("write fixture");
    path
}

/// A TIFF-ish RAW: `lead` bytes of filler, then the given JPEGs back to back.
fn fake_raw(lead: usize, jpegs: &[&[u8]]) -> Vec<u8> {
    let mut data = vec![0x2A; lead];
    for jpeg in jpegs {
        data.extend_from_slice(jpeg);
    }
    data.extend_from_slice(&[0x00; 512]);
    data
}

#[test]
fn returns_the_largest_embedded_preview_first() {
    let small = noise_jpeg(400, 300);
    let large = noise_jpeg(1200, 900);
    assert!(small.len() >= 24 * 1024, "small must clear the threshold");
    // Smaller one first on disk, so ordering can't come out right by accident.
    let path = write_fixture("dcs_raw_two.nef", &fake_raw(128, &[&small, &large]));

    let found = embedded_previews(&path);

    assert_eq!(found.len(), 2);
    assert_eq!(found[0], large, "largest first");
    assert_eq!(found[1], small);

    let _ = std::fs::remove_file(&path);
}

#[test]
fn skips_the_tiny_exif_thumbnail() {
    let thumb = smooth_jpeg(160, 120);
    assert!(thumb.len() < 24 * 1024, "the thumbnail is below threshold");
    let path = write_fixture("dcs_raw_thumb_only.cr2", &fake_raw(64, &[&thumb]));

    assert!(
        embedded_previews(&path).is_empty(),
        "a 160x120 thumbnail is not a preview"
    );

    let _ = std::fs::remove_file(&path);
}

#[test]
fn a_file_with_no_embedded_jpeg_yields_nothing() {
    let path = write_fixture("dcs_raw_empty.arw", &vec![0x7F; 200 * 1024]);

    assert!(embedded_previews(&path).is_empty());

    let _ = std::fs::remove_file(&path);
}

#[test]
fn a_truncated_preview_is_rejected() {
    let jpeg = noise_jpeg(1000, 800);
    let half = &jpeg[..jpeg.len() / 2];
    let path = write_fixture("dcs_raw_truncated.nef", &fake_raw(64, &[half]));

    assert!(
        embedded_previews(&path).is_empty(),
        "half a JPEG is not a preview"
    );

    let _ = std::fs::remove_file(&path);
}

#[test]
fn a_thumbnail_nested_in_exif_does_not_split_its_parent() {
    // The real-world shape: a full-size preview whose own APP1 block carries a
    // small thumbnail. A naive marker scan would end the parent at the child's
    // EOI and hand the decoder a truncated image.
    let parent = noise_jpeg(1200, 900);
    let child = smooth_jpeg(160, 120);
    assert!(child.len() < 65_534, "APP1 payload must fit its u16 length");

    let mut nested = Vec::new();
    nested.extend_from_slice(&parent[..2]);
    nested.extend_from_slice(&[0xFF, 0xE1]);
    nested.extend_from_slice(&((child.len() + 2) as u16).to_be_bytes());
    nested.extend_from_slice(&child);
    nested.extend_from_slice(&parent[2..]);

    let path = write_fixture("dcs_raw_nested.orf", &fake_raw(64, &[&nested]));

    let found = embedded_previews(&path);

    assert_eq!(
        found.len(),
        1,
        "the nested thumbnail is not its own preview"
    );
    assert_eq!(found[0].len(), nested.len(), "the parent is whole");

    let _ = std::fs::remove_file(&path);
}

#[test]
fn finds_a_preview_past_the_first_read_stage() {
    let jpeg = noise_jpeg(900, 700);
    // Beyond the 1 MiB first stage, so only the escalating read reaches it.
    let path = write_fixture("dcs_raw_deep.dng", &fake_raw(2 * 1024 * 1024, &[&jpeg]));

    let found = embedded_previews(&path);

    assert_eq!(found.len(), 1);
    assert_eq!(found[0], jpeg);

    let _ = std::fs::remove_file(&path);
}

#[test]
fn raf_header_locates_the_preview_exactly() {
    let jpeg = noise_jpeg(1000, 750);
    let offset: u32 = 148;
    let mut data = Vec::new();
    data.extend_from_slice(b"FUJIFILMCCD-RAW ");
    data.resize(84, 0x00);
    data.extend_from_slice(&offset.to_be_bytes());
    data.extend_from_slice(&(jpeg.len() as u32).to_be_bytes());
    data.resize(offset as usize, 0x00);
    data.extend_from_slice(&jpeg);
    data.extend_from_slice(&[0x11; 4096]);
    let path = write_fixture("dcs_raw_fuji.raf", &data);

    let found = embedded_previews(&path);

    assert_eq!(found.len(), 1, "the header states one preview");
    assert_eq!(found[0], jpeg);

    let _ = std::fs::remove_file(&path);
}

#[test]
fn decode_thumbnail_reads_a_raw_through_its_embedded_preview() {
    let jpeg = noise_jpeg(1200, 800);
    let path = write_fixture("dcs_raw_decode.nef", &fake_raw(96, &[&jpeg]));

    let thumb = decode_thumbnail(&path, Orientation::Normal, 256, None).expect("decode preview");

    assert!(thumb.width <= 256 && thumb.height <= 256);
    assert!(thumb.width == 256 || thumb.height == 256, "touches the box");
    let aspect = thumb.width as f32 / thumb.height as f32;
    assert!((aspect - 1.5).abs() < 0.1, "aspect preserved, got {aspect}");
    assert_eq!(
        thumb.source_width, 1200,
        "source dims come from the preview"
    );
    assert_eq!(thumb.source_height, 800);

    let _ = std::fs::remove_file(&path);
}

#[test]
fn decode_thumbnail_declines_a_raw_with_no_preview() {
    let path = write_fixture("dcs_raw_no_preview.cr3", &vec![0x5A; 300 * 1024]);

    assert!(decode_thumbnail(&path, Orientation::Normal, 256, None).is_none());

    let _ = std::fs::remove_file(&path);
}
