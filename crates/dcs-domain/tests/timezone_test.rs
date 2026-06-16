use dcs_domain::timezone;

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
