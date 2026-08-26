//! Locating the JPEG preview a RAW file embeds. v1 never decodes sensor data:
//! every RAW format carries one or more ordinary JPEGs (a small EXIF thumbnail
//! and usually a medium or full-size preview), and those decode with the same
//! libjpeg-turbo path as a camera JPEG.
//!
//! Finding them is a byte scan, not a format parse, so one code path covers
//! TIFF-based RAWs (CR2, NEF, ARW, DNG, ORF, RW2, PEF) and container formats
//! (CR3) alike, with an exact fast path for RAF where the header states the
//! offset outright.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::ops::Range;
use std::path::Path;

/// How much of the file to read before giving up on finding a preview. Previews
/// are indexed from the header, so each stage after the first is a rare fallback
/// for a format that parks its preview deep in the file.
const STAGE_BYTES: &[u64] = &[1 << 20, 8 << 20, 32 << 20];

/// Smallest JPEG worth showing. The 160×120 EXIF thumbnail every camera writes
/// lands well under this; upscaling it into a grid cell would look like a broken
/// decode, so a file offering nothing bigger gets the RAW plate instead.
const MIN_PREVIEW_BYTES: usize = 24 * 1024;

/// How many candidates to hand back. The largest is nearly always the right one;
/// the runners-up only matter when it turns out to be undecodable (some DNGs
/// store lossless-JPEG sensor tiles that scan like an ordinary JPEG).
const MAX_CANDIDATES: usize = 3;

const RAF_MAGIC: &[u8] = b"FUJIFILMCCD-RAW";
/// Big-endian `u32` offset and length of the preview, in the RAF header.
const RAF_JPEG_OFFSET_AT: usize = 84;
const RAF_JPEG_LENGTH_AT: usize = 88;
const RAF_HEADER_BYTES: usize = 92;

const SOI: [u8; 2] = [0xFF, 0xD8];

/// How far into the file to look for an embedded EXIF block, when the offset
/// isn't stated outright. Bounded because this runs on the scan, per RAW.
const EXIF_SCAN_BYTES: u64 = 1 << 20;
/// Window read at RAF's stated preview offset — the preview's EXIF sits in its
/// first segments, so there is no reason to read the whole preview.
const RAF_EXIF_WINDOW: usize = 128 * 1024;
/// How much to hand the EXIF parser per candidate. Tag offsets are relative to
/// the block's own TIFF header, and IFDs sit at its front.
const EXIF_BLOCK_BYTES: usize = 256 * 1024;
const MAX_EXIF_CANDIDATES: usize = 4;
/// The JPEG APP1 EXIF marker; the TIFF block starts right after it.
const APP1_EXIF: &[u8] = b"Exif\0\0";
/// Canon CR3 keeps EXIF in these ISO-BMFF boxes rather than in a JPEG segment:
/// `CMT1` is IFD0 (make, model, orientation), `CMT2` the Exif IFD (capture
/// time). A box's payload — a whole TIFF block — follows its four-byte name.
const CR3_EXIF_BOXES: [&[u8]; 2] = [b"CMT1", b"CMT2"];

/// EXIF blocks embedded in a RAW, in the order worth trying.
///
/// RAF and CR3 are not TIFF at offset 0, so a container-sniffing EXIF reader
/// rejects them outright even though the metadata is right there. Without this a
/// Fuji or recent-Canon shoot imports undated, which means it groups and sorts
/// wrong. TIFF-based RAWs (NEF, CR2, ARW, DNG, ORF) never need it — the reader
/// parses those directly.
pub fn embedded_exif_blocks(path: &Path) -> Vec<Vec<u8>> {
    let Some(data) = raf_exif_window(path).or_else(|| read_prefix(path, EXIF_SCAN_BYTES)) else {
        return Vec::new();
    };
    let mut starts: Vec<usize> = find_all(&data, APP1_EXIF)
        .map(|at| at + APP1_EXIF.len())
        .collect();
    for name in CR3_EXIF_BOXES {
        starts.extend(find_all(&data, name).map(|at| at + name.len()));
    }
    starts.truncate(MAX_EXIF_CANDIDATES);
    starts
        .into_iter()
        .map(|at| {
            let end = (at + EXIF_BLOCK_BYTES).min(data.len());
            data[at..end].to_vec()
        })
        .collect()
}

