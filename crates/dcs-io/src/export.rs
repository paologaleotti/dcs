//! The dumb export executor (§6.9). Walks a finished `ExportPlan` on a worker
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
use dcs_domain::export::{ExportOp, ExportPlan, FileRole};

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
    /// A file already exists at the dest — never overwritten (§6.6).
    DestExists,
    /// The source file is gone since planning.
    SourceMissing,
}

/// Live handle to a running export. Poll it for events each frame; cancel stops
/// the worker after the current file (a clean, reported partial state, §6.7).
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
    match copy_atomic(&op.source, &op.dest) {
        Ok(()) => ExportEvent::Copied { role: op.role },
        Err(CopyError::DestExists) => ExportEvent::Skipped {
            reason: SkipKind::DestExists,
        },
        Err(CopyError::Io(msg)) => ExportEvent::Failed { error: msg },
    }
}

enum CopyError {
    DestExists,
    Io(String),
}

/// Copy `source` to `dest` via `dest.part` → fsync → rename, the same atomic
/// contract as saves (§10b): a crash leaves either no dest or the whole file,
/// never a torn one. Refuses to overwrite an existing dest.
fn copy_atomic(source: &Path, dest: &Path) -> Result<(), CopyError> {
    let tmp = part_path(dest);
    fs::copy(source, &tmp).map_err(|e| CopyError::Io(format!("copy {}: {e}", source.display())))?;
    if let Ok(file) = File::open(&tmp) {
        let _ = file.sync_all();
    }
    // Re-check just before the rename: never overwrite (§6.6).
    if dest.exists() {
        let _ = fs::remove_file(&tmp);
        return Err(CopyError::DestExists);
    }
    fs::rename(&tmp, dest).map_err(|e| {
        let _ = fs::remove_file(&tmp);
        CopyError::Io(format!("rename into {}: {e}", dest.display()))
    })
}

fn part_path(dest: &Path) -> std::path::PathBuf {
    let mut name = dest.as_os_str().to_os_string();
    name.push(OsString::from(".part"));
    name.into()
}
