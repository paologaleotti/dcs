//! Folder scanning. Walks a root recursively on a worker thread, classifies
//! image files, reads EXIF orientation and capture time, and streams
//! `ScannedFile`s back over a channel so the grid can fill progressively (§4).
//! The UI thread never blocks: it drains the handle each frame.

use std::fs::File;
use std::io::{BufReader, Read, Seek, SeekFrom};
use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::UNIX_EPOCH;

use crossbeam_channel::{Receiver, Sender, unbounded};
use dcs_domain::fingerprint::ContentFingerprint;
use dcs_domain::pairing::{FileKind, ScannedFile, classify};
use dcs_domain::photo::{CaptureMeta, Orientation};
use rayon::prelude::*;
use time::PrimitiveDateTime;
use time::macros::format_description;
use walkdir::WalkDir;

use crate::cache::{FingerprintCache, SharedCache};

/// Bytes hashed from each end of a large file. Files at or below twice this are
/// hashed whole, so small files collide only on a true blake3 collision.
const FP_CHUNK: u64 = 64 * 1024;

/// Live handle to a running scan. Drop it and the worker finishes on its own;
/// the channel simply stops being read.
pub struct ScanHandle {
    rx: Receiver<ScannedFile>,
    done: Arc<AtomicBool>,
}

/// Start scanning `root` on a worker thread. Returns immediately. When a
/// `cache` is supplied, unchanged files (matching `(mtime, size)`) skip
/// re-hashing — the import-budget pre-filter (open Q#8).
pub fn scan(root: PathBuf, cache: Option<SharedCache>) -> ScanHandle {
    let (tx, rx) = unbounded();
    let done = Arc::new(AtomicBool::new(false));
    let worker_done = Arc::clone(&done);
    thread::spawn(move || {
        walk(&root, &tx, cache.as_ref());
        worker_done.store(true, Ordering::Release);
    });
    ScanHandle { rx, done }
}

impl ScanHandle {
    /// Take every file discovered since the last call. Non-blocking.
    pub fn drain(&self) -> Vec<ScannedFile> {
        self.rx.try_iter().collect()
    }

    /// True while the worker is still walking the tree.
    pub fn is_running(&self) -> bool {
        !self.done.load(Ordering::Acquire)
    }
}

fn walk(root: &Path, tx: &Sender<ScannedFile>, cache: Option<&SharedCache>) {
    // Enumerate first (a fast stat-only walk), then read EXIF + fingerprint in
    // parallel: file reads are the scan's real cost and embarrassingly
    // parallel. Sort order is derived, so arrival order doesn't matter (§2.2).
    let files: Vec<(PathBuf, FileKind)> = WalkDir::new(root)
        .into_iter()
        .filter_entry(|e| !is_hidden(e.path()))
        .filter_map(Result::ok)
        .filter(|e| e.file_type().is_file())
        // Import both JPEG and RAW so a JPEG+RAW pair merges into one photo and a
        // RAW's presence is known. v1 only decodes the JPEG; a RAW-only photo is
        // paired/persisted but hidden until RAW decode lands.
        .filter_map(|e| classify(e.path()).map(|kind| (e.into_path(), kind)))
        .collect();

    files
        .into_par_iter()
        .for_each_with(tx.clone(), |tx, (path, kind)| {
            let scanned = match kind {
                FileKind::Jpeg => {
                    let (orientation, captured_at, meta) = read_meta(&path);
                    let fingerprint = fingerprint_of(&path, root, cache);
                    ScannedFile {
                        path,
                        kind,
                        orientation,
                        fingerprint,
                        captured_at,
                        meta,
                    }
                }
                // v1 never decodes a RAW and a pair takes the JPEG's metadata, so
                // reading the RAW's EXIF and hashing its (often huge) contents is
                // pure waste. Identify it cheaply by path+size — enough to pair by
                // name and to know the RAW exists.
                FileKind::Raw => ScannedFile {
                    fingerprint: cheap_fingerprint(&path),
                    path,
                    kind,
                    orientation: Default::default(),
                    captured_at: None,
                    meta: CaptureMeta::default(),
                },
            };
            // A closed receiver means the session moved on; the send simply fails.
            let _ = tx.send(scanned);
        });
}

