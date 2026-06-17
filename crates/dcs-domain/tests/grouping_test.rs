//! Exhaustive tests for the pure grouping core (CLAUDE.md). Covers axis none,
//! day/hour/week buckets, smart-day boundaries + the night-spanning-midnight
//! attribution, auto resolution, the `No date` tail, sort direction, group
//! order by earliest member, and DST-aware `adjusted()`.

use std::path::PathBuf;

use dcs_domain::fingerprint::ContentFingerprint;
use dcs_domain::grouping::{Axis, GroupKind, TimeGranularity, group, resolve_auto};
use dcs_domain::pairing::{FileKind, ScannedFile, pair};
use dcs_domain::photo::{CaptureMeta, Pool};
use dcs_domain::sort::{Sort, SortDir, SortKey};
use dcs_domain::timezone::{self, adjusted};
use time::PrimitiveDateTime;
use time::macros::datetime;
use time_tz::Tz;

fn utc() -> &'static Tz {
    timezone::zone("UTC").expect("UTC exists")
}

fn at(path: &str, when: Option<PrimitiveDateTime>) -> ScannedFile {
    let mut bytes = [0u8; 32];
    for (i, b) in path.bytes().enumerate() {
        bytes[i % 32] ^= b;
    }
    ScannedFile {
        path: PathBuf::from(path),
        kind: FileKind::Jpeg,
        orientation: Default::default(),
        fingerprint: ContentFingerprint::from_bytes(bytes),
        captured_at: when,
        meta: CaptureMeta::default(),
    }
}

fn names(pool: &Pool, members: &[usize]) -> Vec<String> {
    members
        .iter()
        .map(|&i| pool.photos()[i].file_name())
        .collect()
}

#[test]
fn axis_none_is_one_stream_in_sort_order() {
    let pool = pair([
        at("a/c.JPG", Some(datetime!(2025-05-11 14:00:00))),
        at("a/a.JPG", Some(datetime!(2025-05-11 09:00:00))),
        at("a/b.JPG", Some(datetime!(2025-05-11 12:00:00))),
    ]);
    let groups = group(pool.photos(), Axis::None, utc(), Sort::default());
    assert_eq!(groups.len(), 1);
    assert_eq!(groups[0].kind, GroupKind::Stream);
    assert_eq!(
        names(&pool, &groups[0].members),
        ["a.JPG", "b.JPG", "c.JPG"]
    );
}

#[test]
fn empty_pool_yields_no_groups() {
    let pool = pair(std::iter::empty());
    assert!(group(pool.photos(), Axis::None, utc(), Sort::default()).is_empty());
    assert!(
        group(
            pool.photos(),
            Axis::Time(TimeGranularity::Day),
            utc(),
            Sort::default()
        )
        .is_empty()
    );
}

#[test]
fn day_groups_are_dated_titled_and_numbered() {
    let pool = pair([
        at("a/d2.JPG", Some(datetime!(2025-05-12 09:00:00))),
        at("a/d1.JPG", Some(datetime!(2025-05-11 09:00:00))),
    ]);
    let groups = group(
        pool.photos(),
        Axis::Time(TimeGranularity::Day),
        utc(),
        Sort::default(),
    );
    assert_eq!(groups.len(), 2);
    assert_eq!(groups[0].title, "Day 1 · 11/05/25");
    assert_eq!(groups[1].title, "Day 2 · 12/05/25");
    assert_eq!(names(&pool, &groups[0].members), ["d1.JPG"]);
}

#[test]
fn undated_photos_fall_into_no_date_tail_last() {
    let pool = pair([
        at("a/dated.JPG", Some(datetime!(2025-05-11 09:00:00))),
        at("a/undated.JPG", None),
    ]);
    let groups = group(
        pool.photos(),
        Axis::Time(TimeGranularity::Day),
        utc(),
        Sort::default(),
    );
    let last = groups.last().expect("has groups");
    assert_eq!(last.kind, GroupKind::Leftover);
    assert_eq!(last.title, "No date");
    assert_eq!(names(&pool, &last.members), ["undated.JPG"]);
}

