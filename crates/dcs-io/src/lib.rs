//! dcs-io — infrastructure behind traits. The thread model lives here.
//!
//! Current slice: `imaging` (off-thread JPEG decode, orientation, thumbnails),
//! `source` (folder scan + EXIF), persistence, and the dumb export executor.
//!
//! Depends DOWN on dcs-domain only.

pub mod cache;
pub mod contact_sheet;
pub mod diskspace;
pub mod embedding;
pub mod export;
pub mod imaging;
pub mod jpeg_meta;
pub mod lock;
pub mod persistence;
pub mod print;
pub mod recents;
pub mod reveal;
pub mod source;
pub mod undo_log;