/// The head of RAF's stated preview, where its EXIF lives. `None` for any other
/// format, leaving the caller to scan.
fn raf_exif_window(path: &Path) -> Option<Vec<u8>> {
    let mut file = File::open(path).ok()?;
    let mut header = [0u8; RAF_HEADER_BYTES];
    file.read_exact(&mut header).ok()?;
    if !header.starts_with(RAF_MAGIC) {
        return None;
    }
    let offset = be_u32(&header, RAF_JPEG_OFFSET_AT)? as u64;
    let length = be_u32(&header, RAF_JPEG_LENGTH_AT)? as usize;
    file.seek(SeekFrom::Start(offset)).ok()?;
    let mut window = Vec::new();
    file.take(length.min(RAF_EXIF_WINDOW) as u64)
        .read_to_end(&mut window)
        .ok()?;
    Some(window)
}

fn find_all<'a>(data: &'a [u8], needle: &'a [u8]) -> impl Iterator<Item = usize> + 'a {
    data.windows(needle.len())
        .enumerate()
        .filter_map(move |(at, w)| (w == needle).then_some(at))
}

/// The complete JPEGs embedded in a RAW file, largest first and capped at
/// [`MAX_CANDIDATES`]. Empty when the file embeds nothing big enough to show.
///
/// Only whole JPEGs are returned — a candidate whose stream is cut off is
/// discarded rather than handed to the decoder as a half image.
pub fn embedded_previews(path: &Path) -> Vec<Vec<u8>> {
    if let Some(jpeg) = raf_preview(path) {
        return vec![jpeg];
    }
    let Ok(len) = std::fs::metadata(path).map(|m| m.len()) else {
        return Vec::new();
    };
    for &stage in STAGE_BYTES {
        let read = stage.min(len);
        let Some(data) = read_prefix(path, read) else {
            return Vec::new();
        };
        let (spans, cut_off) = scan_candidates(&data);
        let mut spans: Vec<Range<usize>> = spans
            .into_iter()
            .filter(|s| s.len() >= MIN_PREVIEW_BYTES)
            .collect();
        // A JPEG that ran past the end of what was read may well be the biggest
        // one, so reading further comes before settling for what did fit. The OS
        // page cache serves the re-read of the earlier bytes.
        if read < len && (spans.is_empty() || cut_off) {
            continue;
        }
        // Largest first: the decoder takes the first that works, so the best
        // available preview wins and a rejected one falls through to the next.
        spans.sort_by_key(|s| std::cmp::Reverse(s.len()));
        spans.truncate(MAX_CANDIDATES);
        return spans.into_iter().map(|s| data[s].to_vec()).collect();
    }
    Vec::new()
}

/// Fuji's RAF header states the preview's offset and length, so no scan is
/// needed. `None` for any other format, or a header that points out of bounds.
fn raf_preview(path: &Path) -> Option<Vec<u8>> {
    let mut file = File::open(path).ok()?;
    let mut header = [0u8; RAF_HEADER_BYTES];
    file.read_exact(&mut header).ok()?;
    if !header.starts_with(RAF_MAGIC) {
        return None;
    }
    let offset = be_u32(&header, RAF_JPEG_OFFSET_AT)? as u64;
    let length = be_u32(&header, RAF_JPEG_LENGTH_AT)? as usize;
    if length < MIN_PREVIEW_BYTES {
        return None;
    }
    file.seek(SeekFrom::Start(offset)).ok()?;
    let mut jpeg = vec![0u8; length];
    file.read_exact(&mut jpeg).ok()?;
    jpeg.starts_with(&SOI).then_some(jpeg)
}

