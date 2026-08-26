//! JPEG metadata transplant for the crop-render export path. A rendered crop
//! is a fresh encode, so the executor copies the source's metadata segments
//! (EXIF, XMP, ICC profile, IPTC, comments) into the new file. Facts that
//! describe the old bitstream are corrected in place: the EXIF and XMP
//! orientation values become 1 (the render bakes the rotation into the
//! pixels), the EXIF pixel dimension tags become the cropped size, and the
//! embedded EXIF thumbnail is stripped (it shows the uncropped frame, which
//! a crop made to remove content must not deliver). An EXIF block whose
//! orientation cannot be corrected is dropped rather than shipped over
//! rotation-baked pixels. Structural segments (JFIF APP0, Adobe APP14, MPF
//! offsets) are not copied; they describe the old encode.

/// Copy the metadata segments of `source` into `rendered` and return the new
/// JPEG bytes. `out_w`/`out_h` are the rendered pixel dimensions, written into
/// the EXIF size tags. Returns `None` when either buffer is not a JPEG or the
/// source carries no metadata; the caller then keeps `rendered` unchanged.
pub fn transplant_metadata(
    source: &[u8],
    rendered: &[u8],
    out_w: u32,
    out_h: u32,
) -> Option<Vec<u8>> {
    let segments = metadata_segments(source)?;
    if segments.is_empty() {
        return None;
    }
    if rendered.get(0..2) != Some(&SOI) {
        return None;
    }
    // Insert after the rendered APP0 when one exists: JFIF wants APP0 first.
    let insert_at = after_leading_app0(rendered);
    let extra: usize = segments.iter().map(|s| s.len()).sum();
    let mut out = Vec::with_capacity(rendered.len() + extra);
    out.extend_from_slice(&rendered[..insert_at]);
    for seg in &segments {
        let start = out.len();
        out.extend_from_slice(seg);
        if let Some(tiff) = exif_tiff_range(seg) {
            if !patch_exif(&mut out[start + tiff.start..start + tiff.end], out_w, out_h) {
                // A live orientation over rotation-baked pixels displays the
                // export rotated twice; no EXIF beats wrong EXIF.
                out.truncate(start);
            }
        } else if is_xmp(seg) {
            patch_xmp_orientation(&mut out[start..]);
        }
    }
    out.extend_from_slice(&rendered[insert_at..]);
    Some(out)
}

const SOI: [u8; 2] = [0xFF, 0xD8];
const APP0: u8 = 0xE0;
const APP1: u8 = 0xE1;
const APP2: u8 = 0xE2;
const APP13: u8 = 0xED;
const COM: u8 = 0xFE;
const SOS: u8 = 0xDA;
const EOI: u8 = 0xD9;

/// Collect the metadata segments of `jpeg`, in order, up to the first SOS.
/// Each returned slice is a whole segment: marker, length, payload.
/// `None` when the buffer does not start with SOI.
fn metadata_segments(jpeg: &[u8]) -> Option<Vec<&[u8]>> {
    if jpeg.get(0..2) != Some(&SOI) {
        return None;
    }
    let mut segs = Vec::new();
    let mut i = 2;
    while i + 4 <= jpeg.len() {
        if jpeg[i] != 0xFF {
            break;
        }
        let marker = jpeg[i + 1];
        match marker {
            0xFF => {
                // Fill byte before a marker.
                i += 1;
                continue;
            }
            SOS | EOI => break,
            // Standalone markers carry no length field.
            0x01 | 0xD0..=0xD8 => {
                i += 2;
                continue;
            }
            _ => {}
        }
        let len = u16::from_be_bytes([jpeg[i + 2], jpeg[i + 3]]) as usize;
        if len < 2 || i + 2 + len > jpeg.len() {
            break;
        }
        let seg = &jpeg[i..i + 2 + len];
        if keep_segment(marker, &seg[4..]) {
            segs.push(seg);
        }
        i += 2 + len;
    }
    Some(segs)
}

/// True for segments that carry portable metadata. APP2 is kept only for the
/// ICC profile; other APP2 payloads (MPF) hold byte offsets into the old file.
fn keep_segment(marker: u8, payload: &[u8]) -> bool {
    match marker {
        APP1 | APP13 | COM => true,
        APP2 => payload.starts_with(b"ICC_PROFILE\0"),
        _ => false,
    }
}

