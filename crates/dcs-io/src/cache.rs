//! `cache.sqlite3` — the disposable store (§5, §10b). Two contracts, one file:
//!
//! - **Fingerprint pre-filter:** `(path, mtime, size) → ContentFingerprint`, so
//!   a re-scan skips re-hashing files that haven't changed (open Q#8).
//! - **Thumb blobs:** `(content_key, tier) → encoded JPEG`, LRU-evicted under a
//!   byte cap, keyed by content so a renamed file keeps its thumbnails.
//!
//! Disposable by contract: delete the file and it rebuilds; corruption can
//! never cost owned state, so reads degrade to a miss rather than an error.
//! `rusqlite` is hidden here and never leaks above `dcs-io` (CLAUDE.md).

use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use dcs_domain::fingerprint::ContentFingerprint;
use rusqlite::{Connection, OptionalExtension, params};
use thiserror::Error;

/// A cache shared across the scan worker and the decode pool, behind a `Mutex`
/// so the single SQLite connection is reachable from any thread. Locks are held
/// only for fast keyed queries — never across a file read, hash, or image
/// encode/decode (CLAUDE.md threading rules).
pub type SharedCache = Arc<Mutex<SqliteCache>>;

/// Default thumb-blob budget (~512 MB). The cache never exceeds it; the LRU
/// recycles the least-recently-used blobs. Tunable per open.
pub const DEFAULT_THUMB_CAP_BYTES: u64 = 512 * 1024 * 1024;

/// Which decode tier a thumb blob belongs to (§10b). Distinct cache rows so the
/// grid and gallery sizes coexist for one photo.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThumbTier {
    /// ~256 px grid thumbnail.
    Grid,
    /// ~1024 px gallery thumbnail.
    Gallery,
}

/// Errors opening or initializing the cache. Per-row read failures are *not*
/// errors — they degrade to a miss, because the store is disposable.
#[derive(Debug, Error)]
pub enum CacheError {
    #[error("cache database error: {0}")]
    Db(String),
}

/// The fast pre-filter that lets a re-scan avoid re-hashing unchanged files.
pub trait FingerprintCache {
    /// The cached fingerprint for `path`, but only if `(mtime, size)` still
    /// match — otherwise the file changed and must be re-hashed. Any read
    /// failure returns `None`.
    fn lookup(&self, path: &str, mtime: i64, size: u64) -> Option<ContentFingerprint>;

    /// Record (or refresh) the fingerprint for `path` with its current
    /// `(mtime, size)`. Failures are swallowed; the cache is disposable.
    fn store(&self, path: &str, mtime: i64, size: u64, fingerprint: &ContentFingerprint);
}

/// Two-tier thumbnail blob store keyed by content fingerprint.
pub trait ThumbCache {
    /// The encoded thumbnail blob for a photo at a tier, if resident. Marks it
    /// recently used so it survives the next eviction.
    fn get(&self, key: &ContentFingerprint, tier: ThumbTier) -> Option<Vec<u8>>;

    /// Store an encoded thumbnail, then evict the least-recently-used blobs if
    /// the store is over its byte cap.
    fn put(&self, key: &ContentFingerprint, tier: ThumbTier, blob: &[u8]);
}

/// SQLite-backed cache implementing both contracts. A monotonic tick stands in
/// for access time so LRU ordering is deterministic and testable.
pub struct SqliteCache {
    conn: Connection,
    thumb_cap_bytes: u64,
    tick: AtomicU64,
}

impl SqliteCache {
    /// Open (creating if needed) the cache at `path` with the default cap.
    pub fn open(path: &Path) -> Result<Self, CacheError> {
        Self::open_with_cap(path, DEFAULT_THUMB_CAP_BYTES)
    }

    /// Open with an explicit thumb byte cap (used to exercise eviction).
    ///
    /// The cache is disposable: if the file is corrupt and won't open, it is
    /// deleted and rebuilt from scratch once, rather than failing the caller.
    /// A corrupt disposable store must never cost owned state (§10b).
    pub fn open_with_cap(path: &Path, thumb_cap_bytes: u64) -> Result<Self, CacheError> {
        match Self::try_open(path, thumb_cap_bytes) {
            Ok(cache) => Ok(cache),
            Err(_) => {
                remove_db_files(path);
                Self::try_open(path, thumb_cap_bytes)
            }
        }
    }

    /// Open and initialize the schema, with a light sanity query that surfaces
    /// most corruption without the cost of a full `integrity_check`.
    fn try_open(path: &Path, thumb_cap_bytes: u64) -> Result<Self, CacheError> {
        let conn = Connection::open(path).map_err(db)?;
        let cache = Self::from_conn(conn, thumb_cap_bytes)?;
        cache
            .conn
            .query_row("SELECT count(*) FROM sqlite_master", [], |r| {
                r.get::<_, i64>(0)
            })
            .map_err(db)?;
        Ok(cache)
    }

    /// An in-memory cache — used by tests and never touches disk.
    pub fn in_memory(thumb_cap_bytes: u64) -> Result<Self, CacheError> {
        let conn = Connection::open_in_memory().map_err(db)?;
        Self::from_conn(conn, thumb_cap_bytes)
    }

