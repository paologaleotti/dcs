//! dcs-io — infrastructure behind traits. The thread model lives here.
//!
//! Current slice: `imaging` (off-thread JPEG decode, orientation, thumbnails),
//! `source` (folder scan + EXIF), persistence, and the dumb export executor.
//!
//! Depends DOWN on dcs-domain only.

pub mod cache;
pub mod export;
pub mod imaging;
pub mod lock;
pub mod persistence;
pub mod recents;
pub mod reveal;
pub mod source;
pub mod undo_log;
