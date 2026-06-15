# dcs — Digital Contact Sheet · Specification v8.1

Changes from v7: review fixes folded into the decisions log and open
questions (§8, §12); export dialog expanded into a flexible engine (§6);
rejected-photo affordances added.
Changes from v8: export logic split into a **pure planner in
`dcs-domain`** (`plan_export` → `ExportPlan`) executed by a dumb
`dcs-io` runner (§6.9, §9, §11); `dcs-domain` error-type ownership made
explicit (§9); decision #36.

---

# PART A — FUNCTIONAL SPEC

## 1. Thesis

A fast, minimal, powerful **digital contact sheet** — closer to an analog
contact sheet than to a photo editor. The grouped, tagged sheet *is* the
product. Target user: artist over retoucher; back from a trip with 3,000
Fuji JPEGs; wants to scan, cull, and organize without Lightroom.

**Non-negotiables:**

1. **Sustained 60 fps, always** — scrolling 3,000 photos, regrouping,
   filtering. "Fast" is a number, enforced by a benchmark from day one.
2. **Keyboard-first.** Remappable keys (config file), multi-select,
   batch ops, command palette.
3. **Non-destructive.** Originals never modified; all state in a
   human-readable project file.
4. **Promptless + undoable.** No confirmation dialogs (export excepted);
   every mutation undoable. *Prompts confirm nothing; rules govern
   everything; undo reverses anything.*

**Anti-features (v1):** dcs never deletes files (rejecting is metadata;
disk cleanup happens outside the app, on purpose — but the app gives
rejects an *exit*, see §6.5). Undo is **durable across sessions**
(persisted command log, §5). No develop pipeline, no multi-folder
catalog, no cloud, no manual grid arrangement (freeform = the future
board view).

## 2. Mental model

### 2.1 Photo = logical unit

`DSCF1234.JPG` + `DSCF1234.RAF` = one photo (`Both`); either alone =
`Jpeg`/`Raw`. Cull and tag **photos**; export **files**. Display prefers
the JPEG; RAW-only shows the embedded preview (no raw decode in v1).
`R` badge on raw-only cells, hidden below a zoom threshold.

### 2.2 Derived vs owned — the load-bearing rule

- **Owned (persisted):** verdicts, tags + assignments, views. User
  intent; mutations; undoable.
- **Derived (never persisted):** grouping, bursts, sort, titles, counts.
  Computed from metadata + settings; changing them is a display setting —
  instant, safe, nothing to confirm or undo.
- **Crystallization:** `F2` / *tag all members* on any derived structure
  (a day, a burst) converts it into an owned tag.

### 2.3 One pool, one grid, three settings

- **Group by:** `time` | `tag` | `none`. One active. (GPS axis deferred;
  coordinates are still read at import.) `none` = one unsegmented stream.
- **Sort:** explicit key + direction, always visible — `time` or `name`,
  `↑`/`↓`; default `time ↑`. Within groups; group order by earliest
  member; leftover group always last. **Zero-timestamp folders** default
  to `name ↑` + axis `none` — fully usable with no dates at all.
- **Zoom:** thumbnail density.

No drill-in, no breadcrumb, no nesting. Focus = the solo filter, never a
place.

### 2.4 Time

