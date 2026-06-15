//! Folder scanning. Walks a root recursively on a worker thread, classifies
//! image files, reads EXIF orientation and capture time, and streams
//! `ScannedFile`s back over a channel so the grid can fill progressively (§4).
//! The UI thread never blocks: it drains the handle each frame.

use std::io::BufReader;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;

use crossbeam_channel::{Receiver, Sender, unbounded};
use dcs_domain::pairing::{FileKind, ScannedFile, classify};
use dcs_domain::photo::Orientation;
use rayon::prelude::*;
use time::PrimitiveDateTime;
use time::macros::format_description;
use walkdir::WalkDir;

/// Live handle to a running scan. Drop it and the worker finishes on its own;
/// the channel simply stops being read.
pub struct ScanHandle {
    rx: Receiver<ScannedFile>,
    done: Arc<AtomicBool>,
}

/// Start scanning `root` on a worker thread. Returns immediately.
pub fn scan(root: PathBuf) -> ScanHandle {
    let (tx, rx) = unbounded();
    let done = Arc::new(AtomicBool::new(false));
    let worker_done = Arc::clone(&done);
    thread::spawn(move || {
        walk(&root, &tx);
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

fn walk(root: &Path, tx: &Sender<ScannedFile>) {
    // Enumerate first (a fast stat-only walk), then read EXIF in parallel:
    // metadata reads are the scan's real cost and embarrassingly parallel.
    // Sort order is derived, so arrival order doesn't matter (§2.2).
    let paths: Vec<PathBuf> = WalkDir::new(root)
        .into_iter()
        .filter_entry(|e| !is_hidden(e.path()))
        .filter_map(Result::ok)
        .filter(|e| e.file_type().is_file())
        .map(|e| e.into_path())
        // JPEG-only for now: RAW files are recognized by the domain but not
        // imported yet (no embedded-preview decode).
        .filter(|p| classify(p) == Some(FileKind::Jpeg))
        .collect();

    paths.into_par_iter().for_each_with(tx.clone(), |tx, path| {
        let (orientation, captured_at) = read_meta(&path);
        // A closed receiver means the session moved on; the send simply fails.
        let _ = tx.send(ScannedFile {
            path,
            kind: FileKind::Jpeg,
            orientation,
            captured_at,
        });
    });
}

fn is_hidden(path: &Path) -> bool {
    path.file_name()
        .and_then(|n| n.to_str())
        .map(|n| n.starts_with('.'))
        .unwrap_or(false)
}

/// One EXIF pass for orientation + capture time (§4). Missing or unreadable
/// EXIF degrades to `(Normal, None)` rather than failing the scan.
fn read_meta(path: &Path) -> (Orientation, Option<PrimitiveDateTime>) {
    let Some(exif) = read_exif(path) else {
        return (Orientation::default(), None);
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
    (orientation, captured_at)
}

fn ascii_value(field: &exif::Field) -> Option<&str> {
    match &field.value {
        exif::Value::Ascii(parts) => parts.first().and_then(|bytes| std::str::from_utf8(bytes).ok()),
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