/// A cheap, content-free identity for a RAW: blake3 over its (absolute) path and
/// size, reading no file content. RAW identity isn't stable across renames or a
/// moved folder (a future concern when RAW-only photos decode), but a pair's
/// identity is its JPEG's, so this never touches the photo the user sees.
fn cheap_fingerprint(path: &Path) -> ContentFingerprint {
    let (_, size) = file_stat(path);
    let mut hasher = blake3::Hasher::new();
    hasher.update(path.to_string_lossy().as_bytes());
    hasher.update(&size.to_le_bytes());
    ContentFingerprint::from_bytes(*hasher.finalize().as_bytes())
}

/// The content fingerprint for a file, reusing the cache when `(mtime, size)`
/// are unchanged, else hashing and caching the result. Cache keys are paths
/// relative to the scan root so the project folder stays portable (§5).
fn fingerprint_of(path: &Path, root: &Path, cache: Option<&SharedCache>) -> ContentFingerprint {
    let (mtime, size) = file_stat(path);
    let rel = path
        .strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .into_owned();

    // Lock held only for the keyed lookup, never across the hash below.
    if let Some(cache) = cache
        && let Ok(guard) = cache.lock()
        && let Some(fingerprint) = guard.lookup(&rel, mtime, size)
    {
        return fingerprint;
    }

    let fingerprint = hash_file(path, size);
    if let Some(cache) = cache
        && let Ok(guard) = cache.lock()
    {
        guard.store(&rel, mtime, size, &fingerprint);
    }
    fingerprint
}

/// `(mtime_secs, size)` for the pre-filter. Missing metadata degrades to
/// `(0, 0)` so a stat failure never aborts the scan (§4).
fn file_stat(path: &Path) -> (i64, u64) {
    let Ok(meta) = std::fs::metadata(path) else {
        return (0, 0);
    };
    let mtime = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    (mtime, meta.len())
}

/// blake3 over `head[64K] ‖ tail[64K] ‖ size` (decision #33, open Q#8). Files at
/// or below `2 * FP_CHUNK` are hashed whole. An unreadable file still gets a
/// stable fingerprint from its path + size, so the scan never aborts (§4).
fn hash_file(path: &Path, size: u64) -> ContentFingerprint {
    let mut hasher = blake3::Hasher::new();
    match File::open(path) {
        Ok(mut file) => {
            if size <= 2 * FP_CHUNK {
                let mut buf = Vec::new();
                let _ = file.read_to_end(&mut buf);
                hasher.update(&buf);
            } else {
                let mut head = vec![0u8; FP_CHUNK as usize];
                let n = read_into(&mut file, &mut head);
                hasher.update(&head[..n]);
                if file.seek(SeekFrom::Start(size - FP_CHUNK)).is_ok() {
                    let mut tail = vec![0u8; FP_CHUNK as usize];
                    let n = read_into(&mut file, &mut tail);
                    hasher.update(&tail[..n]);
                }
            }
        }
        Err(_) => {
            // Unreadable: fall back to a path-derived identity so the photo is
            // still distinguishable and the scan continues.
            hasher.update(path.to_string_lossy().as_bytes());
        }
    }
    hasher.update(&size.to_le_bytes());
    ContentFingerprint::from_bytes(*hasher.finalize().as_bytes())
}

/// Read up to `buf.len()` bytes, returning how many were read. Tolerates short
/// reads; the caller hashes only the bytes returned.
fn read_into(file: &mut File, buf: &mut [u8]) -> usize {
    let mut read = 0;
    while read < buf.len() {
        match file.read(&mut buf[read..]) {
            Ok(0) => break,
            Ok(n) => read += n,
            Err(_) => break,
        }
    }
    read
}

fn is_hidden(path: &Path) -> bool {
    path.file_name()
        .and_then(|n| n.to_str())
        .map(|n| n.starts_with('.'))
        .unwrap_or(false)
}

