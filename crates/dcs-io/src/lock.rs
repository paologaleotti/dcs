//! Single-writer lock for a project (§10b, decision #34). One file in `.dcs/`
//! carries a **timestamp refreshed by the live instance**. A second instance
//! that finds a fresh timestamp opens read-only (the UI offers "Take over"); a
//! timestamp older than the stale window is reclaimed automatically, so a crash
//! never strands the project read-only. There is no PID liveness check — the
//! timestamp *is* the liveness signal.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// How long since the last refresh before a lock is considered abandoned.
pub const DEFAULT_STALE: Duration = Duration::from_secs(300);

const LOCK_FILE: &str = "lock";

/// Whether this instance owns the project for writing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LockOutcome {
    /// We hold the lock — safe to write.
    Acquired,
    /// Another live instance holds it — open read-only until "Take over".
    HeldByOther,
}

/// A held (or observed) project lock. Dropping it releases the lock only if we
/// still own it, so a read-only second instance — or one that was taken over —
/// never deletes another instance's lock.
///
/// The file holds `"<unix_secs> <token>"`. The token is a per-instance value
/// written then read back to settle the acquire race: if two instances both
/// find a stale lock and write at nearly the same moment, the file ends up with
/// one token, and the instance whose token didn't survive demotes itself to
/// read-only. This closes the check-then-write TOCTOU without OS file locks
/// (spec §10b keeps the timestamp as the liveness signal, no PID liveness).
pub struct ProjectLock {
    path: PathBuf,
    token: u64,
    owned: bool,
}

impl ProjectLock {
    /// Acquire the lock in `dir`: granted when the file is absent or stale,
    /// refused (read-only) when a fresh timestamp from a live instance is found.
    pub fn acquire(dir: &Path, stale: Duration) -> (Self, LockOutcome) {
        let path = dir.join(LOCK_FILE);
        let token = make_token();
        if held_by_live_instance(&path, stale) {
            return (ProjectLock { path, token, owned: false }, LockOutcome::HeldByOther);
        }
        // Write our token, then read it back: whoever's write survived owns it.
        let owned = stamp(&path, token).is_ok() && read_token(&path) == Some(token);
        let outcome = if owned {
            LockOutcome::Acquired
        } else {
            LockOutcome::HeldByOther
        };
        (ProjectLock { path, token, owned }, outcome)
    }

    /// True if this instance owns the lock (may write).
    pub fn is_owned(&self) -> bool {
        self.owned
    }

    /// Refresh the timestamp so other instances keep seeing us as live. No-op
    /// when we don't own the lock.
    pub fn refresh(&self) {
        if self.owned {
            let _ = stamp(&self.path, self.token);
        }
    }

    /// Forcibly claim the lock (the UI's "Take over"), stamping it as ours and
    /// verifying our token survived.
    pub fn take_over(&mut self) {
        self.owned = stamp(&self.path, self.token).is_ok() && read_token(&self.path) == Some(self.token);
    }

    /// Release the lock if we still own it — but only when the file still holds
    /// our token, so a peer that took over isn't clobbered. Idempotent.
    pub fn release(&mut self) {
        if self.owned {
            if read_token(&self.path) == Some(self.token) {
                let _ = fs::remove_file(&self.path);
            }
            self.owned = false;
        }
    }
}

impl Drop for ProjectLock {
    fn drop(&mut self) {
        self.release();
    }
}

fn held_by_live_instance(path: &Path, stale: Duration) -> bool {
    match read_stamp(path) {
        Some(ts) => now_secs().saturating_sub(ts) < stale.as_secs(),
        None => false, // absent or unreadable → free to take
    }
}

fn stamp(path: &Path, token: u64) -> io::Result<()> {
    fs::write(path, format!("{} {}", now_secs(), token))
}

/// The timestamp field (first token) of the lock file.
fn read_stamp(path: &Path) -> Option<u64> {
    let contents = fs::read_to_string(path).ok()?;
    contents.split_whitespace().next()?.parse().ok()
}

/// The owner-token field (second token) of the lock file.
fn read_token(path: &Path) -> Option<u64> {
    let contents = fs::read_to_string(path).ok()?;
    contents.split_whitespace().nth(1)?.parse().ok()
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// A value distinct across concurrent live instances: the process id (unique
/// among running processes) mixed with the high-resolution clock.
fn make_token() -> u64 {
    let pid = std::process::id() as u64;
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0);
    (pid << 32) ^ nanos
}