    fn from_conn(conn: Connection, thumb_cap_bytes: u64) -> Result<Self, CacheError> {
        // WAL keeps the scan worker's writes from blocking concurrent reads.
        conn.pragma_update(None, "journal_mode", "WAL")
            .map_err(db)?;
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS fingerprints (
                 path  TEXT PRIMARY KEY,
                 mtime INTEGER NOT NULL,
                 size  INTEGER NOT NULL,
                 key   BLOB NOT NULL
             );
             CREATE TABLE IF NOT EXISTS thumbs (
                 content_key BLOB NOT NULL,
                 tier        INTEGER NOT NULL,
                 blob        BLOB NOT NULL,
                 last_used   INTEGER NOT NULL,
                 PRIMARY KEY (content_key, tier)
             );
             CREATE INDEX IF NOT EXISTS thumbs_lru ON thumbs (last_used);",
        )
        .map_err(db)?;
        Ok(SqliteCache {
            conn,
            thumb_cap_bytes,
            tick: AtomicU64::new(1),
        })
    }

    /// Total bytes currently held by thumb blobs (diagnostics + eviction).
    pub fn thumb_bytes(&self) -> u64 {
        self.conn
            .query_row(
                "SELECT COALESCE(SUM(LENGTH(blob)), 0) FROM thumbs",
                [],
                |r| r.get::<_, i64>(0),
            )
            .map(|n| n.max(0) as u64)
            .unwrap_or(0)
    }

    fn next_tick(&self) -> i64 {
        self.tick.fetch_add(1, Ordering::Relaxed) as i64
    }

    /// Delete least-recently-used blobs until total bytes are within the cap.
    fn evict_to_cap(&self) {
        while self.thumb_bytes() > self.thumb_cap_bytes {
            let deleted = self
                .conn
                .execute(
                    "DELETE FROM thumbs WHERE rowid = (
                         SELECT rowid FROM thumbs ORDER BY last_used ASC LIMIT 1
                     )",
                    [],
                )
                .unwrap_or(0);
            if deleted == 0 {
                break; // empty table but still over cap (cap smaller than one blob)
            }
        }
    }
}

impl FingerprintCache for SqliteCache {
    fn lookup(&self, path: &str, mtime: i64, size: u64) -> Option<ContentFingerprint> {
        let row: Option<(i64, i64, Vec<u8>)> = self
            .conn
            .query_row(
                "SELECT mtime, size, key FROM fingerprints WHERE path = ?1",
                params![path],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .optional()
            .ok()
            .flatten();
        let (cached_mtime, cached_size, key) = row?;
        if cached_mtime != mtime || cached_size as u64 != size {
            return None; // file changed since we hashed it
        }
        let bytes: [u8; 32] = key.try_into().ok()?;
        Some(ContentFingerprint::from_bytes(bytes))
    }

    fn store(&self, path: &str, mtime: i64, size: u64, fingerprint: &ContentFingerprint) {
        let _ = self.conn.execute(
            "INSERT INTO fingerprints (path, mtime, size, key) VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(path) DO UPDATE SET mtime = ?2, size = ?3, key = ?4",
            params![path, mtime, size as i64, fingerprint.as_bytes().as_slice()],
        );
    }
}

impl ThumbCache for SqliteCache {
    fn get(&self, key: &ContentFingerprint, tier: ThumbTier) -> Option<Vec<u8>> {
        let tier = tier_code(tier);
        let blob: Option<Vec<u8>> = self
            .conn
            .query_row(
                "SELECT blob FROM thumbs WHERE content_key = ?1 AND tier = ?2",
                params![key.as_bytes().as_slice(), tier],
                |r| r.get(0),
            )
            .optional()
            .ok()
            .flatten();
        if blob.is_some() {
            let _ = self.conn.execute(
                "UPDATE thumbs SET last_used = ?3 WHERE content_key = ?1 AND tier = ?2",
                params![key.as_bytes().as_slice(), tier, self.next_tick()],
            );
        }
        blob
    }

    fn put(&self, key: &ContentFingerprint, tier: ThumbTier, blob: &[u8]) {
        let stored = self.conn.execute(
            "INSERT INTO thumbs (content_key, tier, blob, last_used) VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(content_key, tier) DO UPDATE SET blob = ?3, last_used = ?4",
            params![
                key.as_bytes().as_slice(),
                tier_code(tier),
                blob,
                self.next_tick()
            ],
        );
        if stored.is_ok() {
            self.evict_to_cap();
        }
    }
}

fn tier_code(tier: ThumbTier) -> i64 {
    match tier {
        ThumbTier::Grid => 0,
        ThumbTier::Gallery => 1,
    }
}

fn db(e: rusqlite::Error) -> CacheError {
    CacheError::Db(e.to_string())
}

/// Delete the SQLite database and its WAL sidecars, so a fresh one can be
/// created. Best-effort: a missing file is not an error.
fn remove_db_files(path: &Path) {
    let _ = std::fs::remove_file(path);
    for suffix in ["-wal", "-shm"] {
        if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
            let _ = std::fs::remove_file(path.with_file_name(format!("{name}{suffix}")));
        }
    }
}