/// Byte offset just past the SOI and one optional APP0 segment.
fn after_leading_app0(jpeg: &[u8]) -> usize {
    if jpeg.len() >= 6 && jpeg[2] == 0xFF && jpeg[3] == APP0 {
        let len = u16::from_be_bytes([jpeg[4], jpeg[5]]) as usize;
        let end = 4 + len;
        if len >= 2 && end <= jpeg.len() {
            return end;
        }
    }
    2
}

/// The byte range of the TIFF body inside an EXIF APP1 segment, if this
/// segment is one.
fn exif_tiff_range(seg: &[u8]) -> Option<std::ops::Range<usize>> {
    if seg.get(1) == Some(&APP1) && seg.get(4..10) == Some(b"Exif\0\0") {
        Some(10..seg.len())
    } else {
        None
    }
}

/// True for an XMP APP1 segment.
fn is_xmp(seg: &[u8]) -> bool {
    seg.get(1) == Some(&APP1) && seg[4..].starts_with(b"http://ns.adobe.com/xap/1.0/\0")
}

/// Patch the orientation and size tags inside a TIFF body, in place, and
/// strip the embedded thumbnail. Every read is bounds-checked. Returns false
/// when the body cannot be made safe to ship: the IFD0 walk failed, an
/// orientation entry exists but could not be set to 1, or the thumbnail IFD
/// could not be unlinked. The caller then drops the segment.
fn patch_exif(tiff: &mut [u8], w: u32, h: u32) -> bool {
    let le = match tiff.get(0..2) {
        Some(b"II") => true,
        Some(b"MM") => false,
        _ => return false,
    };
    if read_u16(tiff, 2, le) != Some(42) {
        return false;
    }
    let Some(ifd0) = read_u32(tiff, 4, le) else {
        return false;
    };
    let Some((exif_ifd, orientation_ok)) = patch_ifd(tiff, ifd0 as usize, le, w, h) else {
        return false;
    };
    if let Some(off) = exif_ifd {
        // A failed size patch leaves informational tags stale; not a reason
        // to drop the whole block.
        patch_ifd(tiff, off as usize, le, w, h);
    }
    orientation_ok && strip_thumbnail(tiff, ifd0 as usize, le).is_some()
}

/// Unlink IFD1 from the IFD chain and zero the embedded thumbnail bitstream.
/// The thumbnail shows the uncropped source frame; unlinking hides it from
/// readers and zeroing keeps it out of the delivered bytes. `None` when the
/// next-IFD pointer is out of bounds.
fn strip_thumbnail(tiff: &mut [u8], ifd0: usize, le: bool) -> Option<()> {
    let count = read_u16(tiff, ifd0, le)? as usize;
    let next = ifd0 + 2 + count * 12;
    let ifd1 = read_u32(tiff, next, le)? as usize;
    if ifd1 != 0 {
        wipe_thumbnail_bytes(tiff, ifd1, le);
        tiff.get_mut(next..next + 4)?.fill(0);
    }
    Some(())
}

/// Zero the byte range that IFD1's thumbnail pointer tags name. Best effort:
/// a malformed IFD1 stays unreadable anyway once its pointer is zeroed.
fn wipe_thumbnail_bytes(tiff: &mut [u8], ifd1: usize, le: bool) {
    let Some(count) = read_u16(tiff, ifd1, le) else {
        return;
    };
    let (mut offset, mut len) = (None, None);
    for entry in 0..count as usize {
        let e = ifd1 + 2 + entry * 12;
        let Some(tag) = read_u16(tiff, e, le) else {
            return;
        };
        match tag {
            TAG_THUMB_OFFSET => offset = read_u32(tiff, e + 8, le),
            TAG_THUMB_LENGTH => len = read_u32(tiff, e + 8, le),
            _ => {}
        }
    }
    if let (Some(offset), Some(len)) = (offset, len)
        && let Some(bytes) =
            tiff.get_mut(offset as usize..(offset as usize).saturating_add(len as usize))
    {
        bytes.fill(0);
    }
}

