use dcs_domain::burst::{BurstKnobs, FileSeq, derive_bursts, file_seq};
use dcs_domain::photo::PhotoId;
use time::macros::datetime;
use time::{Duration, OffsetDateTime, UtcOffset};

/// Build a frame at `secs` whole seconds (plus optional `nanos` subseconds) past
/// a fixed epoch, with id and FileSeq equal to its position.
fn frame(i: u32, secs: i64, nanos: i64) -> (PhotoId, OffsetDateTime, FileSeq) {
    let base = datetime!(2025-05-11 10:00:00).assume_offset(UtcOffset::UTC);
    let at = base + Duration::seconds(secs) + Duration::nanoseconds(nanos);
    (PhotoId(i), at, i as FileSeq)
}

fn knobs() -> BurstKnobs {
    BurstKnobs::default()
}

#[test]
fn defaults_match_spec() {
    let k = BurstKnobs::default();
    assert_eq!(k.gap, Duration::seconds(2));
    assert_eq!(k.min, 3);
    assert_eq!(k.max_dur, None);
    assert!(k.on);
}

#[test]
fn three_tight_frames_form_one_burst() {
    let frames = [frame(0, 0, 0), frame(1, 1, 0), frame(2, 2, 0)];
    assert_eq!(derive_bursts(&frames, &knobs()), vec![0..3]);
}

#[test]
fn two_frames_never_qualify() {
    let frames = [frame(0, 0, 0), frame(1, 1, 0)];
    assert!(derive_bursts(&frames, &knobs()).is_empty());
}

#[test]
fn gap_over_threshold_splits_runs() {
    // 0,1,2 tight; then a 5 s gap; then 7,8,9 tight → two bursts.
    let frames = [
        frame(0, 0, 0),
        frame(1, 1, 0),
        frame(2, 2, 0),
        frame(3, 7, 0),
        frame(4, 8, 0),
        frame(5, 9, 0),
    ];
    assert_eq!(derive_bursts(&frames, &knobs()), vec![0..3, 3..6]);
}

#[test]
fn gap_exactly_at_threshold_joins() {
    // Consecutive frames exactly 2.0 s apart — the boundary is inclusive.
    let frames = [frame(0, 0, 0), frame(1, 2, 0), frame(2, 4, 0)];
    assert_eq!(derive_bursts(&frames, &knobs()), vec![0..3]);
}

#[test]
fn gap_just_over_threshold_breaks() {
    // 2.000000001 s gaps never join; nothing reaches the 3-frame floor.
    let frames = [frame(0, 0, 0), frame(1, 2, 1), frame(2, 4, 2)];
    assert!(derive_bursts(&frames, &knobs()).is_empty());
}

#[test]
fn equal_second_run_without_subseconds_is_gap_zero() {
    // A 10 fps burst read at 1 s resolution: ten identical timestamps. Gap 0
    // everywhere → one cohesive burst, exactly the spec's fallback.
    let frames: Vec<_> = (0..10).map(|i| frame(i, 0, 0)).collect();
    assert_eq!(derive_bursts(&frames, &knobs()), vec![0..10]);
}

#[test]
fn subseconds_within_a_second_still_join_when_close() {
    // Same whole second, sub-second apart — well under the 2 s gap.
    let frames = [
        frame(0, 0, 100_000_000),
        frame(1, 0, 300_000_000),
        frame(2, 0, 600_000_000),
    ];
    assert_eq!(derive_bursts(&frames, &knobs()), vec![0..3]);
}

#[test]
fn subseconds_can_break_a_run_clock_seconds_would_merge() {
    // 10:00:00.1 then 10:00:02.9 reads as a 2 s clock gap (would join) but is
    // really 2.8 s apart — subseconds correctly break it, and neither side
    // reaches the floor.
    let frames = [
        frame(0, 0, 100_000_000),
        frame(1, 2, 900_000_000),
        frame(2, 5, 0),
    ];
    assert!(derive_bursts(&frames, &knobs()).is_empty());
}

#[test]
fn off_yields_no_bursts() {
    let frames: Vec<_> = (0..5).map(|i| frame(i, i as i64 % 2, 0)).collect();
    let k = BurstKnobs {
        on: false,
        ..knobs()
    };
    assert!(derive_bursts(&frames, &k).is_empty());
}

#[test]
fn min_frames_knob_raises_the_floor() {
    let frames = [frame(0, 0, 0), frame(1, 1, 0), frame(2, 2, 0)];
    let k = BurstKnobs { min: 4, ..knobs() };
    assert!(derive_bursts(&frames, &k).is_empty());
    let k = BurstKnobs { min: 2, ..knobs() };
    assert_eq!(derive_bursts(&frames, &k), vec![0..3]);
}

#[test]
fn max_dur_discards_a_long_timelapse() {
    // Even 1 s cadence joins under the 2 s gap, but the whole run spans 9 s.
    let frames: Vec<_> = (0..10).map(|i| frame(i, i as i64, 0)).collect();
    let k = BurstKnobs {
        max_dur: Some(Duration::seconds(4)),
        ..knobs()
    };
    assert!(derive_bursts(&frames, &k).is_empty());
}

#[test]
fn max_dur_keeps_a_short_real_burst() {
    // A genuine sub-4 s burst survives the same cap that kills the timelapse.
    let frames = [
        frame(0, 0, 0),
        frame(1, 1, 0),
        frame(2, 2, 0),
        frame(3, 3, 0),
    ];
    let k = BurstKnobs {
        max_dur: Some(Duration::seconds(4)),
        ..knobs()
    };
    assert_eq!(derive_bursts(&frames, &k), vec![0..4]);
}

#[test]
fn empty_input_is_empty() {
    assert!(derive_bursts(&[], &knobs()).is_empty());
}

#[test]
fn single_frame_is_empty() {
    let frames = [frame(0, 0, 0)];
    assert!(derive_bursts(&frames, &knobs()).is_empty());
}

#[test]
fn burst_then_isolated_frame_then_burst() {
    // 0,1,2 burst; a lone frame 6 s later (5 s gap on each side); 4,5,6 burst.
    let frames = [
        frame(0, 0, 0),
        frame(1, 1, 0),
        frame(2, 2, 0),
        frame(3, 8, 0),
        frame(4, 14, 0),
        frame(5, 15, 0),
        frame(6, 16, 0),
    ];
    assert_eq!(derive_bursts(&frames, &knobs()), vec![0..3, 4..7]);
}

#[test]
fn file_seq_reads_trailing_digits() {
    assert_eq!(file_seq("IMG_0421.JPG"), 421);
    assert_eq!(file_seq("DSCF1234.RAF"), 1234);
    assert_eq!(file_seq("photo.jpg"), 0);
    assert_eq!(file_seq("no_extension_99"), 99);
    assert_eq!(file_seq("100_0001.CR2"), 1);
    assert_eq!(file_seq(""), 0);
}

#[test]
fn file_seq_overflow_falls_back_to_zero() {
    // A digit run far past u64 range degrades to 0 rather than panicking.
    let huge = format!("img{}.jpg", "9".repeat(40));
    assert_eq!(file_seq(&huge), 0);
}
