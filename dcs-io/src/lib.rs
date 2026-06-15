//! dcs-io — infrastructure behind traits.
//!
//! `imaging` (decode, embedded preview, orientation, thumb cache, prefetch),
//! `source` (scan, EXIF, content fingerprint, progressive import, missing-file
//! detection), `persistence` (versioned DTOs, undo.log), and the *dumb* export
//! executor (walks an `ExportPlan`, copies .part->fsync->rename, makes no
//! decisions). Whole thread model lives here behind handle-returning traits. (§9)
//!
//! Depends DOWN on dcs-domain only.

pub fn placeholder() {}
