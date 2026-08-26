//! Shared JPEG and EXIF fixture builders for the dcs-io integration tests.
//! Each test binary includes this file via `#[path = "support/jpeg.rs"]`; the
//! subdirectory keeps cargo from compiling it as a test binary of its own.

use std::io::Cursor;

/// Encode a small gradient JPEG in memory.
pub fn encode_test_jpeg(w: u32, h: u32) -> Vec<u8> {
    let mut img = image::RgbImage::new(w, h);
    for (x, y, px) in img.enumerate_pixels_mut() {
        *px = image::Rgb([(x % 256) as u8, (y % 256) as u8, 96]);
    }
    let mut buf = Vec::new();
    img.write_to(&mut Cursor::new(&mut buf), image::ImageFormat::Jpeg)
        .unwrap();
    buf
}

/// A whole JPEG segment: marker, big-endian length, payload.
pub fn segment(marker: u8, payload: &[u8]) -> Vec<u8> {
    let mut s = vec![0xFF, marker];
    s.extend_from_slice(&((payload.len() + 2) as u16).to_be_bytes());
    s.extend_from_slice(payload);
    s
}

/// Insert segments right after the SOI of `jpeg`.
pub fn with_segments(jpeg: &[u8], segments: &[Vec<u8>]) -> Vec<u8> {
    let mut out = jpeg[..2].to_vec();
    for s in segments {
        out.extend_from_slice(s);
    }
    out.extend_from_slice(&jpeg[2..]);
    out
}

/// One 12-byte TIFF IFD entry.
pub fn tiff_entry(le: bool, tag: u16, typ: u16, count: u32, value: [u8; 4]) -> Vec<u8> {
    let mut e = Vec::with_capacity(12);
    let u16b = |v: u16| if le { v.to_le_bytes() } else { v.to_be_bytes() };
    let u32b = |v: u32| if le { v.to_le_bytes() } else { v.to_be_bytes() };
    e.extend_from_slice(&u16b(tag));
    e.extend_from_slice(&u16b(typ));
    e.extend_from_slice(&u32b(count));
    e.extend_from_slice(&value);
    e
}

/// An inline SHORT value: left-justified in the 4-byte value field.
pub fn short_value(le: bool, v: u16) -> [u8; 4] {
    let b = if le { v.to_le_bytes() } else { v.to_be_bytes() };
    [b[0], b[1], 0, 0]
}

/// An inline LONG value.
pub fn long_value(le: bool, v: u32) -> [u8; 4] {
    if le { v.to_le_bytes() } else { v.to_be_bytes() }
}

/// Wrap a TIFF body into an EXIF APP1 segment.
pub fn exif_segment(tiff: &[u8]) -> Vec<u8> {
    let mut payload = b"Exif\0\0".to_vec();
    payload.extend_from_slice(tiff);
    segment(0xE1, &payload)
}

/// An EXIF APP1 segment with an orientation tag in IFD0 and pixel-dimension
/// tags in the EXIF sub-IFD. `PixelXDimension` is a SHORT and
/// `PixelYDimension` is a LONG, so both inline widths get exercised.
pub fn exif_app1(le: bool, orientation: u16, w: u32, h: u32) -> Vec<u8> {
    let u16b = |v: u16| if le { v.to_le_bytes() } else { v.to_be_bytes() };
    let u32b = |v: u32| if le { v.to_le_bytes() } else { v.to_be_bytes() };
    let mut t = Vec::new();
    t.extend_from_slice(if le { b"II" } else { b"MM" });
    t.extend_from_slice(&u16b(42));
    t.extend_from_slice(&u32b(8));
    // IFD0 at offset 8: 2 entries + next pointer = 30 bytes, so the sub-IFD
    // lands at offset 38.
    t.extend_from_slice(&u16b(2));
    t.extend_from_slice(&tiff_entry(le, 0x0112, 3, 1, short_value(le, orientation)));
    t.extend_from_slice(&tiff_entry(le, 0x8769, 4, 1, long_value(le, 38)));
    t.extend_from_slice(&u32b(0));
    t.extend_from_slice(&u16b(2));
    t.extend_from_slice(&tiff_entry(le, 0xA002, 3, 1, short_value(le, w as u16)));
    t.extend_from_slice(&tiff_entry(le, 0xA003, 4, 1, long_value(le, h)));
    t.extend_from_slice(&u32b(0));
    exif_segment(&t)
}

/// Read the EXIF block back out of a JPEG buffer.
pub fn read_exif(jpeg: &[u8]) -> exif::Exif {
    exif::Reader::new()
        .read_from_container(&mut Cursor::new(jpeg))
        .expect("output carries readable exif")
}

/// One unsigned tag value from the primary IFD.
pub fn uint_field(exif: &exif::Exif, tag: exif::Tag) -> u32 {
    exif.get_field(tag, exif::In::PRIMARY)
        .and_then(|f| f.value.get_uint(0))
        .unwrap_or_else(|| panic!("missing {tag}"))
}
