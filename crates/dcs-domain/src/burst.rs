//! Derived bursts. A burst is a maximal run of frames shot in rapid
//! succession — recomputed from capture times and the knobs, never persisted,
//! never a tag. Pure: change a knob and it re-derives.
//!
//! The ordering basis is the *adjusted* capture instant (EXIF subseconds when
//! present), with a filename trailing-digit sequence breaking intra-second ties
//! for ordering only — equal timestamps never split a run. Callers pass frames
//! already in that order; the returned index ranges address the input slice.

use std::ops::Range;

use time::{Duration, OffsetDateTime};

use crate::photo::PhotoId;

/// Intra-second ordering hint: a frame's filename trailing-digit sequence. Used
/// only to break ties between frames sharing a capture timestamp; it never
/// affects whether two frames join a run.
pub type FileSeq = u64;

/// Burst derivation knobs — display settings, not owned state. Adjusting them
/// re-derives instantly.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BurstKnobs {
    /// Maximum gap between consecutive frames that still joins a run.
    pub gap: Duration,
    /// Minimum frames for a run to qualify as a burst.
    pub min: usize,
    /// Optional cap on a run's total duration. A candidate run longer than this
    /// is discarded — the knob that stops an even-cadence timelapse from
    /// registering as one giant burst. `None` (the default) keeps every
    /// gap-joined run regardless of length.
    pub max_dur: Option<Duration>,
    /// Whether burst derivation is active at all.
    pub on: bool,
}

impl Default for BurstKnobs {
    /// Defaults validated against real burst-mode folders (decision #3): a 2.0 s
    /// join gap, 3-frame floor, no duration cap, on.
    fn default() -> Self {
        BurstKnobs {
            gap: Duration::seconds(2),
            min: 3,
            max_dur: None,
            on: true,
        }
    }
}

/// Derive the burst runs over `frames`, returning contiguous index ranges into
/// the input slice. `frames` MUST already be ordered ascending by adjusted
/// capture instant, then by [`FileSeq`] — the ranges are meaningless otherwise.
///
/// A run joins consecutive frames whose gap is `≤ knobs.gap` (equal timestamps
/// = gap zero, so a sub-second burst with no EXIF subseconds always coheres),
/// qualifies at `≥ knobs.min` frames, and — when `knobs.max_dur` is set — is
/// discarded if its total span exceeds that cap. Returns an empty vec when
/// derivation is off or there are too few frames to ever qualify.
pub fn derive_bursts(
    frames: &[(PhotoId, OffsetDateTime, FileSeq)],
    knobs: &BurstKnobs,
) -> Vec<Range<usize>> {
    if !knobs.on || frames.len() < knobs.min.max(1) {
        return Vec::new();
    }

    let mut runs = Vec::new();
    let mut start = 0;
    for i in 1..frames.len() {
        if frames[i].1 - frames[i - 1].1 <= knobs.gap {
            continue;
        }
        push_if_qualifies(&mut runs, frames, start, i, knobs);
        start = i;
    }
    push_if_qualifies(&mut runs, frames, start, frames.len(), knobs);
    runs
}

/// Extract a frame's intra-second ordering sequence from its filename: the run
/// of trailing digits before the extension, as a number (`IMG_0421.JPG` → 421).
/// No trailing digits yields 0 — such frames keep their incoming order.
pub fn file_seq(name: &str) -> FileSeq {
    let stem = name.rsplit_once('.').map_or(name, |(stem, _)| stem);
    let digits: String = stem
        .chars()
        .rev()
        .take_while(|c| c.is_ascii_digit())
        .collect();
    // Reverse back and parse; a very long digit run that overflows falls back to
    // 0 rather than panicking — ordering is best-effort, never load-bearing.
    digits
        .chars()
        .rev()
        .collect::<String>()
        .parse()
        .unwrap_or(0)
}

/// Close a candidate run `[start, end)`: keep it only if it meets the frame
/// floor and, when a duration cap is set, does not exceed it.
fn push_if_qualifies(
    runs: &mut Vec<Range<usize>>,
    frames: &[(PhotoId, OffsetDateTime, FileSeq)],
    start: usize,
    end: usize,
    knobs: &BurstKnobs,
) {
    if end - start < knobs.min {
        return;
    }
    if let Some(max) = knobs.max_dur
        && frames[end - 1].1 - frames[start].1 > max
    {
        return;
    }
    runs.push(start..end);
}
