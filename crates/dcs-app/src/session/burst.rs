use dcs_domain::burst::{self, BurstKnobs, FileSeq};
use dcs_domain::grouping::Axis;
use dcs_domain::photo::PhotoId;
use dcs_domain::sort::{SortDir, SortKey};
use dcs_domain::timezone;
use time::OffsetDateTime;

use super::{BurstMark, Session};

impl Session {
    /// The active burst derivation knobs.
    pub fn burst_knobs(&self) -> BurstKnobs {
        self.burst_knobs
    }

    /// Number of derived bursts across all groups — the live count the knobs UI
    /// shows as they're adjusted.
    pub fn burst_count(&self) -> usize {
        self.burst_count
    }

    /// Replace the burst knobs and re-derive immediately. A no-op when unchanged,
    /// so the UI can call it freely. Bursts are derived membership, never owned —
    /// nothing here touches `dirty` or persistence.
    pub fn set_burst_knobs(&mut self, knobs: BurstKnobs) {
        if self.burst_knobs == knobs {
            return;
        }
        self.burst_knobs = knobs;
        self.derive_bursts();
    }

    /// Whether bursts can be shown for the current view: the overlay is on, the
    /// knob is enabled, the axis isn't `tag`, and the sort is by time. Bursts are
    /// chronological runs; under a name sort their frames need not be contiguous
    /// in display order, so the span overlay can't render coherently — we don't
    /// derive them at all rather than paint broken spans. The UI greys the toggle
    /// and explains when this is false.
    pub fn bursts_available(&self) -> bool {
        self.show_bursts()
            && self.burst_knobs.on
            && !matches!(self.axis, Axis::Tag)
            && self.sort.key == SortKey::Time
    }

    /// Recompute burst membership over every group. Each group is ordered
    /// chronologically (by adjusted instant, then filename sequence), segmented
    /// into runs by the knobs. Undated photos can't belong to a burst. Skipped
    /// entirely unless [`Self::bursts_available`] (off / tag axis / name sort).
    pub(super) fn derive_bursts(&mut self) {
        self.bursts.clear();
        self.burst_count = 0;
        if !self.bursts_available() {
            return;
        }
        // Under a descending time sort the display reverses, so the run's
        // chronological first frame is its rightmost cell. Flip first/last so the
        // label and the span's left cap always land on the visually-first cell.
        let desc = self.sort.dir == SortDir::Desc;
        let camera = self.resolve_camera_zone();
        let display = self.resolve_display_zone();
        let photos = self.builder.photos();
        let mut next_id = 0u32;
        for group in &self.groups {
            let mut frames: Vec<(PhotoId, OffsetDateTime, FileSeq)> = group
                .members
                .iter()
                .filter_map(|&i| {
                    let p = &photos[i];
                    let naive = p.captured_at?;
                    let instant = timezone::adjusted(
                        timezone::source_instant(naive, p.captured_offset, camera),
                        display,
                    );
                    Some((p.id, instant, burst::file_seq(&p.file_name())))
                })
                .collect();
            frames.sort_by(|a, b| a.1.cmp(&b.1).then_with(|| a.2.cmp(&b.2)));

            for range in burst::derive_bursts(&frames, &self.burst_knobs) {
                let id = next_id;
                next_id += 1;
                self.burst_count += 1;
                let (start, end, len) = (range.start, range.end, range.len());
                let (lead, trail) = if desc {
                    (end - 1, start)
                } else {
                    (start, end - 1)
                };
                for fi in range {
                    self.bursts.insert(
                        frames[fi].0,
                        BurstMark {
                            id,
                            len,
                            first: fi == lead,
                            last: fi == trail,
                        },
                    );
                }
            }
        }
    }
}
