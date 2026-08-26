//! Tests for the JPEG metadata transplant used by the crop-render export path.

use dcs_io::jpeg_meta::transplant_metadata;

#[path = "support/jpeg.rs"]
#[allow(dead_code)]
mod support;

use support::{
    encode_test_jpeg, exif_app1, exif_segment, long_value, read_exif, segment, short_value,
    tiff_entry, uint_field, with_segments,
};

#[test]
fn transplants_exif_and_patches_orientation_and_size() {
    for le in [true, false] {
        let source = with_segments(&encode_test_jpeg(800, 600), &[exif_app1(le, 6, 800, 600)]);
        let rendered = encode_test_jpeg(400, 300);

        let out = transplant_metadata(&source, &rendered, 400, 300).expect("transplant succeeds");

        // Still a decodable JPEG with the rendered pixels.
        let img = image::load_from_memory(&out).expect("output decodes");
        assert_eq!((img.width(), img.height()), (400, 300));

        let meta = read_exif(&out);
        assert_eq!(uint_field(&meta, exif::Tag::Orientation), 1, "le={le}");
        assert_eq!(uint_field(&meta, exif::Tag::PixelXDimension), 400);
        assert_eq!(uint_field(&meta, exif::Tag::PixelYDimension), 300);
    }
}

#[test]
fn keeps_xmp_icc_and_comment_and_drops_mpf() {
    let xmp = {
        let mut p = b"http://ns.adobe.com/xap/1.0/\0".to_vec();
        p.extend_from_slice(b"<x:xmpmeta>creator</x:xmpmeta>");
        segment(0xE1, &p)
    };
    let icc = {
        let mut p = b"ICC_PROFILE\0\x01\x01".to_vec();
        p.extend_from_slice(b"fake-icc-payload");
        segment(0xE2, &p)
    };
    let mpf = {
        let mut p = b"MPF\0".to_vec();
        p.extend_from_slice(b"stale-offsets");
        segment(0xE2, &p)
    };
    let com = segment(0xFE, b"shot on a rainy day");
    let source = with_segments(
        &encode_test_jpeg(64, 64),
        &[xmp.clone(), icc.clone(), mpf, com.clone()],
    );
    let rendered = encode_test_jpeg(32, 32);

    let out = transplant_metadata(&source, &rendered, 32, 32).expect("transplant succeeds");

    let contains = |needle: &[u8]| out.windows(needle.len()).any(|w| w == needle);
    assert!(contains(b"<x:xmpmeta>creator</x:xmpmeta>"), "xmp kept");
    assert!(contains(b"fake-icc-payload"), "icc kept");
    assert!(contains(b"shot on a rainy day"), "comment kept");
    assert!(!contains(b"stale-offsets"), "mpf dropped");
    image::load_from_memory(&out).expect("output decodes");
}

#[test]
fn patches_xmp_orientation_in_both_forms() {
    let xmp = {
        let mut p = b"http://ns.adobe.com/xap/1.0/\0".to_vec();
        p.extend_from_slice(b"<rdf:Description tiff:Orientation=\"3\">");
        p.extend_from_slice(b"<tiff:Orientation>6</tiff:Orientation>");
        p.extend_from_slice(b"</rdf:Description>");
        segment(0xE1, &p)
    };
    let source = with_segments(&encode_test_jpeg(64, 64), &[xmp]);
    let rendered = encode_test_jpeg(32, 32);

    let out = transplant_metadata(&source, &rendered, 32, 32).expect("transplant succeeds");

    let contains = |needle: &[u8]| out.windows(needle.len()).any(|w| w == needle);
    assert!(contains(b"tiff:Orientation=\"1\""), "attribute patched");
    assert!(
        contains(b"<tiff:Orientation>1</tiff:Orientation>"),
        "element patched"
    );
    assert!(!contains(b"tiff:Orientation=\"3\""));
    assert!(!contains(b">6</tiff:Orientation>"));
    image::load_from_memory(&out).expect("output decodes");
}

