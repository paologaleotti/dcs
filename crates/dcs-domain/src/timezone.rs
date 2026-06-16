//! IANA timezone helpers (open Q#5). The shoot zone is owned, freeze-critical
//! state: a crystallized tag made under the wrong zone is wrong forever. This
//! module is the pure source of the zone list (for a picker), zone lookup, and
//! the per-instant `OffsetDateTime` adjustment that grouping derives from.

use time::{OffsetDateTime, PrimitiveDateTime};
use time_tz::{PrimitiveDateTimeExt, TimeZone, Tz, timezones};

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

/// Anchor a naive EXIF capture time to the shoot zone, deriving the offset *for
/// that instant* so a trip spanning a DST change stays correct (#7). The offset
/// is per-photo, never a single project-wide constant.
///
/// On the rare impossible local time (the spring-forward gap), we fall back to
/// UTC rather than panic — a real capture never lands there, and grouping must
/// stay total.
pub fn adjusted(naive: PrimitiveDateTime, zone: &Tz) -> OffsetDateTime {
    match naive.assume_timezone(zone) {
        time_tz::OffsetResult::Some(dt) => dt,
        // Ambiguous (fall-back hour, seen twice): take the first (pre-transition)
        // occurrence — deterministic and good enough for grouping.
        time_tz::OffsetResult::Ambiguous(first, _) => first,
        time_tz::OffsetResult::None => naive.assume_utc(),
    }
}
