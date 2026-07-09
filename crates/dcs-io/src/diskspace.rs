//! Disk-space queries for the export dialog's space check: free bytes on the
//! destination filesystem, and a single file's size. Infrastructure — the free
//! function talks to the OS (`statvfs` / `GetDiskFreeSpaceExW` via `fs2`); the
//! sizes it returns feed the pure planner's `required_bytes` accounting above.

use std::fs;
use std::io;
use std::path::Path;

/// Free bytes available to the current user on the filesystem holding `path`.
/// `path` must exist (the chosen destination folder); an error surfaces as an
/// unknown-space state in the dialog rather than a wrong number.
pub fn available_space(path: &Path) -> io::Result<u64> {
    fs2::available_space(path)
}

/// A single file's size in bytes, or `None` when it can't be stat-ed (gone since
/// scan, unreadable). The export size estimate treats a missing source as zero.
pub fn file_size(path: &Path) -> Option<u64> {
    fs::metadata(path).ok().map(|m| m.len())
}