/// An EXIF APP1 whose IFD0 links an IFD1 with an embedded thumbnail payload.
fn exif_app1_with_thumbnail(orientation: u16, thumb: &[u8]) -> Vec<u8> {
    let le = true;
    let mut t = Vec::new();
    t.extend_from_slice(b"II");
    t.extend_from_slice(&42u16.to_le_bytes());
    t.extend_from_slice(&8u32.to_le_bytes());
    // IFD0 at 8: 1 entry + next pointer -> IFD1 at 26; IFD1 (2 entries) ends
    // at 56, where the thumbnail bytes start.
    t.extend_from_slice(&1u16.to_le_bytes());
    t.extend_from_slice(&tiff_entry(le, 0x0112, 3, 1, short_value(le, orientation)));
    t.extend_from_slice(&26u32.to_le_bytes());
    t.extend_from_slice(&2u16.to_le_bytes());
    t.extend_from_slice(&tiff_entry(le, 0x0201, 4, 1, long_value(le, 56)));
    t.extend_from_slice(&tiff_entry(
        le,
        0x0202,
        4,
        1,
        long_value(le, thumb.len() as u32),
    ));
    t.extend_from_slice(&0u32.to_le_bytes());
    t.extend_from_slice(thumb);
    exif_segment(&t)
}

#[test]
fn strips_the_embedded_exif_thumbnail() {
    let thumb = b"THUMBNAIL-OF-THE-UNCROPPED-FRAME";
    let source = with_segments(
        &encode_test_jpeg(64, 64),
        &[exif_app1_with_thumbnail(6, thumb)],
    );
    let rendered = encode_test_jpeg(32, 32);

    let out = transplant_metadata(&source, &rendered, 32, 32).expect("transplant succeeds");

    // The thumbnail bytes are zeroed and IFD1 is unlinked, so no reader can
    // recover the uncropped frame from the delivered file.
    let contains = |needle: &[u8]| out.windows(needle.len()).any(|w| w == needle);
    assert!(!contains(thumb), "thumbnail bytes wiped");
    let meta = read_exif(&out);
    assert_eq!(uint_field(&meta, exif::Tag::Orientation), 1);
    assert!(
        meta.get_field(exif::Tag::JPEGInterchangeFormat, exif::In::THUMBNAIL)
            .is_none(),
        "IFD1 unlinked"
    );
    image::load_from_memory(&out).expect("output decodes");
}

#[test]
fn drops_exif_it_cannot_make_safe() {
    // Orientation with type BYTE: kamadak tolerates it at scan time, but the
    // patcher cannot rewrite it, so shipping the segment would double-rotate.
    let le = true;
    let mut t = Vec::new();
    t.extend_from_slice(b"II");
    t.extend_from_slice(&42u16.to_le_bytes());
    t.extend_from_slice(&8u32.to_le_bytes());
    t.extend_from_slice(&1u16.to_le_bytes());
    t.extend_from_slice(&tiff_entry(le, 0x0112, 1, 1, [6, 0, 0, 0]));
    t.extend_from_slice(&0u32.to_le_bytes());
    let com = segment(0xFE, b"kept alongside");
    let source = with_segments(&encode_test_jpeg(64, 64), &[exif_segment(&t), com]);
    let rendered = encode_test_jpeg(32, 32);

    let out = transplant_metadata(&source, &rendered, 32, 32).expect("transplant succeeds");

    let contains = |needle: &[u8]| out.windows(needle.len()).any(|w| w == needle);
    assert!(!contains(b"Exif\0\0"), "unpatchable exif dropped");
    assert!(contains(b"kept alongside"), "other segments kept");
    image::load_from_memory(&out).expect("output decodes");
}

#[test]
fn returns_none_when_the_source_has_no_metadata() {
    let source = encode_test_jpeg(64, 64);
    let rendered = encode_test_jpeg(32, 32);
    assert_eq!(transplant_metadata(&source, &rendered, 32, 32), None);
}

#[test]
fn returns_none_for_a_non_jpeg_source() {
    let rendered = encode_test_jpeg(32, 32);
    assert_eq!(transplant_metadata(b"not a jpeg", &rendered, 32, 32), None);
    assert_eq!(transplant_metadata(&[], &rendered, 32, 32), None);
}

#[test]
fn drops_a_truncated_exif_body_without_a_panic() {
    // An APP1 that claims EXIF but truncates mid-IFD cannot be proven safe,
    // so it is dropped; the transplant itself must not panic.
    let mut app1 = exif_app1(true, 6, 800, 600);
    app1.truncate(24);
    let fixed_len = ((app1.len() - 2) as u16).to_be_bytes();
    app1[2] = fixed_len[0];
    app1[3] = fixed_len[1];
    let source = with_segments(&encode_test_jpeg(64, 64), &[app1]);
    let rendered = encode_test_jpeg(32, 32);

    let out = transplant_metadata(&source, &rendered, 32, 32).expect("transplant succeeds");
    let contains = |needle: &[u8]| out.windows(needle.len()).any(|w| w == needle);
    assert!(!contains(b"Exif\0\0"), "unsafe exif dropped");
    image::load_from_memory(&out).expect("output decodes");
}
