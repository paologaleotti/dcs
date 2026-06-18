//! `undo.log` — the durable command log. Undo/redo survive
//! quit and reopen; the promptless design leans on this. Append-only and cheap
//! per keystroke; compacted to the canonical stacks at save and bounded by an
//! entry cap. Corruption costs only history, never owned state.
//!
//! **Loaded, never replayed.** `project.json` is authoritative for
//! verdict state. On open the log is *folded* only to reconstruct the undo and
//! redo stacks — its records are never re-applied to state, so the two stores
//! can't double-count.
//!
//! Framing is JSON-lines: one `LogRecord` per line. `Do`/`DoTag` carry a
//! command's deltas (verdict or tag); `Undo`/`Redo` are one-line cursor moves.
//! Folding the records in order reproduces the stacks exactly:
//!   - `Do`/`DoTag` → push to undo, clear redo
//!   - `Undo`       → move the top of undo onto redo
//!   - `Redo`       → move the top of redo onto undo
//!
//! **Append-only framing.** `Do` (verdict) predates tags; it is kept verbatim so
//! existing logs still replay. `DoTag` is the additive tag record.

use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};

use dcs_domain::command::TagDelta;
use serde::{Deserialize, Serialize};
use thiserror::Error;

pub use dcs_domain::command::{Patch, VerdictChange};

/// Undo entries kept after compaction. The live in-RAM stack is capped
/// separately; this bounds the on-disk log so it can't grow without end.
pub const DEFAULT_ENTRY_CAP: usize = 1000;

/// Errors appending to or reading the log. History only — never fatal to state.
#[derive(Debug, Error)]
pub enum LogError {
    #[error("undo log i/o error: {0}")]
    Io(#[from] std::io::Error),
    #[error("undo log json error: {0}")]
    Json(#[from] serde_json::Error),
}

/// The undo + redo stacks reconstructed from the log. Each entry is one
/// command's reversible [`Patch`]; the last element of each vec is the stack top.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Stacks {
    pub undo: Vec<Patch>,
    pub redo: Vec<Patch>,
}

/// A single append-only log line.
#[derive(Debug, Clone, Serialize, Deserialize)]
enum LogRecord {
    /// Verdict deltas (predates tags; kept for back-compat replay).
    Do {
        changes: Vec<VerdictChange>,
    },
    /// Tag deltas.
    DoTag {
        deltas: Vec<TagDelta>,
    },
    Undo,
    Redo,
}

/// An open, append-only handle to `undo.log`.
pub struct UndoLog {
    file: File,
}

impl UndoLog {
    /// Open (creating if needed) the log at `path` for appending.
    pub fn open(path: &Path) -> Result<Self, LogError> {
        let file = OpenOptions::new().create(true).append(true).open(path)?;
        Ok(UndoLog { file })
    }

    /// Record one applied command's patch (and the implied redo-clear).
    pub fn record_patch(&mut self, patch: &Patch) -> Result<(), LogError> {
        self.append(&record_of(patch))
    }

    /// Record an undo cursor move.
    pub fn record_undo(&mut self) -> Result<(), LogError> {
        self.append(&LogRecord::Undo)
    }

    /// Record a redo cursor move.
    pub fn record_redo(&mut self) -> Result<(), LogError> {
        self.append(&LogRecord::Redo)
    }

    fn append(&mut self, record: &LogRecord) -> Result<(), LogError> {
        let mut line = serde_json::to_vec(record)?;
        line.push(b'\n');
        self.file.write_all(&line)?;
        Ok(())
    }
}

/// Fold the log into its undo + redo stacks. A truncated or corrupt trailing
/// line is ignored (the cost of a crash mid-append is one lost entry, never a
/// failed open). Returns empty stacks when the file doesn't exist.
pub fn load(path: &Path) -> Result<Stacks, LogError> {
    let file = match File::open(path) {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Stacks::default()),
        Err(e) => return Err(e.into()),
    };
    let mut stacks = Stacks::default();
    for line in BufReader::new(file).lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        // A partial final line (interrupted append) simply doesn't parse; skip it.
        let Ok(record) = serde_json::from_str::<LogRecord>(&line) else {
            continue;
        };
        match record {
            LogRecord::Do { changes } => {
                stacks.undo.push(Patch::Verdict(changes));
                stacks.redo.clear();
            }
            LogRecord::DoTag { deltas } => {
                stacks.undo.push(Patch::Tag(deltas));
                stacks.redo.clear();
            }
            LogRecord::Undo => {
                if let Some(entry) = stacks.undo.pop() {
                    stacks.redo.push(entry);
                }
            }
            LogRecord::Redo => {
                if let Some(entry) = stacks.redo.pop() {
                    stacks.undo.push(entry);
                }
            }
        }
    }
    Ok(stacks)
}

/// Rewrite the log to the canonical record sequence for `stacks`, trimming the
/// undo side to the newest `cap` entries. Written atomically (tmp → rename) so
/// a crash during compaction can't lose the live history.
///
/// Encoding the redo stack append-only: replay the undo entries as `Do`s, then
/// the redo entries (newest-first) as `Do`s followed by one `Undo` each, which
/// folds them back onto the redo side in the right order.
pub fn compact(path: &Path, stacks: &Stacks, cap: usize) -> Result<(), LogError> {
    let undo = trim_oldest(&stacks.undo, cap);
    let mut out = Vec::new();
    for patch in undo {
        write_record(&mut out, &record_of(patch))?;
    }
    for patch in stacks.redo.iter().rev() {
        write_record(&mut out, &record_of(patch))?;
    }
    for _ in 0..stacks.redo.len() {
        write_record(&mut out, &LogRecord::Undo)?;
    }
    atomic_write(path, &out)
}

/// The log record for one patch: `Do` for verdict, `DoTag` for tag.
fn record_of(patch: &Patch) -> LogRecord {
    match patch {
        Patch::Verdict(changes) => LogRecord::Do {
            changes: changes.clone(),
        },
        Patch::Tag(deltas) => LogRecord::DoTag {
            deltas: deltas.clone(),
        },
    }
}

/// The newest `cap` entries of a stack (drop the oldest when over the cap).
fn trim_oldest(stack: &[Patch], cap: usize) -> &[Patch] {
    if stack.len() > cap {
        &stack[stack.len() - cap..]
    } else {
        stack
    }
}

fn write_record(out: &mut Vec<u8>, record: &LogRecord) -> Result<(), LogError> {
    serde_json::to_writer(&mut *out, record)?;
    out.push(b'\n');
    Ok(())
}

fn atomic_write(path: &Path, bytes: &[u8]) -> Result<(), LogError> {
    let tmp: PathBuf = path.with_extension("log.tmp");
    {
        let mut file = File::create(&tmp)?;
        file.write_all(bytes)?;
        file.sync_all()?;
    }
    std::fs::rename(&tmp, path)?;
    if let Some(dir) = path.parent()
        && let Ok(handle) = File::open(dir)
    {
        let _ = handle.sync_all();
    }
    Ok(())
}
