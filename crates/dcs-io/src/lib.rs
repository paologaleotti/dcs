//! dcs-io — infrastructure behind traits. The thread model lives here.
//!
//! Current slice: `imaging` (off-thread JPEG decode, orientation, thumbnails)
//! and `source` (folder scan + EXIF). Persistence and the export executor land
//! in later slices (§9).
//!
//! Depends DOWN on dcs-domain only.

pub mod cache;
pub mod imaging;
pub mod lock;
pub mod persistence;
pub mod recents;
pub mod source;
pub mod undo_log;