#[test]
fn no_date_tail_stays_last_even_when_sorted_descending() {
    let pool = pair([
        at("a/d1.JPG", Some(datetime!(2025-05-11 09:00:00))),
        at("a/d2.JPG", Some(datetime!(2025-05-12 09:00:00))),
        at("a/undated.JPG", None),
    ]);
    let desc = Sort {
        key: SortKey::Time,
        dir: SortDir::Desc,
    };
    let groups = group(pool.photos(), Axis::Time(TimeGranularity::Day), utc(), desc);
    // Descending puts the later day first, but the leftover stays pinned last.
    assert_eq!(groups[0].title, "Day 2 · 12/05/25");
    assert_eq!(groups[1].title, "Day 1 · 11/05/25");
    assert_eq!(groups.last().unwrap().kind, GroupKind::Leftover);
}

#[test]
fn members_within_a_group_follow_the_active_sort_key() {
    // One day, so a single bucket — its members must order by the sort key, not
    // by capture time (exercises the `Name` path through `time_groups`).
    let pool = pair([
        at("a/c.JPG", Some(datetime!(2025-05-11 09:00:00))),
        at("a/a.JPG", Some(datetime!(2025-05-11 10:00:00))),
        at("a/b.JPG", Some(datetime!(2025-05-11 11:00:00))),
    ]);
    let name_desc = Sort {
        key: SortKey::Name,
        dir: SortDir::Desc,
    };
    let groups = group(
        pool.photos(),
        Axis::Time(TimeGranularity::Day),
        utc(),
        name_desc,
    );
    assert_eq!(groups.len(), 1);
    assert_eq!(
        names(&pool, &groups[0].members),
        ["c.JPG", "b.JPG", "a.JPG"]
    );
}

#[test]
fn smart_day_splits_a_single_day_into_time_of_day_buckets() {
    let pool = pair([
        at("a/midday.JPG", Some(datetime!(2025-05-11 12:00:00))),
        at("a/early.JPG", Some(datetime!(2025-05-11 06:00:00))),
        at("a/morning.JPG", Some(datetime!(2025-05-11 09:00:00))),
    ]);
    let groups = group(
        pool.photos(),
        Axis::Time(TimeGranularity::SmartDay),
        utc(),
        Sort::default(),
    );
    let titles: Vec<&str> = groups.iter().map(|g| g.title.as_str()).collect();
    // Ordered by earliest member: Early, Morning, Midday — empty buckets absent.
    assert_eq!(
        titles,
        [
            "Early · 11/05/25",
            "Morning · 11/05/25",
            "Midday · 11/05/25"
        ]
    );
}

#[test]
fn smart_day_night_spans_midnight_into_one_group() {
    let pool = pair([
        at("a/late.JPG", Some(datetime!(2025-05-11 23:30:00))),
        at("a/after_midnight.JPG", Some(datetime!(2025-05-12 01:00:00))),
    ]);
    let groups = group(
        pool.photos(),
        Axis::Time(TimeGranularity::SmartDay),
        utc(),
        Sort::default(),
    );
    assert_eq!(groups.len(), 1, "both belong to the same night");
    assert_eq!(groups[0].title, "Night · 11/05/25");
    // Sorted by time, the 23:30 frame precedes the 01:00 one.
    assert_eq!(
        names(&pool, &groups[0].members),
        ["late.JPG", "after_midnight.JPG"]
    );
}

#[test]
fn hour_groups_split_by_clock_hour() {
    let pool = pair([
        at("a/h14a.JPG", Some(datetime!(2025-05-11 14:10:00))),
        at("a/h14b.JPG", Some(datetime!(2025-05-11 14:50:00))),
        at("a/h15.JPG", Some(datetime!(2025-05-11 15:05:00))),
    ]);
    let groups = group(
        pool.photos(),
        Axis::Time(TimeGranularity::Hour),
        utc(),
        Sort::default(),
    );
    assert_eq!(groups.len(), 2);
    assert_eq!(groups[0].title, "14:00 · 11/05/25");
    assert_eq!(groups[0].members.len(), 2);
    assert_eq!(groups[1].title, "15:00 · 11/05/25");
}

