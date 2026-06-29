use dcs_domain::timezone;
use time::UtcOffset;
use time::macros::datetime;

#[test]
fn format_offset_signs_off_whole_offset_not_hour() {
    let f = |h, m| timezone::format_offset(UtcOffset::from_hms(h, m, 0).unwrap());
    assert_eq!(f(2, 0), "+02:00");
    assert_eq!(f(-5, 0), "-05:00");
    assert_eq!(f(0, 0), "+00:00");
    assert_eq!(f(5, 30), "+05:30");
    assert_eq!(f(-9, -30), "-09:30");
    // Regression: a sub-hour negative offset has hour component 0 but must
    // still render with a minus sign.
    assert_eq!(f(0, -30), "-00:30");
    assert_eq!(f(0, -45), "-00:45");
}

fn rome() -> &'static time_tz::Tz {
    timezone::zone("Europe/Rome").expect("Rome exists")
}

#[test]
fn zone_names_are_sorted_nonempty_and_include_known_zones() {
    let names = timezone::zone_names();
    assert!(names.len() > 100, "the IANA database has hundreds of zones");
    assert!(
        names.windows(2).all(|w| w[0] <= w[1]),
        "sorted for the picker"
    );
    assert!(names.contains(&"Europe/Rome"));
    assert!(names.contains(&"Asia/Tokyo"));
    assert!(names.contains(&"UTC"));
}

#[test]
fn is_valid_accepts_known_and_rejects_bogus() {
    assert!(timezone::is_valid("Europe/Rome"));
    assert!(timezone::is_valid("Asia/Tokyo"));
    assert!(!timezone::is_valid("Mars/Olympus_Mons"));
    assert!(!timezone::is_valid(""));
}

#[test]
fn source_instant_spring_forward_gap_falls_back_to_utc() {
    // 2025-03-30 02:00→03:00 in Rome: 02:30 never existed. Derivation must stay
    // total — fall back to UTC (offset 0) rather than panic.
    let gap = timezone::source_instant(datetime!(2025-03-30 02:30:00), None, rome());
    assert_eq!(gap.offset().whole_hours(), 0, "impossible local time → UTC");
}

#[test]
fn source_instant_fall_back_hour_takes_first_occurrence() {
    // 2025-10-26 03:00→02:00 in Rome: 02:30 happens twice (CEST then CET). The
    // first (pre-transition, +02:00) occurrence is chosen, deterministically.
    let ambiguous = timezone::source_instant(datetime!(2025-10-26 02:30:00), None, rome());
    assert_eq!(
        ambiguous.offset().whole_hours(),
        2,
        "ambiguous hour → first (CEST) occurrence"
    );
}

#[test]
fn source_instant_prefers_exif_offset_over_camera_zone() {
    // A photo carrying its own EXIF offset ignores the camera-zone argument.
    use time::macros::offset;
    let instant = timezone::source_instant(
        datetime!(2025-07-15 12:00:00),
        Some(offset!(+05:30)),
        rome(),
    );
    assert_eq!(instant.offset().whole_hours(), 5);
    assert_eq!(instant.offset().minutes_past_hour(), 30);
}
