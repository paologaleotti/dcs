//! Derived grouping (§2.2–2.4, §2.8). The grid is one pool segmented into an
//! ordered list of groups by an axis + granularity, in the shoot zone. Pure and
//! derived: nothing here is persisted — change a knob and it re-derives.
//!
//! A photo belongs to exactly one group. Members carry pool indices in sort
//! order; the leftover (`No date`) group is always last (#8). Counts are *not*
//! stored — they're derived after filtering, by the caller.

use std::collections::HashMap;

use time::{Date, OffsetDateTime};
use time_tz::Tz;

use crate::photo::Photo;
use crate::sort::{self, Sort, SortDir};
use crate::timezone;

/// The grouping axis (§2.8). `Gps`/`Tag` are deferred to later slices; the enum
/// stays small so the active set is exactly what's wired.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Axis {
    Time(TimeGranularity),
    None,
}

/// Time bucket size (§2.4). `Auto` resolves from the data (single day →
/// `SmartDay`, multi-day → `Day`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TimeGranularity {
    Auto,
    SmartDay,
    Hour,
    Day,
    Week,
}

/// What produced a group — drives header styling and ordering (§2.8, §3).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GroupKind {
    /// A derived time bucket.
    Time,
    /// The `No date` tail — undated photos, always last (#8).
    Leftover,
    /// The single group when the axis is `None`.
    Stream,
}

/// One derived group: an ordered run of pool indices under a title. Derived,
/// never persisted; counts are computed by the caller after filtering.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DerivedGroup {
    pub kind: GroupKind,
    pub title: String,
    pub members: Vec<usize>,
}

/// Segment `photos` into ordered groups by `axis`, in the shoot `zone`, with
/// members sorted by `sort`. Time groups order chronologically (reversed when
/// the sort direction is `Desc`) regardless of the sort *key*; the `No date`
/// group is always last.
pub fn group(photos: &[Photo], axis: Axis, zone: &Tz, sort: Sort) -> Vec<DerivedGroup> {
    match axis {
        Axis::None => stream(photos, sort),
        Axis::Time(granularity) => {
            time_groups(photos, resolve_auto(photos, zone, granularity), zone, sort)
        }
    }
}

/// Resolve `Auto` against the data: one calendar day (in zone) → `SmartDay`,
/// otherwise `Day`. Other granularities pass through unchanged. The UI shows
/// the resolution, e.g. `groups: auto (day)`.
pub fn resolve_auto(photos: &[Photo], zone: &Tz, granularity: TimeGranularity) -> TimeGranularity {
    if granularity != TimeGranularity::Auto {
        return granularity;
    }
    let mut first: Option<Date> = None;
    let mut single = true;
    for p in photos {
        let Some(naive) = p.captured_at else { continue };
        let date = timezone::adjusted(naive, zone).date();
        match first {
            None => first = Some(date),
            Some(d) if d != date => {
                single = false;
                break;
            }
            _ => {}
        }
    }
    if single {
        TimeGranularity::SmartDay
    } else {
        TimeGranularity::Day
    }
}

/// Neutral time-of-day buckets for smart-day grouping (§2.4). Evocative labels
/// ("golden hour") are a future opt-in; these are the always-on defaults.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
enum DayPart {
    Early,
    Morning,
    Midday,
    Afternoon,
    LateAfternoon,
    Evening,
    Night,
}

impl DayPart {
    fn label(self) -> &'static str {
        match self {
            DayPart::Early => "Early",
            DayPart::Morning => "Morning",
            DayPart::Midday => "Midday",
            DayPart::Afternoon => "Afternoon",
            DayPart::LateAfternoon => "Late afternoon",
            DayPart::Evening => "Evening",
            DayPart::Night => "Night",
        }
    }
}

fn stream(photos: &[Photo], sort: Sort) -> Vec<DerivedGroup> {
    if photos.is_empty() {
        return Vec::new();
    }
    vec![DerivedGroup {
        kind: GroupKind::Stream,
        title: String::new(),
        members: sort::order(photos, sort),
    }]
}

/// A bucket key in chronological order, so groups sort by it before the leftover
/// tail: the attributed `Date` plus the within-day discriminant for the active
/// granularity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
struct BucketKey {
    date: Date,
    sub: SubKey,
}

/// The within-day part of a bucket key. A grouping only ever produces one
/// variant kind, so the cross-variant ordering is never exercised — within a
/// kind, `Hour`/`Part` order chronologically, which is what bucket ordering
/// needs. Carrying the `DayPart` directly (not a packed `u8`) keeps the title
/// derivation total and type-checked.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
enum SubKey {
    /// `Day`/`Auto`: one bucket per calendar date.
    Day,
    /// `Week`: one bucket per ISO week (keyed on its Monday).
    Week,
    /// `Hour`: one bucket per clock hour.
    Hour(u8),
    /// `SmartDay`: one bucket per time-of-day part.
    Part(DayPart),
}