#[test]
fn week_groups_collapse_days_in_the_same_iso_week() {
    // 2025-05-12 is a Monday; 05-11 (Sun) is the prior week, 05-14 (Wed) same.
    let pool = pair([
        at("a/sun.JPG", Some(datetime!(2025-05-11 09:00:00))),
        at("a/mon.JPG", Some(datetime!(2025-05-12 09:00:00))),
        at("a/wed.JPG", Some(datetime!(2025-05-14 09:00:00))),
    ]);
    let groups = group(
        pool.photos(),
        Axis::Time(TimeGranularity::Week),
        utc(),
        Sort::default(),
    );
    assert_eq!(groups.len(), 2, "Sunday is its own week; Mon+Wed share one");
    assert_eq!(groups[0].title, "Week of 05/05/25");
    assert_eq!(groups[1].title, "Week of 12/05/25");
    assert_eq!(groups[1].members.len(), 2);
}

#[test]
fn auto_resolves_single_day_to_smart_day_and_multi_day_to_day() {
    let single = pair([
        at("a/a.JPG", Some(datetime!(2025-05-11 09:00:00))),
        at("a/b.JPG", Some(datetime!(2025-05-11 14:00:00))),
    ]);
    assert_eq!(
        resolve_auto(single.photos(), utc(), TimeGranularity::Auto),
        TimeGranularity::SmartDay
    );

    let multi = pair([
        at("a/a.JPG", Some(datetime!(2025-05-11 09:00:00))),
        at("a/b.JPG", Some(datetime!(2025-05-12 09:00:00))),
    ]);
    assert_eq!(
        resolve_auto(multi.photos(), utc(), TimeGranularity::Auto),
        TimeGranularity::Day
    );
}

#[test]
fn auto_passes_through_an_explicit_granularity() {
    let pool = pair([at("a/a.JPG", Some(datetime!(2025-05-11 09:00:00)))]);
    assert_eq!(
        resolve_auto(pool.photos(), utc(), TimeGranularity::Week),
        TimeGranularity::Week
    );
}

#[test]
fn group_order_follows_earliest_member() {
    // The earlier day must come first regardless of pool insertion order.
    let pool = pair([
        at("a/later.JPG", Some(datetime!(2025-06-01 09:00:00))),
        at("a/earlier.JPG", Some(datetime!(2025-05-01 09:00:00))),
    ]);
    let groups = group(
        pool.photos(),
        Axis::Time(TimeGranularity::Day),
        utc(),
        Sort::default(),
    );
    assert_eq!(names(&pool, &groups[0].members), ["earlier.JPG"]);
    assert_eq!(names(&pool, &groups[1].members), ["later.JPG"]);
}

#[test]
fn adjusted_derives_per_instant_offset_across_dst() {
    let rome = timezone::zone("Europe/Rome").expect("Rome exists");
    let winter = adjusted(datetime!(2025-01-15 12:00:00), rome);
    let summer = adjusted(datetime!(2025-07-15 12:00:00), rome);
    assert_eq!(winter.offset().whole_hours(), 1, "CET = UTC+1");
    assert_eq!(summer.offset().whole_hours(), 2, "CEST = UTC+2");
}

#[test]
fn grouping_uses_wall_clock_date_and_stamps_the_zone_offset() {
    // The naive EXIF time is the camera's wall clock; the shoot zone stamps the
    // offset for that instant but does not move the wall-clock date. So a
    // 22:00 frame groups on its own date, with the zone's offset applied.
    let tokyo = timezone::zone("Asia/Tokyo").expect("Tokyo exists");
    assert_eq!(
        adjusted(datetime!(2025-05-11 22:00:00), tokyo)
            .offset()
            .whole_hours(),
        9
    );
    let pool = pair([at("a/a.JPG", Some(datetime!(2025-05-11 22:00:00)))]);
    let groups = group(
        pool.photos(),
        Axis::Time(TimeGranularity::Day),
        tokyo,
        Sort::default(),
    );
    assert_eq!(groups[0].title, "Day 1 · 11/05/25");
}