/// One EXIF pass for orientation, capture time, and the gallery caption facts.
/// Missing or unreadable EXIF degrades to defaults rather than failing the scan;
/// each field is read independently so one bad tag never loses the rest.
fn read_meta(path: &Path) -> (Orientation, Option<PrimitiveDateTime>, CaptureMeta) {
    let Some(exif) = read_exif(path) else {
        return (Orientation::default(), None, CaptureMeta::default());
    };
    let orientation = exif
        .get_field(exif::Tag::Orientation, exif::In::PRIMARY)
        .and_then(|f| f.value.get_uint(0))
        .map(|n| Orientation::from_exif(n as u16))
        .unwrap_or_default();
    let captured_at = exif
        .get_field(exif::Tag::DateTimeOriginal, exif::In::PRIMARY)
        .and_then(ascii_value)
        .and_then(parse_exif_datetime);
    let meta = CaptureMeta {
        camera: camera_label(&exif),
        lens: ascii_field(&exif, exif::Tag::LensModel),
        focal_mm: rational_f32(&exif, exif::Tag::FocalLength),
        aperture: rational_f32(&exif, exif::Tag::FNumber),
        exposure_secs: rational_f32(&exif, exif::Tag::ExposureTime),
        iso: exif
            .get_field(exif::Tag::PhotographicSensitivity, exif::In::PRIMARY)
            .and_then(|f| f.value.get_uint(0)),
    };
    (orientation, captured_at, meta)
}

/// Camera make + model joined into one label, avoiding the common case where the
/// model already repeats the make (e.g. Canon writes `Canon EOS R5`).
fn camera_label(exif: &exif::Exif) -> Option<String> {
    let make = ascii_field(exif, exif::Tag::Make);
    let model = ascii_field(exif, exif::Tag::Model);
    match (make, model) {
        (Some(make), Some(model)) => {
            if model
                .to_ascii_lowercase()
                .starts_with(&make.to_ascii_lowercase())
            {
                Some(model)
            } else {
                Some(format!("{make} {model}"))
            }
        }
        (Some(make), None) => Some(make),
        (None, Some(model)) => Some(model),
        (None, None) => None,
    }
}

/// A trimmed, non-empty ASCII EXIF field as an owned `String`.
fn ascii_field(exif: &exif::Exif, tag: exif::Tag) -> Option<String> {
    let field = exif.get_field(tag, exif::In::PRIMARY)?;
    let value = ascii_value(field)?.trim();
    (!value.is_empty()).then(|| value.to_string())
}

/// The first rational of a field as `f32`, guarding a zero denominator.
fn rational_f32(exif: &exif::Exif, tag: exif::Tag) -> Option<f32> {
    let field = exif.get_field(tag, exif::In::PRIMARY)?;
    match &field.value {
        exif::Value::Rational(parts) => parts
            .first()
            .filter(|r| r.denom != 0)
            .map(|r| r.num as f32 / r.denom as f32),
        _ => None,
    }
}

fn ascii_value(field: &exif::Field) -> Option<&str> {
    match &field.value {
        exif::Value::Ascii(parts) => parts
            .first()
            .and_then(|bytes| std::str::from_utf8(bytes).ok()),
        _ => None,
    }
}

fn read_exif(path: &Path) -> Option<exif::Exif> {
    let file = std::fs::File::open(path).ok()?;
    let mut reader = BufReader::new(&file);
    exif::Reader::new().read_from_container(&mut reader).ok()
}

fn parse_exif_datetime(value: &str) -> Option<PrimitiveDateTime> {
    // EXIF `DateTimeOriginal` is NUL-terminated ASCII ("YYYY:MM:DD HH:MM:SS\0").
    let value = value.trim_matches(|c: char| c.is_whitespace() || c == '\0');
    let fmt = format_description!("[year]:[month]:[day] [hour]:[minute]:[second]");
    PrimitiveDateTime::parse(value, fmt).ok()
}

#[cfg(test)]
mod tests {
    use super::parse_exif_datetime;
    use time::macros::datetime;

    #[test]
    fn parses_standard_exif_datetime() {
        assert_eq!(
            parse_exif_datetime("2025:05:11 14:30:00"),
            Some(datetime!(2025-05-11 14:30:00))
        );
    }

    #[test]
    fn tolerates_trailing_nul_and_whitespace() {
        // Real EXIF values are NUL-terminated; the parser must strip it.
        assert_eq!(
            parse_exif_datetime("2025:05:11 14:30:00\0"),
            Some(datetime!(2025-05-11 14:30:00))
        );
        assert_eq!(
            parse_exif_datetime("  2025:05:11 14:30:00  "),
            Some(datetime!(2025-05-11 14:30:00))
        );
    }

    #[test]
    fn rejects_garbage_and_zeroed_dates() {
        assert_eq!(parse_exif_datetime("not a date"), None);
        assert_eq!(parse_exif_datetime("0000:00:00 00:00:00"), None);
        assert_eq!(parse_exif_datetime(""), None);
    }
}