fn be_u32(data: &[u8], at: usize) -> Option<u32> {
    let bytes: [u8; 4] = data.get(at..at + 4)?.try_into().ok()?;
    Some(u32::from_be_bytes(bytes))
}

/// Read at most `limit` bytes from the start of the file.
fn read_prefix(path: &Path, limit: u64) -> Option<Vec<u8>> {
    let file = File::open(path).ok()?;
    let mut data = Vec::with_capacity(limit as usize);
    file.take(limit).read_to_end(&mut data).ok()?;
    Some(data)
}

/// Every complete JPEG in `data`, in the order they appear. Each match is walked
/// segment by segment, so a JPEG nested inside another's EXIF block is consumed
/// as part of its parent rather than reported separately — and cannot truncate it.
///
/// The flag reports that some match did not complete, which for a partial read
/// means a preview may extend past it.
fn scan_candidates(data: &[u8]) -> (Vec<Range<usize>>, bool) {
    let mut found = Vec::new();
    let mut cut_off = false;
    let mut at = 0;
    while let Some(start) = find_soi(data, at) {
        match jpeg_end(data, start) {
            Some(end) => {
                found.push(start..end);
                at = end;
            }
            // Ran off the end, or malformed. Resume past this marker rather than
            // stopping, so one bad match can't hide a later good one.
            None => {
                cut_off = true;
                at = start + SOI.len();
            }
        }
    }
    (found, cut_off)
}

/// The next `FF D8 FF` at or after `from`. The third byte (the start of the
/// following marker) is required so arbitrary sensor data matching `FF D8` is
/// less likely to be mistaken for a JPEG.
fn find_soi(data: &[u8], from: usize) -> Option<usize> {
    data.get(from..)?
        .windows(3)
        .position(|w| w[0] == 0xFF && w[1] == 0xD8 && w[2] == 0xFF)
        .map(|p| from + p)
}

/// Walk the JPEG starting at `start` and return the offset just past its `EOI`.
/// `None` when the stream is malformed or runs off the end of `data`.
fn jpeg_end(data: &[u8], start: usize) -> Option<usize> {
    let mut at = start + SOI.len();
    loop {
        // Markers may be preceded by any number of 0xFF fill bytes.
        while *data.get(at)? == 0xFF && *data.get(at + 1)? == 0xFF {
            at += 1;
        }
        if *data.get(at)? != 0xFF {
            return None;
        }
        let marker = *data.get(at + 1)?;
        at += 2;
        match marker {
            0xD9 => return Some(at),
            // Standalone markers: no length field, nothing to skip.
            0x01 | 0xD0..=0xD7 => {}
            // Start of scan: skip its header, then the entropy-coded data, which
            // has no length and ends at the next real marker.
            0xDA => {
                at += segment_len(data, at)?;
                at = end_of_entropy(data, at)?;
            }
            _ => at += segment_len(data, at)?,
        }
    }
}

/// The big-endian segment length at `at`, which includes its own two bytes.
fn segment_len(data: &[u8], at: usize) -> Option<usize> {
    let bytes: [u8; 2] = data.get(at..at + 2)?.try_into().ok()?;
    let len = u16::from_be_bytes(bytes) as usize;
    (len >= 2).then_some(len)
}

/// Skip entropy-coded scan data and return the offset of the next real marker.
/// Inside a scan, a literal `0xFF` byte is stuffed as `FF 00`, and restart
/// markers are expected mid-stream; neither ends the scan.
fn end_of_entropy(data: &[u8], from: usize) -> Option<usize> {
    let mut at = from;
    loop {
        at += data.get(at..)?.iter().position(|&b| b == 0xFF)?;
        match *data.get(at + 1)? {
            // A fill byte: step over one 0xFF and re-examine the next pair.
            0xFF => at += 1,
            0x00 | 0xD0..=0xD7 => at += 2,
            _ => return Some(at),
        }
    }
}
