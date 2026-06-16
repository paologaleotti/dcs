# Persistence Phase

Build order step 3 + the durable-undo half of step 4 (spec §13). Anchored to
spec §4, §5, §9, §10, §10b, §11; decisions #18, #33, #34; open Q#8, Q#9.

This phase gives the app its memory: file identity that survives renames, the
precious `project.json`, the durable `undo.log`, and the disposable
`cache.sqlite3`. After it, you can cull a folder, quit, reopen, and find your
verdicts and undo history intact — even if files were renamed on disk.

## Decisions taken (locked before writing this)

1. **Runtime stays `PhotoId`-keyed; persistence reconciles via fingerprint.**
   `Cull`, `Command`, and `undo.log` never change their key type. `project.json`
   stores per-photo records `{id, fingerprint, verdict}`; on load/re-scan the
   persistence layer matches by fingerprint to hand the old `PhotoId` back.
   Resolves the `SEAM(#33)` comment in `cull.rs`: there is **no** literal
   re-key — the reconciliation is one translation seam at load/save plus a
   seeded `PoolBuilder`. (That comment must be corrected, see Cleanup.)
2. **Fingerprint = `blake3(head[64K] ‖ tail[64K] ‖ le_bytes(size))`** with an
   `(mtime, size)` pre-filter from `cache.sqlite3` to skip re-hashing unchanged
   files (open Q#8). Runs on the scan worker, never the frame path.
3. **`undo.log` is loaded, never replayed onto state** (open Q#9). `project.json`
   is authoritative for verdicts; the log is folded only to reconstruct the
   undo + redo stacks. Losing the log costs history, never owned state (§10b).
4. **`cache.sqlite3` is built in full this phase**: a fingerprint table (the
   pre-filter) and the two-tier thumb-blob cache with LRU eviction (§10b).

### Defaults (reversible, not blocking)

- A photo's identity fingerprint is its **display file's** (JPEG preferred, else
  RAW) — the same key the thumb cache uses for that photo.
- **Out of scope, explicit follow-up:** lock file / read-only takeover (#34),
  missing-file placeholders + reanimation (§4), the rename re-link *UI* (v1.1).
  The identity *keying* that makes re-link possible ships here; the UI does not.

## Identity model — two concepts, kept distinct

| Concept | Type | Role | Lifetime |
|---|---|---|---|
| Stable id | `PhotoId(u32)` | what commands, `undo.log`, and (later) board positions ride | persisted monotonic counter, never reused (§10b) |
| File identity | `ContentFingerprint` | matches a file across rename-in-place | recomputed from bytes, cached by `(mtime,size)` |

The bridge: `project.json` stores both per photo. On open and on re-scan the app
seeds the `PoolBuilder` with the known `fingerprint → PhotoId` map and the saved
`next_id`; a file whose fingerprint is known reclaims its id (and the app
restores its verdict), a genuinely new fingerprint gets a fresh id. A
renamed-in-place file gets a new pairing key, forms a "new" photo slot, but its
fingerprint match restores id + verdict — so it never returns as a blank
(spec §4, decision #33).

## Layering

Arrows down only (CLAUDE.md). Each store hides its infrastructure type behind a
trait; `rusqlite`, `serde_json`, and `blake3` never leak above `dcs-io`.

```
dcs-app    Session: load on open (seed builder + Cull + stacks), save on dirty,
           mirror each cull op into the log.            → dcs-io, dcs-domain
dcs-io     persistence (project.json) · undo_log · cache (sqlite) · source (now
           fingerprints).  Versioned DTOs live here.    → dcs-domain
dcs-domain ContentFingerprint type; Photo/ScannedFile gain the field; PoolBuilder
           gains seeding.  No I/O.                       → nothing
```

## dcs-domain changes

- **`fingerprint.rs`** (new, re-exported from `lib.rs`):
  `pub struct ContentFingerprint([u8; 32])` — `Copy, Eq, Hash`, serde as a lower
  hex string. The *value type* only; computation is `dcs-io`'s job.
- **`Photo`** gains `pub fingerprint: ContentFingerprint` (§11).
- **`ScannedFile`** gains `pub fingerprint: ContentFingerprint` (per file).
- **Pairing** (`new_photo`/`merge_file`): a photo's fingerprint is its display
  file's — set from the JPEG when present, else the RAW; when a RAW later joins
  a JPEG photo the JPEG's fingerprint stays authoritative.
- **`PoolBuilder` seeding** — a pure constructor fed by the app from loaded
  state, keeping the builder I/O-free:
  ```rust
  pub fn seeded(known: HashMap<ContentFingerprint, PhotoId>, next_id: u32) -> Self
  ```
  In `add`, when creating a new photo, look up the file's fingerprint in `known`:
  hit → reuse that `PhotoId` (don't bump `next_id`); miss → assign `next_id` and
  increment. Default `PoolBuilder::default()` stays the empty seed (next_id 0),
  so the open-folder path is unchanged when no project exists.

Edge to document in code: if a JPEG and a RAW that were previously two
RAW-only/JPEG-only photos pair into one `Both` photo, identity follows the
display (JPEG) fingerprint; the other file's old id orphans. Acceptable, noted.

## dcs-io::persistence — `project.json`

- **Versioned DTO envelope** (`ProjectDto`) owns the JSON shape and the upgrade
  seam (§10b). Domain types (`PhotoId`, `AcceptState`, `ContentFingerprint`) are
  referenced directly where stable; everything version-fragile is a local DTO.
  ```
  { "version": 1,
    "photos": [ { "id", "fingerprint", "verdict" /* tags later */ } ],
    "next_id": u32,
    "views":  [ <raw JSON value> ],   // unknown ViewKind preserved verbatim
    "config": { /* reserved; minimal this phase */ } }
  ```
- **Unknown-`ViewKind` preservation:** `views` is stored as `Vec<serde_json::Value>`.
  Known kinds (`Grid`) are parsed on use; unknown kinds round-trip untouched on
  save — guarantees forward-compat without an exhaustive enum (spec §9b, open Q#6).
  This phase writes one default `Grid` view; the array shape is what matters.
- **Atomic save** (§10b): `atomic_write(path, bytes)` = write `path.tmp` → fsync
  file → fsync dir → rename over `path`; rotate the previous `project.json` to
  `project.json.bak` first. A crash leaves old or new, never torn.
- **`trait ProjectStore`** (`load`, `save`) with a concrete `JsonProjectStore`.
  Returns a `ProjectSnapshot` to the app: the `fingerprint → PhotoId` map, the
  `PhotoId → verdict` map, `next_id`, raw views, config.
- **`PersistError`** (thiserror, this module): `Io`, `Serialize`, `Deserialize`,
  `UnsupportedVersion(u32)`, `Corrupt(String)`. No bare `io::Error` escapes; the
  domain never sees a path-as-error (CLAUDE.md).

## dcs-io::undo_log — `undo.log`

Append-only, cheap per keystroke, folded (not replayed) on open (decision #18,
open Q#9).

- **Record framing — JSONL, three record kinds:**
  ```
  Do { changes: Vec<(PhotoId, AcceptState, AcceptState)> }   // before, after
  Undo
  Redo
  ```
  `Do` appends on each recorded dispatch (and clears redo, mirroring `Cull`);
  `Undo`/`Redo` append a one-line marker on each stack move. Append-only keeps
  writes O(1); no rewrite on every keystroke.
- **Fold on open** reconstructs the two stacks exactly — `Do` pushes to undo &
  clears redo, `Undo` moves undo→redo, `Redo` moves redo→undo — and **touches no
  verdict state**. State comes from `project.json`.
- **Compaction at save:** rewrite the log to the canonical undo + redo stacks
  (drop the marker churn), trimming the undo side to an entry cap. Atomic write,
  same contract as `project.json`.
- **`LogRecord` DTO lives here** (not in `dcs-app`, which is above `dcs-io`).
  `dcs-app` converts its private `UndoEntry` ↔ the `changes` vec.

## dcs-io::cache — `cache.sqlite3`

Disposable; corruption can never cost owned state (§5, §10b). `rusqlite`
(`bundled`) hidden behind traits; in-memory (`:memory:`) for tests.

- `fingerprints(path TEXT PRIMARY KEY, mtime INTEGER, size INTEGER, key BLOB)` —
  the pre-filter: scan looks up by relative path; `(mtime,size)` match → reuse
  `key`, else recompute and upsert.
- `thumbs(content_key BLOB, tier INTEGER, blob BLOB, last_used INTEGER,
  PRIMARY KEY(content_key, tier))` — encoded-JPEG thumbnails keyed by
  fingerprint, two tiers (~256 grid, ~1024 gallery), LRU eviction by `last_used`
  under a size cap.
- Traits: `FingerprintCache` (lookup/upsert) and `ThumbCache` (get/put/evict).
  Integration tail: `Session`'s decode path checks `ThumbCache` before
  dispatching a decode and populates it after — decoded RGBA is JPEG-encoded
  (turbojpeg, already a dep) for the blob, decoded back on read.

## dcs-io::source — fingerprint on scan

The parallel meta pass already reads EXIF per file; it now also computes the
fingerprint. Order: stat for `(mtime,size)` → `FingerprintCache` lookup →
hit reuses the key, miss reads head+tail+size, blake3-hashes, upserts. The
`ScannedFile` carries the resulting `ContentFingerprint`. Still off the UI thread.

## dcs-app wiring

- **`Cull` constructors + dispatch return value.** Add
  `Cull::from_state(verdicts, undo_stack, redo_stack)` (seed from loaded data) and
  make `dispatch` return the recorded `&UndoEntry` (or `None` on a no-op) so the
  `Session` can mirror it into the log. `Cull` stays pure RAM; the log is the
  `Session`'s concern.
- **`Session::open_folder`** now: locate `.dcs/`; if `project.json` exists, load
  the snapshot → seed `PoolBuilder` (`fingerprint→id`, `next_id`), seed `Cull`
  verdicts, fold `undo.log` into the `Cull` stacks; open `cache.sqlite3`. No
  project → today's empty path.
- **On cull op:** after `cull.dispatch`/`undo`/`redo`, append the matching
  `LogRecord` and `mark_dirty()`.
- **Save:** `save_if_dirty()` writes `project.json` (verdicts + photo
  fingerprints + the default view + config) atomically and compacts `undo.log`.
  Trigger wiring (debounce interval, quit hook) is a `dcs-ui` integration tail;
  the machinery and an explicit `save()` land here.

## Cleanup

- Correct the `SEAM(#33)` comment in `crates/dcs-app/src/cull.rs:48` — the
  persistence layer reconciles by fingerprint at load/save; `Cull` stays
  `PhotoId`-keyed (decision 1 above).
- Spec §12 open Q#8 and Q#9 are resolved by this phase; update the spec when it
  lands (don't silently — note in the PR).

## Tests (non-negotiable, CLAUDE.md)

- **domain:** `PoolBuilder::seeded` — renamed file reclaims its id; new
  fingerprint gets fresh id; `next_id` not bumped on a reclaim; both-file photo
  takes the JPEG fingerprint. `ContentFingerprint` hex serde round-trip.
- **persistence:** `ProjectDto` round-trip including **unknown-`ViewKind`
  preservation** (raw value survives); `UnsupportedVersion` rejected; atomic
  write leaves old-or-new under a simulated mid-write failure; `.bak` rotation.
- **undo_log:** append → fold reconstructs undo + redo stacks exactly;
  compaction trims to the cap; **fold does not mutate verdict state** (the
  load-not-replay guarantee).
- **cache (in-memory SQLite):** fingerprint pre-filter hit vs miss; thumb
  put/get round-trip; LRU eviction under the size cap.
- **app (round-trips):** cull → save → reopen restores verdicts;
  rename-in-place (new path, same fingerprint) keeps the verdict; undo survives
  reopen and reverses correctly; `PhotoId` dedup before undo entries still holds
  (spec decision #10).

## Phase build order

1. domain — `ContentFingerprint`, `Photo`/`ScannedFile` field, `PoolBuilder::seeded` (+ tests).
2. `dcs-io::cache` — sqlite schema, `FingerprintCache` + `ThumbCache` traits, in-mem tests.
3. `dcs-io::source` — blake3 head+tail+size + `(mtime,size)` pre-filter via the cache.
4. `dcs-io::persistence` — `ProjectDto`, atomic write + `.bak`, `ProjectStore`, version guard (+ tests).
5. `dcs-io::undo_log` — JSONL `Do`/`Undo`/`Redo`, append/compact/fold (+ tests).
6. `dcs-app` — `Cull` constructors + dispatch return; `Session` load/save/reconcile + log + cache wiring (+ round-trip tests).

`cargo build --workspace && cargo clippy --workspace -- -D warnings &&
cargo test --workspace` green before done.
