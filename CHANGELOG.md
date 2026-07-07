# Changelog

All notable changes to dcs are documented here.

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
