//! IANA timezone helpers. The shoot zone is owned, freeze-critical
//! state: a crystallized tag made under the wrong zone is wrong forever. This
//! module is the pure source of the zone list (for a picker), zone lookup, and
//! the per-instant `OffsetDateTime` adjustment that grouping derives from.

use time::{OffsetDateTime, PrimitiveDateTime, UtcOffset};
use time_tz::{OffsetDateTimeExt, PrimitiveDateTimeExt, TimeZone, Tz, timezones};

/// Every IANA zone name, sorted — the data behind a timezone picker.
pub fn zone_names() -> Vec<&'static str> {
    let mut names: Vec<&'static str> = timezones::iter().map(|tz| tz.name()).collect();
    names.sort_unstable();
    names.dedup();
    names
}

/// Whether `name` is a known IANA zone (e.g. `"Europe/Rome"`).
pub fn is_valid(name: &str) -> bool {
    zone(name).is_some()
}

/// Look up an IANA zone by name (e.g. `"Europe/Rome"`), `None` if unknown.
pub fn zone(name: &str) -> Option<&'static Tz> {
    timezones::get_by_name(name)
}

/// Resolve the absolute capture instant from the naive EXIF time. The naive time
/// is wall-clock with no zone, so it must be anchored before it means an instant:
/// a per-photo EXIF offset (`OffsetTimeOriginal`) wins when present, otherwise the
/// camera zone supplies the offset *for that instant* (DST mid-trip safe).
///
/// On the rare impossible local time (the spring-forward gap) we fall back to UTC
/// rather than panic — a real capture never lands there, and derivation must stay
/// total.
pub fn source_instant(
    naive: PrimitiveDateTime,
    captured_offset: Option<UtcOffset>,
    camera_zone: &Tz,
) -> OffsetDateTime {
    match captured_offset {
        Some(offset) => naive.assume_offset(offset),
        None => match naive.assume_timezone(camera_zone) {
            time_tz::OffsetResult::Some(dt) => dt,
            // Ambiguous (fall-back hour, seen twice): take the first (pre-transition)
            // occurrence — deterministic and good enough for grouping.
            time_tz::OffsetResult::Ambiguous(first, _) => first,
            time_tz::OffsetResult::None => naive.assume_utc(),
        },
    }
}

/// Convert an absolute capture instant into the display (shoot) zone, deriving the
/// offset for that instant so a trip spanning a DST change stays correct. The
/// returned wall-clock is what grouping and the caption read; it only differs from
/// the shot time when the display zone differs from the photo's source offset.
pub fn adjusted(instant: OffsetDateTime, display_zone: &Tz) -> OffsetDateTime {
    instant.to_timezone(display_zone)
}

/// The capture instant in display-zone wall-clock: anchor the naive EXIF time to
/// its source ([`source_instant`]), then convert into the display zone
/// ([`adjusted`]). The one derivation behind every time-group bucket, the gallery
/// caption, and burst ordering — so they can never disagree on a frame's instant.
pub fn attributed_instant(
    naive: PrimitiveDateTime,
    captured_offset: Option<UtcOffset>,
    camera_zone: &Tz,
    display_zone: &Tz,
) -> OffsetDateTime {
    adjusted(
        source_instant(naive, captured_offset, camera_zone),
        display_zone,
    )
}