fn time_groups(
    photos: &[Photo],
    granularity: TimeGranularity,
    zone: &Tz,
    sort: Sort,
) -> Vec<DerivedGroup> {
    // Pre-derive names once so member sorts don't re-allocate per comparison.
    let names: Vec<String> = photos.iter().map(Photo::file_name).collect();
    let cmp = |x: &usize, y: &usize| {
        sort::compare(&photos[*x], &names[*x], &photos[*y], &names[*y], sort)
    };

    let mut buckets: Vec<(BucketKey, Vec<usize>)> = Vec::new();
    let mut index: HashMap<BucketKey, usize> = HashMap::new();
    let mut undated: Vec<usize> = Vec::new();
    for (i, p) in photos.iter().enumerate() {
        let Some(naive) = p.captured_at else {
            undated.push(i);
            continue;
        };
        let key = bucket_key(timezone::adjusted(naive, zone), granularity);
        let slot = *index.entry(key).or_insert_with(|| {
            buckets.push((key, Vec::new()));
            buckets.len() - 1
        });
        buckets[slot].1.push(i);
    }

    // Day numbers are chronological (#6): assigned before any direction reversal.
    let mut day_numbers: Vec<Date> = buckets.iter().map(|(k, _)| k.date).collect();
    day_numbers.sort_unstable();
    day_numbers.dedup();

    buckets.sort_by(|(a, _), (b, _)| a.cmp(b));
    if sort.dir == SortDir::Desc {
        buckets.reverse();
    }

    let mut groups: Vec<DerivedGroup> = buckets
        .into_iter()
        .map(|(key, mut members)| {
            members.sort_by(&cmp);
            DerivedGroup {
                kind: GroupKind::Time,
                title: title_for(key, &day_numbers),
                members,
            }
        })
        .collect();

    if !undated.is_empty() {
        undated.sort_by(&cmp);
        groups.push(DerivedGroup {
            kind: GroupKind::Leftover,
            title: "No date".to_string(),
            members: undated,
        });
    }
    groups
}

fn bucket_key(at: OffsetDateTime, granularity: TimeGranularity) -> BucketKey {
    let date = at.date();
    let hour = at.hour();
    match granularity {
        TimeGranularity::Hour => BucketKey {
            date,
            sub: SubKey::Hour(hour),
        },
        TimeGranularity::Day | TimeGranularity::Auto => BucketKey {
            date,
            sub: SubKey::Day,
        },
        TimeGranularity::Week => {
            // Collapse to the week's Monday so every day in the week shares a key.
            let monday =
                date - time::Duration::days(date.weekday().number_days_from_monday() as i64);
            BucketKey {
                date: monday,
                sub: SubKey::Week,
            }
        }
        TimeGranularity::SmartDay => {
            let part = day_part(hour);
            // Night spans midnight: pre-5am attributes to the previous day (§2.4).
            let attributed = if part == DayPart::Night && hour < 5 {
                date.previous_day().unwrap_or(date)
            } else {
                date
            };
            BucketKey {
                date: attributed,
                sub: SubKey::Part(part),
            }
        }
    }
}

fn day_part(hour: u8) -> DayPart {
    match hour {
        5..=7 => DayPart::Early,
        8..=10 => DayPart::Morning,
        11..=13 => DayPart::Midday,
        14..=16 => DayPart::Afternoon,
        17..=18 => DayPart::LateAfternoon,
        19..=21 => DayPart::Evening,
        _ => DayPart::Night, // 22–23 and 0–4
    }
}

fn title_for(key: BucketKey, days: &[Date]) -> String {
    let date = key.date;
    match key.sub {
        SubKey::Hour(h) => format!("{:02}:00 · {}", h, fmt_date(date)),
        SubKey::Week => format!("Week of {}", fmt_date(date)),
        SubKey::Part(part) => format!("{} · {}", part.label(), fmt_date(date)),
        SubKey::Day => {
            let n = days
                .iter()
                .position(|&d| d == date)
                .map(|i| i + 1)
                .unwrap_or(1);
            format!("Day {} · {}", n, fmt_date(date))
        }
    }
}

/// `DD/MM/YY`, the date anchor carried in every time-group title (§2.4).
fn fmt_date(date: Date) -> String {
    format!(
        "{:02}/{:02}/{:02}",
        date.day(),
        u8::from(date.month()),
        (date.year() % 100).unsigned_abs()
    )
}
