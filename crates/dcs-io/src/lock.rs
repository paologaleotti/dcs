//! Single-writer lock for a project. One file in `.dcs/`
//! carries a **timestamp refreshed by the live instance**. A second instance
//! that finds a fresh timestamp opens read-only (the UI offers "Take over"); a
//! timestamp older than the stale window is reclaimed automatically, so a crash
//! never strands the project read-only. There is no PID liveness check — the
//! timestamp *is* the liveness signal.

use std::cell::Cell;
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
/// The file holds `"<unix_secs> <token>"`, the token a per-instance value.
/// Acquisition is settled by filesystem atomicity, not read-back: a stale lock
/// is reclaimed under an exclusive `lock.claim` marker (`create_new`, so one
/// reclaimer at a time, staleness re-verified inside), and the new lock is
/// *created* by hard-linking a fully written temp into place (the link fails
/// if a lock already exists — unlike rename, which replaces). Two instances
/// can therefore never both believe they own the single-writer lock, without
/// OS file locks; the timestamp stays the liveness signal.
pub struct ProjectLock {
    path: PathBuf,
    token: u64,
    /// `Cell`: `refresh` runs on a shared borrow (the heartbeat) but must be
    /// able to demote ownership when it finds a peer's token in the file.
    owned: Cell<bool>,
}

impl ProjectLock {
    /// Acquire the lock in `dir`: granted when the file is absent or stale,
    /// refused (read-only) when a fresh timestamp from a live instance is found.
    pub fn acquire(dir: &Path, stale: Duration) -> (Self, LockOutcome) {
        let path = dir.join(LOCK_FILE);
        let token = make_token();
        if held_by_live_instance(&path, stale) {
            return (
                ProjectLock {
                    path,
                    token,
                    owned: Cell::new(false),
                },
                LockOutcome::HeldByOther,
            );
        }
        // A stale lock is reclaimed under an exclusive marker so only one of
        // several concurrent reclaimers proceeds; an absent lock goes straight
        // to creation, where hard-link atomicity settles any race.
        let owned = if path.exists() {
            reclaim_stale(&path, token, stale)
        } else {
            create_lock(&path, token)
        };
        let outcome = if owned {
            LockOutcome::Acquired
        } else {
            LockOutcome::HeldByOther
        };
        (
            ProjectLock {
                path,
                token,
                owned: Cell::new(owned),
            },
            outcome,
        )
    }

    /// True if this instance owns the lock (may write).
    pub fn is_owned(&self) -> bool {
        self.owned.get()
    }

    /// Refresh the timestamp so other instances keep seeing us as live. No-op
    /// when we don't own the lock. Finding a peer's token in the file means we
    /// were taken over: demote instead of stamping — stamping would silently
    /// steal the lock back from the new owner.
    pub fn refresh(&self) {
        if !self.owned.get() {
            return;
        }
        if read_token(&self.path) != Some(self.token) {
            self.owned.set(false);
            return;
        }
        let _ = stamp(&self.path, self.token);
    }

    /// Forcibly claim the lock (the UI's "Take over"): move the peer's lock
    /// aside, then create ours atomically. If the peer re-creates its lock in
    /// the gap, our create fails and we honestly stay read-only.
    pub fn take_over(&mut self) {
        claim(&self.path, self.token);
        self.owned.set(create_lock(&self.path, self.token));
    }

    /// Release the lock if we still own it — but only when the file still holds
    /// our token, so a peer that took over isn't clobbered. Idempotent.
    pub fn release(&mut self) {
        if self.owned.get() {
            if read_token(&self.path) == Some(self.token) {
                let _ = fs::remove_file(&self.path);
            }
            self.owned.set(false);
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

/// Reclaim a stale lock: enter the exclusive claim marker (only one reclaimer
/// at a time), re-verify staleness *under* the marker — the lock may have been
/// legitimately recreated since our first look — then remove it and create our
/// own. A fresh lock is never deleted or renamed here.
fn reclaim_stale(path: &Path, token: u64, stale: Duration) -> bool {
    let marker = path.with_extension("claim");
    if !enter_claim(&marker, token, stale) {
        return false;
    }
    let owned = if held_by_live_instance(path, stale) {
        false
    } else {
        let _ = fs::remove_file(path);
        // A racer that saw the lock absent may create in this gap; our link
        // then fails and it wins — either way a single owner.
        create_lock(path, token)
    };
    let _ = fs::remove_file(&marker);
    owned
}

/// Create the claim marker with `create_new` (O_EXCL) — the one true mutual
/// exclusion primitive here. A marker older than the stale window is a crash
/// orphan (its holder never lived long enough to matter); it is claimed by
/// *renaming* it to a token-keyed name — atomic, one winner — never by
/// remove-then-create, which would let one racer delete another's freshly
/// created live marker. Marker age comes from mtime, not content, so a crash
/// mid-create can't produce an unparseable-forever marker.
fn enter_claim(marker: &Path, token: u64, stale: Duration) -> bool {
    let create = || {
        fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(marker)
            .is_ok()
    };
    if create() {
        return true;
    }
    let orphaned = fs::metadata(marker)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.elapsed().ok())
        .is_some_and(|age| age >= stale);
    if orphaned {
        let grave = marker.with_extension(format!("{token}.orphan"));
        if fs::rename(marker, &grave).is_ok() {
            let _ = fs::remove_file(&grave);
            return create();
        }
    }
    false
}

/// Move the lock aside under a token-keyed claim name — the forcible half of
/// "Take over". Rename is atomic, so of two concurrent takers one loses the
/// rename and then fails the create below. The claim file is deleted right
/// away (a crash in between leaves a harmless orphan in `.dcs/`).
fn claim(path: &Path, token: u64) -> bool {
    let claimed = path.with_extension(format!("{token}.claim"));
    let won = fs::rename(path, &claimed).is_ok();
    if won {
        let _ = fs::remove_file(&claimed);
    }
    won
}

/// Atomically create the lock carrying our stamp: write a private temp, then
/// hard-link it into place — the link fails when a lock already exists, unlike
/// rename, which would silently replace a racer's freshly created lock. On
/// filesystems without hard links, degrades to stamp + read-back (the pre-link
/// behavior: a narrow race window, but never a torn file).
fn create_lock(path: &Path, token: u64) -> bool {
    let tmp = path.with_extension(format!("{token}.tmp"));
    if fs::write(&tmp, format!("{} {}", now_secs(), token)).is_err() {
        return false;
    }
    let linked = fs::hard_link(&tmp, path);
    let _ = fs::remove_file(&tmp);
    match linked {
        Ok(()) => true,
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => false,
        Err(_) => stamp(path, token).is_ok() && read_token(path) == Some(token),
    }
}

fn stamp(path: &Path, token: u64) -> io::Result<()> {
    // Write to a per-token temp then rename: rename is atomic, so a concurrent
    // reader never sees a half-written stamp. Used by `refresh`, where we
    // already own the lock and replacing it is the point.
    let tmp = path.with_extension(format!("{token}.tmp"));
    fs::write(&tmp, format!("{} {}", now_secs(), token))?;
    fs::rename(&tmp, path)
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