/// Set every `tiff:Orientation` value inside an XMP packet to 1, in place.
/// Handles the attribute form (`tiff:Orientation="6"`) and the element form
/// (`<tiff:Orientation>6<`). Orientation is a single digit, so the swap keeps
/// the packet and segment lengths stable. The XMP dimension properties are
/// left as they are: stale sizes are informational, a stale orientation
/// rotates the export twice.
fn patch_xmp_orientation(xmp: &mut [u8]) {
    const NAME: &[u8] = b"tiff:Orientation";
    let mut from = 0;
    while let Some(pos) = find(&xmp[from..], NAME) {
        let at = from + pos;
        from = at + NAME.len();
        // Skip the closing tag `</tiff:Orientation>`.
        if at > 0 && xmp[at - 1] == b'/' {
            continue;
        }
        match xmp.get(from..from + 3) {
            Some([b'=', quote, digit])
                if (*quote == b'"' || *quote == b'\'') && digit.is_ascii_digit() =>
            {
                xmp[from + 2] = b'1';
            }
            Some([b'>', digit, b'<']) if digit.is_ascii_digit() => {
                xmp[from + 1] = b'1';
            }
            _ => {}
        }
    }
}

/// First occurrence of `needle` in `hay`.
fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

const TAG_IMAGE_WIDTH: u16 = 0x0100;
const TAG_IMAGE_LENGTH: u16 = 0x0101;
const TAG_ORIENTATION: u16 = 0x0112;
const TAG_THUMB_OFFSET: u16 = 0x0201;
const TAG_THUMB_LENGTH: u16 = 0x0202;
const TAG_EXIF_IFD: u16 = 0x8769;
const TAG_PIXEL_X: u16 = 0xA002;
const TAG_PIXEL_Y: u16 = 0xA003;
const TYPE_SHORT: u16 = 3;
const TYPE_LONG: u16 = 4;

/// Patch the known tags of one IFD in place. Returns the EXIF sub-IFD offset
/// when the IFD carries the pointer tag, plus whether the orientation is
/// consistent: no orientation entry, or one that was set to 1. `None` when
/// the walk hit an unreadable entry.
fn patch_ifd(tiff: &mut [u8], ifd: usize, le: bool, w: u32, h: u32) -> Option<(Option<u32>, bool)> {
    let count = read_u16(tiff, ifd, le)? as usize;
    let mut exif_ifd = None;
    let mut orientation_ok = true;
    for entry in 0..count {
        let e = ifd + 2 + entry * 12;
        let tag = read_u16(tiff, e, le)?;
        let typ = read_u16(tiff, e + 2, le)?;
        if read_u32(tiff, e + 4, le)? != 1 {
            orientation_ok &= tag != TAG_ORIENTATION;
            continue;
        }
        match tag {
            TAG_ORIENTATION => orientation_ok &= write_inline(tiff, e + 8, le, typ, 1),
            TAG_IMAGE_WIDTH | TAG_PIXEL_X => {
                write_inline(tiff, e + 8, le, typ, w);
            }
            TAG_IMAGE_LENGTH | TAG_PIXEL_Y => {
                write_inline(tiff, e + 8, le, typ, h);
            }
            TAG_EXIF_IFD if typ == TYPE_LONG => exif_ifd = read_u32(tiff, e + 8, le),
            _ => {}
        }
    }
    Some((exif_ifd, orientation_ok))
}

/// Write a single inline SHORT or LONG value at `off`, respecting the byte
/// order. Returns whether the value was written; other types, or a value too
/// large for the type, are left untouched.
fn write_inline(tiff: &mut [u8], off: usize, le: bool, typ: u16, value: u32) -> bool {
    match typ {
        TYPE_SHORT => {
            let Ok(v) = u16::try_from(value) else {
                return false;
            };
            let bytes = if le { v.to_le_bytes() } else { v.to_be_bytes() };
            match tiff.get_mut(off..off + 2) {
                Some(dst) => {
                    dst.copy_from_slice(&bytes);
                    true
                }
                None => false,
            }
        }
        TYPE_LONG => {
            let bytes = if le {
                value.to_le_bytes()
            } else {
                value.to_be_bytes()
            };
            match tiff.get_mut(off..off + 4) {
                Some(dst) => {
                    dst.copy_from_slice(&bytes);
                    true
                }
                None => false,
            }
        }
        _ => false,
    }
}

fn read_u16(b: &[u8], off: usize, le: bool) -> Option<u16> {
    let s: [u8; 2] = b.get(off..off + 2)?.try_into().ok()?;
    Some(if le {
        u16::from_le_bytes(s)
    } else {
        u16::from_be_bytes(s)
    })
}

fn read_u32(b: &[u8], off: usize, le: bool) -> Option<u32> {
    let s: [u8; 4] = b.get(off..off + 4)?.try_into().ok()?;
    Some(if le {
        u32::from_le_bytes(s)
    } else {
        u32::from_be_bytes(s)
    })
}
