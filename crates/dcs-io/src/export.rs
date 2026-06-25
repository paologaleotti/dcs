//! The dumb export executor. Walks a finished `ExportPlan` on a worker
//! thread, copies each file with the atomic `.part` → fsync → rename contract,
//! streams progress, and supports cancel between files. It makes no path,
//! rename, or skip decisions — the planner settled all of those. It only ever
//! refuses to overwrite: a dest that already exists on disk (a pre-existing
//! file the pure planner couldn't see) is skipped and reported, never clobbered.

use std::ffi::OsString;
use std::fs::{self, File};
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;

use crossbeam_channel::{Receiver, unbounded};
use dcs_domain::crops::CropEdit;
use dcs_domain::export::{ExportOp, ExportPlan, FileRole, OpKind};

use crate::imaging;

/// One executor outcome per plan op, in order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExportEvent {
    /// The file was copied to its dest.
    Copied { role: FileRole },
    /// The op was not performed; the original is untouched.
    Skipped { reason: SkipKind },
    /// The copy failed; the message carries context for the toast.
    Failed { error: String },
}

/// Why an op was skipped without copying.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SkipKind {
    /// A file already exists at the dest — never overwritten.
    DestExists,
    /// The source file is gone since planning.
    SourceMissing,
}

/// Live handle to a running export. Poll it for events each frame; cancel stops
/// the worker after the current file (a clean, reported partial state).
pub struct ExportHandle {
    rx: Receiver<ExportEvent>,
    cancel: Arc<AtomicBool>,
    done: Arc<AtomicBool>,
    total: usize,
}

/// Execute `plan` on a worker thread. Returns immediately; the UI drains
/// [`ExportHandle::poll`] each frame and reads [`ExportHandle::total`] for a
/// progress denominator.
pub fn run_export(plan: ExportPlan) -> ExportHandle {
    let (tx, rx) = unbounded();
    let cancel = Arc::new(AtomicBool::new(false));
    let done = Arc::new(AtomicBool::new(false));
    let total = plan.ops.len();

    let worker_cancel = Arc::clone(&cancel);
    let worker_done = Arc::clone(&done);
    thread::spawn(move || {
        for op in &plan.ops {
            if worker_cancel.load(Ordering::Acquire) {
                break;
            }
            // A closed receiver means the session moved on; the send simply fails.
            let _ = tx.send(execute_op(op));
        }
        worker_done.store(true, Ordering::Release);
    });

    ExportHandle {
        rx,
        cancel,
        done,
        total,
    }
}

impl ExportHandle {
    /// Take every event produced since the last call. Non-blocking.
    pub fn poll(&self) -> Vec<ExportEvent> {
        self.rx.try_iter().collect()
    }

    /// True while files are still being copied.
    pub fn is_running(&self) -> bool {
        !self.done.load(Ordering::Acquire)
    }

    /// Total ops in the plan — the progress denominator.
    pub fn total(&self) -> usize {
        self.total
    }

    /// Request cancellation; the worker stops after the in-flight file.
    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::Release);
    }
}

fn execute_op(op: &ExportOp) -> ExportEvent {
    if !op.source.exists() {
        return ExportEvent::Skipped {
            reason: SkipKind::SourceMissing,
        };
    }
    if op.dest.exists() {
        return ExportEvent::Skipped {
            reason: SkipKind::DestExists,
        };
    }
    if let Some(parent) = op.dest.parent()
        && let Err(e) = fs::create_dir_all(parent)
    {
        return ExportEvent::Failed {
            error: format!("create {}: {e}", parent.display()),
        };
    }
    let result = match op.kind {
        OpKind::Copy => copy_atomic(&op.source, &op.dest),
        OpKind::RenderCrop { edit, orientation } => {
            render_crop_atomic(&op.source, &op.dest, &edit, orientation)
        }
    };
    match result {
        Ok(()) => ExportEvent::Copied { role: op.role },
        Err(CopyError::DestExists) => ExportEvent::Skipped {
            reason: SkipKind::DestExists,
        },
        Err(CopyError::Io(msg)) => ExportEvent::Failed { error: msg },
    }
}

/// Render a cropped+straightened JPEG to `dest` via the same atomic
/// `.part` → fsync → rename + never-overwrite contract as a copy. Decodes the
/// source at full resolution, applies orientation then the crop (one resample),
/// and encodes a high-quality JPEG. The planner already decided the source,
/// dest, and that this op renders — this only executes the pixels.
fn render_crop_atomic(
    source: &Path,
    dest: &Path,
    edit: &CropEdit,
    orientation: dcs_domain::photo::Orientation,
) -> Result<(), CopyError> {
    let jpeg = render_crop_jpeg(source, edit, orientation).ok_or_else(|| {
        CopyError::Io(format!("render {}: decode/encode failed", source.display()))
    })?;
    let tmp = part_path(dest);
    fs::write(&tmp, &jpeg).map_err(|e| CopyError::Io(format!("write {}: {e}", tmp.display())))?;
    finalize_atomic(&tmp, dest)
}

/// Decode → orient → crop → encode, returning the JPEG bytes. `EXPORT_QUALITY`
/// is high so the re-encode is visually lossless against the cropped frame.
/// Edge `u32::MAX` keeps full output resolution (no downscale) for the export.
fn render_crop_jpeg(
    source: &Path,
    edit: &CropEdit,
    orientation: dcs_domain::photo::Orientation,
) -> Option<Vec<u8>> {
    let oriented = imaging::decode_oriented_full(source, orientation)?;
    let cropped = imaging::apply_crop(&oriented, edit, u32::MAX).into_rgba8();
    imaging::encode_jpeg(&cropped, EXPORT_QUALITY)
}

/// JPEG quality for cropped export renders. Higher than the disposable thumbnail
/// cache — this is a delivered file.
const EXPORT_QUALITY: i32 = 92;

enum CopyError {
    DestExists,
    Io(String),
}

/// Copy `source` to `dest` via `dest.part` → fsync → rename, the same atomic
/// contract as saves: a crash leaves either no dest or the whole file,
/// never a torn one. Refuses to overwrite an existing dest.
fn copy_atomic(source: &Path, dest: &Path) -> Result<(), CopyError> {
    let tmp = part_path(dest);
    fs::copy(source, &tmp).map_err(|e| CopyError::Io(format!("copy {}: {e}", source.display())))?;
    finalize_atomic(&tmp, dest)
}

/// Finish the atomic write of a populated `.part` file: fsync it, re-check the
/// dest one last time (never overwrite), rename into place, then fsync the parent
/// so the rename itself is durable. Shared by the copy and crop-render paths so
/// the safety-critical "never overwrite, never torn" contract can't drift between
/// them.
fn finalize_atomic(tmp: &Path, dest: &Path) -> Result<(), CopyError> {
    if let Ok(file) = File::open(tmp) {
        let _ = file.sync_all();
    }
    if dest.exists() {
        let _ = fs::remove_file(tmp);
        return Err(CopyError::DestExists);
    }
    fs::rename(tmp, dest).map_err(|e| {
        let _ = fs::remove_file(tmp);
        CopyError::Io(format!("rename into {}: {e}", dest.display()))
    })?;
    // Directory fsync makes the rename itself durable; not all platforms permit
    // it, so failure is non-fatal.
    if let Some(parent) = dest.parent()
        && let Ok(handle) = File::open(parent)
    {
        let _ = handle.sync_all();
    }
    Ok(())
}

fn part_path(dest: &Path) -> std::path::PathBuf {
    let mut name = dest.as_os_str().to_os_string();
    name.push(OsString::from(".part"));
    name.into()
}