- **Shoot timezone:** project-level IANA zone (offset derived per photo,
  so DST mid-trip survives — see decision #7 and open question #5)
  applied to all derivation (cameras get left on home time). Set from
  settings or the palette; the sheet regroups live; EXIF untouched;
  default = system zone.
- **Granularity:** `auto` | `smart day` | `hour` | `day` | `week`.
  `smart day` = neutral time-of-day buckets by default (early · morning ·
  midday · afternoon · late afternoon · evening · night); the
  evening/night boundary attributes night to the evening's date; empty
  buckets hidden. Evocative labels ("golden hour") are opt-in and, when
  enabled, derived from GPS+date rather than fixed clock hours (see
  decision #6). `auto` resolves from the data (single day → smart day,
  multi-day → day) and shows it: `groups: auto (day)`.
- Day titles always carry the date (`Day 2 · 11/05/25`) — numbers are
  relative and may renumber; the date is the anchor. `No date` last.

### 2.5 Bursts — derived, not tags

A burst = a maximal run of frames shot in rapid succession. Recomputed,
never stored, never in the tag namespace.

Rule: order by adjusted time (EXIF subseconds when present; filename
trailing digits break intra-second ties — ordering only, no run
splitting); gap ≤ `2.0 s` joins a run (equal-second runs without
subseconds = gap 0); ≥ `3` frames qualifies; optional max-duration knob
(off) stops timelapses. Knobs are display settings; adjusting re-derives
instantly with the burst count shown live.

Rendering (`time`/`none`): a span in the single neutral burst accent
across the run, label on the first cell, segmented at group boundaries;
members packed at **half gap** (burst bricks). Cull a burst by accepting
the keeper and rejecting the rest; keep one as meaningful by
crystallizing it.

### 2.6 Thumbnail geometry

Uniform **square cells**, thumbnails undistorted: contain-fit, centered
(portrait gets side margins) — the Lightroom convention. **No crop, no
masonry, no adaptive cells.** EXIF orientation applied automatically.
Gap ≈ 10% of cell size (min 4 px); burst bricks at half gap.

### 2.7 Tags

A tag = `{name, color}`, many-to-many, the only persisted structure; all
user-created. **Color tags** on `1–9` are built-in tags — one color
system: every color on the sheet is a tag color. Accept/reject is a
verdict, not a tag.

Hygiene: the palette **never creates silently** (fuzzy matches first,
explicit "create new…" row last). **Merge = rename-to-existing** (the
full manager panel is deferred). Empty tags never render. Non-contiguous
tags render as bottom-edge strips (max 2, `+n` on hover).

### 2.8 Grouping axes

- **time** — §2.4.
- **tag** — one band per non-empty tag, ordered by earliest member.
  Multi-tagged photos appear in every band as **projections of one
  photo**: select one, all highlight; state reflects everywhere; batch
  ops hit once; counts are unique. `Untagged` last.
- **none** — one stream.
- Headers: quiet for derived groups, tag-colored for tag bands.
  Controls: collapse (cover = first accepted, else first), **select
  members** (the one exception that includes hidden/collapsed), solo,
  `12 of 46` filtered counts, `F2` = tag all members.

### 2.9 Culling & filters

- **Accept / Reject / Unreviewed** on `A`/`X` (toggle back). Rejected =
  dimmed + glyph, never deleted. **Unreviewed is the working filter.**
- Chips: state + tags. **Each chip group combines with AND across groups,
  OR within a group** — e.g. `(accepted) AND (temples OR shrines)`. A
  per-chip-group AND/OR toggle is visible; default is the common case
  above. Same rules drive export scope.
- **Solo** = filter to one group/tag; other headers stay as muted
  ghosts. Active filters always visible as removable chips.
- **Esc never clears filters.** Filters are dismissed only by explicit
  chip-X (see §2.12).

### 2.10 Command surfaces

One **command registry**, three surfaces:

1. **Keys** (`?` overlay): `←↑→↓` move focus · `Shift+arrows` extend
   selection · `Space` open focused photo in gallery · `A`/`X` ·
   `1–9` colors · `T`/`Shift+T` tag/untag · `S` solo · `F` gallery ·
   `Z` 1:1 · `F2` crystallize/rename · `Ctrl+A` select visible ·
   `Ctrl+Z`/`+Shift` undo/redo · `Esc`.
2. **Palette `Cmd/Ctrl+P`:** fuzzy over every command; rows show
   bindings; acts on the selection.
3. **Context menus on two surfaces only:** photo/selection and headers.
   Menus mirror the registry — nothing exists only in a menu or only on
   a key.

### 2.11 Cell anatomy — fixed layer budget

Background = burst span · outline = selection · top-left = RAW badge
(zoom-gated) · bottom-right = verdict glyph · bottom edge = tag strips ·
hover/status bar = everything by name. Color is never the sole identity
channel.

### 2.12 Selection & Esc

**Visible-only rule:** batch ops and `Ctrl+A` touch visible photos only
(header select-members is the exception; tag projections count once).
**Esc order:** palette/rename → gallery → selection → solo. **Filters
are not in the Esc chain** — they persist until a chip is explicitly
removed, so over-pressing Esc can never destroy a built-up filter set.

### 2.13 View modes: grid + gallery

A **view-mode switcher** lives in the top bar — `GRID | GALLERY` in v1;
future modes (board) join it there. Gallery is a mode, not only an
overlay: switching to it opens on the focused photo (else the first
visible).

- **Grid** to scan: virtualized, zoomable, **fully arrow-navigable**.
  A focus cursor (one cell, marked like the selection outline) moves
  with `←→` through the visible order and `↑↓` by visual row; the
  cursor is the selection anchor; a plain arrow selects the focused
  photo, `Shift+arrow` extends the range, and prefetch follows the
  cursor like it follows scroll. **`Space` opens the focused photo in
  the gallery.**
- **Gallery** to judge: `Space`/`F`/double-click/switcher in, arrows
  traverse the
  **visible grid order**, full key parity (`A` `X` `T` `1–9` `Z`, undo).
  **`Z` toggles fit ↔ 1:1 at cursor** (no pan-lock, no compare — by
  design). **Filmstrip** docked below: thumbs of the visible order,
  current centered, auto-scrolls, click-to-jump, verdict glyphs + burst
  spans visible, collapsible. Corner caption: name, adjusted time, type,
  group, tags.

Three orthogonal controls: zoom (view) · group/tag (structure) ·
gallery (judging). Resist a fourth.

## 3. UI design language — the analog contact sheet

The reference is a **film contact sheet on a light table**: photographs
on near-black, grease-pencil marks, frame-edge codes. Professional
darkroom tool, not a web app.

- **Dark, neutral, photo-first.** Near-black charcoal surfaces
  (neutral gray scale — no blue-tinted darks, they shift color
  perception). Thumbnails are the only bright, colorful objects; chrome
  recedes to grays. Background of empty cell area slightly distinct from
  chrome so the "sheet" reads as a surface.
- **Square. Everything.** Border radius 0 on every element — cells,
  buttons, chips, inputs, menus, dialogs. No drop shadows, no gradients,
  no glass, no glow. Separation by 1 px hairlines in low-contrast gray
  and by spacing, nothing else.
- **Typography does the chrome.** Small and dense. **Monospace for all
  data** — counts, times, filenames, key hints, group sublabels — the
  way frame numbers print on a film rebate. One plain sans for names
  (tags, groups). Uppercase micro-labels for section headers. No icon
  fonts where a short mono word works (`solo`, `select`, `12 of 46`).
- **Color = meaning, never decoration.** The only colors in the entire
  UI: tag colors, the verdict green/red marks, the burst accent. All
  buttons, chips, and controls are grayscale; an active control is a
  brighter gray, not a brand blue.
- **Selection is a grease pencil mark:** a plain light rectangular
  outline, 1–2 px, no glow, no fill tint.
- **Compact and information-dense.** Tight paddings, small controls,
  a status bar that always talks (visible/selected counts, last action,
  key hints) — Photo Mechanic / Capture One density, not consumer-app
  whitespace.
- **No motion.** State changes are instant; no animated transitions, no
  easing, no skeleton shimmer. The only "animation" is thumbnails
  resolving from embedded preview to decoded image.
- Group headers read like **edge annotations**: a thin rule, small mono
  sublabel (`428 · group`), name in sans. Tag bands get a 2 px color
  rule, nothing more.

## 4. Entry, import, files on disk

- **Entry:** an empty window with one action — **Open folder…** (or
  drag a folder in). Recents/reopen-last: deferred.
- Read-only import; basename pairing; one EXIF pass (time + subseconds
  + GPS + orientation). A **content fingerprint** is computed per file at
  import (see §5) — identity is keyed on it from day one, even though the
  rename re-link *UI* is v1.1. **Progressive:** the grid appears
  immediately and fills as the scan streams; culling starts before it
  finishes.
- **Re-scan** adds new photos, never duplicates known ones. Because
  identity is fingerprint-keyed, a file renamed-in-place keeps its
  verdicts and tags instead of returning as a blank new photo.
- **Missing files:** placeholder cell + `missing` badge, state
  preserved, reported; reanimates if the file returns (matched by
  fingerprint, so owned state is restored even under a new name).
  **Unreadable files:** same treatment; a scan never aborts.

## 5. Project sidecar — `.dcs/`

One hidden `.dcs/` directory at the imported root (subfolders covered):

- **`project.json`** — the precious store: verdicts, tags, views,
  config. Human-readable, diffable, **relative paths** (the folder is
  portable). Nothing derived is ever stored.
- **`undo.log`** — the durable command log. Undo/redo survive quit and
  reopen; the promptless design leans on this (decision #18). Append-only,
  compacted at save, bounded by an entry cap; corruption costs only undo
  history, never owned state.
- **`cache.sqlite3`** — the disposable store: thumbnails, fingerprints,
  (future) embeddings. Delete it, it rebuilds; corruption can never cost
  user state.

Crash safety, backups, locking: Part B §10b.

## 6. Export — the flexible engine (v1)

Export is the one deliberate dialog: a *summarizing* confirmation that
never lies about what it will write. It is built as a small engine —
**scope → file selection → destination shape → collision policy →
naming → dry-run → run** — so power comes from composing simple,
honest stages rather than from many special cases.

### 6.1 The dialog reads as a sentence, top to bottom

Every option live-updates a plain-language summary and a final dry-run
line. The user should be able to read the dialog aloud and have it be
exactly true. Nothing is written until the final button, which restates
the concrete action (`Copy 91 files`).

### 6.2 Stage 1 — Scope (which photos)

A single scope selector, mutually exclusive, with a live count:

- **Selection** — the current multi-select (default when one exists).
- **Current filter** — whatever the chips currently resolve to
  (default when there's no selection). Shown as the active filter
  sentence, e.g. *"accepted AND (temples OR shrines)"*.
- **Verdict shortcuts** — one-click `Accepted`, `Rejected`,
  `Unreviewed`, `Accepted + Unreviewed`, `Everything`. These are just
  pre-built filters surfaced for the common cases.
- **Solo group** — if a group/tag is soloed, "this group only" appears.

Scope honesty: when scope = accepted, the dialog states how many
**unreviewed** photos are excluded (`12 unreviewed excluded — include?`
as a one-click toggle), so a half-finished cull never silently drops
work.

### 6.3 Stage 2 — File selection (which files of each photo)

A photo can be `Jpeg`, `Raw`, or `Both`; export picks files, not photos:

- **JPEG only** · **RAW only** · **Both** · **Original-as-shot**
  (whatever files exist for each photo, no filtering).
- **Honest counts per choice:** *"RAW only: 44 files — 3 photos have no
  RAW, skipped (show)"*. The "(show)" reveals exactly which photos are
  affected, selectable back into the grid.
- **Sidecar handling:** if XMP/other sidecars exist next to a file
  (none written by dcs in v1, but cameras and other tools leave them),
  a checkbox **"include adjacent sidecars"** carries them along.

### 6.4 Stage 3 — Destination shape (how it's laid out)

Independent toggles, each restated in the summary sentence:

- **Folder layout:** `Together` (one flat folder) · `Split JPEG/ + RAW/`
  · **Mirror source tree** (recreate the subfolder structure under the
  destination) · **Group-as-folders** (one folder per current
  group/tag — e.g. a folder per day or per tag — so the on-disk result
  matches the sheet's organization). Group-as-folders reuses the active
  grouping; it's the export-side payoff of the whole grouping model.
- **Flatten vs preserve:** when mirroring or grouping, a checkbox
  controls whether multi-tagged photos are **duplicated into each tag
  folder** or **placed once** in a primary folder (primary = first tag
  by band order), with the count delta shown.

### 6.5 Stage 3b — Rejected photos get an exit

Rejecting never deletes, but the export engine is how rejects leave:

- **Export rejected** is a first-class scope (Stage 1 shortcut), so a
  user can copy the 2,400-photo reject pile somewhere to delete it
  *outside* the app, on purpose, with full knowledge.
- A companion non-export action lives in the registry/menus:
  **"Reveal rejected in file manager"** (opens the OS file browser with
  the rejected files selected where the platform allows, else opens the
  containing folder). This is the affordance that keeps "dcs never
  deletes" from being a dead end.
- The summary is explicit when scope = rejected: *"Exporting 2,400
  rejected photos. dcs will not delete the originals; you can delete the
  copies, or the originals, yourself."*

### 6.6 Stage 4 — Naming & collisions

- **Operation:** **Copy only** in v1 (move deferred — stated in the
  dialog so the user knows originals stay).
- **Never overwrite, ever.** On a name collision: **Skip** or
  **Rename `-1, -2…`** (default rename). The dialog shows the projected
  collision count before running.
- **Rename template (optional, opt-in):** a simple token string —
  `{name}`, `{date}`, `{time}`, `{group}`, `{seq}`, `{tag}` — with a
  live preview of the first three resulting filenames. Off by default
  (originals keep their names); this is the one place naming gets
  powerful without becoming a develop pipeline.
- **Conflict between rename-template and split/group layout** is
  resolved by the engine and shown in preview, never by a silent rule.

### 6.7 Stage 5 — Dry-run & run

- **Dry-run line** above the button restates the whole action in one
  sentence and one count: *"Copy 91 files (47 JPEG + 44 RAW) into
  `~/Export/Japan`, split JPEG/RAW, rename on collision."*
- The button text is the dry-run verb+count: **`Copy 91 files`**.
- **Progress with cancel:** cancel leaves a clean, reported partial
  state (already-copied files listed; nothing half-written — copy to
  `.part`, fsync, rename, same atomic contract as saves).
- **Completion toast + Open folder.** **Last full config remembered**
  (every stage), so re-exporting a refined cull is one confirm.

### 6.8 What export deliberately is NOT (v1)

No move, no in-place rename of originals, no develop/convert (no
JPEG↔anything), no resize/watermark/ICC, no zip. These are future; the
engine's stages leave clean seams for each, but v1 stays a faithful,
flexible *copy* tool.

### 6.9 Architecture: pure planner, dumb executor

Export is the most logic-heavy feature in v1 (scope resolution,
per-photo file selection with skip accounting, layout/folder mapping,
collision renaming, name-template expansion) and therefore the most
bug-prone — so all of that logic is **pure and lives in `dcs-domain`**,
none of it in `dcs-io` or `dcs-app`. The split:

- **`dcs-domain::plan_export(pool, tags, groups, request) -> Result<ExportPlan, ExportError>`**
  — a pure function. Takes the resolved settings (the §6 stages as a
  plain `ExportRequest`) and produces a fully-decided **`ExportPlan`**:
  an ordered list of concrete `(source_path, dest_path, file_role)`
  operations, plus the skip report (photos with no matching file), the
  collision-resolution decisions already baked into the dest paths, and
  the dry-run sentence + counts. No disk access, no copying — it only
  *decides*. This is unit-tested exhaustively (RAW-only with missing
  RAWs, collision cascades `-1 -2 -3`, template + split-layout
  interaction, empty scope) with zero mocks.
- **`dcs-io` executes the plan and nothing else.** It walks the
  `ExportPlan`'s operation list, copies each file (`.part` → fsync →
  rename, the atomic contract), emits progress events, and supports
  cancel. It makes **no decisions** — every path, rename, and skip was
  settled by the planner. The executor can't produce a wrong filename
  because it never computes one.
- **`dcs-app` is the thin trigger.** It gathers the dialog state into an
  `ExportRequest`, calls the pure planner to get the live dry-run
  sentence shown in the dialog (so the preview *is* the plan — what you
  see is exactly what runs), and on confirm hands the `ExportPlan` to
  `dcs-io` to execute.

The payoff: the dialog's live summary and the actual run are the same
artifact (`ExportPlan`), so the "reads true aloud" guarantee (§6.1) is
structural, not a thing two code paths have to agree on. And the buggiest
feature is testable in the pure core.

## 7. Scope

### v1

60 fps pipeline (preview-first, thumb cache, decode pool + prefetch,
virtualized grid) · entry + progressive import + re-scan +
missing/unreadable handling + fingerprint identity · time/tag/none
grouping, smart day, timezone (IANA), derived bursts · explicit sort,
zoom, bricks, square cells · tags + palette + `1–9` + merge-via-rename ·
A/X/unreviewed + AND/OR filters + solo · registry → keys + `Cmd+P` +
two context menus · gallery + filmstrip + 1:1 · durable undo/redo ·
`.dcs/` sidecar (project.json + undo.log + cache.sqlite3) with `views`
array · flexible copy-only export engine (§6) including export-rejected +
reveal-rejected.

### Deferred (v1.1)

GPS axis (data already collected) · move export · named export presets ·
rename re-link UI via content fingerprint (identity already keyed on it) ·
tag-manager panel · empty-grid menu · verdict sort · recents.

### Future

- **Board view** — freeform canvas: drag anywhere, overlap, resize;
  curated membership; per-board positions; multiple boards; rejected
  photos placed stay, dimmed. Architecture ready: Part B §8.
- **AI auto-tagging (zero-shot, local).** Small image–text embedding
  model (MobileCLIP/SigLIP class, CPU) scores photos against a
  *user-defined vocabulary*, multi-label with a confidence knob.
  Rules: **propose, don't assign** (suggestion layer + crystallize to
  confirm — derived-vs-owned holds); feeds on the 256 px cache tier (no
  extra decode); **embeddings cached** so vocabulary changes recompute
  only the text side (instant re-tag); opt-in model download, inference
  fully local. Embeddings also unlock text search and embedding-based
  near-duplicate detection.
- Sheet export (WYSIWYG; pairs with the board) · map view · composed
  grouping · RAW decode/develop · loupe compare · XMP sidecars (write) ·
  develop/convert + resize/watermark on export · watch-folder ·
  histogram · configurable smart-day boundaries · per-batch timezone
  offsets · manual rotate · ICC.

## 8. Decisions log

| # | Decision |
|---|---|
| 1 | No drill-in/breadcrumb/nesting; focus = solo filter |
| 2 | Removed: manual groups, clusters, stacks, promote, maybe, notes |
| 3 | Derived vs owned; `F2`/tag-all crystallizes derived → tag |
| 4 | Bursts derived, not tags; no sequence-veto; subseconds for ordering, equal-second = gap 0 |
| 5 | Axes v1: time/tag/none; gps deferred, data collected |
| 6 | Smart day; `auto` shows its resolution; day titles carry the date. **Default buckets are neutral time-of-day labels; evocative labels ("golden hour") are opt-in and GPS+date-derived, not fixed clock hours** |
| 7 | Shoot timezone at derivation as an **IANA zone** (offset derived per photo, survives DST mid-trip); EXIF untouched |
| 8 | Leftover group always last |
| 9 | Sort explicit (time/name, ↑/↓); zero-EXIF → `name ↑` + `none` |
| 10 | Tag-band duplicates = projections; unique counts; **commands carry `PhotoId` sets — dispatch dedups to photos before building any undo entry, never projection identities or positions** |
| 11 | Unreviewed is a first-class filter |
| 12 | Palette never creates silently; merge via rename |
| 13 | **Filters support AND-across-groups / OR-within-group (v1), per-group toggle; same rules = export scope** |
| 14 | Visible-only batch rule; header-select exception; **Esc: palette → gallery → selection → solo. Filters are NOT in the Esc chain — removed only by explicit chip-X** |
| 15 | Fixed cell layer budget; RAW badge zoom-gated; color never sole channel |
| 16 | Collapsed cover = first accepted, else first |
| 17 | Burst spans segment per group; bricks at half gap; neutral accent |
| 18 | **Undo durable across sessions (persisted `undo.log`); promptless design depends on it** |
| 19 | Export: copy-only, never overwrite, honest skip counts; **flexible staged engine (§6)** |
| 20 | Gallery: key parity, visible order, filmstrip, 1:1 in v1 |
| 21 | Square cells, contain-fit, EXIF orientation auto |
| 22 | One registry → keys, palette, two context menus |
| 23 | Missing/unreadable: placeholder + badge, state kept, reported |
| 24 | `views` array in the project file from day one (board-ready) |
| 25 | `.dcs/`: project.json + undo.log + cache.sqlite3; atomic saves + `.bak`; lock with take-over |
| 26 | 60 fps sustained = the acceptance criterion; **the benchmark drives the real painted egui grid (bricks, strips, regroup), not a headless decode loop** |
| 27 | Entry: empty window + Open folder, nothing else |
| 28 | Anti-features: never deletes files; no keymap editor UI. **But rejects get an exit: export-rejected scope + reveal-rejected-in-file-manager** |
| 29 | UI: dark neutral, square corners, mono data type, color = meaning only, no motion (§3) |
| 30 | "Cluster" banned as a term in code and UI |
| 31 | Grid is arrow-navigable: focus cursor (←→ visible order, ↑↓ by row), Shift extends, **Space opens gallery**; prefetch follows the cursor |
| 32 | View-mode switcher in the top bar: grid \| gallery (board joins later) |
| 33 | **File identity keyed on content fingerprint from import day one; rename-in-place keeps owned state (re-link UI is v1.1, the keying is not)** |
| 34 | **Lock file carries a refreshed timestamp; stale after N minutes so a crash doesn't strand the project in read-only forever** |
| 35 | **`MoveOnBoard` drags coalesce into one undo entry on drop; an aborted/interrupted drag commits nothing — positions roll back to pre-drag, no torn entry** |
| 36 | **Export = pure planner in `dcs-domain` (`plan_export` → `ExportPlan`) + dumb executor in `dcs-io`; `dcs-app` is the thin trigger. The dialog preview *is* the plan, so "reads true aloud" is structural. `dcs-domain` owns its own error enums; domain failures never leak from io (§6.9, §9)** |

---

# PART B — TECHNICAL SPEC (to be discussed)

## 9. Architecture

Layered Cargo workspace, dependency arrows down only, enforced at
compile time:

```
dcs-ui      egui binary: view modes (grid, gallery; board later) + ephemeral UI state
dcs-app     conductor: session, command registry, dispatch, keymap, undo stack
dcs-io      infrastructure behind traits: imaging / source / persistence
dcs-domain  PURE core: types + pure functions. no I/O, no async, no egui
```

- **dcs-domain:** `Photo`, `Pool`, `Tag`+assignments, `AcceptState`,
  `View`/`ViewKind`, pairing, grouping (time incl. smart-day / tag /
  none + leftovers; `Gps` variant typed now, wired in v1.1),
  `derive_bursts`, timezone adjustment, filtering (AND/OR chip
  resolution), fuzzy matching, tag merge, and the **pure export planner**
  (`plan_export` → `ExportPlan`, §6.9). Pure, unit-tested, no mocks.
  **Owns its own error enums** (e.g. `ExportError`, pairing errors,
  filter-resolution errors) — domain failures are domain types, never
  leaked up from `dcs-io`, so I/O concepts never appear in pure logic.
- **dcs-io:** `imaging` (decode, embedded preview, orientation, thumb
  cache, prefetch), `source` (scan, EXIF incl. subseconds, content
  fingerprint, progressive import stream, missing-file detection),
  `persistence` (versioned DTOs — serde structs + a version field,
  **derive everything, no hand-written mapper universe**; `views` array
  with unknown-kind preservation; `undo.log` append/compact). The
  **export executor** lives here too but is *dumb*: it walks a finished
  `ExportPlan` from the domain planner, copies files (`.part` → fsync →
  rename), emits progress, supports cancel — it makes no path or rename
  decisions (§6.9). The entire thread model lives here behind
  handle-returning traits. Each subsystem keeps its internal types to
  itself so `imaging` (the performance-critical one) stays cleanly
  promotable to its own crate later.
- **dcs-app:** session (active view settings, selection, zoom, gallery
  state), the single command registry (keys, palette, menus all consume
  it), dispatch → undo stack (durable) → io effects → debounced save.
  For export it is the **thin trigger**: gathers dialog state into an
  `ExportRequest`, calls the pure planner for the live dry-run sentence
  (the preview *is* the plan), and on confirm hands the `ExportPlan` to
  `dcs-io`. It owns the in-memory undo stack and emits serializable
  `Command` records; `dcs-io` only appends/replays those bytes and never
  interprets command *semantics* (replay validation, if any, is
  `dcs-app`'s job). `Command` is defined in `dcs-domain` so both layers
  depend *down* on it, never on each other.
- **dcs-ui:** grid (square cells, contain-fit, gap/bricks), gallery +
  virtualized filmstrip, palette/menus/export dialog over the registry.
  Board module slots in later. Ephemeral state never travels down.

## 9b. Views system — board-ready now, built later

- `ViewKind::Grid(GridSettings)` (v1 ships one) and
  `ViewKind::Board(BoardState)` (curated `members`, `positions`,
  z-order) — typed and serialized from day one; unknown kinds are
  preserved on load, not rendered.
- **Ownership rule, enforced by the types:** per-photo facts (verdict,
  tags) on the photo, true in every view; layout facts (position,
  membership, z) on the view. The board never invents photo state; the
  grid never stores layout.
- Board mutations are ordinary registry commands (`AddToBoard`,
  `MoveOnBoard` — drags coalesce into one undo entry on drop, aborted
  drags commit nothing, decision #35) riding the existing undo stack;
  the board reuses the thumbnail pipeline verbatim. The board never
  gains its own culling, filters, or grouping — it is an arrangement
  lens.

## 10. Threading

egui main thread never blocks: requests return handles instantly.
rayon decode pool (≈ physical cores) for JPEG/preview decode; prefetch
driver feeds it ahead of scroll, selection, and filmstrip neighborhood;
disk I/O thread(s) for cache, debounced save, fingerprinting, export
jobs (off-thread, progress events, cancel). One-directional channels;
all inside `dcs-io` behind the traits. **No tokio in v1.**

## 10b. Storage & reliability

- **Three stores, distinct contracts.** `project.json` = precious: JSON
  deliberately (thousands of records serialize in milliseconds; buys
  diffability, hand repair, transparency; the versioned-DTO seam is the
  upgrade path if a pool ever outgrows it). `undo.log` = durable but
  rebuildable-from-nothing: an append-only command log, compacted on
  save, capped; losing it costs only history. `cache.sqlite3` =
  disposable: right tool for thousands of small blobs, keyed lookups,
  concurrent readers. Schema `(content_key, tier, blob, last_used)`.
- **Atomic saves:** write `project.json.tmp`, fsync, rename; rotated
  `project.json.bak`; save on debounce, quit, and interval. A crash
  leaves old or new, never torn. Export copies use the same
  `.part` → fsync → rename contract.
- **Locking:** lock file in `.dcs/` carrying a **timestamp refreshed by
  the live instance**; a second instance opens **read-only** with a
  banner and a "Take over" action; a lock whose timestamp is older than
  N minutes is treated as stale and reclaimed automatically, so a crash
  never strands the project read-only (decision #34). No PID liveness
  checking — the timestamp is the liveness signal.
- **Stable IDs:** persisted monotonic counters; never recomputed or
  reused, so assignments and board positions survive any re-scan.
- **File identity:** **matched by content fingerprint** (mtime+size as a
  fast pre-filter to decide whether to re-fingerprint). Renamed-on-disk
  keeps its photo, ID, verdicts, and tags (decision #33); only a genuine
  content change invalidates thumbs. The re-link *UI* (surfacing "this
  looks like a moved file") is v1.1; the identity keying is v1.
- **Thumb cache:** two tiers (≈256 px grid, ≈1024 px gallery), encoded
  JPEG blobs, key = content fingerprint, LRU under a size cap,
  orientation baked in, fully rebuildable.
- **EXIF subseconds (burst-critical):** `DateTimeOriginal` is
  1-second resolution — a 10 fps burst is ten identical timestamps.
  Read `SubSecTimeOriginal`; fall back to filename digits for
  intra-second order; equal-second runs = gap 0.
- **Color:** assume sRGB end-to-end in v1; documented simplification.
- **Diagnostics overlay** (palette command): fps, decode-queue depth,
  cache hit rate, memory — the benchmark harness and this overlay are
  the same numbers, **measured on the real painted grid** (decision #26).

## 11. Types (sketch, not final)

```rust
pub enum PhotoType { Jpeg, Raw, Both }
pub enum AcceptState { Accepted, Rejected, Unreviewed }

pub struct Photo {
    pub id: PhotoId,
    pub files: AssociatedFiles,              // jpeg / raw paths (relative)
    pub fingerprint: ContentFingerprint,     // identity, keyed from import (#33)
    pub captured_at: Option<OffsetDateTime>, // raw EXIF value
    pub gps: Option<GpsCoord>,               // collected now, used in v1.1
    pub orientation: ExifOrientation,
    pub state: AcceptState,
    pub missing: bool,
}

pub struct Tag { pub id: TagId, pub name: String, pub color: Color }

pub enum TimeGranularity { Auto, SmartDay, Hour, Day, Week }
pub enum Axis { Time(TimeGranularity), Gps { radius_m: f64 }, Tag, None }

// Timezone as IANA zone; per-photo offset derived (DST mid-trip safe, #7)
pub fn adjusted(t: OffsetDateTime, shoot_zone: &Tz) -> OffsetDateTime;
pub fn group(pool: &Pool, tags: &Tags, axis: Axis, zone: &Tz, sort: Sort)
    -> Vec<DerivedGroup>;

pub struct BurstKnobs { gap: Duration, min: usize, max_dur: Option<Duration>, on: bool }
pub fn derive_bursts(frames: &[(PhotoId, OffsetDateTime, FileSeq)], k: BurstKnobs)
    -> Vec<Range<usize>>;     // FileSeq = intra-second ordering only

// Filters: AND across groups, OR within a group (#13)
pub enum ChipOp { And, Or }
pub struct FilterGroup { op: ChipOp, chips: Vec<Chip> }   // op = within-group
pub struct Filter { groups: Vec<FilterGroup> }            // groups AND-combined
pub fn resolve(pool: &Pool, tags: &Tags, f: &Filter) -> Vec<PhotoId>;

pub enum ViewKind { Grid(GridSettings), Board(BoardState) }   // Board typed now

pub enum Command {            // ONE registry: keys, palette, menus
    SetState(Vec<PhotoId>, AcceptState),
    AssignTag(TagId, Vec<PhotoId>), UnassignTag(TagId, Vec<PhotoId>),
    CreateTag { name: String, color: Color },
    RenameTag(TagId, String),               // rename→existing offers merge
    MergeTags { into: TagId, from: TagId }, DeleteTag(TagId),
    AddToBoard(ViewId, Vec<PhotoId>), MoveOnBoard(ViewId, Vec<(PhotoId, Pos)>),
    // dispatch dedups Vec<PhotoId> to unique photos before the undo entry (#10)
    // session-level palette entries (not undoable): SetAxis, SetSort,
    // SetShootZone, SetFilter, Solo, CollapseAll, OpenExport, RevealRejected, …
}

// Export (§6): a pure planner in dcs-domain turns settings into a
// fully-decided plan; dcs-io only executes the plan. (§6.9)

// What the dialog collects — the §6 stages as plain settings.
pub struct ExportRequest {
    scope: ExportScope,                     // Selection | Filter(Filter) | Verdict(..) | SoloGroup
    files: ExportFiles,                     // JpegOnly | RawOnly | Both | AsShot
    sidecars: bool,
    layout: ExportLayout,                   // Together | SplitJpegRaw | MirrorTree | GroupAsFolders
    multitag: MultiTagPlacement,            // DuplicatePerTag | PrimaryOnce
    collisions: SkipOrRename,               // overwrite doesn't exist
    naming: Option<NameTemplate>,           // tokens: {name}{date}{time}{group}{seq}{tag}
    dest: PathBuf,                          // copy-only in v1
}

// Fully decided. Every path/rename/skip is settled here — io makes no choices.
pub struct ExportOp { src: PathBuf, dst: PathBuf, role: FileRole }   // role = Jpeg|Raw|Sidecar
pub struct ExportPlan {
    ops: Vec<ExportOp>,                     // ordered, dst collisions already resolved
    skipped: Vec<(PhotoId, SkipReason)>,    // e.g. RawOnly but no RAW — surfaced as "(show)"
    summary: String,                        // the live dry-run sentence (§6.7)
    counts: ExportCounts,                   // total files, per-role, projected collisions
}

pub enum ExportError { EmptyScope, BadTemplate(String), /* … domain-level only */ }

// PURE: no disk access. The dialog preview and the actual run are this one artifact.
pub fn plan_export(pool: &Pool, tags: &Tags, groups: &[DerivedGroup], req: &ExportRequest)
    -> Result<ExportPlan, ExportError>;
// dcs-io then executes: walk plan.ops, copy (.part→fsync→rename), progress, cancel.
```

## 11b. AI tagging — accommodation (future)

Nothing in v1 changes; the seams exist. The job is a `dcs-io` background
consumer of the thumb cache; embeddings = a new disposable table in
`cache.sqlite3` (`fingerprint → vec<f32>`); suggestions are derived
state in `dcs-app` (never persisted); confirmation emits ordinary
`CreateTag`/`AssignTag` commands through the registry, riding undo.
Runtime: `candle` first, `ort` if speed demands. Model ships separately
or downloads opt-in.

## 12. Open technical decisions

1. **Pairing rule** (first thing `dcs-domain` needs): basename scope,
   raw-extension list, subfolders, one-jpeg-two-raws.
2. Color-tag palette hexes + auto-assign cycle.
3. Burst defaults (gap 2.0 s, min 3) validated on a real Fuji folder.
4. Filmstrip virtualization under egui (shares the thumb cache).
5. **Timezone: settled as IANA zone** (decision #7) — remaining detail
   is the `Tz` library/representation and the UI for picking a zone.
   **Must land before crystallization ships: a frozen tag made under the
   wrong zone is owned and wrong forever, un-re-derivable.**
6. Unknown-`ViewKind` forward-compat mechanics in the DTO layer.
7. Cache size-cap default + exact tier resolutions (validate in the
   benchmark harness).
8. **Content-fingerprint algorithm + cost** (decision #33): hash choice,
   whether to hash full file vs head+tail+size, and the mtime+size
   pre-filter threshold so import stays within the 60 fps budget.
9. **`undo.log` format + compaction** (decision #18): record framing,
   when to compact, the entry cap, and replay-on-open ordering vs the
   loaded `project.json` state.
10. **Export name-template grammar** (§6.6): exact token set, escaping,
    and conflict resolution order against split/group layout.

## 13. Build order

1. **dcs-domain** — pairing, grouping (smart-day boundaries!),
   `adjusted()` (IANA zone), `derive_bursts` (test the 10 fps
   identical-timestamp case), filter resolution (AND/OR), merge, fuzzy,
   and **`plan_export` → `ExportPlan`** (test collision cascades,
   RAW-only-with-missing-RAW, template × split-layout, empty scope).
   Pure, fully tested. (Building the planner here means the buggiest v1
   feature is proven before any I/O exists.)
2. **dcs-io::imaging behind a benchmark harness that drives the real
   painted grid** — 3,000 JPEGs at 60 fps with bricks/strips/regroup,
   not a headless decode loop (decision #26). If this fails, nothing
   else matters.
3. **dcs-io::source + persistence** — progressive import, content
   fingerprint, missing files, `views` round-trip, `undo.log`
   append/compact/replay.
4. **dcs-app** — registry, dispatch, durable undo, keymap.
5. **dcs-ui** — thin grid + headers + gallery/filmstrip/1:1 + A/X.
6. Then: tag palette/strips, AND/OR filters/solo, `Cmd+P`, menus,
   timezone picker, **export: the planner already exists (step 1), so
   this is the dialog + the dumb `dcs-io` executor + export-rejected +
   reveal-rejected**. Board when v1 is stable.

## 14. Principles

- Fast is the product; benchmark the real grid before building.
- Originals are sacred; all state in the project file; rejects get an
  exit, never a deletion.
- Arrows point down; the compiler enforces the architecture.
- Derived vs owned; nothing derived is persisted; identity is content,
  not path.
- Prompts confirm nothing; rules govern everything; durable undo
  reverses anything.
- One registry, three surfaces. One color system. Visible-only batch
  ops. Predictable geometry over clever packing.
- Per-photo facts on the photo; layout facts on the view.
- Export is an honest, composable copy engine — a pure planner decides,
  a dumb executor copies; the dialog preview *is* the plan, so it reads
  true aloud by construction.
- Square, dark, dense, silent. The photos are the interface.