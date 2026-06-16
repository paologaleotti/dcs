//! IANA timezone helpers (open Q#5). The shoot zone is owned, freeze-critical
//! state: a crystallized tag made under the wrong zone is wrong forever. This
//! module is the pure source of the zone list (for a picker) and validation;
//! the actual `OffsetDateTime` adjustment lands with the grouping slice.

use time_tz::TimeZone;
use time_tz::timezones;

/// Every IANA zone name, sorted — the data behind a timezone picker.
pub fn zone_names() -> Vec<&'static str> {
    let mut names: Vec<&'static str> = timezones::iter().map(|tz| tz.name()).collect();
    names.sort_unstable();
    names.dedup();
    names
}

/// Whether `name` is a known IANA zone (e.g. `"Europe/Rome"`).
pub fn is_valid(name: &str) -> bool {
    timezones::get_by_name(name).is_some()
}
