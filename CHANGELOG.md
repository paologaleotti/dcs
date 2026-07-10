# Changelog

All notable changes to dcs are documented here.

## v0.3.0

### Added

- **Crash screen** — an unexpected main-thread panic is now caught and shown on a
  dedicated screen with a copiable report, instead of the window vanishing. The
  last good autosave is left untouched so a crash can't corrupt the project.
- **Disk-space check in export** — the export dialog shows the space the export
  will need against the free space on the destination drive.

### Changed

- UI refinements across the app.

### Fixed

- Photos now sort by their zone-anchored capture instant rather than the naive
  EXIF wall-clock, so two cameras in different zones interleave by true time.
- Export `{date}`/`{time}` name-template tokens derive from the same
  zone-attributed instant grouping uses, so a file's name can no longer disagree
  with its `{group}` folder near a day boundary.
- Lateral crop handles resize correctly when a locked aspect ratio is active.
- Disabled egui's native (browser-style) zoom, which fought the gallery's own
  zoom.
- `available_space` now errors on a nonexistent path on Windows instead of
  reporting the whole drive's free space.
- Builds on rustc 1.97 (ambiguous `f32` stroke literals annotated).
- Updated all dependencies to their latest versions.

## v0.2.0

### Added

- **Board view mode** — a free-form canvas for arranging photos: pan/zoom, drag
  to place, per-item context menu, and a docked sidebar grid for row navigation.
- **Gallery pinch-to-zoom** — the single-photo view now supports real, continuous
  zoom (contain-fit → 1:1) that is cursor-centered and works with both mouse wheel
  and trackpad pinch. Panning by drag or two-finger scroll, a minimap while zoomed,
  and `Z` / double-click to snap between fit and 100%. Full-resolution pixels are
  fetched progressively on zoom-in via a decode-tier ladder, so scrolling the
  gallery at fit stays cheap.
- **Print / contact-sheet export** — lay the visible photos out on paper and
  export a printable contact sheet.

### Changed

- **UI overhaul** — new typography, headers, and colour palette across the app.
- **Crop straighten tool** — improved straightening interaction and accuracy.

### Fixed

- Locking now uses unique temp names, preserving the single-writer guarantee.
- Filmstrip icons render correctly in gallery mode.
- Data-integrity and safety hardening across the session and UI layers, closing
  several bugs found in deep review.
- Timezone handling hardened, with added test coverage.
- Corrected distributable package metadata.

## v0.1.0

Initial release: keyboard-first digital contact sheet — scan, cull, tag, and
export photos, with a fast egui grid, gallery view, tagging, verdicts, timezone-
aware capture times, and atomic project persistence.
